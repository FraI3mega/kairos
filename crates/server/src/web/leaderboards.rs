use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use askama::Template;
use axum::extract::{Query, State};
use axum::response::Response;
use chrono::{Days, Utc};
use serde::Deserialize;
use tokio::sync::Mutex;
use tokio::time::Instant;

use super::{avatar, render, Base, Ctx};
use crate::credit::local_day;
use crate::leaderboard::{self, Entry, Period};
use crate::live::streak_label;
use crate::streak::{self, DAY_SECONDS, MAX_DAYS};
use crate::util::{short, Res};
use crate::St;

const TTL: Duration = Duration::from_secs(60);
const LIMIT: i64 = 100;

pub struct Line {
    pub rank: u32,
    pub username: String,
    pub name: String,
    pub avatar: String,
    pub time: String,
    pub streak: String,
}

pub struct Boards {
    today: Vec<Line>,
    week: Vec<Line>,
}

impl Boards {
    pub fn pick(&self, week: bool) -> &[Line] {
        if week {
            &self.week
        } else {
            &self.today
        }
    }
}

/// Kept for a minute so a crowd refreshing costs one set of queries.
#[derive(Default)]
pub struct Cache(Mutex<Option<(Instant, Arc<Boards>)>>);

impl Cache {
    pub async fn get(&self, st: &St) -> Res<Arc<Boards>> {
        let mut slot = self.0.lock().await;
        if let Some((at, boards)) = &*slot {
            if at.elapsed() < TTL {
                return Ok(boards.clone());
            }
        }
        let boards = Arc::new(compute(st).await?);
        *slot = Some((Instant::now(), boards.clone()));
        Ok(boards)
    }
}

async fn compute(st: &St) -> Res<Boards> {
    let c = st.pool.get().await?;
    let today = leaderboard::top(&c, Period::Today, LIMIT).await?;
    let week = leaderboard::top(&c, Period::Week, LIMIT).await?;
    let ids: Vec<i64> = today.iter().chain(&week).map(|e| e.user_id).collect::<HashSet<_>>().into_iter().collect();

    let zones: HashMap<i64, chrono_tz::Tz> = c
        .query("SELECT id, timezone FROM users WHERE id = ANY($1)", &[&ids])
        .await?
        .iter()
        .map(|r| (r.get(0), r.get::<_, String>(1).parse().unwrap_or(chrono_tz::UTC)))
        .collect();
    // Two days of slack cover every timezone.
    let from = Utc::now().date_naive().checked_sub_days(Days::new(MAX_DAYS + 2)).unwrap_or(chrono::NaiveDate::MIN);
    let mut qualifying: HashMap<i64, HashSet<chrono::NaiveDate>> = HashMap::new();
    for r in c
        .query(
            "SELECT user_id, day FROM user_days WHERE user_id = ANY($1) AND day >= $2 AND class <> 'agent'
             GROUP BY user_id, day HAVING sum(seconds) >= $3",
            &[&ids, &from, &DAY_SECONDS],
        )
        .await?
    {
        qualifying.entry(r.get(0)).or_default().insert(r.get(1));
    }
    let t = crate::util::now();
    let streaks: HashMap<i64, u32> = ids
        .iter()
        .map(|id| {
            let tz = zones.get(id).copied().unwrap_or(chrono_tz::UTC);
            (*id, qualifying.get(id).map_or(0, |days| streak::count(days, local_day(tz, t))))
        })
        .collect();

    let lines = |list: Vec<Entry>| {
        list.into_iter()
            .map(|e| Line {
                rank: e.rank,
                name: e.display_name.clone().unwrap_or_else(|| e.username.clone()),
                avatar: avatar(&e.username),
                time: short(e.seconds),
                streak: streak_label(streaks.get(&e.user_id).copied().unwrap_or(0)),
                username: e.username,
            })
            .collect()
    };
    Ok(Boards { today: lines(today), week: lines(week) })
}

#[derive(Deserialize)]
pub struct PeriodQ {
    period: Option<String>,
}

#[derive(Template)]
#[template(path = "leaderboards.html")]
struct Page {
    base: Base,
    week: bool,
    boards: Arc<Boards>,
}

pub async fn page(State(st): State<St>, ctx: Ctx, Query(q): Query<PeriodQ>) -> Res<Response> {
    let week = q.period.as_deref().and_then(Period::parse) == Some(Period::Week);
    let boards = st.boards.get(&st).await?;
    let base = Base::new(&st, &ctx, "Leaderboards · Kairos").share(
        "Who put in the most focus time on Kairos today and over the last 7 days, from people with public profiles.",
        "/leaderboards",
        "website",
    );
    render(&Page { base, week, boards })
}
