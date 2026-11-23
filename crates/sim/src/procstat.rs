use std::fs;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy)]
pub struct Sample {
    pub at: f64,
    pub rss_mb: f64,
    pub cpu: f64,
    pub load1: f64,
}

fn ticks(pid: u32) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name may contain spaces and parentheses, so split after the last ')'.
    let rest = &stat[stat.rfind(')')? + 2..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    Some(f.get(11)?.parse::<u64>().ok()? + f.get(12)?.parse::<u64>().ok()?)
}

fn rss_mb(pid: u32) -> Option<f64> {
    let s = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let kb: f64 = s.lines().find_map(|l| l.strip_prefix("VmRSS:"))?.trim().trim_end_matches("kB").trim().parse().ok()?;
    Some(kb / 1024.0)
}

fn load1() -> f64 {
    fs::read_to_string("/proc/loadavg").ok().and_then(|s| s.split_whitespace().next()?.parse().ok()).unwrap_or(0.0)
}

pub fn listener_pid(port: u16) -> Option<u32> {
    let mut inodes = Vec::new();
    for file in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(text) = fs::read_to_string(file) else { continue };
        for line in text.lines().skip(1) {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 10 || f[3] != "0A" {
                continue;
            }
            if f[1].rsplit(':').next().and_then(|p| u16::from_str_radix(p, 16).ok()) == Some(port) {
                inodes.push(f[9].to_string());
            }
        }
    }
    for entry in fs::read_dir("/proc").ok()?.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else { continue };
        let Ok(fds) = fs::read_dir(format!("/proc/{pid}/fd")) else { continue };
        for fd in fds.flatten() {
            if let Ok(link) = fs::read_link(fd.path()) {
                let l = link.to_string_lossy();
                if inodes.iter().any(|i| l == format!("socket:[{i}]")) {
                    return Some(pid);
                }
            }
        }
    }
    None
}

pub struct Monitor {
    pub samples: Arc<Mutex<Vec<Sample>>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
}

impl Monitor {
    pub fn start(pid: u32, every: Duration) -> Monitor {
        let samples = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (s, st) = (samples.clone(), stop.clone());
        tokio::spawn(async move {
            let t0 = Instant::now();
            let mut last = (Instant::now(), ticks(pid));
            while !st.load(std::sync::atomic::Ordering::Relaxed) {
                tokio::time::sleep(every).await;
                let now = (Instant::now(), ticks(pid));
                let (Some(a), Some(b), Some(rss)) = (last.1, now.1, rss_mb(pid)) else { break };
                let cpu = (b - a) as f64 / 100.0 / now.0.duration_since(last.0).as_secs_f64() * 100.0;
                s.lock().unwrap_or_else(|p| p.into_inner()).push(Sample { at: t0.elapsed().as_secs_f64(), rss_mb: rss, cpu, load1: load1() });
                last = now;
            }
        });
        Monitor { samples, stop }
    }

    pub fn stop(&self) -> Vec<Sample> {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        self.samples.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

pub fn summary(label: &str, s: &[Sample]) -> String {
    if s.is_empty() {
        return format!("{label}: no samples (server pid not found or exited)\n");
    }
    let rss: Vec<f64> = s.iter().map(|x| x.rss_mb).collect();
    let cpu_avg = s.iter().map(|x| x.cpu).sum::<f64>() / s.len() as f64;
    let cpu_max = s.iter().map(|x| x.cpu).fold(0.0, f64::max);
    let mut out = format!(
        "{label}: RSS first {:.0} MB, peak {:.0} MB, last {:.0} MB (growth {:+.0} MB); CPU avg {:.0}%, peak {:.0}% of one core; box load1 peak {:.1}\n",
        rss[0],
        rss.iter().copied().fold(0.0, f64::max),
        rss[rss.len() - 1],
        rss[rss.len() - 1] - rss[0],
        cpu_avg,
        cpu_max,
        s.iter().map(|x| x.load1).fold(0.0, f64::max),
    );
    let step = (s.len() / 12).max(1);
    out.push_str("    t(s)   RSS MB   CPU%  load1\n");
    for x in s.iter().step_by(step) {
        out.push_str(&format!("  {:>6.0} {:>8.0} {:>6.0} {:>6.1}\n", x.at, x.rss_mb, x.cpu, x.load1));
    }
    out
}
