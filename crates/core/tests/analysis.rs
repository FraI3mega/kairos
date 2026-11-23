use kairos_core::{analyze, find_session_files, human_credits, parse_session, total_secs, Options, Session};
use chrono::{Duration, NaiveDate, TimeZone, Utc};
use serde_json::{json, Value};

fn ts(secs: i64) -> String {
    let base = Utc.with_ymd_and_hms(2026, 3, 10, 10, 0, 0).unwrap();
    (base + Duration::seconds(secs)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn words(n: usize) -> String {
    vec!["word"; n].join(" ")
}

fn prompt(secs: i64, text: &str) -> Value {
    json!({"type": "user", "timestamp": ts(secs), "message": {"role": "user", "content": text}})
}

fn assistant(secs: i64, reply_words: usize, tool_uses: usize) -> Value {
    let mut content = vec![json!({"type": "text", "text": words(reply_words)})];
    for _ in 0..tool_uses {
        content.push(json!({"type": "tool_use", "name": "Bash", "input": {}}));
    }
    json!({"type": "assistant", "timestamp": ts(secs),
           "message": {"role": "assistant", "model": "test-model", "content": content}})
}

fn tool_result(secs: i64) -> Value {
    json!({"type": "user", "timestamp": ts(secs),
           "message": {"role": "user", "content": [{"type": "tool_result", "content": "ok"}]}})
}

fn session(name: &str, events: &[Value]) -> Session {
    let text: Vec<String> = events.iter().map(|e| e.to_string()).collect();
    parse_session(&text.join("\n"), name, "proj")
}

fn opts(think_secs: f64) -> Options {
    Options { think_secs, days: 1 }
}

fn approx(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-6, "{a} != {b}");
}

#[test]
fn tool_result_is_not_a_prompt() {
    let s = session(
        "a",
        &[
            prompt(0, "please fix the build now"),
            assistant(10, 0, 1),
            tool_result(20),
            assistant(30, 460, 0),
            prompt(1000, &words(10)),
        ],
    );
    assert_eq!(s.turns.len(), 2);
    assert_eq!(s.turns[0].tools, 1);
    assert_eq!(s.turns[0].reply_words, 460);
    approx(s.turns[0].end - s.turns[0].start, 30.0);
    let credits = human_credits(&s.turns, 0.0);
    approx(total_secs(&credits), 60.0 * (460.0 / 230.0 + 10.0 / 35.0));
}

#[test]
fn short_prompt_earns_nothing() {
    let s = session(
        "a",
        &[
            prompt(0, "do the thing with the files"),
            assistant(10, 0, 0),
            prompt(500, "yes please"),
            assistant(510, 0, 0),
            prompt(1000, &words(10)),
        ],
    );
    assert_eq!(s.turns.len(), 3);
    let credits = human_credits(&s.turns, 120.0);
    assert_eq!(credits.len(), 1);
    approx(credits[0].0, s.turns[1].end);
}

#[test]
fn overlapping_sessions_are_unioned() {
    let a = session("a", &[prompt(0, "first session prompt here"), assistant(600, 0, 0)]);
    let b = session("b", &[prompt(300, "second session prompt here"), assistant(900, 0, 0)]);
    let today = NaiveDate::from_ymd_opt(2026, 3, 10).unwrap();
    let r = analyze(&[a, b], &opts(120.0), today);
    approx(r.totals.agent_hours, 0.25);
    assert_eq!(r.totals.sessions, 2);
    assert_eq!(r.totals.prompts, 2);
}

#[test]
fn overlapping_human_credit_is_unioned() {
    let mk = |name: &str, off: i64| {
        session(
            name,
            &[
                prompt(off, "start something new please"),
                assistant(off + 10, 0, 0),
                prompt(off + 10_000, &words(10)),
            ],
        )
    };
    let (a, b) = (mk("a", 0), mk("b", 0));
    let today = NaiveDate::from_ymd_opt(2026, 3, 10).unwrap();
    let both = analyze(&[a.clone(), b], &opts(0.0), today).totals.human_hours;
    let one = analyze(&[a], &opts(0.0), today).totals.human_hours;
    approx(both, one);
}

#[test]
fn paste_typing_time_is_capped() {
    let pasted = session(
        "a",
        &[prompt(0, "look at this log"), assistant(10, 0, 0), prompt(10_000, &words(300))],
    );
    assert!(pasted.turns[1].paste);
    approx(total_secs(&human_credits(&pasted.turns, 0.0)), 60.0 * 60.0 / 35.0);

    let typed = session(
        "b",
        &[prompt(0, "look at this log"), assistant(10, 0, 0), prompt(10_000, &words(200))],
    );
    assert!(!typed.turns[1].paste);
    approx(total_secs(&human_credits(&typed.turns, 0.0)), 60.0 * 200.0 / 35.0);
}

#[test]
fn many_lines_count_as_paste() {
    let text = (0..20).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
    let s = session("a", &[prompt(0, &text)]);
    assert!(s.turns[0].paste);
}

#[test]
fn sidechain_and_non_cli_entries_are_skipped() {
    let mut side_prompt = prompt(100, "sidechain prompt that is long enough");
    side_prompt["isSidechain"] = json!(true);
    let mut side_reply = assistant(5000, 0, 0);
    side_reply["isSidechain"] = json!(true);
    let mut sdk = prompt(200, "sdk entrypoint prompt that is long");
    sdk["entrypoint"] = json!("sdk-cli");
    let s = session(
        "a",
        &[prompt(0, "the real prompt from the cli"), side_prompt, side_reply, sdk],
    );
    assert_eq!(s.turns.len(), 1);
    approx(s.turns[0].end, s.turns[0].start);
}

#[test]
fn subagent_files_are_skipped() {
    let dir = std::env::temp_dir().join(format!("kairos-test-{}", std::process::id()));
    let sub = dir.join("proj").join("sess").join("subagents");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(dir.join("proj").join("a.jsonl"), "").unwrap();
    std::fs::write(sub.join("agent-1.jsonl"), "").unwrap();
    let found = find_session_files(&[dir.clone()]);
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(found.len(), 1);
    assert!(found[0].ends_with("proj/a.jsonl"));
}

#[test]
fn credit_never_exceeds_gap() {
    let s = session(
        "a",
        &[prompt(0, "go do a big job"), assistant(10, 2000, 0), prompt(20, &words(50))],
    );
    let credits = human_credits(&s.turns, 600.0);
    approx(total_secs(&credits), 10.0);
}

#[test]
fn interrupts_are_counted_and_not_prompts() {
    let s = session(
        "a",
        &[
            prompt(0, "start the long task now"),
            assistant(100, 0, 0),
            json!({"type": "user", "timestamp": ts(110), "message": {"role": "user",
                "content": [{"type": "text", "text": "[Request interrupted by user]"}]}}),
        ],
    );
    assert_eq!(s.interrupts, 1);
    assert_eq!(s.turns.len(), 1);
    approx(s.turns[0].end - s.turns[0].start, 110.0);
}
