use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::snapshot::{now, Snapshots, Turn};
use super::{words, Found, Source};
use crate::{prompt_shape, prompt_stats};

const MAX_FILE: u64 = 64 * 1024 * 1024;
const CWD_MARK: &str = "# Current Working Directory (";
const EDIT_TOOLS: [&str; 8] = [
    "editedExistingFile",
    "newFileCreated",
    "fileDeleted",
    "appliedDiff",
    "writeToFile",
    "insertContent",
    "searchAndReplace",
    "applyPatch",
];
const TOOL_ASKS: [&str; 3] = ["tool", "command", "use_mcp_server"];
const TOOL_SAYS: [&str; 3] = ["tool", "command", "mcp_server_request_started"];
const WAITS: [&str; 6] =
    ["completion_result", "followup", "api_req_failed", "mistake_limit_reached", "resume_task", "resume_completed_task"];
// A subtask whose first message lands this soon after a parent's newTask call is that subtask.
const SPAWN_WINDOW: f64 = 30.0;

// VS Code and its forks keep each extension's tasks under globalStorage/<extension id>/tasks/<id>/.
fn hosts() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(cfg) = dirs::config_dir() {
        for name in ["Code", "Code - Insiders", "VSCodium", "Cursor", "Windsurf", "Kiro"] {
            out.push(cfg.join(name).join("User/globalStorage"));
        }
    }
    if let Some(home) = dirs::home_dir() {
        for d in [".vscode-server/data", ".cursor-server/data", ".windsurf-server/data"] {
            out.push(home.join(d).join("User/globalStorage"));
        }
    }
    out
}

pub fn cline() -> Cline {
    Cline::new("cline", "cline", &["saoudrizwan.claude-dev"], hosts())
}

pub fn roo() -> Cline {
    Cline::new("roo_code", "roo-code", &["rooveterinaryinc.roo-cline", "rooveterinaryinc.roo-code-nightly"], hosts())
}

pub fn kilo() -> Cline {
    Cline::new("kilo_code", "kilo", &["kilocode.kilo-code"], hosts())
}

fn s<'a>(m: &'a Value, k: &str) -> &'a str {
    m.get(k).and_then(Value::as_str).unwrap_or("")
}

fn tool_json(m: &Value) -> Option<Value> {
    serde_json::from_str(m.get("text")?.as_str()?).ok()
}

fn cwd_of(msgs: &[Value]) -> Option<PathBuf> {
    msgs.iter().filter(|m| s(m, "say") == "api_req_started").find_map(|m| {
        let req = tool_json(m)?;
        let text = req.get("request")?.as_str()?;
        let rest = &text[text.find(CWD_MARK)? + CWD_MARK.len()..];
        let line = &rest[..rest.find('\n').unwrap_or(rest.len())];
        let dir = line.trim_end().strip_suffix(") Files").or_else(|| line.trim_end().strip_suffix(')'))?;
        (!dir.is_empty()).then(|| PathBuf::from(dir))
    })
}

fn spawns(msgs: &[Value]) -> Vec<f64> {
    msgs.iter()
        .filter(|m| s(m, "ask") == "tool" || s(m, "say") == "tool")
        .filter(|m| tool_json(m).is_some_and(|t| s(&t, "tool") == "newTask"))
        .filter_map(|m| m.get("ts").and_then(Value::as_f64).map(|t| t / 1000.0))
        .collect()
}

fn turns(msgs: &[Value], cwd: Option<&Path>) -> Vec<Turn> {
    let mut out: Vec<Turn> = Vec::new();
    let mut files: BTreeSet<PathBuf> = BTreeSet::new();
    let mut last = 0.0;
    for (i, m) in msgs.iter().enumerate() {
        let Some(ts) = m.get("ts").and_then(Value::as_f64).map(|t| t / 1000.0) else { continue };
        let (ask, say) = (s(m, "ask"), s(m, "say"));
        let first_text = i == 0 && say == "text";
        if say == "task" || say == "user_feedback" || first_text {
            let (w, pasted) = prompt_stats(s(m, "text"));
            let has_images = m.get("images").and_then(Value::as_array).is_some_and(|a| !a.is_empty());
            if w == 0 && !has_images {
                continue;
            }
            if let Some(t) = out.last_mut().filter(|t| t.done.is_none()) {
                t.files = files.len();
                t.done = Some(f64::min(last, ts));
            }
            files.clear();
            last = ts;
            out.push(Turn { ts, words: w, pasted, shape: Some(prompt_shape(s(m, "text"))), ..Turn::default() });
            continue;
        }
        let Some(t) = out.last_mut().filter(|t| t.done.is_none()) else { continue };
        if m.get("partial").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        if TOOL_ASKS.contains(&ask) || TOOL_SAYS.contains(&say) {
            t.started.get_or_insert(ts);
            t.tools += 1;
            if let Some(tool) = tool_json(m) {
                if EDIT_TOOLS.contains(&s(&tool, "tool")) && !s(&tool, "path").is_empty() {
                    files.insert(cwd.unwrap_or(Path::new("")).join(s(&tool, "path")));
                }
            }
        }
        if say == "text" || say == "completion_result" || ask == "completion_result" || ask == "followup" {
            t.reply_words += words(s(m, "text"));
        }
        if WAITS.contains(&ask) || say == "completion_result" {
            t.files = files.len();
            t.done = Some(if ask.starts_with("resume") { last } else { ts });
            continue;
        }
        last = ts;
    }
    if let Some(t) = out.last_mut().filter(|t| t.done.is_none()) {
        t.files = files.len();
    }
    out
}

pub struct Cline {
    name: &'static str,
    harness: &'static str,
    ids: Vec<&'static str>,
    hosts: Vec<PathBuf>,
    snaps: Snapshots,
    spawned: Vec<f64>,
}

impl Cline {
    pub fn new(name: &'static str, harness: &'static str, ids: &[&'static str], hosts: Vec<PathBuf>) -> Cline {
        Cline { name, harness, ids: ids.to_vec(), hosts, snaps: Snapshots::default(), spawned: Vec::new() }
    }

    // Extension folders are lower-cased by VS Code on some systems but not others.
    fn files(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for host in &self.hosts {
            for e in fs::read_dir(host).into_iter().flatten().flatten() {
                let name = e.file_name().to_string_lossy().to_lowercase();
                if !self.ids.contains(&name.as_str()) {
                    continue;
                }
                for task in fs::read_dir(e.path().join("tasks")).into_iter().flatten().flatten() {
                    let p = task.path().join("ui_messages.json");
                    if p.is_file() {
                        out.push(p);
                    }
                }
            }
        }
        out
    }
}

impl Source for Cline {
    fn name(&self) -> &'static str {
        self.name
    }

    fn harness(&self) -> &'static str {
        self.harness
    }

    fn poll(&mut self) -> Vec<Found> {
        let started = now();
        let mut tasks = Vec::new();
        for path in self.snaps.changed(self.files()) {
            if fs::metadata(&path).map_or(true, |m| m.len() > MAX_FILE) {
                continue;
            }
            let Some(msgs) = fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<Vec<Value>>(&t).ok()) else {
                continue;
            };
            self.spawned.extend(spawns(&msgs));
            let id = path.parent().and_then(Path::file_name).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let cwd = cwd_of(&msgs);
            let ts = turns(&msgs, cwd.as_deref());
            let first = msgs.first().and_then(|m| m.get("ts")).and_then(Value::as_f64).map(|t| t / 1000.0);
            tasks.push((id, cwd, ts, first));
        }
        let mut out = Vec::new();
        for (id, cwd, ts, first) in tasks {
            if first.is_some_and(|f| self.spawned.iter().any(|&s| f >= s && f - s <= SPAWN_WINDOW)) {
                continue;
            }
            self.snaps.emit(&id, cwd.as_deref(), &ts, &mut out);
        }
        self.spawned.retain(|&s| started - s < 3600.0);
        self.snaps.finish(started);
        out.sort_by(|a, b| a.ts.total_cmp(&b.ts));
        out
    }

    fn state(&self) -> Value {
        self.snaps.state()
    }

    fn restore(&mut self, state: &Value) {
        self.snaps.restore(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Kind;
    use serde_json::json;

    fn names(out: &[Found]) -> Vec<&'static str> {
        out.iter().map(|f| f.kind.name()).collect()
    }

    fn msg(ts: f64, kind: &str, what: &str, text: &str) -> Value {
        json!({ "ts": (ts * 1000.0) as i64, "type": kind, kind: what, "text": text })
    }

    fn req(ts: f64) -> Value {
        let request = "<task>\nfix it\n</task>\n<environment_details>\n# Current Working Directory (/w/p) Files\nmain.go\n</environment_details>";
        msg(ts, "say", "api_req_started", &json!({ "request": request, "tokensIn": 1 }).to_string())
    }

    fn tool(ts: f64, kind: &str, name: &str, path: &str) -> Value {
        msg(ts, kind, "tool", &json!({ "tool": name, "path": path, "diff": "<<<<<<< SEARCH" }).to_string())
    }

    fn task(t0: f64) -> Vec<Value> {
        vec![
            msg(t0, "say", "task", "please fix the failing parser test now"),
            req(t0 + 1.0),
            msg(t0 + 2.0, "say", "reasoning", "private reasoning words"),
            msg(t0 + 3.0, "say", "text", "Looking at it."),
            tool(t0 + 5.0, "ask", "editedExistingFile", "src/a.rs"),
            tool(t0 + 6.0, "say", "readFile", "src/b.rs"),
            msg(t0 + 60.0, "say", "completion_result", "Fixed it in a.rs."),
        ]
    }

    #[test]
    fn a_task_becomes_prompt_start_done() {
        let t0 = 1_000.0;
        let msgs = task(t0);
        let cwd = cwd_of(&msgs);
        assert_eq!(cwd.as_deref(), Some(Path::new("/w/p")));
        let ts = turns(&msgs, cwd.as_deref());
        assert_eq!(ts.len(), 1);
        let t = &ts[0];
        assert_eq!((t.ts, t.words, t.started, t.done), (t0, 7, Some(t0 + 5.0), Some(t0 + 60.0)));
        assert_eq!((t.reply_words, t.tools, t.files), (7, 2, 1));
    }

    #[test]
    fn followups_feedback_and_interrupted_tasks() {
        let t0 = 1_000.0;
        let mut msgs = task(t0);
        msgs.truncate(5);
        msgs.push(msg(t0 + 20.0, "ask", "followup", "Which file do you mean?"));
        msgs.push(msg(t0 + 40.0, "say", "user_feedback", "the parser one in src"));
        msgs.push(tool(t0 + 45.0, "say", "command", ""));
        msgs.push(msg(t0 + 50.0, "ask", "resume_task", ""));
        let ts = turns(&msgs, None);
        assert_eq!(ts.len(), 2);
        assert_eq!(ts[0].done, Some(t0 + 20.0));
        assert_eq!((ts[1].words, ts[1].started, ts[1].done), (5, Some(t0 + 45.0), Some(t0 + 45.0)));
    }

    #[test]
    fn reads_tasks_skips_subtasks_partial_writes_and_resumes() {
        let dir = std::env::temp_dir().join(format!("kairos-cline-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let ext = dir.join("RooVeterinaryInc.roo-cline/tasks");
        fs::create_dir_all(ext.join("t1")).unwrap();
        fs::create_dir_all(ext.join("t2")).unwrap();
        let mut src = Cline::new("roo_code", "roo-code", &["rooveterinaryinc.roo-cline"], vec![dir.clone()]);
        assert!(src.poll().is_empty());

        let t0 = now().ceil() + 1.0;
        let mut msgs = task(t0);
        msgs[0] = msg(t0, "say", "text", "please fix the failing parser test now");
        msgs.truncate(6);
        msgs.push(msg(t0 + 7.0, "ask", "tool", &json!({ "tool": "newTask", "mode": "code", "content": "x" }).to_string()));
        fs::write(ext.join("t1/ui_messages.json"), Value::Array(msgs.clone()).to_string()).unwrap();
        let sub = vec![msg(t0 + 8.0, "say", "text", "look into the parser module please"), req(t0 + 9.0)];
        fs::write(ext.join("t2/ui_messages.json"), Value::Array(sub).to_string()).unwrap();
        let out = src.poll();
        assert_eq!(names(&out), ["prompt", "agent_start"]);
        assert_eq!(out[0].cwd.as_deref(), Some(Path::new("/w/p")));
        assert!(matches!(&out[0].kind, Kind::Prompt { session, words: 7, .. } if session == "t1"));

        let full = Value::Array(msgs.clone()).to_string();
        fs::write(ext.join("t1/ui_messages.json"), &full[..full.len() - 20]).unwrap();
        assert!(src.poll().is_empty());

        let mut again = Cline::new("roo_code", "roo-code", &["rooveterinaryinc.roo-cline"], vec![dir.clone()]);
        again.restore(&src.state());
        msgs.push(msg(t0 + 60.0, "say", "completion_result", "Fixed it in a.rs."));
        fs::write(ext.join("t1/ui_messages.json"), Value::Array(msgs).to_string()).unwrap();
        let out = again.poll();
        assert_eq!(names(&out), ["reply_done"]);
        assert!(matches!(&out[0].kind, Kind::ReplyDone { tools: 3, files_changed: 1, .. }));
        assert!(again.poll().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}
