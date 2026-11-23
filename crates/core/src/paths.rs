use std::path::{Path, PathBuf};

pub const SYSTEMD_UNIT: &str = "kairosd.service";
pub const LAUNCHD_LABEL: &str = "dev.obayy.kairos";
pub const API_PORT: u16 = 24816;

/// Local on Windows, so roaming profiles never sync it.
pub fn data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("KAIROS_DIR") {
        return PathBuf::from(d);
    }
    let base = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("."));
    if cfg!(target_os = "linux") {
        base.join("kairos")
    } else {
        base.join("Kairos")
    }
}

/// Events name projects and files, so other accounts on a shared computer must not read them.
/// Windows keeps `%LOCALAPPDATA%` to its owner already.
pub fn make_private(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if std::fs::metadata(dir)?.permissions().mode() & 0o777 != 0o700 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(())
}

pub fn install_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        dirs::data_local_dir().map(|d| d.join("Programs").join("Kairos"))
    } else if cfg!(target_os = "macos") {
        dirs::data_dir().map(|d| d.join("Kairos").join("bin"))
    } else {
        dirs::home_dir().map(|h| h.join(".local").join("bin"))
    }
}

pub fn socket_path() -> PathBuf {
    if let Some(s) = std::env::var_os("KAIROS_SOCK") {
        return PathBuf::from(s);
    }
    dirs::runtime_dir().unwrap_or_else(data_dir).join("kairos.sock")
}

pub fn spool_path() -> PathBuf {
    data_dir().join("spool.jsonl")
}

pub fn log_path(date: &str) -> PathBuf {
    data_dir().join(format!("events-{date}.jsonl"))
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn the_data_folder_is_made_owner_only() {
        let dir = std::env::temp_dir().join(format!("kairos-private-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mode = |d: &std::path::Path| std::fs::metadata(d).unwrap().permissions().mode() & 0o777;
        super::make_private(&dir.join("new")).unwrap();
        assert_eq!(mode(&dir.join("new")), 0o700);
        std::fs::set_permissions(&dir.join("new"), std::fs::Permissions::from_mode(0o755)).unwrap();
        super::make_private(&dir.join("new")).unwrap();
        assert_eq!(mode(&dir.join("new")), 0o700, "an open folder from an earlier version");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
