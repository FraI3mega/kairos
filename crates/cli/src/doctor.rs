use std::fmt::Display;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use kairos_core::sources::{claude, codex, opencode};
use kairos_core::upload::{self, State};
use kairos_core::{account, exclude, paths, update, wakatime, wakatime_cli};
use reqwest::blocking::Client;
use serde_json::Value;

use crate::install;

const WARNINGS: usize = 12;

fn row(label: &str, value: impl Display) {
    println!("{label:<16}{value}");
}

fn os_version() -> String {
    let found = if cfg!(windows) {
        install::output("cmd", &["/c", "ver"])
    } else if cfg!(target_os = "macos") {
        install::output("sw_vers", &["-productVersion"]).map(|v| format!("macOS {v}"))
    } else {
        fs::read_to_string("/etc/os-release").ok().and_then(|t| {
            t.lines().find_map(|l| l.strip_prefix("PRETTY_NAME=").map(|v| v.trim_matches('"').to_string()))
        })
    };
    found.filter(|v| !v.is_empty()).unwrap_or_else(|| "unknown version".into())
}

fn folder(p: &Path) -> String {
    format!("{} ({})", p.display(), if p.is_dir() { "exists" } else { "missing" })
}

fn found(paths: Vec<PathBuf>, is: fn(&Path) -> bool) -> String {
    let hits: Vec<String> = paths.iter().filter(|p| is(p)).map(|p| p.display().to_string()).collect();
    if hits.is_empty() {
        match paths.first() {
            Some(p) => format!("not found (looked in {})", p.display()),
            None => "not found".into(),
        }
    } else {
        hits.join(", ")
    }
}

fn wakatime_cli_line() -> String {
    let Some(cli) = wakatime_cli::find() else {
        return "not found (Hackatime's editor plugins add it, or kairos hackatime)".into();
    };
    let out = std::process::Command::new(&cli).arg("--version").stdin(std::process::Stdio::null()).output();
    let version = out.ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|v| !v.is_empty());
    format!("{} ({})", version.as_deref().unwrap_or("unknown version"), cli.display())
}

/// kairosd's last run of wakatime-cli with AI sync on, to see what Hackatime would get from agents' logs.
fn last_run(data: &Path) -> String {
    let path = wakatime::shadow_dir(data).join(wakatime::SHADOW_LAST_RUN);
    let Some(v) = fs::read_to_string(path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) else {
        return "not run yet (it runs when an agent finishes a turn)".into();
    };
    let at = v["at"].as_f64().and_then(|t| chrono::DateTime::from_timestamp(t as i64, 0)).map(|t| t.with_timezone(&chrono::Local));
    let when = at.map(|t| t.format("%b %-d, %H:%M").to_string()).unwrap_or_else(|| "?".into());
    match v["error"].as_str() {
        Some(e) => format!("last run {when} failed: {e}"),
        None => format!("last ran {when}, took {:.1}s", v["secs"].as_f64().unwrap_or(0.0)),
    }
}

fn duration(secs: f64) -> String {
    let m = (secs / 60.0).round() as u64;
    format!("{}h {:02}m", m / 60, m % 60)
}

fn daemon_line(status: &Value) -> String {
    let text = |k: &str| status[k].as_str().filter(|s| !s.is_empty());
    let mut parts = vec![format!("running {}", text("version").unwrap_or("?"))];
    if let (Some(f), Some(i)) = (text("focus"), text("idle")) {
        parts.push(format!("focus {f}, idle {i}"));
    }
    if let Some(s) = status["today_secs"].as_f64() {
        parts.push(format!("today {}", duration(s)));
    }
    if status["paused_until"].is_number() {
        parts.push("paused".into());
    }
    match status["extension_seen_secs_ago"].as_u64() {
        Some(s) => parts.push(format!("browser extension seen {s}s ago")),
        None => parts.push("browser extension not seen".into()),
    }
    if let Some(e) = text("upload_error") {
        parts.push(format!("upload error: {e}"));
    }
    if let Some(u) = text("update") {
        parts.push(format!("update: {u}"));
    }
    parts.join(", ")
}

fn is_warning(line: &str) -> bool {
    let l = line.to_lowercase();
    ["warning", "failed", "error", "could not", "stopped", "disabled", "panicked"].iter().any(|w| l.contains(w))
}

fn recent_warnings(data: &Path) -> Vec<String> {
    let mut lines: Vec<String> = fs::read(data.join("kairosd.log"))
        .map(|b| String::from_utf8_lossy(&b).lines().map(str::to_string).collect())
        .unwrap_or_default();
    if cfg!(target_os = "linux") {
        if let Some(journal) =
            install::output("journalctl", &["--user", "-u", paths::SYSTEMD_UNIT, "-n", "500", "-o", "short-iso", "--no-pager"])
        {
            lines.extend(journal.lines().map(str::to_string));
        }
    }
    let mut hits: Vec<String> = lines.into_iter().filter(|l| is_warning(l)).collect();
    hits.drain(..hits.len().saturating_sub(WARNINGS));
    hits
}

pub fn doctor() -> Result<()> {
    let data = paths::data_dir();
    let http = Client::builder().timeout(Duration::from_secs(5)).build()?;
    let target = update::current_target();
    let dir = install::install_dir().ok();
    let server = update::server_for(&data);

    println!("Kairos doctor. Copy everything below when you ask for help.\n");
    row("kairos", format!("{} ({})", env!("CARGO_PKG_VERSION"), std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default()));
    let daemon = dir.as_ref().map(|d| d.join(install::exe("kairosd")));
    row(
        "kairosd",
        match &daemon {
            Some(d) if d.is_file() => match update::binary_version(d) {
                Ok(v) => format!("{v} ({})", d.display()),
                Err(e) => format!("installed at {} but does not run: {e:#}", d.display()),
            },
            Some(d) => format!("not installed (expected at {})", d.display()),
            None => "not installed".into(),
        },
    );
    row(
        "latest release",
        match update::Client::new(&server).and_then(|c| c.latest()) {
            Ok(r) => format!("{} (signature ok)", r.version),
            Err(e) => format!("could not check ({e:#})"),
        },
    );
    row(
        "system",
        format!(
            "{} {} ({}), download target {}",
            std::env::consts::OS,
            std::env::consts::ARCH,
            os_version(),
            target.unwrap_or("none")
        ),
    );
    row("data folder", folder(&data));
    if let Some(d) = &dir {
        row("install folder", folder(d));
    }
    row("autostart", install::autostart());
    row("wakatime", crate::wakatime::describe().unwrap_or_else(|e| format!("unknown ({e:#})")));
    row(
        "hackatime",
        match wakatime::config_path() {
            Some(c) if wakatime::on_hackatime(&data, &c) => "set up".to_string(),
            _ => "not set up on this computer, run kairos hackatime".into(),
        },
    );
    row("wakatime-cli", wakatime_cli_line());
    row("hackatime w/ AI", last_run(&data));
    row(
        "excluded",
        match exclude::load(&data) {
            Ok(ex) => format!("{} folder{}", ex.roots.len(), if ex.roots.len() == 1 { "" } else { "s" }),
            Err(e) => format!("unreadable ({e:#})"),
        },
    );
    let api = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .build()?
        .get(format!("http://127.0.0.1:{}/v1/status", paths::API_PORT))
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.json::<Value>());
    row(
        "background app",
        match (api, install::daemon_pid(), install::daemon_running()) {
            (Ok(status), pid, _) => format!("{}{}", daemon_line(&status), pid.map(|p| format!(", pid {p}")).unwrap_or_default()),
            (Err(_), Some(pid), _) => format!("running (pid {pid}), local API not answering"),
            (Err(_), None, true) => "running, local API not answering".into(),
            (Err(_), None, false) => "not running".into(),
        },
    );
    row("Claude Code", found(claude::default_roots(), Path::is_dir));
    row("Codex", found(codex::default_roots(), Path::is_dir));
    row("opencode", found(vec![opencode::default_db()], Path::is_file));
    row(
        "server",
        match http.get(format!("{server}/healthz")).send() {
            Ok(r) if r.status().is_success() => format!("{server} (reachable)"),
            Ok(r) => format!("{server} (answered {})", r.status()),
            Err(e) => format!("{server} (not reachable: {e})"),
        },
    );
    row(
        "account",
        match account::load(&data) {
            Some(a) => format!("signed in as {} on {}", a.username, a.server),
            None => "not signed in, run kairos login".into(),
        },
    );
    row("upload backlog", format!("{} events not uploaded yet", upload::pending(&data, &State::load(&data))));

    let warnings = recent_warnings(&data);
    if warnings.is_empty() {
        println!("\nNo recent warnings in the kairosd log.");
    } else {
        println!("\nRecent warnings from kairosd:");
        for w in warnings {
            println!("  {w}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_line_from_the_local_api() {
        let v = serde_json::json!({
            "version": "1.2.3", "focus": "hyprland", "idle": "ext-idle-notify", "today_secs": 4980.0,
            "paused_until": null, "upload_error": null, "update": "installing 1.3.0", "extension_seen_secs_ago": 4
        });
        assert_eq!(
            daemon_line(&v),
            "running 1.2.3, focus hyprland, idle ext-idle-notify, today 1h 23m, browser extension seen 4s ago, update: installing 1.3.0"
        );
        assert_eq!(daemon_line(&serde_json::json!({})), "running ?, browser extension not seen");
    }

    #[test]
    fn warning_filter() {
        assert!(is_warning("2026-11-19 10:00:00 kairosd: warning: watch limit reached"));
        assert!(is_warning("kairosd: upload failed: timeout, retrying in 60s"));
        assert!(!is_warning("kairosd: uploaded 12 events"));
        assert!(!is_warning("kairosd: focus editor"));
    }
}
