use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::DateTime;
use serde_json::Value;
use walkdir::WalkDir;

const SKIP_PREFIXES: [&str; 8] = [
    "<command-name>",
    "<command-message>",
    "<local-command",
    "Caveat:",
    "<task-notification>",
    "<bash-input>",
    "<bash-stdout>",
    "<user-prompt-submit-hook>",
];

#[derive(Debug, Clone)]
pub struct Turn {
    pub start: f64,
    pub end: f64,
    pub words: usize,
    pub paste: bool,
    pub reply_words: usize,
    pub tools: usize,
    pub model: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Session {
    pub path: String,
    pub project: String,
    pub turns: Vec<Turn>,
    pub interrupts: usize,
}

enum Kind {
    Prompt(String),
    Interrupt,
    Other,
}

pub fn find_session_files(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut found = BTreeSet::new();
    for dir in dirs {
        for entry in WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
            let p = entry.path();
            if !entry.file_type().is_file() || p.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            if p.components().any(|c| c.as_os_str() == "subagents") {
                continue;
            }
            found.insert(p.to_path_buf());
        }
    }
    found.into_iter().collect()
}

pub fn parse_file(path: &Path) -> Result<Session> {
    let bytes = std::fs::read(path)?;
    let content = String::from_utf8_lossy(&bytes);
    let project = path
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(parse_session(&content, &path.to_string_lossy(), &project))
}

pub fn parse_session(content: &str, path: &str, project: &str) -> Session {
    let mut events: Vec<(f64, Value)> = Vec::new();
    for line in content.lines() {
        let Ok(e) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if e.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let Some(t) = timestamp(&e) else { continue };
        events.push((t, e));
    }
    events.sort_by(|a, b| a.0.total_cmp(&b.0));

    let mut turns = Vec::new();
    let mut interrupts = 0;
    let mut cur: Option<Turn> = None;
    for (t, e) in &events {
        match classify(e) {
            Kind::Prompt(text) => {
                if let Some(c) = cur.take() {
                    turns.push(c);
                }
                if e.get("entrypoint").and_then(Value::as_str).unwrap_or("cli") != "cli" {
                    continue;
                }
                cur = Some(Turn {
                    start: *t,
                    end: *t,
                    words: text.split_whitespace().count(),
                    paste: is_paste(&text),
                    reply_words: 0,
                    tools: 0,
                    model: None,
                });
                continue;
            }
            Kind::Interrupt => interrupts += 1,
            Kind::Other => {}
        }
        let Some(c) = cur.as_mut() else { continue };
        let ty = e.get("type").and_then(Value::as_str);
        if ty != Some("assistant") && ty != Some("user") {
            continue;
        }
        c.end = c.end.max(*t);
        if ty == Some("assistant") {
            let msg = e.get("message");
            if let Some(m) = msg.and_then(|m| m.get("model")).and_then(Value::as_str) {
                if !m.is_empty() && m != "<synthetic>" {
                    c.model = Some(m.to_string());
                }
            }
            if let Some(blocks) = msg.and_then(|m| m.get("content")).and_then(Value::as_array) {
                for b in blocks {
                    match b.get("type").and_then(Value::as_str) {
                        Some("tool_use") => c.tools += 1,
                        Some("text") => {
                            c.reply_words += b
                                .get("text")
                                .and_then(Value::as_str)
                                .map_or(0, |s| s.split_whitespace().count());
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    if let Some(c) = cur {
        turns.push(c);
    }
    Session {
        path: path.to_string(),
        project: project.to_string(),
        turns,
        interrupts,
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct LastTurn {
    pub reply_words: usize,
    pub tools: usize,
    pub files_changed: usize,
    pub model: Option<String>,
}

pub fn prompt_stats(text: &str) -> (usize, bool) {
    let text = strip_reminders(text);
    let text = text.trim();
    (text.split_whitespace().count(), is_paste(text))
}

pub fn last_turn(content: &str) -> LastTurn {
    let mut out = LastTurn::default();
    let mut files: BTreeSet<String> = BTreeSet::new();
    for line in content.lines().rev() {
        let Ok(e) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if e.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        if matches!(classify(&e), Kind::Prompt(_)) {
            break;
        }
        if e.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let msg = e.get("message");
        if out.model.is_none() {
            out.model = msg
                .and_then(|m| m.get("model"))
                .and_then(Value::as_str)
                .filter(|m| !m.is_empty() && *m != "<synthetic>")
                .map(str::to_string);
        }
        let Some(blocks) = msg.and_then(|m| m.get("content")).and_then(Value::as_array) else {
            continue;
        };
        for b in blocks {
            match b.get("type").and_then(Value::as_str) {
                Some("tool_use") => {
                    out.tools += 1;
                    let name = b.get("name").and_then(Value::as_str).unwrap_or("");
                    if matches!(name, "Edit" | "Write" | "MultiEdit" | "NotebookEdit") {
                        let input = b.get("input");
                        let path = ["file_path", "notebook_path"]
                            .iter()
                            .find_map(|k| input.and_then(|i| i.get(*k)).and_then(Value::as_str));
                        if let Some(p) = path {
                            files.insert(p.to_string());
                        }
                    }
                }
                Some("text") => {
                    out.reply_words += b
                        .get("text")
                        .and_then(Value::as_str)
                        .map_or(0, |s| s.split_whitespace().count());
                }
                _ => {}
            }
        }
    }
    out.files_changed = files.len();
    out
}

pub(crate) fn timestamp(e: &Value) -> Option<f64> {
    let s = e.get("timestamp")?.as_str()?;
    let d = DateTime::parse_from_rfc3339(s).ok()?;
    Some(d.timestamp_millis() as f64 / 1000.0)
}

pub(crate) enum UserLine {
    Prompt(String),
    Interrupt,
    Other,
}

pub(crate) fn user_line(e: &Value) -> UserLine {
    match classify(e) {
        Kind::Prompt(t) => UserLine::Prompt(t),
        Kind::Interrupt => UserLine::Interrupt,
        Kind::Other => UserLine::Other,
    }
}

fn classify(e: &Value) -> Kind {
    if e.get("type").and_then(Value::as_str) != Some("user")
        || e.get("isMeta").and_then(Value::as_bool) == Some(true)
        || e.get("isCompactSummary").and_then(Value::as_bool) == Some(true)
    {
        return Kind::Other;
    }
    let text = match e.get("message").and_then(|m| m.get("content")) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => {
            if blocks
                .iter()
                .any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
            {
                return Kind::Other;
            }
            blocks
                .iter()
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(" ")
        }
        _ => return Kind::Other,
    };
    let text = unwrap_pastes(&strip_reminders(&text));
    let text = text.trim();
    if text.is_empty() || SKIP_PREFIXES.iter().any(|p| text.starts_with(p)) {
        return Kind::Other;
    }
    // Claude Code's own line when Esc stops a turn: "[Request interrupted by user]", or "... for tool use]".
    if text.starts_with("[Request interrupted by user") {
        return Kind::Interrupt;
    }
    Kind::Prompt(text.to_string())
}

pub(crate) fn strip_reminders(s: &str) -> String {
    const OPEN: &str = "<system-reminder>";
    const CLOSE: &str = "</system-reminder>";
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find(OPEN) {
        let after = &rest[i + OPEN.len()..];
        let Some(j) = after.find(CLOSE) else { break };
        out.push_str(&rest[..i]);
        rest = &after[j + CLOSE.len()..];
    }
    out.push_str(rest);
    out
}

/// Claude Code 2.1 writes a paste into the transcript on its own lines between `<pasted_content id="…">` tags, after a
/// blank line. What the person sent is the text around it with the paste in its place.
pub(crate) fn unwrap_pastes(s: &str) -> String {
    const OPEN: &str = "<pasted_content id=\"";
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find(OPEN) {
        let after = &rest[i + OPEN.len()..];
        let Some(end) = after.find("\">") else { break };
        let id = &after[..end];
        if id.is_empty() || id.len() > 32 || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
            break;
        }
        let body = &after[end + 2..];
        let close = format!("</pasted_content id=\"{id}\">");
        let Some(j) = body.find(&close) else { break };
        let before = &rest[..i];
        out.push_str(before.strip_suffix("\n\n").unwrap_or(before));
        let inner = &body[..j];
        out.push_str(inner.strip_prefix('\n').unwrap_or(inner));
        let tail = &body[j + close.len()..];
        rest = tail.strip_prefix('\n').unwrap_or(tail);
    }
    out.push_str(rest);
    out
}

fn is_paste(t: &str) -> bool {
    t.matches('\n').count() > 15 || logish_hits(t) > 5 || t.split_whitespace().count() > 250
}

pub(crate) fn logish_hits(t: &str) -> usize {
    let c: Vec<char> = t.chars().collect();
    let mut i = 0;
    let mut hits = 0;
    while i < c.len() {
        match logish_at(&c, i) {
            Some(len) => {
                hits += 1;
                i += len.max(1);
            }
            None => i += 1,
        }
    }
    hits
}

fn starts(c: &[char], i: usize, lit: &str) -> bool {
    let mut k = i;
    for ch in lit.chars() {
        if c.get(k) != Some(&ch) {
            return false;
        }
        k += 1;
    }
    true
}

fn digits(c: &[char], i: usize, n: usize) -> bool {
    (i..i + n).all(|k| c.get(k).is_some_and(|ch| ch.is_ascii_digit()))
}

fn word_or_dot(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_' || ch == '.' || ch == '$'
}

fn at_eol(c: &[char], i: usize) -> bool {
    i >= c.len() || c[i] == '\n'
}

// Mirrors the regex (\d{2}:\d{2}:\d{2}|Traceback|at [\w.$]+\(|Error:|\$ |^\s{4,}|[{}\[\]];?$) in multiline mode.
fn logish_at(c: &[char], i: usize) -> Option<usize> {
    if digits(c, i, 2)
        && c.get(i + 2) == Some(&':')
        && digits(c, i + 3, 2)
        && c.get(i + 5) == Some(&':')
        && digits(c, i + 6, 2)
    {
        return Some(8);
    }
    if starts(c, i, "Traceback") {
        return Some(9);
    }
    if starts(c, i, "at ") {
        let mut k = i + 3;
        while c.get(k).is_some_and(|&ch| word_or_dot(ch)) {
            k += 1;
        }
        if k > i + 3 && c.get(k) == Some(&'(') {
            return Some(k + 1 - i);
        }
    }
    if starts(c, i, "Error:") {
        return Some(6);
    }
    if starts(c, i, "$ ") {
        return Some(2);
    }
    if i == 0 || c[i - 1] == '\n' {
        let run = c[i..].iter().take_while(|ch| ch.is_whitespace()).count();
        if run >= 4 {
            return Some(run);
        }
    }
    if matches!(c.get(i), Some('{' | '}' | '[' | ']')) {
        if c.get(i + 1) == Some(&';') && at_eol(c, i + 2) {
            return Some(2);
        }
        if at_eol(c, i + 1) {
            return Some(1);
        }
    }
    None
}
