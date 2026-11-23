//! Where a browser sign-in comes back to: a listener on 127.0.0.1 that lives only while the sign-in runs. A visit counts
//! only on the expected path, with the expected state and Host (so a page that rebinds a name to 127.0.0.1 gets nowhere),
//! and the browser waits on its answer until the caller says where to send it next.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use base64::Engine;

#[derive(Debug)]
pub struct TimedOut;

impl std::fmt::Display for TimedOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("nothing came back from the browser in time")
    }
}

impl std::error::Error for TimedOut {}

pub struct Loopback {
    listener: TcpListener,
    port: u16,
}

pub struct Visit {
    query: HashMap<String, String>,
    stream: TcpStream,
}

const MAX_REQUEST: usize = 16 * 1024;

pub fn random_state() -> Result<String> {
    let mut b = [0u8; 24];
    getrandom::fill(&mut b).map_err(|e| anyhow::anyhow!("os random: {e}"))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b))
}

impl Loopback {
    pub fn bind() -> Result<Loopback> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        Ok(Loopback { listener, port })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    /// The first visit to `path` carrying `state`. Anything else gets a short answer and the wait goes on. Each
    /// connection is read on its own thread: browsers open spare connections that send nothing, and one of those must not
    /// hold up the real request behind it.
    pub fn wait(&self, path: &str, state: &str, timeout: Duration) -> Result<Visit> {
        let deadline = Instant::now() + timeout;
        let (tx, rx) = mpsc::channel();
        loop {
            loop {
                match self.listener.accept() {
                    Ok((stream, _)) => {
                        let (tx, path, state, port) = (tx.clone(), path.to_string(), state.to_string(), self.port);
                        thread::spawn(move || {
                            if let Some(v) = visit(stream, port, &path, &state) {
                                let _ = tx.send(v);
                            }
                        });
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) => return Err(e.into()),
                }
            }
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(v) => return Ok(v),
                Err(_) if Instant::now() > deadline => return Err(TimedOut.into()),
                Err(_) => {}
            }
        }
    }
}

fn visit(mut stream: TcpStream, port: u16, path: &str, state: &str) -> Option<Visit> {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let Ok(head) = read_head(&mut stream) else { return None };
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or("").split(' ');
    let (method, target) = (first.next().unwrap_or(""), first.next().unwrap_or(""));
    let host = lines.filter_map(|l| l.split_once(':')).find(|(k, _)| k.trim().eq_ignore_ascii_case("host")).map(|(_, v)| v.trim());
    let Ok(url) = reqwest::Url::parse(&format!("http://127.0.0.1{target}")) else {
        answer(&mut stream, "400 Bad Request", "");
        return None;
    };
    if method != "GET" || url.path() != path || host != Some(format!("127.0.0.1:{port}").as_str()) {
        answer(&mut stream, "404 Not Found", "");
        return None;
    }
    let query: HashMap<String, String> = url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
    if query.get("state").map(String::as_str) != Some(state) {
        answer(&mut stream, "400 Bad Request", &page("This link is from another sign-in", "Go back to your terminal, it has the one to use."));
        return None;
    }
    Some(Visit { query, stream })
}

impl Visit {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.query.get(key).map(String::as_str)
    }

    pub fn redirect(mut self, to: &str) {
        let head = format!(
            "HTTP/1.1 303 See Other\r\nLocation: {to}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let _ = self.stream.write_all(head.as_bytes());
    }

    pub fn show(mut self, heading: &str, text: &str) {
        answer(&mut self.stream, "200 OK", &page(heading, text));
    }
}

fn read_head(stream: &mut TcpStream) -> Result<String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            return Ok(String::from_utf8_lossy(&buf[..i]).into_owned());
        }
        if buf.len() > MAX_REQUEST {
            bail!("request too large");
        }
        match stream.read(&mut chunk)? {
            0 => bail!("closed early"),
            n => buf.extend_from_slice(&chunk[..n]),
        }
    }
}

fn answer(stream: &mut TcpStream, status: &str, body: &str) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nCache-Control: no-store\r\nContent-Security-Policy: default-src 'none'; style-src 'unsafe-inline'\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
}

fn page(heading: &str, text: &str) -> String {
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;");
    format!(
        "<!doctype html><meta charset=utf-8><meta name=viewport content=\"width=device-width\"><title>Kairos</title>\
         <body style=\"font:16px/1.5 system-ui,sans-serif;max-width:32rem;margin:15vh auto;padding:0 1rem;background:#16122b;color:#ece9ff\">\
         <h1 style=\"font-size:1.4rem\">{}</h1><p>{}</p></body>",
        esc(heading),
        esc(text)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn send(port: u16, head: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.write_all(head.as_bytes()).unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out
    }

    fn get(port: u16, target: &str) -> String {
        send(port, &format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"))
    }

    #[test]
    fn only_the_right_path_and_state_count() {
        let lb = Loopback::bind().unwrap();
        let port = lb.port();
        let browser = thread::spawn(move || {
            let favicon = get(port, "/favicon.ico");
            let other_state = get(port, "/callback?code=x&state=nope");
            let rebound = send(port, "GET /callback?code=x&state=good HTTP/1.1\r\nHost: evil.example:80\r\n\r\n");
            let post = send(port, &format!("POST /callback?code=x&state=good HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"));
            let ok = get(port, "/callback?code=kg_1%2B2&state=good");
            (favicon, other_state, rebound, post, ok)
        });
        let v = lb.wait("/callback", "good", Duration::from_secs(10)).unwrap();
        assert_eq!(v.get("code"), Some("kg_1+2"));
        v.redirect("https://kairos.example/me?connected=1");
        let (favicon, other_state, rebound, post, ok) = browser.join().unwrap();
        assert!(favicon.starts_with("HTTP/1.1 404"), "{favicon}");
        assert!(other_state.starts_with("HTTP/1.1 400") && other_state.contains("another sign-in"), "{other_state}");
        assert!(rebound.starts_with("HTTP/1.1 404"), "{rebound}");
        assert!(post.starts_with("HTTP/1.1 404"), "{post}");
        assert!(ok.starts_with("HTTP/1.1 303") && ok.contains("\r\nLocation: https://kairos.example/me?connected=1\r\n"), "{ok}");
    }

    #[test]
    fn a_spare_connection_that_sends_nothing_does_not_hold_up_the_real_one() {
        let lb = Loopback::bind().unwrap();
        let port = lb.port();
        let started = Instant::now();
        let browser = thread::spawn(move || {
            let _spare = TcpStream::connect(("127.0.0.1", port)).unwrap();
            let _another = TcpStream::connect(("127.0.0.1", port)).unwrap();
            thread::sleep(Duration::from_millis(100));
            get(port, "/callback?code=c&state=s")
        });
        let v = lb.wait("/callback", "s", Duration::from_secs(10)).unwrap();
        assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
        v.redirect("https://kairos.example/me");
        assert!(browser.join().unwrap().starts_with("HTTP/1.1 303"));
    }

    #[test]
    fn pages_are_escaped() {
        let lb = Loopback::bind().unwrap();
        let port = lb.port();
        let browser = thread::spawn(move || get(port, "/done?state=s"));
        lb.wait("/done", "s", Duration::from_secs(10)).unwrap().show("Fine", "<script>x</script> & more");
        let out = browser.join().unwrap();
        assert!(out.contains("&lt;script&gt;x&lt;/script&gt; &amp; more") && !out.contains("<script>"), "{out}");
    }

    #[test]
    fn waiting_ends() {
        let lb = Loopback::bind().unwrap();
        let err = lb.wait("/callback", "s", Duration::from_millis(200)).err().unwrap();
        assert!(err.downcast_ref::<TimedOut>().is_some());
    }

    #[test]
    fn states_are_random_and_url_safe() {
        let (a, b) = (random_state().unwrap(), random_state().unwrap());
        assert_ne!(a, b);
        assert_eq!(a.len(), 32);
        assert!(a.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'));
    }
}
