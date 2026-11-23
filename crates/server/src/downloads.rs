//! KAIROS_DOWNLOADS holds one folder per release version plus a `latest` symlink to one of them, or a `LATEST` file naming it.

use std::path::{Path as FsPath, PathBuf};

use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use kairos_core::account::DEFAULT_SERVER;
use kairos_core::update::{plain_name, MANIFEST, SIGNATURE, SUMS};

use crate::util::{err, AppError, Res};
use crate::St;

const TEXT: &str = "text/plain; charset=utf-8";
const SHORT: &str = "public, max-age=60";
const IMMUTABLE: &str = "public, max-age=31536000, immutable";

fn script(text: &str, public_url: &str) -> String {
    text.replace(DEFAULT_SERVER, public_url)
}

pub async fn install_ps1(State(app): State<St>) -> impl IntoResponse {
    let body = script(include_str!("../../../packaging/install.ps1"), &app.config.public_url);
    ([(header::CONTENT_TYPE, TEXT), (header::CACHE_CONTROL, SHORT)], body)
}

pub async fn install_sh(State(app): State<St>) -> impl IntoResponse {
    let body = script(include_str!("../../../packaging/install.sh"), &app.config.public_url);
    ([(header::CONTENT_TYPE, TEXT), (header::CACHE_CONTROL, SHORT)], body)
}

fn content_type(file: &str) -> &'static str {
    match file {
        MANIFEST => "application/json",
        SIGNATURE | SUMS => TEXT,
        f if f.ends_with(".exe") => "application/vnd.microsoft.portable-executable",
        _ => "application/octet-stream",
    }
}

fn version_dir(name: &str) -> bool {
    plain_name(name) && name != "latest"
}

fn latest_version(root: &FsPath) -> Option<String> {
    let name = match std::fs::read_link(root.join("latest")) {
        Ok(target) => target.to_str()?.trim_end_matches('/').to_string(),
        Err(_) => std::fs::read_to_string(root.join("LATEST")).ok()?.trim().to_string(),
    };
    version_dir(&name).then_some(name)
}

fn release_file(root: &FsPath, version: &str, target: &str, file: &str) -> Option<PathBuf> {
    (version_dir(version) && plain_name(target) && plain_name(file)).then(|| root.join(version).join(target).join(file))
}

fn latest_file(root: &FsPath, file: &str) -> Option<PathBuf> {
    if !matches!(file, MANIFEST | SIGNATURE) {
        return None;
    }
    Some(root.join(latest_version(root)?).join(file))
}

fn not_found() -> AppError {
    err(StatusCode::NOT_FOUND, "not found")
}

fn root(app: &St) -> Res<PathBuf> {
    app.config.downloads.clone().ok_or_else(not_found)
}

async fn serve(path: PathBuf, file: &str, cache: &'static str) -> Res<Response> {
    let bytes = tokio::task::spawn_blocking(move || std::fs::read(path))
        .await
        .map_err(|e| anyhow::anyhow!(e))?
        .map_err(|_| not_found())?;
    Ok(([(header::CONTENT_TYPE, content_type(file)), (header::CACHE_CONTROL, cache)], bytes).into_response())
}

pub async fn latest(State(app): State<St>, Path(file): Path<String>) -> Res<Response> {
    let root = root(&app)?;
    let path = tokio::task::spawn_blocking({
        let file = file.clone();
        move || latest_file(&root, &file)
    })
    .await
    .map_err(|e| anyhow::anyhow!(e))?
    .ok_or_else(not_found)?;
    serve(path, &file, SHORT).await
}

pub async fn file(State(app): State<St>, Path((version, target, file)): Path<(String, String, String)>) -> Res<Response> {
    let path = release_file(&root(&app)?, &version, &target, &file).ok_or_else(not_found)?;
    serve(path, &file, IMMUTABLE).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("kairos-downloads-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("1.2.0/linux-x86_64")).unwrap();
        d
    }

    #[test]
    fn versioned_paths_reject_traversal() {
        let root = FsPath::new("/srv/downloads");
        assert_eq!(
            release_file(root, "1.2.0", "linux-x86_64", "kairosd"),
            Some(PathBuf::from("/srv/downloads/1.2.0/linux-x86_64/kairosd"))
        );
        for (v, t, f) in [
            ("..", "linux-x86_64", "kairosd"),
            ("latest", "linux-x86_64", "kairosd"),
            ("1.2.0", "..", "kairosd"),
            ("1.2.0", "linux-x86_64", ".."),
            ("1.2.0", "linux-x86_64", "a/b"),
            ("1.2.0", "linux-x86_64", "..\\x"),
            ("1.2.0", "linux-x86_64", ".env"),
            ("1.2.0", "linux-x86_64", "%2e%2e"),
            ("", "linux-x86_64", "kairosd"),
            ("1.2.0+x", "linux-x86_64", "kairosd"),
        ] {
            assert_eq!(release_file(root, v, t, f), None, "{v}/{t}/{f}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn latest_follows_the_symlink() {
        let root = temp("link");
        std::os::unix::fs::symlink("1.2.0", root.join("latest")).unwrap();
        assert_eq!(latest_version(&root).as_deref(), Some("1.2.0"));
        assert_eq!(latest_file(&root, "manifest.json"), Some(root.join("1.2.0/manifest.json")));
        assert_eq!(latest_file(&root, "manifest.json.sig"), Some(root.join("1.2.0/manifest.json.sig")));
        assert_eq!(latest_file(&root, "kairosd"), None);
        std::fs::remove_file(root.join("latest")).unwrap();
        std::os::unix::fs::symlink("/etc", root.join("latest")).unwrap();
        assert_eq!(latest_version(&root), None);
        std::fs::remove_file(root.join("latest")).unwrap();
        std::os::unix::fs::symlink("../x", root.join("latest")).unwrap();
        assert_eq!(latest_version(&root), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn latest_from_a_file() {
        let root = temp("file");
        assert_eq!(latest_version(&root), None);
        std::fs::write(root.join("LATEST"), "1.2.0\n").unwrap();
        assert_eq!(latest_version(&root).as_deref(), Some("1.2.0"));
        std::fs::write(root.join("LATEST"), "../../etc\n").unwrap();
        assert_eq!(latest_version(&root), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn content_types() {
        assert_eq!(content_type("kairosd.exe"), "application/vnd.microsoft.portable-executable");
        assert_eq!(content_type("manifest.json"), "application/json");
        assert_eq!(content_type("manifest.json.sig"), TEXT);
        assert_eq!(content_type("SHA256SUMS"), TEXT);
        assert_eq!(content_type("kairos"), "application/octet-stream");
    }

    #[test]
    fn scripts_point_at_this_server() {
        for text in [include_str!("../../../packaging/install.sh"), include_str!("../../../packaging/install.ps1")] {
            assert!(text.contains(DEFAULT_SERVER));
            let s = script(text, "https://example.test");
            assert!(!s.contains(DEFAULT_SERVER));
            assert!(s.contains("https://example.test"));
        }
    }
}
