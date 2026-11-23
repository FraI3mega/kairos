use anyhow::{bail, Result};
use axum::http::StatusCode;
use deadpool_postgres::Pool;
use serde_json::json;

use crate::audit;
use crate::auth::oidc::UserInfo;
use crate::auth::User;
use crate::util::{clean, err, hash, random_bytes, random_uuid, Refused, Res};
use crate::St;

pub const ROLES: &[&str] = &["user", "reviewer", "admin"];
pub const TRUST: &[&str] = &["unscored", "trusted", "suspicious", "banned"];
pub const TRUST_REASON_MAX: usize = 500;

const RESERVED: &[&str] = &[
    "about", "account", "accounts", "admin", "administrator", "api", "app", "auth", "billing", "blog", "callback",
    "current", "dashboard", "delete", "device", "devices", "docs", "download", "downloads", "everyone", "explore", "export",
    "faq", "favicon", "hackatime", "hackclub", "hack-club", "hcb", "health", "healthz", "help", "here", "home",
    "install", "kairos", "leaderboard", "leaderboards", "login", "logout", "lookup_email", "lookup_slack_uid",
    "me", "mock-auth", "moderator", "mod", "my", "new", "news", "null", "official", "og", "orpheus", "privacy",
    "program", "programs", "projects", "review", "reviewer", "reviewers", "root", "security", "settings", "signin",
    "signout", "signup", "staff", "static", "stats", "status", "support", "system", "team", "terms", "undefined",
    "user", "users", "www",
];

pub fn check_username(name: &str) -> std::result::Result<(), &'static str> {
    let n = name.len();
    if !(2..=32).contains(&n) {
        return Err("Usernames are 2 to 32 characters.");
    }
    if !name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_') {
        return Err("Use lowercase letters, digits, - and _ only.");
    }
    if !name.as_bytes()[0].is_ascii_alphanumeric() {
        return Err("Start with a letter or a digit.");
    }
    if name.bytes().all(|b| b.is_ascii_digit()) {
        return Err("Usernames can't be only digits.");
    }
    if RESERVED.contains(&name) {
        return Err("That username is reserved.");
    }
    Ok(())
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.to_lowercase().chars() {
        let c = if c.is_ascii_alphanumeric() || c == '_' { c } else { '-' };
        if !(c == '-' && (out.is_empty() || out.ends_with('-'))) {
            out.push(c);
        }
    }
    out.trim_matches('-').chars().take(24).collect::<String>().trim_matches('-').to_string()
}

fn candidates(info: &Identity) -> String {
    [info.nickname.as_deref(), info.name.as_deref(), info.email.as_deref().and_then(|e| e.split('@').next())]
        .into_iter()
        .flatten()
        .map(slug)
        .find(|s| s.len() >= 2 && !s.bytes().all(|b| b.is_ascii_digit()))
        .unwrap_or_else(|| "hacker".into())
}

#[derive(Debug, Clone)]
pub struct Identity {
    pub sub: String,
    pub name: Option<String>,
    pub nickname: Option<String>,
    pub email: Option<String>,
    pub slack_id: Option<String>,
    pub verification_status: Option<String>,
    pub ysws_eligible: Option<bool>,
}

impl From<UserInfo> for Identity {
    fn from(u: UserInfo) -> Identity {
        let opt = |s: Option<String>, max| s.map(|s| clean(&s, max)).filter(|s| !s.is_empty());
        Identity {
            sub: u.sub,
            name: opt(u.name, 80),
            nickname: opt(u.nickname, 64),
            email: opt(u.email, 254).filter(|_| u.email_verified != Some(false)),
            slack_id: opt(u.slack_id, 32),
            verification_status: opt(u.verification_status, 32),
            ysws_eligible: u.ysws_eligible,
        }
    }
}

/// Username and display name are only set on the first sign-in; the rest of the identity refreshes from HCA each time.
pub async fn upsert(st: &St, id: &Identity) -> Res<i64> {
    let mut c = st.pool.get().await?;
    let update = "UPDATE users SET email = $2, slack_id = $3, verification_status = $4, ysws_eligible = $5
                  WHERE hca_sub = $1 RETURNING id";
    let params: [&(dyn tokio_postgres::types::ToSql + Sync); 5] =
        [&id.sub, &id.email, &id.slack_id, &id.verification_status, &id.ysws_eligible];
    if let Some(r) = c.query_opt(update, &params).await? {
        return Ok(r.get(0));
    }
    let base = candidates(id);
    let display = id.nickname.clone().or_else(|| id.name.as_ref().and_then(|n| n.split(' ').next().map(str::to_string)));
    for n in 0..40u32 {
        let username = match n {
            0 => base.clone(),
            1..=19 => format!("{base}-{}", n + 1),
            _ => format!("{base}-{}", u32::from_le_bytes(random_bytes::<4>()?) % 100_000),
        };
        if check_username(&username).is_err() {
            continue;
        }
        let tx = c.transaction().await?;
        let row = tx
            .query_opt(
                "INSERT INTO users (hca_sub, username, display_name, email, slack_id, verification_status, ysws_eligible)
                 VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT DO NOTHING RETURNING id",
                &[&id.sub, &username, &display, &id.email, &id.slack_id, &id.verification_status, &id.ysws_eligible],
            )
            .await?;
        if let Some(r) = row {
            let user_id: i64 = r.get(0);
            restore(&tx, user_id, &id.sub).await?;
            tx.commit().await?;
            return Ok(user_id);
        }
        drop(tx);
        if let Some(r) = c.query_opt(update, &params).await? {
            return Ok(r.get(0));
        }
    }
    Err(err(StatusCode::CONFLICT, "could not pick a username, try again"))
}

async fn restore(tx: &tokio_postgres::Transaction<'_>, user_id: i64, sub: &str) -> Result<()> {
    let Some(r) = tx.query_opt("DELETE FROM departed WHERE hca_sub = $1 RETURNING trust, notes", &[&sub]).await? else {
        return Ok(());
    };
    let trust: String = r.get(0);
    let notes: serde_json::Value = r.get(1);
    tx.execute("UPDATE users SET trust = $2 WHERE id = $1", &[&user_id, &trust]).await?;
    let restored = tx
        .execute(
            "INSERT INTO review_notes (user_id, author_id, body, created_at)
             SELECT $1, (SELECT id FROM users WHERE id = (n->>'author_id')::bigint), n->>'body', (n->>'created_at')::timestamptz
             FROM jsonb_array_elements($2::jsonb) WITH ORDINALITY AS t(n, i) ORDER BY i",
            &[&user_id, &notes],
        )
        .await?;
    audit::log(tx, None, "trust.restore", Some(user_id), json!({ "to": trust, "notes": restored })).await?;
    Ok(())
}

pub async fn new_editor_key(st: &St, user_id: i64) -> Res<String> {
    let key = random_uuid()?;
    let mut c = st.pool.get().await?;
    let tx = c.transaction().await?;
    tx.execute(
        "UPDATE api_keys SET revoked_at = now() WHERE user_id = $1 AND kind = 'editor' AND revoked_at IS NULL",
        &[&user_id],
    )
    .await?;
    tx.execute(
        "INSERT INTO api_keys (kind, user_id, token_hash, prefix) VALUES ('editor', $1, $2, $3)",
        &[&user_id, &hash(&key), &key[..8].to_string()],
    )
    .await?;
    tx.commit().await?;
    Ok(key)
}

pub async fn editor_key_user(st: &St, key: &str) -> Res<Option<i64>> {
    if key.is_empty() || key.len() > 128 {
        return Ok(None);
    }
    let c = st.pool.get().await?;
    let Some(r) = c
        .query_opt(
            "SELECT id, user_id FROM api_keys WHERE token_hash = $1 AND kind = 'editor' AND revoked_at IS NULL",
            &[&hash(key)],
        )
        .await?
    else {
        return Ok(None);
    };
    let (id, user_id): (i64, Option<i64>) = (r.get(0), r.get(1));
    c.execute(
        "UPDATE api_keys SET last_used_at = now() WHERE id = $1 AND (last_used_at IS NULL OR last_used_at < now() - interval '5 minutes')",
        &[&id],
    )
    .await?;
    Ok(user_id)
}

pub async fn delete(pool: &Pool, user_id: i64) -> Result<()> {
    let mut c = pool.get().await?;
    let tx = c.transaction().await?;
    audit::log(&tx, None, "account.delete", Some(user_id), json!({ "user_id": user_id })).await?;
    tx.execute(
        "INSERT INTO departed (hca_sub, trust, notes)
         SELECT u.hca_sub, u.trust,
                COALESCE((SELECT jsonb_agg(jsonb_build_object('author_id', n.author_id, 'body', n.body, 'created_at', n.created_at) ORDER BY n.id)
                          FROM review_notes n WHERE n.user_id = u.id), '[]')
         FROM users u
         WHERE u.id = $1 AND (u.trust <> 'unscored' OR EXISTS (SELECT 1 FROM review_notes n WHERE n.user_id = u.id))
         ON CONFLICT (hca_sub) DO UPDATE SET trust = EXCLUDED.trust, notes = EXCLUDED.notes, deleted_at = now()",
        &[&user_id],
    )
    .await?;
    tx.execute("DELETE FROM users WHERE id = $1", &[&user_id]).await?;
    tx.commit().await?;
    Ok(())
}

pub fn may_set_trust(role: &str, from: &str, to: &str) -> bool {
    match role {
        "admin" => true,
        "reviewer" => from != "banned" && to != "banned",
        _ => false,
    }
}

pub async fn set_trust(pool: &Pool, actor: &User, user_id: i64, to: &str, reason: &str) -> Result<Option<String>> {
    if !TRUST.contains(&to) {
        bail!(Refused(format!("trust must be one of {}", TRUST.join(", "))));
    }
    let reason = clean(reason, TRUST_REASON_MAX);
    if reason.is_empty() {
        bail!(Refused("give a reason, it goes in the audit log".into()));
    }
    if actor.id == user_id {
        bail!(Refused("ask another reviewer to change your own trust".into()));
    }
    let mut c = pool.get().await?;
    let tx = c.transaction().await?;
    let Some(r) = tx.query_opt("SELECT trust FROM users WHERE id = $1 FOR UPDATE", &[&user_id]).await? else {
        bail!(Refused("no such user".into()));
    };
    let from: String = r.get(0);
    if !may_set_trust(&actor.role, &from, to) {
        bail!(Refused("only admins can ban someone or lift a ban".into()));
    }
    if from == to {
        return Ok(None);
    }
    tx.execute("UPDATE users SET trust = $2 WHERE id = $1", &[&user_id, &to]).await?;
    audit::log(&tx, Some(actor.id), "trust.set", Some(user_id), json!({ "from": from, "to": to, "reason": reason })).await?;
    tx.commit().await?;
    Ok(Some(from))
}

pub async fn grant(pool: &Pool, actor: Option<i64>, username: &str, role: &str, via: &str) -> Result<String> {
    if !ROLES.contains(&role) {
        bail!("role must be one of {}", ROLES.join(", "));
    }
    let mut c = pool.get().await?;
    let tx = c.transaction().await?;
    let Some(r) = tx
        .query_opt("SELECT id, role FROM users WHERE username = $1 FOR UPDATE", &[&username.trim().to_lowercase()])
        .await?
    else {
        bail!("no user named {username}");
    };
    let (id, old): (i64, String) = (r.get(0), r.get(1));
    if old != role {
        tx.execute("UPDATE users SET role = $2 WHERE id = $1", &[&id, &role]).await?;
        audit::log(&tx, actor, "role.change", Some(id), json!({ "from": old, "to": role, "via": via })).await?;
    }
    tx.commit().await?;
    Ok(old)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usernames() {
        for ok in ["ada", "a1", "ada_l", "ada-lovelace", "1ada"] {
            assert!(check_username(ok).is_ok(), "{ok}");
        }
        for bad in ["a", "Ada", "-ada", "ada!", "1234", "admin", "settings", "x".repeat(33).as_str()] {
            assert!(check_username(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn only_admins_ban_or_unban() {
        for (role, from, to, ok) in [
            ("reviewer", "unscored", "trusted", true),
            ("reviewer", "trusted", "suspicious", true),
            ("reviewer", "suspicious", "unscored", true),
            ("reviewer", "suspicious", "banned", false),
            ("reviewer", "banned", "unscored", false),
            ("admin", "suspicious", "banned", true),
            ("admin", "banned", "trusted", true),
            ("user", "unscored", "trusted", false),
        ] {
            assert_eq!(may_set_trust(role, from, to), ok, "{role} {from} -> {to}");
        }
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("Ada Lovelace"), "ada-lovelace");
        assert_eq!(slug("  Zoë  "), "zo");
        assert_eq!(slug("!!!"), "");
    }
}
