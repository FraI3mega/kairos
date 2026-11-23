//! Program keys are shown once, stored hashed, and every read one makes is audited.

use anyhow::{bail, Result};
use deadpool_postgres::Pool;
use serde_json::json;

use crate::audit;
use crate::util::{clean, hash, random_token, Refused, Res};
use crate::St;

pub const KEY_PREFIX: &str = "kp_";
// "kp_" plus the first characters of the random part, enough to tell keys apart in lists.
const PREFIX_LEN: usize = 11;
const MIN_REVOKE_PREFIX: usize = 6;

#[derive(Debug, Clone)]
pub struct Key {
    pub id: i64,
    pub program_id: i64,
    pub program: String,
    pub prefix: String,
}

pub struct KeyInfo {
    pub prefix: String,
    pub name: Option<String>,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub revoked_at: Option<String>,
}

pub struct ProgramInfo {
    pub slug: String,
    pub name: String,
    pub created_at: String,
    pub keys: Vec<KeyInfo>,
}

fn check_slug(slug: &str) -> Result<()> {
    let b = slug.as_bytes();
    let ok = (2..=40).contains(&b.len())
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-');
    if !ok {
        bail!(Refused("a program slug is 2 to 40 characters: lowercase letters, digits and -, starting with a letter or digit".into()));
    }
    Ok(())
}

pub async fn create(pool: &Pool, actor: Option<i64>, slug: &str, name: &str, via: &str) -> Result<i64> {
    check_slug(slug)?;
    let name = clean(name, 80);
    if name.is_empty() {
        bail!(Refused("a program needs a name".into()));
    }
    let mut c = pool.get().await?;
    let tx = c.transaction().await?;
    let Some(r) = tx
        .query_opt(
            "INSERT INTO programs (slug, name, created_by) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING RETURNING id",
            &[&slug, &name, &actor],
        )
        .await?
    else {
        bail!(Refused(format!("a program with the slug {slug} already exists")));
    };
    audit::log(&tx, actor, "program.create", None, json!({ "slug": slug, "name": name, "via": via })).await?;
    tx.commit().await?;
    Ok(r.get(0))
}

pub async fn new_key(pool: &Pool, actor: Option<i64>, slug: &str, name: Option<&str>, via: &str) -> Result<(String, String)> {
    let name = name.map(|n| clean(n, 80)).filter(|n| !n.is_empty());
    let key = random_token(KEY_PREFIX)?;
    let prefix = key[..PREFIX_LEN].to_string();
    let mut c = pool.get().await?;
    let tx = c.transaction().await?;
    let Some(r) = tx.query_opt("SELECT id FROM programs WHERE slug = $1", &[&slug]).await? else {
        bail!(Refused(format!("no program with the slug {slug}")));
    };
    let program_id: i64 = r.get(0);
    tx.execute(
        "INSERT INTO api_keys (kind, program_id, name, token_hash, prefix) VALUES ('program', $1, $2, $3, $4)",
        &[&program_id, &name, &hash(&key), &prefix],
    )
    .await?;
    audit::log(&tx, actor, "program.key_create", None, json!({ "program": slug, "prefix": prefix, "name": name, "via": via })).await?;
    tx.commit().await?;
    Ok((key, prefix))
}

pub async fn revoke(pool: &Pool, actor: Option<i64>, prefix: &str, via: &str) -> Result<(String, String)> {
    let prefix = prefix.trim();
    if !prefix.starts_with(KEY_PREFIX) || prefix.len() < MIN_REVOKE_PREFIX {
        bail!(Refused(format!("give at least the first {MIN_REVOKE_PREFIX} characters of the key, starting with {KEY_PREFIX}")));
    }
    let mut c = pool.get().await?;
    let tx = c.transaction().await?;
    let rows = tx
        .query(
            "SELECT k.id, k.prefix, p.slug FROM api_keys k JOIN programs p ON p.id = k.program_id
             WHERE k.kind = 'program' AND k.revoked_at IS NULL AND starts_with(k.prefix, $1) FOR UPDATE OF k",
            &[&prefix],
        )
        .await?;
    let [r] = rows.as_slice() else {
        if rows.is_empty() {
            bail!(Refused(format!("no live program key starts with {prefix}")));
        }
        bail!(Refused(format!("{} live program keys start with {prefix}, give more characters", rows.len())));
    };
    let (id, full, slug): (i64, String, String) = (r.get(0), r.get(1), r.get(2));
    tx.execute("UPDATE api_keys SET revoked_at = now() WHERE id = $1", &[&id]).await?;
    audit::log(&tx, actor, "program.key_revoke", None, json!({ "program": slug, "prefix": full, "via": via })).await?;
    tx.commit().await?;
    Ok((slug, full))
}

pub async fn list(pool: &Pool) -> Result<Vec<ProgramInfo>> {
    let c = pool.get().await?;
    let programs = c
        .query("SELECT id, slug, name, to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') FROM programs ORDER BY slug", &[])
        .await?;
    let mut out = Vec::new();
    for p in &programs {
        let id: i64 = p.get(0);
        let keys = c
            .query(
                "SELECT prefix, name,
                        to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'),
                        to_char(last_used_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'),
                        to_char(revoked_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')
                 FROM api_keys WHERE kind = 'program' AND program_id = $1 ORDER BY id",
                &[&id],
            )
            .await?;
        out.push(ProgramInfo {
            slug: p.get(1),
            name: p.get(2),
            created_at: p.get(3),
            keys: keys
                .iter()
                .map(|k| KeyInfo { prefix: k.get(0), name: k.get(1), created_at: k.get(2), last_used_at: k.get(3), revoked_at: k.get(4) })
                .collect(),
        });
    }
    Ok(out)
}

pub async fn key_by_token(st: &St, token: &str) -> Res<Option<Key>> {
    if !token.starts_with(KEY_PREFIX) || token.len() > 128 {
        return Ok(None);
    }
    let c = st.pool.get().await?;
    let Some(r) = c
        .query_opt(
            "SELECT k.id, k.program_id, p.slug, k.prefix FROM api_keys k JOIN programs p ON p.id = k.program_id
             WHERE k.token_hash = $1 AND k.kind = 'program' AND k.revoked_at IS NULL",
            &[&hash(token)],
        )
        .await?
    else {
        return Ok(None);
    };
    let key = Key { id: r.get(0), program_id: r.get(1), program: r.get(2), prefix: r.get(3) };
    c.execute(
        "UPDATE api_keys SET last_used_at = now() WHERE id = $1 AND (last_used_at IS NULL OR last_used_at < now() - interval '1 minute')",
        &[&key.id],
    )
    .await?;
    Ok(Some(key))
}

/// At most once per key, user and endpoint per hour; the advisory lock stops simultaneous reads writing twice.
pub async fn audit_read(st: &St, key: &Key, user_id: i64, endpoint: &str) -> Res<()> {
    let mut c = st.pool.get().await?;
    let tx = c.transaction().await?;
    let lock = format!("program.read:{}:{user_id}:{endpoint}", key.id);
    tx.execute("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))", &[&lock]).await?;
    let detail = json!({ "key_id": key.id, "prefix": key.prefix, "program": key.program, "endpoint": endpoint });
    tx.execute(
        "INSERT INTO audit_log (actor_id, action, target_user_id, detail)
         SELECT NULL::bigint, 'program.read', $1::bigint, $2::jsonb
         WHERE NOT EXISTS (
             SELECT 1 FROM audit_log WHERE action = 'program.read' AND target_user_id = $1::bigint
               AND at > now() - interval '1 hour' AND detail->>'key_id' = $3 AND detail->>'endpoint' = $4)",
        &[&user_id, &detail, &key.id.to_string(), &endpoint],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        for ok in ["ab", "stardance", "ysws-2026", "1st"] {
            assert!(check_slug(ok).is_ok(), "{ok}");
        }
        for bad in ["a", "-ab", "Ab", "a_b", "a b", "x".repeat(41).as_str()] {
            assert!(check_slug(bad).is_err(), "{bad}");
        }
    }
}
