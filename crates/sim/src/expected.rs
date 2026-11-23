//! The cross-device carving and day splitting are written out again here on purpose, so the check
//! compares the server against an independent implementation.

use std::collections::BTreeMap;

use chrono::{Days, NaiveDate, TimeZone};
use chrono_tz::Tz;
use kairos_core::credit::Rule;
use kairos_core::event::Event;

const PRIORITY: &[&str] = &["typing", "editing", "terminal", "testing", "reading", "bridge", "trip"];

#[derive(Debug, Clone, PartialEq)]
pub struct Piece {
    pub start: f64,
    pub end: f64,
    pub project: Option<String>,
    pub class: String,
}

pub fn device_pieces(events: &[Event]) -> Vec<Piece> {
    Rule::CURRENT
        .apply(events)
        .into_iter()
        .map(|c| Piece { start: c.start, end: c.end, project: c.project, class: c.class.as_str().to_string() })
        .collect()
}

fn rank(class: &str) -> usize {
    PRIORITY.iter().position(|c| *c == class).unwrap_or(PRIORITY.len())
}

pub fn union_secs(mut iv: Vec<(f64, f64)>) -> f64 {
    iv.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut total = 0.0;
    let mut cur: Option<(f64, f64)> = None;
    for (s, e) in iv {
        match cur.as_mut() {
            Some(c) if s <= c.1 => c.1 = c.1.max(e),
            _ => {
                if let Some(c) = cur {
                    total += c.1 - c.0;
                }
                cur = Some((s, e));
            }
        }
    }
    total + cur.map_or(0.0, |c| c.1 - c.0)
}

/// Highest class priority wins, then lowest device number, then project name (no project first).
pub fn carve(devices: &[Vec<Piece>]) -> Vec<Piece> {
    let all: Vec<(usize, &Piece)> = devices.iter().enumerate().flat_map(|(d, ps)| ps.iter().map(move |p| (d, p))).collect();
    let mut bounds: Vec<f64> = all.iter().flat_map(|(_, p)| [p.start, p.end]).collect();
    bounds.sort_by(|a, b| a.total_cmp(b));
    bounds.dedup();
    let mut out: Vec<Piece> = Vec::new();
    for w in bounds.windows(2) {
        let (lo, hi) = (w[0], w[1]);
        let winner = all
            .iter()
            .filter(|(_, p)| p.start <= lo && p.end >= hi)
            .min_by(|a, b| rank(&a.1.class).cmp(&rank(&b.1.class)).then(a.0.cmp(&b.0)).then(a.1.project.cmp(&b.1.project)));
        if let Some((_, p)) = winner {
            out.push(Piece { start: lo, end: hi, project: p.project.clone(), class: p.class.clone() });
        }
    }
    out
}

pub fn day_start(tz: Tz, d: NaiveDate) -> f64 {
    for h in 0..4 {
        if let Some(t) = d.and_hms_opt(h, 0, 0).and_then(|n| tz.from_local_datetime(&n).earliest()) {
            return t.timestamp() as f64;
        }
    }
    d.and_hms_opt(0, 0, 0).map_or(0.0, |n| n.and_utc().timestamp() as f64)
}

pub fn local_day(tz: Tz, t: f64) -> NaiveDate {
    chrono::DateTime::from_timestamp(t.floor() as i64, 0).map_or(NaiveDate::MIN, |d| d.with_timezone(&tz).date_naive())
}

fn next_day(d: NaiveDate) -> NaiveDate {
    d.checked_add_days(Days::new(1)).unwrap_or(d)
}

fn by_day(tz: Tz, p: &Piece) -> Vec<(NaiveDate, f64)> {
    let mut out = Vec::new();
    let mut t = p.start;
    while t < p.end {
        let day = local_day(tz, t);
        let end = p.end.min(day_start(tz, next_day(day)));
        let end = if end <= t { p.end } else { end };
        out.push((day, end - t));
        t = end;
    }
    out
}

#[derive(Debug, Default, Clone)]
pub struct Stats {
    pub total: f64,
    pub projects: BTreeMap<String, f64>,
    pub classes: BTreeMap<String, f64>,
}

pub struct Expected {
    pub tz: Tz,
    /// Project-level comparisons are skipped when the live rule splits shared time differently from run to run.
    pub stable: bool,
    pub per_device: Vec<Vec<Piece>>,
    pub carved: Vec<Piece>,
}

impl Expected {
    pub fn new(tz: Tz, devices: &[&[Event]]) -> Expected {
        let per_device: Vec<Vec<Piece>> = devices.iter().map(|e| device_pieces(e)).collect();
        let stable = devices.iter().zip(&per_device).all(|(e, first)| (0..4).all(|_| device_pieces(e) == *first));
        let carved = carve(&per_device);
        Expected { tz, stable, per_device, carved }
    }

    pub fn days(&self) -> BTreeMap<(NaiveDate, String, String), f64> {
        let mut m = BTreeMap::new();
        for p in &self.carved {
            for (d, s) in by_day(self.tz, p) {
                *m.entry((d, p.project.clone().unwrap_or_default(), p.class.clone())).or_default() += s;
            }
        }
        m
    }

    pub fn all_total(&self) -> f64 {
        self.carved.iter().map(|p| p.end - p.start).sum()
    }

    /// Project seconds are each project's own union; class seconds are carved, so they add up to the total.
    pub fn stats(&self, from: f64, to: f64, project: Option<&str>) -> Stats {
        let mut all = Vec::new();
        let mut projects: BTreeMap<String, Vec<(f64, f64)>> = BTreeMap::new();
        let mut clipped: Vec<Vec<Piece>> = Vec::new();
        for device in &self.per_device {
            let mut mine = Vec::new();
            for p in device {
                let Some(pr) = &p.project else { continue };
                if project.is_some_and(|f| f != pr) {
                    continue;
                }
                let iv = (p.start.max(from), p.end.min(to));
                if iv.1 <= iv.0 {
                    continue;
                }
                all.push(iv);
                projects.entry(pr.clone()).or_default().push(iv);
                mine.push(Piece { start: iv.0, end: iv.1, project: p.project.clone(), class: p.class.clone() });
            }
            clipped.push(mine);
        }
        let mut classes: BTreeMap<String, f64> = BTreeMap::new();
        for p in carve(&clipped) {
            *classes.entry(p.class).or_default() += p.end - p.start;
        }
        Stats { total: union_secs(all), projects: projects.into_iter().map(|(k, v)| (k, union_secs(v))).collect(), classes }
    }

    pub fn span_totals(&self, device: usize, from: f64, to: f64) -> BTreeMap<(String, String), f64> {
        let mut m = BTreeMap::new();
        for p in &self.per_device[device] {
            let (s, e) = (p.start.max(from), p.end.min(to));
            if e > s {
                *m.entry((p.project.clone().unwrap_or_default(), p.class.clone())).or_default() += e - s;
            }
        }
        m
    }

    /// Every local day that has credit, plus one either side so a stray day shows up as a failure.
    pub fn day_range(&self) -> Vec<NaiveDate> {
        let days = self.days();
        let (Some(first), Some(last)) = (days.keys().map(|k| k.0).min(), days.keys().map(|k| k.0).max()) else {
            return Vec::new();
        };
        let mut out = vec![first.checked_sub_days(Days::new(1)).unwrap_or(first)];
        let mut d = first;
        while d <= last {
            out.push(d);
            d = next_day(d);
        }
        out.push(next_day(last));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: f64, e: f64, project: Option<&str>, class: &str) -> Piece {
        Piece { start: s, end: e, project: project.map(String::from), class: class.into() }
    }

    #[test]
    fn carve_gives_each_second_once() {
        let a = vec![p(0.0, 100.0, Some("a"), "reading")];
        let b = vec![p(50.0, 150.0, Some("b"), "editing")];
        let out = carve(&[a, b]);
        let total: f64 = out.iter().map(|x| x.end - x.start).sum();
        assert_eq!(total, 150.0);
        let editing: f64 = out.iter().filter(|x| x.class == "editing").map(|x| x.end - x.start).sum();
        assert_eq!(editing, 100.0);
        let reading: f64 = out.iter().filter(|x| x.class == "reading").map(|x| x.end - x.start).sum();
        assert_eq!(reading, 50.0);
    }

    #[test]
    fn same_class_goes_to_the_lower_device_whoever_started_first() {
        let a = vec![p(50.0, 150.0, Some("a"), "editing")];
        let b = vec![p(0.0, 100.0, Some("b"), "editing")];
        let out = carve(&[a, b]);
        let of = |proj: &str| out.iter().filter(|x| x.project.as_deref() == Some(proj)).map(|x| x.end - x.start).sum::<f64>();
        assert_eq!((of("a"), of("b")), (100.0, 50.0));
        let tie = carve(&[vec![p(0.0, 10.0, Some("y"), "editing")], vec![p(0.0, 10.0, Some("x"), "editing")]]);
        assert_eq!(tie.len(), 1);
        assert_eq!(tie[0].project.as_deref(), Some("y"));
    }

    #[test]
    fn days_split_at_local_midnight_and_dst() {
        let tz: Tz = "America/New_York".parse().unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 3, 8).unwrap();
        let mid = day_start(tz, d);
        assert_eq!(mid, 1772946000.0);
        let parts = by_day(tz, &p(mid - 600.0, mid + 300.0, None, "editing"));
        assert_eq!(parts.iter().map(|x| x.1).collect::<Vec<_>>(), vec![600.0, 300.0]);
        let next = day_start(tz, next_day(d));
        assert_eq!(next - mid, 23.0 * 3600.0);
    }

    #[test]
    fn union_counts_overlap_once() {
        assert_eq!(union_secs(vec![(0.0, 10.0), (5.0, 20.0), (30.0, 40.0)]), 30.0);
    }
}
