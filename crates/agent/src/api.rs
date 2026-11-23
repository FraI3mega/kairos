//! Requests are checked before their body is read: `Host` must be ours (DNS rebinding), `Origin` must be a
//! browser-extension origin, and mutating CLI calls need `X-Kairos-Client: cli`, which a web page can't send
//! without a preflight we refuse.

use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use chrono::Local;
use kairos_core::project::project_of;
use serde::{Deserialize, Serialize};
use tiny_http::{Header, Method, Request, Response, Server};

use crate::browser::{BrowserState, Tab};
use crate::diag::diag;
use crate::exclusions::Excluded;
use crate::relay::{Entry, Relay};
use crate::sessions::Sessions;
use crate::shadow::Shadow;
use crate::status::{self, Shared, Signin, Status};
use crate::tray::pause::Pause;
use crate::wakatime::Via;
use crate::{now, platform, ports, wakatime, Msg};
use kairos_core::event::Class;

const MAX_BODY: usize = 16 * 1024;
const MAX_HEARTBEAT_BODY: usize = 1024 * 1024;
const WORKERS: usize = 4;
const RETRY: Duration = Duration::from_secs(60);
const MAX_PAUSE_MINUTES: u32 = 24 * 60;
const EXTENSION_SCHEMES: &[&str] = &["chrome-extension://", "moz-extension://", "safari-web-extension://"];

type PortProject = Box<dyn Fn(u16) -> Option<String> + Send + Sync>;
type EditorKey = Box<dyn Fn() -> Option<String> + Send + Sync>;
type PassOn = Box<dyn Fn(Entry) + Send + Sync>;

pub struct Ctx {
    hosts: [String; 2],
    status: Shared,
    browser: Arc<BrowserState>,
    excluded: Arc<Excluded>,
    msgs: Sender<Msg>,
    port_project: PortProject,
    editor_key: EditorKey,
    pass_on: PassOn,
    shadow: Arc<Shadow>,
    sessions: Arc<Sessions>,
    /// Whether kairosd has the port, which another account's kairosd may hold instead.
    bound: Arc<AtomicBool>,
    extension_seen: Mutex<Option<Instant>>,
}

/// What the WakaTime calls need besides the editor key.
pub struct Wakatime {
    pub shadow: Arc<Shadow>,
    pub sessions: Arc<Sessions>,
    pub bound: Arc<AtomicBool>,
}

impl Ctx {
    pub fn new(
        port: u16,
        data: PathBuf,
        status: Shared,
        browser: Arc<BrowserState>,
        excluded: Arc<Excluded>,
        msgs: Sender<Msg>,
        relay: Arc<Relay>,
        waka: Wakatime,
    ) -> Ctx {
        let for_ports = excluded.clone();
        Ctx::with_lookups(
            port,
            status,
            browser,
            excluded,
            msgs,
            Box::new(move |port| project_for_port(&for_ports, port)),
            Box::new(move || kairos_core::wakatime::state(&data).map(|s| s.key)),
            Box::new(move |entry| relay.push(entry)),
            waka,
        )
    }

    fn with_lookups(
        port: u16,
        status: Shared,
        browser: Arc<BrowserState>,
        excluded: Arc<Excluded>,
        msgs: Sender<Msg>,
        port_project: PortProject,
        editor_key: EditorKey,
        pass_on: PassOn,
        waka: Wakatime,
    ) -> Ctx {
        Ctx {
            hosts: [format!("127.0.0.1:{port}"), format!("localhost:{port}")],
            status,
            browser,
            excluded,
            msgs,
            port_project,
            editor_key,
            pass_on,
            shadow: waka.shadow,
            sessions: waka.sessions,
            bound: waka.bound,
            extension_seen: Mutex::new(None),
        }
    }

    fn host_ok(&self, host: Option<&str>) -> bool {
        host.is_some_and(|h| self.hosts.iter().any(|ok| ok.eq_ignore_ascii_case(h)))
    }

    fn mark_extension_seen(&self) {
        if let Ok(mut seen) = self.extension_seen.lock() {
            *seen = Some(Instant::now());
        }
    }

    fn extension_seen_secs_ago(&self) -> Option<u64> {
        self.extension_seen.lock().ok()?.map(|at| at.elapsed().as_secs())
    }
}

fn project_for_port(excluded: &Excluded, port: u16) -> Option<String> {
    let pid = ports::listener_pid(port)?;
    let cwd = platform::process_cwd(pid).filter(|c| !excluded.covers(c))?;
    project_of(&cwd).map(|project| project.id)
}

pub fn spawn(ctx: Ctx, port: u16) {
    let ctx = Arc::new(ctx);
    thread::spawn(move || serve_forever(("127.0.0.1", port), &ctx, RETRY));
}

fn serve_forever(addr: (&str, u16), ctx: &Arc<Ctx>, retry: Duration) {
    let mut warned = false;
    loop {
        match Server::http(addr) {
            Ok(server) => {
                diag!("kairosd: local API listening");
                ctx.bound.store(true, Ordering::Relaxed);
                serve(Arc::new(server), ctx);
                ctx.bound.store(false, Ordering::Relaxed);
                diag!("kairosd: local API stopped, rebinding");
            }
            Err(e) => {
                if !warned {
                    diag!("kairosd: warning: local API port unavailable, retrying every {}s ({e})", retry.as_secs());
                    warned = true;
                }
            }
        }
        thread::sleep(retry);
    }
}

fn serve(server: Arc<Server>, ctx: &Arc<Ctx>) {
    let workers: Vec<_> = (0..WORKERS)
        .map(|_| {
            let (server, ctx) = (server.clone(), ctx.clone());
            thread::spawn(move || {
                while let Ok(request) = server.recv() {
                    handle_request(&ctx, request);
                }
            })
        })
        .collect();
    for w in workers {
        let _ = w.join();
    }
}

fn header(request: &Request, name: &str) -> Option<String> {
    request
        .headers()
        .iter()
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str().to_string())
}

fn handle_request(ctx: &Ctx, mut request: Request) {
    let method = match request.method() {
        Method::Get => "GET",
        Method::Post => "POST",
        Method::Options => "OPTIONS",
        _ => "",
    };
    let url = request.url().to_string();
    let (host, origin) = (header(&request, "Host"), header(&request, "Origin"));
    let (client, content_type) = (header(&request, "X-Kairos-Client"), header(&request, "Content-Type"));
    let (authorization, user_agent) = (header(&request, "Authorization"), header(&request, "User-Agent"));
    let (machine_name, timezone) = (header(&request, "X-Machine-Name"), header(&request, "Timezone"));
    let req = Req {
        method,
        path: &url,
        host: host.as_deref(),
        origin: origin.as_deref(),
        client: client.as_deref(),
        content_type: content_type.as_deref(),
        authorization: authorization.as_deref(),
        user_agent: user_agent.as_deref(),
        machine_name: machine_name.as_deref(),
        timezone: timezone.as_deref(),
        length: request.body_length(),
    };
    let resp = match check(ctx, &req) {
        Err(resp) => resp,
        Ok(route) => {
            let mut body = Vec::new();
            let limit = route.max_body();
            let read = match route {
                Route::Tab | Route::Pause | Route::Resume | Route::Heartbeats | Route::Shadow => {
                    request.as_reader().take(limit as u64 + 1).read_to_end(&mut body).map(|_| ())
                }
                _ => Ok(()),
            };
            let cors = origin.as_deref().and_then(extension_origin).map(str::to_string);
            match read {
                Ok(()) if body.len() <= limit => dispatch(ctx, &req, route, &body).with_cors(cors),
                Ok(()) => Resp::error(413, "body too large").with_cors(cors),
                Err(_) => Resp::error(400, "could not read body").with_cors(cors),
            }
        }
    };
    let _ = request.respond(resp.into_response());
}

#[derive(Clone, Copy)]
struct Req<'a> {
    method: &'a str,
    path: &'a str,
    host: Option<&'a str>,
    origin: Option<&'a str>,
    client: Option<&'a str>,
    content_type: Option<&'a str>,
    authorization: Option<&'a str>,
    user_agent: Option<&'a str>,
    machine_name: Option<&'a str>,
    timezone: Option<&'a str>,
    length: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    Status,
    Tab,
    Pause,
    Resume,
    Heartbeats,
    Shadow,
    Today,
    Preflight,
}

impl Route {
    fn max_body(self) -> usize {
        match self {
            Route::Heartbeats | Route::Shadow => MAX_HEARTBEAT_BODY,
            _ => MAX_BODY,
        }
    }
}

struct Resp {
    status: u16,
    body: String,
    cors: Option<String>,
    allow: Option<&'static str>,
}

impl Resp {
    fn json(status: u16, value: &impl Serialize) -> Resp {
        let body = serde_json::to_string(value).unwrap_or_else(|_| "{}".into());
        Resp { status, body, cors: None, allow: None }
    }

    fn error(status: u16, message: &str) -> Resp {
        Resp::json(status, &serde_json::json!({ "error": message }))
    }

    fn with_cors(mut self, origin: Option<String>) -> Resp {
        self.cors = origin;
        self
    }

    fn into_response(self) -> Response<std::io::Cursor<Vec<u8>>> {
        let mut r = Response::from_string(self.body).with_status_code(self.status);
        let mut add = |name: &str, value: &str| {
            if let Ok(h) = Header::from_bytes(name.as_bytes(), value.as_bytes()) {
                r.add_header(h);
            }
        };
        add("Content-Type", "application/json");
        add("Cache-Control", "no-store");
        add("X-Content-Type-Options", "nosniff");
        if let Some(allow) = self.allow {
            add("Allow", allow);
        }
        if let Some(origin) = &self.cors {
            add("Access-Control-Allow-Origin", origin);
            add("Vary", "Origin");
            if self.status == 204 {
                add("Access-Control-Allow-Methods", "GET, POST");
                add("Access-Control-Allow-Headers", "Content-Type");
                add("Access-Control-Max-Age", "600");
            }
        }
        r
    }
}

fn extension_origin(origin: &str) -> Option<&str> {
    let id = EXTENSION_SCHEMES.iter().find_map(|s| origin.strip_prefix(s))?;
    let ok = !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.');
    ok.then_some(origin)
}

fn is_json(content_type: &str) -> bool {
    let (essence, _) = content_type.split_once(';').unwrap_or((content_type, ""));
    essence.trim().eq_ignore_ascii_case("application/json")
}

fn check(ctx: &Ctx, req: &Req) -> Result<Route, Resp> {
    if !ctx.host_ok(req.host) {
        return Err(Resp::error(403, "unexpected Host"));
    }
    let ext = req.origin.and_then(extension_origin);
    if req.origin.is_some() && ext.is_none() {
        return Err(Resp::error(403, "origin not allowed"));
    }
    let cors = ext.map(str::to_string);
    let fail = |status: u16, message: &str| Resp::error(status, message).with_cors(cors.clone());

    let path = req.path.split('?').next().unwrap_or_default();
    let (route, method) = match path {
        "/v1/status" => (Route::Status, "GET"),
        "/v1/browser/tab" => (Route::Tab, "POST"),
        "/v1/pause" => (Route::Pause, "POST"),
        "/v1/resume" => (Route::Resume, "POST"),
        "/wakatime/v1/users/current/heartbeats" | "/wakatime/v1/users/current/heartbeats.bulk" => (Route::Heartbeats, "POST"),
        "/wakatime/v1/users/current/statusbar/today" => (Route::Today, "GET"),
        "/shadow/v1/users/current/heartbeats" | "/shadow/v1/users/current/heartbeats.bulk" => (Route::Shadow, "POST"),
        _ => return Err(fail(404, "not found")),
    };
    if req.method == "OPTIONS" {
        return match ext {
            Some(_) if matches!(route, Route::Tab | Route::Status) => Ok(Route::Preflight),
            _ => Err(fail(403, "preflight refused")),
        };
    }
    if req.method != method {
        let mut resp = fail(405, "method not allowed");
        resp.allow = Some(method);
        return Err(resp);
    }
    match route {
        Route::Tab if ext.is_none() => return Err(fail(403, "extension origin required")),
        Route::Pause | Route::Resume if req.origin.is_some() || req.client != Some("cli") => {
            return Err(fail(403, "this call is for the kairos CLI"));
        }
        Route::Heartbeats | Route::Today | Route::Shadow if req.origin.is_some() => return Err(fail(403, "this call is for WakaTime plugins")),
        Route::Heartbeats | Route::Today if !key_ok(req, (ctx.editor_key)().as_deref()) => return Err(fail(401, "unknown api key")),
        Route::Shadow if !key_ok(req, Some(ctx.shadow.key())) => return Err(fail(401, "unknown api key")),
        _ => {}
    }
    if method == "POST" {
        if req.length.is_some_and(|n| n > route.max_body()) {
            return Err(fail(413, "body too large"));
        }
        if req.length != Some(0) && !req.content_type.is_some_and(is_json) {
            return Err(fail(415, "content type must be application/json"));
        }
    }
    Ok(route)
}

/// wakatime-cli sends the key as basic auth, other clients as a bearer token or an `api_key` parameter.
fn key_ok(req: &Req, key: Option<&str>) -> bool {
    let Some(key) = key else {
        return false;
    };
    let given = match req.authorization.and_then(|a| a.trim().split_once(' ')) {
        Some((scheme, value)) if scheme.eq_ignore_ascii_case("basic") => {
            BASE64.decode(value.trim()).ok().and_then(|b| String::from_utf8(b).ok())
        }
        Some((scheme, value)) if scheme.eq_ignore_ascii_case("bearer") => Some(value.trim().to_string()),
        _ => req.path.split_once('?').and_then(|(_, q)| q.split('&').find_map(|p| p.strip_prefix("api_key="))).map(String::from),
    };
    given.is_some_and(|g| g.split(':').any(|part| part == key))
}

fn dispatch(ctx: &Ctx, req: &Req, route: Route, body: &[u8]) -> Resp {
    match route {
        Route::Preflight => Resp { status: 204, body: String::new(), cors: None, allow: None },
        Route::Status => status(ctx, req.origin.and_then(extension_origin).is_some()),
        Route::Tab => tab(ctx, body),
        Route::Pause => pause(ctx, body),
        Route::Resume => resume(ctx, body),
        Route::Heartbeats => heartbeats(ctx, body, req, Via::Relay),
        Route::Shadow => heartbeats(ctx, body, req, Via::Shadow),
        Route::Today => Resp::json(200, &wakatime::today(ctx.status.read().map(|s| s.today_secs).unwrap_or(0.0))),
    }
}

#[derive(Serialize)]
struct StatusBody {
    #[serde(flatten)]
    status: Status,
    extension_seen_secs_ago: Option<u64>,
}

fn status(ctx: &Ctx, from_extension: bool) -> Resp {
    let Ok(mut status) = ctx.status.read().map(|s| s.clone()) else {
        return Resp::error(500, "status unavailable");
    };
    // Any installed extension can call us, and whoever approves a pairing code owns the device's
    // uploads, so the code is only shown to the CLI and the tray.
    if from_extension {
        if let Signin::Waiting { code } = &mut status.signin {
            code.clear();
        }
    }
    Resp::json(200, &StatusBody { status, extension_seen_secs_ago: ctx.extension_seen_secs_ago() })
}

fn parse<'a, T: Deserialize<'a>>(body: &'a [u8]) -> Result<T, Resp> {
    serde_json::from_slice(body).map_err(|e| Resp::error(400, &format!("invalid body: {e}")))
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Browser {
    Chrome,
    Edge,
    Brave,
    Vivaldi,
    Opera,
    Arc,
    Firefox,
}

impl Browser {
    fn family(self) -> &'static str {
        match self {
            Browser::Chrome => "chrome",
            Browser::Edge => "edge",
            Browser::Brave => "brave",
            Browser::Vivaldi => "vivaldi",
            Browser::Opera => "opera",
            Browser::Arc => "arc",
            Browser::Firefox => "firefox",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TabKind {
    Local,
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TabReport {
    browser: Browser,
    kind: TabKind,
    port: Option<u16>,
}

fn tab(ctx: &Ctx, body: &[u8]) -> Resp {
    let report: TabReport = match parse(body) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let tab = match resolve(ctx, &report) {
        Ok(t) => t,
        Err(message) => return Resp::error(400, message),
    };
    ctx.browser.report(report.browser.family(), tab);
    ctx.mark_extension_seen();
    Resp::json(200, &serde_json::json!({}))
}

fn resolve(ctx: &Ctx, r: &TabReport) -> Result<Tab, &'static str> {
    let tab = |class, project| Tab { class, project };
    match (r.kind, r.port) {
        (TabKind::Local, Some(port)) if port != 0 => Ok(match (ctx.port_project)(port) {
            Some(id) => tab(Class::Testing, Some(id)),
            None => tab(Class::Other, None),
        }),
        (TabKind::Local, _) => Err("kind local needs a port"),
        (TabKind::Other, Some(_)) => Err("a port is only for kind local"),
        (TabKind::Other, None) => Ok(tab(Class::Other, None)),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PauseReq {
    minutes: Option<u32>,
    until: Option<String>,
}

fn pause(ctx: &Ctx, body: &[u8]) -> Resp {
    let req: PauseReq = match parse(body) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let until = match (req.minutes, req.until.as_deref()) {
        (Some(m), None) if (1..=MAX_PAUSE_MINUTES).contains(&m) => now() + f64::from(m) * 60.0,
        (Some(_), None) => return Resp::error(400, "minutes must be between 1 and 1440"),
        (None, Some("tomorrow")) => match Pause::Tomorrow.until(&Local::now()) {
            Some(t) => t,
            None => return Resp::error(500, "could not work out tomorrow"),
        },
        (None, Some(_)) => return Resp::error(400, "until must be \"tomorrow\""),
        _ => return Resp::error(400, "give either minutes or until"),
    };
    if ctx.msgs.send(Msg::Pause(until)).is_err() {
        return Resp::error(503, "recorder is not running");
    }
    status::set(&ctx.status, |s| s.paused_until = Some(until));
    Resp::json(200, &serde_json::json!({ "paused_until": until }))
}

fn resume(ctx: &Ctx, body: &[u8]) -> Resp {
    if !body.is_empty() && !matches!(serde_json::from_slice::<serde_json::Value>(body), Ok(v) if v.as_object().is_some_and(|o| o.is_empty())) {
        return Resp::error(400, "resume takes no fields");
    }
    if ctx.msgs.send(Msg::Resume).is_err() {
        return Resp::error(503, "recorder is not running");
    }
    status::set(&ctx.status, |s| s.paused_until = None);
    Resp::json(200, &serde_json::json!({}))
}

/// Answered even while tracking is paused and the recorder drops them: a plugin keeps heartbeats it got no
/// answer for and sends them again later. Relayed ones are passed on, and wakatime-cli just ran, so the shadow
/// runs too.
fn heartbeats(ctx: &Ctx, body: &[u8], req: &Req, via: Via) -> Resp {
    let beats: serde_json::Value = match parse(body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let session = |s: &str| ctx.sessions.cwd(s);
    let (events, responses) = wakatime::heartbeats(&beats, req.user_agent, via, project_of, session, &ctx.excluded.current());
    for e in events {
        if ctx.msgs.send(Msg::Event(e)).is_err() {
            return Resp::error(503, "recorder is not running");
        }
    }
    if via == Via::Shadow {
        return Resp::json(202, &serde_json::json!({ "responses": responses }));
    }
    ctx.shadow.nudge();
    let path = req.path.split('?').next().unwrap_or_default();
    (ctx.pass_on)(Entry {
        bulk: path.ends_with(".bulk"),
        user_agent: req.user_agent.map(str::to_string),
        machine_name: req.machine_name.map(str::to_string),
        timezone: req.timezone.map(str::to_string),
        body: body.to_vec(),
    });
    Resp::json(202, &serde_json::json!({ "responses": responses }))
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc::{self, Receiver};

    use kairos_core::event::Kind;
    use serde_json::{json, Value};

    use super::*;

    const PORT: u16 = 24816;
    const EXT: &str = "chrome-extension://abcdefghijklmnopabcdefghijklmnop";
    const KEY: &str = "0b5f1c2d-3e4f-4a5b-8c6d-7e8f9a0b1c2d";

    fn ctx() -> (Arc<Ctx>, Receiver<Msg>) {
        ctx_on(PORT)
    }

    fn ctx_on(port: u16) -> (Arc<Ctx>, Receiver<Msg>) {
        ctx_with(port, no_exclusions())
    }

    fn no_exclusions() -> Arc<Excluded> {
        Arc::new(Excluded::new(std::env::temp_dir().join(format!("kairos-api-none-{}", std::process::id()))))
    }

    const SHADOW_KEY: &str = "5a1d0c2e-3f4a-4b5c-8d6e-7f8a9b0c1d2e";

    fn waka() -> Wakatime {
        Wakatime {
            shadow: Arc::new(Shadow::with_key(SHADOW_KEY.into())),
            sessions: Arc::new(Sessions::in_memory()),
            bound: Arc::new(AtomicBool::new(false)),
        }
    }

    fn ctx_with(port: u16, excluded: Arc<Excluded>) -> (Arc<Ctx>, Receiver<Msg>) {
        let (tx, rx) = mpsc::channel();
        let port_project: PortProject = Box::new(|port| (port == 3000).then(|| "o/app".to_string()));
        let ctx = Ctx::with_lookups(
            port,
            status::shared(),
            Arc::new(BrowserState::default()),
            excluded,
            tx,
            port_project,
            Box::new(|| Some(KEY.to_string())),
            Box::new(|_| {}),
            waka(),
        );
        (Arc::new(ctx), rx)
    }

    fn req<'a>(method: &'a str, path: &'a str) -> Req<'a> {
        Req {
            method,
            path,
            host: Some("127.0.0.1:24816"),
            origin: None,
            client: None,
            content_type: None,
            authorization: None,
            user_agent: None,
            machine_name: None,
            timezone: None,
            length: Some(0),
        }
    }

    fn handle(ctx: &Ctx, r: &Req, body: &[u8]) -> Resp {
        let r = Req { length: Some(body.len()), ..*r };
        match check(ctx, &r) {
            Ok(route) => {
                let cors = r.origin.and_then(extension_origin).map(str::to_string);
                dispatch(ctx, &r, route, body).with_cors(cors)
            }
            Err(resp) => resp,
        }
    }

    fn json_of(resp: &Resp) -> Value {
        serde_json::from_str(&resp.body).unwrap_or(Value::Null)
    }

    fn post<'a>(path: &'a str, origin: Option<&'a str>, client: Option<&'a str>) -> Req<'a> {
        Req { origin, client, content_type: Some("application/json"), ..req("POST", path) }
    }

    #[test]
    fn host_header_must_be_ours() {
        let (ctx, _rx) = ctx();
        for host in [None, Some("evil.com"), Some("127.0.0.1"), Some("127.0.0.1:1"), Some("localhost.evil.com:24816"), Some("[::1]:24816")] {
            let r = Req { host, ..req("GET", "/v1/status") };
            assert_eq!(handle(&ctx, &r, b"").status, 403, "{host:?}");
        }
        for host in ["127.0.0.1:24816", "localhost:24816", "LOCALHOST:24816"] {
            let r = Req { host: Some(host), ..req("GET", "/v1/status") };
            assert_eq!(handle(&ctx, &r, b"").status, 200, "{host}");
        }
    }

    #[test]
    fn only_extension_origins_get_through_and_cors() {
        let (ctx, _rx) = ctx();
        for origin in ["https://evil.com", "http://localhost:24816", "null", "chrome-extension://", "chrome-extension://a b", "chrome-extension://a/b", "file://"] {
            let r = Req { origin: Some(origin), ..req("GET", "/v1/status") };
            let resp = handle(&ctx, &r, b"");
            assert_eq!(resp.status, 403, "{origin}");
            assert_eq!(resp.cors, None);
        }
        for origin in [EXT, "moz-extension://6b2f4c1e-0000-4000-8000-123456789abc", "safari-web-extension://AB12-cd"] {
            let r = Req { origin: Some(origin), ..req("GET", "/v1/status") };
            let resp = handle(&ctx, &r, b"");
            assert_eq!(resp.status, 200, "{origin}");
            assert_eq!(resp.cors.as_deref(), Some(origin));
        }
        let none = handle(&ctx, &req("GET", "/v1/status"), b"");
        assert_eq!((none.status, none.cors), (200, None));
    }

    #[test]
    fn pairing_code_is_hidden_from_extensions() {
        let (ctx, _rx) = ctx();
        status::set(&ctx.status, |s| s.signin = Signin::Waiting { code: "ABCD-1234".into() });
        let ext = json_of(&handle(&ctx, &Req { origin: Some(EXT), ..req("GET", "/v1/status") }, b""));
        assert_eq!(ext["signin"]["waiting"]["code"], "");
        let cli = json_of(&handle(&ctx, &req("GET", "/v1/status"), b""));
        assert_eq!(cli["signin"]["waiting"]["code"], "ABCD-1234");
    }

    #[test]
    fn preflight_is_answered_for_extensions_only() {
        let (ctx, _rx) = ctx();
        let ok = handle(&ctx, &Req { origin: Some(EXT), ..req("OPTIONS", "/v1/browser/tab") }, b"");
        assert_eq!((ok.status, ok.cors.as_deref()), (204, Some(EXT)));
        for origin in [Some("https://evil.com"), None] {
            let r = Req { origin, ..req("OPTIONS", "/v1/browser/tab") };
            assert_eq!(handle(&ctx, &r, b"").status, 403, "{origin:?}");
        }
        let pause = Req { origin: Some(EXT), ..req("OPTIONS", "/v1/pause") };
        assert_eq!(handle(&ctx, &pause, b"").status, 403);
    }

    #[test]
    fn unknown_paths_and_methods() {
        let (ctx, _rx) = ctx();
        assert_eq!(handle(&ctx, &req("GET", "/"), b"").status, 404);
        assert_eq!(handle(&ctx, &req("GET", "/v1/nope"), b"").status, 404);
        let wrong = handle(&ctx, &req("POST", "/v1/status"), b"");
        assert_eq!((wrong.status, wrong.allow), (405, Some("GET")));
        assert_eq!(handle(&ctx, &req("GET", "/v1/status?x=1"), b"").status, 200);
        assert_eq!(handle(&ctx, &req("DELETE", "/v1/status"), b"").status, 405);
    }

    #[test]
    fn tab_reports_need_an_extension_origin() {
        let (ctx, _rx) = ctx();
        let body = br#"{"browser":"chrome","kind":"other"}"#;
        assert_eq!(handle(&ctx, &post("/v1/browser/tab", None, None), body).status, 403);
        assert_eq!(handle(&ctx, &post("/v1/browser/tab", None, Some("cli")), body).status, 403);
        assert_eq!(handle(&ctx, &post("/v1/browser/tab", Some("https://evil.com"), None), body).status, 403);
        assert_eq!(handle(&ctx, &post("/v1/browser/tab", Some(EXT), None), body).status, 200);
    }

    #[test]
    fn tab_kinds_resolve_to_classes() {
        let (ctx, _rx) = ctx();
        let send = |body: Value| {
            let resp = handle(&ctx, &post("/v1/browser/tab", Some(EXT), None), body.to_string().as_bytes());
            (resp.status, ctx.browser.current("chrome"))
        };
        let tab = |class, project: Option<&str>| Some(Tab { class, project: project.map(String::from) });
        assert_eq!(send(json!({"browser":"chrome","kind":"local","port":3000})), (200, tab(Class::Testing, Some("o/app"))));
        assert_eq!(send(json!({"browser":"chrome","kind":"local","port":4000})), (200, tab(Class::Other, None)));
        assert_eq!(send(json!({"browser":"chrome","kind":"other"})), (200, tab(Class::Other, None)));
        assert_eq!(ctx.browser.current("firefox"), None);
    }

    #[test]
    fn tab_bodies_are_validated_strictly() {
        let (ctx, _rx) = ctx();
        let bad = |body: &str| handle(&ctx, &post("/v1/browser/tab", Some(EXT), None), body.as_bytes()).status;
        assert_eq!(bad(""), 400);
        assert_eq!(bad("not json"), 400);
        assert_eq!(bad("[]"), 400);
        assert_eq!(bad(r#"{"browser":"chrome"}"#), 400);
        assert_eq!(bad(r#"{"browser":"safari","kind":"other"}"#), 400);
        assert_eq!(bad(r#"{"browser":"chrome","kind":"banking"}"#), 400);
        for gone in ["docs", "web_agent", "site"] {
            assert_eq!(bad(&format!(r#"{{"browser":"chrome","kind":"{gone}"}}"#)), 400, "{gone}");
        }
        assert_eq!(bad(r#"{"browser":"chrome","kind":"other","url":"https://x.com"}"#), 400);
        assert_eq!(bad(r#"{"browser":"chrome","kind":"other","port":3000}"#), 400);
        assert_eq!(bad(r#"{"browser":"chrome","kind":"other","host":"a.com"}"#), 400);
        assert_eq!(bad(r#"{"browser":"chrome","kind":"local"}"#), 400);
        assert_eq!(bad(r#"{"browser":"chrome","kind":"local","port":0}"#), 400);
        assert_eq!(bad(r#"{"browser":"chrome","kind":"local","port":70000}"#), 400);
        assert_eq!(bad(r#"{"browser":"chrome","kind":"local","port":3000,"host":"a.com"}"#), 400);
        assert_eq!(ctx.browser.current("chrome"), None);
    }

    #[test]
    fn bodies_must_be_small_json() {
        let (ctx, _rx) = ctx();
        let body = br#"{"browser":"chrome","kind":"other"}"#;
        let plain = Req { content_type: Some("text/plain"), ..post("/v1/browser/tab", Some(EXT), None) };
        assert_eq!(handle(&ctx, &plain, body).status, 415);
        let none = Req { content_type: None, ..post("/v1/browser/tab", Some(EXT), None) };
        assert_eq!(handle(&ctx, &none, body).status, 415);
        let charset = Req { content_type: Some("application/json; charset=utf-8"), ..post("/v1/browser/tab", Some(EXT), None) };
        assert_eq!(handle(&ctx, &charset, body).status, 200);
        let big = Req { length: Some(MAX_BODY + 1), ..post("/v1/browser/tab", Some(EXT), None) };
        assert_eq!(check(&ctx, &big).err().map(|r| r.status), Some(413));
        let max = vec![b' '; MAX_BODY];
        assert_eq!(handle(&ctx, &post("/v1/browser/tab", Some(EXT), None), &max).status, 400);
    }

    #[test]
    fn pause_and_resume_are_for_the_cli() {
        let (ctx, rx) = ctx();
        let minutes = br#"{"minutes":30}"#;
        assert_eq!(handle(&ctx, &post("/v1/pause", None, None), minutes).status, 403);
        assert_eq!(handle(&ctx, &post("/v1/pause", None, Some("browser")), minutes).status, 403);
        assert_eq!(handle(&ctx, &post("/v1/pause", Some("https://evil.com"), Some("cli")), minutes).status, 403);
        assert_eq!(handle(&ctx, &post("/v1/pause", Some(EXT), Some("cli")), minutes).status, 403);
        assert_eq!(handle(&ctx, &post("/v1/resume", None, None), b"").status, 403);
        assert!(rx.try_recv().is_err());

        let resp = handle(&ctx, &post("/v1/pause", None, Some("cli")), minutes);
        assert_eq!(resp.status, 200);
        let Ok(Msg::Pause(until)) = rx.try_recv() else { panic!("expected a pause") };
        assert!((until - now() - 1800.0).abs() < 5.0);
        assert_eq!(json_of(&resp)["paused_until"].as_f64(), Some(until));
        let shown = || json_of(&handle(&ctx, &req("GET", "/v1/status"), b""))["paused_until"].as_f64();
        assert_eq!(shown(), Some(until));

        let cli_empty = Req { content_type: None, ..post("/v1/resume", None, Some("cli")) };
        assert_eq!(handle(&ctx, &cli_empty, b"").status, 200);
        assert!(matches!(rx.try_recv(), Ok(Msg::Resume)));
        assert_eq!(shown(), None);
        assert_eq!(handle(&ctx, &post("/v1/resume", None, Some("cli")), b"{}").status, 200);
        assert_eq!(handle(&ctx, &post("/v1/resume", None, Some("cli")), br#"{"x":1}"#).status, 400);
    }

    #[test]
    fn pause_bodies_are_validated() {
        let (ctx, rx) = ctx();
        let bad = |body: &str| handle(&ctx, &post("/v1/pause", None, Some("cli")), body.as_bytes()).status;
        for body in [
            "", "{}", r#"{"minutes":0}"#, r#"{"minutes":1441}"#, r#"{"minutes":-5}"#, r#"{"minutes":"30"}"#,
            r#"{"until":"never"}"#, r#"{"minutes":30,"until":"tomorrow"}"#, r#"{"minutes":30,"extra":1}"#,
        ] {
            assert_eq!(bad(body), 400, "{body}");
        }
        assert!(rx.try_recv().is_err());
        assert_eq!(bad(r#"{"until":"tomorrow"}"#), 200);
        let Ok(Msg::Pause(until)) = rx.try_recv() else { panic!("expected a pause") };
        assert!(until > now() && until <= now() + 25.0 * 3600.0);
        assert_eq!(bad(r#"{"minutes":1440}"#), 200);
    }

    #[test]
    fn tomorrow_is_the_next_local_midnight() {
        use chrono::TimeZone;
        let now = Local::now();
        let t = Pause::Tomorrow.until(&now).unwrap();
        let at = Local.timestamp_opt(t as i64, 0).unwrap();
        assert_eq!(at.date_naive(), now.date_naive().succ_opt().unwrap());
        assert_eq!(at.format("%H:%M").to_string(), "00:00");
    }

    #[test]
    fn status_reports_when_the_extension_was_last_seen() {
        let (ctx, _rx) = ctx();
        let get = || json_of(&handle(&ctx, &req("GET", "/v1/status"), b""));
        let before = get();
        assert_eq!(before["extension_seen_secs_ago"], Value::Null);
        assert_eq!(before["today_secs"], json!(0.0));
        assert!(before["version"].is_string());
        handle(&ctx, &post("/v1/browser/tab", Some(EXT), None), br#"{"browser":"chrome","kind":"other"}"#);
        assert_eq!(get()["extension_seen_secs_ago"], json!(0));
    }

    // Not a test of its own: the test below re-runs this binary on it so a separate process, whose
    // working folder is a project, ends up owning a listening socket.
    #[test]
    fn port_listener_helper() {
        if std::env::var_os("KAIROS_PORT_HELPER").is_none() {
            return;
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        println!("PORT={}", listener.local_addr().unwrap().port());
        let _ = std::io::stdin().read(&mut [0u8; 1]);
    }

    #[cfg(any(target_os = "linux", windows))]
    #[test]
    fn a_local_port_resolves_to_the_project_of_its_listener() {
        use std::io::{BufRead, BufReader};
        use std::process::{Command, Stdio};

        let repo = std::env::temp_dir().join(format!("kairos-api-port-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join(".git/config"), "[remote \"origin\"]\n\turl = git@github.com:octo/widget.git\n").unwrap();

        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "api::tests::port_listener_helper", "--nocapture"])
            .env("KAIROS_PORT_HELPER", "1")
            .current_dir(&repo)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let out = BufReader::new(child.stdout.take().unwrap());
        let port: u16 = out
            .lines()
            .map_while(Result::ok)
            .find_map(|l| l.strip_prefix("PORT=").and_then(|p| p.parse().ok()))
            .expect("the helper prints its port");

        let data = repo.with_extension("data");
        kairos_core::exclude::add(&data, &repo).unwrap();
        let hidden = project_for_port(&Excluded::new(data.clone()), port);
        let found = project_for_port(&no_exclusions(), port);
        let pid = ports::listener_pid(port);
        let _ = std::fs::remove_dir_all(&data);
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&repo);
        assert_eq!(pid, Some(child.id()));
        assert_eq!(found.as_deref(), Some("octo/widget"));
        assert_eq!(hidden, None, "a listener in an excluded folder");
        assert_eq!(ports::listener_pid(1), None);
    }

    fn start() -> (u16, Receiver<Msg>) {
        let server = Arc::new(Server::http("127.0.0.1:0").unwrap());
        let port = server.server_addr().to_ip().unwrap().port();
        let (ctx, rx) = ctx_on(port);
        thread::spawn(move || serve(server, &ctx));
        (port, rx)
    }

    fn raw(port: u16, head: &str, body: &str) -> (u16, String, String) {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let req = format!("{head}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        s.write_all(req.as_bytes()).unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        let (headers, body) = out.split_once("\r\n\r\n").unwrap_or((&out, ""));
        let status = headers.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        (status, headers.to_string(), body.to_string())
    }

    #[test]
    fn end_to_end_over_real_sockets() {
        let (port, rx) = start();
        let host = format!("Host: 127.0.0.1:{port}");
        let ext = format!("Origin: {EXT}");
        let json_type = "Content-Type: application/json";

        let (code, _, body) = raw(port, &format!("GET /v1/status HTTP/1.1\r\n{host}"), "");
        assert_eq!(code, 200);
        assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["extension_seen_secs_ago"], Value::Null);

        let (code, _, body) = raw(port, "GET /v1/status HTTP/1.1\r\nHost: evil.example:24816", "");
        assert_eq!(code, 403, "{body}");
        let (code, _, _) = raw(port, &format!("GET /v1/status HTTP/1.1\r\nHost: evil.example:{port}"), "");
        assert_eq!(code, 403);

        let web = format!("{host}\r\nOrigin: https://evil.example");
        assert_eq!(raw(port, &format!("GET /v1/status HTTP/1.1\r\n{web}"), "").0, 403);
        let tab = r#"{"browser":"firefox","kind":"other"}"#;
        let (code, headers, _) = raw(port, &format!("POST /v1/browser/tab HTTP/1.1\r\n{web}\r\n{json_type}"), tab);
        assert_eq!(code, 403);
        assert!(!headers.to_ascii_lowercase().contains("access-control-allow-origin"));
        assert_eq!(raw(port, &format!("OPTIONS /v1/browser/tab HTTP/1.1\r\n{web}\r\nAccess-Control-Request-Method: POST"), "").0, 403);
        assert_eq!(raw(port, &format!("POST /v1/pause HTTP/1.1\r\n{web}\r\n{json_type}\r\nX-Kairos-Client: cli"), r#"{"minutes":5}"#).0, 403);
        assert!(rx.try_recv().is_err());

        let (code, headers, _) = raw(port, &format!("OPTIONS /v1/browser/tab HTTP/1.1\r\n{host}\r\n{ext}\r\nAccess-Control-Request-Method: POST"), "");
        assert_eq!(code, 204);
        assert!(headers.contains(&format!("Access-Control-Allow-Origin: {EXT}")));

        let (code, headers, _) = raw(port, &format!("POST /v1/browser/tab HTTP/1.1\r\n{host}\r\n{ext}\r\n{json_type}"), tab);
        assert_eq!(code, 200);
        assert!(headers.contains(&format!("Access-Control-Allow-Origin: {EXT}")));
        let (_, _, body) = raw(port, &format!("GET /v1/status HTTP/1.1\r\n{host}"), "");
        assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["extension_seen_secs_ago"], json!(0));

        assert_eq!(raw(port, &format!("POST /v1/browser/tab HTTP/1.1\r\n{host}\r\n{ext}\r\nContent-Type: text/plain"), tab).0, 415);
        let huge = "x".repeat(MAX_BODY + 1);
        assert_eq!(raw(port, &format!("POST /v1/browser/tab HTTP/1.1\r\n{host}\r\n{ext}\r\n{json_type}"), &huge).0, 413);
        assert_eq!(raw(port, &format!("POST /v1/browser/tab HTTP/1.1\r\n{host}\r\n{ext}\r\n{json_type}"), r#"{"browser":"chrome","kind":"other","url":"https://x"}"#).0, 400);

        assert_eq!(raw(port, &format!("POST /v1/pause HTTP/1.1\r\n{host}\r\n{json_type}"), r#"{"minutes":5}"#).0, 403);
        let cli = format!("{host}\r\n{json_type}\r\nX-Kairos-Client: cli");
        assert_eq!(raw(port, &format!("POST /v1/pause HTTP/1.1\r\n{cli}"), r#"{"minutes":5}"#).0, 200);
        assert!(matches!(rx.try_recv(), Ok(Msg::Pause(_))));
        assert_eq!(raw(port, &format!("POST /v1/resume HTTP/1.1\r\n{cli}"), "").0, 200);
        assert!(matches!(rx.try_recv(), Ok(Msg::Resume)));
        assert_eq!(raw(port, &format!("GET /v1/nope HTTP/1.1\r\n{host}"), "").0, 404);
    }

    #[test]
    fn a_taken_port_is_retried_until_it_frees_up() {
        let held = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = held.local_addr().unwrap().port();
        let (ctx, _rx) = ctx_on(port);
        thread::spawn(move || serve_forever(("127.0.0.1", port), &ctx, Duration::from_millis(100)));
        thread::sleep(Duration::from_millis(250));
        drop(held);
        let host = format!("Host: 127.0.0.1:{port}");
        for _ in 0..50 {
            // Another test can take the freed port for a moment, so a refused connection only means not yet.
            if let Ok(mut s) = TcpStream::connect(("127.0.0.1", port)) {
                let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
                let _ = s.write_all(format!("GET /v1/status HTTP/1.1\r\n{host}\r\nConnection: close\r\n\r\n").as_bytes());
                let mut out = String::new();
                let _ = s.read_to_string(&mut out);
                if out.split_whitespace().nth(1) == Some("200") {
                    return;
                }
            }
            thread::sleep(Duration::from_millis(100));
        }
        panic!("the API never came up on the freed port");
    }

    fn plugin<'a>(method: &'a str, path: &'a str, authorization: Option<&'a str>) -> Req<'a> {
        let content_type = (method == "POST").then_some("application/json");
        Req { authorization, content_type, user_agent: Some("wakatime/v1.102.1 (linux) go1.22.5 vim/9.1 vim-wakatime/11.2.0"), ..req(method, path) }
    }

    #[test]
    fn wakatime_plugins_need_the_key_and_no_origin() {
        let (ctx, rx) = ctx();
        let basic = format!("Basic {}", BASE64.encode(KEY));
        let bulk = "/wakatime/v1/users/current/heartbeats.bulk";
        let beats = br#"[{"entity":"/nowhere/a.rs","type":"file","time":1700000000.25,"language":"Rust"},{"type":"file"}]"#;
        let today = "/wakatime/v1/users/current/statusbar/today";
        let wrong = format!("Basic {}", BASE64.encode("00000000-0000-4000-8000-000000000000"));
        for auth in [None, Some(wrong.as_str()), Some("Basic !!"), Some("Token x")] {
            assert_eq!(handle(&ctx, &plugin("POST", bulk, auth), beats).status, 401, "{auth:?}");
            assert_eq!(handle(&ctx, &plugin("GET", today, auth), b"").status, 401, "{auth:?}");
        }
        let from_page = Req { origin: Some("https://evil.com"), ..plugin("POST", bulk, Some(&basic)) };
        assert_eq!(handle(&ctx, &from_page, beats).status, 403);
        let from_extension = Req { origin: Some(EXT), ..plugin("POST", bulk, Some(&basic)) };
        assert_eq!(handle(&ctx, &from_extension, beats).status, 403);
        assert!(rx.try_recv().is_err());

        let resp = handle(&ctx, &plugin("POST", bulk, Some(&basic)), beats);
        assert_eq!(resp.status, 202);
        let body = json_of(&resp);
        assert_eq!(body["responses"][0][1], 201);
        assert_eq!(body["responses"][1][1], 400);
        match rx.try_recv() {
            Ok(Msg::Event(e)) => assert!(
                matches!(&e.kind, Kind::Heartbeat { at, editor: Some(ed), language: Some(l), .. } if *at == 1_700_000_000.25 && ed == "vim" && l == "Rust"),
                "{e:?}"
            ),
            Ok(_) => panic!("not an event"),
            Err(e) => panic!("{e:?}"),
        }
        assert!(rx.try_recv().is_err(), "the heartbeat without a time is answered, not recorded");

        let single = "/wakatime/v1/users/current/heartbeats?api_key=0b5f1c2d-3e4f-4a5b-8c6d-7e8f9a0b1c2d";
        assert_eq!(handle(&ctx, &plugin("POST", single, None), br#"{"time":5}"#).status, 202);
        let bearer = format!("Bearer {KEY}");
        assert_eq!(handle(&ctx, &plugin("POST", bulk, Some(&bearer)), b"[]").status, 202);

        status::set(&ctx.status, |s| s.today_secs = 4_000.0);
        let resp = handle(&ctx, &plugin("GET", today, Some(&basic)), b"");
        assert_eq!((resp.status, json_of(&resp)["data"]["grand_total"]["text"].as_str()), (200, Some("1 hr 6 mins")));
    }

    #[test]
    fn heartbeats_from_an_excluded_folder_are_answered_and_not_recorded() {
        let data = std::env::temp_dir().join(format!("kairos-api-excluded-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&data);
        let secret = data.join("secret");
        kairos_core::exclude::add(&data, &secret).unwrap();
        let (ctx, rx) = ctx_with(PORT, Arc::new(Excluded::new(data.clone())));
        let basic = format!("Basic {}", BASE64.encode(KEY));
        let beats = json!([
            { "entity": secret.join("keys.rs"), "type": "file", "time": 1_700_000_000.0 },
            { "entity": data.join("secrets").join("a.rs"), "type": "file", "time": 1_700_000_001.0 },
        ]);
        let resp = handle(&ctx, &plugin("POST", "/wakatime/v1/users/current/heartbeats.bulk", Some(&basic)), beats.to_string().as_bytes());
        assert_eq!(resp.status, 202);
        let body = json_of(&resp);
        assert_eq!((body["responses"][0][1].clone(), body["responses"][1][1].clone()), (json!(201), json!(201)));
        let Ok(Msg::Event(e)) = rx.try_recv() else { panic!("the sibling folder's heartbeat is recorded") };
        assert!(matches!(e.kind, Kind::Heartbeat { at, .. } if at == 1_700_000_001.0));
        assert!(rx.try_recv().is_err());
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn accepted_heartbeat_requests_are_queued_to_be_passed_on_as_they_came() {
        let (tx, _rx) = mpsc::channel();
        let queued = Arc::new(Mutex::new(Vec::new()));
        let log = queued.clone();
        let ctx = Ctx::with_lookups(
            PORT,
            status::shared(),
            Arc::new(BrowserState::default()),
            no_exclusions(),
            tx,
            Box::new(|_| None),
            Box::new(|| Some(KEY.to_string())),
            Box::new(move |entry| log.lock().unwrap().push(entry)),
            waka(),
        );
        let basic = format!("Basic {}", BASE64.encode(KEY));
        let bulk = "/wakatime/v1/users/current/heartbeats.bulk";
        let single = "/wakatime/v1/users/current/heartbeats?api_key=0b5f1c2d-3e4f-4a5b-8c6d-7e8f9a0b1c2d";
        let machine = Req { machine_name: Some("laptop"), timezone: Some("Europe/Berlin"), ..plugin("POST", bulk, Some(&basic)) };
        let beats = br#"[ {"entity":"/nowhere/a.rs","time":1700000000.25} ]"#;
        assert_eq!(handle(&ctx, &machine, beats).status, 202);
        assert_eq!(handle(&ctx, &plugin("POST", single, None), br#"{"time":5}"#).status, 202);

        assert_eq!(handle(&ctx, &plugin("POST", bulk, None), beats).status, 401);
        assert_eq!(handle(&ctx, &plugin("POST", bulk, Some(&basic)), b"{ nope").status, 400);
        let browser = Req { origin: Some(EXT), ..plugin("POST", bulk, Some(&basic)) };
        assert_eq!(handle(&ctx, &browser, beats).status, 403);

        let queued = queued.lock().unwrap();
        let ua = Some("wakatime/v1.102.1 (linux) go1.22.5 vim/9.1 vim-wakatime/11.2.0".to_string());
        assert_eq!(queued.len(), 2, "only what was accepted");
        assert_eq!(
            queued[0],
            Entry { bulk: true, user_agent: ua.clone(), machine_name: Some("laptop".into()), timezone: Some("Europe/Berlin".into()), body: beats.to_vec() }
        );
        assert_eq!(queued[1], Entry { bulk: false, user_agent: ua, machine_name: None, timezone: None, body: br#"{"time":5}"#.to_vec() });
        assert!(ctx.shadow.nudged(), "wakatime-cli just ran, so the shadow runs too");
    }

    #[test]
    fn the_shadow_has_its_own_key_and_its_heartbeats_go_nowhere() {
        let (tx, rx) = mpsc::channel();
        let queued = Arc::new(Mutex::new(Vec::new()));
        let log = queued.clone();
        let ctx = Ctx::with_lookups(
            PORT,
            status::shared(),
            Arc::new(BrowserState::default()),
            no_exclusions(),
            tx,
            Box::new(|_| None),
            Box::new(|| Some(KEY.to_string())),
            Box::new(move |entry| log.lock().unwrap().push(entry)),
            waka(),
        );
        ctx.sessions.note("s1", std::path::Path::new("/nowhere/w"));
        let bulk = "/shadow/v1/users/current/heartbeats.bulk";
        let beats = br#"[
            {"entity":"/nowhere/a.rs","type":"file","time":1700000000.25},
            {"entity":"Claude s1","type":"app","time":1700000001.0,"ai_session":"s1","user_agent":"wakatime/v2 (x) go1 claude-code/2"}
        ]"#;
        let editor = format!("Basic {}", BASE64.encode(KEY));
        assert_eq!(handle(&ctx, &plugin("POST", bulk, Some(&editor)), beats).status, 401, "the editor key is not the shadow's");
        let shadow = format!("Basic {}", BASE64.encode(SHADOW_KEY));
        let relay = plugin("POST", "/wakatime/v1/users/current/heartbeats.bulk", Some(&shadow));
        assert_eq!(handle(&ctx, &relay, beats).status, 401, "nor the other way round");
        let resp = handle(&ctx, &plugin("POST", bulk, Some(&shadow)), beats);
        assert_eq!(resp.status, 202);
        assert_eq!(json_of(&resp)["responses"].as_array().map(Vec::len), Some(2));
        let Ok(Msg::Event(e)) = rx.try_recv() else { panic!("the agent's heartbeat is recorded") };
        assert!(matches!(e.kind, Kind::AiHeartbeat { sent: false, at, .. } if at == 1_700_000_001.0));
        assert!(rx.try_recv().is_err(), "an editor's heartbeat never comes from the shadow");
        assert!(queued.lock().unwrap().is_empty(), "nothing is passed on");
        assert!(!ctx.shadow.nudged(), "and it doesn't set itself off again");
        let browser = Req { origin: Some(EXT), ..plugin("POST", bulk, Some(&shadow)) };
        assert_eq!(handle(&ctx, &browser, beats).status, 403);
    }

    #[test]
    fn wakatime_calls_without_a_key_on_file_are_refused() {
        let (tx, _rx) = mpsc::channel();
        let ctx = Ctx::with_lookups(
            PORT,
            status::shared(),
            Arc::new(BrowserState::default()),
            no_exclusions(),
            tx,
            Box::new(|_| None),
            Box::new(|| None),
            Box::new(|_| {}),
            waka(),
        );
        let basic = format!("Basic {}", BASE64.encode(KEY));
        assert_eq!(handle(&ctx, &plugin("GET", "/wakatime/v1/users/current/statusbar/today", Some(&basic)), b"").status, 401);
    }

    #[test]
    fn heartbeat_bodies_may_be_larger_than_other_calls() {
        let (ctx, _rx) = ctx();
        let basic = format!("Basic {}", BASE64.encode(KEY));
        let big = Req { length: Some(MAX_BODY * 4), ..plugin("POST", "/wakatime/v1/users/current/heartbeats.bulk", Some(&basic)) };
        assert!(check(&ctx, &big).is_ok());
        let too_big = Req { length: Some(MAX_HEARTBEAT_BODY + 1), ..big };
        assert_eq!(check(&ctx, &too_big).err().map(|r| r.status), Some(413));
        let tab = Req { length: Some(MAX_BODY + 1), ..post("/v1/browser/tab", Some(EXT), None) };
        assert_eq!(check(&ctx, &tab).err().map(|r| r.status), Some(413));
    }
}
