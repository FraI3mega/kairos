use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::Engine;
use serde_json::{json, Value};

#[derive(Debug)]
pub struct AppError {
    pub status: StatusCode,
    pub body: Value,
    pub retry_after: Option<u64>,
}

pub type Res<T> = Result<T, AppError>;

#[derive(Clone)]
pub struct ErrorMessage(pub String);

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let msg = self.body.get("error").and_then(Value::as_str).unwrap_or("error").to_string();
        let mut res = (self.status, Json(self.body)).into_response();
        if let Some(secs) = self.retry_after {
            res.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from(secs));
        }
        res.extensions_mut().insert(ErrorMessage(msg));
        res
    }
}

impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(e: E) -> AppError {
        let e: anyhow::Error = e.into();
        tracing::error!(error = format!("{e:#}"), "request failed");
        AppError { status: StatusCode::INTERNAL_SERVER_ERROR, body: json!({ "error": "internal" }), retry_after: None }
    }
}

pub fn err(code: StatusCode, msg: &str) -> AppError {
    AppError { status: code, body: json!({ "error": msg }), retry_after: None }
}

/// Its message is written for the person who asked.
#[derive(Debug)]
pub struct Refused(pub String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

pub fn refused_or_internal(e: anyhow::Error) -> AppError {
    match e.downcast_ref::<Refused>() {
        Some(r) => err(StatusCode::BAD_REQUEST, &r.0),
        None => AppError::from(e),
    }
}

pub fn random_bytes<const N: usize>() -> anyhow::Result<[u8; N]> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|e| anyhow::anyhow!("os random: {e}"))?;
    Ok(b)
}

pub fn b64url(b: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

pub fn random_token(prefix: &str) -> anyhow::Result<String> {
    Ok(format!("{prefix}{}", b64url(&random_bytes::<32>()?)))
}

/// wakatime-cli refuses api keys that are not UUIDs.
pub fn random_uuid() -> anyhow::Result<String> {
    let mut b = random_bytes::<16>()?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    Ok(format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..]))
}

pub fn user_code() -> anyhow::Result<String> {
    const ALPHA: &[u8] = b"BCDFGHJKLMNPQRSTVWXZ23456789";
    let b = random_bytes::<8>()?;
    let s: String = b.iter().map(|x| ALPHA[(*x as usize * ALPHA.len()) >> 8] as char).collect();
    Ok(format!("{}-{}", &s[..4], &s[4..]))
}

pub fn hash(s: &str) -> String {
    blake3::hash(s.as_bytes()).to_hex().to_string()
}

pub fn bearer(h: &HeaderMap) -> Option<String> {
    let v = h.get(header::AUTHORIZATION)?.to_str().ok()?;
    if let Some(t) = v.strip_prefix("Bearer ") {
        return Some(t.trim().to_string());
    }
    let b = v.strip_prefix("Basic ")?;
    let raw = base64::engine::general_purpose::STANDARD.decode(b.trim()).ok()?;
    let s = String::from_utf8(raw).ok()?;
    Some(s.split(':').next().unwrap_or(&s).to_string())
}

pub fn cookie(h: &HeaderMap, name: &str) -> Option<String> {
    h.get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_string())
}

pub fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

pub fn human(secs: f64) -> String {
    let m = (secs / 60.0).round() as i64;
    match (m / 60, m % 60) {
        (0, 0) if secs > 0.0 => "under 1 min".to_string(),
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} hr"),
        (h, m) => format!("{h} hr {m} min"),
    }
}

pub fn short(secs: f64) -> String {
    let m = (secs / 60.0).round() as i64;
    match m / 60 {
        0 => format!("{m}m"),
        h if m % 60 == 0 => format!("{h}h"),
        h => format!("{h}h {:02}m", m % 60),
    }
}

pub fn urlenc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub fn clean(s: &str, max: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(max).collect::<String>().trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_shape() {
        let u = random_uuid().unwrap();
        assert_eq!(u.len(), 36);
        assert_eq!(&u[14..15], "4");
        assert!(matches!(&u[19..20], "8" | "9" | "a" | "b"));
    }

    #[test]
    fn human_durations_read_naturally() {
        assert_eq!(human(0.0), "0 min");
        assert_eq!(human(14.0), "under 1 min");
        assert_eq!(human(60.0), "1 min");
        assert_eq!(human(3600.0), "1 hr");
        assert_eq!(human(4920.0), "1 hr 22 min");
        assert_eq!(short(0.0), "0m");
        assert_eq!(short(1560.0), "26m");
        assert_eq!(short(12540.0), "3h 29m");
        assert_eq!(short(3840.0), "1h 04m");
        assert_eq!(short(3600.0), "1h");
        assert_eq!(short(21900.0), "6h 05m");
    }

    #[test]
    fn user_codes() {
        let c = user_code().unwrap();
        assert_eq!(c.len(), 9);
        assert!(c.chars().all(|c| c == '-' || c.is_ascii_uppercase() || c.is_ascii_digit()));
    }
}
