use chrono::{DateTime, TimeZone};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pause {
    HalfHour,
    Hour,
    Tomorrow,
}

impl Pause {
    pub fn until<Tz: TimeZone>(self, now: &DateTime<Tz>) -> Option<f64> {
        match self {
            Pause::HalfHour => Some(now.timestamp() as f64 + 30.0 * 60.0),
            Pause::Hour => Some(now.timestamp() as f64 + 60.0 * 60.0),
            Pause::Tomorrow => next_midnight(now),
        }
    }
}

/// The start of the next local day. When a daylight saving change swallows midnight, the first
/// hour that exists that day is used; when it happens twice, the first time.
fn next_midnight<Tz: TimeZone>(now: &DateTime<Tz>) -> Option<f64> {
    let day = now.date_naive().succ_opt()?;
    (0..4)
        .filter_map(|hour| day.and_hms_opt(hour, 0, 0))
        .find_map(|t| now.timezone().from_local_datetime(&t).earliest())
        .map(|t| t.timestamp() as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use chrono_tz::{America, Tz};

    fn at(tz: Tz, y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Tz> {
        tz.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    fn hours_until(now: &DateTime<Tz>) -> f64 {
        (Pause::Tomorrow.until(now).unwrap() - now.timestamp() as f64) / 3600.0
    }

    #[test]
    fn fixed_durations_add_to_now() {
        let now = Utc.with_ymd_and_hms(2025, 6, 1, 12, 0, 0).unwrap();
        let t = now.timestamp() as f64;
        assert_eq!(Pause::HalfHour.until(&now), Some(t + 1800.0));
        assert_eq!(Pause::Hour.until(&now), Some(t + 3600.0));
    }

    #[test]
    fn tomorrow_is_the_next_local_midnight() {
        let now = Utc.with_ymd_and_hms(2025, 6, 1, 15, 30, 0).unwrap();
        let want = Utc.with_ymd_and_hms(2025, 6, 2, 0, 0, 0).unwrap().timestamp() as f64;
        assert_eq!(Pause::Tomorrow.until(&now), Some(want));
        let just_after = Utc.with_ymd_and_hms(2025, 6, 2, 0, 0, 1).unwrap();
        assert_eq!(Pause::Tomorrow.until(&just_after), Some(want + 86400.0));
    }

    #[test]
    fn tomorrow_follows_the_local_calendar_not_24_hours() {
        // Spring forward in New York: the day is 23 hours long.
        assert_eq!(hours_until(&at(America::New_York, 2024, 3, 10, 0, 30)), 22.5);
        // Fall back: 25 hours long.
        assert_eq!(hours_until(&at(America::New_York, 2024, 11, 3, 0, 30)), 24.5);
    }

    #[test]
    fn midnight_lost_to_a_clock_change_uses_the_first_hour_that_exists() {
        // Sao Paulo skipped 00:00 to 01:00 on 2018-11-04.
        let now = at(America::Sao_Paulo, 2018, 11, 3, 20, 0);
        let want = at(America::Sao_Paulo, 2018, 11, 4, 1, 0).timestamp() as f64;
        assert_eq!(Pause::Tomorrow.until(&now), Some(want));
        assert_eq!(hours_until(&now), 4.0);
    }

    #[test]
    fn midnight_repeated_by_a_clock_change_uses_the_first_one() {
        // Havana's 00:00 happened twice on 2018-11-04.
        let now = at(America::Havana, 2018, 11, 3, 20, 0);
        let first = America::Havana.with_ymd_and_hms(2018, 11, 4, 0, 0, 0).earliest().unwrap();
        assert_eq!(Pause::Tomorrow.until(&now), Some(first.timestamp() as f64));
        assert_eq!(hours_until(&now), 4.0);
    }
}
