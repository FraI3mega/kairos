use askama::Template;
use axum::extract::{Query, State};
use axum::http::{header, HeaderValue};
use axum::response::Response;
use axum::Form;
use kairos_core::pkce;
use serde::Deserialize;

use super::{render, Base, Ctx, Message};
use crate::auth::SignedIn;
use crate::util::{hash, random_token, Res};
use crate::St;

const GRANT_SECS: f64 = 120.0;

#[derive(Template)]
#[template(path = "device.html")]
struct DevicePage {
    base: Base,
    code: String,
    device: Option<(String, String)>,
    done: Option<String>,
}

fn normalize(code: &str) -> String {
    code.trim().to_uppercase().chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(16).collect()
}

#[derive(Deserialize)]
pub struct CodeQ {
    code: Option<String>,
}

pub async fn page(State(st): State<St>, ctx: Ctx, SignedIn(_): SignedIn, Query(q): Query<CodeQ>) -> Res<Response> {
    let code = normalize(q.code.as_deref().unwrap_or(""));
    let c = st.pool.get().await?;
    let device = c
        .query_opt("SELECT name, os FROM device_codes WHERE user_code = $1 AND expires_at > now() AND approved_at IS NULL", &[&code])
        .await?
        .map(|r| (r.get(0), r.get(1)));
    render(&DevicePage { base: Base::new(&st, &ctx, "Connect a computer · Kairos"), code, device, done: None })
}

#[derive(Deserialize)]
pub struct ApproveForm {
    code: String,
    csrf: String,
}

pub async fn approve(State(st): State<St>, ctx: Ctx, SignedIn(s): SignedIn, Form(f): Form<ApproveForm>) -> Res<Response> {
    s.check_csrf(&f.csrf)?;
    let code = normalize(&f.code);
    let c = st.pool.get().await?;
    let row = c
        .query_opt(
            "UPDATE device_codes SET user_id = $1, approved_at = now(), expires_at = GREATEST(expires_at, now() + interval '2 minutes')
             WHERE user_code = $2 AND expires_at > now() AND approved_at IS NULL RETURNING name",
            &[&s.user.id, &code],
        )
        .await?;
    let base = Base::new(&st, &ctx, "Connect a computer · Kairos");
    render(&DevicePage { base, code, device: None, done: row.map(|r| r.get(0)) })
}

#[derive(Template)]
#[template(path = "device_handoff.html")]
struct Handoff {
    base: Base,
    to: String,
}

#[derive(Deserialize)]
pub struct ConnectQ {
    port: Option<String>,
    state: Option<String>,
    challenge: Option<String>,
}

fn is_state(s: &str) -> bool {
    (16..=128).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

/// Signs in the computer whose `kairos login` opened this page. The code goes back to that program on 127.0.0.1, and only
/// it can trade the code for a device token, since only it holds the verifier behind the challenge. So there is nothing
/// to compare or click: a link someone else made can only ever hand a code to a program on your own computer.
pub async fn connect(State(st): State<St>, ctx: Ctx, SignedIn(s): SignedIn, Query(q): Query<ConnectQ>) -> Res<Response> {
    let port = q.port.and_then(|p| p.parse::<u16>().ok()).filter(|p| *p >= 1024);
    let state = q.state.filter(|s| is_state(s));
    let challenge = q.challenge.filter(|c| pkce::is_challenge(c));
    let (Some(port), Some(state), Some(challenge)) = (port, state, challenge) else {
        return render(&Message {
            base: Base::new(&st, &ctx, "Connect a computer · Kairos"),
            heading: "That link doesn't work".into(),
            text: "Run kairos login in a terminal to connect this computer.".into(),
        });
    };
    let code = random_token("kg_")?;
    st.pool
        .get()
        .await?
        .execute(
            "INSERT INTO device_grants (code_hash, user_id, challenge, expires_at) VALUES ($1, $2, $3, now() + make_interval(secs => $4))",
            &[&hash(&code), &s.user.id, &challenge, &GRANT_SECS],
        )
        .await?;
    // A page that moves on, not a redirect: when this request is the tail of a sign-in form's redirects, Chrome holds the
    // whole chain to the form-action of the page that posted, and a hop to 127.0.0.1 fails it.
    let to = format!("http://127.0.0.1:{port}/callback?code={code}&state={state}");
    let mut res = render(&Handoff { base: Base::new(&st, &ctx, "Connecting · Kairos"), to })?;
    res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(res)
}
