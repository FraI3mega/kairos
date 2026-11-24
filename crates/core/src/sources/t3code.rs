use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags};

// T3 Code runs Claude Code through the SDK, so the transcript shows one kind of prompt whether a person typed it in the
// app or an agent, the `t3 thread` command, a scheduled task or T3 Code itself sent it. T3 Code's own store says which:
// each message has `createdBy` (user, agent, system) and `creationSource` (web, mobile, mcp, provider, server), and each
// user message is a turn item stamped when it was sent, before Claude Code writes it down.

// $T3CODE_HOME, else ~/.t3 on every system (apps/desktop DesktopStatePaths, apps/server config.ts).
pub fn default_dbs() -> Vec<PathBuf> {
    let mut homes: Vec<PathBuf> = std::env::var_os("T3CODE_HOME").filter(|h| !h.is_empty()).map(PathBuf::from).into_iter().collect();
    homes.extend(dirs::home_dir().map(|h| h.join(".t3")));
    homes.into_iter().map(|h| h.join("userdata").join("statev2.sqlite")).collect()
}

// The app's projects, as (title, folder). Since 0.1.10 the window is titled after the open thread and its project.
pub fn projects(dbs: &[PathBuf]) -> Vec<(String, PathBuf)> {
    let read = |db: &PathBuf| -> Option<Vec<(String, PathBuf)>> {
        if !db.exists() {
            return None;
        }
        let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX).ok()?;
        conn.busy_timeout(Duration::from_millis(250)).ok()?;
        let mut stmt = conn.prepare("SELECT title, workspace_root FROM projection_projects WHERE deleted_at IS NULL").ok()?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, PathBuf::from(r.get::<_, String>(1)?)))).ok()?;
        rows.collect::<Result<_, _>>().ok()
    };
    dbs.iter().filter_map(read).flatten().collect()
}

/// The folder of the project a T3 Code window title names: `project / thread`, or `thread - project` (after a status
/// and a colon) with the screen reader title on. `None` unless exactly one folder fits.
pub fn titled_folder<'a>(title: &str, projects: &'a [(String, PathBuf)]) -> Option<&'a Path> {
    let title = title.trim();
    let mut found: Option<&Path> = None;
    for (name, folder) in projects {
        let name = name.trim();
        let named = !name.is_empty() && (title.strip_prefix(name).is_some_and(|rest| rest.starts_with(" / ")) || title.strip_suffix(name).is_some_and(|rest| rest.ends_with(" - ")));
        if named && found.is_some_and(|f| f != folder) {
            return None;
        }
        if named {
            found = Some(folder);
        }
    }
    found
}

const ITEMS: &str = "
    SELECT (julianday(json_extract(i.payload_json, '$.startedAt')) - 2440587.5) * 86400.0,
           json_extract(m.payload_json, '$.createdBy'),
           json_extract(m.payload_json, '$.creationSource'),
           COALESCE(json_type(m.payload_json, '$.scheduledTaskId'), json_type(i.payload_json, '$.scheduledTaskId'),
                    json_type(m.payload_json, '$.senderThreadId'), json_type(i.payload_json, '$.senderThreadId'),
                    json_type(m.payload_json, '$.linkedSender'), json_type(i.payload_json, '$.linkedSender'),
                    json_type(m.payload_json, '$.idleCompaction'), json_type(i.payload_json, '$.idleCompaction'))
    FROM orchestration_v2_projection_provider_threads p
    LEFT JOIN orchestration_v2_projection_turn_items i ON i.provider_thread_id = p.provider_thread_id AND i.type = 'user_message'
    LEFT JOIN orchestration_v2_projection_messages m ON m.message_id = json_extract(i.payload_json, '$.messageId')
    WHERE json_extract(p.payload_json, '$.nativeThreadRef.nativeId') = ?1";

// A new session's id reaches T3 Code's store a moment after Claude Code writes the first prompt down.
const WAIT: Duration = Duration::from_millis(500);
const TRIES: usize = 4;
const FRESH_SECS: f64 = 15.0;

struct Item {
    at: f64,
    typed: Option<bool>,
}

pub struct T3Code {
    dbs: Vec<PathBuf>,
    unknown: HashSet<String>,
}

impl T3Code {
    pub fn new(dbs: Vec<PathBuf>) -> T3Code {
        T3Code { dbs, unknown: HashSet::new() }
    }

    /// Whether the prompt Claude Code got at `ts` in `session` is one a person typed in the T3 Code app. `after` is the
    /// prompt before it in the session. `None` when T3 Code's store does not say: no store, a layout this does not
    /// know, or a session it never heard of.
    pub fn typed(&mut self, session: &str, after: Option<f64>, ts: f64) -> Option<bool> {
        if self.unknown.contains(session) {
            return None;
        }
        let fresh = SystemTime::now().duration_since(UNIX_EPOCH).is_ok_and(|now| now.as_secs_f64() - ts < FRESH_SECS);
        for attempt in 0..TRIES {
            if let Some(items) = self.dbs.iter().find_map(|db| items(db, session)) {
                return verdict(&items, after, ts);
            }
            if !fresh || attempt + 1 == TRIES || !self.dbs.iter().any(|db| db.exists()) {
                break;
            }
            thread::sleep(WAIT);
        }
        self.unknown.insert(session.to_string());
        None
    }
}

// The user messages T3 Code sent into the Claude Code session `session`, oldest first. `None` when it has no such session.
fn items(db: &Path, session: &str) -> Option<Vec<Item>> {
    if !db.exists() {
        return None;
    }
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX).ok()?;
    conn.busy_timeout(Duration::from_millis(250)).ok()?;
    let mut stmt = conn.prepare(ITEMS).ok()?;
    let rows = stmt
        .query_map([session], |r| {
            Ok((r.get::<_, Option<f64>>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, Option<String>>(3)?))
        })
        .ok()?;
    let mut known = false;
    let mut out = Vec::new();
    for row in rows {
        let (at, by, source, other) = row.ok()?;
        known = true;
        // A provider thread with no user message yet comes back as one row of nulls.
        let Some(at) = at else { continue };
        let typed = match (by.as_deref(), source.as_deref()) {
            (Some(by), Some(source)) => Some(by == "user" && source == "web" && other.is_none()),
            _ => None,
        };
        out.push(Item { at, typed });
    }
    out.sort_by(|a, b| a.at.total_cmp(&b.at));
    known.then_some(out)
}

// The message behind a prompt was sent before `ts` and after the prompt before it, unless it waited in the queue behind
// another. So a prompt counts only when every message sent in that stretch was typed: an agent's message in it could be
// this prompt. With no prompt before it to go by, only the newest message is looked at.
fn verdict(items: &[Item], after: Option<f64>, ts: f64) -> Option<bool> {
    let sent: Vec<&Item> = items.iter().filter(|i| i.at <= ts).collect();
    let stretch = match after {
        Some(after) => sent.into_iter().filter(|i| i.at > after).collect(),
        None => sent.last().copied().into_iter().collect::<Vec<_>>(),
    };
    if stretch.iter().any(|i| i.typed.is_none()) {
        return None;
    }
    Some(!stretch.is_empty() && stretch.iter().all(|i| i.typed == Some(true)))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const SCHEMA: &str = "
        CREATE TABLE orchestration_v2_projection_provider_threads (provider_thread_id TEXT PRIMARY KEY, payload_json TEXT NOT NULL);
        CREATE TABLE orchestration_v2_projection_turn_items (turn_item_id TEXT PRIMARY KEY, provider_thread_id TEXT, type TEXT NOT NULL, payload_json TEXT NOT NULL);
        CREATE TABLE orchestration_v2_projection_messages (message_id TEXT PRIMARY KEY, payload_json TEXT NOT NULL);";

    pub(crate) struct Store {
        pub(crate) path: PathBuf,
        conn: Connection,
        n: usize,
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kairos-t3code-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    pub(crate) fn store(name: &str) -> Store {
        let path = scratch(name).join("statev2.sqlite");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        Store { path, conn, n: 0 }
    }

    impl Drop for Store {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(self.path.parent().unwrap());
        }
    }

    impl Store {
        pub(crate) fn thread(&self, id: &str, session: &str) {
            let payload = format!(r#"{{"id":"{id}","driver":"claudeAgent","nativeThreadRef":{{"driver":"claudeAgent","nativeId":"{session}","strength":"strong"}}}}"#);
            self.conn.execute("INSERT INTO orchestration_v2_projection_provider_threads VALUES (?1, ?2)", [id, &payload]).unwrap();
        }

        // `at` is seconds past 2026-11-07T05:00:00Z; `extra` goes into the message as more fields.
        pub(crate) fn message(&mut self, thread: &str, at: u32, by: &str, source: &str, extra: &str) {
            self.n += 1;
            let (item, message) = (format!("i{}", self.n), format!("m{}", self.n));
            let started = format!("2026-11-07T05:{:02}:{:02}.500Z", at / 60, at % 60);
            let payload = format!(r#"{{"id":"{item}","type":"user_message","messageId":"{message}","startedAt":"{started}","text":"hi"}}"#);
            self.conn.execute("INSERT INTO orchestration_v2_projection_turn_items VALUES (?1, ?2, 'user_message', ?3)", [&item, thread, &payload]).unwrap();
            let fields = if by.is_empty() { String::new() } else { format!(r#","createdBy":"{by}","creationSource":"{source}""#) };
            let payload = format!(r#"{{"id":"{message}","role":"user","text":"hi"{fields}{extra}}}"#);
            self.conn.execute("INSERT INTO orchestration_v2_projection_messages VALUES (?1, ?2)", [&message, &payload]).unwrap();
        }

        fn typed(&self, session: &str, after: Option<u32>, at: u32) -> Option<bool> {
            T3Code::new(vec![self.path.clone()]).typed(session, after.map(secs), secs(at))
        }
    }

    // 2026-11-07T05:00:00Z, well in the past so nothing waits for a fresh session.
    fn secs(at: u32) -> f64 {
        1_794_027_600.0 + f64::from(at)
    }

    #[test]
    fn a_window_title_names_one_project_or_none() {
        let s = store("projects");
        s.conn.execute_batch("CREATE TABLE projection_projects (project_id TEXT, title TEXT, workspace_root TEXT, deleted_at TEXT);
            INSERT INTO projection_projects VALUES ('1', 'kairos', '/w/kairos', NULL), ('2', 'site', '/w/site', NULL), ('3', 'site / v2', '/w/v2', NULL),
            ('4', 'kairos', '/w/kairos', NULL), ('5', 'gone', '/w/gone', '2026-11-01T00:00:00Z'), ('6', 'twin', '/a/twin', NULL), ('7', 'twin', '/b/twin', NULL);").unwrap();
        let all = projects(&[s.path.clone(), s.path.with_file_name("missing.sqlite")]);
        assert_eq!(all.len(), 6);
        let folder = |title: &str| titled_folder(title, &all).map(|p| p.to_string_lossy().into_owned());
        assert_eq!(folder("kairos / Fix the parser test"), Some("/w/kairos".into()));
        assert_eq!(folder("Fix the parser test - kairos"), Some("/w/kairos".into()));
        assert_eq!(folder("Working: Fix the parser test - kairos"), Some("/w/kairos".into()));
        assert_eq!(folder("kairos / look at site / again"), Some("/w/kairos".into()));
        assert_eq!(folder("T3 Code (Beta)"), None);
        assert_eq!(folder("kairos"), None);
        assert_eq!(folder("kairosx / thread"), None);
        assert_eq!(folder("gone / thread"), None);
        // Two projects fit: `site` with a thread named `v2 / x`, or `site / v2`.
        assert_eq!(folder("site / v2 / x"), None);
        assert_eq!(folder("twin / thread"), None);
        assert_eq!(folder("kairos / compare - site"), None);
        assert!(projects(&[s.path.with_file_name("missing.sqlite")]).is_empty());
    }

    #[test]
    fn a_message_typed_in_the_app_counts() {
        let mut s = store("typed");
        s.thread("p1", "s1");
        s.message("p1", 10, "user", "web", "");
        assert_eq!(s.typed("s1", None, 11), Some(true));
        s.message("p1", 60, "user", "web", "");
        assert_eq!(s.typed("s1", Some(11), 61), Some(true));
    }

    #[test]
    fn messages_from_agents_tools_tasks_and_other_devices_do_not() {
        for (by, source, extra) in [
            ("agent", "mcp", ""),
            ("agent", "provider", ""),
            ("agent", "server", ""),
            ("system", "server", ""),
            ("user", "mobile", ""),
            ("user", "mcp", ""),
            ("user", "server", ""),
            ("user", "web", r#","scheduledTaskId":"task-1""#),
            ("user", "web", r#","senderThreadId":"thread-2""#),
            ("user", "web", r#","linkedSender":{"threadId":"t","environmentId":"e"}"#),
            ("user", "web", r#","idleCompaction":"beforeCacheExpires""#),
        ] {
            let mut s = store("others");
            s.thread("p1", "s1");
            s.message("p1", 10, by, source, extra);
            assert_eq!(s.typed("s1", None, 11), Some(false), "{by} {source} {extra}");
        }
    }

    #[test]
    fn an_agent_message_in_the_same_stretch_keeps_the_prompt_out() {
        let mut s = store("stretch");
        s.thread("p1", "s1");
        s.message("p1", 10, "user", "web", "");
        s.message("p1", 20, "user", "web", "");
        s.message("p1", 25, "agent", "mcp", "");
        // Either could be the prompt Claude Code got at 30.
        assert_eq!(s.typed("s1", Some(11), 30), Some(false));
        // Nothing was sent between 30 and 40, so the prompt at 40 came out of the queue: it may be the agent's.
        assert_eq!(s.typed("s1", Some(30), 40), Some(false));
        s.message("p1", 50, "user", "web", "");
        assert_eq!(s.typed("s1", Some(40), 51), Some(true));
    }

    #[test]
    fn a_message_sent_after_the_prompt_is_not_it() {
        let mut s = store("after");
        s.thread("p1", "s1");
        s.message("p1", 10, "agent", "mcp", "");
        s.message("p1", 30, "user", "web", "");
        assert_eq!(s.typed("s1", None, 11), Some(false));
        assert_eq!(s.typed("s1", Some(11), 31), Some(true));
        let mut s = store("after2");
        s.thread("p1", "s1");
        s.message("p1", 30, "user", "web", "");
        assert_eq!(s.typed("s1", None, 11), Some(false));
    }

    #[test]
    fn sessions_are_kept_apart() {
        let mut s = store("apart");
        s.thread("p1", "s1");
        s.thread("p2", "s2");
        s.message("p1", 10, "agent", "mcp", "");
        s.message("p2", 10, "user", "web", "");
        assert_eq!(s.typed("s1", None, 11), Some(false));
        assert_eq!(s.typed("s2", None, 11), Some(true));
    }

    #[test]
    fn nothing_is_said_about_what_the_store_does_not_show() {
        let mut s = store("unknown");
        s.thread("p1", "s1");
        assert_eq!(s.typed("other", None, 11), None);
        // Written before T3 Code recorded who sent a message.
        s.message("p1", 10, "", "", "");
        assert_eq!(s.typed("s1", None, 11), None);

        let dir = scratch("missing");
        assert_eq!(T3Code::new(vec![dir.join("statev2.sqlite")]).typed("s1", None, secs(11)), None);
        let other = dir.join("other.sqlite");
        Connection::open(&other).unwrap().execute_batch("CREATE TABLE unrelated (x)").unwrap();
        assert_eq!(T3Code::new(vec![other]).typed("s1", None, secs(11)), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_session_with_no_message_yet_has_no_typed_prompt() {
        let s = store("empty");
        s.thread("p1", "s1");
        assert_eq!(s.typed("s1", None, 11), Some(false));
    }
}
