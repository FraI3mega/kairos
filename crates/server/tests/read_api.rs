mod common;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use base64::Engine;
use common::{day_seconds, upload, user_id, Chain, Resp, TestApp};
use kairos_server::{accounts, programs};
use serde_json::{json, Value};

const D: f64 = 1_780_272_000.0;
const WINDOW: &str = "start_date=2026-06-01&end_date=2026-06-03";

async fn user(app: &TestApp, name: &str) -> i64 {
    let c = app.st.pool.get().await.unwrap();
    c.query_one("INSERT INTO users (hca_sub, username) VALUES ($1, $1) RETURNING id", &[&name]).await.unwrap().get(0)
}

async fn device(app: &TestApp, user_id: i64, name: &str) -> i64 {
    let c = app.st.pool.get().await.unwrap();
    c.query_one(
        "INSERT INTO devices (user_id, token_hash, name, os) VALUES ($1, $2, $3, 'linux') RETURNING id",
        &[&user_id, &format!("{user_id}-{name}"), &name],
    )
    .await
    .unwrap()
    .get(0)
}

async fn span(app: &TestApp, uid: i64, dev: i64, start: f64, secs: f64, project: Option<&str>, class: &str) {
    let c = app.st.pool.get().await.unwrap();
    c.execute(
        "INSERT INTO spans (device_id, user_id, start_ts, end_ts, project, class, rule) VALUES ($1, $2, $3, $4, $5, $6, 'credit_v5')",
        &[&dev, &uid, &(D + start), &(D + start + secs), &project, &class],
    )
    .await
    .unwrap();
}

async fn agent(app: &TestApp, uid: i64, dev: i64, start: f64, secs: f64, project: Option<&str>) {
    let c = app.st.pool.get().await.unwrap();
    c.execute(
        "INSERT INTO agent_spans (device_id, user_id, start_ts, end_ts, project) VALUES ($1, $2, $3, $4, $5)",
        &[&dev, &uid, &(D + start), &(D + start + secs), &project],
    )
    .await
    .unwrap();
}

async fn beat(app: &TestApp, uid: i64, at: f64, project: &str, language: Option<&str>, category: Option<&str>) {
    let c = app.st.pool.get().await.unwrap();
    c.execute(
        "INSERT INTO heartbeats (user_id, time, entity_hash, project, language, editor, category) VALUES ($1, $2, $3, $4, $5, 'vscode', $6)",
        &[&uid, &(D + at), &format!("e{at}"), &project, &language, &category],
    )
    .await
    .unwrap();
}

async fn day(app: &TestApp, uid: i64, days_ago: i32, class: &str, secs: f64) {
    let c = app.st.pool.get().await.unwrap();
    c.execute(
        "INSERT INTO user_days (user_id, day, project, class, seconds) VALUES ($1, now()::date - $2::int, 'o/p', $3, $4)",
        &[&uid, &days_ago, &class, &secs],
    )
    .await
    .unwrap();
}

async fn get(app: &TestApp, path: &str, key: Option<&str>) -> Resp {
    app.api(Method::GET, path, None, key).await
}

async fn program_key(app: &TestApp, slug: &str) -> String {
    programs::create(&app.st.pool, None, slug, "A program", "test").await.unwrap();
    programs::new_key(&app.st.pool, None, slug, Some("site"), "test").await.unwrap().0
}

async fn audit_rows(app: &TestApp, action: &str) -> Vec<(Option<i64>, Option<i64>, Value)> {
    let c = app.st.pool.get().await.unwrap();
    c.query("SELECT actor_id, target_user_id, detail FROM audit_log WHERE action = $1 ORDER BY id", &[&action])
        .await
        .unwrap()
        .iter()
        .map(|r| (r.get(0), r.get(1), r.get(2)))
        .collect()
}

fn total(r: &Resp) -> Value {
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    r.json()["total_seconds"].clone()
}

#[tokio::test]
async fn stats_have_hackatimes_shape() {
    let app = TestApp::new().await;
    let ada = user(&app, "ada").await;
    app.exec("UPDATE users SET trust = 'trusted', display_name = 'Ada L' WHERE id = 1").await;
    let d1 = device(&app, ada, "laptop").await;
    let d2 = device(&app, ada, "desktop").await;
    span(&app, ada, d1, 3600.0, 1800.0, Some("acme/app"), "editing").await;
    span(&app, ada, d1, 10_000.0, 600.0, Some("acme/app"), "typing").await;
    span(&app, ada, d2, 10_300.0, 600.0, Some("acme/app"), "reading").await;
    span(&app, ada, d1, 20_000.0, 300.0, Some("acme/web"), "terminal").await;
    span(&app, ada, d1, 30_000.0, 120.0, Some("acme/web"), "testing").await;
    span(&app, ada, d1, 40_000.0, 1000.0, None, "editing").await;
    agent(&app, ada, d1, 5000.0, 500.0, Some("acme/app")).await;
    agent(&app, ada, d1, 6000.0, 100.0, None).await;
    for (at, lang) in [(3600.0, "Rust"), (3660.0, "Rust"), (3720.0, "Rust"), (10_000.0, "Python"), (10_100.0, "Python")] {
        beat(&app, ada, at, "acme/app", Some(lang), Some("coding")).await;
    }
    day(&app, ada, 0, "editing", 1000.0).await;
    day(&app, ada, 1, "editing", 1000.0).await;
    day(&app, ada, 2, "agent", 99_999.0).await;

    let r = get(&app, &format!("/api/v1/users/ada/stats?{WINDOW}&features=languages,projects&filter_by_project=acme/app"), None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let v = r.json();
    let d = &v["data"];
    assert_eq!(d["username"], "Ada L");
    assert_eq!(d["user_id"], ada.to_string());
    assert_eq!(d["status"], "ok");
    assert_eq!((d["is_coding_activity_visible"].clone(), d["is_other_usage_visible"].clone()), (json!(true), json!(true)));
    assert_eq!((d["range"].clone(), d["human_readable_range"].clone()), (json!("all_time"), json!("All Time")));
    assert_eq!((d["start"].clone(), d["end"].clone()), (json!("2026-06-01T00:00:00Z"), json!("2026-06-03T00:00:00Z")));
    assert_eq!(d["total_seconds"], 2700);
    assert_eq!(d["human_readable_total"], "45m");
    assert_eq!(d["daily_average"], 1350, "an integer, total over whole days");
    assert_eq!(d["human_readable_daily_average"], "22m 30s");
    assert_eq!(d["streak"], 2);
    assert_eq!(v["trust_factor"], json!({ "trust_level": "green", "trust_value": 2 }));
    assert_eq!(d["unique_total_seconds"], 220, "gaps over two minutes count for nothing");
    let langs = d["languages"].as_array().unwrap();
    assert_eq!(langs.len(), 2);
    assert_eq!(langs[0], json!({
        "name": "Rust", "total_seconds": 240, "text": "4m", "hours": 0, "minutes": 4, "percent": 8.89,
        "digital": "00:04:00", "color": "#dea584"
    }));
    assert_eq!(langs[1]["name"], "Python");
    assert_eq!(langs[1]["total_seconds"], 220);
    let projects = d["projects"].as_array().unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0], json!({
        "name": "acme/app", "total_seconds": 2700, "text": "45m", "hours": 0, "minutes": 45, "percent": 100.0, "digital": "00:45:00"
    }));
    assert!(projects[0].get("color").is_none());

    let all = get(&app, &format!("/api/v1/users/ada/stats?{WINDOW}"), None).await.json();
    let d = &all["data"];
    assert_eq!(d["total_seconds"], 3120);
    assert!(d.get("projects").is_none(), "only languages by default");
    assert!(d.get("unique_total_seconds").is_none());
    assert_eq!(d["kairos"]["focus_seconds"], json!({ "editing": 1800, "typing": 600, "reading": 300, "terminal": 300, "testing": 120 }));
    assert_eq!(d["kairos"]["categories"], json!({ "ai coding": 900, "coding": 2100, "running tests": 120, "researching": 0 }));
    assert_eq!(d["kairos"]["agent_seconds"], 500, "agent runtime is reported, never credited");
    let sum: i64 = d["kairos"]["focus_seconds"].as_object().unwrap().values().map(|v| v.as_i64().unwrap()).sum();
    assert_eq!(sum, 3120);

    let default = get(&app, "/api/v1/users/ada/stats", None).await.json();
    assert_eq!(default["data"]["total_seconds"], 3120, "default window is ten years back to the end of today");
    assert!(default["data"]["end"].as_str().unwrap().ends_with("T23:59:59Z"));
    app.finish().await;
}

#[tokio::test]
async fn identifiers_resolve_in_hackatimes_order() {
    let app = TestApp::new().await;
    for (id, name, slack, hca) in [
        (101, "alpha", Some("202"), "sub-alpha"),
        (202, "beta", Some("U202"), "sub-beta"),
        (303, "gamma", None, "delta"),
        (404, "delta", None, "sub-delta"),
        (505, "epsilon", Some("dup"), "sub-epsilon"),
        (606, "zeta", None, "dup"),
    ] {
        let c = app.st.pool.get().await.unwrap();
        c.execute("INSERT INTO users (id, hca_sub, username, slack_id) VALUES ($1, $2, $3, $4)", &[&(id as i64), &hca, &name, &slack]).await.unwrap();
    }
    let who = |ident: &'static str| {
        let app = &app;
        async move {
            let r = get(app, &format!("/api/v1/users/{ident}/stats?features="), None).await;
            (r.status, r.json()["data"]["user_id"].as_str().map(String::from))
        }
    };
    assert_eq!(who("202").await.1.as_deref(), Some("202"), "a numeric id beats a Slack id that looks the same");
    assert_eq!(who("U202").await.1.as_deref(), Some("202"), "Slack id");
    assert_eq!(who("dup").await.1.as_deref(), Some("505"), "Slack id beats Hack Club Auth subject");
    assert_eq!(who("delta").await.1.as_deref(), Some("303"), "Hack Club Auth subject beats username");
    assert_eq!(who("sub-delta").await.1.as_deref(), Some("404"));
    assert_eq!(who("ALPHA").await.1.as_deref(), Some("101"), "usernames match case-insensitively");
    assert_eq!(who("epsilon").await.1.as_deref(), Some("505"));
    let missing = get(&app, "/api/v1/users/nobody/stats", None).await;
    assert_eq!((missing.status, missing.json()), (StatusCode::NOT_FOUND, json!({ "error": "User not found" })));
    assert_eq!(get(&app, "/api/v1/users/99999999999999999999/stats", None).await.status, StatusCode::NOT_FOUND);

    let key = accounts::new_editor_key(&app.st, 101).await.unwrap();
    let mine = get(&app, "/api/v1/users/my/stats?features=", Some(&key)).await;
    assert_eq!(mine.json()["data"]["user_id"], "101");
    let anon = get(&app, "/api/v1/users/my/stats", None).await;
    assert_eq!((anon.status, anon.json()), (StatusCode::NOT_FOUND, json!({ "error": "User not found" })));
    app.finish().await;
}

#[tokio::test]
async fn windows_are_end_exclusive_and_dates_parse_like_rails() {
    let app = TestApp::new().await;
    let u = user(&app, "edge").await;
    let dev = device(&app, u, "laptop").await;
    span(&app, u, dev, 100.0, 100.0, Some("o/p"), "editing").await;
    let q = |extra: &str| format!("/api/v1/users/edge/stats?total_seconds=true&{extra}");

    assert_eq!(total(&get(&app, &q("start_date=2026-06-01"), None).await), 100);
    assert_eq!(total(&get(&app, &q("start_date=2026-06-01T00:02:30Z"), None).await), 50, "credit is clipped to the window");
    assert_eq!(total(&get(&app, &q("start_date=2026-06-01T02:02:30%2B02:00"), None).await), 50, "offsets are honoured");
    assert_eq!(total(&get(&app, &q("start_date=2026-06-01%2000:02:30"), None).await), 50, "a space works as the separator, as UTC");
    assert_eq!(total(&get(&app, &q("end_date=2026-06-01T00:01:40Z"), None).await), 0, "the end of the window is exclusive");
    assert_eq!(total(&get(&app, &q("end_date=2026-06-01T00:01:41Z"), None).await), 1);
    assert_eq!(total(&get(&app, &q("start_date=2026-06-02"), None).await), 0);
    assert_eq!(total(&get(&app, &q("end_date=2026-06-01"), None).await), 0, "a bare end date is midnight, not the end of that day");
    assert_eq!(total(&get(&app, &q("start_date=&end_date="), None).await), 100, "blank means default");

    for (param, path) in [("start_date", q("start_date=nope")), ("end_date", q("end_date=2026-13-45")), ("start_date", "/api/v1/users/edge/stats?start_date=x".to_string())] {
        let r = get(&app, &path, None).await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{path}");
        assert_eq!(r.json(), json!({ "error": format!("Invalid {param}") }));
    }
    let key = accounts::new_editor_key(&app.st, u).await.unwrap();
    let r = get(&app, "/api/v1/users/edge/heartbeats/spans?end_date=x", Some(&key)).await;
    assert_eq!((r.status, r.json()), (StatusCode::UNPROCESSABLE_ENTITY, json!({ "error": "Invalid end_date" })));

    // test_param floors the window to whole seconds like Hackatime's service does, so 0.6 s of credit is lost.
    assert_eq!(total(&get(&app, &q("end_date=2026-06-01T00:01:40.6Z"), None).await), 1);
    assert_eq!(total(&get(&app, &q("end_date=2026-06-01T00:01:40.6Z&test_param=true"), None).await), 0);
    app.finish().await;
}

#[tokio::test]
async fn filters_and_flags() {
    let app = TestApp::new().await;
    let u = user(&app, "fil").await;
    let dev = device(&app, u, "laptop").await;
    span(&app, u, dev, 0.0, 600.0, Some("a"), "editing").await;
    span(&app, u, dev, 1000.0, 300.0, Some("b"), "typing").await;
    span(&app, u, dev, 2000.0, 200.0, Some("a"), "testing").await;
    beat(&app, u, 0.0, "a", Some("Rust"), None).await;
    beat(&app, u, 60.0, "a", Some("Rust"), Some("coding")).await;
    beat(&app, u, 1000.0, "a", Some("Rust"), Some("coding")).await;
    beat(&app, u, 1100.0, "a", Some("Rust"), Some("debugging")).await;
    beat(&app, u, 50.0, "b", Some("Go"), Some("coding")).await;
    let s = |extra: &str| format!("/api/v1/users/fil/stats?{WINDOW}&{extra}");
    let data = |extra: &str| {
        let app = &app;
        let path = s(extra);
        async move { get(app, &path, None).await.json()["data"].clone() }
    };

    assert_eq!(data("filter_by_project=a").await["total_seconds"], 800);
    assert_eq!(data("filter_by_project=a,b").await["total_seconds"], 1100);
    assert_eq!(data("filter_by_project=zzz").await["total_seconds"], 0);
    assert_eq!(data("no_ai_coding=true").await["total_seconds"], 800, "no_ai_coding drops typing and reading time");
    assert_eq!(data("no_ai_coding=false").await["total_seconds"], 1100);
    assert_eq!(data("filter_by_category=ai%20coding").await["total_seconds"], 1100, "filter_by_category only applies to total_seconds");

    let only = |extra: &str| {
        let app = &app;
        let path = s(&format!("total_seconds=true&{extra}"));
        async move { get(app, &path, None).await }
    };
    let r = only("").await;
    assert_eq!(r.json(), json!({ "total_seconds": 1100 }), "just the one key");
    assert_eq!(total(&only("filter_by_category=ai%20coding").await), 300);
    assert_eq!(total(&only("filter_by_category=coding,running%20tests").await), 800);
    assert_eq!(total(&only("filter_by_category=researching").await), 0);
    assert_eq!(total(&only("filter_by_category=ai%20coding&filter_by_project=a").await), 0);
    assert_eq!(total(&only("filter_by_category=ai%20coding&test_param=true").await), 1100, "strict mode ignores filter_by_category");
    assert_eq!(total(&only("filter_by_category=ai%20coding&no_ai_coding=true").await), 0);
    assert_eq!(total(&only("no_ai_coding=true").await), 800);
    assert_eq!(total(&only("boundary_aware=true").await), 1100);
    assert_eq!(total(&only("boundary_aware=true&no_ai_coding=true&test_param=true").await), 800);
    assert_eq!(total(&only("test_param=true&filter_by_project=b").await), 300);

    let d = data("features=projects").await;
    assert!(d.get("languages").is_none());
    let names: Vec<_> = d["projects"].as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap().to_string()).collect();
    assert_eq!(names, ["a", "b"], "most time first");
    assert_eq!(data("features=projects&limit=1").await["projects"].as_array().unwrap().len(), 1);
    assert_eq!(data("features=projects&limit=0").await["projects"].as_array().unwrap().len(), 2, "limit 0 is unlimited");
    assert_eq!(data("features=projects&limit=abc").await["projects"].as_array().unwrap().len(), 2, "to_i of junk is 0");
    assert_eq!(data("features=projects&limit=5x").await["projects"].as_array().unwrap().len(), 2);
    let d = data("features=").await;
    assert!(d.get("languages").is_none() && d.get("projects").is_none(), "an empty features list turns both off");
    let d = data("features=projects,languages,editors").await;
    assert!(d.get("languages").is_some() && d.get("projects").is_some() && d.get("editors").is_none());
    let d = data("features=languages&limit=1").await;
    assert_eq!(d["languages"].as_array().unwrap().len(), 1);
    assert_eq!(d["languages"][0]["name"], "Rust");

    assert!(data("features=projects").await.get("unique_total_seconds").is_none(), "needs filter_by_project too");
    assert!(data("features=languages&filter_by_project=a").await.get("unique_total_seconds").is_none());
    assert_eq!(data("features=projects&filter_by_project=a").await["unique_total_seconds"], 60, "only the 60 s gap counts: 940 s is too long and the debugging heartbeat is not coding");
    assert_eq!(data("features=projects&filter_by_project=a,b").await["unique_total_seconds"], 60);
    app.finish().await;
}

#[tokio::test]
async fn private_users_need_a_key_and_hidden_projects_stay_hidden() {
    let app = TestApp::new().await;
    let ada = user(&app, "ada").await;
    let bob = user(&app, "bob").await;
    app.exec("UPDATE users SET is_public = false WHERE username = 'ada'").await;
    let dev = device(&app, ada, "laptop").await;
    span(&app, ada, dev, 0.0, 600.0, Some("open"), "editing").await;
    span(&app, ada, dev, 1000.0, 300.0, Some("secret"), "editing").await;
    app.exec("INSERT INTO projects (user_id, id, hidden, first_seen, last_seen) VALUES (1, 'secret', true, 0, 0)").await;
    let url = format!("/api/v1/users/ada/stats?{WINDOW}&total_seconds=true");

    let anon = get(&app, &url, None).await;
    assert_eq!((anon.status, anon.json()), (StatusCode::FORBIDDEN, json!({ "error": "user has disabled public stats" })));
    for path in ["projects", "projects/details", "heartbeats/spans", "project/open"] {
        assert_eq!(get(&app, &format!("/api/v1/users/ada/{path}"), None).await.status, StatusCode::FORBIDDEN, "{path}");
    }
    assert_eq!(get(&app, "/api/v1/users/ada/trust_factor", None).await.status, StatusCode::OK, "trust_factor is open to everyone");

    let own = accounts::new_editor_key(&app.st, ada).await.unwrap();
    let theirs = accounts::new_editor_key(&app.st, bob).await.unwrap();
    assert_eq!(total(&get(&app, &url, Some(&own)).await), 900, "the owner sees every project");
    let basic = base64::engine::general_purpose::STANDARD.encode(&own);
    let r = app.send(Request::builder().uri(&url).header(header::AUTHORIZATION, format!("Basic {basic}")).body(Body::empty()).unwrap()).await;
    assert_eq!(total(&r), 900, "Basic base64(key)");
    let r = app.send(Request::builder().uri(&url).header(header::AUTHORIZATION, format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{own}:")))).body(Body::empty()).unwrap()).await;
    assert_eq!(total(&r), 900, "Basic base64(key:)");
    assert_eq!(total(&get(&app, &format!("{url}&api_key={own}"), None).await), 900, "?api_key=");
    assert_eq!(get(&app, &url, Some(&theirs)).await.status, StatusCode::FORBIDDEN, "another user's key is no use");
    assert_eq!(get(&app, &url, Some("not-a-key")).await.status, StatusCode::FORBIDDEN);

    app.exec("UPDATE users SET is_public = true WHERE username = 'ada'").await;
    assert_eq!(total(&get(&app, &url, None).await), 600, "anonymous readers do not see hidden projects");
    assert_eq!(total(&get(&app, &url, Some(&own)).await), 900);
    let proj = get(&app, "/api/v1/users/ada/projects/details?start=2026-06-01&end=2026-06-03", None).await.json();
    assert_eq!(proj["projects"].as_array().unwrap().len(), 1);
    assert_eq!(get(&app, "/api/v1/users/ada/project/secret?start=2026-06-01&end=2026-06-03", None).await.status, StatusCode::NOT_FOUND);
    assert!(!get(&app, &format!("/api/v1/users/ada/stats?{WINDOW}&features=projects"), None).await.body.contains("secret"));
    app.finish().await;
}

#[tokio::test]
async fn program_keys_read_everyone_until_revoked() {
    let app = TestApp::new().await;
    let ada = user(&app, "ada").await;
    app.exec("UPDATE users SET is_public = false WHERE username = 'ada'").await;
    let dev = device(&app, ada, "laptop").await;
    span(&app, ada, dev, 0.0, 600.0, Some("open"), "editing").await;
    span(&app, ada, dev, 1000.0, 300.0, Some("secret"), "editing").await;
    app.exec("INSERT INTO projects (user_id, id, hidden, first_seen, last_seen) VALUES (1, 'secret', true, 0, 0)").await;
    let key = program_key(&app, "stardance").await;
    assert!(key.starts_with("kp_"));
    let url = format!("/api/v1/users/ada/stats?{WINDOW}&total_seconds=true");

    assert_eq!(total(&get(&app, &url, Some(&key)).await), 900, "participants join programs knowingly: private and hidden are readable");
    let basic = base64::engine::general_purpose::STANDARD.encode(&key);
    let r = app.send(Request::builder().uri(&url).header(header::AUTHORIZATION, format!("Basic {basic}")).body(Body::empty()).unwrap()).await;
    assert_eq!(total(&r), 900);
    assert_eq!(total(&get(&app, &format!("{url}&api_key={key}"), None).await), 900);
    for path in ["projects", "projects/details?start=2026-06-01&end=2026-06-03", "heartbeats/spans", "project/secret?start=2026-06-01&end=2026-06-03"] {
        assert_eq!(get(&app, &format!("/api/v1/users/ada/{path}"), Some(&key)).await.status, StatusCode::OK, "{path}");
    }
    let my = get(&app, "/api/v1/users/my/stats", Some(&key)).await;
    assert_eq!(my.status, StatusCode::NOT_FOUND, "a program has no `my`");

    let used: Option<String> = {
        let c = app.st.pool.get().await.unwrap();
        c.query_one("SELECT last_used_at::text FROM api_keys WHERE kind = 'program'", &[]).await.unwrap().get(0)
    };
    assert!(used.is_some(), "last_used_at is updated");

    let stored = {
        let c = app.st.pool.get().await.unwrap();
        c.query("SELECT token_hash FROM api_keys WHERE kind = 'program'", &[]).await.unwrap()
    };
    assert_ne!(stored[0].get::<_, String>(0), key, "only a hash is kept");

    programs::revoke(&app.st.pool, None, &key[..11], "test").await.unwrap();
    let r = get(&app, &url, Some(&key)).await;
    assert_eq!((r.status, r.json()), (StatusCode::FORBIDDEN, json!({ "error": "user has disabled public stats" })));
    app.exec("UPDATE users SET is_public = true WHERE username = 'ada'").await;
    assert_eq!(total(&get(&app, &url, Some(&key)).await), 600, "a revoked key is just an anonymous caller");
    app.finish().await;
}

#[tokio::test]
async fn program_reads_are_audited_at_most_once_per_hour() {
    let app = TestApp::new().await;
    let ada = user(&app, "ada").await;
    let bob = user(&app, "bob").await;
    app.exec("UPDATE users SET email = 'ada@example.com', slack_id = 'U0ADA' WHERE username = 'ada'").await;
    let key = program_key(&app, "stardance").await;
    let other = programs::new_key(&app.st.pool, None, "stardance", None, "test").await.unwrap().0;

    for _ in 0..3 {
        assert_eq!(get(&app, "/api/v1/users/ada/stats", Some(&key)).await.status, StatusCode::OK);
    }
    assert_eq!(audit_rows(&app, "program.read").await.len(), 1);
    assert_eq!(get(&app, "/api/v1/users/ada/projects", Some(&key)).await.status, StatusCode::OK);
    assert_eq!(get(&app, &format!("/api/v1/users/{bob}/stats"), Some(&key)).await.status, StatusCode::OK);
    assert_eq!(get(&app, "/api/v1/users/ada/stats", Some(&other)).await.status, StatusCode::OK);
    assert_eq!(get(&app, "/api/v1/users/nobody/stats", Some(&key)).await.status, StatusCode::NOT_FOUND);
    assert_eq!(get(&app, "/api/v1/users/ada/stats", None).await.status, StatusCode::OK);
    let rows = audit_rows(&app, "program.read").await;
    assert_eq!(rows.len(), 4, "per endpoint, per user and per key; missing users and anonymous reads write nothing: {rows:?}");
    let (actor, target, detail) = &rows[0];
    assert_eq!((*actor, *target), (None, Some(ada)));
    assert_eq!((detail["endpoint"].clone(), detail["program"].clone(), detail["prefix"].clone()), (json!("stats"), json!("stardance"), json!(&key[..11])));
    assert!(!rows.iter().any(|r| r.2.to_string().contains(&key)), "the key itself is never logged");

    app.exec("UPDATE audit_log SET at = now() - interval '2 hours' WHERE action = 'program.read'").await;
    assert_eq!(get(&app, "/api/v1/users/ada/stats", Some(&key)).await.status, StatusCode::OK);
    assert_eq!(audit_rows(&app, "program.read").await.len(), 5, "an hour later the same read is logged again");

    assert_eq!(get(&app, "/api/v1/users/lookup_slack_uid/U0ADA", Some(&key)).await.status, StatusCode::OK);
    assert_eq!(get(&app, "/api/v1/users/lookup_slack_uid/U0ADA", Some(&key)).await.status, StatusCode::OK);
    let rows = audit_rows(&app, "program.read").await;
    assert_eq!(rows.len(), 6);
    assert_eq!(rows[5].2["endpoint"], "lookup_slack_uid");
    app.finish().await;
}

#[tokio::test]
async fn banned_users_are_readable_but_cannot_use_their_key() {
    let app = TestApp::new().await;
    let mal = user(&app, "mal").await;
    app.exec("UPDATE users SET trust = 'banned' WHERE username = 'mal'").await;
    let dev = device(&app, mal, "laptop").await;
    span(&app, mal, dev, 0.0, 600.0, Some("o/p"), "editing").await;
    let key = accounts::new_editor_key(&app.st, mal).await.unwrap();

    let r = get(&app, &format!("/api/v1/users/mal/stats?{WINDOW}"), None).await;
    assert_eq!(r.status, StatusCode::OK, "Hackatime still serves a red user's public stats");
    assert_eq!(r.json()["trust_factor"], json!({ "trust_level": "red", "trust_value": 1 }));
    assert_eq!(get(&app, "/api/v1/users/mal/trust_factor", None).await.json(), json!({ "trust_level": "red", "trust_value": 1 }));
    assert_eq!(get(&app, "/api/v1/users/my/stats", Some(&key)).await.status, StatusCode::NOT_FOUND, "the key is refused, so `my` finds nobody");

    app.exec("UPDATE users SET is_public = false WHERE username = 'mal'").await;
    assert_eq!(get(&app, "/api/v1/users/mal/stats", Some(&key)).await.status, StatusCode::FORBIDDEN, "and it is not the owner's key either");
    let pk = program_key(&app, "stardance").await;
    assert_eq!(get(&app, "/api/v1/users/mal/stats", Some(&pk)).await.status, StatusCode::OK);
    app.finish().await;
}

#[tokio::test]
async fn trust_factor_masks_suspicion() {
    let app = TestApp::new().await;
    for (name, trust) in [("t1", "trusted"), ("t2", "unscored"), ("t3", "suspicious"), ("t4", "banned")] {
        user(&app, name).await;
        app.exec(&format!("UPDATE users SET trust = '{trust}' WHERE username = '{name}'")).await;
    }
    for (name, level, value) in [("t1", "green", 2), ("t2", "blue", 0), ("t3", "blue", 0), ("t4", "red", 1)] {
        let r = get(&app, &format!("/api/v1/users/{name}/trust_factor"), None).await;
        assert_eq!(r.json(), json!({ "trust_level": level, "trust_value": value }), "{name}");
        let s = get(&app, &format!("/api/v1/users/{name}/stats?features="), None).await.json();
        assert_eq!(s["trust_factor"], json!({ "trust_level": level, "trust_value": value }));
    }
    let r = get(&app, "/api/v1/users/2/trust_factor", None).await;
    assert_eq!(r.json()["trust_level"], "blue", "numeric ids work");
    let r = get(&app, "/api/v1/users/nobody/trust_factor", None).await;
    assert_eq!((r.status, r.json()), (StatusCode::NOT_FOUND, json!({ "error": "User not found" })));
    app.finish().await;
}

#[tokio::test]
async fn project_endpoints() {
    let app = TestApp::new().await;
    let u = user(&app, "pro").await;
    let dev = device(&app, u, "laptop").await;
    span(&app, u, dev, 100.0, 600.0, Some("acme/app"), "editing").await;
    span(&app, u, dev, 1000.0, 300.0, Some("acme/web"), "terminal").await;
    span(&app, u, dev, 5000.0, 60.0, Some("acme/hush"), "editing").await;
    for (at, lang) in [(100.0, "Rust"), (160.0, "Rust"), (220.0, "TypeScript")] {
        beat(&app, u, at, "acme/app", Some(lang), Some("coding")).await;
    }
    beat(&app, u, 9000.0, "scratch", None, None).await;
    app.exec("INSERT INTO projects (user_id, id, repo_url, first_seen, last_seen) VALUES (1, 'acme/app', 'https://github.com/acme/app', 0, 0)").await;
    app.exec("INSERT INTO projects (user_id, id, hidden, first_seen, last_seen) VALUES (1, 'acme/hush', true, 0, 0)").await;
    let w = "start=2026-06-01&end=2026-06-03";

    let r = get(&app, &format!("/api/v1/users/pro/projects?{WINDOW}"), None).await;
    assert_eq!(r.json(), json!({ "projects": ["acme/app", "acme/web", "scratch"] }), "names only, hidden left out, heartbeat-only projects included");
    assert_eq!(get(&app, "/api/v1/users/pro/projects", None).await.json(), json!({ "projects": [] }), "the default window is the last 30 days");
    assert_eq!(get(&app, "/api/v1/users/pro/projects?since=2026-06-01&until=2026-06-03", None).await.json()["projects"].as_array().unwrap().len(), 3);
    assert_eq!(get(&app, "/api/v1/users/pro/projects?since=junk&start=2026-06-01&end=2026-06-03", None).await.json()["projects"].as_array().unwrap().len(), 3, "a bad value falls through to the next name");

    let r = get(&app, &format!("/api/v1/users/pro/projects/details?{w}"), None).await;
    let rows = r.json()["projects"].as_array().unwrap().clone();
    let names: Vec<_> = rows.iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["acme/app", "acme/web", "scratch"], "most time first");
    assert_eq!(rows[0], json!({
        "name": "acme/app", "total_seconds": 600, "languages": ["Rust", "TypeScript"], "repo_url": "https://github.com/acme/app",
        "total_heartbeats": 3, "first_heartbeat": "2026-06-01T00:01:40Z", "last_heartbeat": "2026-06-01T00:11:40Z",
        "most_recent_heartbeat": "2026-06-01T00:11:40Z", "archived": false
    }));
    assert_eq!(rows[1]["repo_url"], Value::Null);
    assert_eq!((rows[1]["total_heartbeats"].clone(), rows[1]["languages"].clone()), (json!(0), json!([])));
    assert_eq!((rows[1]["first_heartbeat"].clone(), rows[1]["last_heartbeat"].clone()), (json!("2026-06-01T00:16:40Z"), json!("2026-06-01T00:21:40Z")));
    assert_eq!((rows[2]["total_seconds"].clone(), rows[2]["total_heartbeats"].clone()), (json!(0), json!(1)));

    let one = get(&app, &format!("/api/v1/users/pro/projects/details?{w}&projects=acme/web,%20scratch"), None).await.json();
    assert_eq!(one["projects"].as_array().unwrap().len(), 2);
    assert_eq!(get(&app, &format!("/api/v1/users/pro/projects/details?{w}&projects=nope"), None).await.json(), json!({ "projects": [] }));
    assert_eq!(get(&app, "/api/v1/users/pro/projects/details?projects=acme/app", None).await.json()["projects"][0]["total_seconds"], 600, "details default to the last year");

    for path in ["acme/app", "acme%2Fapp"] {
        let r = get(&app, &format!("/api/v1/users/pro/project/{path}?{w}"), None).await;
        assert_eq!(r.status, StatusCode::OK, "{path}: {}", r.body);
        assert_eq!(r.json(), rows[0], "a single object, the same row");
    }
    let r = get(&app, &format!("/api/v1/users/pro/project/nope?{w}"), None).await;
    assert_eq!((r.status, r.json()), (StatusCode::NOT_FOUND, json!({ "error": "found nuthin" })));
    assert_eq!(get(&app, &format!("/api/v1/users/pro/project/acme/hush?{w}"), None).await.status, StatusCode::NOT_FOUND);
    let pk = program_key(&app, "stardance").await;
    assert_eq!(get(&app, &format!("/api/v1/users/pro/project/acme/hush?{w}"), Some(&pk)).await.status, StatusCode::OK);
    assert_eq!(get(&app, &format!("/api/v1/users/pro/projects?{WINDOW}"), Some(&pk)).await.json()["projects"].as_array().unwrap().len(), 4);
    app.finish().await;
}

#[tokio::test]
async fn heartbeat_spans() {
    let app = TestApp::new().await;
    let u = user(&app, "sp").await;
    let d1 = device(&app, u, "laptop").await;
    let d2 = device(&app, u, "desktop").await;
    span(&app, u, d1, 100.0, 300.0, Some("a"), "editing").await;
    span(&app, u, d2, 300.0, 200.0, Some("a"), "terminal").await;
    span(&app, u, d1, 1000.0, 60.0, Some("b"), "editing").await;
    span(&app, u, d1, 2000.0, 0.4, Some("a"), "editing").await;
    span(&app, u, d1, 3000.0, 60.0, None, "editing").await;
    let path = |extra: &str| format!("/api/v1/users/sp/heartbeats/spans?{WINDOW}&{extra}");
    let key = accounts::new_editor_key(&app.st, u).await.unwrap();
    let key = Some(key.as_str());

    let anon = get(&app, &path(""), None).await;
    assert_eq!(anon.status, StatusCode::FORBIDDEN, "a public profile does not open the timeline to strangers");
    let other = accounts::new_editor_key(&app.st, user(&app, "other").await).await.unwrap();
    assert_eq!(get(&app, &path(""), Some(&other)).await.status, StatusCode::FORBIDDEN, "nor to another participant");
    let program = program_key(&app, "stardance").await;
    assert_eq!(get(&app, &path(""), Some(&program)).await.status, StatusCode::OK);

    let r = get(&app, &path(""), key).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.json(), json!({ "spans": [
        { "start_time": D + 100.0, "end_time": D + 500.0, "duration": 400 },
        { "start_time": D + 1000.0, "end_time": D + 1060.0, "duration": 60 },
    ] }), "devices are merged, empty runs and Other time are dropped");
    assert_eq!(get(&app, &path("project=b"), key).await.json()["spans"].as_array().unwrap().len(), 1);
    assert_eq!(get(&app, &path("filter_by_project=a"), key).await.json()["spans"][0]["duration"], 400);
    assert_eq!(get(&app, &path("project=b&filter_by_project=a"), key).await.json()["spans"][0]["duration"], 60, "project wins over filter_by_project");
    let clipped = get(&app, "/api/v1/users/sp/heartbeats/spans?start_date=2026-06-01T00:03:20Z&end_date=2026-06-01T00:05:00Z", key).await.json();
    assert_eq!(clipped["spans"], json!([{ "start_time": D + 200.0, "end_time": D + 300.0, "duration": 100 }]));

    assert_eq!(get(&app, "/api/v1/users/sp/spans", key).await.status, StatusCode::OK, "the old owner-only listing still works");
    assert!(get(&app, "/api/v1/users/sp/spans", key).await.json()["spans"].is_array());
    assert_eq!(get(&app, "/api/v1/users/sp/spans", None).await.status, StatusCode::FORBIDDEN);
    app.finish().await;
}

#[tokio::test]
async fn leaderboards() {
    let app = TestApp::new().await;
    let ada = user(&app, "ada").await;
    let bob = user(&app, "bob").await;
    let eve = user(&app, "eve").await;
    let mal = user(&app, "mal").await;
    app.exec("UPDATE users SET display_name = 'Ada L', avatar_url = 'https://img/ada.png' WHERE username = 'ada'").await;
    app.exec("UPDATE users SET is_public = false WHERE username = 'eve'").await;
    app.exec("UPDATE users SET trust = 'banned' WHERE username = 'mal'").await;
    day(&app, ada, 0, "typing", 1200.0).await;
    day(&app, ada, 3, "editing", 3000.0).await;
    day(&app, bob, 0, "editing", 1800.0).await;
    day(&app, bob, 0, "agent", 99_999.0).await;
    day(&app, eve, 0, "typing", 5000.0).await;
    day(&app, mal, 0, "typing", 5000.0).await;

    let daily = get(&app, "/api/v1/leaderboard", None).await;
    assert_eq!(daily.status, StatusCode::OK, "{}", daily.body);
    let v = daily.json();
    assert_eq!(v["period"], "daily");
    assert_eq!(v["date_range"], "Today");
    assert!(v["start_date"].as_str().unwrap().len() == 10 && v["generated_at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(v["entries"], json!([
        { "rank": 1, "user": { "id": bob, "username": "bob", "handle": "bob", "avatar_url": Value::Null }, "total_seconds": 1800 },
        { "rank": 2, "user": { "id": ada, "username": "Ada L", "handle": "ada", "avatar_url": "https://img/ada.png" }, "total_seconds": 1200 },
    ]));
    assert_eq!(get(&app, "/api/v1/leaderboard/daily", None).await.json()["entries"], v["entries"]);

    let weekly = get(&app, "/api/v1/leaderboard/weekly", None).await.json();
    assert_eq!(weekly["period"], "last_7_days");
    assert!(weekly["date_range"].as_str().unwrap().contains(" - "));
    let rows: Vec<_> = weekly["entries"].as_array().unwrap().iter().map(|e| (e["rank"].as_i64().unwrap(), e["user"]["handle"].as_str().unwrap(), e["total_seconds"].as_i64().unwrap())).collect();
    assert_eq!(rows, [(1, "ada", 4200), (2, "bob", 1800)]);
    assert_eq!(get(&app, "/api/v1/leaderboard/weekly?limit=1", None).await.json()["entries"].as_array().unwrap().len(), 1);
    app.finish().await;
}

#[tokio::test]
async fn lookups_need_a_program_key() {
    let app = TestApp::new().await;
    let ada = user(&app, "ada").await;
    app.exec("UPDATE users SET email = 'Ada@Example.com', slack_id = 'U0ADA' WHERE username = 'ada'").await;
    let key = program_key(&app, "stardance").await;
    let editor = accounts::new_editor_key(&app.st, ada).await.unwrap();

    let r = get(&app, "/api/v1/users/lookup_slack_uid/U0ADA", Some(&key)).await;
    assert_eq!((r.status, r.json()), (StatusCode::OK, json!({ "user_id": ada, "slack_uid": "U0ADA" })));
    let r = get(&app, "/api/v1/users/lookup_email/ada@example.com", Some(&key)).await;
    assert_eq!((r.status, r.json()), (StatusCode::OK, json!({ "user_id": ada, "email": "ada@example.com" })));
    assert_eq!(get(&app, "/api/v1/users/lookup_email/ada%40example.com", Some(&key)).await.status, StatusCode::OK);
    let r = get(&app, "/api/v1/users/lookup_slack_uid/U0NOPE", Some(&key)).await;
    assert_eq!((r.status, r.json()), (StatusCode::NOT_FOUND, json!({ "error": "User not found", "slack_uid": "U0NOPE" })));
    let r = get(&app, "/api/v1/users/lookup_email/who@example.com", Some(&key)).await;
    assert_eq!((r.status, r.json()), (StatusCode::NOT_FOUND, json!({ "error": "User not found", "email": "who@example.com" })));

    for path in ["lookup_slack_uid/U0ADA", "lookup_email/ada@example.com"] {
        for k in [None, Some(editor.as_str()), Some("kp_wrong")] {
            let r = get(&app, &format!("/api/v1/users/{path}"), k).await;
            assert_eq!((r.status, r.json()), (StatusCode::UNAUTHORIZED, json!({ "error": "Unauthorized" })), "{path} {k:?}");
        }
    }
    programs::revoke(&app.st.pool, None, &key[..11], "test").await.unwrap();
    assert_eq!(get(&app, "/api/v1/users/lookup_slack_uid/U0ADA", Some(&key)).await.status, StatusCode::UNAUTHORIZED);
    app.finish().await;
}

#[tokio::test]
async fn cors_and_caching_headers() {
    let app = TestApp::new().await;
    user(&app, "ada").await;
    app.exec("UPDATE users SET is_public = false WHERE username = 'ada'").await;
    let h = |r: &Resp, name: &str| r.headers.get(name).and_then(|v| v.to_str().ok()).map(String::from);

    for path in ["/api/v1/users/ada/stats", "/api/v1/users/nobody/stats", "/api/v1/users/ada/trust_factor", "/api/v1/leaderboard", "/api/v1/leaderboard/weekly", "/api/v1/users/ada/projects", "/api/v1/users/lookup_email/a@b.c"] {
        let r = get(&app, path, None).await;
        assert_eq!(h(&r, "access-control-allow-origin").as_deref(), Some("*"), "{path} {}", r.status);
        assert_eq!(h(&r, "cache-control").as_deref(), Some("no-store"), "{path}");
        assert!(h(&r, "access-control-allow-credentials").is_none(), "{path}: never with credentials");
    }
    let pre = app
        .send(
            Request::builder()
                .method(Method::OPTIONS)
                .uri("/api/v1/users/ada/stats")
                .header(header::ORIGIN, "https://program.example")
                .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
                .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "authorization")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(pre.status, StatusCode::NO_CONTENT);
    assert_eq!(h(&pre, "access-control-allow-origin").as_deref(), Some("*"));
    assert!(h(&pre, "access-control-allow-methods").unwrap().contains("GET"));
    assert_eq!(h(&pre, "access-control-allow-headers").as_deref(), Some("authorization"));
    assert!(h(&pre, "access-control-allow-credentials").is_none());
    app.finish().await;
}

#[tokio::test]
async fn program_keys_have_their_own_rate_limit() {
    let app = TestApp::new().await;
    user(&app, "ada").await;
    let key = program_key(&app, "stardance").await;
    let mut limited = None;
    for _ in 0..80 {
        let r = get(&app, "/api/v1/users/ada/trust_factor", None).await;
        if r.status == StatusCode::TOO_MANY_REQUESTS {
            limited = Some(r);
            break;
        }
    }
    let r = limited.expect("anonymous callers are limited per IP");
    assert!(r.headers.contains_key(header::RETRY_AFTER));
    assert_eq!(get(&app, "/api/v1/users/ada/stats?features=", Some(&key)).await.status, StatusCode::OK, "a program key has its own bucket");
    app.finish().await;
}

#[tokio::test]
async fn more_program_reads_at_once_than_pool_connections_all_finish() {
    let app = TestApp::new().await;
    let uid = user(&app, "ada").await;
    let dev = device(&app, uid, "laptop").await;
    span(&app, uid, dev, 1_773_100_800.0, 1800.0, Some("ada/site"), "editing").await;
    let key = program_key(&app, "stardance").await;
    let reads = (0..24).map(|i| {
        let path = if i % 2 == 0 { "/api/v1/users/ada/stats?features=projects".to_string() } else { "/api/v1/users/lookup_email/nobody@example.com".to_string() };
        let key = key.clone();
        let app = &app;
        async move { get(app, &path, Some(&key)).await.status }
    });
    let all = futures_util::future::join_all(reads);
    let statuses = tokio::time::timeout(std::time::Duration::from_secs(30), all).await.expect("concurrent reads finished");
    assert!(statuses.iter().all(|s| s.is_success() || *s == StatusCode::NOT_FOUND), "{statuses:?}");
    app.finish().await;
}

async fn stat_total(app: &TestApp, path: String) -> f64 {
    get(app, &path, None).await.json()["total_seconds"].as_f64().unwrap()
}

#[tokio::test]
async fn projects_share_each_second_across_a_users_devices_as_the_days_do() {
    let app = TestApp::new().await;
    let t0 = 1_773_000_000.0;
    let w = "start_date=2026-03-08&end_date=2026-03-10";
    let mut ada = app.browser();
    ada.sign_in("Ada Lovelace", "ada@example.com").await;
    for (name, project, from, to) in [("laptop", "acme/a", 0.0, 3600.0), ("desktop", "acme/b", 1800.0, 5400.0)] {
        let token = ada.pair(name).await;
        let mut chain = Chain::default();
        chain.editing(project, t0 + from, t0 + to);
        assert_eq!(upload(&app, &token, chain.take()).await.status, StatusCode::OK);
    }
    let mut bea = app.browser();
    bea.sign_in("Bea Hacker", "bea@example.com").await;
    let token = bea.pair("laptop").await;
    let mut chain = Chain::default();
    chain.editing("acme/a", t0, t0 + 3600.0);
    assert_eq!(upload(&app, &token, chain.take()).await.status, StatusCode::OK);

    let (ada_id, bea_id) = (user_id(&app, "ada").await, user_id(&app, "bea").await);
    let c = app.st.pool.get().await.unwrap();
    let (wall, per_device): (f64, f64) = {
        let r = c.query_one("SELECT max(end_ts) - min(start_ts), sum(end_ts - start_ts) FROM spans WHERE user_id = $1", &[&ada_id]).await.unwrap();
        (r.get(0), r.get(1))
    };
    assert!(per_device > wall + 1000.0, "the devices overlap");
    let days = |rows: &[(String, String, String, f64)], project: &str| rows.iter().filter(|r| r.1 == project).map(|r| r.3).sum::<f64>();
    let held = day_seconds(&app, ada_id).await;
    let (a, b) = (days(&held, "acme/a"), days(&held, "acme/b"));
    assert_eq!(a + b, wall, "the days give each second to one project");

    let stats = |extra: &str| format!("/api/v1/users/ada/stats?{w}&total_seconds=true{extra}");
    let (api_a, api_b) = (stat_total(&app, stats("&filter_by_project=acme/a")).await, stat_total(&app, stats("&filter_by_project=acme/b")).await);
    assert_eq!((api_a, api_b), (a.round(), b.round()));
    assert_eq!(api_a + api_b, wall.round());
    assert_eq!(stat_total(&app, stats("")).await, wall.round());

    let r = get(&app, &format!("/api/v1/users/ada/stats?{w}&features=projects"), None).await.json();
    let listed: f64 = r["data"]["projects"].as_array().unwrap().iter().map(|p| p["total_seconds"].as_f64().unwrap()).sum();
    assert_eq!(listed, wall.round(), "the project list adds up to the total");
    let r = get(&app, "/api/v1/users/ada/projects/details?start=2026-03-08&end=2026-03-10", None).await.json();
    let details: Vec<(String, f64)> = r["projects"].as_array().unwrap().iter().map(|p| (p["name"].as_str().unwrap().to_string(), p["total_seconds"].as_f64().unwrap())).collect();
    assert_eq!(details.iter().map(|d| d.1).sum::<f64>(), wall.round());
    assert!(details.contains(&("acme/a".to_string(), a.round())) && details.contains(&("acme/b".to_string(), b.round())), "{details:?}");
    let key = accounts::new_editor_key(&app.st, ada_id).await.unwrap();
    let r = get(&app, &format!("/api/v1/users/ada/heartbeats/spans?{w}"), Some(&key)).await.json();
    let spans: f64 = r["spans"].as_array().unwrap().iter().map(|s| s["duration"].as_f64().unwrap()).sum();
    assert_eq!(spans, wall.round());

    let held = day_seconds(&app, bea_id).await;
    let alone = days(&held, "acme/a");
    let r = get(&app, &format!("/api/v1/users/bea/stats?{w}&total_seconds=true&filter_by_project=acme/a"), None).await.json();
    assert_eq!(r["total_seconds"].as_f64().unwrap(), alone.round());
    let length: f64 = c.query_one("SELECT sum(end_ts - start_ts) FROM spans WHERE user_id = $1", &[&bea_id]).await.unwrap().get(0);
    assert_eq!(alone, length, "one device is credited whole");
    app.finish().await;
}
