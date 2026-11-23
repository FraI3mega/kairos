use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};

use super::Found;
use crate::event::Kind;
use crate::shape::Shape;

// Turns older than this are never emitted, so per-session progress can be forgotten after it.
const KEEP: f64 = 14.0 * 86400.0;

#[derive(Debug, Default, Clone)]
pub(crate) struct Turn {
    pub ts: f64,
    pub words: usize,
    pub pasted: bool,
    pub shape: Option<Shape>,
    pub model: Option<String>,
    pub started: Option<f64>,
    pub done: Option<f64>,
    pub reply_words: usize,
    pub tools: usize,
    pub files: usize,
}

#[derive(Default)]
struct Seen {
    u: f64,
    started: bool,
    done: bool,
    // Edits and commits go out in time order; `w` is the last one's time and `wn` how many share it.
    w: f64,
    wn: u32,
}

pub(crate) fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

pub(crate) fn mtime(m: &std::fs::Metadata) -> f64 {
    m.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0.0, |d| d.as_secs_f64())
}

// For stores rewritten in place (JSON documents, compacted patch logs). The first poll with no saved state only records
// what exists, so installing never imports history.
#[derive(Default)]
pub(crate) struct Snapshots {
    since: Option<f64>,
    files: HashMap<PathBuf, (f64, u64)>,
    seen: HashMap<String, Seen>,
}

impl Snapshots {
    fn cutoff(&self) -> f64 {
        self.since.unwrap_or(f64::MAX).max(now() - KEEP)
    }

    pub fn changed(&mut self, paths: Vec<PathBuf>) -> Vec<PathBuf> {
        let cutoff = self.cutoff();
        let mut out = Vec::new();
        for p in paths {
            let Ok(m) = std::fs::metadata(&p) else { continue };
            let key = (mtime(&m), m.len());
            if self.files.insert(p.clone(), key) == Some(key) || self.since.is_none() || key.0 < cutoff - 5.0 {
                continue;
            }
            out.push(p);
        }
        out
    }

    pub fn emit(&mut self, session: &str, cwd: Option<&Path>, turns: &[Turn], out: &mut Vec<Found>) {
        let cutoff = self.cutoff();
        let st = self.seen.entry(session.to_string()).or_default();
        let found = |ts: f64, kind: Kind| Found { ts, cwd: cwd.map(Path::to_path_buf), kind };
        for t in turns {
            if t.ts <= cutoff || t.ts < st.u {
                continue;
            }
            if t.ts > st.u {
                *st = Seen { u: t.ts, w: st.w, wn: st.wn, ..Seen::default() };
                out.push(found(
                    t.ts,
                    Kind::Prompt {
                        words: t.words,
                        pasted: t.pasted,
                        model: t.model.clone(),
                        session: session.to_string(),
                        shape: t.shape.clone(),
                        version: None,
                    },
                ));
            }
            if let (false, Some(ts)) = (st.started, t.started) {
                st.started = true;
                out.push(found(ts, Kind::AgentStart { session: session.to_string() }));
            }
            if let (false, Some(ts)) = (st.done, t.done) {
                st.done = true;
                out.push(found(
                    ts,
                    Kind::ReplyDone {
                        session: session.to_string(),
                        reply_words: t.reply_words,
                        tools: t.tools,
                        files_changed: t.files,
                        model: t.model.clone(),
                    },
                ));
            }
        }
    }

    pub fn emit_work(&mut self, session: &str, work: Vec<Found>, out: &mut Vec<Found>) {
        let cutoff = self.cutoff();
        let st = self.seen.entry(session.to_string()).or_default();
        let (w0, n0) = (st.w, st.wn);
        let mut at_w0 = 0;
        for f in work {
            if f.ts <= cutoff || f.ts < w0 {
                continue;
            }
            if f.ts == w0 {
                at_w0 += 1;
                if at_w0 <= n0 {
                    continue;
                }
            }
            if f.ts == st.w {
                st.wn += 1;
            } else {
                (st.w, st.wn) = (f.ts, 1);
            }
            out.push(f);
        }
    }

    pub fn finish(&mut self, started: f64) {
        self.since.get_or_insert(started);
        let cutoff = self.cutoff();
        self.seen.retain(|_, s| s.u > cutoff || s.w > cutoff);
        self.files.retain(|p, _| p.exists());
    }

    pub fn state(&self) -> Value {
        let sessions: Map<String, Value> = self
            .seen
            .iter()
            .map(|(k, s)| (k.clone(), json!({ "u": s.u, "s": s.started, "d": s.done, "w": s.w, "n": s.wn })))
            .collect();
        json!({ "since": self.since, "sessions": sessions })
    }

    pub fn restore(&mut self, state: &Value) {
        if let Some(s) = state.get("since").and_then(Value::as_f64) {
            self.since = Some(s);
        }
        for (k, s) in state.get("sessions").and_then(Value::as_object).into_iter().flatten() {
            self.seen.insert(
                k.clone(),
                Seen {
                    u: s.get("u").and_then(Value::as_f64).unwrap_or(0.0),
                    started: s.get("s").and_then(Value::as_bool).unwrap_or(false),
                    done: s.get("d").and_then(Value::as_bool).unwrap_or(false),
                    w: s.get("w").and_then(Value::as_f64).unwrap_or(0.0),
                    wn: s.get("n").and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()).unwrap_or(0),
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(ts: f64, path: &str) -> Found {
        Found {
            ts,
            cwd: None,
            kind: Kind::AgentEdit { session: "s".into(), path: path.into(), added: 1, removed: 0 },
        }
    }

    fn paths(out: &[Found]) -> Vec<String> {
        out.iter()
            .map(|f| match &f.kind {
                Kind::AgentEdit { path, .. } => path.clone(),
                k => panic!("{k:?}"),
            })
            .collect()
    }

    #[test]
    fn work_goes_out_once_even_when_the_log_is_read_again() {
        let mut snaps = Snapshots { since: Some(0.0), ..Snapshots::default() };
        let t = now() + 100.0;
        let mut out = Vec::new();
        snaps.emit_work("s", vec![edit(t, "a"), edit(t, "b"), edit(t + 1.0, "c")], &mut out);
        assert_eq!(paths(&out), ["a", "b", "c"]);

        let mut out = Vec::new();
        snaps.emit_work("s", vec![edit(t, "a"), edit(t, "b"), edit(t + 1.0, "c"), edit(t + 1.0, "d"), edit(t + 2.0, "e")], &mut out);
        assert_eq!(paths(&out), ["d", "e"]);

        let mut again = Snapshots::default();
        again.restore(&snaps.state());
        let mut out = Vec::new();
        again.emit_work("s", vec![edit(t + 2.0, "e"), edit(t + 2.0, "f")], &mut out);
        assert_eq!(paths(&out), ["f"]);
        let mut out = Vec::new();
        again.emit_work("s", vec![edit(t - 500.0, "old")], &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn history_before_the_first_poll_is_not_work() {
        let mut snaps = Snapshots { since: Some(now()), ..Snapshots::default() };
        let mut out = Vec::new();
        snaps.emit_work("s", vec![edit(now() - 60.0, "old")], &mut out);
        assert!(out.is_empty());
    }
}
