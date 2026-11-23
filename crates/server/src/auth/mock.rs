use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use askama::Template;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Form, Json};
use base64::Engine;
use ring::rand::SystemRandom;
use ring::signature::{RsaKeyPair, RSA_PKCS1_SHA256};
use serde::Deserialize;
use serde_json::{json, Value};

use super::mock_key;
use super::oidc;
use crate::util::{b64url, bearer, err, hash, now, random_token, Res};
use crate::web::{render, Base, Ctx};
use crate::St;

const CODE_TTL: Duration = Duration::from_secs(120);
const TOKEN_TTL: Duration = Duration::from_secs(3600);

struct Grant {
    at: Instant,
    redirect_uri: String,
    challenge: String,
    nonce: Option<String>,
    claims: Value,
}

pub struct Provider {
    key: RsaKeyPair,
    rng: SystemRandom,
    codes: Mutex<HashMap<String, Grant>>,
    tokens: Mutex<HashMap<String, (Instant, Value)>>,
}

impl Provider {
    pub fn new() -> Result<Provider> {
        let der = base64::engine::general_purpose::STANDARD.decode(mock_key::DER_B64)?;
        let key = RsaKeyPair::from_der(&der).map_err(|e| anyhow!("mock key: {e}"))?;
        Ok(Provider { key, rng: SystemRandom::new(), codes: Mutex::default(), tokens: Mutex::default() })
    }

    pub fn cleanup(&self) {
        self.codes.lock().unwrap_or_else(|p| p.into_inner()).retain(|_, g| g.at.elapsed() < CODE_TTL);
        self.tokens.lock().unwrap_or_else(|p| p.into_inner()).retain(|_, t| t.0.elapsed() < TOKEN_TTL);
    }

    fn sign(&self, claims: &Value) -> Result<String> {
        let header = json!({ "alg": "RS256", "typ": "JWT", "kid": mock_key::KID });
        let input = format!("{}.{}", b64url(header.to_string().as_bytes()), b64url(claims.to_string().as_bytes()));
        let mut sig = vec![0u8; self.key.public().modulus_len()];
        self.key.sign(&RSA_PKCS1_SHA256, &self.rng, input.as_bytes(), &mut sig).map_err(|_| anyhow!("signing failed"))?;
        Ok(format!("{input}.{}", b64url(&sig)))
    }
}

fn provider(st: &St) -> Res<&Provider> {
    st.mock.as_ref().ok_or_else(|| err(StatusCode::NOT_FOUND, "not found"))
}

pub async fn discovery(State(st): State<St>) -> Json<Value> {
    let i = &st.config.issuer;
    Json(json!({
        "issuer": i,
        "authorization_endpoint": format!("{i}/oauth/authorize"),
        "token_endpoint": format!("{i}/oauth/token"),
        "userinfo_endpoint": format!("{i}/oauth/userinfo"),
        "jwks_uri": format!("{i}/oauth/discovery/keys"),
        "scopes_supported": ["openid", "profile", "email", "slack_id", "verification_status"],
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code"],
        "token_endpoint_auth_methods_supported": ["client_secret_basic", "client_secret_post"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["RS256"],
        "code_challenge_methods_supported": ["S256"],
        "claims_supported": ["iss", "sub", "aud", "exp", "iat", "email", "email_verified", "name", "given_name",
            "family_name", "nickname", "slack_id", "verification_status", "ysws_eligible"],
    }))
}

pub async fn jwks() -> Json<Value> {
    Json(json!({ "keys": [{ "kty": "RSA", "n": mock_key::N, "e": mock_key::E, "kid": mock_key::KID, "use": "sig", "alg": "RS256" }] }))
}

#[derive(Deserialize, Clone)]
pub struct AuthorizeQ {
    #[serde(default)]
    client_id: String,
    #[serde(default)]
    redirect_uri: String,
    #[serde(default)]
    response_type: String,
    #[serde(default)]
    scope: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    nonce: String,
    #[serde(default)]
    code_challenge: String,
    #[serde(default)]
    code_challenge_method: String,
}

impl AuthorizeQ {
    fn check(&self, st: &St) -> Res<()> {
        let bad = |m: &str| Err(err(StatusCode::BAD_REQUEST, m));
        if self.client_id != st.config.client_id {
            return bad("unknown client_id");
        }
        if self.redirect_uri != oidc::redirect_uri(&st.config) {
            return bad("redirect_uri is not registered for this client");
        }
        if self.response_type != "code" {
            return bad("response_type must be code");
        }
        if self.code_challenge_method != "S256" || self.code_challenge.len() < 43 {
            return bad("PKCE with S256 is required");
        }
        if !self.scope.split(' ').any(|s| s == "openid") {
            return bad("scope must include openid");
        }
        Ok(())
    }

    fn hidden(&self) -> Vec<(&'static str, String)> {
        vec![
            ("client_id", self.client_id.clone()),
            ("redirect_uri", self.redirect_uri.clone()),
            ("response_type", self.response_type.clone()),
            ("scope", self.scope.clone()),
            ("state", self.state.clone()),
            ("nonce", self.nonce.clone()),
            ("code_challenge", self.code_challenge.clone()),
            ("code_challenge_method", self.code_challenge_method.clone()),
        ]
    }
}

pub struct Preset {
    pub label: &'static str,
    pub name: &'static str,
    pub email: &'static str,
    pub slack_id: &'static str,
    pub status: &'static str,
    pub eligible: bool,
}

const PRESETS: &[Preset] = &[
    Preset { label: "Ada, verified", name: "Ada Lovelace", email: "ada@example.com", slack_id: "U0MOCKADA", status: "verified", eligible: true },
    Preset { label: "Bea, not verified yet", name: "Bea Builder", email: "bea@example.com", slack_id: "U0MOCKBEA", status: "needs_submission", eligible: false },
    Preset { label: "Cy, over 18", name: "Cy Senior", email: "cy@example.com", slack_id: "", status: "verified", eligible: false },
];

#[derive(Template)]
#[template(path = "mock_authorize.html")]
struct AuthorizePage {
    base: Base,
    hidden: Vec<(&'static str, String)>,
    presets: &'static [Preset],
}

pub async fn authorize(State(st): State<St>, ctx: Ctx, Query(q): Query<AuthorizeQ>) -> Res<Response> {
    provider(&st)?;
    q.check(&st)?;
    render(&AuthorizePage { base: Base::new(&st, &ctx, "Mock Hack Club Auth"), hidden: q.hidden(), presets: PRESETS })
}

#[derive(Deserialize)]
pub struct ApproveForm {
    #[serde(flatten)]
    q: AuthorizeQ,
    #[serde(default)]
    action: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    slack_id: String,
    #[serde(default)]
    verification_status: String,
    #[serde(default)]
    ysws_eligible: Option<String>,
}

fn redirect_with(uri: &str, params: &[(&str, &str)]) -> Res<Response> {
    let url = reqwest::Url::parse_with_params(uri, params).map_err(anyhow::Error::from)?;
    Ok(Redirect::to(url.as_str()).into_response())
}

pub async fn approve(State(st): State<St>, Form(f): Form<ApproveForm>) -> Res<Response> {
    let p = provider(&st)?;
    f.q.check(&st)?;
    if f.action == "deny" {
        return redirect_with(&f.q.redirect_uri, &[("error", "access_denied"), ("state", &f.q.state)]);
    }
    let name = crate::util::clean(&f.name, 80);
    let email = crate::util::clean(&f.email, 120).to_lowercase();
    if name.is_empty() && email.is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "enter a name or an email"));
    }
    let status = match f.verification_status.as_str() {
        s @ ("needs_submission" | "pending" | "verified" | "ineligible") => s,
        _ => "needs_submission",
    };
    let seed = if email.is_empty() { name.to_lowercase() } else { email.clone() };
    let sub = format!("ident!mock{}", &hash(&seed)[..10]);
    let (given, family) = name.split_once(' ').unwrap_or((name.as_str(), ""));
    let scopes: Vec<&str> = f.q.scope.split(' ').collect();
    let mut claims = json!({ "sub": sub });
    if scopes.contains(&"profile") {
        claims["name"] = json!(name);
        claims["given_name"] = json!(given);
        claims["family_name"] = json!(family);
        claims["nickname"] = json!(given);
    }
    if scopes.contains(&"email") && !email.is_empty() {
        claims["email"] = json!(email);
        claims["email_verified"] = json!(true);
    }
    if scopes.contains(&"slack_id") {
        let slack = crate::util::clean(&f.slack_id, 32);
        claims["slack_id"] = if slack.is_empty() { Value::Null } else { json!(slack) };
    }
    if scopes.contains(&"verification_status") {
        claims["verification_status"] = json!(status);
        claims["ysws_eligible"] = json!(f.ysws_eligible.is_some());
    }
    let code = random_token("mc_")?;
    p.codes.lock().unwrap_or_else(|e| e.into_inner()).insert(
        hash(&code),
        Grant {
            at: Instant::now(),
            redirect_uri: f.q.redirect_uri.clone(),
            challenge: f.q.code_challenge.clone(),
            nonce: Some(f.q.nonce.clone()).filter(|n| !n.is_empty()),
            claims,
        },
    );
    redirect_with(&f.q.redirect_uri, &[("code", &code), ("state", &f.q.state)])
}

#[derive(Deserialize)]
pub struct TokenForm {
    #[serde(default)]
    grant_type: String,
    #[serde(default)]
    code: String,
    #[serde(default)]
    redirect_uri: String,
    client_id: Option<String>,
    client_secret: Option<String>,
    #[serde(default)]
    code_verifier: String,
}

fn oauth_error(status: StatusCode, error: &str) -> Response {
    (status, Json(json!({ "error": error }))).into_response()
}

fn basic_client(h: &HeaderMap) -> Option<(String, String)> {
    let v = h.get(axum::http::header::AUTHORIZATION)?.to_str().ok()?.strip_prefix("Basic ")?;
    let raw = base64::engine::general_purpose::STANDARD.decode(v.trim()).ok()?;
    let (id, secret) = String::from_utf8(raw).ok()?.split_once(':').map(|(a, b)| (a.to_string(), b.to_string()))?;
    Some((id, secret))
}

pub async fn token(State(st): State<St>, h: HeaderMap, Form(f): Form<TokenForm>) -> Res<Response> {
    let p = provider(&st)?;
    let (id, secret) = match (basic_client(&h), f.client_id, f.client_secret) {
        (Some(c), _, _) => c,
        (None, Some(id), Some(secret)) => (id, secret),
        _ => return Ok(oauth_error(StatusCode::UNAUTHORIZED, "invalid_client")),
    };
    if id != st.config.client_id || blake3::hash(secret.as_bytes()) != blake3::hash(st.config.client_secret.as_bytes()) {
        return Ok(oauth_error(StatusCode::UNAUTHORIZED, "invalid_client"));
    }
    if f.grant_type != "authorization_code" {
        return Ok(oauth_error(StatusCode::BAD_REQUEST, "unsupported_grant_type"));
    }
    let grant = p.codes.lock().unwrap_or_else(|e| e.into_inner()).remove(&hash(&f.code));
    let Some(grant) = grant.filter(|g| g.at.elapsed() < CODE_TTL) else {
        return Ok(oauth_error(StatusCode::BAD_REQUEST, "invalid_grant"));
    };
    if grant.redirect_uri != f.redirect_uri || oidc::challenge(&f.code_verifier) != grant.challenge {
        return Ok(oauth_error(StatusCode::BAD_REQUEST, "invalid_grant"));
    }
    let t = now().floor();
    let mut id_claims = grant.claims.clone();
    id_claims["iss"] = json!(st.config.issuer);
    id_claims["aud"] = json!(st.config.client_id);
    id_claims["iat"] = json!(t as i64);
    id_claims["exp"] = json!(t as i64 + 3600);
    if let Some(n) = &grant.nonce {
        id_claims["nonce"] = json!(n);
    }
    let id_token = p.sign(&id_claims)?;
    let access = random_token("idntk.mock")?;
    p.tokens.lock().unwrap_or_else(|e| e.into_inner()).insert(hash(&access), (Instant::now(), grant.claims));
    Ok(Json(json!({
        "access_token": access,
        "token_type": "Bearer",
        "expires_in": TOKEN_TTL.as_secs(),
        "id_token": id_token,
        "scope": "openid profile email slack_id verification_status",
    }))
    .into_response())
}

pub async fn userinfo(State(st): State<St>, h: HeaderMap) -> Res<Response> {
    let p = provider(&st)?;
    let claims = bearer(&h).and_then(|t| {
        let map = p.tokens.lock().unwrap_or_else(|e| e.into_inner());
        map.get(&hash(&t)).filter(|(at, _)| at.elapsed() < TOKEN_TTL).map(|(_, c)| c.clone())
    });
    Ok(match claims {
        Some(c) => Json(c).into_response(),
        None => oauth_error(StatusCode::UNAUTHORIZED, "invalid_token"),
    })
}
