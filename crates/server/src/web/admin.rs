use askama::Template;
use axum::extract::{Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::Form;
use deadpool_postgres::GenericClient;
use serde::Deserialize;
use serde_json::json;

use super::{render, Base, Ctx};
use crate::accounts::{self, ROLES};
use crate::audit;
use crate::auth::{Session, SignedIn};
use crate::programs::{self, ProgramInfo};
use crate::util::{clean, err, refused_or_internal, Res};
use crate::St;

pub struct UserRow {
    pub id: i64,
    pub username: String,
    pub display_name: Option<String>,
    pub slack_id: Option<String>,
    pub role: String,
    pub trust: String,
    pub created: String,
}

pub struct AuditRow {
    pub at: String,
    pub actor: String,
    pub action: String,
    pub target: String,
    pub detail: String,
}

#[derive(Template)]
#[template(path = "admin.html")]
struct AdminPage {
    base: Base,
    q: String,
    users: Vec<UserRow>,
    staff: Vec<UserRow>,
    audit: Vec<AuditRow>,
    roles: &'static [&'static str],
    me: i64,
    programs: Vec<ProgramInfo>,
}

#[derive(Template)]
#[template(path = "admin_key.html")]
struct KeyPage {
    base: Base,
    program: String,
    prefix: String,
    key: String,
}

fn admin(s: &Session) -> Res<()> {
    if s.user.is_admin() {
        Ok(())
    } else {
        Err(err(StatusCode::NOT_FOUND, "not found"))
    }
}

const USER_ROW: &str = "SELECT id, username, display_name, slack_id, role, trust, to_char(created_at, 'YYYY-MM-DD') FROM users";

fn user_row(r: &tokio_postgres::Row) -> UserRow {
    UserRow {
        id: r.get(0),
        username: r.get(1),
        display_name: r.get(2),
        slack_id: r.get(3),
        role: r.get(4),
        trust: r.get(5),
        created: r.get(6),
    }
}

pub async fn search(c: &impl GenericClient, q: &str) -> anyhow::Result<Vec<UserRow>> {
    if q.is_empty() {
        return Ok(Vec::new());
    }
    let like = format!("{}%", q.to_lowercase().replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
    Ok(c
        .query(
            &format!("{USER_ROW} WHERE username LIKE $1 OR slack_id = $2 OR hca_sub = $2 OR lower(email) = lower($2) ORDER BY username LIMIT 50"),
            &[&like, &q],
        )
        .await?
        .iter()
        .map(user_row)
        .collect())
}

#[derive(Deserialize)]
pub struct SearchQ {
    pub q: Option<String>,
}

pub async fn page(State(st): State<St>, ctx: Ctx, SignedIn(s): SignedIn, Query(sq): Query<SearchQ>) -> Res<Response> {
    admin(&s)?;
    let q = clean(sq.q.as_deref().unwrap_or(""), 64);
    let (users, staff, audit) = {
        let mut c = st.pool.get().await?;
        let tx = c.transaction().await?;
        audit::log(&tx, Some(s.user.id), "admin.view", None, json!({ "page": "admin", "q": q })).await?;
        let users = search(&tx, &q).await?;
        let staff = tx.query(&format!("{USER_ROW} WHERE role <> 'user' ORDER BY role, username"), &[]).await?.iter().map(user_row).collect();
        let audit = tx
            .query(
                "SELECT to_char(a.at, 'YYYY-MM-DD HH24:MI:SS'), COALESCE(au.username, CASE WHEN a.actor_id IS NULL THEN 'system' ELSE 'deleted' END),
                        a.action, COALESCE(tu.username, CASE WHEN a.target_user_id IS NULL THEN '' ELSE 'deleted' END), a.detail::text
                 FROM audit_log a LEFT JOIN users au ON au.id = a.actor_id LEFT JOIN users tu ON tu.id = a.target_user_id
                 ORDER BY a.id DESC LIMIT 100",
                &[],
            )
            .await?
            .iter()
            .map(|r| AuditRow { at: r.get(0), actor: r.get(1), action: r.get(2), target: r.get(3), detail: r.get(4) })
            .collect();
        tx.commit().await?;
        (users, staff, audit)
    };
    let mut programs = programs::list(&st.pool).await?;
    for p in &mut programs {
        to_minute(&mut p.created_at);
        for k in &mut p.keys {
            to_minute(&mut k.created_at);
            k.last_used_at.iter_mut().chain(k.revoked_at.iter_mut()).for_each(to_minute);
        }
    }
    render(&AdminPage { base: Base::new(&st, &ctx, "Admin · Kairos"), q, users, staff, audit, roles: ROLES, me: s.user.id, programs })
}

fn to_minute(stamp: &mut String) {
    if let Some(s) = stamp.get(..16) {
        *stamp = s.replacen('T', " ", 1);
    }
}

#[derive(Deserialize)]
pub struct RoleForm {
    csrf: String,
    username: String,
    role: String,
}

pub async fn role(State(st): State<St>, SignedIn(s): SignedIn, Form(f): Form<RoleForm>) -> Res<Response> {
    admin(&s)?;
    s.check_csrf(&f.csrf)?;
    if f.username.trim().to_lowercase() == s.user.username {
        return Err(err(StatusCode::BAD_REQUEST, "change your own role with kairos-server grant"));
    }
    accounts::grant(&st.pool, Some(s.user.id), &f.username, &f.role, "admin").await.map_err(|e| {
        tracing::info!(error = format!("{e:#}"), "role change refused");
        err(StatusCode::BAD_REQUEST, "unknown user or role")
    })?;
    Ok(Redirect::to(&format!("/admin?q={}", crate::util::urlenc(&f.username))).into_response())
}

#[derive(Deserialize)]
pub struct ProgramForm {
    csrf: String,
    slug: String,
    name: String,
}

pub async fn create_program(State(st): State<St>, SignedIn(s): SignedIn, Form(f): Form<ProgramForm>) -> Res<Response> {
    admin(&s)?;
    s.check_csrf(&f.csrf)?;
    programs::create(&st.pool, Some(s.user.id), f.slug.trim(), &f.name, "admin").await.map_err(refused_or_internal)?;
    Ok(Redirect::to("/admin#programs").into_response())
}

#[derive(Deserialize)]
pub struct KeyForm {
    csrf: String,
    slug: String,
    #[serde(default)]
    name: String,
}

/// The key is shown on this response only, so the page is never cached.
pub async fn create_key(State(st): State<St>, ctx: Ctx, SignedIn(s): SignedIn, Form(f): Form<KeyForm>) -> Res<Response> {
    admin(&s)?;
    s.check_csrf(&f.csrf)?;
    let slug = f.slug.trim();
    let (key, prefix) = programs::new_key(&st.pool, Some(s.user.id), slug, Some(&f.name), "admin").await.map_err(refused_or_internal)?;
    let mut res = render(&KeyPage { base: Base::new(&st, &ctx, "New program key · Kairos"), program: slug.to_string(), prefix, key })?;
    res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(res)
}

#[derive(Deserialize)]
pub struct RevokeForm {
    csrf: String,
    prefix: String,
}

pub async fn revoke_key(State(st): State<St>, SignedIn(s): SignedIn, Form(f): Form<RevokeForm>) -> Res<Response> {
    admin(&s)?;
    s.check_csrf(&f.csrf)?;
    programs::revoke(&st.pool, Some(s.user.id), &f.prefix, "admin").await.map_err(refused_or_internal)?;
    Ok(Redirect::to("/admin#programs").into_response())
}
