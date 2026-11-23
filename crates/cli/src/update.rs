use anyhow::{bail, Context, Result};
use kairos_core::paths;
use kairos_core::update as release;

use crate::install;

const USAGE: &str = "usage: kairos update [--check]";

pub fn update(args: &[String]) -> Result<()> {
    let mut check_only = false;
    for a in args {
        match a.as_str() {
            "--check" => check_only = true,
            other => bail!("unknown option {other:?}\n{USAGE}"),
        }
    }
    let target = release::current_target().context("there are no Kairos downloads for this kind of computer")?;
    let dir = install::install_dir()?;
    let daemon = dir.join(&release::binaries(target)[1]);
    if !daemon.is_file() {
        bail!("Kairos is not installed in {}, run the install command from the website first", dir.display());
    }
    let installed = release::installed_version(&daemon)
        .map(|v| v.to_string())
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
    let data = paths::data_dir();
    let client = release::Client::new(&release::server_for(&data))?;
    let latest = client.latest().with_context(|| format!("could not check {} for updates", client.server()))?;
    let Some(version) = latest.newer_than(&installed)?.map(|v| v.to_string()) else {
        println!("Kairos {installed} is up to date.");
        return Ok(());
    };
    if check_only {
        println!("Kairos {version} is available (you have {installed}). Run kairos update to install it.");
        return Ok(());
    }

    let Some(lock) = release::lock(&data)? else {
        bail!("another update is running right now, try again in a minute");
    };
    release::cleanup(&dir);
    println!("Downloading Kairos {version}...");
    client.install(&latest, target, &dir)?;
    drop(lock);
    if let Err(e) = install::refresh_entry(&version) {
        println!("Note: {e:#}");
    }
    println!("Installed Kairos {version}.");
    match install::daemon_pid() {
        Some(pid) => println!("Restarted the background app ({}).", install::restart(&daemon, pid)?),
        None if install::daemon_running() => println!("Log out and back in to start the new version."),
        None => println!("The background app is not running. It starts with the new version next time you log in."),
    }
    Ok(())
}
