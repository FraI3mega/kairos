use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const DEFAULT_SERVER: &str = "https://kairos.obayy.dev";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Account {
    pub server: String,
    pub token: String,
    pub username: String,
}

pub fn path(dir: &Path) -> PathBuf {
    dir.join("account.json")
}

pub fn load(dir: &Path) -> Option<Account> {
    serde_json::from_str(&fs::read_to_string(path(dir)).ok()?).ok()
}

pub fn save(dir: &Path, account: &Account) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let target = path(dir);
    let tmp = target.with_extension("json.tmp");
    write_private(&tmp, serde_json::to_string_pretty(account)?.as_bytes())
        .with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, &target).with_context(|| format!("writing {}", target.display()))
}

pub fn remove(dir: &Path) -> Result<bool> {
    match fs::remove_file(path(dir)) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

#[cfg(unix)]
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let _ = fs::remove_file(path);
    fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?.write_all(bytes)
}

#[cfg(not(unix))]
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    fs::write(path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_load_remove() {
        let dir = std::env::temp_dir().join(format!("kairos-account-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(load(&dir), None);
        let a = Account { server: "http://x".into(), token: "kd_1".into(), username: "u".into() };
        save(&dir, &a).unwrap();
        assert_eq!(load(&dir), Some(a));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(path(&dir)).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert!(remove(&dir).unwrap());
        assert!(!remove(&dir).unwrap());
        let _ = fs::remove_dir_all(&dir);
    }
}
