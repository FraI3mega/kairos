
use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::{Local, TimeZone};
use kairos_core::credit::{total_credit_secs, Rule};
use kairos_core::event::load_events;
use kairos_core::paths::{self, API_PORT};
use reqwest::blocking::{Client, Response};
use serde::Deserialize;
use serde_json::{json, Value};

const NOT_RUNNING: &str = "kairosd is not running";

fn client() -> Result<Client> {
    // Localhost only: never go through a proxy from the environment.
    Client::builder().no_proxy().timeout(Duration::from_secs(3)).build().context("building the http client")
}

fn url(path: &str) -> String {
    format!("http://127.0.0.1:{API_PORT}{path}")
}

fn post(path: &str, body: &Value) -> Result<Response> {
    client()?
        .post(url(path))
        .header("X-Kairos-Client", "cli")
        .json(body)
        .send()
        .map_err(|_| anyhow::anyhow!("{NOT_RUNNING}, start it and try again"))
}

fn checked(res: Response) -> Result<Value> {
    let ok = res.status().is_success();
    let body: Value = res.json().unwrap_or(Value::Null);
    if ok {
        return Ok(body);
    }
    bail!("kairosd refused: {}", body["error"].as_str().unwrap_or("unknown error"))
}

#[derive(Deserialize)]
struct Live {
    version: String,
    #[serde(default)]
    focus: String,
    #[serde(default)]
    idle: String,
    #[serde(default)]
    today_secs: f64,
    paused_until: Option<f64>,
    extension_seen_secs_ago: Option<u64>,
    upload_error: Option<String>,
    update: Option<String>,
}

fn live() -> Option<Live> {
    client().ok()?.get(url("/v1/status")).send().ok()?.error_for_status().ok()?.json().ok()
}

pub fn focus_time(secs: f64) -> String {
    let minutes = (secs.max(0.0) / 60.0).round() as u64;
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m}m"),
        (h, m) => format!("{h}h {m:02}m"),
    }
}

fn clock(ts: f64) -> String {
    let Some(at) = Local.timestamp_opt(ts as i64, 0).single() else { return "later".into() };
    let format = if at.date_naive() == Local::now().date_naive() { "%H:%M" } else { "%a %H:%M" };
    at.format(format).to_string()
}

fn offline_today_secs() -> f64 {
    let now = Local::now();
    let path = paths::log_path(&now.date_naive().to_string());
    let Ok(events) = load_events(&path) else { return 0.0 };
    let midnight = now.date_naive().and_hms_opt(0, 0, 0).and_then(|m| Local.from_local_datetime(&m).earliest());
    let start = midnight.map_or(0.0, |m| m.timestamp() as f64);
    let credits: Vec<_> = Rule::CURRENT
        .apply(&events)
        .into_iter()
        .filter(|c| c.end > start)
        .map(|mut c| {
            c.start = c.start.max(start);
            c
        })
        .collect();
    total_credit_secs(&credits)
}

pub fn print_status() {
    match live() {
        Some(s) => {
            println!("kairosd  running {}", s.version);
            println!("today    {} of focus time", focus_time(s.today_secs));
            if let Some(until) = s.paused_until {
                println!("paused   until {}", clock(until));
            }
            println!("watching focus {}, presence {}", s.focus, s.idle);
            match s.extension_seen_secs_ago {
                Some(secs) => println!("browser  extension seen {secs}s ago"),
                None => println!("browser  extension not connected"),
            }
            if let Some(e) = s.upload_error {
                println!("upload   {e}");
            }
            if let Some(u) = s.update {
                println!("update   {u}");
            }
        }
        None => {
            println!("kairosd  not running");
            println!("today    {} of focus time (from the local log)", focus_time(offline_today_secs()));
        }
    }
}

fn parse_duration(arg: &str) -> Result<Value> {
    let bad = || anyhow::anyhow!("pause takes 30m, 1h, 2h or tomorrow, not {arg:?}");
    if arg == "tomorrow" {
        return Ok(json!({ "until": "tomorrow" }));
    }
    let unit = arg.chars().last().ok_or_else(bad)?;
    let n: u32 = arg[..arg.len() - unit.len_utf8()].parse().map_err(|_| bad())?;
    let minutes = match unit {
        'm' => n,
        'h' => n.checked_mul(60).ok_or_else(bad)?,
        _ => return Err(bad()),
    };
    Ok(json!({ "minutes": minutes }))
}

pub fn pause(args: &[String]) -> Result<()> {
    let arg = match args {
        [] => "30m",
        [one] => one.as_str(),
        _ => bail!("usage: kairos pause [30m|1h|2h|tomorrow]"),
    };
    let body = parse_duration(arg)?;
    let reply = checked(post("/v1/pause", &body)?)?;
    match reply["paused_until"].as_f64() {
        Some(until) => println!("Paused until {}", clock(until)),
        None => println!("Paused"),
    }
    Ok(())
}

pub fn resume() -> Result<()> {
    checked(post("/v1/resume", &json!({}))?)?;
    println!("Resumed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_become_minutes_or_tomorrow() {
        assert_eq!(parse_duration("30m").unwrap(), json!({ "minutes": 30 }));
        assert_eq!(parse_duration("1h").unwrap(), json!({ "minutes": 60 }));
        assert_eq!(parse_duration("2h").unwrap(), json!({ "minutes": 120 }));
        assert_eq!(parse_duration("tomorrow").unwrap(), json!({ "until": "tomorrow" }));
        for bad in ["", "m", "h", "30", "1.5h", "-5m", "5d", "soon", "99999999999h"] {
            assert!(parse_duration(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn focus_time_reads_like_the_tray() {
        assert_eq!(focus_time(0.0), "0m");
        assert_eq!(focus_time(29.0), "0m");
        assert_eq!(focus_time(45.0 * 60.0), "45m");
        assert_eq!(focus_time(3600.0), "1h 00m");
        assert_eq!(focus_time(5000.0), "1h 23m");
        assert_eq!(focus_time(-3.0), "0m");
    }

    #[test]
    fn status_json_from_the_daemon_parses() {
        let text = r#"{"version":"0.1.0","focus":"hyprland","idle":"none","today_secs":61.5,"paused_until":null,
            "username":null,"server":null,"last_upload":null,"upload_error":null,"update":null,"extension_seen_secs_ago":4}"#;
        let s: Live = serde_json::from_str(text).unwrap();
        assert_eq!((s.version.as_str(), s.extension_seen_secs_ago), ("0.1.0", Some(4)));
    }
}
