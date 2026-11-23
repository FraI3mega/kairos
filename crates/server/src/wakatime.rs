use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::util::{bearer, err, hash, Res};
use crate::St;

#[derive(Deserialize)]
pub struct KeyQ {
    api_key: Option<String>,
}

async fn user_id(st: &St, h: &HeaderMap, q: &KeyQ) -> Res<i64> {
    let key = bearer(h).or_else(|| q.api_key.clone()).ok_or_else(|| err(StatusCode::UNAUTHORIZED, "missing api key"))?;
    crate::limits::check(st, crate::limits::Bucket::Heartbeats, &hash(&key))?;
    crate::accounts::editor_key_user(st, &key).await?.ok_or_else(|| err(StatusCode::UNAUTHORIZED, "unknown api key"))
}

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).filter(|x| !x.is_empty()).map(|x| x.chars().take(200).collect())
}

async fn store(st: &St, uid: i64, ua: Option<&str>, beats: &[Value]) -> Res<Vec<(Value, u16)>> {
    let c = st.pool.get().await?;
    let ed = ua.and_then(kairos_core::wakatime::editor);
    let mut out = Vec::with_capacity(beats.len());
    for b in beats {
        let Some(time) = b.get("time").and_then(Value::as_f64).filter(|t| t.is_finite() && *t > 0.0) else {
            out.push((json!({ "error": "missing time" }), 400));
            continue;
        };
        if kairos_core::wakatime::from_agent(b) {
            out.push((json!({ "data": { "id": format!("{uid}-{time}"), "time": time } }), 201));
            continue;
        }
        let entity_hash = hash(&s(b, "entity").unwrap_or_default());
        let is_write = b.get("is_write").and_then(Value::as_bool).unwrap_or(false);
        c.execute(
            "INSERT INTO heartbeats (user_id, time, entity_hash, project, language, editor, branch, category, is_write)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) ON CONFLICT DO NOTHING",
            &[&uid, &time, &entity_hash, &s(b, "project"), &s(b, "language"), &ed, &s(b, "branch"), &s(b, "category"), &is_write],
        )
        .await?;
        out.push((json!({ "data": { "id": format!("{uid}-{time}"), "time": time } }), 201));
    }
    Ok(out)
}

fn parse(body: &Bytes) -> Res<Value> {
    serde_json::from_slice(body).map_err(|_| err(StatusCode::BAD_REQUEST, "invalid json"))
}

pub async fn one(State(st): State<St>, h: HeaderMap, Query(q): Query<KeyQ>, body: Bytes) -> Res<Response> {
    let uid = user_id(&st, &h, &q).await?;
    let v = parse(&body)?;
    let beats = match v {
        Value::Array(a) => a,
        v => vec![v],
    };
    let ua = h.get(header::USER_AGENT).and_then(|v| v.to_str().ok());
    let mut res = store(&st, uid, ua, &beats).await?;
    let (body, code) = res.pop().unwrap_or((json!({}), 201));
    Ok((StatusCode::from_u16(code).unwrap_or(StatusCode::CREATED), Json(body)).into_response())
}

pub async fn bulk(State(st): State<St>, h: HeaderMap, Query(q): Query<KeyQ>, body: Bytes) -> Res<Response> {
    let uid = user_id(&st, &h, &q).await?;
    let v = parse(&body)?;
    let beats = match v {
        Value::Array(a) => a,
        v => vec![v],
    };
    let ua = h.get(header::USER_AGENT).and_then(|v| v.to_str().ok());
    let res = store(&st, uid, ua, &beats).await?;
    let responses: Vec<Value> = res.into_iter().map(|(b, c)| json!([b, c])).collect();
    Ok((StatusCode::ACCEPTED, Json(json!({ "responses": responses }))).into_response())
}
