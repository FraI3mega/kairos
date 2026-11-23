use std::collections::{BTreeMap, HashMap, HashSet};

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use chrono::NaiveDate;
use kairos_core::{total_secs, union, Interval};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{accounts, aliases};
use crate::auth::{User, USER_COLUMNS};
use crate::credit::{self, day_start, MAX_SPAN, RULE};
use crate::limits::{self, Bucket};
use crate::util::{bearer, err, now, Res};
use crate::St;

const HEARTBEAT_SPAN: f64 = 120.0;

#[derive(Deserialize, Default)]
pub struct RangeQ {
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub filter_by_project: Option<String>,
    pub project: Option<String>,
}

pub struct Span {
    pub start: f64,
    pub end: f64,
    pub project: Option<String>,
    pub class: String,
    pub device_id: i64,
}

pub async fn user_by_name(st: &St, username: &str) -> Res<Option<User>> {
    let c = st.pool.get().await?;
    let r = c
        .query_opt(&format!("SELECT {USER_COLUMNS} FROM users u WHERE u.username = $1"), &[&username.to_lowercase()])
        .await?;
    Ok(r.as_ref().map(User::from_row))
}

async fn visible(st: &St, h: &HeaderMap, ext: &axum::http::Extensions, username: &str) -> Res<(User, bool)> {
    let ip = limits::client_ip(ext, h, &st.config.trusted_proxies);
    limits::check(st, Bucket::Api, &limits::ip_key(ip))?;
    let u = user_by_name(st, username).await?.ok_or_else(|| err(StatusCode::NOT_FOUND, "no such user"))?;
    let owner = match bearer(h) {
        Some(key) => accounts::editor_key_user(st, &key).await? == Some(u.id),
        None => false,
    };
    if u.is_public || owner {
        return Ok((u, owner));
    }
    Err(err(StatusCode::NOT_FOUND, "no such user"))
}

pub async fn hidden_projects(st: &St, user_id: i64) -> Res<HashSet<String>> {
    let c = st.pool.get().await?;
    let rows = c.query("SELECT id FROM projects WHERE user_id = $1 AND hidden", &[&user_id]).await?;
    Ok(rows.iter().map(|r| r.get(0)).collect())
}

pub fn range(q: &RangeQ, u: &User) -> (f64, f64) {
    let tz = u.tz();
    let parse = |d: &str| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok();
    let start = q.start_date.as_deref().and_then(parse).map_or(0.0, |d| day_start(tz, d));
    let end = q
        .end_date
        .as_deref()
        .and_then(parse)
        .and_then(|d| d.succ_opt())
        .map_or(now() + 86400.0, |d| day_start(tz, d));
    (start, end)
}

pub async fn load_spans(st: &St, user_id: i64, start: f64, end: f64) -> Res<Vec<Span>> {
    let c = st.pool.get().await?;
    let rows = c
        .query(
            "SELECT s.start_ts, s.end_ts, COALESCE(a.canonical, s.project), s.class, s.device_id FROM spans s
             LEFT JOIN project_aliases a ON a.user_id = s.user_id AND a.alias = s.project
             WHERE s.user_id = $1 AND s.start_ts >= $2 AND s.start_ts < $3 AND s.end_ts > $4 ORDER BY s.start_ts",
            &[&user_id, &(start - MAX_SPAN), &end, &start],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|r| Span {
            start: r.get::<_, f64>(0).max(start),
            end: r.get::<_, f64>(1).min(end),
            project: r.get(2),
            class: r.get(3),
            device_id: r.get(4),
        })
        .collect())
}

/// Each second goes to one project across all of the user's devices, as `user_days` has it.
pub async fn load_carved(st: &St, user_id: i64, start: f64, end: f64) -> Res<Vec<Span>> {
    let pieces = load_spans(st, user_id, start, end)
        .await?
        .into_iter()
        .filter(|s| s.end > s.start)
        .map(|s| (s.device_id, credit::Piece { start: s.start, end: s.end, project: s.project, class: s.class }))
        .collect();
    Ok(credit::carve(pieces, credit::credit_rank)
        .into_iter()
        .map(|(device_id, p)| Span { start: p.start, end: p.end, project: p.project, class: p.class, device_id })
        .collect())
}

pub struct Piece {
    pub span: Span,
    pub rule: String,
}

pub struct AgentRun {
    pub start: f64,
    pub end: f64,
    pub project: Option<String>,
    pub device_id: i64,
}

// Uploads re-credit from the settle point, so a long session is stored as many spans that touch end to start.
// Joining exactly touching pieces of one device, class, project and rule leaves every union and total unchanged.
const JOINED_SPANS: &str = "
    SELECT min(start_ts), max(end_ts), project, class, device_id, rule FROM (
        SELECT *, sum(brk) OVER (PARTITION BY device_id ORDER BY start_ts, end_ts) AS grp FROM (
            SELECT s.device_id, s.start_ts, s.end_ts, COALESCE(a.canonical, s.project) AS project, s.class, s.rule,
                   CASE WHEN lag(s.end_ts) OVER w = s.start_ts AND lag(s.class) OVER w = s.class AND lag(s.rule) OVER w = s.rule
                         AND lag(COALESCE(a.canonical, s.project)) OVER w IS NOT DISTINCT FROM COALESCE(a.canonical, s.project) THEN 0 ELSE 1 END AS brk
            FROM spans s LEFT JOIN project_aliases a ON a.user_id = s.user_id AND a.alias = s.project
            WHERE s.user_id = $1 AND s.start_ts >= $2 AND s.start_ts < $3 AND s.end_ts > $4
            WINDOW w AS (PARTITION BY s.device_id ORDER BY s.start_ts, s.end_ts)
        ) a
    ) b GROUP BY device_id, grp, project, class, rule ORDER BY 1";

const JOINED_AGENT: &str = "
    SELECT min(start_ts), max(end_ts), project, device_id FROM (
        SELECT *, sum(brk) OVER (PARTITION BY device_id ORDER BY start_ts, end_ts) AS grp FROM (
            SELECT s.device_id, s.start_ts, s.end_ts, COALESCE(a.canonical, s.project) AS project,
                   CASE WHEN lag(s.end_ts) OVER w = s.start_ts
                         AND lag(COALESCE(a.canonical, s.project)) OVER w IS NOT DISTINCT FROM COALESCE(a.canonical, s.project) THEN 0 ELSE 1 END AS brk
            FROM agent_spans s LEFT JOIN project_aliases a ON a.user_id = s.user_id AND a.alias = s.project
            WHERE s.user_id = $1 AND s.start_ts >= $2 AND s.start_ts < $3 AND s.end_ts > $4
            WINDOW w AS (PARTITION BY s.device_id ORDER BY s.start_ts, s.end_ts)
        ) a
    ) b GROUP BY device_id, grp, project ORDER BY 1";

pub async fn load_pieces(st: &St, user_id: i64, start: f64, end: f64) -> Res<Vec<Piece>> {
    let c = st.pool.get().await?;
    let rows = c.query(JOINED_SPANS, &[&user_id, &(start - MAX_SPAN), &end, &start]).await?;
    Ok(rows
        .iter()
        .map(|r| Piece {
            span: Span {
                start: r.get::<_, f64>(0).max(start),
                end: r.get::<_, f64>(1).min(end),
                project: r.get(2),
                class: r.get(3),
                device_id: r.get(4),
            },
            rule: r.get(5),
        })
        .filter(|p| p.span.end > p.span.start)
        .collect())
}

pub async fn load_agent_runs(st: &St, user_id: i64, start: f64, end: f64) -> Res<Vec<AgentRun>> {
    let c = st.pool.get().await?;
    let rows = c.query(JOINED_AGENT, &[&user_id, &(start - MAX_SPAN), &end, &start]).await?;
    Ok(rows
        .iter()
        .map(|r| AgentRun { start: r.get::<_, f64>(0).max(start), end: r.get::<_, f64>(1).min(end), project: r.get(2), device_id: r.get(3) })
        .filter(|a| a.end > a.start)
        .collect())
}

fn overlap(a: &[Interval], s: f64, e: f64) -> f64 {
    let i = a.partition_point(|x| x.1 <= s);
    a[i..].iter().take_while(|x| x.0 < e).map(|x| x.1.min(e) - x.0.max(s)).filter(|d| *d > 0.0).sum()
}

/// Editor heartbeats never add time: they label credited time with a language.
pub async fn languages(st: &St, user_id: i64, credited: &[Interval], start: f64, end: f64) -> Res<Vec<(String, f64)>> {
    let c = st.pool.get().await?;
    let hb = c
        .query(
            "SELECT time, language FROM heartbeats WHERE user_id = $1 AND time >= $2 AND time < $3 AND language IS NOT NULL ORDER BY time",
            &[&user_id, &(start - HEARTBEAT_SPAN), &end],
        )
        .await?;
    let mut langs: HashMap<String, f64> = HashMap::new();
    for (i, r) in hb.iter().enumerate() {
        let t: f64 = r.get(0);
        let next = hb.get(i + 1).map_or(t + HEARTBEAT_SPAN, |n| n.get::<_, f64>(0));
        let secs = overlap(credited, t, next.min(t + HEARTBEAT_SPAN));
        if secs > 0.0 {
            *langs.entry(r.get(1)).or_default() += secs;
        }
    }
    let mut out: Vec<(String, f64)> = langs.into_iter().collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1));
    Ok(out)
}

pub struct Summary {
    pub total: f64,
    pub projects: Vec<(String, f64)>,
    pub classes: Vec<(String, f64)>,
    pub languages: Vec<(String, f64)>,
}

pub fn by<K: Ord + Clone>(spans: &[&Span], key: impl Fn(&Span) -> K) -> Vec<(K, f64)> {
    let mut groups: BTreeMap<K, Vec<Interval>> = BTreeMap::new();
    for s in spans {
        groups.entry(key(s)).or_default().push((s.start, s.end));
    }
    let mut out: Vec<(K, f64)> = groups.into_iter().map(|(k, iv)| (k, total_secs(&union(iv)))).collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1));
    out
}

/// Project time only (Other is left out, as programs count it), hidden projects removed for
/// anyone but the owner.
pub async fn summarize(st: &St, u: &User, start: f64, end: f64, filter: &[String], hidden: &HashSet<String>) -> Res<Summary> {
    let all = load_carved(st, u.id, start, end).await?;
    let spans: Vec<&Span> = all
        .iter()
        .filter(|s| s.project.as_ref().is_some_and(|p| !hidden.contains(p)))
        .filter(|s| filter.is_empty() || s.project.as_ref().is_some_and(|p| filter.contains(p)))
        .collect();
    let credited = union(spans.iter().map(|s| (s.start, s.end)).collect());
    Ok(Summary {
        total: total_secs(&credited),
        projects: by(&spans, |s| s.project.clone().unwrap_or_default()),
        classes: by(&spans, |s| s.class.clone()),
        languages: languages(st, u.id, &credited, start, end).await?,
    })
}

pub async fn spans(
    State(st): State<St>,
    h: HeaderMap,
    ext: axum::http::Extensions,
    Path(username): Path<String>,
    Query(q): Query<RangeQ>,
) -> Res<Json<Value>> {
    let (u, owner) = visible(&st, &h, &ext, &username).await?;
    if !owner {
        return Err(err(StatusCode::FORBIDDEN, "spans are only shown to their owner for now"));
    }
    let (start, end) = range(&q, &u);
    let project = match q.project.as_deref() {
        Some(p) => Some(aliases::canonical(&aliases::map(&st.pool.get().await?, u.id).await?, p).to_string()),
        None => None,
    };
    let list: Vec<Value> = load_spans(&st, u.id, start, end)
        .await?
        .into_iter()
        .filter(|s| project.is_none() || s.project == project)
        .map(|s| json!({ "start": s.start, "end": s.end, "project": s.project, "class": s.class, "device_id": s.device_id }))
        .collect();
    let c = st.pool.get().await?;
    let rows = c
        .query(
            "SELECT e.kind, COUNT(*) FROM events e JOIN devices d ON d.id = e.device_id
             LEFT JOIN project_aliases a ON a.user_id = d.user_id AND a.alias = e.project
             WHERE d.user_id = $1 AND e.ts >= $2 AND e.ts < $3 AND ($4::text IS NULL OR COALESCE(a.canonical, e.project) = $4) GROUP BY e.kind",
            &[&u.id, &start, &end, &project],
        )
        .await?;
    let counts: serde_json::Map<String, Value> = rows.iter().map(|r| (r.get::<_, String>(0), json!(r.get::<_, i64>(1)))).collect();
    Ok(Json(json!({ "rule": RULE.name(), "spans": list, "event_counts": counts })))
}
