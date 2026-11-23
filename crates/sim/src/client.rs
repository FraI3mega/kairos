use std::collections::HashMap;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use kairos_core::event::Event;
use kairos_core::upload::{self, State};
use reqwest::header::{HeaderMap, AUTHORIZATION, COOKIE, ORIGIN, SET_COOKIE, USER_AGENT};
use reqwest::{Client, Method, RequestBuilder};
use serde_json::{json, Value};
use tokio::sync::Semaphore;

use crate::metrics::Metrics;
use crate::rng::Rng;

pub const DEVICE_UA: &str = "kairosd/0.1.0";
const BROWSER_UA: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:130.0) Gecko/20100101 Firefox/130.0";
const MAX_429_WAITS: u32 = 120;

pub struct Resp {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: String,
}

impl Resp {
    pub fn location(&self) -> String {
        self.headers.get("location").and_then(|v| v.to_str().ok()).unwrap_or("").to_string()
    }

    pub fn json(&self) -> Result<Value> {
        serde_json::from_str(&self.body).with_context(|| format!("not JSON ({}): {}", self.status, self.body.chars().take(200).collect::<String>()))
    }

    pub fn retry_after(&self) -> Option<u64> {
        self.headers.get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok())
    }

    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

pub struct Http {
    c: Client,
    pub base: String,
    pub metrics: Arc<Metrics>,
    gate: Semaphore,
}

impl Http {
    pub fn new(base: &str, concurrency: usize, metrics: Arc<Metrics>) -> Result<Arc<Http>> {
        let c = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(60))
            .pool_max_idle_per_host(concurrency.max(8))
            .build()?;
        Ok(Arc::new(Http { c, base: base.trim_end_matches('/').to_string(), metrics, gate: Semaphore::new(concurrency.max(1)) }))
    }

    pub fn url(&self, path: &str) -> String {
        if path.starts_with("http://") || path.starts_with("https://") {
            path.to_string()
        } else {
            format!("{}{}", self.base, path)
        }
    }

    pub fn req(&self, method: Method, path: &str) -> RequestBuilder {
        self.c.request(method, self.url(path))
    }

    pub async fn send(&self, label: &str, rb: RequestBuilder) -> Result<Resp> {
        let _permit = self.gate.acquire().await?;
        let t = Instant::now();
        let out = async {
            let res = rb.send().await?;
            let status = res.status().as_u16();
            let headers = res.headers().clone();
            let body = res.text().await?;
            Ok::<_, reqwest::Error>(Resp { status, headers, body })
        }
        .await;
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        match out {
            Ok(r) => {
                self.metrics.record(label, Some(r.status), ms);
                Ok(r)
            }
            Err(e) => {
                self.metrics.record(label, None, ms);
                let mut msg = format!("{label}: {e}");
                let mut src = std::error::Error::source(&e);
                while let Some(s) = src {
                    msg.push_str(&format!(": {s}"));
                    src = s.source();
                }
                Err(anyhow!(msg))
            }
        }
    }

    pub async fn patient(&self, label: &str, make: impl Fn() -> RequestBuilder) -> Result<Resp> {
        for _ in 0..MAX_429_WAITS {
            let r = self.send(label, make()).await?;
            if r.status != 429 {
                return Ok(r);
            }
            tokio::time::sleep(Duration::from_secs(r.retry_after().unwrap_or(1).clamp(1, 30))).await;
        }
        bail!("{label}: still rate limited after {MAX_429_WAITS} waits")
    }
}

/// The server only believes it when started with `KAIROS_TRUSTED_PROXY=loopback`.
pub fn fake_ip(n: usize) -> String {
    let n = n + 1;
    format!("10.{}.{}.{}", (n >> 16) & 255, (n >> 8) & 255, n & 255)
}

pub fn csrf_of(body: &str) -> Option<String> {
    let (at, key) = ["name=\"csrf\" value=\"", "name=csrf value=\""].iter().find_map(|k| body.find(k).map(|at| (at, k)))?;
    Some(body[at + key.len()..].split('"').next()?.to_string())
}

pub struct Browser {
    pub http: Arc<Http>,
    pub ip: String,
    jar: HashMap<String, String>,
    pub patient: bool,
}

impl Browser {
    pub fn new(http: Arc<Http>, ip: String) -> Browser {
        Browser { http, ip, jar: HashMap::new(), patient: true }
    }

    fn cookies(&self) -> String {
        self.jar.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; ")
    }

    pub async fn request(&mut self, label: &str, method: Method, path: &str, form: Option<&[(&str, &str)]>) -> Result<Resp> {
        let cookie = self.cookies();
        let make = || {
            let mut b = self.http.req(method.clone(), path).header(USER_AGENT, BROWSER_UA).header("x-forwarded-for", &self.ip);
            if !cookie.is_empty() {
                b = b.header(COOKIE, &cookie);
            }
            if let Some(f) = form {
                b = b.header(ORIGIN, &self.http.base).form(f);
            }
            b
        };
        let res = if self.patient { self.http.patient(label, make).await? } else { self.http.send(label, make()).await? };
        for c in res.headers.get_all(SET_COOKIE) {
            let Ok(c) = c.to_str() else { continue };
            let Some((k, v)) = c.split(';').next().unwrap_or("").split_once('=') else { continue };
            if v.is_empty() || c.contains("Max-Age=0") {
                self.jar.remove(k);
            } else {
                self.jar.insert(k.to_string(), v.to_string());
            }
        }
        Ok(res)
    }

    pub async fn get(&mut self, label: &str, path: &str) -> Result<Resp> {
        self.request(label, Method::GET, path, None).await
    }

    pub async fn post(&mut self, label: &str, path: &str, form: &[(&str, &str)]) -> Result<Resp> {
        self.request(label, Method::POST, path, Some(form)).await
    }

    pub async fn sign_in(&mut self, name: &str, email: &str) -> Result<()> {
        let login = self.get("GET /auth/login", "/auth/login?next=/me").await?;
        if login.status != 303 {
            bail!("sign-in did not redirect ({}): {}", login.status, login.body.chars().take(200).collect::<String>());
        }
        let auth_url = login.location();
        let page = self.get("GET /mock-auth/oauth/authorize", &auth_url).await?;
        if page.status != 200 {
            bail!("mock authorize page answered {}: {}", page.status, page.body.chars().take(200).collect::<String>());
        }
        let url = reqwest::Url::parse(&auth_url).or_else(|_| reqwest::Url::parse(&self.http.url(&auth_url)))?;
        let q: Vec<(String, String)> = url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
        let mut pairs: Vec<(&str, &str)> = q.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        pairs.extend([
            ("action", "approve"),
            ("name", name),
            ("email", email),
            ("slack_id", "U0SIM"),
            ("verification_status", "verified"),
            ("ysws_eligible", "on"),
        ]);
        let approved = self.post("POST /mock-auth/oauth/authorize", "/mock-auth/oauth/authorize", &pairs).await?;
        if approved.status != 303 {
            bail!("mock approve answered {}: {}", approved.status, approved.body.chars().take(200).collect::<String>());
        }
        let cb = self.get("GET /auth/callback", &approved.location()).await?;
        if cb.status != 303 || cb.location() != "/me" {
            bail!("callback answered {} to {:?}: {}", cb.status, cb.location(), cb.body.chars().take(200).collect::<String>());
        }
        Ok(())
    }

    async fn csrf(&mut self) -> Result<String> {
        let page = self.get("GET /settings", "/settings").await?;
        csrf_of(&page.body).ok_or_else(|| anyhow!("no csrf field on /settings (status {})", page.status))
    }

    pub async fn set_timezone(&mut self, username: &str, tz: &str) -> Result<()> {
        let csrf = self.csrf().await?;
        let r = self
            .post(
                "POST /settings/profile",
                "/settings/profile",
                &[("csrf", &csrf), ("username", username), ("display_name", ""), ("is_public", "on"), ("show_ai_tools", "on"), ("timezone", tz)],
            )
            .await?;
        if r.status != 303 {
            bail!("saving the timezone answered {}: {}", r.status, r.body.chars().take(300).collect::<String>());
        }
        Ok(())
    }

    pub async fn editor_key(&mut self) -> Result<String> {
        let csrf = self.csrf().await?;
        let r = self.post("POST /settings/key", "/settings/key", &[("csrf", &csrf)]).await?;
        let at = r.body.find("api_key = ").ok_or_else(|| anyhow!("no key on the page (status {})", r.status))? + "api_key = ".len();
        Ok(r.body[at..].split('<').next().unwrap_or("").trim().to_string())
    }

    pub async fn pair(&mut self, name: &str, os: &str) -> Result<(String, String)> {
        let ip = self.ip.clone();
        let http = self.http.clone();
        let start = http
            .patient("POST /api/v1/device/start", || {
                http.req(Method::POST, "/api/v1/device/start")
                    .header(USER_AGENT, DEVICE_UA)
                    .header("x-forwarded-for", &ip)
                    .json(&json!({ "name": name, "os": os, "utc_offset_min": 0 }))
            })
            .await?;
        if !start.ok() {
            bail!("device/start answered {}: {}", start.status, start.body);
        }
        let s = start.json()?;
        let code = s["user_code"].as_str().ok_or_else(|| anyhow!("no user_code"))?.to_string();
        let poll_token = s["poll_token"].as_str().ok_or_else(|| anyhow!("no poll_token"))?.to_string();
        let page = self.get("GET /device", &format!("/device?code={code}")).await?;
        let csrf = csrf_of(&page.body).ok_or_else(|| anyhow!("device page has no csrf (status {})", page.status))?;
        let ok = self.post("POST /device/approve", "/device/approve", &[("code", &code), ("csrf", &csrf)]).await?;
        if !ok.body.contains("Computer connected") {
            bail!("approving the device failed ({}): {}", ok.status, ok.body.chars().take(300).collect::<String>());
        }
        for _ in 0..10 {
            let p = http
                .patient("POST /api/v1/device/poll", || {
                    http.req(Method::POST, "/api/v1/device/poll")
                        .header(USER_AGENT, DEVICE_UA)
                        .header("x-forwarded-for", &ip)
                        .json(&json!({ "poll_token": poll_token }))
                })
                .await?;
            if p.status == 410 {
                bail!("the pairing code expired");
            }
            let v = p.json()?;
            if v["status"] == "approved" {
                let token = v["token"].as_str().ok_or_else(|| anyhow!("approved without a token"))?.to_string();
                let username = v["username"].as_str().unwrap_or("").to_string();
                return Ok((token, username));
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        bail!("the code was approved but polling never returned a token")
    }
}

#[derive(Debug)]
pub enum Fail {
    Retry(String, Option<u64>),
    Halt(String),
}

impl std::fmt::Display for Fail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fail::Retry(e, _) | Fail::Halt(e) => f.write_str(e),
        }
    }
}

enum Reply {
    Accepted,
    Mismatch(Option<String>),
}

#[derive(Default, Debug, Clone)]
pub struct UploadStats {
    pub posts: u64,
    pub events: u64,
    pub lost_responses: u64,
    pub resumes: u64,
    pub retries: u64,
    pub biggest_batch: usize,
}

pub struct Device {
    pub http: Arc<Http>,
    pub token: String,
    pub ip: String,
    pub dir: PathBuf,
    pub lose_response: f64,
    pub rng: Rng,
    pub stats: UploadStats,
}

impl Device {
    pub fn new(http: Arc<Http>, token: String, ip: String, dir: PathBuf, lose_response: f64, rng: Rng) -> Result<Device> {
        std::fs::create_dir_all(&dir)?;
        Ok(Device { http, token, ip, dir, lose_response, rng, stats: UploadStats::default() })
    }

    pub fn append(&self, events: &[Event]) -> Result<()> {
        let mut i = 0;
        while i < events.len() {
            let day = |e: &Event| chrono::DateTime::from_timestamp(e.ts as i64, 0).unwrap().format("%Y-%m-%d").to_string();
            let d = day(&events[i]);
            let mut j = i;
            let mut text = String::new();
            while j < events.len() && day(&events[j]) == d {
                text.push_str(&serde_json::to_string(&events[j])?);
                text.push('\n');
                j += 1;
            }
            let mut f = std::fs::OpenOptions::new().create(true).append(true).open(self.dir.join(format!("events-{d}.jsonl")))?;
            f.write_all(text.as_bytes())?;
            i = j;
        }
        Ok(())
    }

    pub fn pending(&self) -> usize {
        upload::pending(&self.dir, &State::load(&self.dir))
    }

    async fn post(&mut self, body: String) -> Result<Reply, Fail> {
        let rb = self
            .http
            .req(Method::POST, "/api/v1/events")
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(USER_AGENT, DEVICE_UA)
            .header("x-forwarded-for", &self.ip)
            .header("content-type", "application/json")
            .body(body);
        let res = self.http.send("POST /api/v1/events", rb).await.map_err(|e| Fail::Retry(e.to_string(), None))?;
        match res.status {
            200 => Ok(Reply::Accepted),
            409 => {
                let v = res.json().map_err(|e| Fail::Retry(format!("unreadable 409 body: {e}"), None))?;
                Ok(Reply::Mismatch(v["last_hash"].as_str().map(str::to_string)))
            }
            401 => Err(Fail::Halt("the server rejected the token".into())),
            429 => Err(Fail::Retry("server answered 429".into(), res.retry_after())),
            s if s >= 500 || s == 408 => Err(Fail::Retry(format!("server answered {s}"), None)),
            s => Err(Fail::Halt(format!("server answered {s}: {}", res.body.chars().take(300).collect::<String>()))),
        }
    }

    pub async fn sync(&mut self, max: usize) -> Result<usize, Fail> {
        let mut state = State::load(&self.dir);
        let mut sent = 0;
        let mut stuck = false;
        loop {
            let Some(batch) = upload::next_batch(&self.dir, &state, max) else {
                return Ok(sent);
            };
            let n = batch.lines.len();
            self.stats.posts += 1;
            self.stats.biggest_batch = self.stats.biggest_batch.max(n);
            match self.post(batch.body()).await {
                Ok(Reply::Accepted) => {
                    if self.lose_response > 0.0 && (self.stats.lost_responses == 0 || self.rng.chance(self.lose_response)) {
                        self.stats.lost_responses += 1;
                        return Err(Fail::Retry("response lost".into(), None));
                    }
                    sent += n;
                    self.stats.events += n as u64;
                    state = batch.end;
                    stuck = false;
                    state.save(&self.dir).map_err(|e| Fail::Retry(format!("saving upload.json: {e}"), None))?;
                }
                Ok(Reply::Mismatch(server_last)) => {
                    self.stats.resumes += 1;
                    if stuck {
                        return Err(Fail::Halt("the server keeps reporting chain_mismatch after resuming".into()));
                    }
                    stuck = true;
                    state = match server_last {
                        None => State::default(),
                        Some(h) => upload::locate(&self.dir, &h)
                            .ok_or_else(|| Fail::Halt(format!("the server's last event {h} is not in the local logs")))?,
                    };
                    state.save(&self.dir).map_err(|e| Fail::Retry(format!("saving upload.json: {e}"), None))?;
                }
                Err(e) => {
                    self.stats.retries += 1;
                    return Err(e);
                }
            }
        }
    }

    pub async fn sync_all(&mut self, max: usize, wait: Duration) -> Result<()> {
        for _ in 0..500 {
            match self.sync(max).await {
                Ok(_) => return Ok(()),
                Err(Fail::Retry(_, after)) => tokio::time::sleep(after.map_or(wait, |a| Duration::from_secs(a.clamp(1, 30)))).await,
                Err(Fail::Halt(e)) => bail!("upload stopped: {e}"),
            }
        }
        bail!("upload kept failing")
    }

    pub async fn me(&self) -> Result<(i64, Option<String>, i64)> {
        let rb = self.http.req(Method::GET, "/api/v1/device/me").header(AUTHORIZATION, format!("Bearer {}", self.token)).header(USER_AGENT, DEVICE_UA).header("x-forwarded-for", &self.ip);
        let r = self.http.send("GET /api/v1/device/me", rb).await?;
        let v = r.json()?;
        Ok((v["device_id"].as_i64().unwrap_or(0), v["last_hash"].as_str().map(str::to_string), v["events"].as_i64().unwrap_or(0)))
    }
}

pub struct Reader {
    pub http: Arc<Http>,
    pub ip: String,
    pub key: Option<String>,
}

impl Reader {
    pub async fn get(&self, label: &str, path: &str) -> Result<Resp> {
        let make = || {
            let mut b = self.http.req(Method::GET, path).header(USER_AGENT, "kairos-sim-reader/0.1").header("x-forwarded-for", &self.ip);
            if let Some(k) = &self.key {
                b = b.header(AUTHORIZATION, format!("Bearer {k}"));
            }
            b
        };
        self.http.patient(label, make).await
    }

    pub async fn json(&self, label: &str, path: &str) -> Result<Value> {
        let r = self.get(label, path).await?;
        if !r.ok() {
            bail!("{path} answered {}: {}", r.status, r.body.chars().take(200).collect::<String>());
        }
        r.json()
    }
}
