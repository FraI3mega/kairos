//! Streaks the way Hackatime counts them: consecutive local days with at least 15 minutes of
//! focus time, counted back from today if today already qualifies, otherwise from yesterday.

use std::collections::HashSet;

use anyhow::Result;
use chrono::{Days, NaiveDate};
use deadpool_postgres::GenericClient;

pub const DAY_SECONDS: f64 = 900.0;
pub const MAX_DAYS: u64 = 1000;

pub fn count(qualifying: &HashSet<NaiveDate>, today: NaiveDate) -> u32 {
    let mut day = if qualifying.contains(&today) { today } else { today.pred_opt().unwrap_or(today) };
    let mut n = 0;
    while qualifying.contains(&day) && u64::from(n) < MAX_DAYS {
        n += 1;
        match day.pred_opt() {
            Some(d) => day = d,
            None => break,
        }
    }
    n
}

pub async fn current(c: &impl GenericClient, user_id: i64, today: NaiveDate) -> Result<u32> {
    let from = today.checked_sub_days(Days::new(MAX_DAYS)).unwrap_or(NaiveDate::MIN);
    let rows = c
        .query(
            "SELECT day FROM user_days WHERE user_id = $1 AND day >= $2 AND class <> 'agent'
             GROUP BY day HAVING sum(seconds) >= $3",
            &[&user_id, &from, &DAY_SECONDS],
        )
        .await?;
    Ok(count(&rows.iter().map(|r| r.get(0)).collect(), today))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn days(list: &[&str]) -> HashSet<NaiveDate> {
        list.iter().map(|s| d(s)).collect()
    }

    #[test]
    fn counts_back_from_today_when_today_qualifies() {
        assert_eq!(count(&days(&["2026-11-18", "2026-11-19", "2026-11-20"]), d("2026-11-20")), 3);
    }

    #[test]
    fn a_quiet_today_keeps_yesterdays_streak_alive() {
        assert_eq!(count(&days(&["2026-11-18", "2026-11-19"]), d("2026-11-20")), 2);
    }

    #[test]
    fn a_gap_ends_the_streak() {
        assert_eq!(count(&days(&["2026-11-16", "2026-11-17", "2026-11-19", "2026-11-20"]), d("2026-11-20")), 2);
        assert_eq!(count(&days(&["2026-11-17", "2026-11-18"]), d("2026-11-20")), 0);
        assert_eq!(count(&HashSet::new(), d("2026-11-20")), 0);
    }
}
