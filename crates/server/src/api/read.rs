//! Shaped like Hackatime's API so a program changes only its base URL.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use axum::extract::{Path, Query, Request, State};
use axum::http::{header, Extensions, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{from_fn, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, Months, NaiveDate, NaiveDateTime, Utc};
use kairos_core::{total_secs, union, Interval};
use serde_json::{json, Value};

use crate::aliases;
use crate::api::stats::{by, hidden_projects, languages, load_carved, Span};
use crate::auth::{User, USER_COLUMNS};
use crate::credit::MAX_SPAN;
use crate::leaderboard::{self, Period};
use crate::limits::{self, Bucket};
use crate::programs::{self, Key};
use crate::util::{bearer, err, now, Res};
use crate::{accounts, streak, St};

const HEARTBEAT_TIMEOUT: f64 = 120.0;
const MAX_TIME: f64 = 253_402_300_799.0;
const DAY: f64 = 86_400.0;
// The order credit uses when it carves overlapping devices, so per-class seconds add up to the total.
const CLASS_ORDER: &[&str] = &["typing", "editing", "terminal", "testing", "reading", "bridge", "trip"];

pub fn router() -> Router<St> {
    Router::new()
        .route("/api/v1/users/lookup_slack_uid/{id}", get(lookup_slack_uid))
        .route("/api/v1/users/lookup_email/{email}", get(lookup_email))
        .route("/api/v1/users/{username}/stats", get(stats))
        .route("/api/v1/users/{username}/projects", get(projects))
        .route("/api/v1/users/{username}/projects/details", get(projects_details))
        .route("/api/v1/users/{username}/project/{*name}", get(project))
        .route("/api/v1/users/{username}/heartbeats/spans", get(heartbeat_spans))
        .route("/api/v1/users/{username}/trust_factor", get(trust_factor))
        .route("/api/v1/leaderboard", get(daily_board))
        .route("/api/v1/leaderboard/daily", get(daily_board))
        .route("/api/v1/leaderboard/weekly", get(weekly_board))
        .layer(from_fn(cors))
}

async fn cors(req: Request, next: Next) -> Response {
    let preflight = req.method() == Method::OPTIONS;
    let requested = req.headers().get(header::ACCESS_CONTROL_REQUEST_HEADERS).cloned();
    let mut res = if preflight { StatusCode::NO_CONTENT.into_response() } else { next.run(req).await };
    let h = res.headers_mut();
    h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if preflight {
        h.insert(header::ACCESS_CONTROL_ALLOW_METHODS, HeaderValue::from_static("GET, OPTIONS"));
        h.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, requested.unwrap_or(HeaderValue::from_static("authorization, content-type")));
        h.insert(header::ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("86400"));
    }
    res
}

/// Query parameters the way Rails reads them: the last duplicate wins and blank means absent.
struct P(HashMap<String, String>);

impl P {
    fn raw(&self, k: &str) -> Option<&str> {
        self.0.get(k).map(String::as_str)
    }

    fn get(&self, k: &str) -> Option<&str> {
        self.raw(k).filter(|v| !v.trim().is_empty())
    }

    fn is_true(&self, k: &str) -> bool {
        self.raw(k) == Some("true")
    }
}

/// Ruby's `split(",")`: trailing empty pieces are dropped.
fn csv(s: &str) -> Vec<String> {
    let mut v: Vec<String> = s.split(',').map(String::from).collect();
    while v.last().is_some_and(String::is_empty) {
        v.pop();
    }
    v
}

fn unprocessable(msg: String) -> crate::util::AppError {
    err(StatusCode::UNPROCESSABLE_ENTITY, &msg)
}

pub enum Caller {
    Anonymous,
    User(i64),
    Program(Key),
}

async fn authenticate(st: &St, h: &HeaderMap, ext: &Extensions, p: &P) -> Res<Caller> {
    let key = bearer(h).or_else(|| p.get("api_key").map(String::from)).filter(|k| !k.is_empty());
    let caller = match key {
        None => Caller::Anonymous,
        Some(k) if k.starts_with(programs::KEY_PREFIX) => programs::key_by_token(st, &k).await?.map_or(Caller::Anonymous, Caller::Program),
        Some(k) => match accounts::editor_key_user(st, &k).await? {
            Some(uid) if !is_banned(st, uid).await? => Caller::User(uid),
            _ => Caller::Anonymous,
        },
    };
    match &caller {
        Caller::Program(k) => limits::check(st, Bucket::Program, &k.id.to_string())?,
        _ => limits::check(st, Bucket::Api, &limits::ip_key(limits::client_ip(ext, h, &st.config.trusted_proxies)))?,
    }
    Ok(caller)
}

// A banned user's own key stops working for the API, as a convicted Hackatime user's does.
async fn is_banned(st: &St, user_id: i64) -> Res<bool> {
    let c = st.pool.get().await?;
    Ok(c.query_opt("SELECT trust = 'banned' FROM users WHERE id = $1", &[&user_id]).await?.is_some_and(|r| r.get(0)))
}

struct Target {
    user: User,
    trust: String,
}

/// Numeric id, then Slack id, then Hack Club Auth subject, then username (case-insensitive).
async fn find(st: &St, ident: &str) -> Res<Option<Target>> {
    if ident.is_empty() || ident.len() > 200 {
        return Ok(None);
    }
    let numeric: Option<i64> = if ident.bytes().all(|b| b.is_ascii_digit()) { ident.parse().ok() } else { None };
    let c = st.pool.get().await?;
    let rows = c
        .query(
            &format!(
                "SELECT {USER_COLUMNS}, u.trust, u.slack_id, u.hca_sub FROM users u
                 WHERE u.id = $2 OR u.slack_id = $1 OR u.hca_sub = $1 OR u.username = lower($1) ORDER BY u.id"
            ),
            &[&ident, &numeric],
        )
        .await?;
    let lower = ident.to_lowercase();
    let pick = numeric
        .and_then(|n| rows.iter().find(|r| r.get::<_, i64>("id") == n))
        .or_else(|| rows.iter().find(|r| r.get::<_, Option<String>>("slack_id").as_deref() == Some(ident)))
        .or_else(|| rows.iter().find(|r| r.get::<_, String>("hca_sub") == ident))
        .or_else(|| rows.iter().find(|r| r.get::<_, String>("username") == lower));
    Ok(pick.map(|r| Target { user: User::from_row(r), trust: r.get("trust") }))
}

struct Access {
    owner: bool,
    program: bool,
}

impl Access {
    // Project hiding is a privacy setting for the public. The owner and the programs the user joined see everything.
    fn sees_hidden(&self) -> bool {
        self.owner || self.program
    }
}

async fn open(st: &St, caller: &Caller, ident: &str, endpoint: &str) -> Res<(Target, Access)> {
    let found = match (ident, caller) {
        ("my", Caller::User(id)) => find(st, &id.to_string()).await?,
        ("my", _) => None,
        _ => find(st, ident).await?,
    };
    let t = found.ok_or_else(|| err(StatusCode::NOT_FOUND, "User not found"))?;
    let access = match caller {
        Caller::Program(k) => {
            programs::audit_read(st, k, t.user.id, endpoint).await?;
            Access { owner: false, program: true }
        }
        Caller::User(id) if *id == t.user.id => Access { owner: true, program: false },
        _ if t.user.is_public => Access { owner: false, program: false },
        _ => return Err(err(StatusCode::FORBIDDEN, "user has disabled public stats")),
    };
    Ok((t, access))
}

async fn hidden_for(st: &St, t: &Target, a: &Access) -> Res<HashSet<String>> {
    if a.sees_hidden() {
        Ok(HashSet::new())
    } else {
        hidden_projects(st, t.user.id).await
    }
}

fn trust_level(trust: &str) -> (&'static str, i64) {
    match trust {
        "trusted" => ("green", 2),
        "banned" => ("red", 1),
        _ => ("blue", 0),
    }
}

fn trust_json(trust: &str) -> Value {
    let (level, value) = trust_level(trust);
    json!({ "trust_level": level, "trust_value": value })
}

pub fn category(class: &str) -> &'static str {
    match class {
        // Bridging time and trips sit almost all between prompts and replies.
        "typing" | "reading" | "bridge" | "trip" => "ai coding",
        "terminal" | "editing" => "coding",
        "testing" => "running tests",
        _ => "coding",
    }
}

// `researching` stays for Hackatime clients that filter by it, though no class maps to it.
const CATEGORIES: &[&str] = &["ai coding", "coding", "running tests", "researching"];

/// Ruby's `Time.zone.parse` for UTC, limited to ISO 8601 dates and times (offsets and `Z` honoured).
pub(crate) fn parse_time(s: &str) -> Option<f64> {
    let s = s.trim();
    let at = |t: DateTime<Utc>| t.timestamp() as f64 + f64::from(t.timestamp_subsec_nanos()) / 1e9;
    if let Ok(d) = DateTime::parse_from_rfc3339(s) {
        return Some(at(d.with_timezone(&Utc)));
    }
    for f in ["%Y-%m-%d %H:%M:%S%.f %z", "%Y-%m-%d %H:%M:%S%.f%z", "%Y-%m-%dT%H:%M:%S%.f%z", "%Y-%m-%d %H:%M%z", "%Y-%m-%dT%H:%M%z"] {
        if let Ok(d) = DateTime::parse_from_str(s, f) {
            return Some(at(d.with_timezone(&Utc)));
        }
    }
    for f in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M"] {
        if let Ok(d) = NaiveDateTime::parse_from_str(s, f) {
            return Some(at(d.and_utc()));
        }
    }
    NaiveDate::parse_from_str(s, "%Y-%m-%d").ok().and_then(|d| d.and_hms_opt(0, 0, 0)).map(|d| at(d.and_utc()))
}

fn years_ago(n: u32) -> f64 {
    Utc::now().checked_sub_months(Months::new(12 * n)).map_or(0.0, |t| t.timestamp() as f64)
}

fn next_midnight() -> f64 {
    (now() / DAY).floor() * DAY + DAY
}

/// 422 `Invalid <name>` if it doesn't parse; absent or blank gives None.
fn strict(p: &P, key: &str) -> Res<Option<f64>> {
    match p.get(key) {
        None => Ok(None),
        Some(s) => parse_time(s).map(Some).ok_or_else(|| unprocessable(format!("Invalid {key}"))),
    }
}

fn lenient(p: &P, keys: &[&str], default: f64) -> f64 {
    keys.iter().filter_map(|k| p.get(k)).find_map(parse_time).unwrap_or(default)
}

fn simple(s: i64) -> String {
    let (h, m) = (s / 3600, (s % 3600) / 60);
    match (h, m) {
        (0, 0) => "0m".into(),
        (h, 0) => format!("{h}h"),
        (0, m) => format!("{m}m"),
        (h, m) => format!("{h}h {m}m"),
    }
}

fn detailed(s: i64) -> String {
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    [(h, 'h'), (m, 'm'), (sec, 's')].iter().filter(|(n, _)| *n > 0).map(|(n, u)| format!("{n}{u}")).collect::<Vec<_>>().join(" ")
}

fn digital(s: i64) -> String {
    format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}

fn iso(t: f64) -> String {
    DateTime::from_timestamp(t.floor() as i64, 0).map_or_else(String::new, |d| d.format("%Y-%m-%dT%H:%M:%SZ").to_string())
}

fn language_color(name: &str) -> &'static str {
    match name {
        "Rust" => "#dea584",
        "Python" => "#3572A5",
        "JavaScript" => "#f1e05a",
        "TypeScript" => "#3178c6",
        "Ruby" => "#701516",
        "Go" => "#00ADD8",
        "Java" => "#b07219",
        "Kotlin" => "#A97BFF",
        "Swift" => "#F05138",
        "C" => "#555555",
        "C++" => "#f34b7d",
        "C#" => "#178600",
        "HTML" => "#e34c26",
        "CSS" => "#563d7c",
        "SCSS" => "#c6538c",
        "Svelte" => "#ff3e00",
        "Vue" => "#41b883",
        "PHP" => "#4F5D95",
        "Shell" => "#89e051",
        "Bash" => "#89e051",
        "Lua" => "#000080",
        "Dart" => "#00B4AB",
        "Elixir" => "#6e4a7e",
        "Haskell" => "#5e5086",
        "Zig" => "#ec915c",
        "Markdown" => "#083fa1",
        "JSON" => "#292929",
        "YAML" => "#cb171e",
        "TOML" => "#9c4221",
        "SQL" => "#e38c00",
        "Dockerfile" => "#384d54",
        _ => "#888888",
    }
}

fn entry(name: &str, secs: i64, total: i64, color: bool) -> Value {
    let percent = if total > 0 { (100.0 * secs as f64 / total as f64 * 100.0).round() / 100.0 } else { 0.0 };
    let name = if name.is_empty() { "Other" } else { name };
    let mut v = json!({
        "name": name,
        "total_seconds": secs,
        "text": simple(secs),
        "hours": secs / 3600,
        "minutes": (secs % 3600) / 60,
        "percent": percent,
        "digital": digital(secs),
    });
    if color {
        v["color"] = json!(language_color(name));
    }
    v
}

fn chunk(items: &[(String, f64)], total: i64, limit: usize, color: bool) -> Vec<Value> {
    let mut rows: Vec<(&str, i64)> = items.iter().map(|(n, s)| (n.as_str(), s.round() as i64)).collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    if limit > 0 {
        rows.truncate(limit);
    }
    rows.into_iter().map(|(n, s)| entry(n, s, total, color)).collect()
}

/// Ruby's `to_i` on a string: leading digits, anything else is 0.
fn to_i(s: &str) -> i64 {
    let s = s.trim_start();
    let (neg, s) = s.strip_prefix('-').map_or((false, s.strip_prefix('+').unwrap_or(s)), |r| (true, r));
    let n: i64 = s.chars().take_while(char::is_ascii_digit).collect::<String>().parse().unwrap_or(0);
    if neg {
        -n
    } else {
        n
    }
}

async fn canonical_filter(st: &St, user_id: i64, list: Option<Vec<String>>) -> Res<Option<Vec<String>>> {
    match list {
        None => Ok(None),
        Some(l) => Ok(Some(aliases::canonical_list(&st.pool.get().await?, user_id, &l).await?)),
    }
}

pub(crate) fn counts(project: Option<&str>, projects: Option<&[String]>, hidden: &HashSet<String>) -> bool {
    project.is_some_and(|p| !hidden.contains(p) && projects.is_none_or(|l| l.iter().any(|x| x == p)))
}

async fn credited(
    st: &St,
    user_id: i64,
    (start, end): (f64, f64),
    projects: Option<&[String]>,
    hidden: &HashSet<String>,
    keep: impl Fn(&str) -> bool,
) -> Res<Vec<Span>> {
    Ok(load_carved(st, user_id, start, end)
        .await?
        .into_iter()
        .filter(|s| s.end > s.start && keep(&s.class))
        .filter(|s| counts(s.project.as_deref(), projects, hidden))
        .collect())
}

fn intervals(spans: &[Span]) -> Vec<Interval> {
    union(spans.iter().map(|s| (s.start, s.end)).collect())
}

pub(crate) fn carve(spans: &[&Span]) -> Vec<(String, f64)> {
    let mut classes: Vec<&str> = CLASS_ORDER.to_vec();
    for s in spans {
        if !classes.contains(&s.class.as_str()) {
            classes.push(&s.class);
        }
    }
    let mut covered: Vec<Interval> = Vec::new();
    let mut out = Vec::new();
    for class in classes {
        let own: Vec<Interval> = spans.iter().filter(|s| s.class == class).map(|s| (s.start, s.end)).collect();
        if own.is_empty() {
            continue;
        }
        let before = total_secs(&covered);
        covered = union(covered.into_iter().chain(own).collect());
        out.push((class.to_string(), total_secs(&covered) - before));
    }
    out
}

async fn agent_seconds(st: &St, user_id: i64, (start, end): (f64, f64), projects: Option<&[String]>, hidden: &HashSet<String>) -> Res<f64> {
    let c = st.pool.get().await?;
    let rows = c
        .query(
            "SELECT s.start_ts, s.end_ts, COALESCE(a.canonical, s.project) FROM agent_spans s
             LEFT JOIN project_aliases a ON a.user_id = s.user_id AND a.alias = s.project
             WHERE s.user_id = $1 AND s.start_ts >= $2 AND s.start_ts < $3 AND s.end_ts > $4",
            &[&user_id, &(start - MAX_SPAN), &end, &start],
        )
        .await?;
    let iv = rows
        .iter()
        .filter(|r| counts(r.get(2), projects, hidden))
        .map(|r| (r.get::<_, f64>(0).max(start), r.get::<_, f64>(1).min(end)))
        .filter(|(s, e)| e > s)
        .collect();
    Ok(total_secs(&union(iv)))
}

/// Only gaps of at most two minutes between coding heartbeats count; a longer one contributes nothing.
async fn unique_seconds(st: &St, user_id: i64, (start, end): (f64, f64), projects: &[String], hidden: &HashSet<String>) -> Res<i64> {
    let names: Vec<String> = projects.iter().filter(|p| !hidden.contains(*p)).cloned().collect();
    let c = st.pool.get().await?;
    let rows = c
        .query(
            "SELECT h.time FROM heartbeats h LEFT JOIN project_aliases a ON a.user_id = h.user_id AND a.alias = h.project
             WHERE h.user_id = $1 AND COALESCE(a.canonical, h.project) = ANY($2) AND COALESCE(h.category, 'coding') = 'coding'
             AND h.time >= $3 AND h.time <= $4 AND h.time >= 0 AND h.time <= $5 ORDER BY h.time",
            &[&user_id, &names, &start, &end, &MAX_TIME],
        )
        .await?;
    let times: Vec<f64> = rows.iter().map(|r| r.get(0)).collect();
    let secs: f64 = times.windows(2).map(|w| w[1] - w[0]).filter(|g| *g > 0.0 && *g <= HEARTBEAT_TIMEOUT).sum();
    Ok(secs.floor() as i64)
}

async fn stats(
    State(st): State<St>,
    h: HeaderMap,
    ext: Extensions,
    Path(ident): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Res<Json<Value>> {
    let p = P(q);
    let caller = authenticate(&st, &h, &ext, &p).await?;
    let (t, access) = open(&st, &caller, &ident, "stats").await?;
    let u = &t.user;

    let start = strict(&p, "start_date")?;
    let end = strict(&p, "end_date")?;
    let projects = canonical_filter(&st, u.id, p.get("filter_by_project").map(csv)).await?;
    let features = p.raw("features").map_or_else(|| vec!["languages".to_string()], csv);
    let limit = p.raw("limit").map_or(0, to_i).max(0) as usize;
    let no_ai = p.is_true("no_ai_coding");
    let test = p.is_true("test_param");
    let hidden = hidden_for(&st, &t, &access).await?;
    let not_ai = |class: &str| !no_ai || category(class) != "ai coding";

    if p.is_true("total_seconds") {
        // Only this branch reads filter_by_category, and not in strict (test_param) mode. Kairos credit is
        // already clipped to the window, so boundary_aware has nothing left to change.
        let cats = if test { None } else { p.get("filter_by_category").map(csv) };
        let win = if test {
            (start.unwrap_or_else(|| years_ago(10)).floor(), end.map_or(next_midnight() - 1.0, f64::floor))
        } else {
            (start.unwrap_or_else(|| years_ago(10)), end.unwrap_or_else(next_midnight))
        };
        let keep = |class: &str| not_ai(class) && cats.as_ref().is_none_or(|c| c.iter().any(|x| x == category(class)));
        let spans = credited(&st, u.id, win, projects.as_deref(), &hidden, keep).await?;
        return Ok(Json(json!({ "total_seconds": total_secs(&intervals(&spans)).round() as i64 })));
    }

    let win = (start.unwrap_or_else(|| years_ago(10)).floor(), end.map_or(next_midnight() - 1.0, f64::floor));
    let spans = credited(&st, u.id, win, projects.as_deref(), &hidden, not_ai).await?;
    let credit = intervals(&spans);
    let total = total_secs(&credit).round() as i64;

    let (start_i, end_i) = (win.0 as i64, win.1 as i64);
    // Whole days only, and an integer average, as Hackatime computes them.
    let days = (end_i - start_i).div_euclid(86_400);
    let average = if days == 0 { 0 } else { total / days };
    let mut data = json!({
        "username": u.display_name.as_deref().unwrap_or(&u.username),
        "user_id": u.id.to_string(),
        "is_coding_activity_visible": true,
        "is_other_usage_visible": true,
        "status": "ok",
        "start": iso(win.0),
        "end": iso(win.1),
        "range": "all_time",
        "human_readable_range": "All Time",
        "total_seconds": total,
        "daily_average": average,
        "human_readable_total": detailed(total),
        "human_readable_daily_average": detailed(average),
    });
    if features.iter().any(|f| f == "languages") {
        data["languages"] = json!(chunk(&languages(&st, u.id, &credit, win.0, win.1).await?, total, limit, true));
    }
    if features.iter().any(|f| f == "projects") {
        let by_project = by(&spans.iter().collect::<Vec<_>>(), |s| s.project.clone().unwrap_or_default());
        data["projects"] = json!(chunk(&by_project, total, limit, false));
    }
    if let Some(list) = projects.as_deref().filter(|_| p.raw("features").is_some_and(|f| f.contains("projects"))) {
        data["unique_total_seconds"] = json!(unique_seconds(&st, u.id, win, list, &hidden).await?);
    }
    let today = Utc::now().with_timezone(&u.tz()).date_naive();
    let streak = {
        let c = st.pool.get().await?;
        streak::current(&c, u.id, today).await?
    };
    data["streak"] = json!(streak);

    let carved = carve(&spans.iter().collect::<Vec<_>>());
    let mut cats: BTreeMap<&str, f64> = CATEGORIES.iter().map(|c| (*c, 0.0)).collect();
    for (class, secs) in &carved {
        *cats.entry(category(class)).or_default() += secs;
    }
    let kairos = json!({
        "focus_seconds": carved.iter().map(|(c, s)| (c.clone(), s.round() as i64)).collect::<BTreeMap<_, _>>(),
        "categories": cats.into_iter().map(|(c, s)| (c, s.round() as i64)).collect::<BTreeMap<_, _>>(),
        "agent_seconds": agent_seconds(&st, u.id, win, projects.as_deref(), &hidden).await?.round() as i64,
    });
    data["kairos"] = kairos;
    Ok(Json(json!({ "data": data, "trust_factor": trust_json(&t.trust) })))
}

async fn heartbeat_projects(st: &St, user_id: i64, (start, end): (f64, f64)) -> Res<BTreeSet<String>> {
    let c = st.pool.get().await?;
    let rows = c
        .query(
            "SELECT DISTINCT COALESCE(a.canonical, h.project) FROM heartbeats h
             LEFT JOIN project_aliases a ON a.user_id = h.user_id AND a.alias = h.project
             WHERE h.user_id = $1 AND h.project IS NOT NULL AND h.project <> ''
             AND h.time >= $2 AND h.time <= $3 AND h.time >= 0 AND h.time <= $4",
            &[&user_id, &start, &end, &MAX_TIME],
        )
        .await?;
    Ok(rows.iter().map(|r| r.get(0)).collect())
}

async fn projects(
    State(st): State<St>,
    h: HeaderMap,
    ext: Extensions,
    Path(ident): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Res<Json<Value>> {
    let p = P(q);
    let caller = authenticate(&st, &h, &ext, &p).await?;
    let (t, access) = open(&st, &caller, &ident, "projects").await?;
    let hidden = hidden_for(&st, &t, &access).await?;
    let since = lenient(&p, &["since", "start", "start_date"], ((now() - 30.0 * DAY) / DAY).floor() * DAY);
    let until = lenient(&p, &["until", "until_date", "end", "end_date"], now());
    let mut names = heartbeat_projects(&st, t.user.id, (since, until)).await?;
    names.extend(credited(&st, t.user.id, (since, until), None, &hidden, |_| true).await?.into_iter().filter_map(|s| s.project));
    names.retain(|n| !hidden.contains(n));
    Ok(Json(json!({ "projects": names })))
}

async fn project_rows(st: &St, t: &Target, hidden: &HashSet<String>, win: (f64, f64), only: Option<&[String]>) -> Res<Vec<Value>> {
    let only = canonical_filter(st, t.user.id, only.map(<[String]>::to_vec)).await?;
    let only = only.as_deref();
    let spans = credited(st, t.user.id, win, only, hidden, |_| true).await?;
    let seconds: HashMap<String, f64> = by(&spans.iter().collect::<Vec<_>>(), |s| s.project.clone().unwrap_or_default()).into_iter().collect();
    let mut seen: HashMap<String, (f64, f64)> = HashMap::new();
    for s in &spans {
        let e = seen.entry(s.project.clone().unwrap_or_default()).or_insert((s.start, s.end));
        e.0 = e.0.min(s.start);
        e.1 = e.1.max(s.end);
    }

    let c = st.pool.get().await?;
    let only_names: Option<Vec<String>> = only.map(<[String]>::to_vec);
    let beats = c
        .query(
            "SELECT COALESCE(a.canonical, h.project), count(*), min(h.time), max(h.time),
                    array_agg(DISTINCT h.language ORDER BY h.language) FILTER (WHERE h.language IS NOT NULL AND h.language <> '')
             FROM heartbeats h LEFT JOIN project_aliases a ON a.user_id = h.user_id AND a.alias = h.project
             WHERE h.user_id = $1 AND h.project IS NOT NULL AND h.project <> ''
               AND h.time >= $2 AND h.time <= $3 AND h.time >= 0 AND h.time <= $4
               AND ($5::text[] IS NULL OR COALESCE(a.canonical, h.project) = ANY($5))
             GROUP BY 1",
            &[&t.user.id, &win.0, &win.1, &MAX_TIME, &only_names],
        )
        .await?;
    let repos: HashMap<String, Option<String>> =
        c.query("SELECT id, repo_url FROM projects WHERE user_id = $1", &[&t.user.id]).await?.iter().map(|r| (r.get(0), r.get(1))).collect();

    struct Beats {
        count: i64,
        first: f64,
        last: f64,
        languages: Vec<String>,
    }
    let beats: HashMap<String, Beats> = beats
        .iter()
        .map(|r| (r.get(0), Beats { count: r.get(1), first: r.get(2), last: r.get(3), languages: r.get::<_, Option<Vec<String>>>(4).unwrap_or_default() }))
        .collect();

    let names: BTreeSet<&String> = seconds.keys().chain(beats.keys()).filter(|n| !hidden.contains(*n)).collect();
    let mut rows: Vec<(i64, Value)> = names
        .into_iter()
        .map(|name| {
            let b = beats.get(name);
            let s = seen.get(name);
            let first = [b.map(|b| b.first), s.map(|s| s.0)].into_iter().flatten().fold(f64::INFINITY, f64::min);
            let last = [b.map(|b| b.last), s.map(|s| s.1)].into_iter().flatten().fold(f64::NEG_INFINITY, f64::max);
            let total = seconds.get(name).map_or(0, |s| s.round() as i64);
            let row = json!({
                "name": name,
                "total_seconds": total,
                "languages": b.map(|b| b.languages.clone()).unwrap_or_default(),
                "repo_url": repos.get(name).cloned().flatten(),
                "total_heartbeats": b.map_or(0, |b| b.count),
                "first_heartbeat": iso(first),
                "last_heartbeat": iso(last),
                "most_recent_heartbeat": iso(last),
                "archived": false,
            });
            (total, row)
        })
        .collect();
    rows.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(rows.into_iter().map(|r| r.1).collect())
}

fn details_window(p: &P) -> (f64, f64) {
    (lenient(p, &["start", "start_date"], years_ago(1)), lenient(p, &["end", "end_date"], now()))
}

async fn project(
    State(st): State<St>,
    h: HeaderMap,
    ext: Extensions,
    Path((ident, name)): Path<(String, String)>,
    Query(q): Query<HashMap<String, String>>,
) -> Res<Json<Value>> {
    let p = P(q);
    let caller = authenticate(&st, &h, &ext, &p).await?;
    let (t, access) = open(&st, &caller, &ident, "project").await?;
    if name.trim().is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "whats the name?"));
    }
    let hidden = hidden_for(&st, &t, &access).await?;
    let rows = project_rows(&st, &t, &hidden, details_window(&p), Some(&[name])).await?;
    rows.into_iter().next().map(Json).ok_or_else(|| err(StatusCode::NOT_FOUND, "found nuthin"))
}

async fn projects_details(
    State(st): State<St>,
    h: HeaderMap,
    ext: Extensions,
    Path(ident): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Res<Json<Value>> {
    let p = P(q);
    let caller = authenticate(&st, &h, &ext, &p).await?;
    let (t, access) = open(&st, &caller, &ident, "projects_details").await?;
    let hidden = hidden_for(&st, &t, &access).await?;
    let names: Vec<String> = p.get("projects").map(csv).unwrap_or_default().into_iter().map(|n| n.trim().to_string()).filter(|n| !n.is_empty()).collect();
    let only = (!names.is_empty()).then_some(names.as_slice());
    Ok(Json(json!({ "projects": project_rows(&st, &t, &hidden, details_window(&p), only).await? })))
}

async fn heartbeat_spans(
    State(st): State<St>,
    h: HeaderMap,
    ext: Extensions,
    Path(ident): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Res<Json<Value>> {
    let p = P(q);
    let caller = authenticate(&st, &h, &ext, &p).await?;
    let (t, access) = open(&st, &caller, &ident, "heartbeats_spans").await?;
    // When someone was at work, to the second, is not for strangers, public profile or not.
    if !(access.owner || access.program) {
        return Err(err(StatusCode::FORBIDDEN, "spans are only shown to their owner and to programs"));
    }
    let start = strict(&p, "start_date")?.unwrap_or_else(|| years_ago(10));
    let end = strict(&p, "end_date")?.unwrap_or_else(next_midnight);
    let projects = p.get("project").map(|s| vec![s.to_string()]).or_else(|| p.get("filter_by_project").map(csv));
    let projects = canonical_filter(&st, t.user.id, projects).await?;
    let hidden = hidden_for(&st, &t, &access).await?;
    let spans = credited(&st, t.user.id, (start, end), projects.as_deref(), &hidden, |_| true).await?;
    let list: Vec<Value> = intervals(&spans)
        .into_iter()
        .map(|(s, e)| (s, e, (e - s).round() as i64))
        .filter(|(_, _, d)| *d > 0)
        .map(|(s, e, d)| json!({ "start_time": s, "end_time": e, "duration": d }))
        .collect();
    Ok(Json(json!({ "spans": list })))
}

/// Public to everyone, private profiles included, like Hackatime's.
async fn trust_factor(State(st): State<St>, h: HeaderMap, ext: Extensions, Path(ident): Path<String>) -> Res<Json<Value>> {
    limits::check(&st, Bucket::Api, &limits::ip_key(limits::client_ip(&ext, &h, &st.config.trusted_proxies)))?;
    let t = find(&st, &ident).await?.ok_or_else(|| err(StatusCode::NOT_FOUND, "User not found"))?;
    Ok(Json(trust_json(&t.trust)))
}

async fn board(st: St, h: HeaderMap, ext: Extensions, q: HashMap<String, String>, period: Period) -> Res<Json<Value>> {
    let p = P(q);
    authenticate(&st, &h, &ext, &p).await?;
    let limit = p.get("limit").map_or(1000, to_i).clamp(1, 5000);
    let c = st.pool.get().await?;
    let entries = leaderboard::top(&c, period, limit).await?;
    let today = Utc::now().date_naive();
    let (name, range) = match period {
        Period::Today => ("daily", "Today".to_string()),
        Period::Week => {
            let from = today.checked_sub_days(chrono::Days::new(6)).unwrap_or(today);
            ("last_7_days", format!("{} - {}", from.format("%b %-d"), today.format("%b %-d, %Y")))
        }
    };
    let rows: Vec<Value> = entries
        .iter()
        .map(|e| {
            json!({
                "rank": e.rank,
                "user": {
                    "id": e.user_id,
                    "username": e.display_name.as_deref().unwrap_or(&e.username),
                    "handle": e.username,
                    "avatar_url": e.avatar_url,
                },
                "total_seconds": e.seconds.round() as i64,
            })
        })
        .collect();
    Ok(Json(json!({
        "period": name,
        "start_date": today.format("%Y-%m-%d").to_string(),
        "date_range": range,
        "generated_at": iso(now()),
        "entries": rows,
    })))
}

async fn daily_board(State(st): State<St>, h: HeaderMap, ext: Extensions, Query(q): Query<HashMap<String, String>>) -> Res<Json<Value>> {
    board(st, h, ext, q, Period::Today).await
}

async fn weekly_board(State(st): State<St>, h: HeaderMap, ext: Extensions, Query(q): Query<HashMap<String, String>>) -> Res<Json<Value>> {
    board(st, h, ext, q, Period::Week).await
}

async fn lookup(st: &St, h: &HeaderMap, ext: &Extensions, q: HashMap<String, String>, column: &str, endpoint: &str, value: &str) -> Res<Response> {
    let caller = authenticate(st, h, ext, &P(q)).await?;
    let Caller::Program(key) = caller else {
        return Err(err(StatusCode::UNAUTHORIZED, "Unauthorized"));
    };
    let sql = if column == "email" {
        "SELECT id FROM users WHERE lower(email) = lower($1) ORDER BY id LIMIT 1"
    } else {
        "SELECT id FROM users WHERE slack_id = $1 ORDER BY id LIMIT 1"
    };
    let field = if column == "email" { "email" } else { "slack_uid" };
    let found = st.pool.get().await?.query_opt(sql, &[&value]).await?;
    let Some(r) = found else {
        let mut body = json!({ "error": "User not found" });
        body[field] = json!(value);
        return Ok((StatusCode::NOT_FOUND, Json(body)).into_response());
    };
    let id: i64 = r.get(0);
    programs::audit_read(st, &key, id, endpoint).await?;
    let mut body = json!({ "user_id": id });
    body[field] = json!(value);
    Ok(Json(body).into_response())
}

async fn lookup_slack_uid(
    State(st): State<St>,
    h: HeaderMap,
    ext: Extensions,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Res<Response> {
    lookup(&st, &h, &ext, q, "slack_id", "lookup_slack_uid", &id).await
}

async fn lookup_email(
    State(st): State<St>,
    h: HeaderMap,
    ext: Extensions,
    Path(email): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Res<Response> {
    lookup(&st, &h, &ext, q, "email", "lookup_email", &email).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_parse_like_rails_in_utc() {
        assert_eq!(parse_time("2026-06-01"), Some(1_780_272_000.0));
        assert_eq!(parse_time("2026-06-01T00:00:00Z"), Some(1_780_272_000.0));
        assert_eq!(parse_time("2026-06-01 00:00:00"), Some(1_780_272_000.0));
        assert_eq!(parse_time("2026-06-01T02:00:00+02:00"), Some(1_780_272_000.0));
        assert_eq!(parse_time("2026-06-01T00:00:00.5Z"), Some(1_780_272_000.5));
        assert_eq!(parse_time("2026-06-01T00:00"), Some(1_780_272_000.0));
        assert_eq!(parse_time("yesterday"), None);
        assert_eq!(parse_time("2026-13-01"), None);
    }

    #[test]
    fn durations_read_like_hackatime() {
        assert_eq!(detailed(0), "");
        assert_eq!(detailed(3600), "1h");
        assert_eq!(detailed(18_610), "5h 10m 10s");
        assert_eq!(simple(0), "0m");
        assert_eq!(simple(14_820), "4h 7m");
        assert_eq!(digital(9000), "02:30:00");
    }

    #[test]
    fn ruby_style_parsing() {
        assert_eq!(to_i("5x"), 5);
        assert_eq!(to_i("abc"), 0);
        assert_eq!(to_i("-3"), -3);
        assert_eq!(csv("a,b,,"), vec!["a", "b"]);
        assert!(csv(",").is_empty());
    }

    #[test]
    fn classes_map_to_categories() {
        for (c, cat) in [("typing", "ai coding"), ("reading", "ai coding"), ("editing", "coding"), ("terminal", "coding"), ("testing", "running tests"), ("bridge", "ai coding"), ("trip", "ai coding")] {
            assert_eq!(category(c), cat);
        }
    }
}
