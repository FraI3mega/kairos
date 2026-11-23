mod common;

use std::time::Duration;

use axum::http::{header, StatusCode};
use common::{day_seconds, upload, user_id, Chain, Events, TestApp};
use kairos_server::util::{now, short};

const WAIT: Duration = Duration::from_secs(5);

#[tokio::test]
async fn the_stream_needs_a_session() {
    let app = TestApp::new().await;
    let mut b = app.browser();
    let res = b.get("/me/live").await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    app.finish().await;
}

#[tokio::test]
async fn an_upload_pushes_fresh_totals_to_open_dashboards() {
    let app = TestApp::new().await;
    let mut b = app.browser();
    b.sign_in("Ada Lovelace", "ada@example.com").await;
    let token = b.pair("laptop").await;

    let res = b.open("/me/live").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()[header::CONTENT_TYPE], "text/event-stream");
    assert_eq!(res.headers()[header::CACHE_CONTROL], "no-store");
    assert!(res.headers()[header::CONTENT_SECURITY_POLICY].to_str().unwrap().contains("script-src 'self' 'nonce-"));
    let mut events = Events::new(res);

    let (name, data) = events.next(WAIT).await.expect("a snapshot on connect");
    assert_eq!(name, "snapshot");
    let first: serde_json::Value = serde_json::from_str(&data).unwrap();
    assert_eq!(first["week"], "0m");
    assert_eq!(first["bars"].as_array().unwrap().len(), 7);

    let t = now();
    let mut chain = Chain::default();
    chain.editing("ada/site", t - 1800.0, t - 300.0);
    assert_eq!(upload(&app, &token, chain.take()).await.status, StatusCode::OK);

    let pushed = events.json(WAIT).await.expect("a snapshot after the upload");
    let uid = user_id(&app, "ada").await;
    let total: f64 = day_seconds(&app, uid).await.iter().filter(|r| r.2 != "agent").map(|r| r.3).sum();
    assert!(total >= 1400.0, "{total}");
    assert_eq!(pushed["week"], short(total));
    app.finish().await;
}

#[tokio::test]
async fn other_users_do_not_see_the_update() {
    let app = TestApp::new().await;
    let mut ada = app.browser();
    ada.sign_in("Ada Lovelace", "ada@example.com").await;
    let token = ada.pair("laptop").await;
    let mut bob = app.browser();
    bob.sign_in("Bob Builder", "bob@example.com").await;
    let mut bobs = Events::new(bob.open("/me/live").await);
    assert!(bobs.next(WAIT).await.is_some());

    let t = now();
    let mut chain = Chain::default();
    chain.editing("ada/site", t - 1800.0, t - 300.0);
    assert_eq!(upload(&app, &token, chain.take()).await.status, StatusCode::OK);
    assert!(bobs.next(Duration::from_millis(600)).await.is_none());
    app.finish().await;
}

#[tokio::test]
async fn a_user_can_keep_three_streams_open() {
    let app = TestApp::new().await;
    let mut b = app.browser();
    b.sign_in("Ada Lovelace", "ada@example.com").await;
    let uid = user_id(&app, "ada").await;

    let mut open = Vec::new();
    for _ in 0..3 {
        let res = b.open("/me/live").await;
        assert_eq!(res.status(), StatusCode::OK);
        open.push(res);
    }
    assert_eq!(app.st.live.streams(uid), 3);
    let fourth = b.open("/me/live").await;
    assert_eq!(fourth.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(app.st.live.streams(uid), 3);

    let mut other = app.browser();
    other.sign_in("Bob Builder", "bob@example.com").await;
    assert_eq!(other.open("/me/live").await.status(), StatusCode::OK);

    drop(open.pop());
    assert_eq!(app.st.live.streams(uid), 2);
    assert_eq!(b.open("/me/live").await.status(), StatusCode::OK);
    drop(open);
    drop(fourth);
    app.finish().await;
}
