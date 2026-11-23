
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Local};
use kairos_core::paths;
use kairos_core::wakatime::{self, Points, Released};

const USAGE: &str = "usage: kairos wakatime [on|off]";

fn config() -> Result<PathBuf> {
    wakatime::config_path().context("cannot find your home folder")
}

pub fn command(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        None => println!("{}", describe()?),
        Some("on") => {
            take()?;
            println!("{}", describe()?);
        }
        Some("off") => println!("{}", release()?.unwrap_or_else(|| "Kairos had not changed your WakaTime settings.".into())),
        Some(_) => bail!("{USAGE}"),
    }
    Ok(())
}

pub fn take() -> Result<PathBuf> {
    let path = config()?;
    wakatime::take(&paths::data_dir(), &path)?;
    Ok(path)
}

pub fn release() -> Result<Option<String>> {
    Ok(match wakatime::release(&paths::data_dir())? {
        Released::Restored(p) => Some(format!("WakaTime editor plugins use your earlier settings again ({}).", p.display())),
        Released::Untouched(p) => Some(format!("Left {} as it is: it no longer points at Kairos.", p.display())),
        Released::NotTaken => None,
    })
}

pub fn describe() -> Result<String> {
    let path = config()?;
    let upstream = wakatime::upstream(&paths::data_dir());
    let mut out = say(wakatime::points(&path)?, upstream.as_ref().map(|u| u.host()), &path);
    if let Some(line) = repointed() {
        out = format!("{out}\n{line}");
    }
    if wakatime::parked(&paths::data_dir()) {
        out = format!(
            "{out}\nAnother program is using Kairos's port ({}), so Kairos gave WakaTime plugins their earlier settings back. It takes them again once the port is free.",
            paths::API_PORT
        );
    }
    Ok(out)
}

/// Said when kairosd last pointed the config back at Kairos after something else changed it.
pub fn repointed() -> Option<String> {
    let data = paths::data_dir();
    let s = wakatime::state(&data)?;
    let when = DateTime::from_timestamp(s.repointed? as i64, 0)?.with_timezone(&Local).format("%b %-d, %H:%M").to_string();
    Some(changed(&s.config, &when, wakatime::upstream(&data).as_ref().map(|u| u.host())))
}

fn changed(config: &Path, when: &str, passed_on_to: Option<&str>) -> String {
    let on = passed_on_to.map(|h| format!(" and passes heartbeats on to {h} with the new key")).unwrap_or_default();
    format!("{} was changed to send heartbeats elsewhere on {when}. Kairos pointed it back{on}.", config.display())
}

fn say(points: Points, passed_on_to: Option<&str>, path: &Path) -> String {
    match points {
        Points::Kairos => {
            let passed_on = passed_on_to.map(|host| format!(", and Kairos passes them on to {host} like before")).unwrap_or_default();
            format!("WakaTime editor plugins now send to Kairos{passed_on} ({}). kairos wakatime off puts your earlier settings back.", path.display())
        }
        Points::Elsewhere(url) => format!(
            "WakaTime editor plugins send their heartbeats to {} ({}). Run kairos wakatime on to send them to Kairos.",
            url.as_deref().unwrap_or("WakaTime"),
            path.display()
        ),
        Points::NoConfig => "No WakaTime settings yet. Run kairos wakatime on to send editor heartbeats to Kairos.".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_when_the_config_was_pointed_back() {
        let path = Path::new("/home/a/.wakatime.cfg");
        assert_eq!(
            changed(path, "Nov 22, 14:03", Some("hackatime.hackclub.com")),
            "/home/a/.wakatime.cfg was changed to send heartbeats elsewhere on Nov 22, 14:03. \
             Kairos pointed it back and passes heartbeats on to hackatime.hackclub.com with the new key."
        );
        assert_eq!(
            changed(path, "Nov 22, 14:03", None),
            "/home/a/.wakatime.cfg was changed to send heartbeats elsewhere on Nov 22, 14:03. Kairos pointed it back."
        );
    }

    #[test]
    fn says_where_heartbeats_go_on_to() {
        let path = Path::new("/home/a/.wakatime.cfg");
        assert_eq!(
            say(Points::Kairos, Some("hackatime.hackclub.com"), path),
            "WakaTime editor plugins now send to Kairos, and Kairos passes them on to hackatime.hackclub.com like before (/home/a/.wakatime.cfg). \
             kairos wakatime off puts your earlier settings back."
        );
        assert_eq!(
            say(Points::Kairos, None, path),
            "WakaTime editor plugins now send to Kairos (/home/a/.wakatime.cfg). kairos wakatime off puts your earlier settings back."
        );
    }
}
