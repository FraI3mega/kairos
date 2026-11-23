mod common;

use common::TestApp;
use kairos_server::programs;

async fn actions(app: &TestApp) -> Vec<String> {
    let c = app.st.pool.get().await.unwrap();
    c.query("SELECT action FROM audit_log WHERE action LIKE 'program.%' ORDER BY id", &[]).await.unwrap().iter().map(|r| r.get(0)).collect()
}

#[tokio::test]
async fn programs_and_keys_are_managed_and_audited() {
    let app = TestApp::new().await;
    let pool = &app.st.pool;
    assert!(programs::create(pool, None, "A", "Bad slug", "test").await.is_err());
    assert!(programs::create(pool, None, "ok-slug", "  ", "test").await.is_err());
    programs::create(pool, None, "stardance", "Stardance", "test").await.unwrap();
    assert!(programs::create(pool, None, "stardance", "Again", "test").await.unwrap_err().to_string().contains("already exists"));
    assert!(programs::new_key(pool, None, "nope", None, "test").await.unwrap_err().to_string().contains("no program"));

    let (a, pa) = programs::new_key(pool, None, "stardance", Some("site"), "test").await.unwrap();
    let (b, pb) = programs::new_key(pool, None, "stardance", None, "test").await.unwrap();
    assert!(a.starts_with("kp_") && a.len() > 40 && a != b);
    assert_eq!((pa.as_str(), pb.as_str()), (&a[..11], &b[..11]));

    let c = pool.get().await.unwrap();
    let leaked: i64 = c.query_one("SELECT count(*) FROM api_keys WHERE token_hash = $1 OR prefix = $1", &[&a]).await.unwrap().get(0);
    assert_eq!(leaked, 0, "the key is never stored");
    let logged: i64 = c.query_one("SELECT count(*) FROM audit_log WHERE detail::text LIKE '%' || $1 || '%'", &[&a]).await.unwrap().get(0);
    assert_eq!(logged, 0, "nor logged");

    assert!(programs::key_by_token(&app.st, &a).await.unwrap().is_some());
    assert!(programs::key_by_token(&app.st, "kp_nope").await.unwrap().is_none());
    assert!(programs::key_by_token(&app.st, "not-a-program-key").await.unwrap().is_none());

    for bad in ["", "kp_", "kp_abc", "abcdefghijk"] {
        assert!(programs::revoke(pool, None, bad, "test").await.is_err(), "{bad:?}");
    }
    assert!(programs::revoke(pool, None, "kp_zzzzzzzz", "test").await.unwrap_err().to_string().contains("no live"));
    let (slug, full) = programs::revoke(pool, None, &pa[..7], "test").await.unwrap();
    assert_eq!((slug.as_str(), full.as_str()), ("stardance", pa.as_str()));
    assert!(programs::key_by_token(&app.st, &a).await.unwrap().is_none());
    assert!(programs::key_by_token(&app.st, &b).await.unwrap().is_some(), "other keys keep working");
    assert!(programs::revoke(pool, None, &pa, "test").await.is_err(), "already revoked");

    let list = programs::list(pool).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!((list[0].slug.as_str(), list[0].name.as_str()), ("stardance", "Stardance"));
    assert_eq!(list[0].keys.len(), 2);
    assert_eq!((list[0].keys[0].name.as_deref(), list[0].keys[0].revoked_at.is_some()), (Some("site"), true));
    assert_eq!((list[0].keys[1].name.clone(), list[0].keys[1].revoked_at.is_some(), list[0].keys[1].last_used_at.is_some()), (None, false, true));

    assert_eq!(actions(&app).await, ["program.create", "program.key_create", "program.key_create", "program.key_revoke"]);
    app.finish().await;
}
