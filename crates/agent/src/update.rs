use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Result};
use kairos_core::{paths, update};

use crate::diag::diag;
use crate::status::{self, Shared};

const FIRST: Duration = Duration::from_secs(2 * 60);
const EVERY: Duration = Duration::from_secs(20 * 60);
const RETRY: Duration = Duration::from_secs(5 * 60);
/// Exit status that tells systemd (`RestartForceExitStatus=75`) or launchd (`SuccessfulExit: false`)
/// to start the freshly installed binary.
pub const RESTART: i32 = 75;

pub fn spawn(dir: PathBuf, status: Shared) {
    let (Some(install_dir), Some(target)) = (update::running_installed(), update::current_target()) else {
        diag!("kairosd: not running from the install folder, automatic updates are off");
        return;
    };
    let _ = thread::Builder::new().name("updater".into()).spawn(move || {
        thread::sleep(FIRST);
        // This kairosd has stayed up, so the version before it is no longer needed.
        if let Ok(Some(_lock)) = update::lock(&dir) {
            update::cleanup(&install_dir);
        }
        let mut failures = 0;
        loop {
            match check(&dir, &install_dir, target, &status) {
                Ok(Some(version)) => restart(&dir, &install_dir, target, &format!("updated to {version}")),
                Ok(None) => failures = 0,
                Err(e) => {
                    failures += 1;
                    diag!("kairosd: update failed: {e:#}");
                }
            }
            thread::sleep(delay(failures, jitter()));
        }
    });
}

fn check(dir: &Path, install_dir: &Path, target: &str, status: &Shared) -> Result<Option<String>> {
    let client = update::Client::new(&update::server_for(dir))?;
    let release = client.latest()?;
    let Some(version) = release.newer_than(env!("CARGO_PKG_VERSION"))?.map(|v| v.to_string()) else {
        return Ok(None);
    };
    if update::skipped(dir).is_some_and(|v| v == release.version) {
        return Ok(None);
    }
    let Some(_lock) = update::lock(dir)? else { bail!("another update is running") };
    let daemon = install_dir.join(&update::binaries(target)[1]);
    if update::installed_version(&daemon).is_some_and(|v| v == release.version) {
        diag!("kairosd: {version} is already installed");
        return Ok(Some(version));
    }
    status::set(status, |s| s.update = Some(format!("installing {version}")));
    diag!("kairosd: installing {version}");
    if let Err(e) = client.install(&release, target, install_dir) {
        status::set(status, |s| s.update = None);
        return Err(e);
    }
    Ok(Some(version))
}

/// Called by a starting kairosd once it holds the instance lock. If the update that installed it keeps starting without
/// staying up, the version before it is put back and started instead.
pub fn begin(dir: &Path) {
    let (Some(install_dir), Some(target)) = (update::running_installed(), update::current_target()) else { return };
    if update::begin(&install_dir, dir, env!("CARGO_PKG_VERSION")) {
        restart(dir, &install_dir, target, concat!("put the previous version back, ", env!("CARGO_PKG_VERSION"), " did not stay up"));
    }
}

const WATCH: [Duration; 2] = [Duration::from_secs(45), Duration::from_secs(55)];

/// `kairosd --watch-update`, run from the kairosd an update set aside where nothing restarts a kairosd that dies
/// (Windows, a plain autostart entry): if the new one is gone soon after it started, the old one is put back.
pub fn watch() {
    let dir = paths::data_dir();
    crate::diag::init(&dir);
    let Some(install_dir) = paths::install_dir() else { return };
    let Some(target) = update::current_target() else { return };
    for wait in WATCH {
        thread::sleep(wait);
        let Ok(Some(instance)) = crate::instance::acquire(&dir) else { continue };
        let Ok(Some(_lock)) = update::lock(&dir) else { return };
        if !update::roll_back(&install_dir, &dir) {
            return;
        }
        drop(instance);
        diag!("kairosd: the updated kairosd is not running, put the previous version back");
        let names = update::binaries(target);
        if cfg!(windows) {
            let _ = quiet(Command::new(install_dir.join(&names[0])).arg("uninstall-entry")).status();
        }
        if let Err(e) = start(&dir, &install_dir.join(&names[1]), &[]) {
            diag!("kairosd: could not start it ({e}), it runs from the next start");
        }
        return;
    }
}

fn start(dir: &Path, exe: &Path, args: &[String]) -> std::io::Result<()> {
    let mut cmd = Command::new(exe);
    cmd.args(args);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let log = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("kairosd.log"));
        cmd.process_group(0).stderr(log.map(Stdio::from).unwrap_or_else(|_| Stdio::null()));
    }
    #[cfg(not(unix))]
    let _ = dir;
    quiet(&mut cmd).spawn().map(|_| ())
}

fn restart(dir: &Path, install_dir: &Path, target: &str, why: &str) {
    let names = update::binaries(target);
    if cfg!(windows) {
        let _ = quiet(Command::new(install_dir.join(&names[0])).arg("uninstall-entry")).status();
    }
    let cgroup = std::fs::read_to_string("/proc/self/cgroup").ok();
    let supervisor = supervisor(
        std::env::var("INVOCATION_ID").ok().as_deref(),
        cgroup.as_deref(),
        std::env::var("XPC_SERVICE_NAME").ok().as_deref(),
    );
    if let Some(name) = supervisor {
        diag!("kairosd: {why}, exiting so {name} starts it");
        std::process::exit(RESTART);
    }
    if let Some(previous) = update::previous_daemon(install_dir, target) {
        let _ = start(dir, &previous, &["--watch-update".to_string()]);
    }
    match start(dir, &install_dir.join(&names[1]), &["--replace".to_string(), std::process::id().to_string()]) {
        Ok(()) => {
            diag!("kairosd: {why}, handing over to that kairosd");
            std::process::exit(0);
        }
        Err(e) => diag!("kairosd: {why} but could not start it ({e}), it runs from the next start"),
    }
}

fn quiet(cmd: &mut Command) -> &mut Command {
    cmd.stdin(Stdio::null()).stdout(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.stderr(Stdio::null()).creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
    cmd
}

/// `INVOCATION_ID` alone is not enough: terminals started by systemd pass it on to shells.
fn supervisor(invocation: Option<&str>, cgroup: Option<&str>, xpc_service: Option<&str>) -> Option<&'static str> {
    let unit = format!("/{}", paths::SYSTEMD_UNIT);
    if invocation.is_some_and(|i| !i.is_empty()) && cgroup.is_some_and(|c| c.lines().any(|l| l.ends_with(&unit))) {
        return Some("systemd");
    }
    if xpc_service == Some(paths::LAUNCHD_LABEL) {
        return Some("launchd");
    }
    None
}

fn jitter() -> u64 {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    u64::from(nanos ^ std::process::id().rotate_left(16))
}

fn delay(failures: u32, jitter: u64) -> Duration {
    let base = match failures {
        0 => EVERY,
        n => RETRY.saturating_mul(1 << (n - 1).min(5)).min(EVERY),
    };
    base + Duration::from_secs(jitter % (base.as_secs() / 8).max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supervisor_detection() {
        let cg = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/kairosd.service\n";
        assert_eq!(supervisor(Some("abc"), Some(cg), None), Some("systemd"));
        assert_eq!(supervisor(None, Some(cg), None), None);
        let terminal = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/vte-spawn-1.scope\n";
        assert_eq!(supervisor(Some("abc"), Some(terminal), None), None);
        assert_eq!(supervisor(None, None, Some("dev.obayy.kairos")), Some("launchd"));
        assert_eq!(supervisor(None, None, Some("0")), None);
        assert_eq!(supervisor(None, None, Some("application.com.apple.Terminal.1")), None);
    }

    #[test]
    fn delays_back_off_and_stay_jittered() {
        assert!(delay(0, 0) == EVERY);
        assert!(delay(0, u64::MAX) < EVERY + EVERY / 8);
        assert_eq!(delay(1, 0), RETRY);
        assert_eq!(delay(2, 0), RETRY * 2);
        assert_eq!(delay(3, 0), RETRY * 4);
        assert_eq!(delay(40, 0), EVERY);
        assert!(delay(1, 7) > RETRY);
    }
}
