use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use kairos_core::exclude::Exclusions;
use serde::{Deserialize, Serialize};

const FILE: &str = "project-ids.json";
const MAX_ROOTS: usize = 512;

#[derive(Serialize, Deserialize)]
struct Seen {
    id: String,
    at: f64,
}

pub struct ProjectIds {
    path: PathBuf,
    roots: HashMap<String, Seen>,
}

fn is_repo(id: &str) -> bool {
    id.contains('/')
}

impl ProjectIds {
    pub fn load(dir: &Path) -> ProjectIds {
        let path = dir.join(FILE);
        let roots = fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        ProjectIds { path, roots }
    }

    /// A repo id never gives way to a bare folder name: that only happens when the remote can't be read for a moment, or was removed.
    pub fn observe(&mut self, root: &Path, id: &str, now: f64) -> Option<String> {
        let key = root.to_string_lossy().into_owned();
        let old = match self.roots.get_mut(&key) {
            Some(seen) if seen.id == id => {
                seen.at = now;
                return None;
            }
            Some(seen) if is_repo(&seen.id) && !is_repo(id) => {
                seen.at = now;
                return None;
            }
            Some(seen) => Some(std::mem::replace(&mut seen.id, id.to_string())),
            None => {
                self.roots.insert(key, Seen { id: id.to_string(), at: now });
                self.evict();
                None
            }
        };
        self.save();
        old
    }

    pub fn forget(&mut self, ex: &Exclusions) {
        let before = self.roots.len();
        self.roots.retain(|root, _| !ex.covers(Path::new(root)));
        if self.roots.len() != before {
            self.save();
        }
    }

    fn evict(&mut self) {
        while self.roots.len() > MAX_ROOTS {
            let oldest = self.roots.iter().min_by(|a, b| a.1.at.total_cmp(&b.1.at)).map(|(k, _)| k.clone());
            match oldest {
                Some(k) => self.roots.remove(&k),
                None => break,
            };
        }
    }

    fn save(&self) {
        let Ok(text) = serde_json::to_string(&self.roots) else { return };
        let tmp = self.path.with_extension("json.tmp");
        if fs::write(&tmp, text).is_ok() {
            let _ = fs::rename(&tmp, &self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kairos-ids-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_root_that_gains_a_remote_or_is_renamed_reports_its_old_id_once() {
        let dir = tmp("rename");
        let root = Path::new("/home/ada/Projects/game");
        let mut ids = ProjectIds::load(&dir);
        assert_eq!(ids.observe(root, "game", 1.0), None);
        assert_eq!(ids.observe(root, "game", 2.0), None);
        assert_eq!(ids.observe(root, "ada/game", 3.0).as_deref(), Some("game"));
        assert_eq!(ids.observe(root, "ada/game", 4.0), None);
        assert_eq!(ids.observe(root, "ada/space-game", 5.0).as_deref(), Some("ada/game"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_repo_id_never_falls_back_to_the_folder_name() {
        let dir = tmp("fallback");
        let root = Path::new("/home/ada/Projects/game");
        let mut ids = ProjectIds::load(&dir);
        ids.observe(root, "ada/game", 1.0);
        assert_eq!(ids.observe(root, "game", 2.0), None);
        assert_eq!(ids.observe(root, "ada/game", 3.0), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ids_survive_a_restart_and_roots_are_kept_apart() {
        let dir = tmp("restart");
        let (a, b) = (Path::new("/p/a"), Path::new("/p/b"));
        let mut ids = ProjectIds::load(&dir);
        ids.observe(a, "a", 1.0);
        ids.observe(b, "b", 1.0);
        let mut ids = ProjectIds::load(&dir);
        assert_eq!(ids.observe(a, "ada/a", 2.0).as_deref(), Some("a"));
        assert_eq!(ids.observe(b, "b", 2.0), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_least_recently_seen_roots_are_forgotten_first() {
        let dir = tmp("evict");
        let mut ids = ProjectIds::load(&dir);
        for i in 0..MAX_ROOTS + 1 {
            ids.observe(Path::new(&format!("/p/{i}")), &format!("p{i}"), i as f64);
        }
        assert_eq!(ids.roots.len(), MAX_ROOTS);
        assert!(!ids.roots.contains_key("/p/0"));
        let _ = fs::remove_dir_all(&dir);
    }
}
