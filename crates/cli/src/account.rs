use std::fmt;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use kairos_core::account::{self, Account};
use kairos_core::loopback::{Loopback, TimedOut};
use kairos_core::pairing::{self, Connect, Pairing};
use kairos_core::paths;
use kairos_core::upload::{self, State};
use kairos_core::wakatime;
use anyhow::{bail, Context, Result};

use crate::hackatime::{self, Oauth};

pub(crate) const WAIT: Duration = Duration::from_secs(600);

// The link comes from the server. Only a web link is opened, and never through a shell, which would run whatever
// follows an `&` in it.
pub(crate) fn open_browser(url: &str) -> bool {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return false;
    }
    let mut cmd = if cfg!(target_os = "windows") {
        let mut c = Command::new("rundll32");
        c.args(["url.dll,FileProtocolHandler", url]);
        c
    } else if cfg!(target_os = "macos") {
        let mut c = Command::new("open");
        c.arg(url);
        c
    } else {
        let mut c = Command::new("xdg-open");
        c.arg(url);
        c
    };
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().is_ok()
}

/// Whether a browser opened from here shows up in front of the person: not over SSH, and on Linux only in a desktop session.
pub(crate) fn browser_here() -> bool {
    let set = |v: &str| std::env::var_os(v).is_some_and(|x| !x.is_empty());
    if set("SSH_CONNECTION") || set("SSH_TTY") {
        return false;
    }
    !cfg!(target_os = "linux") || set("DISPLAY") || set("WAYLAND_DISPLAY")
}

#[derive(Debug)]
pub(crate) struct NoBrowser;

impl fmt::Display for NoBrowser {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("no browser could be opened")
    }
}

impl std::error::Error for NoBrowser {}

/// One trip to the browser: signing in to Kairos when `connect`, then Hackatime when given, ending on the dashboard of
/// `server`. Every step comes back to this computer on 127.0.0.1, so there is no code to compare or paste.
pub(crate) struct Trip<'a> {
    pub server: Option<&'a str>,
    pub connect: bool,
    pub hackatime: Option<(&'a Oauth, &'a Path)>,
    pub data: &'a Path,
    pub open: &'a dyn Fn(&str) -> bool,
    pub wait: Duration,
}

pub(crate) struct Done {
    pub hackatime: Option<Result<()>>,
}

pub(crate) fn trip(t: &Trip) -> Result<Done> {
    let lb = Loopback::bind()?;
    let (mut visit, mut acct) = (None, None);
    if t.connect {
        let c = Connect::new(t.server.context("no server to sign in to")?)?;
        let url = c.url(lb.port());
        if !(t.open)(&url) {
            return Err(NoBrowser.into());
        }
        println!("Your browser should open so you can connect Kairos to your Hack Club account.\nIf it doesn't, open this link on this computer:\n  {url}");
        let v = lb.wait("/callback", c.state(), t.wait)?;
        match v.get("code").context("the browser came back without a sign-in code").and_then(|code| c.finish(code, t.data)) {
            Ok(a) => {
                println!("Signed in as {} on {}", a.username, a.server);
                (visit, acct) = (Some(v), Some(a));
            }
            Err(e) => {
                v.show("Kairos couldn't connect this computer", "Your terminal says what went wrong and what to do next.");
                return Err(e);
            }
        }
    }
    let mut set_up = None;
    if let Some((o, config)) = t.hackatime {
        let (r, v) = hackatime::via_browser(o, &lb, visit, t.open, t.data, config, t.wait);
        (set_up, visit) = (Some(r), v);
    }
    if let Some(v) = visit {
        let hackatime = matches!(set_up, Some(Ok(())));
        let q: Vec<&str> = [(t.connect, "connected=1"), (hackatime, "hackatime=1")].into_iter().filter(|(on, _)| *on).map(|(_, q)| q).collect();
        match acct.as_ref().map(|a| a.server.as_str()).or(t.server) {
            Some(s) => v.redirect(&format!("{}/me{}{}", s.trim_end_matches('/'), if q.is_empty() { "" } else { "?" }, q.join("&"))),
            None if hackatime => v.show("Hackatime is set up", "WakaTime plugins on this computer send your time to Hackatime now. You can close this tab."),
            None => v.show("Hackatime wasn't set up", "Your terminal says what went wrong and what to do next."),
        }
    }
    Ok(Done { hackatime: set_up })
}

/// What to tell the person when signing in through the browser didn't work.
pub(crate) fn retry_hint(e: &anyhow::Error) -> String {
    if e.is::<TimedOut>() {
        "Nothing came back from the browser. Run kairos login to try again, or kairos login --code to sign in with a code.".to_string()
    } else {
        format!("Could not finish signing in ({e:#}).\nRun kairos login to try again.")
    }
}

pub fn login(args: &[String]) -> Result<()> {
    let mut server = pairing::default_server();
    let mut code = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--server" => server = it.next().context("--server needs a value")?.clone(),
            s if s.starts_with("--server=") => server = s["--server=".len()..].to_string(),
            "--code" => code = true,
            other => bail!("unknown option {other:?}\nusage: kairos login [--code] [--server URL]"),
        }
    }
    let data = paths::data_dir();
    if !code && browser_here() {
        let t = Trip { server: Some(&server), connect: true, hackatime: None, data: &data, open: &open_browser, wait: WAIT };
        match trip(&t) {
            Ok(_) => return Ok(()),
            Err(e) if e.is::<NoBrowser>() => {}
            Err(e) => bail!("{}", retry_hint(&e)),
        }
    }
    with_code(&server, &data)?;
    Ok(())
}

/// Signing in with a code shown here and typed nowhere: the link works from any device, the person checks the code there.
pub(crate) fn with_code(server: &str, data: &Path) -> Result<Account> {
    let pairing = Pairing::begin(server)?;
    println!("Open this link in a browser, on this computer or any other, to connect Kairos to your account:\n  {}\nYour code is {}", pairing.verify_url(), pairing.user_code());
    if browser_here() {
        open_browser(pairing.verify_url());
    }
    let acct = pairing.wait(data)?;
    println!("Signed in as {} on {}", acct.username, acct.server);
    Ok(acct)
}

pub fn logout() -> Result<()> {
    let dir = paths::data_dir();
    if account::remove(&dir)? {
        let _ = fs::remove_file(upload::state_path(&dir));
        println!("Signed out");
    } else {
        println!("Not signed in");
    }
    Ok(())
}

pub fn status() -> Result<()> {
    let dir = paths::data_dir();
    let pending = upload::pending(&dir, &State::load(&dir));
    match account::load(&dir) {
        Some(a) => {
            println!("server   {}", a.server);
            println!("user     {}", a.username);
        }
        None => println!("Not signed in. Run kairos login to connect your account."),
    }
    println!("pending  {pending} events not uploaded");
    let upstream = wakatime::upstream(&dir);
    if let Some(line) = upstream.and_then(|u| waiting_line(wakatime::relay_waiting(&dir), u.host())) {
        println!("pending  {line}");
    }
    if let Some(line) = crate::wakatime::repointed() {
        println!("wakatime {line}");
    }
    Ok(())
}

fn waiting_line(n: usize, host: &str) -> Option<String> {
    match n {
        0 => None,
        1 => Some(format!("1 editor heartbeat waiting to reach {host}")),
        n => Some(format!("{n} editor heartbeats waiting to reach {host}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waiting_heartbeats_are_only_mentioned_when_there_are_some() {
        assert_eq!(waiting_line(0, "hackatime.hackclub.com"), None);
        assert_eq!(waiting_line(1, "hackatime.hackclub.com").as_deref(), Some("1 editor heartbeat waiting to reach hackatime.hackclub.com"));
        assert_eq!(waiting_line(12, "hackatime.hackclub.com").as_deref(), Some("12 editor heartbeats waiting to reach hackatime.hackclub.com"));
    }

    mod trip {
        use super::*;
        use std::collections::HashMap;
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        use std::path::PathBuf;
        use std::sync::mpsc;
        use std::sync::{Arc, Mutex};
        use std::thread;

        use kairos_core::pkce;
        use serde_json::{json, Value};

        const KEY: &str = "0b5f1c2d-3e4f-4a5b-8c6d-7e8f9a0b1c2d";

        struct Req {
            method: String,
            path: String,
            query: HashMap<String, String>,
            head: String,
            body: String,
        }

        enum Reply {
            Go(String),
            Json(u16, Value),
            Text(&'static str),
        }

        fn pairs(q: &str) -> HashMap<String, String> {
            reqwest::Url::parse(&format!("http://x/?{q}")).unwrap().query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect()
        }

        fn read(s: &mut TcpStream) -> Option<Req> {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            let end = loop {
                if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break i + 4;
                }
                match s.read(&mut chunk) {
                    Ok(0) | Err(_) => return None,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            };
            let head = String::from_utf8_lossy(&buf[..end]).to_string();
            let len = head.to_ascii_lowercase().lines().find_map(|l| l.strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap())).unwrap_or(0);
            let mut body = buf[end..].to_vec();
            while body.len() < len {
                let n = s.read(&mut chunk).ok()?;
                body.extend_from_slice(&chunk[..n]);
            }
            let mut first = head.split(' ');
            let method = first.next()?.to_string();
            let url = reqwest::Url::parse(&format!("http://x{}", first.next()?)).ok()?;
            let query = url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
            Some(Req { method, path: url.path().to_string(), query, head: head.to_ascii_lowercase(), body: String::from_utf8_lossy(&body).to_string() })
        }

        fn serve(handle: impl Fn(&Req) -> Reply + Send + 'static) -> String {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = l.local_addr().unwrap();
            thread::spawn(move || {
                for s in l.incoming() {
                    let Ok(mut s) = s else { return };
                    let Some(r) = read(&mut s) else { continue };
                    let (status, extra, body) = match handle(&r) {
                        Reply::Go(to) => (303, format!("Location: {to}\r\n"), String::new()),
                        Reply::Json(code, v) => (code, "Content-Type: application/json\r\n".into(), v.to_string()),
                        Reply::Text(t) => (200, String::new(), t.to_string()),
                    };
                    let _ = write!(s, "HTTP/1.1 {status} X\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                }
            });
            format!("http://{addr}")
        }

        /// Kairos's side: hands a code back to the port it was given, and trades it only with the matching verifier.
        fn kairos(refuse: bool) -> String {
            let challenge = Arc::new(Mutex::new(String::new()));
            serve(move |r| match (r.method.as_str(), r.path.as_str()) {
                ("GET", "/device/connect") => {
                    *challenge.lock().unwrap() = r.query["challenge"].clone();
                    Reply::Go(format!("http://127.0.0.1:{}/callback?code=kg_ok&state={}", r.query["port"], r.query["state"]))
                }
                ("POST", "/api/v1/device/exchange") => {
                    let v: Value = serde_json::from_str(&r.body).unwrap();
                    let ok = !refuse && v["code"] == "kg_ok" && pkce::challenge(v["verifier"].as_str().unwrap()) == *challenge.lock().unwrap();
                    match ok {
                        true => Reply::Json(200, json!({ "token": "kd_1", "username": "ada" })),
                        false => Reply::Json(400, json!({ "error": "this sign-in expired or was already used, run kairos login again" })),
                    }
                }
                ("GET", "/me") => Reply::Text("dashboard"),
                _ => Reply::Json(404, json!({})),
            })
        }

        /// Hackatime's side, as Doorkeeper does it for a public client with PKCE.
        fn hackatime(deny: bool) -> String {
            let seen = Arc::new(Mutex::new((String::new(), String::new())));
            serve(move |r| match (r.method.as_str(), r.path.as_str()) {
                ("GET", "/oauth/authorize") => {
                    let q = &r.query;
                    assert_eq!((q["client_id"].as_str(), q["response_type"].as_str(), q["code_challenge_method"].as_str()), ("app1", "code", "S256"));
                    let to = &q["redirect_uri"];
                    assert!(to.starts_with("http://127.0.0.1:") && to.ends_with("/hackatime"), "{to}");
                    *seen.lock().unwrap() = (q["code_challenge"].clone(), to.clone());
                    match deny {
                        true => Reply::Go(format!("{to}?error=access_denied&state={}", q["state"])),
                        false => Reply::Go(format!("{to}?code=hk_ok&state={}", q["state"])),
                    }
                }
                ("POST", "/oauth/token") => {
                    let f = pairs(&r.body);
                    let (challenge, to) = seen.lock().unwrap().clone();
                    let ok = f["grant_type"] == "authorization_code"
                        && f["code"] == "hk_ok"
                        && f["client_id"] == "app1"
                        && f["redirect_uri"] == to
                        && pkce::challenge(&f["code_verifier"]) == challenge;
                    match ok {
                        true => Reply::Json(200, json!({ "access_token": "at_1", "token_type": "Bearer" })),
                        false => Reply::Json(400, json!({ "error": "invalid_grant" })),
                    }
                }
                ("GET", "/api/v1/authenticated/api_keys") if r.head.contains("\r\nauthorization: bearer at_1\r\n") => Reply::Json(200, json!({ "token": KEY })),
                ("GET", "/api/v1/authenticated/api_keys") => Reply::Json(401, json!({})),
                _ => Reply::Json(404, json!({})),
            })
        }

        /// Follows redirects like a browser. Every address it went to, then the text of the page it stopped on.
        fn browse(start: String) -> Vec<String> {
            let c = reqwest::blocking::Client::builder().redirect(reqwest::redirect::Policy::none()).no_proxy().build().unwrap();
            let mut url = start;
            let mut seen = Vec::new();
            for _ in 0..10 {
                seen.push(url.clone());
                let r = c.get(&url).send().unwrap();
                match r.headers().get("location") {
                    Some(l) => url = l.to_str().unwrap().to_string(),
                    None => {
                        seen.push(r.text().unwrap());
                        break;
                    }
                }
            }
            seen
        }

        fn temp(name: &str) -> (PathBuf, PathBuf) {
            let dir = std::env::temp_dir().join(format!("kairos-trip-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            (dir.join("data"), dir.join(".wakatime.cfg"))
        }

        struct Run {
            done: Result<Done>,
            browser: Vec<String>,
        }

        fn run(server: Option<&str>, connect: bool, oauth: Option<&Oauth>, data: &Path, config: &Path) -> Run {
            fs::create_dir_all(data).unwrap();
            let (tx, rx) = mpsc::channel();
            let open = move |url: &str| {
                let (url, tx) = (url.to_string(), tx.clone());
                thread::spawn(move || tx.send(browse(url)));
                true
            };
            let t = Trip { server, connect, hackatime: oauth.map(|o| (o, config)), data, open: &open, wait: Duration::from_secs(20) };
            let done = trip(&t);
            let browser = rx.recv_timeout(Duration::from_secs(20)).unwrap_or_default();
            Run { done, browser }
        }

        #[test]
        fn one_trip_signs_in_and_sets_up_hackatime_then_ends_on_the_dashboard() {
            let (server, hk) = (kairos(false), hackatime(false));
            let (data, config) = temp("both");
            let o = Oauth::with(&hk, "app1").unwrap();
            let r = run(Some(&server), true, Some(&o), &data, &config);
            let done = r.done.unwrap();
            assert!(matches!(done.hackatime, Some(Ok(()))), "{:?}", done.hackatime.map(|h| h.err().map(|e| e.to_string())));
            assert_eq!(account::load(&data).map(|a| (a.username, a.token, a.server)), Some(("ada".into(), "kd_1".into(), server.clone())));
            assert!(wakatime::on_hackatime(&data, &config));
            assert!(fs::read_to_string(&config).unwrap().contains(KEY));
            assert_eq!(r.browser.len(), 6, "connect, back here, Hackatime, back here, the dashboard and its page: {:?}", r.browser);
            assert_eq!(r.browser[4], format!("{server}/me?connected=1&hackatime=1"));
            assert_eq!(r.browser[5], "dashboard");
        }

        #[test]
        fn saying_no_on_hackatime_still_signs_in() {
            let (server, hk) = (kairos(false), hackatime(true));
            let (data, config) = temp("deny");
            let o = Oauth::with(&hk, "app1").unwrap();
            let r = run(Some(&server), true, Some(&o), &data, &config);
            let done = r.done.unwrap();
            let err = done.hackatime.unwrap().unwrap_err().to_string();
            assert!(err.contains("wasn't authorized"), "{err}");
            assert!(account::load(&data).is_some());
            assert!(!wakatime::on_hackatime(&data, &config));
            assert_eq!(r.browser[r.browser.len() - 2], format!("{server}/me?connected=1"));
        }

        #[test]
        fn a_refused_sign_in_says_so_in_the_browser_and_saves_nothing() {
            let server = kairos(true);
            let (data, config) = temp("refused");
            let r = run(Some(&server), true, None, &data, &config);
            let err = r.done.err().unwrap();
            assert!(err.to_string().contains("already used"), "{err}");
            assert!(retry_hint(&err).contains("Run kairos login to try again"));
            assert!(r.browser.last().unwrap().contains("Kairos couldn't connect this computer"), "{:?}", r.browser);
            assert_eq!(account::load(&data), None);
        }

        #[test]
        fn hackatime_alone_ends_on_a_page_of_its_own_without_an_account() {
            let hk = hackatime(false);
            let (data, config) = temp("alone");
            let o = Oauth::with(&hk, "app1").unwrap();
            let r = run(None, false, Some(&o), &data, &config);
            assert!(matches!(r.done.unwrap().hackatime, Some(Ok(()))));
            assert!(wakatime::on_hackatime(&data, &config));
            assert!(r.browser.last().unwrap().contains("Hackatime is set up"), "{:?}", r.browser);
        }

        #[test]
        fn no_browser_and_no_answer() {
            let server = kairos(false);
            let (data, _) = temp("none");
            fs::create_dir_all(&data).unwrap();
            let t = Trip { server: Some(&server), connect: true, hackatime: None, data: &data, open: &|_| false, wait: Duration::from_secs(5) };
            assert!(trip(&t).err().unwrap().is::<NoBrowser>());
            let t = Trip { open: &|_| true, wait: Duration::from_millis(300), ..t };
            let err = trip(&t).err().unwrap();
            assert!(retry_hint(&err).contains("kairos login --code"), "{err}");
            assert_eq!(account::load(&data), None);
        }

        #[test]
        fn no_app_on_hackatime_means_no_browser_step() {
            assert!(Oauth::with("https://hackatime.example", "").is_none());
        }
    }
}
