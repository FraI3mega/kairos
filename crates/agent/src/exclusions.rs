//! Every use looks at the file's stamp, so a change applies to the next event without a restart: a few seconds of delay
//! would record what someone just excluded.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use kairos_core::exclude::{self, Exclusions};

use crate::diag::diag;

const FILE: &str = "exclusions.json";

struct State {
    list: Arc<Exclusions>,
    stamp: Option<Stamp>,
}

// `kairos exclude` replaces the file by renaming a new one over it, so on unix the inode changes
// even when the time and length come out the same.
type Stamp = (SystemTime, u64, u64);

fn stamp(path: &Path) -> Option<Stamp> {
    let m = fs::metadata(path).ok()?;
    #[cfg(unix)]
    let id = std::os::unix::fs::MetadataExt::ino(&m);
    #[cfg(not(unix))]
    let id = 0;
    Some((m.modified().ok()?, m.len(), id))
}

pub struct Excluded {
    dir: PathBuf,
    state: Mutex<State>,
}

impl Excluded {
    pub fn new(dir: PathBuf) -> Excluded {
        let mut state = State { list: Arc::default(), stamp: None };
        reload(&dir, &mut state);
        Excluded { dir, state: Mutex::new(state) }
    }

    pub fn current(&self) -> Arc<Exclusions> {
        let Ok(mut state) = self.state.lock() else { return Arc::default() };
        reload(&self.dir, &mut state);
        state.list.clone()
    }

    pub fn covers(&self, path: &Path) -> bool {
        self.current().covers(path)
    }
}

fn reload(dir: &Path, state: &mut State) {
    let stamp = stamp(&dir.join(FILE));
    if stamp == state.stamp {
        return;
    }
    match exclude::load(dir) {
        Ok(list) => state.list = Arc::new(list),
        Err(e) => diag!("kairosd: warning: keeping the earlier excluded folders: {e:#}"),
    }
    state.stamp = stamp;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kairos-excluded-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_change_to_the_file_applies_to_the_next_use() {
        let data = dir("reload");
        let secret = data.join("secret");
        let ex = Excluded::new(data.clone());
        assert!(!ex.covers(&secret));
        let before = ex.current();
        exclude::add(&data, &secret).unwrap();
        assert!(ex.covers(&secret.join("a.rs")));
        assert!(!Arc::ptr_eq(&before, &ex.current()));
        let same = ex.current();
        assert!(Arc::ptr_eq(&same, &ex.current()), "an unchanged file keeps the list");
        exclude::remove(&data, &secret).unwrap();
        assert!(!ex.covers(&secret));
        let _ = fs::remove_dir_all(&data);
    }

    #[test]
    fn a_broken_file_keeps_the_list_that_was_working() {
        let data = dir("broken");
        let secret = data.join("secret");
        exclude::add(&data, &secret).unwrap();
        let ex = Excluded::new(data.clone());
        fs::write(data.join(FILE), "{ nope").unwrap();
        assert!(ex.covers(&secret));
        let _ = fs::remove_dir_all(&data);
    }
}
