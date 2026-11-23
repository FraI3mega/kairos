use std::time::Instant;

use axum::extract::{MatchedPath, Request, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::util::{b64url, err, random_bytes};
use crate::St;

#[derive(Clone)]
pub struct Nonce(pub String);

const PERMISSIONS: &str = "accelerometer=(), camera=(), geolocation=(), gyroscope=(), magnetometer=(), microphone=(), payment=(), usb=(), interest-cohort=()";

// Inline styles stay allowed (the landing page and the bars use them): they can't run script and askama escapes every value.
fn csp(nonce: &str) -> String {
    format!(
        "default-src 'self'; script-src 'self' 'nonce-{nonce}'; style-src 'self'; style-src-attr 'unsafe-inline'; \
         img-src 'self' data:; font-src 'self'; connect-src 'self'; object-src 'none'; base-uri 'none'; \
         form-action 'self'; frame-ancestors 'none'"
    )
}

pub async fn headers(mut req: Request, next: Next) -> Response {
    let nonce = match random_bytes::<16>() {
        Ok(b) => b64url(&b),
        Err(e) => return crate::util::AppError::from(e).into_response(),
    };
    req.extensions_mut().insert(Nonce(nonce.clone()));
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&csp(&nonce)) {
        h.insert(header::CONTENT_SECURITY_POLICY, v);
    }
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("same-origin"));
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert("permissions-policy", HeaderValue::from_static(PERMISSIONS));
    h.insert("cross-origin-opener-policy", HeaderValue::from_static("same-origin"));
    res
}

/// Only the route pattern is logged: paths and queries can carry codes or api keys.
pub async fn log(req: Request, next: Next) -> Response {
    let start = Instant::now();
    let method = req.method().clone();
    let route = req.extensions().get::<MatchedPath>().map_or("-", |p| p.as_str()).to_string();
    let res = next.run(req).await;
    let status = res.status().as_u16();
    let ms = start.elapsed().as_secs_f64() * 1000.0;
    if status >= 500 {
        tracing::warn!(method = %method, route = %route, status, ms = %format!("{ms:.1}"), "request");
    } else {
        tracing::info!(method = %method, route = %route, status, ms = %format!("{ms:.1}"), "request");
    }
    res
}

pub async fn same_origin(State(st): State<St>, req: Request, next: Next) -> Response {
    if !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS) {
        let origin = req.headers().get(header::ORIGIN).and_then(|v| v.to_str().ok());
        if origin != Some(st.config.public_url.as_str()) {
            return err(StatusCode::FORBIDDEN, "cross-origin form").into_response();
        }
    }
    next.run(req).await
}
