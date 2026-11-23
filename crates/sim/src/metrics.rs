use std::collections::BTreeMap;
use std::sync::Mutex;

#[derive(Default, Clone)]
pub struct Series {
    pub lat_ms: Vec<f32>,
    pub status: BTreeMap<u16, u64>,
    pub transport_errors: u64,
}

impl Series {
    pub fn count(&self) -> u64 {
        self.lat_ms.len() as u64 + self.transport_errors
    }

    pub fn percentile(&self, p: f64) -> f64 {
        if self.lat_ms.is_empty() {
            return 0.0;
        }
        let mut v = self.lat_ms.clone();
        v.sort_by(|a, b| a.total_cmp(b));
        let i = ((v.len() as f64 * p).ceil() as usize).clamp(1, v.len()) - 1;
        v[i] as f64
    }

    pub fn max(&self) -> f64 {
        self.lat_ms.iter().copied().fold(0.0, f32::max) as f64
    }

    /// Not a 2xx/3xx, a 409 (the upload resume signal) or a 429 (rate limiter, counted separately).
    pub fn errors(&self) -> u64 {
        self.transport_errors + self.status.iter().filter(|(s, _)| **s >= 400 && **s != 409 && **s != 429).map(|(_, n)| n).sum::<u64>()
    }

    pub fn of(&self, s: u16) -> u64 {
        self.status.get(&s).copied().unwrap_or(0)
    }
}

#[derive(Default)]
pub struct Metrics {
    m: Mutex<BTreeMap<String, Series>>,
}

impl Metrics {
    pub fn record(&self, label: &str, status: Option<u16>, ms: f64) {
        let mut m = self.m.lock().unwrap_or_else(|p| p.into_inner());
        let s = m.entry(label.to_string()).or_default();
        match status {
            Some(st) => {
                s.lat_ms.push(ms as f32);
                *s.status.entry(st).or_default() += 1;
            }
            None => s.transport_errors += 1,
        }
    }

    pub fn take(&self) -> BTreeMap<String, Series> {
        std::mem::take(&mut *self.m.lock().unwrap_or_else(|p| p.into_inner()))
    }
}

pub fn table(title: &str, series: &BTreeMap<String, Series>, secs: f64) -> String {
    let mut out = format!("{title}\n");
    out.push_str(&format!(
        "{:<34} {:>7} {:>7} {:>8} {:>8} {:>8} {:>8} {:>6} {:>5} {:>5}\n",
        "endpoint", "n", "req/s", "p50 ms", "p95 ms", "p99 ms", "max ms", "err", "409", "429"
    ));
    for (label, s) in series {
        out.push_str(&format!(
            "{:<34} {:>7} {:>7.2} {:>8.1} {:>8.1} {:>8.1} {:>8.1} {:>6} {:>5} {:>5}\n",
            label,
            s.count(),
            s.count() as f64 / secs.max(0.001),
            s.percentile(0.50),
            s.percentile(0.95),
            s.percentile(0.99),
            s.max(),
            s.errors(),
            s.of(409),
            s.of(429),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_and_errors() {
        let m = Metrics::default();
        for i in 1..=100 {
            m.record("x", Some(200), i as f64);
        }
        m.record("y", Some(500), 5.0);
        m.record("y", Some(409), 5.0);
        m.record("y", Some(429), 5.0);
        m.record("y", None, 0.0);
        let all = m.take();
        assert_eq!(all["x"].percentile(0.5), 50.0);
        assert_eq!(all["x"].percentile(0.99), 99.0);
        assert_eq!(all["y"].errors(), 2);
        assert_eq!(all["y"].of(409), 1);
        assert!(m.take().is_empty());
    }
}
