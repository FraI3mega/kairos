use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use chrono::{Days, NaiveDate};
use kairos_core::event::Event;
use reqwest::Method;
use serde_json::{json, Value};

use crate::client::{fake_ip, Browser, Device, Http, Reader, UploadStats};
use crate::expected::{day_start, local_day, Expected};
use crate::persona::{Cadence, Persona};
use crate::rng::Rng;
use crate::scrape::{parse_me, MePage};

const BATCH: usize = 5000;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Status {
    Pass,
    Fail,
    Skip,
    Warn,
}

pub struct Check {
    pub name: &'static str,
    pub status: Status,
    pub detail: String,
}

impl Check {
    fn from_diffs(name: &'static str, compared: usize, diffs: Vec<String>) -> Check {
        if diffs.is_empty() {
            Check { name, status: Status::Pass, detail: format!("{compared} values") }
        } else {
            let more = if diffs.len() > 8 { format!(" (+{} more)", diffs.len() - 8) } else { String::new() };
            Check { name, status: Status::Fail, detail: format!("{} of {compared} differ: {}{more}", diffs.len(), diffs.iter().take(8).cloned().collect::<Vec<_>>().join("; ")) }
        }
    }

    fn skip(name: &'static str, why: &str) -> Check {
        Check { name, status: Status::Skip, detail: why.to_string() }
    }
}

pub struct Outcome {
    pub persona: String,
    pub devices: usize,
    pub events: usize,
    pub expected_secs: f64,
    pub stats: UploadStats,
    pub secs: f64,
    pub checks: Vec<Check>,
    pub fatal: Option<String>,
}

impl Outcome {
    pub fn ok(&self) -> bool {
        self.fatal.is_none() && self.checks.iter().all(|c| c.status != Status::Fail)
    }
}

pub struct Opts {
    pub work_dir: PathBuf,
    pub run: String,
    pub now: f64,
}

fn hours(secs: f64) -> String {
    format!("{:.2}h", secs / 3600.0)
}

fn near(map: &BTreeMap<String, f64>, other: &BTreeMap<String, f64>, tol: f64, what: &str, diffs: &mut Vec<String>) -> usize {
    let keys: std::collections::BTreeSet<&String> = map.keys().chain(other.keys()).collect();
    for k in &keys {
        let (a, b) = (map.get(*k).copied().unwrap_or(0.0), other.get(*k).copied().unwrap_or(0.0));
        if (a - b).abs() > tol {
            diffs.push(format!("{what} {k}: expected {a:.1}s, server {b:.1}s"));
        }
    }
    keys.len()
}

pub async fn check_spans(r: &Reader, username: &str, exp: &Expected, device_ids: &[i64], events: &[&[Event]]) -> Result<Check> {
    let v = r.json("GET /api/v1/users/:u/spans", &format!("/api/v1/users/{username}/spans")).await?;
    let mut got: BTreeMap<String, f64> = BTreeMap::new();
    let mut want: BTreeMap<String, f64> = BTreeMap::new();
    for s in v["spans"].as_array().ok_or_else(|| anyhow!("spans is not a list"))? {
        let dev = s["device_id"].as_i64().and_then(|id| device_ids.iter().position(|d| *d == id)).ok_or_else(|| anyhow!("span from an unknown device: {s}"))?;
        let project = if exp.stable { s["project"].as_str().unwrap_or("(other)") } else { "*" };
        let key = format!("dev{dev}/{project}/{}", s["class"].as_str().unwrap_or("?"));
        *got.entry(key).or_default() += s["end"].as_f64().unwrap_or(0.0) - s["start"].as_f64().unwrap_or(0.0);
    }
    for dev in 0..exp.per_device.len() {
        for ((p, c), secs) in exp.span_totals(dev, 0.0, f64::MAX) {
            let p = if !exp.stable { "*".to_string() } else if p.is_empty() { "(other)".to_string() } else { p };
            *want.entry(format!("dev{dev}/{p}/{c}")).or_default() += secs;
        }
    }
    let mut diffs = Vec::new();
    let n = near(&want, &got, 0.05, "spans", &mut diffs);

    let mut kinds_want: BTreeMap<String, f64> = BTreeMap::new();
    for e in events.iter().flat_map(|e| e.iter()) {
        *kinds_want.entry(e.kind.name().to_string()).or_default() += 1.0;
    }
    let kinds_got: BTreeMap<String, f64> =
        v["event_counts"].as_object().map(|m| m.iter().map(|(k, n)| (k.clone(), n.as_f64().unwrap_or(0.0))).collect()).unwrap_or_default();
    let m = near(&kinds_want, &kinds_got, 0.0, "events of kind", &mut diffs);
    Ok(Check::from_diffs("spans + event counts", n + m, diffs))
}

pub async fn check_projects(r: &Reader, username: &str, exp: &Expected) -> Result<Check> {
    let v = r.json("GET /api/v1/users/:u/stats", &format!("/api/v1/users/{username}/stats?features=projects&limit=0")).await?;
    let got = items(&v["data"]["projects"]);
    let want = exp.stats(0.0, f64::MAX, None).projects;
    let mut diffs = Vec::new();
    if exp.stable {
        let n = near(&want, &got, 1.0, "project", &mut diffs);
        return Ok(Check::from_diffs("projects (all time)", n, diffs));
    }
    // Which project gets shared time is not determined by the events: compare the sum only.
    let (a, b): (f64, f64) = (want.values().sum(), got.values().sum());
    if (a - b).abs() > 1.0 + want.len().max(got.len()) as f64 {
        diffs.push(format!("sum over projects: expected {a:.1}s, server {b:.1}s"));
    }
    Ok(Check::from_diffs("projects (all time)", 1, diffs))
}

fn items(v: &Value) -> BTreeMap<String, f64> {
    v.as_array()
        .map(|a| a.iter().map(|i| (i["name"].as_str().unwrap_or("?").to_string(), i["total_seconds"].as_f64().unwrap_or(0.0))).collect())
        .unwrap_or_default()
}

/// Bare dates are UTC midnights on the server and the window is half open, so a local day is asked for by its two instants.
fn iso(t: f64) -> String {
    chrono::DateTime::from_timestamp(t as i64, 0).map_or_else(String::new, |d| d.format("%Y-%m-%dT%H:%M:%SZ").to_string())
}

fn focus_seconds(d: &Value) -> BTreeMap<String, f64> {
    d["kairos"]["focus_seconds"].as_object().map(|m| m.iter().map(|(k, s)| (k.clone(), s.as_f64().unwrap_or(0.0))).collect()).unwrap_or_default()
}

pub async fn check_days(r: &Reader, username: &str, exp: &Expected) -> Result<Check> {
    let mut diffs = Vec::new();
    let mut compared = 0;
    for day in exp.day_range() {
        let next = day.checked_add_days(Days::new(1)).unwrap();
        let (from, to) = (day_start(exp.tz, day), day_start(exp.tz, next));
        let want = exp.stats(from, to, None);
        let path = format!("/api/v1/users/{username}/stats?features=projects&limit=0&start_date={}&end_date={}", iso(from), iso(to));
        let v = r.json("GET /api/v1/users/:u/stats", &path).await?;
        let d = &v["data"];
        let total = d["total_seconds"].as_f64().unwrap_or(-1.0);
        compared += 1;
        if (total - want.total).abs() > 1.0 {
            diffs.push(format!("{day} total: expected {:.1}s, server {total:.1}s", want.total));
        }
        if exp.stable {
            compared += near(&want.projects, &items(&d["projects"]), 1.0, &format!("{day} project"), &mut diffs);
        }
        compared += near(&want.classes, &focus_seconds(d), 1.0, &format!("{day} class"), &mut diffs);
    }
    Ok(Check::from_diffs("per local day (stats)", compared, diffs))
}

fn class_label(c: &str) -> &'static str {
    match c {
        "typing" => "Typing prompts",
        "reading" => "Reading replies",
        "editing" => "Editing",
        "testing" => "Testing",
        "terminal" => "Terminal",
        "bridge" => "Bridging time",
        "trip" => "Trips",
        _ => "Other",
    }
}

fn mins(secs: f64) -> i64 {
    (secs / 60.0).round() as i64
}

/// Page values are rounded to minutes, so one minute either way is a match.
#[derive(Default)]
struct Mins {
    diffs: Vec<String>,
    compared: usize,
}

impl Mins {
    fn cmp(&mut self, what: String, want: f64, got: Option<i64>) {
        self.compared += 1;
        match got {
            Some(g) if (g - mins(want)).abs() <= 1 => {}
            other => self.diffs.push(format!("{what}: expected {} min, page {other:?}", mins(want))),
        }
    }
}

async fn check_me(b: &mut Browser, username: &str, exp: &Expected, now: f64) -> Result<Check> {
    let me = b.get("GET /me", "/me").await?;
    if me.status != 200 {
        bail!("/me answered {}", me.status);
    }
    let page: MePage = parse_me(&me.body);
    if page.week.is_none() && page.days.is_empty() {
        return Ok(Check::skip("/me and profile numbers", "/me markup not recognised"));
    }
    let tz = exp.tz;
    let today = local_day(tz, now);
    let ago = |n: u64| today.checked_sub_days(Days::new(n)).unwrap();
    let days = exp.days();
    let sum = |from: NaiveDate| -> f64 { days.iter().filter(|((d, _, _), _)| *d >= from && *d <= today).map(|(_, s)| s).sum() };
    let mut m = Mins::default();
    m.cmp("this week".into(), sum(ago(6)), page.week);
    m.cmp("all time".into(), exp.all_total(), page.all);
    m.cmp("today".into(), sum(today), page.today);
    let by_label: BTreeMap<&str, i64> = page.days.iter().map(|(l, m)| (l.as_str(), *m)).collect();
    for n in 0..30 {
        let d = ago(n);
        let want: f64 = days.iter().filter(|((x, _, _), _)| *x == d).map(|(_, s)| s).sum();
        m.cmp(format!("day {d}"), want, by_label.get(d.format("%a %b %-d").to_string().as_str()).copied());
    }
    let first = ago(29);
    let mut classes: BTreeMap<&str, f64> = BTreeMap::new();
    let mut projects: BTreeMap<String, f64> = BTreeMap::new();
    for ((d, p, c), s) in &days {
        if *d >= first && *d <= today {
            *classes.entry(class_label(c)).or_default() += s;
            *projects.entry(if p.is_empty() { "Other".to_string() } else { p.clone() }).or_default() += s;
        }
    }
    for (label, secs) in &classes {
        m.cmp(format!("kind of work {label}"), *secs, page.classes.iter().find(|(l, _)| l == label).map(|(_, m)| *m));
    }
    if page.classes.len() != classes.len() {
        m.diffs.push(format!("kind of work rows: expected {}, page {}", classes.len(), page.classes.len()));
    }
    for (label, secs) in projects.iter().filter(|(_, s)| **s > 0.0 && exp.stable).take(12) {
        let got = page.projects.iter().find(|(l, _)| l == label).map(|(_, m)| *m);
        if got.is_some() || projects.len() <= 12 {
            m.cmp(format!("project {label}"), *secs, got);
        }
    }
    let profile = b.get("GET /@user", &format!("/@{username}")).await?;
    if profile.status != 200 {
        m.diffs.push(format!("public profile answered {}", profile.status));
    }
    Ok(Check::from_diffs("/me numbers and profile", m.compared, m.diffs))
}

async fn upload_all(devices: &mut [(Device, &[Event], Cadence)]) -> Result<()> {
    let mut ticks: Vec<(f64, usize)> = Vec::new();
    for (i, (_, ev, cadence)) in devices.iter().enumerate() {
        if let (Cadence::Every(step), Some(first), Some(last)) = (cadence, ev.first(), ev.last()) {
            let mut t = first.ts + step;
            while t < last.ts + step {
                ticks.push((t, i));
                t += step;
            }
        }
    }
    ticks.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut cursor = vec![0usize; devices.len()];
    let wait = Duration::from_millis(50);
    for (t, i) in ticks {
        let (dev, ev, _) = &mut devices[i];
        let n = ev[cursor[i]..].partition_point(|e| e.ts <= t);
        if n == 0 {
            continue;
        }
        dev.append(&ev[cursor[i]..cursor[i] + n])?;
        cursor[i] += n;
        dev.sync_all(BATCH, wait).await?;
    }
    for (i, (dev, ev, _)) in devices.iter_mut().enumerate() {
        if cursor[i] < ev.len() {
            dev.append(&ev[cursor[i]..])?;
            dev.sync_all(BATCH, wait).await?;
        }
    }
    Ok(())
}

pub async fn run_persona(http: Arc<Http>, p: Persona, idx: usize, o: &Opts) -> Outcome {
    let t0 = Instant::now();
    let mut out = Outcome { persona: p.name.to_string(), devices: p.devices.len(), events: p.event_count(), expected_secs: 0.0, stats: UploadStats::default(), secs: 0.0, checks: Vec::new(), fatal: None };
    if let Err(e) = drive(http, &p, idx, o, &mut out).await {
        out.fatal = Some(format!("{e:#}"));
    }
    out.secs = t0.elapsed().as_secs_f64();
    out
}

async fn drive(http: Arc<Http>, p: &Persona, idx: usize, o: &Opts, out: &mut Outcome) -> Result<()> {
    let ip = fake_ip(idx);
    let mut b = Browser::new(http.clone(), ip.clone());
    let email = format!("sim-{}-{}@example.com", o.run, p.name);
    b.sign_in(&format!("Sim {}", p.name), &email).await.context("sign in")?;

    let mut devices: Vec<(Device, &[Event], Cadence)> = Vec::new();
    let mut username = String::new();
    for (i, d) in p.devices.iter().enumerate() {
        let (token, name) = b.pair(&d.name, d.os).await.context("pair device")?;
        username = name;
        let dir = o.work_dir.join(format!("{}-{}", p.name, i));
        let dev = Device::new(http.clone(), token, ip.clone(), dir, d.lose_response, Rng::new(1000 + idx as u64 * 10 + i as u64))?;
        devices.push((dev, d.events.as_slice(), d.cadence));
    }
    if !p.tz_after_upload {
        b.set_timezone(&username, p.tz.name()).await.context("set timezone")?;
    }
    upload_all(&mut devices).await.context("upload")?;
    if p.tz_after_upload {
        b.set_timezone(&username, p.tz.name()).await.context("set timezone after upload")?;
    }
    for (d, _, _) in &devices {
        let s = &d.stats;
        out.stats.posts += s.posts;
        out.stats.events += s.events;
        out.stats.lost_responses += s.lost_responses;
        out.stats.resumes += s.resumes;
        out.stats.retries += s.retries;
        out.stats.biggest_batch = out.stats.biggest_batch.max(s.biggest_batch);
    }

    let logs: Vec<&[Event]> = devices.iter().map(|(_, e, _)| *e).collect();
    let exp = Expected::new(p.tz, &logs);
    out.expected_secs = exp.all_total();
    if !exp.stable {
        out.checks.push(Check {
            name: "project split is deterministic",
            status: Status::Warn,
            detail: "the live rule splits shared time between projects differently from run to run; project-level comparisons skipped".into(),
        });
    }
    let mut ids = Vec::new();
    let mut diffs = Vec::new();
    for (d, ev, _) in &devices {
        match d.me().await {
            Ok((id, head, n)) => {
                ids.push(id);
                if n != ev.len() as i64 {
                    diffs.push(format!("{}: server has {n} events, device wrote {}", d.dir.display(), ev.len()));
                }
                if head.as_deref() != ev.last().map(|e| e.hash.as_str()) {
                    diffs.push(format!("{}: server head is not the local head", d.dir.display()));
                }
            }
            Err(e) => diffs.push(format!("device/me: {e:#}")),
        }
    }
    out.checks.push(Check::from_diffs("device chain state", devices.len() * 2, diffs));

    let key = b.editor_key().await.context("editor key")?;
    let reader = Reader { http: http.clone(), ip: ip.clone(), key: Some(key) };
    let mut run = |r: Result<Check>, name: &'static str| match r {
        Ok(c) => out.checks.push(c),
        Err(e) => out.checks.push(Check { name, status: Status::Fail, detail: format!("{e:#}") }),
    };
    run(check_spans(&reader, &username, &exp, &ids, &logs).await, "spans + event counts");
    run(check_projects(&reader, &username, &exp).await, "projects (all time)");
    run(check_days(&reader, &username, &exp).await, "per local day (stats)");
    run(check_me(&mut b, &username, &exp, o.now).await, "/me numbers and profile");
    Ok(())
}

pub async fn protocol(http: Arc<Http>, idx: usize, o: &Opts) -> Outcome {
    let t0 = Instant::now();
    let mut out = Outcome { persona: "protocol".into(), devices: 1, events: 0, expected_secs: 0.0, stats: UploadStats::default(), secs: 0.0, checks: Vec::new(), fatal: None };
    if let Err(e) = protocol_checks(http, idx, o, &mut out).await {
        out.fatal = Some(format!("{e:#}"));
    }
    out.secs = t0.elapsed().as_secs_f64();
    out
}

fn expect(checks: &mut Vec<(&'static str, bool, String)>, name: &'static str, got: u16, want: u16, extra: String) {
    checks.push((name, got == want, format!("status {got}, wanted {want} {extra}")));
}

async fn protocol_checks(http: Arc<Http>, idx: usize, o: &Opts, out: &mut Outcome) -> Result<()> {
    use kairos_core::event::{Class, Kind, GENESIS};
    let ip = fake_ip(idx);
    let mut b = Browser::new(http.clone(), ip.clone());
    b.sign_in("Sim protocol", &format!("sim-{}-protocol@example.com", o.run)).await?;
    let (token, _) = b.pair("protocol-laptop", "linux").await?;

    let post = |tok: Option<&str>, body: Value| {
        let http = http.clone();
        let ip = ip.clone();
        let tok = tok.map(str::to_string);
        async move {
            let mut rb = http.req(Method::POST, "/api/v1/events").header("x-forwarded-for", ip).header("user-agent", "kairosd/0.1.0").json(&body);
            if let Some(t) = tok {
                rb = rb.header("authorization", format!("Bearer {t}"));
            }
            http.patient("POST /api/v1/events", || rb.try_clone().unwrap()).await
        }
    };
    let chain = |n: usize, t0: f64, anchor: &str| -> Vec<Value> {
        let mut prev = anchor.to_string();
        (0..n)
            .map(|i| {
                let mut e = Event::new(t0 + i as f64, Some("sim/protocol".into()), Kind::Focus { class: Class::Editor });
                e.seal(&prev);
                prev = e.hash.clone();
                serde_json::to_value(&e).unwrap()
            })
            .collect()
    };
    let now = o.now;
    let mut checks: Vec<(&'static str, bool, String)> = Vec::new();

    let good = chain(5, now - 3600.0, GENESIS);
    let r = post(None, json!({ "events": good })).await?;
    expect(&mut checks, "no token is 401", r.status, 401, String::new());
    let r = post(Some("kd_nope"), json!({ "events": good })).await?;
    expect(&mut checks, "unknown token is 401", r.status, 401, String::new());

    let mut tampered = good.clone();
    tampered[2]["ts"] = json!(now - 3000.0);
    let r = post(Some(&token), json!({ "events": tampered })).await?;
    expect(&mut checks, "tampered event is 400 bad_hash", r.status, 400, r.body.clone());
    let v = r.json()?;
    checks.push(("bad_hash names the event", v["error"] == "bad_hash" && v["index"] == 2, v.to_string()));

    let future = chain(2, now + 3600.0, GENESIS);
    let r = post(Some(&token), json!({ "events": future })).await?;
    expect(&mut checks, "future timestamp is 400 bad_ts", r.status, 400, r.body.clone());

    let big: Vec<Value> = (0..=BATCH).map(|_| Value::Null).collect();
    let r = post(Some(&token), json!({ "events": big })).await?;
    expect(&mut checks, "more than 5000 events is 413", r.status, 413, String::new());

    let r = post(Some(&token), json!({ "events": good })).await?;
    expect(&mut checks, "a good batch is accepted", r.status, 200, r.body.clone());
    let last = good.last().unwrap()["hash"].as_str().unwrap().to_string();
    checks.push(("accepted reports the new head", r.json()?["last_hash"] == last, r.body.clone()));

    let r = post(Some(&token), json!({ "events": good })).await?;
    expect(&mut checks, "replaying a batch is 409", r.status, 409, r.body.clone());
    checks.push(("409 carries the server head", r.json()?["last_hash"] == last, r.body.clone()));

    let wrong = chain(2, now - 1000.0, "ff00");
    let r = post(Some(&token), json!({ "events": wrong })).await?;
    expect(&mut checks, "a chain that does not extend the head is 409", r.status, 409, r.body.clone());

    let next = chain(3, now - 3500.0, &last);
    let r = post(Some(&token), json!({ "events": next })).await?;
    expect(&mut checks, "the next batch extends the chain", r.status, 200, r.body.clone());
    let r = post(Some(&token), json!({ "events": [] })).await?;
    expect(&mut checks, "an empty batch is 200", r.status, 200, r.body.clone());

    let dev = Device::new(http.clone(), token.clone(), ip, o.work_dir.join("protocol"), 0.0, Rng::new(1))?;
    let (_, head, n) = dev.me().await?;
    checks.push(("rejected batches stored nothing", n == 8 && head.as_deref() == next.last().unwrap()["hash"].as_str(), format!("{n} events, head {head:?}")));

    out.events = 8;
    for (name, ok, detail) in checks {
        out.checks.push(Check { name, status: if ok { Status::Pass } else { Status::Fail }, detail: if ok { String::new() } else { detail } });
    }
    Ok(())
}

pub fn report(outcomes: &[Outcome]) -> bool {
    println!();
    println!("{:<18} {:>4} {:>7} {:>6} {:>7} {:>6} {:>9} {:>6}   result", "persona", "dev", "events", "posts", "resume", "lost", "credit", "secs");
    for o in outcomes {
        let fails = o.checks.iter().filter(|c| c.status == Status::Fail).count();
        let verdict = if o.fatal.is_some() {
            "ERROR".to_string()
        } else if fails > 0 {
            format!("FAIL ({fails})")
        } else {
            "PASS".to_string()
        };
        println!(
            "{:<18} {:>4} {:>7} {:>6} {:>7} {:>6} {:>9} {:>6.1}   {}",
            o.persona, o.devices, o.events, o.stats.posts, o.stats.resumes, o.stats.lost_responses, hours(o.expected_secs), o.secs, verdict
        );
    }
    println!();
    println!("{:<18} {:<32} {:<5} detail", "persona", "check", "");
    for o in outcomes {
        if let Some(f) = &o.fatal {
            println!("{:<18} {:<32} ERROR {f}", o.persona, "(run)");
        }
        for c in &o.checks {
            let s = match c.status {
                Status::Pass => "PASS",
                Status::Fail => "FAIL",
                Status::Skip => "skip",
                Status::Warn => "WARN",
            };
            println!("{:<18} {:<32} {:<5} {}", o.persona, c.name, s, c.detail);
        }
    }
    let ok = outcomes.iter().all(Outcome::ok);
    println!();
    println!("{}", if ok { "all checks passed" } else { "CHECKS FAILED" });
    ok
}
