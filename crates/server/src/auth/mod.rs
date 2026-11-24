pub mod mock;
mod mock_key;
pub mod oidc;

use axum::extract::{FromRequestParts, Query, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::Form;
use serde::Deserialize;

use crate::accounts::{self, Identity};
use crate::config::Config;
use crate::util::{b64url, cookie, err, hash, random_bytes, random_token, urlenc, AppError, Res};
use crate::St;

pub const SESSION_DAYS: i64 = 30;
const FLOW_SECS: i64 = 600;
const REPEAT_SECS: i64 = 60;
pub const SCOPES: &str = "openid profile email slack_id verification_status";

#[derive(Clone, Debug)]
pub struct User {
    pub id: i64,
    pub username: String,
    pub display_name: Option<String>,
    pub role: String,
    pub is_public: bool,
    pub show_ai_tools: bool,
    pub timezone: String,
}

pub const USER_COLUMNS: &str = "u.id, u.username, u.display_name, u.role, u.is_public, u.show_ai_tools, u.timezone";

impl User {
    pub fn from_row(r: &tokio_postgres::Row) -> User {
        User {
            id: r.get("id"),
            username: r.get("username"),
            display_name: r.get("display_name"),
            role: r.get("role"),
            is_public: r.get("is_public"),
            show_ai_tools: r.get("show_ai_tools"),
            timezone: r.get("timezone"),
        }
    }

    pub fn tz(&self) -> chrono_tz::Tz {
        self.timezone.parse().unwrap_or(chrono_tz::UTC)
    }

    pub fn is_admin(&self) -> bool {
        self.role == "admin"
    }

    pub fn is_staff(&self) -> bool {
        self.role == "reviewer" || self.role == "admin"
    }
}

#[derive(Clone)]
pub struct Session {
    pub token: String,
    pub user: User,
}

impl Session {
    pub fn csrf(&self) -> String {
        csrf_for(&self.token)
    }

    pub fn check_csrf(&self, token: &str) -> Res<()> {
        let expected = blake3::derive_key("kairos 2026-11 csrf", self.token.as_bytes());
        match blake3::Hash::from_hex(token.trim()) {
            Ok(h) if h == blake3::Hash::from(expected) => Ok(()),
            _ => Err(err(StatusCode::FORBIDDEN, "This form expired. Reload the page and try again.")),
        }
    }
}

fn csrf_for(token: &str) -> String {
    blake3::Hash::from(blake3::derive_key("kairos 2026-11 csrf", token.as_bytes())).to_hex().to_string()
}

#[derive(Clone)]
pub struct Current(pub Option<Session>);

pub fn session_cookie(cfg: &Config) -> &'static str {
    if cfg.https() {
        "__Host-kairos_session"
    } else {
        "kairos_session"
    }
}

fn flow_cookie(cfg: &Config) -> &'static str {
    if cfg.https() {
        "__Host-kairos_auth"
    } else {
        "kairos_auth"
    }
}

pub fn set_cookie(cfg: &Config, name: &str, value: &str, max_age: i64) -> String {
    let secure = if cfg.https() { "; Secure" } else { "" };
    format!("{name}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}")
}

fn append_cookie(res: &mut Response, value: String) -> Res<()> {
    res.headers_mut().append(header::SET_COOKIE, HeaderValue::from_str(&value).map_err(anyhow::Error::from)?);
    Ok(())
}

/// Slides the expiry at most once an hour; the bool says the cookie should be re-sent with a fresh Max-Age.
pub async fn load(st: &St, h: &HeaderMap) -> Res<Option<(Session, bool)>> {
    let Some(token) = cookie(h, session_cookie(&st.config)) else { return Ok(None) };
    if token.len() > 128 {
        return Ok(None);
    }
    let c = st.pool.get().await?;
    let row = c
        .query_opt(
            &format!(
                "SELECT s.last_seen_at < now() - interval '1 hour' AS stale, {USER_COLUMNS}
                 FROM web_sessions s JOIN users u ON u.id = s.user_id
                 WHERE s.token_hash = $1 AND s.expires_at > now()"
            ),
            &[&hash(&token)],
        )
        .await?;
    let Some(row) = row else { return Ok(None) };
    let stale: bool = row.get("stale");
    if stale {
        c.execute(
            "UPDATE web_sessions SET last_seen_at = now(), expires_at = now() + make_interval(days => $2) WHERE token_hash = $1",
            &[&hash(&token), &(SESSION_DAYS as i32)],
        )
        .await?;
    }
    Ok(Some((Session { token, user: User::from_row(&row) }, stale)))
}

pub struct SignedIn(pub Session);
pub struct MaybeSignedIn(pub Option<Session>);

impl FromRequestParts<St> for MaybeSignedIn {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, _: &St) -> Result<Self, Self::Rejection> {
        Ok(MaybeSignedIn(parts.extensions.get::<Current>().and_then(|c| c.0.clone())))
    }
}

impl FromRequestParts<St> for SignedIn {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, _: &St) -> Result<Self, Self::Rejection> {
        if let Some(s) = parts.extensions.get::<Current>().and_then(|c| c.0.clone()) {
            return Ok(SignedIn(s));
        }
        if parts.method == axum::http::Method::GET {
            let next = parts.uri.path_and_query().map_or("/me", |p| p.as_str());
            return Err(Redirect::to(&format!("/auth/login?next={}", urlenc(next))).into_response());
        }
        Err(err(StatusCode::UNAUTHORIZED, "sign in first").into_response())
    }
}

pub fn safe_next(next: Option<&str>) -> String {
    match next {
        Some(n) if n.starts_with('/') && !n.starts_with("//") && !n.starts_with("/\\") && n.len() <= 512 => n.to_string(),
        _ => "/me".to_string(),
    }
}

#[derive(Deserialize)]
pub struct LoginQ {
    next: Option<String>,
}

pub async fn login(State(st): State<St>, MaybeSignedIn(s): MaybeSignedIn, Query(q): Query<LoginQ>) -> Res<Response> {
    let next = safe_next(q.next.as_deref());
    if s.is_some() {
        return Ok(Redirect::to(&next).into_response());
    }
    let d = st.oidc.discovery(&st).await.map_err(|e| {
        tracing::error!(error = format!("{e:#}"), "oidc discovery failed");
        err(StatusCode::BAD_GATEWAY, "Hack Club Auth is not reachable right now, try again in a minute")
    })?;
    let state = b64url(&random_bytes::<32>()?);
    let nonce = b64url(&random_bytes::<32>()?);
    let verifier = b64url(&random_bytes::<32>()?);
    let c = st.pool.get().await?;
    c.execute(
        "INSERT INTO auth_flows (state_hash, nonce, verifier, next, expires_at) VALUES ($1, $2, $3, $4, now() + make_interval(secs => $5))",
        &[&hash(&state), &nonce, &verifier, &next, &(FLOW_SECS as f64)],
    )
    .await?;
    let url = oidc::authorize_url(&d, &st.config, &state, &nonce, &oidc::challenge(&verifier))?;
    let mut res = Redirect::to(&url).into_response();
    append_cookie(&mut res, set_cookie(&st.config, flow_cookie(&st.config), &state, FLOW_SECS))?;
    Ok(res)
}

#[derive(Deserialize)]
pub struct CallbackQ {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

pub async fn callback(State(st): State<St>, h: HeaderMap, Query(q): Query<CallbackQ>) -> Res<Response> {
    let expired = || err(StatusCode::BAD_REQUEST, "this sign-in link expired, start again from the sign-in button");
    let saved = cookie(&h, flow_cookie(&st.config)).ok_or_else(expired)?;
    if q.state.as_deref() != Some(saved.as_str()) {
        return Err(err(StatusCode::BAD_REQUEST, "sign-in state did not match, start again from the sign-in button"));
    }
    let flow = st
        .pool
        .get()
        .await?
        .query_opt(
            "DELETE FROM auth_flows WHERE state_hash = $1 AND expires_at > now() RETURNING nonce, verifier, next, user_id",
            &[&hash(&saved)],
        )
        .await?
        .ok_or_else(expired)?;
    let (nonce, verifier, next): (String, String, String) = (flow.get(0), flow.get(1), flow.get(2));
    if q.error.is_some() {
        return Err(err(StatusCode::BAD_REQUEST, "sign-in was cancelled"));
    }
    if let Some(user_id) = flow.get::<_, Option<i64>>(3) {
        return sign_in(&st, &h, user_id, &next).await;
    }
    let code = q.code.filter(|c| !c.is_empty() && c.len() <= 512).ok_or_else(|| err(StatusCode::BAD_REQUEST, "missing code"))?;
    let failed = |e: anyhow::Error| {
        tracing::warn!(error = format!("{e:#}"), "oidc sign-in failed");
        err(StatusCode::BAD_GATEWAY, "Hack Club Auth sign-in failed, try again")
    };
    let tokens = oidc::exchange(&st, &code, &verifier).await.map_err(failed)?;
    let id = oidc::verify_id_token(&st, &tokens.id_token, &nonce).await.map_err(failed)?;
    let info = oidc::userinfo(&st, &tokens.access_token).await.map_err(failed)?;
    if info.sub != id.sub {
        return Err(failed(anyhow::anyhow!("userinfo sub does not match the id token")));
    }
    let user_id = accounts::upsert(&st, &Identity::from(info)).await?;
    // The browser may ask for this callback again and drop this answer, session cookie included. It still holds the
    // flow cookie then, so the same request once more within REPEAT_SECS signs the same person in.
    st.pool
        .get()
        .await?
        .execute(
            "INSERT INTO auth_flows (state_hash, nonce, verifier, next, expires_at, user_id)
             VALUES ($1, $2, $3, $4, now() + make_interval(secs => $5), $6)",
            &[&hash(&saved), &nonce, &verifier, &next, &(REPEAT_SECS as f64), &user_id],
        )
        .await?;
    sign_in(&st, &h, user_id, &next).await
}

async fn sign_in(st: &St, h: &HeaderMap, user_id: i64, next: &str) -> Res<Response> {
    let token = random_token("ks_")?;
    let c = st.pool.get().await?;
    if let Some(old) = cookie(h, session_cookie(&st.config)) {
        c.execute("DELETE FROM web_sessions WHERE token_hash = $1", &[&hash(&old)]).await?;
    }
    c.execute(
        "INSERT INTO web_sessions (token_hash, user_id, expires_at) VALUES ($1, $2, now() + make_interval(days => $3))",
        &[&hash(&token), &user_id, &(SESSION_DAYS as i32)],
    )
    .await?;
    c.execute("UPDATE users SET last_login_at = now() WHERE id = $1", &[&user_id]).await?;
    let mut res = Redirect::to(next).into_response();
    append_cookie(&mut res, set_cookie(&st.config, session_cookie(&st.config), &token, SESSION_DAYS * 86400))?;
    append_cookie(&mut res, set_cookie(&st.config, flow_cookie(&st.config), "", 0))?;
    Ok(res)
}

#[derive(Deserialize)]
pub struct CsrfForm {
    csrf: String,
}

pub fn clear_session(st: &St, res: &mut Response) -> Res<()> {
    append_cookie(res, set_cookie(&st.config, session_cookie(&st.config), "", 0))
}

pub async fn logout(State(st): State<St>, SignedIn(s): SignedIn, Form(f): Form<CsrfForm>) -> Res<Response> {
    s.check_csrf(&f.csrf)?;
    let c = st.pool.get().await?;
    c.execute("DELETE FROM web_sessions WHERE token_hash = $1", &[&hash(&s.token)]).await?;
    let mut res = Redirect::to("/").into_response();
    clear_session(&st, &mut res)?;
    Ok(res)
}

pub async fn logout_all(State(st): State<St>, SignedIn(s): SignedIn, Form(f): Form<CsrfForm>) -> Res<Response> {
    s.check_csrf(&f.csrf)?;
    let c = st.pool.get().await?;
    c.execute("DELETE FROM web_sessions WHERE user_id = $1", &[&s.user.id]).await?;
    let mut res = Redirect::to("/").into_response();
    clear_session(&st, &mut res)?;
    Ok(res)
}
