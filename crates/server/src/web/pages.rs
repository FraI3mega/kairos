use std::collections::{BTreeMap, HashSet};

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use chrono::{Days, NaiveDate};
use kairos_core::union;
use serde::Deserialize;

use super::{avatar, render, Base, Ctx};
use crate::api::stats::{hidden_projects, languages, load_spans, user_by_name};
use crate::auth::{SignedIn, User};
use crate::credit::{day_start, local_day};
use crate::live::{self, days_text, streak_label, Bar, Snapshot};
use crate::streak;
use crate::util::{err, human, now, short, urlenc, Res};
use crate::St;

const OG_PNG: &[u8] = include_bytes!("../../assets/og.png");

pub struct Row {
    pub label: String,
    pub href: Option<String>,
    pub color: Option<&'static str>,
    pub pct: String,
    pub time: String,
}

pub struct DayBar {
    pub title: String,
    pub pct: String,
}

pub struct Summary {
    pub days: Vec<DayBar>,
    pub first_label: String,
    pub projects: Vec<Row>,
    pub classes: Vec<Row>,
    pub languages: Vec<Row>,
}

pub fn kind(class: &str) -> (&'static str, &'static str) {
    match class {
        "typing" => ("Typing prompts", "#ffd166"),
        "reading" => ("Reading replies", "#6ee7c8"),
        "editing" => ("Editing", "#ff8fab"),
        "testing" => ("Testing", "#8fd3ff"),
        "terminal" => ("Terminal", "#c3c8f5"),
        "bridge" => ("Bridging time", "#a8e6a3"),
        "trip" => ("Trips", "#d0b3ff"),
        _ => ("Other", "#d9d6e8"),
    }
}

pub fn project_path(username: &str, project: &str) -> String {
    let segments: Vec<String> = project.split('/').map(urlenc).collect();
    format!("/@{username}/p/{}", segments.join("/"))
}

pub fn rows(list: Vec<(String, f64)>, color: impl Fn(&str) -> Option<(String, &'static str)>, href: impl Fn(&str) -> Option<String>) -> Vec<Row> {
    let max = list.iter().map(|r| r.1).fold(0.0, f64::max);
    list.into_iter()
        .take(12)
        .map(|(n, v)| {
            let (label, c) = match color(&n) {
                Some((l, c)) => (l, Some(c)),
                None => (n, None),
            };
            Row { href: href(&label), label, color: c, pct: format!("{:.1}", if max > 0.0 { v / max * 100.0 } else { 0.0 }), time: human(v) }
        })
        .collect()
}

fn sorted(m: BTreeMap<String, f64>) -> Vec<(String, f64)> {
    let mut v: Vec<(String, f64)> = m.into_iter().filter(|r| r.1 > 0.0).collect();
    v.sort_by(|a, b| b.1.total_cmp(&a.1));
    v
}

pub struct DayRow {
    pub day: NaiveDate,
    pub project: String,
    pub class: String,
    pub seconds: f64,
}

pub async fn user_days(st: &St, user_id: i64, from: NaiveDate) -> Res<Vec<DayRow>> {
    let c = st.pool.get().await?;
    let rows = c
        .query("SELECT day, project, class, seconds FROM user_days WHERE user_id = $1 AND day >= $2", &[&user_id, &from])
        .await?;
    Ok(rows.iter().map(|r| DayRow { day: r.get(0), project: r.get(1), class: r.get(2), seconds: r.get(3) }).collect())
}

/// Hidden projects fold into one row for visitors; Other is its own row.
async fn summary(st: &St, u: &User, today: NaiveDate, hidden: &HashSet<String>) -> Res<Summary> {
    let first = today.checked_sub_days(Days::new(29)).unwrap_or(today);
    let data = user_days(st, u.id, first).await?;
    let mut per_day: BTreeMap<NaiveDate, f64> = BTreeMap::new();
    let mut projects: BTreeMap<String, f64> = BTreeMap::new();
    let mut classes: BTreeMap<String, f64> = BTreeMap::new();
    for r in data.iter().filter(|r| r.class != "agent") {
        *per_day.entry(r.day).or_default() += r.seconds;
        let label = if r.project.is_empty() {
            "Other".to_string()
        } else if hidden.contains(&r.project) {
            "Private projects".to_string()
        } else {
            r.project.clone()
        };
        *projects.entry(label).or_default() += r.seconds;
        *classes.entry(r.class.clone()).or_default() += r.seconds;
    }
    let dates: Vec<NaiveDate> = (0..30).rev().filter_map(|i| today.checked_sub_days(Days::new(i))).collect();
    let max = dates.iter().filter_map(|d| per_day.get(d)).copied().fold(0.0, f64::max);
    let days = dates
        .iter()
        .map(|d| {
            let v = per_day.get(d).copied().unwrap_or(0.0);
            DayBar {
                title: format!("{} · {}", d.format("%a %b %-d"), human(v)),
                pct: format!("{:.1}", if max > 0.0 { v / max * 100.0 } else { 0.0 }),
            }
        })
        .collect();
    let tz = u.tz();
    let (lo, hi) = (day_start(tz, first), now() + 60.0);
    let spans = load_spans(st, u.id, lo, hi).await?;
    let credited = union(spans.iter().map(|s| (s.start, s.end)).collect());
    let langs = languages(st, u.id, &credited, lo, hi).await?;
    Ok(Summary {
        days,
        first_label: dates.first().map(|d| d.format("%b %-d").to_string()).unwrap_or_default(),
        projects: rows(
            sorted(projects),
            |_| None,
            |p| (p != "Other" && p != "Private projects").then(|| project_path(&u.username, p)),
        ),
        classes: rows(
            sorted(classes),
            |c| {
                let (l, col) = kind(c);
                Some((l.to_string(), col))
            },
            |_| None,
        ),
        languages: rows(langs, |_| None, |_| None),
    })
}

async fn total_since(st: &St, user_id: i64, from: Option<NaiveDate>) -> Res<f64> {
    let c = st.pool.get().await?;
    let r = c
        .query_one(
            "SELECT COALESCE(sum(seconds), 0)::float8 FROM user_days WHERE user_id = $1 AND ($2::date IS NULL OR day >= $2) AND class <> 'agent'",
            &[&user_id, &from],
        )
        .await?;
    Ok(r.get(0))
}

pub async fn favicon() -> Response {
    let svg = format!(
        "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 9 9' fill='#3a33a3' shape-rendering='crispEdges'>{}</svg>",
        super::mark_rects()
    );
    ([(header::CONTENT_TYPE, "image/svg+xml"), (header::CACHE_CONTROL, "public, max-age=86400")], svg).into_response()
}

pub async fn og() -> Response {
    ([(header::CONTENT_TYPE, "image/png"), (header::CACHE_CONTROL, "public, max-age=86400")], OG_PNG).into_response()
}

#[derive(Template)]
#[template(path = "landing.html")]
struct Landing {
    base: Base,
}

pub async fn landing(State(st): State<St>, ctx: Ctx) -> Res<Response> {
    if ctx.session.is_some() {
        return Ok(Redirect::to("/me").into_response());
    }
    render(&Landing { base: Base::new(&st, &ctx, "Kairos") })
}

pub struct DeviceCard {
    pub id: i64,
    pub name: String,
    pub os: String,
    pub version: Option<String>,
    pub online: bool,
    pub seen: String,
    pub events: i64,
    pub backends: Option<String>,
}

pub fn tool_name(id: &str) -> String {
    match id {
        "claude-code" => "Claude Code",
        "codex" => "Codex",
        "cursor" => "Cursor",
        "copilot" => "GitHub Copilot",
        "copilot-cli" => "Copilot CLI",
        "antigravity" => "Antigravity",
        "gemini-cli" => "Gemini CLI",
        "cline" => "Cline",
        "roo-code" => "Roo Code",
        "kilo" => "Kilo",
        "opencode" => "opencode",
        "qwen-code" => "Qwen Code",
        "pi" => "Pi",
        "hermes" => "Hermes",
        "amp" => "Amp",
        other => return other.to_string(),
    }
    .to_string()
}

pub fn spoken(secs: f64) -> String {
    let m = (secs / 60.0).round() as i64;
    match (m / 60, m % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} hr"),
        (h, m) => format!("{h} hr {m} min"),
    }
}

pub fn os_name(os: &str) -> String {
    match os {
        "windows" => "Windows",
        "macos" => "macOS",
        "linux" => "Linux",
        other => return other.to_string(),
    }
    .to_string()
}

fn backend_note(focus: Option<&str>, idle: Option<&str>) -> Option<String> {
    let mut missing = Vec::new();
    if focus == Some("none") {
        missing.push("can't tell which app you're using");
    }
    if idle == Some("none") {
        missing.push("can't see keyboard or mouse activity");
    }
    (!missing.is_empty()).then(|| missing.join(" · "))
}

pub fn ago(secs: f64) -> String {
    let (m, h, d) = (secs / 60.0, secs / 3600.0, secs / 86400.0);
    let plural = |n: i64, unit: &str| format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" });
    if m < 2.0 {
        "just now".into()
    } else if h < 1.0 {
        plural(m as i64, "min")
    } else if d < 1.0 {
        plural(h as i64, "hr")
    } else {
        plural(d as i64, "day")
    }
}

async fn tools(st: &St, user_id: i64, today: NaiveDate) -> Res<Vec<Row>> {
    let from = today.checked_sub_days(Days::new(29)).unwrap_or(today);
    let c = st.pool.get().await?;
    let list: Vec<(String, f64)> = c
        .query(
            "SELECT harness, sum(prompts)::float8 FROM user_tool_days WHERE user_id = $1 AND day >= $2 AND day <= $3
             GROUP BY harness ORDER BY 2 DESC, 1 LIMIT 12",
            &[&user_id, &from, &today],
        )
        .await?
        .iter()
        .map(|r| (r.get(0), r.get(1)))
        .collect();
    let max = list.first().map_or(0.0, |r| r.1);
    Ok(list
        .into_iter()
        .map(|(id, n)| Row {
            label: tool_name(&id),
            href: None,
            color: None,
            pct: format!("{:.1}", if max > 0.0 { n / max * 100.0 } else { 0.0 }),
            time: format!("{} {}", n as i64, if n as i64 == 1 { "prompt" } else { "prompts" }),
        })
        .collect())
}

#[derive(Template)]
#[template(path = "me.html")]
struct Me {
    base: Base,
    name: String,
    snap: Snapshot,
    snap_json: String,
    all: String,
    timezone: String,
    timezone_set: bool,
    summary: Summary,
    tools: Option<Vec<Row>>,
    devices: Vec<DeviceCard>,
    connected: Option<&'static str>,
}

/// Where `kairos login` sends the browser once the computer is connected.
#[derive(Deserialize)]
pub struct MeQ {
    connected: Option<String>,
    hackatime: Option<String>,
}

pub async fn me(State(st): State<St>, ctx: Ctx, SignedIn(s): SignedIn, Query(q): Query<MeQ>) -> Res<Response> {
    let u = &s.user;
    let today = local_day(u.tz(), now());
    let all = total_since(&st, u.id, None).await?;
    let snap = live::snapshot(&st, u).await?;
    let snap_json = serde_json::to_string(&snap)?.replace("</", "<\\/");
    let summary = summary(&st, u, today, &HashSet::new()).await?;
    let tools = tools(&st, u.id, today).await?;

    let c = st.pool.get().await?;
    let timezone_set: bool = c.query_one("SELECT timezone_set FROM users WHERE id = $1", &[&u.id]).await?.get(0);
    let devices = c
        .query(
            "SELECT id, name, os, client_version, event_count, backends->>'focus', backends->>'idle',
                    extract(epoch FROM now() - last_seen_at)::float8
             FROM devices WHERE user_id = $1 AND revoked_at IS NULL ORDER BY id",
            &[&u.id],
        )
        .await?
        .iter()
        .map(|r| {
            let age: Option<f64> = r.get(7);
            DeviceCard {
                id: r.get(0),
                name: r.get(1),
                os: os_name(&r.get::<_, String>(2)),
                version: r.get(3),
                online: age.is_some_and(|a| a < 600.0),
                seen: age.map_or_else(|| "not yet".to_string(), ago),
                events: r.get(4),
                backends: backend_note(r.get::<_, Option<String>>(5).as_deref(), r.get::<_, Option<String>>(6).as_deref()),
            }
        })
        .collect();
    render(&Me {
        base: Base::new(&st, &ctx, &format!("{} · Kairos", u.username)),
        name: u.display_name.clone().unwrap_or_else(|| u.username.clone()),
        snap,
        snap_json,
        all: short(all),
        timezone: u.timezone.clone(),
        timezone_set,
        summary,
        tools: Some(tools),
        devices,
        connected: match (q.connected.is_some(), q.hackatime.is_some()) {
            (true, true) => Some("This computer is connected, and Hackatime is set up on it too. Kairos runs in the background from now on, so you can close the terminal."),
            (true, false) => Some("This computer is connected. Kairos runs in the background from now on, so you can close the terminal."),
            (false, true) => Some("Hackatime is set up on this computer."),
            (false, false) => None,
        },
    })
}

#[derive(Template)]
#[template(path = "profile.html")]
struct Profile {
    base: Base,
    name: String,
    username: String,
    avatar: String,
    total: String,
    week: String,
    streak: String,
    since: String,
    bars: Vec<Bar>,
    summary: Summary,
    tools: Option<Vec<Row>>,
}

pub async fn profile(State(st): State<St>, ctx: Ctx, Path(username): Path<String>) -> Res<Response> {
    let not_found = || err(StatusCode::NOT_FOUND, "no such user");
    let u = user_by_name(&st, &username).await?.ok_or_else(not_found)?;
    let own = ctx.session.as_ref().is_some_and(|s| s.user.id == u.id);
    if !u.is_public && !own {
        return Err(not_found());
    }
    let hidden = if own { HashSet::new() } else { hidden_projects(&st, u.id).await? };
    let today = local_day(u.tz(), now());
    let all = total_since(&st, u.id, None).await?;
    let summary = summary(&st, &u, today, &hidden).await?;
    let days = live::week(&st, u.id, today).await?;
    let week: f64 = days.iter().map(|d| d.1).sum();
    let streak = {
        let c = st.pool.get().await?;
        streak::current(&c, u.id, today).await?
    };
    let tools = if u.show_ai_tools || own { Some(tools(&st, u.id, today).await?) } else { None };
    let since: String = st
        .pool
        .get()
        .await?
        .query_one("SELECT to_char(created_at, 'FMMon YYYY') FROM users WHERE id = $1", &[&u.id])
        .await?
        .get(0);
    let name = u.display_name.clone().unwrap_or_else(|| u.username.clone());
    let description = format!(
        "{name} has tracked {} of focus time building with AI on Kairos. {} this week, {}.",
        spoken(all),
        spoken(week),
        streak_label(streak)
    );
    render(&Profile {
        base: Base::new(&st, &ctx, &format!("{name} (@{}) · Kairos", u.username)).share(
            &description,
            &format!("/@{}", u.username),
            "profile",
        ),
        avatar: avatar(&u.username),
        name,
        username: u.username.clone(),
        total: short(all),
        week: short(week),
        streak: days_text(streak),
        since,
        bars: live::bars(&days, false),
        summary,
        tools,
    })
}

#[cfg(test)]
mod tests {
    use super::backend_note;

    #[test]
    fn missing_backends_are_named_and_working_ones_are_quiet() {
        assert_eq!(backend_note(Some("x11"), Some("xprintidle")), None);
        assert_eq!(backend_note(None, None), None);
        assert_eq!(backend_note(Some("none"), Some("x11")).as_deref(), Some("can't tell which app you're using"));
        assert_eq!(backend_note(None, Some("none")).as_deref(), Some("can't see keyboard or mouse activity"));
        assert_eq!(
            backend_note(Some("none"), Some("none")).as_deref(),
            Some("can't tell which app you're using · can't see keyboard or mouse activity")
        );
    }
}
