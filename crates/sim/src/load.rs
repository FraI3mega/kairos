use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use futures_util::stream::{self, StreamExt};
use kairos_core::event::Event;

use crate::check::{self, Status};
use crate::client::{fake_ip, Browser, Device, Fail, Http, Reader};
use crate::expected::Expected;
use crate::metrics::{self, Metrics};
use crate::persona::{live_user, LiveKind};
use crate::procstat::{self, Monitor};
use crate::rng::Rng;

const BATCH: usize = 5000;
const TICK: f64 = 60.0;
const TIMEZONES: &[&str] = &[
    "UTC", "America/New_York", "America/Chicago", "America/Los_Angeles", "Europe/London", "Europe/Berlin", "Asia/Kolkata", "Asia/Tokyo",
    "Australia/Sydney", "America/Sao_Paulo",
];

pub struct Opts {
    pub server: String,
    pub users: usize,
    pub duration: Duration,
    pub concurrency: usize,
    pub setup_concurrency: usize,
    pub history: Duration,
    pub active: f64,
    pub reads_per_min: f64,
    pub verify: usize,
    pub pid: Option<u32>,
    pub seed: u64,
    pub work_dir: PathBuf,
    pub run: String,
}

fn unix() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64()
}

#[derive(Default)]
struct Errors(Mutex<BTreeMap<String, u64>>);

impl Errors {
    fn add(&self, what: &str) {
        let key: String = what.chars().take(160).collect();
        *self.0.lock().unwrap_or_else(|p| p.into_inner()).entry(key).or_default() += 1;
    }
}

struct Account {
    idx: usize,
    username: String,
    tz: &'static str,
    browser: Browser,
    tokens: Vec<String>,
    active: bool,
    rng: Rng,
}

async fn create(http: Arc<Http>, o: &Opts, idx: usize) -> Result<Account> {
    let mut rng = Rng::new(o.seed ^ (idx as u64).wrapping_mul(0x9E37_79B9));
    let active = rng.chance(o.active);
    let ip = fake_ip(idx);
    let mut b = Browser::new(http, ip);
    b.sign_in(&format!("Sim User {idx}"), &format!("sim-{}-{idx}@example.com", o.run)).await.context("sign in")?;
    let n_devices = if active && rng.chance(0.08) { 2 } else { 1 };
    let mut tokens = Vec::new();
    let mut username = String::new();
    for d in 0..n_devices {
        let (token, name) = b.pair(&format!("sim-pc-{d}"), "linux").await.context("pair")?;
        tokens.push(token);
        username = name;
    }
    let tz = *rng.pick(TIMEZONES);
    b.set_timezone(&username, tz).await.context("timezone")?;
    b.patient = false;
    Ok(Account { idx, username, tz, browser: b, tokens, active, rng })
}

struct Uploader {
    idx: usize,
    username: String,
    tz: &'static str,
    active: bool,
    devices: Vec<(Device, Vec<Event>, usize)>,
    rng: Rng,
}

fn tz_of(name: &str) -> chrono_tz::Tz {
    name.parse().unwrap()
}

fn logs(a: &mut Account, t0: f64, o: &Opts) -> Vec<crate::persona::DeviceLog> {
    let from = t0 - o.history.as_secs_f64();
    let mut out = Vec::new();
    for d in 0..a.tokens.len() {
        let kind = if d == 0 { LiveKind::pick(&mut a.rng) } else { LiveKind::Cursor };
        let to = if a.active { t0 + o.duration.as_secs_f64() } else { t0 - 1800.0 };
        let mut dev = live_user(&mut a.rng, kind, from + d as f64 * 600.0, to).remove(0);
        dev.lose_response = if a.rng.chance(0.05) { 0.2 } else { 0.0 };
        out.push(dev);
    }
    out
}

async fn seed(u: &mut Uploader, t0: f64, history: f64) -> Result<()> {
    let slice = history / 4.0;
    for k in 1..=4 {
        let cut = t0 - history + slice * k as f64;
        for (dev, ev, cursor) in &mut u.devices {
            let n = ev[*cursor..].partition_point(|e| e.ts <= cut);
            if n > 0 {
                dev.append(&ev[*cursor..*cursor + n])?;
                *cursor += n;
                dev.sync_all(BATCH, Duration::from_secs(1)).await?;
            }
        }
    }
    Ok(())
}

async fn uploader_loop(mut u: Uploader, t0: f64, live: Instant, duration: Duration, errors: Arc<Errors>) -> Uploader {
    let mut backoff = vec![Duration::from_secs(60); u.devices.len()];
    let mut next = vec![Duration::ZERO; u.devices.len()];
    tokio::time::sleep(Duration::from_secs_f64(u.rng.range(0.0, TICK))).await;
    loop {
        let elapsed = live.elapsed();
        let vt = t0 + elapsed.as_secs_f64();
        for (i, (dev, ev, cursor)) in u.devices.iter_mut().enumerate() {
            if elapsed < next[i] {
                continue;
            }
            let n = ev[*cursor..].partition_point(|e| e.ts <= vt);
            if n > 0 {
                if let Err(e) = dev.append(&ev[*cursor..*cursor + n]) {
                    errors.add(&format!("append: {e}"));
                    continue;
                }
                *cursor += n;
            }
            if dev.pending() == 0 {
                continue;
            }
            match dev.sync(BATCH).await {
                Ok(_) => backoff[i] = Duration::from_secs(60),
                Err(Fail::Retry(e, after)) => {
                    errors.add(&format!("upload retry: {e}"));
                    next[i] = elapsed + after.map_or(backoff[i], Duration::from_secs);
                    backoff[i] = (backoff[i] * 2).min(Duration::from_secs(900));
                }
                Err(Fail::Halt(e)) => {
                    errors.add(&format!("upload halted: {e}"));
                    next[i] = Duration::from_secs(u64::MAX / 4);
                }
            }
        }
        if elapsed >= duration {
            break;
        }
        let jitter = u.rng.range(-1.0, 1.0);
        let remaining = duration.saturating_sub(live.elapsed());
        tokio::time::sleep(Duration::from_secs_f64((TICK + jitter).max(1.0)).min(remaining.max(Duration::from_millis(1)))).await;
    }
    u
}

async fn reader_loop(mut a: Account, live: Instant, duration: Duration, mean_secs: f64, errors: Arc<Errors>) {
    let reader = Reader { http: a.browser.http.clone(), ip: a.browser.ip.clone(), key: None };
    let day = chrono::Utc::now().format("%Y-%m-%d").to_string();
    loop {
        let wait = -mean_secs * (1.0 - a.rng.unit()).ln();
        let remaining = duration.saturating_sub(live.elapsed());
        if remaining.is_zero() {
            return;
        }
        tokio::time::sleep(Duration::from_secs_f64(wait).min(remaining)).await;
        if live.elapsed() >= duration {
            return;
        }
        let u = a.username.clone();
        let x = a.rng.unit();
        let r = if x < 0.45 {
            a.browser.get("GET /me", "/me").await
        } else if x < 0.65 {
            // stats are by user-timezone days, so ask for a window wide enough to cover "today".
            reader.get("GET /api/v1/users/:u/stats", &format!("/api/v1/users/{u}/stats?start_date={day}")).await
        } else if x < 0.75 {
            reader.get("GET /api/v1/users/:u/projects", &format!("/api/v1/users/{u}/projects")).await
        } else if x < 0.92 {
            a.browser.get("GET /@user", &format!("/@{u}")).await
        } else {
            reader.get("GET /api/v1/users/:u/stats (all time)", &format!("/api/v1/users/{u}/stats")).await
        };
        match r {
            Ok(r) if r.status >= 500 => errors.add(&format!("read {}", r.status)),
            Ok(_) => {}
            Err(e) => errors.add(&format!("read: {e}")),
        }
    }
}

pub async fn run(o: Opts) -> Result<bool> {
    let metrics = Arc::new(Metrics::default());
    let http = Http::new(&o.server, o.concurrency, metrics.clone())?;
    let errors = Arc::new(Errors::default());
    let port: Option<u16> = reqwest::Url::parse(&o.server).ok().and_then(|u| u.port_or_known_default());
    let pid = o.pid.or_else(|| port.and_then(procstat::listener_pid));
    println!(
        "load: {} users ({}% active), {}s live, gate {}, history {}m, server pid {}",
        o.users,
        (o.active * 100.0) as u32,
        o.duration.as_secs(),
        o.concurrency,
        o.history.as_secs() / 60,
        pid.map_or("unknown (no RSS/CPU)".to_string(), |p| p.to_string()),
    );
    let monitor = pid.map(|p| Monitor::start(p, Duration::from_secs(5)));

    let t_setup = Instant::now();
    let mut accounts: Vec<Account> = Vec::new();
    let mut failures = 0;
    let mut results = stream::iter(0..o.users).map(|i| create(http.clone(), &o, i)).buffer_unordered(o.setup_concurrency);
    while let Some(r) = results.next().await {
        match r {
            Ok(a) => accounts.push(a),
            Err(e) => {
                failures += 1;
                errors.add(&format!("setup: {e:#}"));
            }
        }
    }
    drop(results);
    accounts.sort_by_key(|a| a.idx);
    let accounts_secs = t_setup.elapsed().as_secs_f64();
    println!("accounts: {} signed in and paired in {:.0}s ({} failed)", accounts.len(), accounts_secs, failures);
    let setup_series = metrics.take();
    print!("{}", metrics::table("setup (sign-in, pairing, timezone)", &setup_series, accounts_secs));

    let t0 = unix();
    let mut uploaders = Vec::new();
    let mut readers = Vec::new();
    for mut a in accounts {
        let device_logs = logs(&mut a, t0, &o);
        let mut devices = Vec::new();
        for (d, log) in device_logs.into_iter().enumerate() {
            let dev = Device::new(
                http.clone(),
                a.tokens[d].clone(),
                a.browser.ip.clone(),
                o.work_dir.join(format!("u{}-{d}", a.idx)),
                log.lose_response,
                Rng::new(a.idx as u64 * 7 + d as u64),
            )?;
            devices.push((dev, log.events, 0usize));
        }
        let rng = a.rng.clone();
        uploaders.push(Uploader { idx: a.idx, username: a.username.clone(), tz: a.tz, active: a.active, devices, rng });
        readers.push(a);
    }

    let t_seed = Instant::now();
    let history = o.history.as_secs_f64();
    let mut seeded = stream::iter(uploaders.into_iter().map(|mut u| async move {
        let r = seed(&mut u, t0, history).await;
        (u, r)
    }))
    .buffer_unordered(o.concurrency);
    let mut ready = Vec::new();
    while let Some((u, r)) = seeded.next().await {
        if let Err(e) = r {
            errors.add(&format!("seed: {e:#}"));
        }
        ready.push(u);
    }
    drop(seeded);
    let seed_series = metrics.take();
    let events_seeded: u64 = ready.iter().flat_map(|u| u.devices.iter()).map(|d| d.0.stats.events).sum();
    println!("history: {events_seeded} events uploaded in {:.0}s", t_seed.elapsed().as_secs_f64());
    print!("{}", metrics::table("history upload", &seed_series, t_seed.elapsed().as_secs_f64()));
    let monitor_seed = monitor.as_ref().map(|m| m.samples.lock().unwrap_or_else(|p| p.into_inner()).clone());

    let live = Instant::now();
    let mut handles = Vec::new();
    for u in ready {
        handles.push(tokio::spawn(uploader_loop(u, t0, live, o.duration, errors.clone())));
    }
    let mut reads = Vec::new();
    for a in readers {
        let mean = 60.0 / (o.reads_per_min * if a.active { 1.0 } else { 0.3 });
        reads.push(tokio::spawn(reader_loop(a, live, o.duration, mean, errors.clone())));
    }
    println!("live: running for {}s ...", o.duration.as_secs());
    let mut finished = Vec::new();
    for h in handles {
        finished.push(h.await?);
    }
    for r in reads {
        r.await?;
    }
    let live_secs = live.elapsed().as_secs_f64();
    let live_series = metrics.take();
    let uploads: u64 = live_series.get("POST /api/v1/events").map_or(0, |s| s.count());
    let events_live: u64 = finished.iter().flat_map(|u| u.devices.iter()).map(|d| d.0.stats.events).sum::<u64>() - events_seeded;
    let lost: u64 = finished.iter().flat_map(|u| u.devices.iter()).map(|d| d.0.stats.lost_responses).sum();
    let total_reqs: u64 = live_series.values().map(|s| s.count()).sum();
    println!();
    print!("{}", metrics::table(&format!("live ({live_secs:.0}s)"), &live_series, live_secs));
    println!(
        "live totals: {} requests ({:.1}/s), {} upload requests, {} events uploaded ({:.1}/s), {} lost upload responses retried",
        total_reqs,
        total_reqs as f64 / live_secs,
        uploads,
        events_live,
        events_live as f64 / live_secs,
        lost
    );
    let samples = monitor.as_ref().map(Monitor::stop);
    if let Some(s) = &samples {
        let seed_n = monitor_seed.as_ref().map_or(0, Vec::len);
        let (before, during) = s.split_at(seed_n.min(s.len()));
        print!("{}", procstat::summary("server during sign-in and history", before));
        print!("{}", procstat::summary("server during live phase", during));
    }

    let cutoff = t0 + o.duration.as_secs_f64();
    let mut flushed = stream::iter(finished.into_iter().map(|mut u| async move {
        for (dev, ev, cursor) in &mut u.devices {
            let n = ev[*cursor..].partition_point(|e| e.ts <= cutoff);
            if n > 0 {
                dev.append(&ev[*cursor..*cursor + n])?;
                *cursor += n;
            }
            dev.sync_all(BATCH, Duration::from_secs(1)).await?;
        }
        Ok::<_, anyhow::Error>(u)
    }))
    .buffer_unordered(o.concurrency);
    let mut flushed_users = Vec::new();
    while let Some(r) = flushed.next().await {
        match r {
            Ok(u) => flushed_users.push(u),
            Err(e) => errors.add(&format!("flush: {e:#}")),
        }
    }
    drop(flushed);
    let _ = metrics.take();

    let mut ok = true;
    if o.verify > 0 {
        let active: Vec<&Uploader> = flushed_users.iter().filter(|u| u.active).collect();
        let step = (active.len() / o.verify).max(1);
        let sample: Vec<&Uploader> = active.into_iter().step_by(step).take(o.verify).collect();
        let mut failed = 0;
        let mut errs = Vec::new();
        for u in &sample {
            let refs: Vec<&[Event]> = u.devices.iter().map(|d| &d.1[..d.2]).collect();
            let exp = Expected::new(tz_of(u.tz), &refs);
            let r: Result<Vec<check::Check>> = async {
                let mut b = Browser::new(http.clone(), fake_ip(u.idx));
                b.sign_in(&format!("Sim User {}", u.idx), &format!("sim-{}-{}@example.com", o.run, u.idx)).await?;
                let key = b.editor_key().await?;
                let reader = Reader { http: http.clone(), ip: fake_ip(u.idx), key: Some(key) };
                let mut dev_ids = Vec::new();
                for (d, _, _) in &u.devices {
                    dev_ids.push(d.me().await?.0);
                }
                Ok(vec![
                    check::check_spans(&reader, &u.username, &exp, &dev_ids, &refs).await?,
                    check::check_projects(&reader, &u.username, &exp).await?,
                    check::check_days(&reader, &u.username, &exp).await?,
                ])
            }
            .await;
            match r {
                Ok(cs) => {
                    for c in cs.iter().filter(|c| c.status == Status::Fail) {
                        failed += 1;
                        errs.push(format!("user {} ({}): {} {}", u.idx, u.username, c.name, c.detail));
                    }
                }
                Err(e) => {
                    failed += 1;
                    errs.push(format!("user {}: {e:#}", u.idx));
                }
            }
        }
        println!();
        println!("verification: {} sampled users checked against their own logs, {failed} failures", sample.len());
        for e in errs.iter().take(10) {
            println!("  {e}");
        }
        ok = failed == 0;
    }
    let errs = errors.0.lock().unwrap_or_else(|p| p.into_inner());
    let total_errs: u64 = errs.values().sum();
    println!();
    println!("client-side errors: {total_errs}");
    for (k, n) in errs.iter().take(15) {
        println!("  {n:>6}  {k}");
    }
    let http_errors: u64 = live_series.values().map(|s| s.errors()).sum();
    Ok(ok && total_errs == 0 && http_errors == 0)
}
