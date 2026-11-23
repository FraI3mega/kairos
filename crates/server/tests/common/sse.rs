use std::time::Duration;

use axum::body::Body;
use axum::http::Response;
use http_body_util::BodyExt;
use serde_json::Value;

/// Skips comment-only (keepalive) events.
pub struct Events {
    body: Body,
    buf: String,
}

impl Events {
    pub fn new(res: Response<Body>) -> Events {
        Events { body: res.into_body(), buf: String::new() }
    }

    pub async fn next(&mut self, wait: Duration) -> Option<(String, String)> {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            while let Some(at) = self.buf.find("\n\n") {
                let raw: String = self.buf.drain(..at + 2).collect();
                let field = |k: &str| raw.lines().find_map(|l| l.strip_prefix(k).map(|v| v.trim_start().to_string()));
                if let Some(data) = field("data:") {
                    return Some((field("event:").unwrap_or_default(), data));
                }
            }
            let frame = tokio::time::timeout_at(deadline, self.body.frame()).await.ok()??.ok()?;
            if let Some(d) = frame.data_ref() {
                self.buf.push_str(&String::from_utf8_lossy(d));
            }
        }
    }

    pub async fn json(&mut self, wait: Duration) -> Option<Value> {
        self.next(wait).await.map(|(_, d)| serde_json::from_str(&d).expect("event data is json"))
    }
}
