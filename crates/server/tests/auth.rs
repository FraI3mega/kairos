mod common;

use axum::http::{header, StatusCode};
use common::{query_of, TestApp};
use kairos_server::auth::{session_cookie, set_cookie};
use kairos_server::config::Config;
use kairos_server::db;

#[tokio::test]
async fn migrations_are_idempotent() {
    let app = TestApp::new().await;
    assert!(db::migrate(&app.st.pool).await.unwrap().is_empty());
    assert!(db::migrate(&app.st.pool).await.unwrap().is_empty());
    app.finish().await;
}

#[tokio::test]
async fn mock_oidc_sign_in_end_to_end() {
    let app = TestApp::new().await;
    let mut b = app.browser();
    b.sign_in("Ada Lovelace", "ada@example.com").await;

    let c = app.st.pool.get().await.unwrap();
    let row = c
        .query_one("SELECT username, display_name, email, slack_id, verification_status, ysws_eligible, hca_sub FROM users", &[])
        .await
        .unwrap();
    assert_eq!(row.get::<_, String>(0), "ada");
    assert_eq!(row.get::<_, Option<String>>(1).as_deref(), Some("Ada"));
    assert_eq!(row.get::<_, Option<String>>(2).as_deref(), Some("ada@example.com"));
    assert_eq!(row.get::<_, Option<String>>(3).as_deref(), Some("U0TEST"));
    assert_eq!(row.get::<_, Option<String>>(4).as_deref(), Some("verified"));
    assert!(row.get::<_, bool>(5));
    assert!(row.get::<_, String>(6).starts_with("ident!"));
    assert_eq!(c.query_one("SELECT count(*) FROM auth_flows WHERE user_id IS NULL", &[]).await.unwrap().get::<_, i64>(0), 0);

    let me = b.get("/me").await;
    assert_eq!(me.status, StatusCode::OK);
    assert!(me.body.contains("ada"));

    let mut again = app.browser();
    again.sign_in("Ada Lovelace", "ada@example.com").await;
    assert_eq!(c.query_one("SELECT count(*) FROM users", &[]).await.unwrap().get::<_, i64>(0), 1);

    let csrf = b.csrf().await;
    let out = b.post("/auth/logout", &[("csrf", &csrf)]).await;
    assert_eq!(out.status, StatusCode::SEE_OTHER);
    let after = b.get("/me").await;
    assert_eq!(after.status, StatusCode::SEE_OTHER);
    assert!(after.location().starts_with("/auth/login"));
    assert_eq!(again.get("/me").await.status, StatusCode::OK);
    app.finish().await;
}

#[tokio::test]
async fn sign_in_rejects_bad_state_nonce_and_pkce() {
    let app = TestApp::new().await;

    let mut b = app.browser();
    let cb = b.authorize("Ada Lovelace", "ada@example.com").await;
    let q = query_of(&cb);
    let forged = cb.replace(&format!("state={}", q["state"]), "state=forged");
    let res = b.get(&forged).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);

    let mut b = app.browser();
    let cb = b.authorize("Ada Lovelace", "ada@example.com").await;
    app.exec("UPDATE auth_flows SET nonce = 'tampered'").await;
    let res = b.get(&cb).await;
    assert_eq!(res.status, StatusCode::BAD_GATEWAY, "{}", res.body);

    let mut b = app.browser();
    let cb = b.authorize("Ada Lovelace", "ada@example.com").await;
    app.exec("UPDATE auth_flows SET verifier = 'not-the-verifier-that-made-the-challenge-xxxxxxxxxxx'").await;
    let res = b.get(&cb).await;
    assert_eq!(res.status, StatusCode::BAD_GATEWAY, "{}", res.body);

    let mut b = app.browser();
    let cb = b.authorize("Ada Lovelace", "ada@example.com").await;
    b.get(&cb).await;
    let replay = b.get(&cb).await;
    assert_eq!(replay.status, StatusCode::BAD_REQUEST, "{}", replay.body);

    assert_eq!(app.count("SELECT count(*) FROM web_sessions WHERE user_id <> $1", 0).await, 1);
    assert_eq!(app.count("SELECT count(*) FROM users WHERE id <> $1", 0).await, 1);
    app.finish().await;
}

#[tokio::test]
async fn a_callback_the_browser_asked_for_twice_signs_in() {
    let app = TestApp::new().await;
    let mut b = app.browser();
    let cb = b.authorize("Ada Lovelace", "ada@example.com").await;
    let dropped = b.jar.clone();
    assert_eq!(b.get(&cb).await.status, StatusCode::SEE_OTHER);

    b.jar = dropped.clone();
    assert_eq!(b.get("/me").await.status, StatusCode::SEE_OTHER, "the first answer never reached the browser");
    let again = b.get(&cb).await;
    assert_eq!(again.status, StatusCode::SEE_OTHER, "{}", again.body);
    assert_eq!(again.location(), "/me");
    assert_eq!(b.get("/me").await.status, StatusCode::OK);
    assert_eq!(app.count("SELECT count(*) FROM users WHERE id <> $1", 0).await, 1);

    b.jar = dropped.clone();
    let third = b.get(&cb).await;
    assert_eq!(third.status, StatusCode::BAD_REQUEST, "one repeat only: {}", third.body);

    let mut late = app.browser();
    let cb = late.authorize("Bea Hacker", "bea@example.com").await;
    let dropped = late.jar.clone();
    assert_eq!(late.get(&cb).await.status, StatusCode::SEE_OTHER);
    app.exec("UPDATE auth_flows SET expires_at = now() - interval '1 second'").await;
    late.jar = dropped;
    let res = late.get(&cb).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "a minute later: {}", res.body);

    let mut other = app.browser();
    let theirs = other.authorize("Cy Stranger", "cy@example.com").await;
    let cb = b.authorize("Ada Lovelace", "ada@example.com").await;
    assert_eq!(b.get(&cb).await.status, StatusCode::SEE_OTHER);
    let stolen = other.get(&cb).await;
    assert_eq!(stolen.status, StatusCode::BAD_REQUEST, "another browser with the same link: {}", stolen.body);
    assert_eq!(other.get(&theirs).await.status, StatusCode::SEE_OTHER);
    app.finish().await;
}

#[tokio::test]
async fn cancelled_sign_in_creates_nothing() {
    let app = TestApp::new().await;
    let mut b = app.browser();
    let login = b.get("/auth/login").await;
    let q = query_of(&login.location());
    let mut pairs: Vec<(&str, &str)> = q.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    pairs.extend([("action", "deny"), ("name", ""), ("email", ""), ("slack_id", ""), ("verification_status", "verified")]);
    let denied = b.post("/mock-auth/oauth/authorize", &pairs).await;
    assert!(denied.location().contains("error=access_denied"), "{}", denied.location());
    let res = b.get(&denied.location()).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert_eq!(app.count("SELECT count(*) FROM users WHERE id <> $1", 0).await, 0);
    app.finish().await;
}

#[tokio::test]
async fn session_cookie_flags() {
    let app = TestApp::new().await;
    let mut b = app.browser();
    let cb = b.authorize("Ada Lovelace", "ada@example.com").await;
    let res = b.get(&cb).await;
    let cookie = res.cookies().into_iter().find(|c| c.starts_with("kairos_session=")).expect("session cookie");
    for flag in ["HttpOnly", "SameSite=Lax", "Path=/", "Max-Age=2592000"] {
        assert!(cookie.contains(flag), "{cookie}");
    }
    assert!(!cookie.contains("Secure"), "plain http test server: {cookie}");

    let vars = |k: &str| match k {
        "DATABASE_URL" => Some("postgres://localhost/kairos".to_string()),
        "PUBLIC_URL" => Some("https://kairos.example.com".to_string()),
        "HCA_CLIENT_ID" => Some("id".to_string()),
        "HCA_CLIENT_SECRET" => Some("secret".to_string()),
        _ => None,
    };
    let https = Config::from_vars(vars).unwrap();
    assert_eq!(session_cookie(&https), "__Host-kairos_session");
    let c = set_cookie(&https, session_cookie(&https), "v", 60);
    assert!(c.starts_with("__Host-kairos_session=v; Path=/;") && c.contains("Secure") && c.contains("HttpOnly"), "{c}");
    app.finish().await;
}

#[tokio::test]
async fn cross_origin_and_bad_csrf_posts_are_rejected() {
    let app = TestApp::new().await;
    let mut b = app.browser();
    b.sign_in("Ada Lovelace", "ada@example.com").await;
    let csrf = b.csrf().await;
    let form = common::form(&[("csrf", &csrf), ("username", "evil"), ("timezone", "UTC")]);

    let none = b.request(axum::http::Method::POST, "/settings/profile", Some(form.clone()), None).await;
    assert_eq!(none.status, StatusCode::FORBIDDEN);
    let other = b.request(axum::http::Method::POST, "/settings/profile", Some(form.clone()), Some("https://evil.example")).await;
    assert_eq!(other.status, StatusCode::FORBIDDEN);
    let bad = b.post("/settings/profile", &[("csrf", "00"), ("username", "evil"), ("timezone", "UTC")]).await;
    assert_eq!(bad.status, StatusCode::FORBIDDEN);
    let mut stranger = app.browser();
    stranger.sign_in("Bea Hacker", "bea@example.com").await;
    let theirs = stranger.post("/settings/profile", &[("csrf", &csrf), ("username", "evil"), ("timezone", "UTC")]).await;
    assert_eq!(theirs.status, StatusCode::FORBIDDEN);
    assert_eq!(app.count("SELECT count(*) FROM users WHERE username = 'evil' AND id <> $1", 0).await, 0);

    let ok = b.post("/settings/profile", &[("csrf", &csrf), ("username", "ada2"), ("timezone", "UTC")]).await;
    assert_eq!(ok.status, StatusCode::SEE_OTHER, "{}", ok.body);
    app.finish().await;
}

#[tokio::test]
async fn auth_routes_are_rate_limited() {
    let app = TestApp::new().await;
    let mut b = app.browser();
    let mut limited = None;
    for _ in 0..200 {
        let res = b.get("/auth/login").await;
        if res.status == StatusCode::TOO_MANY_REQUESTS {
            limited = Some(res);
            break;
        }
        assert_eq!(res.status, StatusCode::SEE_OTHER);
    }
    let res = limited.expect("auth login was never rate limited");
    let retry: u64 = res.headers.get(header::RETRY_AFTER).unwrap().to_str().unwrap().parse().unwrap();
    assert!(retry >= 1);
    app.finish().await;
}

#[tokio::test]
async fn csp_nonce_matches_script_tags() {
    let app = TestApp::new().await;
    let mut b = app.browser();
    let mut seen = Vec::new();
    for path in ["/", "/settings", "/me"] {
        if path == "/settings" {
            b.sign_in("Ada Lovelace", "ada@example.com").await;
        }
        let res = b.get(path).await;
        assert_eq!(res.status, StatusCode::OK, "{path}");
        let csp = res.headers.get(header::CONTENT_SECURITY_POLICY).unwrap().to_str().unwrap().to_string();
        let nonce = csp.split("'nonce-").nth(1).unwrap().split('\'').next().unwrap().to_string();
        assert!(nonce.len() >= 16);
        let scripts = res.body.matches("<script").count();
        assert!(scripts > 0, "{path}");
        assert_eq!(res.body.matches(&format!("nonce=\"{nonce}\"")).count(), scripts, "{path}");
        assert!(csp.contains("object-src 'none'") && csp.contains("frame-ancestors 'none'"));
        assert_eq!(res.headers.get("x-content-type-options").unwrap(), "nosniff");
        assert!(!seen.contains(&nonce), "nonce reused");
        seen.push(nonce);
    }
    app.finish().await;
}

#[tokio::test]
async fn more_sign_ins_at_once_than_pool_connections_all_finish() {
    let app = TestApp::new().await;
    let people: Vec<(String, String)> = (0..24).map(|i| (format!("Person {i}"), format!("p{i}@example.com"))).collect();
    let mut browsers: Vec<_> = people.iter().map(|_| app.browser()).collect();
    let all = futures_util::future::join_all(browsers.iter_mut().zip(&people).map(|(b, (name, email))| b.sign_in(name, email)));
    tokio::time::timeout(std::time::Duration::from_secs(30), all).await.expect("concurrent sign-ins finished");
    app.finish().await;
}
