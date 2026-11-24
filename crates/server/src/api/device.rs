use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use kairos_core::pkce;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::limits::{self, Bucket};
use crate::util::{bearer, clean, err, hash, random_token, user_code, Res};
use crate::St;

const CODE_SECS: f64 = 900.0;

pub struct Device {
    pub id: i64,
    pub user_id: i64,
    pub token_hash: String,
}

pub async fn auth(st: &St, h: &HeaderMap) -> Res<Device> {
    let tok = bearer(h).filter(|t| t.len() <= 128).ok_or_else(|| err(StatusCode::UNAUTHORIZED, "missing device token"))?;
    let token_hash = hash(&tok);
    let c = st.pool.get().await?;
    let r = c
        .query_opt("SELECT id, user_id FROM devices WHERE token_hash = $1 AND revoked_at IS NULL", &[&token_hash])
        .await?
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "unknown or revoked device token"))?;
    Ok(Device { id: r.get(0), user_id: r.get(1), token_hash })
}

pub fn client_version(h: &HeaderMap) -> Option<String> {
    let ua = h.get(header::USER_AGENT)?.to_str().ok()?;
    let v = ua.split_whitespace().find_map(|t| t.strip_prefix("kairosd/").or_else(|| t.strip_prefix("kairos/")))?;
    let ok = !v.is_empty() && v.len() <= 32 && v.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+'));
    ok.then(|| v.to_string())
}

fn name_and_os(name: &str, os: &str) -> (String, String) {
    (Some(clean(name, 64)).filter(|n| !n.is_empty()).unwrap_or_else(|| "computer".into()), clean(os, 16))
}

/// What a computer gets when it signs in.
struct Minted {
    token: String,
    username: String,
    /// The computer got a device it had before, and uploads go on from where they stopped.
    resumed: bool,
}

struct New<'a> {
    name: &'a str,
    os: &'a str,
    version: &'a Option<String>,
    /// The last event the computer uploaded, to anyone.
    last_hash: Option<&'a str>,
}

/// A computer signing in again gets its device back: the one of this account that ends on the last event the computer
/// uploaded, with a new token and with whatever disconnected it undone. Otherwise every sign-in would make another
/// device and the computer would send its whole log again.
async fn mint(tx: &tokio_postgres::Transaction<'_>, user_id: i64, new: New<'_>) -> Res<Minted> {
    let token = random_token("kd_")?;
    let token_hash = hash(&token);
    let last = new.last_hash.filter(|h| !h.is_empty() && h.len() <= 128 && h.bytes().all(|b| b.is_ascii_alphanumeric()));
    let resumed = match last {
        Some(last) => {
            tx.execute(
                "UPDATE devices SET token_hash = $3, revoked_at = NULL, client_version = COALESCE($4, client_version), last_seen_at = now()
                 WHERE id = (SELECT id FROM devices WHERE user_id = $1 AND head = $2 ORDER BY revoked_at IS NOT NULL, id DESC LIMIT 1 FOR UPDATE)",
                &[&user_id, &last, &token_hash, new.version],
            )
            .await?
                == 1
        }
        None => false,
    };
    if !resumed {
        tx.execute(
            "INSERT INTO devices (user_id, token_hash, name, os, client_version, last_seen_at) VALUES ($1, $2, $3, $4, $5, now())",
            &[&user_id, &token_hash, &new.name, &new.os, new.version],
        )
        .await?;
    }
    let username: String = tx.query_one("SELECT username FROM users WHERE id = $1", &[&user_id]).await?.get(0);
    Ok(Minted { token, username, resumed })
}

#[derive(Deserialize)]
pub struct StartReq {
    name: String,
    os: String,
    #[allow(dead_code)]
    #[serde(default)]
    utc_offset_min: i32,
}

pub async fn start(State(st): State<St>, h: HeaderMap, ext: axum::http::Extensions, Json(r): Json<StartReq>) -> Res<Json<Value>> {
    let ip = limits::client_ip(&ext, &h, &st.config.trusted_proxies);
    limits::check(&st, Bucket::DeviceStart, &limits::ip_key(ip))?;
    let (name, os) = name_and_os(&r.name, &r.os);
    let code = user_code()?;
    let poll = random_token("kp_")?;
    let c = st.pool.get().await?;
    c.execute(
        "INSERT INTO device_codes (user_code, poll_hash, name, os, client_version, expires_at)
         VALUES ($1, $2, $3, $4, $5, now() + make_interval(secs => $6))",
        &[&code, &hash(&poll), &name, &os, &client_version(&h), &CODE_SECS],
    )
    .await?;
    Ok(Json(json!({
        "user_code": code,
        "poll_token": poll,
        "verify_url": format!("{}/device?code={code}", st.config.public_url),
        "interval": 3,
        "expires_in": CODE_SECS as u64,
    })))
}

#[derive(Deserialize)]
pub struct PollReq {
    poll_token: String,
    #[serde(default)]
    last_hash: Option<String>,
}

fn gone() -> Response {
    (StatusCode::GONE, Json(json!({ "status": "expired" }))).into_response()
}

/// The token is minted here, on the first poll after approval, so it is never stored in plain text.
/// Deleting the code row in the same statement makes the hand-off single use.
pub async fn poll(State(st): State<St>, Json(r): Json<PollReq>) -> Res<Response> {
    if r.poll_token.is_empty() || r.poll_token.len() > 128 {
        return Ok(gone());
    }
    let poll_hash = hash(&r.poll_token);
    limits::check(&st, Bucket::DevicePoll, &poll_hash)?;
    let mut c = st.pool.get().await?;
    let tx = c.transaction().await?;
    let row = tx
        .query_opt(
            "DELETE FROM device_codes WHERE poll_hash = $1 AND approved_at IS NOT NULL AND expires_at > now()
             RETURNING user_id, name, os, client_version",
            &[&poll_hash],
        )
        .await?;
    if let Some(row) = row {
        let (user_id, name, os, version): (i64, String, String, Option<String>) = (row.get(0), row.get(1), row.get(2), row.get(3));
        let m = mint(&tx, user_id, New { name: &name, os: &os, version: &version, last_hash: r.last_hash.as_deref() }).await?;
        tx.commit().await?;
        return Ok(Json(json!({ "status": "approved", "token": m.token, "username": m.username, "resumed": m.resumed })).into_response());
    }
    let pending = tx.query_opt("SELECT 1 FROM device_codes WHERE poll_hash = $1 AND expires_at > now()", &[&poll_hash]).await?;
    tx.commit().await?;
    Ok(match pending {
        Some(_) => Json(json!({ "status": "pending" })).into_response(),
        None => gone(),
    })
}

#[derive(Deserialize)]
pub struct ExchangeReq {
    code: String,
    verifier: String,
    name: String,
    os: String,
    #[serde(default)]
    last_hash: Option<String>,
}

/// The second half of `/device/connect`: the program that opened it trades the code it got on 127.0.0.1 for a device
/// token, proving with the verifier that it is the one that asked. Each code works once, right or wrong.
pub async fn exchange(State(st): State<St>, h: HeaderMap, ext: axum::http::Extensions, Json(r): Json<ExchangeReq>) -> Res<Json<Value>> {
    let ip = limits::client_ip(&ext, &h, &st.config.trusted_proxies);
    limits::check(&st, Bucket::DeviceStart, &limits::ip_key(ip))?;
    let refused = || err(StatusCode::BAD_REQUEST, "this sign-in expired or was already used, run kairos login again");
    if r.code.is_empty() || r.code.len() > 128 || !pkce::is_verifier(&r.verifier) {
        return Err(refused());
    }
    let mut c = st.pool.get().await?;
    let tx = c.transaction().await?;
    let row = tx
        .query_opt("DELETE FROM device_grants WHERE code_hash = $1 AND expires_at > now() RETURNING user_id, challenge", &[&hash(&r.code)])
        .await?;
    let Some(row) = row else { return Err(refused()) };
    let (user_id, challenge): (i64, String) = (row.get(0), row.get(1));
    if pkce::challenge(&r.verifier) != challenge {
        tx.commit().await?;
        return Err(refused());
    }
    let (name, os) = name_and_os(&r.name, &r.os);
    let m = mint(&tx, user_id, New { name: &name, os: &os, version: &client_version(&h), last_hash: r.last_hash.as_deref() }).await?;
    tx.commit().await?;
    Ok(Json(json!({ "token": m.token, "username": m.username, "resumed": m.resumed })))
}

/// `kairos logout`: the computer gives up its token. The device and its events stay, and it gets the device back when
/// it signs in again.
pub async fn logout(State(st): State<St>, h: HeaderMap) -> Res<Json<Value>> {
    let d = auth(&st, &h).await?;
    let c = st.pool.get().await?;
    c.execute("UPDATE devices SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL", &[&d.id]).await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn me(State(st): State<St>, h: HeaderMap) -> Res<Json<Value>> {
    let d = auth(&st, &h).await?;
    let c = st.pool.get().await?;
    let r = c
        .query_one(
            "UPDATE devices d SET last_seen_at = now(), client_version = COALESCE($2, d.client_version)
             FROM users u WHERE d.id = $1 AND u.id = d.user_id RETURNING u.username, d.head, d.event_count",
            &[&d.id, &client_version(&h)],
        )
        .await?;
    let (username, head, events): (String, Option<String>, i64) = (r.get(0), r.get(1), r.get(2));
    Ok(Json(json!({ "username": username, "device_id": d.id, "last_hash": head, "events": events })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_from_user_agents() {
        let mut h = HeaderMap::new();
        h.insert(header::USER_AGENT, "kairosd/0.3.1".parse().unwrap());
        assert_eq!(client_version(&h).as_deref(), Some("0.3.1"));
        h.insert(header::USER_AGENT, "reqwest/0.12".parse().unwrap());
        assert_eq!(client_version(&h), None);
        h.insert(header::USER_AGENT, "kairosd/<script>".parse().unwrap());
        assert_eq!(client_version(&h), None);
    }
}
