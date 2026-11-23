//! Claims are read from userinfo, whose `sub` must match the verified ID token's.
//! HCA ignores PKCE today (its grants table has no code_challenge column, so Doorkeeper skips
//! the check), but sending it costs nothing and the mock enforces it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use ring::signature::{RsaPublicKeyComponents, RSA_PKCS1_2048_8192_SHA256};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::RwLock;

use crate::config::Config;
use crate::util::{b64url, now};
use crate::St;

const DISCOVERY_TTL: Duration = Duration::from_secs(3600);
const KEYS_TTL: Duration = Duration::from_secs(3600);
const KEYS_REFRESH_MIN: Duration = Duration::from_secs(60);
const LEEWAY: f64 = 60.0;

#[derive(Debug, Clone, Deserialize)]
pub struct Discovery {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub userinfo_endpoint: String,
    pub jwks_uri: String,
    #[serde(default)]
    pub id_token_signing_alg_values_supported: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Jwk {
    pub kty: String,
    #[serde(default)]
    pub kid: Option<String>,
    #[serde(default)]
    pub alg: Option<String>,
    #[serde(default, rename = "use")]
    pub use_: Option<String>,
    #[serde(default)]
    pub n: Option<String>,
    #[serde(default)]
    pub e: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Jwks {
    pub keys: Vec<Jwk>,
}

#[derive(Default)]
pub struct Cache {
    discovery: RwLock<Option<(Instant, Arc<Discovery>)>>,
    keys: RwLock<Option<(Instant, Arc<Jwks>)>>,
}

impl Cache {
    pub async fn discovery(&self, st: &St) -> Result<Arc<Discovery>> {
        if let Some((at, d)) = self.discovery.read().await.as_ref() {
            if at.elapsed() < DISCOVERY_TTL {
                return Ok(d.clone());
            }
        }
        let url = st.config.provider_url(&format!("{}/.well-known/openid-configuration", st.config.issuer));
        let d: Discovery = st.http.get(&url).send().await?.error_for_status()?.json().await.context("discovery document")?;
        if d.issuer.trim_end_matches('/') != st.config.issuer {
            bail!("discovery issuer {} does not match {}", d.issuer, st.config.issuer);
        }
        let d = Arc::new(d);
        *self.discovery.write().await = Some((Instant::now(), d.clone()));
        Ok(d)
    }

    async fn keys(&self, st: &St, d: &Discovery, refresh: bool) -> Result<Arc<Jwks>> {
        if let Some((at, k)) = self.keys.read().await.as_ref() {
            let fresh = if refresh { at.elapsed() < KEYS_REFRESH_MIN } else { at.elapsed() < KEYS_TTL };
            if fresh {
                return Ok(k.clone());
            }
        }
        let k: Jwks = st.http.get(st.config.provider_url(&d.jwks_uri)).send().await?.error_for_status()?.json().await.context("jwks")?;
        let k = Arc::new(k);
        *self.keys.write().await = Some((Instant::now(), k.clone()));
        Ok(k)
    }
}

pub fn challenge(verifier: &str) -> String {
    b64url(ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes()).as_ref())
}

pub fn redirect_uri(cfg: &Config) -> String {
    format!("{}/auth/callback", cfg.public_url)
}

pub fn authorize_url(d: &Discovery, cfg: &Config, state: &str, nonce: &str, challenge: &str) -> Result<String> {
    let url = reqwest::Url::parse_with_params(
        &d.authorization_endpoint,
        &[
            ("response_type", "code"),
            ("client_id", cfg.client_id.as_str()),
            ("redirect_uri", redirect_uri(cfg).as_str()),
            ("scope", super::SCOPES),
            ("state", state),
            ("nonce", nonce),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256"),
        ],
    )?;
    Ok(url.into())
}

pub struct Tokens {
    pub access_token: String,
    pub id_token: String,
}

pub async fn exchange(st: &St, code: &str, verifier: &str) -> Result<Tokens> {
    let d = st.oidc.discovery(st).await?;
    let redirect = redirect_uri(&st.config);
    let res = st
        .http
        .post(st.config.provider_url(&d.token_endpoint))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect.as_str()),
            ("client_id", st.config.client_id.as_str()),
            ("client_secret", st.config.client_secret.as_str()),
            ("code_verifier", verifier),
        ])
        .send()
        .await?;
    let status = res.status();
    let body: Value = res.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        bail!("token endpoint returned {status}: {}", body.get("error").and_then(Value::as_str).unwrap_or("?"));
    }
    let s = |k: &str| body.get(k).and_then(Value::as_str).map(str::to_string).ok_or_else(|| anyhow!("token response has no {k}"));
    Ok(Tokens { access_token: s("access_token")?, id_token: s("id_token")? })
}

#[derive(Debug, Deserialize)]
pub struct IdClaims {
    pub iss: String,
    pub sub: String,
    pub aud: Audience,
    pub exp: f64,
    #[serde(default)]
    pub iat: Option<f64>,
    #[serde(default)]
    pub nonce: Option<String>,
    #[serde(default)]
    pub azp: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum Audience {
    One(String),
    Many(Vec<String>),
}

#[derive(Deserialize)]
struct Header {
    alg: String,
    #[serde(default)]
    kid: Option<String>,
}

fn b64d(s: &str) -> Result<Vec<u8>> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s).context("bad base64url")
}

pub async fn verify_id_token(st: &St, token: &str, nonce: &str) -> Result<IdClaims> {
    let d = st.oidc.discovery(st).await?;
    let mut parts = token.split('.');
    let (Some(h), Some(p), Some(sig), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
        bail!("id token is not a JWS");
    };
    let header: Header = serde_json::from_slice(&b64d(h)?)?;
    if header.alg != "RS256" {
        bail!("id token alg {} is not RS256", header.alg);
    }
    if !d.id_token_signing_alg_values_supported.is_empty() && !d.id_token_signing_alg_values_supported.iter().any(|a| a == "RS256") {
        bail!("issuer does not sign with RS256");
    }
    let pick = |k: &Jwks| -> Option<Jwk> {
        let usable = |j: &&Jwk| j.kty == "RSA" && j.use_.as_deref().unwrap_or("sig") == "sig" && j.alg.as_deref().unwrap_or("RS256") == "RS256";
        let mut c = k.keys.iter().filter(usable);
        match &header.kid {
            Some(kid) => c.find(|j| j.kid.as_deref() == Some(kid.as_str())).cloned(),
            None => {
                let only = c.next().cloned();
                if c.next().is_some() { None } else { only }
            }
        }
    };
    let key = match pick(&*st.oidc.keys(st, &d, false).await?) {
        Some(k) => k,
        None => pick(&*st.oidc.keys(st, &d, true).await?).ok_or_else(|| anyhow!("no signing key for the id token"))?,
    };
    let (Some(n), Some(e)) = (key.n.as_deref(), key.e.as_deref()) else { bail!("RSA key without n/e") };
    RsaPublicKeyComponents { n: b64d(n)?, e: b64d(e)? }
        .verify(&RSA_PKCS1_2048_8192_SHA256, format!("{h}.{p}").as_bytes(), &b64d(sig)?)
        .map_err(|_| anyhow!("id token signature is invalid"))?;

    let claims: IdClaims = serde_json::from_slice(&b64d(p)?).context("id token claims")?;
    if claims.iss.trim_end_matches('/') != st.config.issuer {
        bail!("id token issuer {} is not {}", claims.iss, st.config.issuer);
    }
    let auds: Vec<&str> = match &claims.aud {
        Audience::One(a) => vec![a.as_str()],
        Audience::Many(a) => a.iter().map(String::as_str).collect(),
    };
    if !auds.contains(&st.config.client_id.as_str()) {
        bail!("id token is for another client");
    }
    if auds.len() > 1 && claims.azp.as_deref() != Some(st.config.client_id.as_str()) {
        bail!("id token azp is not this client");
    }
    let t = now();
    if claims.exp + LEEWAY < t {
        bail!("id token expired");
    }
    if claims.iat.is_some_and(|iat| iat > t + LEEWAY) {
        bail!("id token issued in the future");
    }
    if claims.nonce.as_deref() != Some(nonce) {
        bail!("id token nonce does not match");
    }
    Ok(claims)
}

#[derive(Debug, Clone, Deserialize)]
pub struct UserInfo {
    pub sub: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub nickname: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub email_verified: Option<bool>,
    #[serde(default)]
    pub slack_id: Option<String>,
    #[serde(default)]
    pub verification_status: Option<String>,
    #[serde(default)]
    pub ysws_eligible: Option<bool>,
}

pub async fn userinfo(st: &St, access_token: &str) -> Result<UserInfo> {
    let d = st.oidc.discovery(st).await?;
    let info: UserInfo =
        st.http.get(st.config.provider_url(&d.userinfo_endpoint)).bearer_auth(access_token).send().await?.error_for_status()?.json().await.context("userinfo")?;
    if info.sub.is_empty() || info.sub.len() > 200 {
        bail!("userinfo has no usable sub");
    }
    Ok(info)
}
