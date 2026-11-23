use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use kairos_core::event::{Event, Kind, MAX_PATH_LEN};
use kairos_core::exclude::Exclusions;
use kairos_core::project::Project;
use kairos_core::wakatime::{self, editor, from_agent, Released};
use serde_json::{json, Value};

use crate::diag::diag;
use crate::now;

const RECHECK: Duration = Duration::from_secs(30);
/// Checks in a row the API port has been someone else's before the config is given back: about two minutes, longer than
/// kairosd takes to bind it again after its listener stops.
const PARK_AFTER: u32 = 4;

/// wakatime-cli sends at most a few dozen at once; more than this is a plugin gone wrong.
const MAX_HEARTBEATS: usize = 1000;
const MAX_TEXT: usize = 100;

/// Keeps the WakaTime config Kairos holds pointed at Kairos when another setup or a plugin's sign-in changes it, so
/// heartbeats keep coming here and go on with whatever was written. While another program has the API port (`bound` is
/// false) the config is given back instead, and taken again once the port is kairosd's.
pub fn keep(data: PathBuf, bound: Arc<AtomicBool>) {
    let _ = thread::Builder::new().name("wakatime".into()).spawn(move || {
        let mut failed = None;
        let mut busy = 0;
        loop {
            if !bound.load(Ordering::Relaxed) {
                busy += 1;
                if busy == PARK_AFTER {
                    match wakatime::park(&data) {
                        Ok(Some(Released::Restored(p))) => {
                            diag!("kairosd: another program has port {}, gave {} back its earlier settings until it is free", kairos_core::paths::API_PORT, p.display())
                        }
                        Ok(_) => {}
                        Err(e) => diag!("kairosd: could not give back the WakaTime config: {e:#}"),
                    }
                }
                thread::sleep(RECHECK);
                continue;
            }
            busy = 0;
            match wakatime::unpark(&data) {
                Ok(Some(p)) => diag!("kairosd: port {} is free again, {} sends editor heartbeats to Kairos again", kairos_core::paths::API_PORT, p.display()),
                Ok(None) => {}
                Err(e) => diag!("kairosd: could not take the WakaTime config back: {e:#}"),
            }
            match wakatime::repoint(&data, now()) {
                Ok(Some(s)) => {
                    let on = wakatime::upstream(&data).map(|u| format!(", passing heartbeats on to {}", u.host())).unwrap_or_default();
                    diag!("kairosd: {} was changed to send editor heartbeats elsewhere, pointed it back at Kairos{on}", s.config.display());
                    failed = None;
                }
                Ok(None) => failed = None,
                Err(e) => {
                    let why = format!("{e:#}");
                    if failed.as_ref() != Some(&why) {
                        diag!("kairosd: could not check the WakaTime config: {why}");
                        failed = Some(why);
                    }
                }
            }
            thread::sleep(RECHECK);
        }
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    /// From the person's editor plugins and agent hooks, passed on to where they send.
    Relay,
    /// From the person's wakatime-cli run by kairosd with AI sync on (see `shadow`), never sent anywhere.
    Shadow,
}

/// The events for a request's heartbeats (one object or a list of them), and the answer
/// wakatime-cli expects for each, in order. Heartbeats made from agents' logs become `AiHeartbeat`s,
/// filed under the project of the file, else of the folder their session ran in (`session`), and
/// dropped when that folder is excluded. Editor heartbeats only count when relayed. Heartbeats for
/// files in an excluded folder are answered as stored, so wakatime-cli doesn't queue them again, and
/// dropped.
pub fn heartbeats(
    body: &Value,
    user_agent: Option<&str>,
    via: Via,
    project: impl Fn(&Path) -> Option<Project>,
    session: impl Fn(&str) -> Option<PathBuf>,
    ex: &Exclusions,
) -> (Vec<Event>, Vec<Value>) {
    let list = match body {
        Value::Array(list) => list.as_slice(),
        one => std::slice::from_ref(one),
    };
    let mut events = Vec::new();
    let answers = list
        .iter()
        .enumerate()
        .map(|(i, h)| match heartbeat(h, user_agent, via, &project, &session, ex) {
            Ok(_) if i >= MAX_HEARTBEATS => json!([{ "error": "too many heartbeats in one request" }, 400]),
            Ok(e) => {
                events.extend(e);
                json!([{ "data": { "id": format!("{i}"), "time": h["time"] } }, 201])
            }
            Err(message) => json!([{ "error": message }, 400]),
        })
        .collect();
    (events, answers)
}

fn heartbeat(
    h: &Value,
    user_agent: Option<&str>,
    via: Via,
    project: &impl Fn(&Path) -> Option<Project>,
    session: &impl Fn(&str) -> Option<PathBuf>,
    ex: &Exclusions,
) -> Result<Option<Event>, &'static str> {
    let at = h.get("time").and_then(Value::as_f64).filter(|t| t.is_finite() && *t > 0.0).ok_or("missing time")?;
    let agent = from_agent(h);
    if !agent && via == Via::Shadow {
        return Ok(None);
    }
    let ran_in = h.get("ai_session").and_then(Value::as_str).filter(|_| agent).and_then(session);
    if ran_in.as_deref().is_some_and(|cwd| ex.covers(cwd)) {
        return Ok(None);
    }
    let text = |k: &str| {
        let s: String = h.get(k)?.as_str()?.trim().chars().filter(|c| !c.is_control()).take(MAX_TEXT).collect();
        (!s.is_empty()).then_some(s)
    };
    let file = h.get("type").and_then(Value::as_str).is_none_or(|t| t == "file");
    let entity = h.get("entity").and_then(Value::as_str).filter(|_| file).map(Path::new).filter(|p| p.is_absolute());
    if entity.is_some_and(|p| ex.covers(p)) {
        return Ok(None);
    }
    let found = entity.and_then(|p| Some((project(p)?, p)));
    let path = found
        .as_ref()
        .and_then(|(proj, p)| p.strip_prefix(&proj.root).ok())
        .map(|rel| rel.to_string_lossy().replace('\\', "/"))
        .filter(|rel| !rel.is_empty() && rel.len() <= MAX_PATH_LEN);
    let user_agent = h.get("user_agent").and_then(Value::as_str).or(user_agent);
    let lines = |k: &str| h.get(k).and_then(Value::as_i64);
    let write = h.get("is_write").and_then(Value::as_bool).unwrap_or(false);
    if agent {
        let id = match (found, entity, ran_in) {
            (Some((proj, _)), _, _) => Some(proj.id),
            (None, None, Some(cwd)) => project(&cwd).map(|p| p.id),
            _ => None,
        };
        let kind = Kind::AiHeartbeat {
            at,
            sent: via == Via::Relay,
            agent: user_agent.and_then(wakatime::agent),
            path,
            language: text("language"),
            write,
            ai_lines: lines("ai_line_changes"),
        };
        return Ok(Some(Event::new(now(), id, kind)));
    }
    let kind = Kind::Heartbeat {
        at,
        editor: user_agent.and_then(editor),
        language: text("language"),
        category: text("category"),
        path,
        write,
        ai_lines: lines("ai_line_changes"),
        human_lines: lines("human_line_changes"),
    };
    Ok(Some(Event::new(now(), found.map(|(proj, _)| proj.id), kind)))
}

pub fn today(secs: f64) -> Value {
    let secs = secs.max(0.0);
    let whole = secs as u64;
    let (hours, minutes) = (whole / 3600, whole % 3600 / 60);
    let unit = |n: u64, one: &str| format!("{n} {one}{}", if n == 1 { "" } else { "s" });
    let text = match (hours, minutes) {
        (0, m) => unit(m, "min"),
        (h, 0) => unit(h, "hr"),
        (h, m) => format!("{} {}", unit(h, "hr"), unit(m, "min")),
    };
    json!({
        "data": {
            "grand_total": {
                "decimal": format!("{:.2}", secs / 3600.0),
                "digital": format!("{hours}:{minutes:02}"),
                "hours": hours,
                "minutes": minutes,
                "text": text,
                "total_seconds": secs,
            },
            "categories": [],
        }
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn abs(path: &str) -> String {
        format!("{}{path}", if cfg!(windows) { "C:" } else { "" })
    }

    fn project(p: &Path) -> Option<Project> {
        let root = PathBuf::from(abs("/w/kairos"));
        p.starts_with(&root).then(|| Project { id: "ObayM/kairos".into(), root })
    }

    fn kinds(events: &[Event]) -> Vec<(Option<&str>, &Kind)> {
        events.iter().map(|e| (e.project.as_deref(), &e.kind)).collect()
    }

    #[test]
    fn heartbeats_become_events_with_paths_from_the_project() {
        let body = json!([
            {
                "entity": abs("/w/kairos/src/main.rs"), "type": "file", "time": 1_700_000_000.5, "category": "coding",
                "language": "Rust", "is_write": true, "project": "kairos", "branch": "main", "lineno": 4,
                "human_line_changes": 3, "ai_line_changes": 12,
                "user_agent": "wakatime/v1.102.1 (linux-6.8.0-x86_64) go1.22.5 vscode/1.95.0 vscode-wakatime/24.6.2"
            },
            { "entity": abs("/home/a/notes.txt"), "type": "file", "time": 1_700_000_001.0 },
            { "entity": "github.com", "type": "domain", "time": 1_700_000_002.0, "category": "browsing" },
            { "entity": abs("/w/kairos/a.rs") },
            { "entity": abs("/w/kairos/b.rs"), "type": "file", "category": "ai coding", "time": 1_700_000_004.0 },
            {
                "entity": "Claude 0505d47f", "type": "app", "category": "ai coding", "time": 1_700_000_003.0, "project": "kairos",
                "ai_session": "0505d47f", "ai_input_tokens": 648, "ai_output_tokens": 456,
                "user_agent": "wakatime/v2.28.4 (linux-7.2.4-x86_64) go1.27.3 opus/5-5 claude-code/2.1.317"
            },
            {
                "entity": abs("/w/kairos/src/lib.rs"), "type": "file", "category": "ai coding", "time": 1_700_000_005.0,
                "ai_session": "0505d47f", "ai_line_changes": 40,
                "user_agent": "wakatime/v2.28.4 (linux-7.2.4-x86_64) go1.27.3 opus/5-5 claude-code/2.1.317"
            },
        ]);
        let (events, answers) = heartbeats(&body, Some("wakatime/v1 (x) go1 zed/1 zed-wakatime/1"), Via::Relay, project, |_| None, &Exclusions::default());
        let beat = |at, category: Option<&str>| Kind::Heartbeat {
            at,
            editor: Some("zed".into()),
            language: None,
            category: category.map(String::from),
            path: None,
            write: false,
            ai_lines: None,
            human_lines: None,
        };
        let first = Kind::Heartbeat {
            at: 1_700_000_000.5,
            editor: Some("vscode".into()),
            language: Some("Rust".into()),
            category: Some("coding".into()),
            path: Some("src/main.rs".into()),
            write: true,
            ai_lines: Some(12),
            human_lines: Some(3),
        };
        let beside = Kind::Heartbeat {
            at: 1_700_000_004.0,
            editor: Some("zed".into()),
            language: None,
            category: Some("ai coding".into()),
            path: Some("b.rs".into()),
            write: false,
            ai_lines: None,
            human_lines: None,
        };
        let app = Kind::AiHeartbeat { at: 1_700_000_003.0, sent: true, agent: Some("claude-code".into()), path: None, language: None, write: false, ai_lines: None };
        let edit = Kind::AiHeartbeat {
            at: 1_700_000_005.0,
            sent: true,
            agent: Some("claude-code".into()),
            path: Some("src/lib.rs".into()),
            language: None,
            write: false,
            ai_lines: Some(40),
        };
        assert_eq!(
            kinds(&events),
            [
                (Some("ObayM/kairos"), &first),
                (None, &beat(1_700_000_001.0, None)),
                (None, &beat(1_700_000_002.0, Some("browsing"))),
                (Some("ObayM/kairos"), &beside),
                (None, &app),
                (Some("ObayM/kairos"), &edit),
            ],
            "files outside projects and other entities keep no path, editor heartbeats stay whatever their category, \
             and agents' are kept apart, under no project while their session is unknown"
        );
        assert_eq!(answers[0], json!([{ "data": { "id": "0", "time": 1_700_000_000.5 } }, 201]));
        assert_eq!(answers[3], json!([{ "error": "missing time" }, 400]));
        assert_eq!(answers[5], json!([{ "data": { "id": "5", "time": 1_700_000_003.0 } }, 201]));
        assert_eq!(answers[6][1], 201);
        let line = serde_json::to_string(&events[1]).unwrap();
        assert!(!line.contains("notes") && !line.contains("/home"), "{line}");
    }

    #[test]
    fn what_wakatime_cli_would_have_sent_is_kept_apart_from_what_it_sent() {
        let ua = "wakatime/v2.28.4 (linux-7.2.4-x86_64) go1.27.3 opus/5-5 claude-code/2.1.317";
        let body = json!([
            { "entity": abs("/w/kairos/src/a.rs"), "type": "file", "time": 1.0, "is_write": true, "language": "Rust" },
            { "entity": "Claude s1", "type": "app", "category": "ai coding", "time": 2.0, "ai_session": "s1", "user_agent": ua },
            { "entity": abs("/w/kairos/src/b.rs"), "type": "file", "time": 3.0, "ai_session": "s1", "is_write": true, "ai_line_changes": 7, "user_agent": ua },
            { "entity": "Claude s2", "type": "app", "time": 4.0, "ai_session": "s2", "user_agent": ua },
            { "entity": abs("/w/kairos/src/c.rs"), "type": "file", "time": 5.0, "ai_session": "s2", "user_agent": ua },
            { "entity": "Claude s3", "type": "app", "time": 6.0, "ai_session": "s3", "user_agent": ua },
        ]);
        let ran_in = |s: &str| match s {
            "s1" => Some(PathBuf::from(abs("/w/kairos/src"))),
            "s2" => Some(PathBuf::from(abs("/w/kairos/secret/tool"))),
            _ => None,
        };
        let ex = Exclusions { roots: vec![PathBuf::from(abs("/w/kairos/secret"))] };
        let (events, answers) = heartbeats(&body, None, Via::Shadow, project, ran_in, &ex);
        assert!(answers.iter().all(|a| a[1] == 201));
        let ai = |at, path: Option<&str>, write, ai_lines| Kind::AiHeartbeat {
            at,
            sent: false,
            agent: Some("claude-code".into()),
            path: path.map(String::from),
            language: None,
            write,
            ai_lines,
        };
        assert_eq!(
            kinds(&events),
            [
                (Some("ObayM/kairos"), &ai(2.0, None, false, None)),
                (Some("ObayM/kairos"), &ai(3.0, Some("src/b.rs"), true, Some(7))),
                (None, &ai(6.0, None, false, None)),
            ],
            "an editor's heartbeat never comes this way, a session's own goes to its folder's project, and nothing from a session in an excluded folder"
        );

        let (relayed, _) = heartbeats(&body, None, Via::Relay, project, ran_in, &ex);
        assert!(matches!(relayed[0].kind, Kind::Heartbeat { write: true, .. }));
        assert!(relayed[1..].iter().all(|e| matches!(e.kind, Kind::AiHeartbeat { sent: true, .. })));
        assert_eq!(relayed.len(), 4);
    }

    #[test]
    fn one_heartbeat_or_too_many() {
        let (events, answers) = heartbeats(&json!({ "entity": abs("/w/kairos/x"), "time": 5.0 }), None, Via::Relay, project, |_| None, &Exclusions::default());
        assert_eq!((events.len(), answers.len()), (1, 1));
        assert!(matches!(&events[0].kind, Kind::Heartbeat { editor: None, path: Some(p), .. } if p == "x"));
        let many: Vec<Value> = (0..MAX_HEARTBEATS + 2).map(|i| json!({ "time": 1.0 + i as f64 })).collect();
        let (events, answers) = heartbeats(&Value::Array(many), None, Via::Relay, project, |_| None, &Exclusions::default());
        assert_eq!(events.len(), MAX_HEARTBEATS);
        assert_eq!(answers.iter().filter(|a| a[1] == 400).count(), 2);
    }

    #[test]
    fn heartbeats_for_files_in_an_excluded_folder_are_answered_and_dropped() {
        let ex = Exclusions { roots: vec![PathBuf::from(abs("/w/kairos/secret"))] };
        let body = json!([
            { "entity": abs("/w/kairos/secret/keys.rs"), "type": "file", "time": 1.0 },
            { "entity": abs("/w/kairos/secret"), "type": "file", "time": 2.0 },
            { "entity": abs("/w/kairos/secrets/a.rs"), "type": "file", "time": 3.0 },
            { "entity": abs("/w/kairos/src/a.rs"), "type": "file", "time": 4.0 },
        ]);
        let (events, answers) = heartbeats(&body, None, Via::Relay, project, |_| None, &ex);
        let at: Vec<f64> = events.iter().filter_map(|e| match &e.kind {
            Kind::Heartbeat { at, .. } => Some(*at),
            _ => None,
        }).collect();
        assert_eq!(at, [3.0, 4.0]);
        assert!(answers.iter().all(|a| a[1] == 201));
        assert_eq!(answers[0], json!([{ "data": { "id": "0", "time": 1.0 } }, 201]));
        let all = serde_json::to_string(&events).unwrap();
        assert!(!all.contains("keys.rs"), "{all}");
    }

    #[test]
    fn today_reads_like_wakatime() {
        let text = |s: f64| today(s)["data"]["grand_total"]["text"].as_str().unwrap().to_string();
        assert_eq!([text(0.0), text(59.0), text(60.0), text(3600.0), text(3660.0), text(9000.0)], [
            "0 mins", "0 mins", "1 min", "1 hr", "1 hr 1 min", "2 hrs 30 mins"
        ]);
        assert_eq!(today(9000.0)["data"]["grand_total"]["digital"], "2:30");
    }
}
