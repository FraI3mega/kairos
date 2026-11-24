use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::claude_history::History;
use super::{string_words, work, words, Found, Source, Tail};
use crate::event::{Ask, Kind};
use crate::parse::{timestamp, user_line, UserLine};
use crate::{prompt_shape, prompt_stats};

pub fn default_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(d) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        roots.push(PathBuf::from(d).join("projects"));
    }
    if let Some(home) = dirs::home_dir() {
        // Listed before it exists: Claude Code may get installed while kairosd is running.
        let main = home.join(".claude").join("projects");
        if !roots.contains(&main) {
            roots.push(main);
        }
        for e in std::fs::read_dir(&home).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with(".claude-") {
                let p = e.path().join("projects");
                if p.is_dir() && !roots.contains(&p) {
                    roots.push(p);
                }
            }
        }
    }
    // Claude desktop's Cowork runs Claude Code with a per-session config dir under here.
    if let Some(p) = dirs::config_dir().map(|c| c.join("Claude").join(COWORK)) {
        if p.is_dir() {
            roots.push(p);
        }
    }
    roots
}

/// The Claude Code config folder `session`'s log is in, when it isn't the one a program started by kairosd looks in. Claude
/// Code is pointed at another with `CLAUDE_CONFIG_DIR`, which its hooks run with and kairosd doesn't.
pub fn other_config_dir(session: &str) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let usual = std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from).unwrap_or_else(|| home.join(".claude"));
    other_config_dir_in(&home, &usual, session)
}

fn other_config_dir_in(home: &Path, usual: &Path, session: &str) -> Option<PathBuf> {
    if session.is_empty() || session.contains(['/', '\\']) {
        return None;
    }
    let log = format!("{session}.jsonl");
    let named = |e: &std::fs::DirEntry| e.file_name().to_str().is_some_and(|n| n == ".claude" || n.starts_with(".claude-"));
    let holds = |dir: &PathBuf| std::fs::read_dir(dir.join("projects")).into_iter().flatten().flatten().any(|p| p.path().join(&log).is_file());
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(home).ok()?.flatten().filter(named).map(|e| e.path()).filter(|d| d != usual).collect();
    dirs.sort();
    dirs.into_iter().find(holds)
}

const SHOWN_CAP: usize = 2000;
const COWORK: &str = "local-agent-mode-sessions";

fn skip(p: &Path) -> bool {
    let has = |name: &str| p.components().any(|c| c.as_os_str() == name);
    has(COWORK) && !has(".claude")
}

// Subagent transcripts count for what they edit and commit, never for turns.
fn subagent(p: &Path) -> bool {
    p.components().any(|c| c.as_os_str() == "subagents")
}

// A Cowork session runs in a sandbox folder; the folder the user picked for it (if any) is the project.
fn cowork_folder(p: &Path) -> Option<PathBuf> {
    let dir = p.ancestors().find(|a| {
        a.join(".claude").is_dir() && a.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("local_"))
    })?;
    let text = std::fs::read_to_string(dir.with_extension("json")).ok()?;
    let meta: Value = serde_json::from_str(&text).ok()?;
    meta.get("userSelectedFolders")?.as_array()?.iter().find_map(|f| f.as_str()).map(PathBuf::from)
}

#[derive(Default)]
struct Turn {
    session: String,
    cwd: Option<PathBuf>,
    folder: Option<PathBuf>,
    armed: bool,
    open: bool,
    last_ts: f64,
    reply_words: usize,
    tools: usize,
    files: BTreeSet<String>,
    model: Option<String>,
    sub: bool,
    commands: HashMap<String, String>,
    restoring: HashMap<String, ()>,
    asks: HashMap<String, (f64, Option<Ask>, usize)>,
}

impl Turn {
    fn close(&mut self, ts: f64, out: &mut Vec<Found>) {
        if !self.open {
            return;
        }
        self.open = false;
        self.armed = false;
        out.push(Found {
            ts,
            cwd: self.cwd.clone(),
            kind: Kind::ReplyDone {
                session: self.session.clone(),
                reply_words: self.reply_words,
                tools: self.tools,
                files_changed: self.files.len(),
                model: self.model.clone(),
            },
        });
    }
}

pub struct Claude {
    tail: Tail,
    turns: HashMap<PathBuf, Turn>,
    history: History,
}

impl Claude {
    pub fn new(roots: Vec<PathBuf>) -> Claude {
        Claude { history: History::new(&roots), tail: Tail::new(roots, "jsonl", skip), turns: HashMap::new() }
    }
}

// Edit tools (and shell commands, as `bashEditDiff`) put the patch in `toolUseResult`; a commit is a Bash call whose own output printed the sha.
fn agent_work(turn: &mut Turn, e: &Value, ts: f64, cwd: Option<&Path>, out: &mut Vec<Found>) {
    let ty = e.get("type").and_then(Value::as_str);
    let blocks = e.pointer("/message/content").and_then(Value::as_array).into_iter().flatten();
    let session = e.get("sessionId").and_then(Value::as_str).unwrap_or(&turn.session).to_string();
    for b in blocks {
        match (ty, b.get("type").and_then(Value::as_str)) {
            (Some("assistant"), Some("tool_use")) if matches!(b.get("name").and_then(Value::as_str), Some("Bash" | "PowerShell")) => {
                if let (Some(id), Some(cmd)) = (b.get("id").and_then(Value::as_str), b.pointer("/input/command").and_then(Value::as_str)) {
                    if cmd.contains("commit") {
                        work::remember(&mut turn.commands, id.to_string(), cmd.to_string());
                    }
                    if cmd.contains("git") && work::restores_files(cmd) {
                        work::remember(&mut turn.restoring, id.to_string(), ());
                    }
                }
            }
            (Some("user"), Some("tool_result")) => {
                let failed = b.get("is_error").and_then(Value::as_bool) == Some(true);
                let result = e.get("toolUseResult");
                let id = b.get("tool_use_id").and_then(Value::as_str);
                let restored = id.is_some_and(|id| turn.restoring.remove(id).is_some());
                // changedFiles/moreFiles carry no hunks, so those paths have no counts.
                let shell_edits = result.filter(|_| !restored).and_then(|r| r.pointer("/bashEditDiff/files")).and_then(Value::as_array).into_iter().flatten();
                for f in shell_edits {
                    if let (Some(path), Some(hunks)) = (f.get("filePath").and_then(Value::as_str), f.get("hunks").and_then(Value::as_array)) {
                        let lines = hunks.iter().flat_map(|h| h.get("lines").and_then(Value::as_array).into_iter().flatten()).filter_map(Value::as_str);
                        work::edit(ts, cwd, &session, path, work::hunk_counts(lines), out);
                    }
                }
                if let Some(cmd) = id.and_then(|id| turn.commands.remove(id)) {
                    let stdout = result.and_then(|r| r.get("stdout")).and_then(Value::as_str);
                    let text = stdout.map(str::to_string).or_else(|| failed.then(|| result_text(b)).flatten());
                    work::commits(ts, cwd, &session, &cmd, text.as_deref().unwrap_or(""), out);
                } else if let (false, Some(r)) = (failed, result.filter(|r| r.is_object())) {
                    let Some(path) = r.get("filePath").and_then(Value::as_str) else { continue };
                    let counts = match r.get("structuredPatch").and_then(Value::as_array) {
                        Some(hunks) if !hunks.is_empty() => work::hunk_counts(
                            hunks.iter().flat_map(|h| h.get("lines").and_then(Value::as_array).into_iter().flatten()).filter_map(Value::as_str),
                        ),
                        Some(_) if r.get("type").and_then(Value::as_str) == Some("create") => {
                            (r.get("content").and_then(Value::as_str).map_or(0, work::line_count), 0)
                        }
                        _ => continue,
                    };
                    work::edit(ts, cwd, &session, path, counts, out);
                }
            }
            _ => {}
        }
    }
}

// The person answering the agent: the tool_use is the ask, the tool_result with its id is the answer. Questions and plans are
// asks themselves; any other tool call counts only if the person was shown a permission prompt for it (`config` decided it).
fn answered(turn: &mut Turn, e: &Value, ts: f64, out: &mut Vec<Found>) {
    if e.get("entrypoint").and_then(Value::as_str).is_some_and(|p| p.starts_with("sdk")) {
        return;
    }
    let ty = e.get("type").and_then(Value::as_str);
    let session = e.get("sessionId").and_then(Value::as_str).unwrap_or(&turn.session).to_string();
    let source = e.pointer("/permissionDecision/source").and_then(Value::as_str);
    let blocks = e.pointer("/message/content").and_then(Value::as_array).into_iter().flatten();
    for b in blocks {
        match (ty, b.get("type").and_then(Value::as_str)) {
            (Some("assistant"), Some("tool_use")) => {
                let Some(id) = b.get("id").and_then(Value::as_str) else { continue };
                let ask = match b.get("name").and_then(Value::as_str) {
                    Some("AskUserQuestion") => Some(Ask::Question),
                    Some("ExitPlanMode") => Some(Ask::Plan),
                    _ => None,
                };
                let shown = b.get("input").map_or(0, |i| shown_words(b.get("name").and_then(Value::as_str), i));
                work::remember(&mut turn.asks, id.to_string(), (ts, ask, shown));
            }
            (Some("user"), Some("tool_result")) => {
                let Some((asked, ask, shown_words)) = b.get("tool_use_id").and_then(Value::as_str).and_then(|id| turn.asks.remove(id)) else { continue };
                let user = source.map(|s| s.starts_with("user_"));
                let why = match (ask, user) {
                    (Some(_), Some(false)) | (None, Some(false) | None) => continue,
                    (Some(why), _) => why,
                    (None, Some(true)) => Ask::Permission,
                };
                let answer_words = match why {
                    Ask::Question => e.pointer("/toolUseResult/answers").map_or(0, |a| string_words(a, usize::MAX)),
                    _ => 0,
                };
                let kind = Kind::Answered { session: session.clone(), asked, why, shown_words, answer_words };
                out.push(Found { ts, cwd: turn.cwd.clone(), kind });
            }
            _ => {}
        }
    }
}

// What the agent put in front of the person: every question, header and option of a question, the plan, or whatever a
// permission prompt shows of the call (capped; a prompt for a long file write is not read in full).
fn shown_words(tool: Option<&str>, input: &Value) -> usize {
    let text = |v: Option<&Value>| v.and_then(Value::as_str).map_or(0, words);
    match tool {
        Some("AskUserQuestion") => input
            .get("questions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|q| {
                let options = q.get("options").and_then(Value::as_array).into_iter().flatten();
                text(q.get("question")) + text(q.get("header")) + options.map(|o| text(o.get("label")) + text(o.get("description"))).sum::<usize>()
            })
            .sum(),
        Some("ExitPlanMode") => text(input.get("plan")),
        _ => string_words(input, SHOWN_CAP),
    }
}

fn result_text(block: &Value) -> Option<String> {
    match block.get("content")? {
        Value::String(s) => Some(s.clone()),
        Value::Array(a) => Some(a.iter().filter_map(|p| p.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n")),
        _ => None,
    }
}

fn feed(turn: &mut Turn, e: &Value, out: &mut Vec<Found>) {
    let sidechain = turn.sub || e.get("isSidechain").and_then(Value::as_bool) == Some(true);
    let Some(ts) = timestamp(e) else { return };
    let cwd = match e.get("cwd").and_then(Value::as_str) {
        Some(c) => Some(turn.folder.clone().unwrap_or_else(|| PathBuf::from(c))),
        None => turn.cwd.clone(),
    };
    agent_work(turn, e, ts, cwd.as_deref(), out);
    if sidechain {
        return;
    }
    turn.cwd = cwd;
    answered(turn, e, ts, out);
    let ty = e.get("type").and_then(Value::as_str).unwrap_or("");
    match user_line(e) {
        UserLine::Prompt(text) => {
            let last = turn.last_ts;
            turn.close(last.min(ts), out);
            // sdk-cli (`claude -p`), sdk-ts and sdk-py sessions are driven by programs, not typed.
            if e.get("entrypoint").and_then(Value::as_str).is_some_and(|p| p.starts_with("sdk")) {
                return;
            }
            let session = e.get("sessionId").and_then(Value::as_str).unwrap_or_default().to_string();
            let (words, pasted) = prompt_stats(&text);
            let shape = Some(prompt_shape(&text));
            *turn = Turn {
                session: session.clone(),
                cwd: turn.cwd.take(),
                folder: turn.folder.take(),
                commands: std::mem::take(&mut turn.commands),
                restoring: std::mem::take(&mut turn.restoring),
                asks: std::mem::take(&mut turn.asks),
                armed: true,
                open: true,
                last_ts: ts,
                ..Turn::default()
            };
            out.push(Found {
                ts,
                cwd: turn.cwd.clone(),
                kind: Kind::Prompt { words, pasted, model: None, session, shape, version: version(e) },
            });
            return;
        }
        UserLine::Interrupt => {
            turn.close(ts, out);
            return;
        }
        UserLine::Other => {}
    }
    if ty == "attachment" {
        if let Some(text) = queued_prompt(e) {
            let session = e.get("sessionId").and_then(Value::as_str).unwrap_or_default().to_string();
            let (words, pasted) = prompt_stats(text);
            let shape = Some(prompt_shape(text));
            out.push(Found {
                ts,
                cwd: turn.cwd.clone(),
                kind: Kind::Prompt { words, pasted, model: None, session, shape, version: version(e) },
            });
        }
        return;
    }
    if !turn.open {
        return;
    }
    match ty {
        "assistant" => {
            turn.last_ts = ts;
            let msg = e.get("message");
            if let Some(m) = msg.and_then(|m| m.get("model")).and_then(Value::as_str) {
                if !m.is_empty() && m != "<synthetic>" {
                    turn.model = Some(m.to_string());
                }
            }
            let blocks = msg.and_then(|m| m.get("content")).and_then(Value::as_array);
            for b in blocks.into_iter().flatten() {
                match b.get("type").and_then(Value::as_str) {
                    Some("tool_use") => {
                        if turn.armed {
                            turn.armed = false;
                            out.push(Found {
                                ts,
                                cwd: turn.cwd.clone(),
                                kind: Kind::AgentStart { session: turn.session.clone() },
                            });
                        }
                        turn.tools += 1;
                        let name = b.get("name").and_then(Value::as_str).unwrap_or("");
                        if matches!(name, "Edit" | "Write" | "MultiEdit" | "NotebookEdit") {
                            let input = b.get("input");
                            if let Some(p) = ["file_path", "notebook_path"]
                                .iter()
                                .find_map(|k| input.and_then(|i| i.get(*k)).and_then(Value::as_str))
                            {
                                turn.files.insert(p.to_string());
                            }
                        }
                    }
                    Some("text") => {
                        turn.reply_words += b.get("text").and_then(Value::as_str).map_or(0, words);
                    }
                    _ => {}
                }
            }
            // Claude Code writes each block of a message on its own line, all with the message's stop_reason, and the
            // thinking comes first: ending the turn there would lose the reply that follows it.
            let thinking_only = blocks.is_some_and(|list| {
                !list.is_empty() && list.iter().all(|b| matches!(b.get("type").and_then(Value::as_str), Some("thinking" | "redacted_thinking")))
            });
            if msg.and_then(|m| m.get("stop_reason")).and_then(Value::as_str) == Some("end_turn") && !thinking_only {
                turn.close(ts, out);
            }
        }
        "user" => turn.last_ts = ts,
        "system" => {
            if matches!(e.get("subtype").and_then(Value::as_str), Some("turn_duration" | "stop_hook_summary")) {
                turn.close(ts, out);
            }
        }
        _ => {}
    }
}

// A prompt typed while the agent was still running gets folded into the running turn.
fn version(e: &Value) -> Option<String> {
    e.get("version").and_then(Value::as_str).map(str::to_string)
}

fn queued_prompt(e: &Value) -> Option<&str> {
    let a = e.get("attachment")?;
    let human = a.get("type").and_then(Value::as_str) == Some("queued_command")
        && a.get("commandMode").and_then(Value::as_str) == Some("prompt")
        && a.pointer("/origin/kind").and_then(Value::as_str) == Some("human");
    human.then(|| a.get("prompt").and_then(Value::as_str)).flatten()
}

impl Source for Claude {
    fn harness(&self) -> &'static str {
        "claude-code"
    }

    fn name(&self) -> &'static str {
        "claude_code"
    }

    fn poll(&mut self) -> Vec<Found> {
        let mut out = Vec::new();
        let turns = &mut self.turns;
        self.tail.poll(|path, line| {
            if let Ok(e) = serde_json::from_str::<Value>(line) {
                let turn = turns
                    .entry(path.to_path_buf())
                    .or_insert_with(|| Turn { folder: cowork_folder(path), sub: subagent(path), ..Turn::default() });
                feed(turn, &e, &mut out);
            }
        });
        self.turns.retain(|p, _| p.exists());
        // After the transcripts, so a prompt just read finds its history line.
        self.history.refresh();
        self.history.attach(&mut out);
        out.sort_by(|a, b| a.ts.total_cmp(&b.ts));
        out
    }

    fn state(&self) -> Value {
        self.tail.state()
    }

    fn restore(&mut self, state: &Value) {
        self.tail.restore(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(lines: &[&str]) -> Vec<Found> {
        let mut t = Turn::default();
        let mut out = Vec::new();
        for l in lines {
            feed(&mut t, &serde_json::from_str(l).unwrap(), &mut out);
        }
        out
    }

    const P: &str = r#"{"type":"user","sessionId":"s1","cwd":"/w/p","version":"2.1.330","timestamp":"2026-11-19T10:00:00Z","message":{"role":"user","content":"please fix the failing parser test now"}}"#;
    const TOOL: &str = r#"{"type":"assistant","timestamp":"2026-11-19T10:00:05Z","message":{"model":"claude-opus-5-5","stop_reason":"tool_use","content":[{"type":"tool_use","name":"Edit","input":{"file_path":"/w/p/a.rs"}}]}}"#;
    const RES: &str = r#"{"type":"user","timestamp":"2026-11-19T10:00:06Z","message":{"role":"user","content":[{"type":"tool_result","content":"ok"}]}}"#;
    const DONE: &str = r#"{"type":"assistant","timestamp":"2026-11-19T10:01:00Z","message":{"model":"claude-opus-5-5","stop_reason":"end_turn","content":[{"type":"text","text":"Fixed it in a.rs."}]}}"#;
    const DUR: &str = r#"{"type":"system","subtype":"turn_duration","timestamp":"2026-11-19T10:01:01Z"}"#;

    #[test]
    fn a_session_in_another_config_folder_names_that_folder() {
        let home = std::env::temp_dir().join(format!("kairos-claude-dirs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        for (dir, session) in [(".claude", "main"), (".claude-work", "work"), (".claude-old", "old"), (".config", "other")] {
            let project = home.join(dir).join("projects/-w-p");
            std::fs::create_dir_all(&project).unwrap();
            std::fs::write(project.join(format!("{session}.jsonl")), "").unwrap();
        }
        let usual = home.join(".claude");
        assert_eq!(other_config_dir_in(&home, &usual, "work"), Some(home.join(".claude-work")));
        assert_eq!(other_config_dir_in(&home, &usual, "main"), None, "wakatime-cli reads this one anyway");
        assert_eq!(other_config_dir_in(&home, &usual, "other"), None, "not a Claude Code folder");
        assert_eq!(other_config_dir_in(&home, &usual, "gone"), None);
        assert_eq!(other_config_dir_in(&home, &usual, "../../.claude-work/projects/-w-p/work"), None);
        assert_eq!(other_config_dir_in(&home, &home.join(".claude-work"), "main"), Some(usual), "kairosd itself was pointed at another");
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn a_turn_becomes_prompt_start_done() {
        let out = run(&[P, TOOL, RES, DONE, DUR]);
        let kinds: Vec<&str> = out.iter().map(|f| f.kind.name()).collect();
        assert_eq!(kinds, ["prompt", "agent_start", "reply_done"]);
        assert_eq!(out[0].cwd.as_deref(), Some(Path::new("/w/p")));
        assert!(matches!(&out[0].kind, Kind::Prompt { version: Some(v), .. } if v == "2.1.330"));
        match &out[2].kind {
            Kind::ReplyDone { reply_words, tools, files_changed, model, .. } => {
                assert_eq!((*reply_words, *tools, *files_changed), (4, 1, 1));
                assert_eq!(model.as_deref(), Some("claude-opus-5-5"));
            }
            k => panic!("{k:?}"),
        }
        assert_eq!(out[2].ts - out[0].ts, 60.0);
    }

    #[test]
    fn a_thinking_line_does_not_end_the_turn_before_the_reply_text() {
        let think = r#"{"type":"assistant","timestamp":"2026-11-19T10:00:57Z","message":{"model":"claude-opus-5-5","stop_reason":"end_turn","content":[{"type":"thinking","thinking":"","signature":"x"}]}}"#;
        let text = r#"{"type":"assistant","timestamp":"2026-11-19T10:01:03Z","message":{"model":"claude-opus-5-5","stop_reason":"end_turn","content":[{"type":"text","text":"It was the parser, fixed in a.rs and tested."}]}}"#;
        let out = run(&[P, think, text, DUR]);
        let kinds: Vec<&str> = out.iter().map(|f| f.kind.name()).collect();
        assert_eq!(kinds, ["prompt", "reply_done"]);
        match &out[1].kind {
            Kind::ReplyDone { reply_words, .. } => assert_eq!(*reply_words, 9),
            k => panic!("{k:?}"),
        }
        assert_eq!(out[1].ts - out[0].ts, 63.0, "done when the text came, not the thinking");
        let out = run(&[P, think, DUR]);
        assert_eq!(out.last().map(|f| (f.kind.name(), f.ts - out[0].ts)), Some(("reply_done", 61.0)), "a turn of only thinking ends at turn_duration");
    }

    #[test]
    fn prompt_typed_mid_turn_counts_without_closing_the_turn() {
        let q = r#"{"type":"attachment","sessionId":"s1","timestamp":"2026-11-19T10:00:07Z","attachment":{"type":"queued_command","prompt":"also update the docs please","commandMode":"prompt","origin":{"kind":"human"}}}"#;
        let out = run(&[P, TOOL, RES, q, DONE]);
        let kinds: Vec<&str> = out.iter().map(|f| f.kind.name()).collect();
        assert_eq!(kinds, ["prompt", "agent_start", "prompt", "reply_done"]);
    }

    #[test]
    fn interrupted_turn_closes_when_next_prompt_arrives() {
        let p2 = P.replace("10:00:00", "10:02:00");
        let out = run(&[P, TOOL, RES, &p2]);
        let kinds: Vec<&str> = out.iter().map(|f| f.kind.name()).collect();
        assert_eq!(kinds, ["prompt", "agent_start", "reply_done", "prompt"]);
        assert_eq!(out[2].ts, out[1].ts + 1.0);
    }

    #[test]
    fn esc_closes_the_turn_where_it_was_pressed_and_nothing_left_over_closes_the_next() {
        let int = |text: &str| {
            format!(r#"{{"type":"user","sessionId":"s1","cwd":"/w/p","timestamp":"2026-11-19T10:00:30Z","message":{{"role":"user","content":[{{"type":"text","text":"{text}"}}]}}}}"#)
        };
        let late = DUR.replace("10:01:01", "10:00:31");
        let p2 = P.replace("10:00:00", "10:02:00");
        let tool2 = TOOL.replace("10:00:05", "10:02:05");
        for text in ["[Request interrupted by user]", "[Request interrupted by user for tool use]"] {
            let out = run(&[P, TOOL, RES, &int(text), &late, &p2, &tool2]);
            let kinds: Vec<&str> = out.iter().map(|f| f.kind.name()).collect();
            assert_eq!(kinds, ["prompt", "agent_start", "reply_done", "prompt", "agent_start"], "{text}");
            assert_eq!(out[2].ts - out[0].ts, 30.0);
        }
        let quoted = P.replace("please fix the failing parser test now", "why does it say [Request interrupted by user] here");
        let out = run(&[P, TOOL, &quoted]);
        let kinds: Vec<&str> = out.iter().map(|f| f.kind.name()).collect();
        assert_eq!(kinds, ["prompt", "agent_start", "reply_done", "prompt"], "a prompt that mentions the marker is a prompt");
    }

    #[test]
    fn tail_skips_history_then_reads_appends() {
        let dir = std::env::temp_dir().join(format!("kairos-claude-src-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("proj")).unwrap();
        let f = dir.join("proj/s1.jsonl");
        std::fs::write(&f, format!("{P}\n{TOOL}\n")).unwrap();
        let mut src = Claude::new(vec![dir.clone()]);
        assert!(src.poll().is_empty());
        let mut text = std::fs::read_to_string(&f).unwrap();
        text.push_str(&format!("{}\n{RES}\n{DONE}", P.replace("10:00:00", "10:05:00")));
        std::fs::write(&f, &text).unwrap();
        let kinds: Vec<&str> = src.poll().iter().map(|f| f.kind.name()).collect();
        assert_eq!(kinds, ["prompt"]);
        std::fs::write(&f, format!("{text}\n")).unwrap();
        let kinds: Vec<&str> = src.poll().iter().map(|f| f.kind.name()).collect();
        assert_eq!(kinds, ["reply_done"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn desktop_vscode_and_t3_prompts_count_but_sdk_compact_and_sidechains_do_not() {
        for ep in ["claude-desktop", "claude-vscode", "t3-code", "local-agent"] {
            let p = P.replace(r#""type":"user","#, &format!(r#""type":"user","entrypoint":"{ep}","#));
            assert_eq!(run(&[&p, TOOL, RES, DONE]).len(), 3, "{ep}");
        }
        let sdk = P.replace(r#""type":"user","#, r#""type":"user","entrypoint":"sdk-cli","#);
        assert!(run(&[&sdk, TOOL, RES, DONE]).is_empty());
        let compact = P.replace(r#""type":"user","#, r#""type":"user","isCompactSummary":true,"#);
        assert!(run(&[&compact, TOOL, RES, DONE]).is_empty());
        let side = P.replace(r#""type":"user","#, r#""type":"user","isSidechain":true,"#);
        assert!(run(&[&side]).is_empty());
    }

    #[test]
    fn cowork_sessions_use_the_picked_folder_and_skip_audit_logs() {
        let dir = std::env::temp_dir().join(format!("kairos-claude-cowork-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let org = dir.join(COWORK).join("acc/org");
        let projects = org.join("local_1/.claude/projects/-sandbox-outputs");
        std::fs::create_dir_all(&projects).unwrap();
        std::fs::write(org.join("local_1.json"), r#"{"cwd":"/sandbox/outputs","userSelectedFolders":["/w/picked"]}"#).unwrap();
        let f = projects.join("s1.jsonl");
        std::fs::write(&f, "").unwrap();
        std::fs::write(org.join("local_1/audit.jsonl"), "").unwrap();
        let mut src = Claude::new(vec![dir.join(COWORK)]);
        assert!(src.poll().is_empty());
        let p = P.replace(r#""cwd":"/w/p""#, r#""cwd":"/sandbox/outputs""#);
        std::fs::write(&f, format!("{p}\n")).unwrap();
        std::fs::write(org.join("local_1/audit.jsonl"), format!("{P}\n")).unwrap();
        let out = src.poll();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].cwd.as_deref(), Some(Path::new("/w/picked")));

        let mut again = Claude::new(vec![dir.join(COWORK)]);
        again.restore(&src.state());
        assert!(again.poll().is_empty());
        std::fs::write(&f, format!("{p}\n{}\n", p.replace("10:00:00", "10:05:00"))).unwrap();
        let out = again.poll();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].cwd.as_deref(), Some(Path::new("/w/picked")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn result(id: &str, patch: &str) -> String {
        format!(
            r#"{{"type":"user","sessionId":"s1","cwd":"/w/p","timestamp":"2026-11-19T10:00:06Z","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"{id}","content":"done"}}]}},"toolUseResult":{patch}}}"#
        )
    }

    fn bash(id: &str, command: &str) -> String {
        format!(
            r#"{{"type":"assistant","sessionId":"s1","cwd":"/w/p","timestamp":"2026-11-19T10:00:05Z","message":{{"stop_reason":"tool_use","content":[{{"type":"tool_use","id":"{id}","name":"Bash","input":{{"command":{command:?}}}}}]}}}}"#
        )
    }

    fn bash_result(id: &str, stdout: &str, error: bool) -> String {
        let r = if error {
            format!("{stdout:?}")
        } else {
            format!(r#"{{"stdout":{stdout:?},"stderr":"","interrupted":false}}"#)
        };
        format!(
            r#"{{"type":"user","sessionId":"s1","cwd":"/w/p","timestamp":"2026-11-19T10:00:07Z","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"{id}","is_error":{error},"content":{stdout:?}}}]}},"toolUseResult":{r}}}"#
        )
    }

    fn edits(out: &[Found]) -> Vec<(String, u32, u32)> {
        out.iter()
            .filter_map(|f| match &f.kind {
                Kind::AgentEdit { path, added, removed, .. } => Some((path.clone(), *added, *removed)),
                _ => None,
            })
            .collect()
    }

    fn commits(out: &[Found]) -> Vec<(String, Option<String>)> {
        out.iter()
            .filter_map(|f| match &f.kind {
                Kind::AgentCommit { sha, .. } => Some((sha.clone().unwrap_or_default(), f.cwd.as_ref().map(|c| c.to_string_lossy().replace('\\', "/")))),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn edits_count_patch_lines_and_leave_the_turn_alone() {
        let one = r#"{"filePath":"/w/p/a.rs","oldString":"x","newString":"y","structuredPatch":[{"oldStart":1,"oldLines":3,"newStart":1,"newLines":3,"lines":[" keep","-old","+new"," keep"]}],"userModified":false}"#;
        let multi = r#"{"filePath":"/w/p/b.rs","type":"update","structuredPatch":[{"oldStart":1,"oldLines":2,"newStart":1,"newLines":3,"lines":["-a","+b","+c"," d"]},{"oldStart":40,"oldLines":2,"newStart":41,"newLines":1,"lines":[" e","-f","\\ No newline at end of file"]}],"content":"ignored"}"#;
        let create = r#"{"filePath":"/w/p/new.rs","type":"create","structuredPatch":[],"content":"one\ntwo\nthree\n","originalFile":null}"#;
        let same = r#"{"filePath":"/w/p/same.rs","type":"update","structuredPatch":[],"content":"x"}"#;
        let (r1, r2, r3, r4) = (result("e1", one), result("e2", multi), result("e3", create), result("e4", same));
        let out = run(&[P, TOOL, &r1, &r2, &r3, &r4, DONE, DUR]);
        assert_eq!(edits(&out), [("/w/p/a.rs".into(), 1, 1), ("/w/p/b.rs".into(), 2, 2), ("/w/p/new.rs".into(), 3, 0)]);
        assert!(out.iter().all(|f| f.cwd.as_deref() == Some(Path::new("/w/p"))));
        assert!(matches!(&out[2].kind, Kind::AgentEdit { session, .. } if session == "s1"));
        let rest: Vec<&str> = out.iter().filter(|f| !matches!(f.kind, Kind::AgentEdit { .. })).map(|f| f.kind.name()).collect();
        assert_eq!(rest, ["prompt", "agent_start", "reply_done"]);
        match &out.last().unwrap().kind {
            Kind::ReplyDone { tools, files_changed, .. } => assert_eq!((*tools, *files_changed), (1, 1)),
            k => panic!("{k:?}"),
        }
    }

    #[test]
    fn rejected_and_failed_edits_make_nothing() {
        let rejected = r#"{"type":"user","sessionId":"s1","cwd":"/w/p","timestamp":"2026-11-19T10:00:06Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"e1","is_error":true,"content":"The user doesn't want to proceed with this tool use."}]},"toolUseResult":"User rejected tool use"}"#;
        let failed = result("e2", r#""Error: String to replace not found in file.""#).replace(r#""content":"done""#, r#""is_error":true,"content":"x""#);
        let none = result("e3", "null").replace(r#","toolUseResult":null"#, "");
        assert!(edits(&run(&[P, TOOL, rejected, &failed, &none, DONE])).is_empty());
    }

    #[test]
    fn edits_made_outside_a_turn_still_count() {
        let sdk = P.replace(r#""type":"user","#, r#""type":"user","entrypoint":"sdk-cli","#);
        let patch = r#"{"filePath":"/w/p/a.rs","structuredPatch":[{"lines":["+x"]}]}"#;
        let out = run(&[&sdk, &result("e1", patch)]);
        assert_eq!(edits(&out), [("/w/p/a.rs".into(), 1, 0)]);
        assert_eq!(out.len(), 1);
    }

    fn shell_result(id: &str, stdout: &str, error: bool, diff: &str) -> String {
        format!(
            r#"{{"type":"user","sessionId":"s1","cwd":"/w/p","timestamp":"2026-11-19T10:00:07Z","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"{id}","is_error":{error},"content":{stdout:?}}}]}},"toolUseResult":{{"stdout":{stdout:?},"stderr":"","bashEditDiff":{diff}}}}}"#
        )
    }

    const SHELL_DIFF: &str = r#"{"files":[{"filePath":"/w/p/a.rs","hunks":[{"oldStart":1,"oldLines":2,"newStart":1,"newLines":3,"lines":[" keep","-old","+new","+more"]},{"oldStart":9,"oldLines":2,"newStart":10,"newLines":1,"lines":["-gone"," keep"]}]},{"filePath":"/w/p/b.rs","hunks":[{"oldStart":1,"oldLines":1,"newStart":1,"newLines":1,"lines":["-x","+y"]}]},{"filePath":"/w/p/nohunks.rs"}],"moreFiles":2,"changedFiles":["/w/p/a.rs","/w/p/b.rs","/w/p/c.rs","/w/p/d.rs"]}"#;

    #[test]
    fn shell_edits_count_every_file_with_hunks() {
        let edit = result("e1", r#"{"filePath":"/w/p/e.rs","structuredPatch":[{"lines":["+x"]}]}"#);
        let out = run(&[P, &bash("b1", "sed -i s/old/new/ a.rs"), &shell_result("b1", "", false, SHELL_DIFF), &edit, DONE]);
        assert_eq!(edits(&out), [("/w/p/a.rs".into(), 2, 2), ("/w/p/b.rs".into(), 1, 1), ("/w/p/e.rs".into(), 1, 0)]);
    }

    #[test]
    fn shell_edits_count_when_the_command_failed_or_committed() {
        let failed = run(&[P, &bash("b1", "python fix.py"), &shell_result("b1", "boom", true, SHELL_DIFF), DONE]);
        assert_eq!(edits(&failed), [("/w/p/a.rs".into(), 2, 2), ("/w/p/b.rs".into(), 1, 1)]);

        let printed = "[main abc1234] fix\n 2 files changed\n";
        let both = run(&[P, &bash("b1", "sed -i s/old/new/ a.rs && git commit -am fix"), &shell_result("b1", printed, false, SHELL_DIFF), DONE]);
        assert_eq!(edits(&both).len(), 2);
        assert_eq!(commits(&both), [("abc1234".into(), Some("/w/p".into()))]);
    }

    #[test]
    fn lines_git_brings_back_are_not_shell_edits() {
        for cmd in ["git stash pop", "cd /w/p && git -C q checkout -- a.rs", "bash -c 'git reset --hard HEAD~1'"] {
            let out = run(&[P, &bash("b1", cmd), &shell_result("b1", "", false, SHELL_DIFF), DONE]);
            assert!(edits(&out).is_empty(), "{cmd}");
        }
        let printed = "[main abc1234] fix\n";
        let both = run(&[P, &bash("b1", "sed -i s/old/new/ a.rs && git commit -am fix"), &shell_result("b1", printed, false, SHELL_DIFF), DONE]);
        assert_eq!((edits(&both).len(), commits(&both).len()), (2, 1));
        let quoted = run(&[P, &bash("b1", "sed -i s/old/new/ a.rs && git commit -m 'undo git checkout and git stash pop'"), &shell_result("b1", printed, false, SHELL_DIFF), DONE]);
        assert_eq!((edits(&quoted).len(), commits(&quoted).len()), (2, 1));
        let apply = run(&[P, &bash("b1", "git apply fix.patch"), &shell_result("b1", "", false, SHELL_DIFF), DONE]);
        assert_eq!(edits(&apply).len(), 2);
    }

    #[test]
    fn a_commit_is_a_bash_call_whose_output_printed_a_sha() {
        let printed = "[main abc1234] add the thing\n 1 file changed, 1 insertion(+)\n";
        let out = run(&[P, TOOL, &bash("b1", "git add -A && git commit -m 'add the thing'"), &bash_result("b1", printed, false), DONE]);
        assert_eq!(commits(&out), [("abc1234".into(), Some("/w/p".into()))]);
        assert!(matches!(&out.iter().find(|f| f.kind.name() == "agent_commit").unwrap().kind, Kind::AgentCommit { session, .. } if session == "s1"));

        let other = run(&[P, &bash("b1", "git -C ../q commit -m x"), &bash_result("b1", "[feat (root-commit) 0123456789ab] x\n", false)]);
        assert_eq!(commits(&other), [("0123456789ab".into(), Some("/w/p/../q".into()))]);

        let chained = run(&[P, &bash("b1", "cd /w/q && git commit -m x && cargo test"), &bash_result("b1", "[detached HEAD abcdef0] x\n", false)]);
        assert_eq!(commits(&chained), [("abcdef0".into(), Some("/w/q".into()))]);
    }

    #[test]
    fn other_git_commands_and_failed_commits_make_nothing() {
        let printed = "[main abc1234] add the thing\n";
        let status = run(&[P, &bash("b1", "git status"), &bash_result("b1", printed, false)]);
        assert!(commits(&status).is_empty());
        let empty = run(&[P, &bash("b1", "git commit -m x"), &bash_result("b1", "nothing to commit, working tree clean\n", false)]);
        assert!(commits(&empty).is_empty());
        let hook = run(&[P, &bash("b1", "git commit -m x"), &bash_result("b1", "Exit code 1\nhook failed\n", true)]);
        assert!(commits(&hook).is_empty());
        let unmatched = run(&[P, &bash("b1", "git commit -m x"), &bash_result("b2", printed, false)]);
        assert!(commits(&unmatched).is_empty());
        let echoed = run(&[P, &bash("b1", "echo 'git commit'"), &bash_result("b1", printed, false)]);
        assert!(commits(&echoed).is_empty());
    }

    #[test]
    fn subagent_edits_and_commits_count_but_not_their_turns() {
        let patch = r#"{"filePath":"/w/p/a.rs","structuredPatch":[{"lines":["+x","-y"]}]}"#;
        let side = |l: &str| {
            l.replace(r#""type":"user","#, r#""type":"user","isSidechain":true,"#)
                .replace(r#""type":"assistant","#, r#""type":"assistant","isSidechain":true,"#)
        };
        let out = run(&[
            &side(P),
            &side(TOOL),
            &side(&result("e1", patch)),
            &side(&bash("b1", "git commit -m x")),
            &side(&bash_result("b1", "[main abc1234] x\n", false)),
            &side(DONE),
        ]);
        assert_eq!(edits(&out), [("/w/p/a.rs".into(), 1, 1)]);
        assert_eq!(commits(&out).len(), 1);
        assert_eq!(out.len(), 2);

        let mut t = Turn { sub: true, ..Turn::default() };
        let mut out = Vec::new();
        for l in [P, TOOL, &result("e1", patch), DONE] {
            feed(&mut t, &serde_json::from_str(l).unwrap(), &mut out);
        }
        assert_eq!(out.len(), 1);
        assert_eq!(edits(&out).len(), 1);
    }

    #[test]
    fn subagent_files_are_followed_for_their_work() {
        let dir = std::env::temp_dir().join(format!("kairos-claude-sub-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sub = dir.join("proj/s1/subagents");
        std::fs::create_dir_all(&sub).unwrap();
        let f = sub.join("agent-a1.jsonl");
        std::fs::write(&f, "").unwrap();
        let mut src = Claude::new(vec![dir.clone()]);
        assert!(src.poll().is_empty());
        let patch = r#"{"filePath":"/w/p/a.rs","structuredPatch":[{"lines":["+x"]}]}"#;
        let line = result("e1", patch).replace(r#""type":"user","#, r#""type":"user","isSidechain":true,"#);
        std::fs::write(&f, format!("{P}\n{line}\n")).unwrap();
        let out = src.poll();
        assert_eq!(edits(&out), [("/w/p/a.rs".into(), 1, 0)]);
        assert_eq!(out.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    const ASK: &str = r#"{"type":"assistant","sessionId":"s1","cwd":"/w/p","timestamp":"2026-11-19T10:00:10Z","message":{"stop_reason":"tool_use","content":[{"type":"tool_use","id":"q1","name":"AskUserQuestion","input":{"questions":[{"question":"Which parser should I fix first?","header":"Parser","multiSelect":false,"options":[{"label":"Lexer","description":"Fix the lexer"},{"label":"Grammar","description":"Fix the grammar rules"}]}]}}]}}"#;
    const ANSWER: &str = r#"{"type":"user","sessionId":"s1","cwd":"/w/p","timestamp":"2026-11-19T10:00:40Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"q1","content":"User has answered your questions"}]},"toolUseResult":{"questions":[],"answers":{"Which parser should I fix first?":"Lexer first please"},"annotations":{}},"permissionDecision":{"decision":"accept","source":"user_temporary","grantScope":"once"}}"#;
    const PLAN: &str = r#"{"type":"assistant","sessionId":"s1","cwd":"/w/p","timestamp":"2026-11-19T10:00:20Z","message":{"stop_reason":"tool_use","content":[{"type":"tool_use","id":"pl1","name":"ExitPlanMode","input":{"plan":"Step one do this. Step two do that.","planFilePath":"/w/p/plan.md"}}]}}"#;
    const PLAN_OK: &str = r#"{"type":"user","sessionId":"s1","cwd":"/w/p","timestamp":"2026-11-19T10:00:50Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"pl1","content":"User has approved your plan"}]},"toolUseResult":{"plan":"x","isAgent":false,"filePath":"/w/p/plan.md"}}"#;

    fn answers(out: &[Found]) -> Vec<(f64, crate::event::Kind)> {
        out.iter().filter(|f| f.kind.name() == "answered").map(|f| (f.ts, f.kind.clone())).collect()
    }

    fn decided(mut record: String, source: &str) -> String {
        record.pop();
        format!(r#"{record},"permissionDecision":{{"decision":"accept","source":"{source}"}}}}"#)
    }

    fn base(ts: &str) -> f64 {
        timestamp(&serde_json::from_str::<Value>(&format!(r#"{{"timestamp":"2026-11-19T{ts}Z"}}"#)).unwrap()).unwrap()
    }

    #[test]
    fn a_question_the_person_answered_counts_what_was_shown_and_typed() {
        let out = run(&[P, TOOL, ASK, ANSWER, DONE]);
        let got = answers(&out);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, base("10:00:40"));
        assert_eq!(
            got[0].1,
            Kind::Answered { session: "s1".into(), asked: base("10:00:10"), why: Ask::Question, shown_words: 16, answer_words: 3 }
        );
        assert_eq!(out.iter().filter(|f| f.kind.name() == "reply_done").count(), 1);
    }

    #[test]
    fn a_plan_is_an_ask_with_or_without_a_permission_decision() {
        let got = answers(&run(&[P, TOOL, PLAN, PLAN_OK, DONE]));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].1, Kind::Answered { session: "s1".into(), asked: base("10:00:20"), why: Ask::Plan, shown_words: 8, answer_words: 0 });
        let rejected = decided(PLAN_OK.to_string(), "user_reject");
        assert_eq!(answers(&run(&[P, TOOL, PLAN, &rejected, DONE])).len(), 1);
    }

    #[test]
    fn a_permission_prompt_the_person_answered_counts_but_one_config_decided_does_not() {
        let call = bash("p1", "cargo test --all");
        let by_person = decided(bash_result("p1", "ok", false), "user_temporary");
        let got = answers(&run(&[P, TOOL, &call, &by_person, DONE]));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].1, Kind::Answered { session: "s1".into(), asked: base("10:00:05"), why: Ask::Permission, shown_words: 3, answer_words: 0 });
        assert_eq!(got[0].0, base("10:00:07"));

        let by_config = decided(bash_result("p1", "ok", false), "config");
        assert!(answers(&run(&[P, TOOL, &call, &by_config, DONE])).is_empty());
        assert!(answers(&run(&[P, TOOL, &call, &bash_result("p1", "ok", false), DONE])).is_empty());
        let config_question = decided(ANSWER.replace(r#","permissionDecision":{"decision":"accept","source":"user_temporary","grantScope":"once"}"#, ""), "config");
        assert!(answers(&run(&[P, TOOL, ASK, &config_question, DONE])).is_empty());
        let unmatched = decided(bash_result("other", "ok", false), "user_temporary");
        assert!(answers(&run(&[P, TOOL, &call, &unmatched, DONE])).is_empty());
    }

    #[test]
    fn answers_in_sidechains_and_sdk_sessions_are_skipped() {
        let side = |l: &str| {
            l.replace(r#""type":"user","#, r#""type":"user","isSidechain":true,"#)
                .replace(r#""type":"assistant","#, r#""type":"assistant","isSidechain":true,"#)
        };
        assert!(answers(&run(&[P, TOOL, &side(ASK), &side(ANSWER), DONE])).is_empty());
        let sdk = |l: &str| {
            l.replace(r#""type":"user","#, r#""type":"user","entrypoint":"sdk-cli","#)
                .replace(r#""type":"assistant","#, r#""type":"assistant","entrypoint":"sdk-cli","#)
        };
        assert!(answers(&run(&[P, TOOL, &sdk(ASK), &sdk(ANSWER), DONE])).is_empty());
        let call = sdk(&bash("p1", "ls"));
        assert!(answers(&run(&[P, TOOL, &call, &sdk(&decided(bash_result("p1", "ok", false), "user_temporary")), DONE])).is_empty());
    }

    #[test]
    fn a_permission_prompt_for_a_long_write_shows_at_most_the_cap() {
        let long = "word ".repeat(5000);
        let call = format!(
            r#"{{"type":"assistant","sessionId":"s1","timestamp":"2026-11-19T10:00:05Z","message":{{"stop_reason":"tool_use","content":[{{"type":"tool_use","id":"w1","name":"Write","input":{{"file_path":"/w/p/a.txt","content":"{long}"}}}}]}}}}"#
        );
        let result = decided(result("w1", "null"), "user_temporary");
        let got = answers(&run(&[P, &call, &result, DONE]));
        assert!(matches!(got[0].1, Kind::Answered { shown_words: SHOWN_CAP, .. }), "{got:?}");
    }
}
