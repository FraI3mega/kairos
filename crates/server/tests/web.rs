mod common;

use axum::http::{header, StatusCode};
use chrono::{Days, Utc};
use common::{upload, user_id, Browser, Chain, Resp, TestApp};
use kairos_core::event::Kind;
use kairos_server::util::now;

async fn busy_user(app: &TestApp, b: &mut Browser<'_>, name: &str, email: &str) -> i64 {
    b.sign_in(name, email).await;
    let token = b.pair("ada-laptop").await;
    let t = now();
    let mut chain = Chain::default();
    chain.push(t - 4000.0, None, Kind::Backends { focus: "x11".into(), idle: "xprintidle".into() });
    chain.prompt(t - 3900.0, Some("ada/site"), "claude-code");
    chain.prompt(t - 3800.0, Some("ada/site"), "claude-code");
    chain.prompt(t - 3700.0, Some("ada/secret"), "codex");
    chain.reply(t - 3650.0, Some("ada/secret"), "codex");
    chain.editing("ada/site", t - 3600.0, t - 2400.0);
    chain.editing("ada/secret", t - 2300.0, t - 1800.0);
    assert_eq!(upload(app, &token, chain.take()).await.status, StatusCode::OK);
    let uid = user_id(app, &name.split(' ').next().unwrap().to_lowercase()).await;
    uid
}

fn csp(r: &Resp) -> String {
    r.headers[header::CONTENT_SECURITY_POLICY].to_str().unwrap().to_string()
}

fn strict(r: &Resp) {
    let c = csp(r);
    assert!(c.contains("default-src 'self'") && c.contains("connect-src 'self'") && c.contains("object-src 'none'"), "{c}");
    let script = c.split(';').find(|d| d.trim().starts_with("script-src")).unwrap();
    assert!(script.contains("'nonce-") && !script.contains("unsafe-inline") && !script.contains("http"), "{c}");
    let nonce = c.split("'nonce-").nth(1).unwrap().split('\'').next().unwrap();
    for tag in r.body.split("<script").skip(1) {
        let open = tag.split('>').next().unwrap();
        assert!(open.contains(&format!("nonce=\"{nonce}\"")), "script without the nonce: <script{open}>");
    }
    for bad in [" onclick=", " onload=", " onerror=", "<style", "javascript:"] {
        assert!(!r.body.contains(bad), "{bad}");
    }
}

#[tokio::test]
async fn the_dashboard_walks_a_new_user_through_setup() {
    let app = TestApp::new().await;
    let mut b = app.browser();
    b.sign_in("Ada Lovelace", "ada@example.com").await;
    let page = b.get("/me").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    for text in [
        "Set up your first computer",
        "irm ",
        "/install.ps1 | iex",
        "curl -fsSL ",
        "/install.sh | sh",
        "kairos login",
        "Your agents ran",
        "Streak",
        "0 days",
        "This week",
        "AI tools",
        "Nothing here yet.",
    ] {
        assert!(page.body.contains(text), "missing {text:?}");
    }
    assert!(!page.body.contains("Disconnect"));
    strict(&page);
    app.finish().await;
}

#[tokio::test]
async fn the_dashboard_shows_devices_tools_and_time() {
    let app = TestApp::new().await;
    let mut b = app.browser();
    busy_user(&app, &mut b, "Ada Lovelace", "ada@example.com").await;
    let page = b.get("/me").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert!(!page.body.contains("Set up your first computer"));
    for text in ["ada-laptop", "Linux", "v9.9.9", "Disconnect", "Online", "Claude Code", "Codex", "2 prompts", "ada/site", "ada/secret"] {
        assert!(page.body.contains(text), "missing {text:?}");
    }
    assert!(!page.body.contains("xprintidle") && !page.body.contains("can't tell"), "working backends show nothing");
    assert!(page.body.contains("<script type=\"application/json\" id=\"snap\""));
    assert!(!page.body.contains(">0m</div>"), "week total should not be empty");

    let revoke = b.post("/settings/devices/revoke", &[("csrf", &page.csrf()), ("device_id", "1")]).await;
    assert_eq!(revoke.status, StatusCode::SEE_OTHER);
    assert!(b.get("/me").await.body.contains("Set up your first computer"));
    app.finish().await;
}

#[tokio::test]
async fn profiles_show_only_what_the_owner_allows() {
    let app = TestApp::new().await;
    let mut ada = app.browser();
    busy_user(&app, &mut ada, "Ada Lovelace", "ada@example.com").await;
    app.exec("UPDATE projects SET hidden = true WHERE id = 'ada/secret'").await;
    let mut visitor = app.browser();

    let page = visitor.get("/@ada").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    for text in ["Member since", "Streak", "This week", "ada/site", "Private projects", "AI tools", "Claude Code", "Codex"] {
        assert!(page.body.contains(text), "missing {text:?}");
    }
    assert!(!page.body.contains("ada/secret"));
    assert!(!page.body.contains("Your agents ran") && !page.body.contains("Agents ran"));
    assert!(page.body.contains("<meta name=\"description\" content=\"Ada has tracked"));
    assert!(page.body.contains("<meta property=\"og:title\" content=\"Ada (@ada) · Kairos\">"));
    assert!(page.body.contains("<meta property=\"og:type\" content=\"profile\">"));
    assert!(page.body.contains(&format!("<meta property=\"og:image\" content=\"{}/og.png\">", app.url)));
    assert!(page.body.contains(&format!("<link rel=\"canonical\" href=\"{}/@ada\">", app.url)));
    strict(&page);

    let own = ada.get("/@ada").await;
    assert!(own.body.contains("ada/secret"));

    app.exec("UPDATE users SET show_ai_tools = false").await;
    let page = visitor.get("/@ada").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(!page.body.contains("AI tools") && !page.body.contains("Claude Code"));

    app.exec("UPDATE users SET is_public = false").await;
    assert_eq!(visitor.get("/@ada").await.status, StatusCode::NOT_FOUND);
    assert_eq!(ada.get("/@ada").await.status, StatusCode::OK);
    app.finish().await;
}

async fn member(app: &TestApp, name: &str, public: bool, trust: &str, secs: f64) {
    let c = app.st.pool.get().await.unwrap();
    let id: i64 = c
        .query_one(
            "INSERT INTO users (hca_sub, username, display_name, is_public, trust) VALUES ($1, $1, $2, $3, $4) RETURNING id",
            &[&name, &format!("{} Name", name.to_uppercase()), &public, &trust],
        )
        .await
        .unwrap()
        .get(0);
    let today = Utc::now().date_naive();
    for ago in 0..3u64 {
        let d = today.checked_sub_days(Days::new(ago)).unwrap();
        c.execute("INSERT INTO user_days (user_id, day, project, class, seconds) VALUES ($1, $2, 'o/p', 'typing', $3)", &[&id, &d, &secs])
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn the_leaderboard_lists_public_people_and_is_cached() {
    let app = TestApp::new().await;
    member(&app, "ada", true, "unscored", 3000.0).await;
    member(&app, "bob", true, "trusted", 1800.0).await;
    member(&app, "eve", false, "unscored", 9000.0).await;
    member(&app, "mal", true, "banned", 9000.0).await;
    let mut b = app.browser();

    let page = b.get("/leaderboards").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert!(page.body.contains("ADA Name") && page.body.contains("BOB Name"));
    assert!(!page.body.contains("EVE Name") && !page.body.contains("MAL Name"));
    assert!(page.body.contains("href=\"/@ada\"") && page.body.contains("3-day streak"));
    assert!(page.body.find("ADA Name").unwrap() < page.body.find("BOB Name").unwrap());
    assert!(page.body.contains(">50m</b>") && page.body.contains(">30m</b>"));
    strict(&page);

    let week = b.get("/leaderboards?period=week").await;
    assert!(week.body.contains("2h 30m"), "{}", week.body);

    member(&app, "cyd", true, "unscored", 6000.0).await;
    let cached = b.get("/leaderboards").await;
    assert!(!cached.body.contains("CYD Name"));
    app.finish().await;
}

#[tokio::test]
async fn the_leaderboard_says_so_when_it_is_empty() {
    let app = TestApp::new().await;
    let page = app.browser().get("/leaderboards").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("Nobody is on the board yet"));
    app.finish().await;
}

#[tokio::test]
async fn the_extension_page_links_to_the_stores_that_are_configured() {
    let app = TestApp::new().await;
    let page = app.browser().get("/extension").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert!(page.body.contains("Coming soon") && !page.body.contains("noopener"));
    assert!(page.body.contains("No addresses, no page titles"));
    strict(&page);
    app.finish().await;

    let chrome = "https://chromewebstore.google.com/detail/kairos/abcdef";
    let firefox = "https://addons.mozilla.org/firefox/addon/kairos/";
    let app = TestApp::with(&[("KAIROS_CHROME_EXTENSION_URL", chrome), ("KAIROS_FIREFOX_EXTENSION_URL", firefox)]).await;
    let page = app.browser().get("/extension").await;
    assert!(page.body.contains(&format!("href=\"{chrome}\"")) && page.body.contains(&format!("href=\"{firefox}\"")));
    assert!(page.body.contains(">Chrome Web Store<") && page.body.contains(">Firefox Add-ons<") && !page.body.contains("Coming soon"));
    strict(&page);
    app.finish().await;

    let app = TestApp::with(&[("KAIROS_FIREFOX_EXTENSION_URL", firefox)]).await;
    let page = app.browser().get("/extension").await;
    assert!(page.body.contains(">Firefox Add-ons<") && !page.body.contains(">Chrome Web Store<") && !page.body.contains("Coming soon"));
    app.finish().await;
}

#[tokio::test]
async fn the_landing_page_mentions_the_tray_pause() {
    let app = TestApp::new().await;
    let page = app.browser().get("/").await;
    for text in ["tray", "pause", "Cursor"] {
        assert!(page.body.to_lowercase().contains(&text.to_lowercase()), "missing {text:?}");
    }
    assert!(!page.body.contains('\u{2014}'));
    app.finish().await;
}

#[tokio::test]
async fn new_pages_never_use_em_dashes_or_cheat_talk() {
    let app = TestApp::new().await;
    let mut b = app.browser();
    busy_user(&app, &mut b, "Ada Lovelace", "ada@example.com").await;
    for path in ["/me", "/@ada", "/leaderboards", "/extension", "/"] {
        let page = b.get(path).await;
        let text = page.body.to_lowercase();
        assert!(!text.contains('\u{2014}'), "{path}");
        for word in ["cheat", "fraud", "suspicious", "doesn't count", "does not count"] {
            assert!(!text.contains(word), "{path} says {word}");
        }
    }
    app.finish().await;
}

#[tokio::test]
async fn the_browser_sets_the_timezone_once_and_settings_win() {
    let app = TestApp::new().await;
    let mut b = app.browser();
    b.sign_in("Ada Lovelace", "ada@example.com").await;
    let token = b.pair("laptop").await;
    let mut chain = Chain::default();
    let late = 1_773_014_400.0 - 1800.0;
    chain.editing("ada/site", late, late + 1200.0);
    assert_eq!(upload(&app, &token, chain.take()).await.status, StatusCode::OK);
    let uid = user_id(&app, "ada").await;
    let day = "SELECT count(*) FROM user_days WHERE user_id = $1 AND day = '2026-03-09'";
    assert_eq!(app.count(day, uid).await, 0, "23:30 UTC is still March 8");
    assert!(b.get("/me").await.body.contains("data-auto=\"1\""));

    let csrf = b.csrf().await;
    assert_eq!(b.post("/me/timezone", &[("csrf", &csrf), ("timezone", "Mars/Olympus")]).await.status, StatusCode::BAD_REQUEST);
    assert_eq!(b.post("/me/timezone", &[("csrf", "00"), ("timezone", "Europe/Berlin")]).await.status, StatusCode::FORBIDDEN);
    let first = b.post("/me/timezone", &[("csrf", &csrf), ("timezone", "Europe/Berlin")]).await;
    assert_eq!((first.status, first.body.as_str()), (StatusCode::OK, r#"{"changed":true}"#));
    assert!(app.count(day, uid).await > 0, "in Berlin it was past midnight");
    let again = b.post("/me/timezone", &[("csrf", &csrf), ("timezone", "Asia/Tokyo")]).await;
    assert_eq!(again.body, r#"{"changed":false}"#, "only the first browser zone is taken");
    assert_eq!(app.count("SELECT count(*) FROM users WHERE id = $1 AND timezone = 'Europe/Berlin'", uid).await, 1);
    assert!(!b.get("/me").await.body.contains("data-auto"), "after that a different zone only shows the hint");

    let mut bea = app.browser();
    bea.sign_in("Bea Hacker", "bea@example.com").await;
    let csrf = bea.csrf().await;
    bea.post("/settings/profile", &[("csrf", &csrf), ("username", "bea"), ("timezone", "UTC")]).await;
    let taken = bea.post("/me/timezone", &[("csrf", &csrf), ("timezone", "Europe/Berlin")]).await;
    assert_eq!(taken.body, r#"{"changed":false}"#, "UTC picked in settings stays");
    app.finish().await;
}
