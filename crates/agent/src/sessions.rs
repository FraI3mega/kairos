use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const MAX: usize = 4096;

/// The folder each agent session ran in, as read from its log, so a heartbeat wakatime-cli made for the session
/// itself (no file) goes to that folder's project and is dropped once the folder is excluded. Kept on disk so
/// sessions that carry on after kairosd restarts are still known.
pub struct Sessions {
    path: Option<PathBuf>,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    cwd: HashMap<String, PathBuf>,
    order: VecDeque<String>,
    changed: bool,
}

impl Sessions {
    pub fn load(path: PathBuf) -> Sessions {
        let mut inner = Inner::default();
        let saved: Vec<(String, PathBuf)> = fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        for (session, cwd) in saved {
            inner.note(session, cwd);
        }
        inner.changed = false;
        Sessions { path: Some(path), inner: Mutex::new(inner) }
    }

    #[cfg(test)]
    pub fn in_memory() -> Sessions {
        Sessions { path: None, inner: Mutex::new(Inner::default()) }
    }

    pub fn note(&self, session: &str, cwd: &Path) {
        if let Ok(mut inner) = self.inner.lock() {
            if inner.cwd.get(session).map(PathBuf::as_path) != Some(cwd) {
                inner.note(session.to_string(), cwd.to_path_buf());
            }
        }
    }

    pub fn cwd(&self, session: &str) -> Option<PathBuf> {
        self.inner.lock().ok()?.cwd.get(session).cloned()
    }

    pub fn save(&self) {
        let (Some(path), Ok(mut inner)) = (&self.path, self.inner.lock()) else {
            return;
        };
        if !inner.changed {
            return;
        }
        let list: Vec<(&String, &PathBuf)> = inner.order.iter().filter_map(|s| Some((s, inner.cwd.get(s)?))).collect();
        let Ok(text) = serde_json::to_string(&list) else {
            return;
        };
        let tmp = path.with_extension("json.tmp");
        if fs::write(&tmp, text).is_ok() && fs::rename(&tmp, path).is_ok() {
            inner.changed = false;
        }
    }
}

impl Inner {
    fn note(&mut self, session: String, cwd: PathBuf) {
        if self.cwd.insert(session.clone(), cwd).is_none() {
            self.order.push_back(session);
            while self.order.len() > MAX {
                if let Some(old) = self.order.pop_front() {
                    self.cwd.remove(&old);
                }
            }
        }
        self.changed = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_are_remembered_across_restarts_up_to_a_limit() {
        let dir = std::env::temp_dir().join(format!("kairos-sessions-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sessions.json");
        let s = Sessions::load(path.clone());
        s.note("a", Path::new("/w/a"));
        s.note("b", Path::new("/w/b"));
        s.note("a", Path::new("/w/a2"));
        s.save();
        let again = Sessions::load(path.clone());
        assert_eq!(again.cwd("a"), Some(PathBuf::from("/w/a2")));
        assert_eq!(again.cwd("b"), Some(PathBuf::from("/w/b")));
        assert_eq!(again.cwd("c"), None);

        for i in 0..MAX + 1 {
            again.note(&format!("s{i}"), Path::new("/w"));
        }
        assert_eq!((again.cwd("a"), again.cwd("b"), again.cwd("s0")), (None, None, None), "the oldest go first");
        assert_eq!(again.cwd("s1"), Some(PathBuf::from("/w")));
        let _ = fs::remove_dir_all(&dir);
    }
}
