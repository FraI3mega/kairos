mod common;

use axum::http::StatusCode;
use common::{upload, user_id, Chain, TestApp};
use kairos_core::credit::TURN_MAX_SECS;
use kairos_core::event::Kind;

const D: f64 = 1773100800.0;
const HOUR: f64 = 3600.0;

const EDITING_SECS: &str = "SELECT COALESCE(sum(end_ts - start_ts), 0)::bigint FROM spans WHERE user_id = $1 AND class = 'editing'";

// A turn whose reply_done never came gates editor time until TURN_MAX after its start. The turn goes up in one batch
// and the editing hours later, past the settle window, so only what the recompute reads from before it says whether
// the turn still runs.
async fn editing_after(turn_ended: bool, at: f64) -> i64 {
    let app = TestApp::new().await;
    let mut b = app.browser();
    b.sign_in("Ada Lovelace", "ada@example.com").await;
    let uid = user_id(&app, "ada").await;
    let laptop = b.pair("laptop").await;
    let mut a = Chain::default();
    a.prompt(D + 1000.0, Some("ada/site"), "gemini-cli");
    if turn_ended {
        a.push_from(D + 1010.0, Some("ada/site"), "gemini-cli", Kind::AgentStart { session: "s1".into() });
        a.reply(D + 1120.0, Some("ada/site"), "gemini-cli");
    }
    assert_eq!(upload(&app, &laptop, a.take()).await.status, StatusCode::OK);
    a.editing("ada/site", at, at + 600.0);
    assert_eq!(upload(&app, &laptop, a.take()).await.status, StatusCode::OK);
    let secs = app.count(EDITING_SECS, uid).await;
    app.finish().await;
    secs
}

#[tokio::test]
async fn a_turn_that_never_ended_gates_editor_time_until_turn_max() {
    assert_eq!(editing_after(false, D + 1000.0 + TURN_MAX_SECS - HOUR).await, 0);
    assert_eq!(editing_after(false, D + 1000.0 + TURN_MAX_SECS + HOUR).await, 600);
}

#[tokio::test]
async fn a_turn_that_ended_does_not() {
    assert_eq!(editing_after(true, D + 1000.0 + TURN_MAX_SECS - HOUR).await, 600);
}
