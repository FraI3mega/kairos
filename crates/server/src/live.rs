use std::collections::{BTreeMap, HashMap};
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use chrono::{Days, NaiveDate};
use futures_util::stream;
use serde::Serialize;
use tokio::sync::broadcast;
use tokio::time::Instant;

use crate::api::stats::load_spans;
use crate::auth::{MaybeSignedIn, User, USER_COLUMNS};
use crate::credit::{day_start, local_day};
use crate::streak;
use crate::util::{err, human, now, short, Res};
use crate::St;

pub const MAX_STREAMS: usize = 3;
const KEEPALIVE: Duration = Duration::from_secs(25);
// Browsers reconnect on their own, which also re-checks the session.
const MAX_AGE: Duration = Duration::from_secs(30 * 60);

#[derive(Serialize)]
pub struct Bar {
    pub label: String,
    pub title: String,
    pub focus: f64,
    pub agent: f64,
    pub today: bool,
}

#[derive(Serialize)]
pub struct Snapshot {
    pub now: f64,
    pub day_start: f64,
    pub today: String,
    pub agent: String,
    pub week: String,
    pub agent_week: String,
    pub streak: String,
    pub bars: Vec<Bar>,
    /// Today's focus as `[start hour, end hour, class]`, hours counted from local midnight.
    pub spans: Vec<(f64, f64, String)>,
}

pub async fn week(st: &St, user_id: i64, today: NaiveDate) -> Res<Vec<(NaiveDate, f64, f64)>> {
    let first = today.checked_sub_days(Days::new(6)).unwrap_or(today);
    let c = st.pool.get().await?;
    let rows = c
        .query(
            "SELECT day, class = 'agent', sum(seconds)::float8 FROM user_days
             WHERE user_id = $1 AND day >= $2 AND day <= $3 GROUP BY 1, 2",
            &[&user_id, &first, &today],
        )
        .await?;
    let mut per: BTreeMap<NaiveDate, (f64, f64)> = BTreeMap::new();
    for r in &rows {
        let e = per.entry(r.get(0)).or_default();
        if r.get(1) {
            e.1 += r.get::<_, f64>(2);
        } else {
            e.0 += r.get::<_, f64>(2);
        }
    }
    Ok((0..7)
        .rev()
        .filter_map(|i| today.checked_sub_days(Days::new(i)))
        .map(|d| {
            let (f, a) = per.get(&d).copied().unwrap_or_default();
            (d, f, a)
        })
        .collect())
}

pub fn bars(days: &[(NaiveDate, f64, f64)], agents: bool) -> Vec<Bar> {
    let max = days.iter().map(|d| if agents { d.1.max(d.2) } else { d.1 }).fold(0.0, f64::max);
    let pct = |v: f64| if max > 0.0 { (v / max * 1000.0).round() / 10.0 } else { 0.0 };
    days.iter()
        .enumerate()
        .map(|(i, &(d, f, a))| {
            let last = i + 1 == days.len();
            let mut title = format!("{} · focus {}", d.format("%a %b %-d"), human(f));
            if agents {
                title.push_str(&format!(" · agents ran {}", human(a)));
            }
            Bar {
                label: if last { "Today".into() } else { d.format("%a").to_string() },
                title,
                focus: pct(f),
                agent: if agents { pct(a) } else { 0.0 },
                today: last,
            }
        })
        .collect()
}

pub fn days_text(n: u32) -> String {
    format!("{n} {}", if n == 1 { "day" } else { "days" })
}

pub fn streak_label(n: u32) -> String {
    format!("{n}-day streak")
}

pub async fn snapshot(st: &St, u: &User) -> Res<Snapshot> {
    let tz = u.tz();
    let t = now();
    let today = local_day(tz, t);
    let day0 = day_start(tz, today);
    let days = week(st, u.id, today).await?;
    let spans = load_spans(st, u.id, day0, t + 60.0).await?;
    let streak = {
        let c = st.pool.get().await?;
        streak::current(&c, u.id, today).await?
    };
    let (f, a) = days.last().map_or((0.0, 0.0), |d| (d.1, d.2));
    Ok(Snapshot {
        now: t,
        day_start: day0,
        today: short(f),
        agent: short(a),
        week: short(days.iter().map(|d| d.1).sum()),
        agent_week: short(days.iter().map(|d| d.2).sum()),
        streak: days_text(streak),
        bars: bars(&days, true),
        spans: spans.iter().map(|s| ((s.start - day0) / 3600.0, (s.end - day0) / 3600.0, s.class.clone())).collect(),
    })
}

#[derive(Default)]
pub struct Hub {
    chans: Mutex<HashMap<i64, Chan>>,
}

struct Chan {
    tx: broadcast::Sender<Arc<str>>,
    streams: usize,
    ticket: u64,
    sent: u64,
}

impl Hub {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<i64, Chan>> {
        self.chans.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn join(&self, user_id: i64) -> Option<broadcast::Receiver<Arc<str>>> {
        let mut m = self.lock();
        let c = m.entry(user_id).or_insert_with(|| Chan { tx: broadcast::channel(4).0, streams: 0, ticket: 0, sent: 0 });
        if c.streams >= MAX_STREAMS {
            return None;
        }
        c.streams += 1;
        Some(c.tx.subscribe())
    }

    fn leave(&self, user_id: i64) {
        let mut m = self.lock();
        if let Some(c) = m.get_mut(&user_id) {
            c.streams = c.streams.saturating_sub(1);
            if c.streams == 0 {
                m.remove(&user_id);
            }
        }
    }

    pub fn streams(&self, user_id: i64) -> usize {
        self.lock().get(&user_id).map_or(0, |c| c.streams)
    }

    fn begin(&self, user_id: i64) -> Option<u64> {
        let mut m = self.lock();
        let c = m.get_mut(&user_id)?;
        c.ticket += 1;
        Some(c.ticket)
    }

    /// Sends the snapshot unless a newer one has already gone out.
    fn finish(&self, user_id: i64, ticket: u64, msg: Arc<str>) {
        let mut m = self.lock();
        if let Some(c) = m.get_mut(&user_id).filter(|c| c.sent < ticket) {
            c.sent = ticket;
            let _ = c.tx.send(msg);
        }
    }
}

/// Call after the upload's transaction has committed.
pub fn publish(st: &St, user_id: i64) {
    let Some(ticket) = st.live.begin(user_id) else { return };
    let st = st.clone();
    tokio::spawn(async move {
        let sent = async {
            let c = st.pool.get().await?;
            let row = c.query_one(&format!("SELECT {USER_COLUMNS} FROM users u WHERE u.id = $1"), &[&user_id]).await?;
            drop(c);
            let snap = snapshot(&st, &User::from_row(&row)).await.map_err(|e| anyhow::anyhow!("{}", e.body))?;
            anyhow::Ok(serde_json::to_string(&snap)?)
        }
        .await;
        match sent {
            Ok(json) => st.live.finish(user_id, ticket, json.into()),
            Err(e) => tracing::warn!(error = format!("{e:#}"), "live snapshot failed"),
        }
    });
}

struct Watch {
    st: St,
    user_id: i64,
    rx: broadcast::Receiver<Arc<str>>,
    first: Option<String>,
    until: Instant,
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.st.live.leave(self.user_id);
    }
}

pub async fn stream(State(st): State<St>, MaybeSignedIn(s): MaybeSignedIn) -> Res<Response> {
    let s = s.ok_or_else(|| err(StatusCode::UNAUTHORIZED, "sign in first"))?;
    let rx = st
        .live
        .join(s.user.id)
        .ok_or_else(|| err(StatusCode::TOO_MANY_REQUESTS, "Too many dashboards are open. Close one and try again."))?;
    let mut watch = Watch { st: st.clone(), user_id: s.user.id, rx, first: None, until: Instant::now() + MAX_AGE };
    watch.first = Some(serde_json::to_string(&snapshot(&st, &s.user).await?)?);

    let events = stream::unfold(watch, |mut w| async move {
        let data = match w.first.take() {
            Some(first) => first,
            None => loop {
                tokio::select! {
                    got = w.rx.recv() => match got {
                        Ok(m) => break m.to_string(),
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => return None,
                    },
                    () = tokio::time::sleep_until(w.until) => return None,
                }
            },
        };
        Some((Ok::<_, Infallible>(Event::default().event("snapshot").data(data)), w))
    });
    let mut res = Sse::new(events).keep_alive(KeepAlive::new().interval(KEEPALIVE).text("keepalive")).into_response();
    res.headers_mut().insert("x-accel-buffering", HeaderValue::from_static("no"));
    res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn streak_labels_are_hyphenated() {
        assert_eq!((streak_label(0).as_str(), streak_label(1).as_str(), streak_label(4).as_str()), ("0-day streak", "1-day streak", "4-day streak"));
    }

    #[test]
    fn bars_scale_to_the_busiest_value() {
        let days = [(d("2026-11-19"), 3600.0, 7200.0), (d("2026-11-20"), 1800.0, 0.0)];
        let b = bars(&days, true);
        assert_eq!((b[0].focus, b[0].agent, b[1].focus), (50.0, 100.0, 25.0));
        assert_eq!((b[0].label.as_str(), b[1].label.as_str()), ("Thu", "Today"));
        let b = bars(&days, false);
        assert_eq!((b[0].focus, b[0].agent), (100.0, 0.0));
        assert!(!b[0].title.contains("agents"));
        assert_eq!(bars(&[(d("2026-11-20"), 0.0, 0.0)], true)[0].focus, 0.0);
    }

    #[test]
    fn hub_caps_streams_and_orders_snapshots() {
        let hub = Hub::default();
        let _a = hub.join(1).unwrap();
        let _b = hub.join(1).unwrap();
        let mut c = hub.join(1).unwrap();
        assert!(hub.join(1).is_none());
        assert!(hub.join(2).is_some());
        hub.leave(1);
        assert!(hub.join(1).is_some());
        let (t1, t2) = (hub.begin(1).unwrap(), hub.begin(1).unwrap());
        hub.finish(1, t2, "new".into());
        hub.finish(1, t1, "old".into());
        assert_eq!(&*c.try_recv().unwrap(), "new");
        assert!(c.try_recv().is_err());
        assert!(hub.begin(9).is_none());
    }
}
