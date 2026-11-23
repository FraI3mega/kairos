use chrono::{Days, NaiveDate};
use chrono_tz::Tz;
use kairos_core::event::{Class, Event};

use crate::rng::Rng;
use crate::tape::Tape;
use crate::work::{chat_loop, editor_and_docs, editor_burst, local, local_day, multi_agent, turn, Chat};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Cadence {
    Every(f64),
    AtEnd,
}

pub struct DeviceLog {
    pub name: String,
    pub os: &'static str,
    pub events: Vec<Event>,
    pub cadence: Cadence,
    pub lose_response: f64,
}

pub struct Persona {
    pub name: &'static str,
    pub blurb: &'static str,
    pub tz: Tz,
    pub tz_after_upload: bool,
    pub devices: Vec<DeviceLog>,
}

impl Persona {
    pub fn event_count(&self) -> usize {
        self.devices.iter().map(|d| d.events.len()).sum()
    }
}

#[derive(Clone, Copy)]
pub struct Ctx {
    pub now: f64,
    pub seed: u64,
}

pub const NAMES: &[&str] =
    &["steady", "t3-multi", "cursor-docs", "walkaway", "pauser", "night-owl", "night-owl-late-tz", "dst-owl", "two-device", "flaky"];

fn tz(name: &str) -> Tz {
    name.parse().expect("known timezone")
}

fn seed_of(ctx: Ctx, name: &str) -> Rng {
    let h = name.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3));
    Rng::new(ctx.seed ^ h)
}

fn days_ago(tz: Tz, ctx: Ctx, n: u64) -> NaiveDate {
    local_day(tz, ctx.now).checked_sub_days(Days::new(n)).unwrap()
}

fn device(name: &str, os: &'static str, tape: Tape, cadence: Cadence) -> DeviceLog {
    DeviceLog { name: name.into(), os, events: tape.finish(), cadence, lose_response: 0.0 }
}

fn jitter(rng: &mut Rng, h: f64) -> f64 {
    h + rng.range(-0.3, 0.3)
}

fn steady(ctx: Ctx) -> Persona {
    let z = tz("America/New_York");
    let mut rng = seed_of(ctx, "steady");
    let mut tape = Tape::new();
    let project = "obaym/steady-app";
    for back in [3, 2, 1] {
        let day = days_ago(z, ctx, back);
        tape.start(local(z, day, 9.2), "linux");
        let chat = Chat::new(project, "claude-code", &format!("steady-{back}"), Class::AgentTerminal);
        let (a, b) = (jitter(&mut rng, 9.5), jitter(&mut rng, 12.25));
        chat_loop(&mut tape, &mut rng, &chat, local(z, day, a), local(z, day, b));
        let (a, b) = (jitter(&mut rng, 13.5), jitter(&mut rng, 17.0));
        chat_loop(&mut tape, &mut rng, &chat, local(z, day, a), local(z, day, b));
    }
    Persona {
        name: "steady",
        blurb: "Claude Code in one project, three weekdays, two sessions a day",
        tz: z,
        tz_after_upload: false,
        devices: vec![device("steady-laptop", "linux", tape, Cadence::Every(600.0))],
    }
}

fn t3_multi(ctx: Ctx) -> Persona {
    let z = tz("Europe/London");
    let mut rng = seed_of(ctx, "t3-multi");
    let mut tape = Tape::new();
    let projects = ["obaym/t3-web", "obaym/t3-api", "obaym/t3-docs"];
    for back in [3, 2, 1] {
        let day = days_ago(z, ctx, back);
        tape.start(local(z, day, 9.8), "macos");
        let mk = |p: usize, harness: &'static str, tag: &str| {
            let mut c = Chat::new(projects[p], harness, &format!("t3-{back}-{tag}"), Class::AgentApp);
            c.run = (90.0, 600.0);
            c.reply_words = (150, 1200);
            c
        };
        let threads = [mk(0, "claude-code", "web-a"), mk(0, "codex", "web-b"), mk(1, "claude-code", "api"), mk(2, "codex", "docs")];
        let (a, b) = (jitter(&mut rng, 10.0), jitter(&mut rng, 13.0));
        multi_agent(&mut tape, &mut rng, &threads, local(z, day, a), local(z, day, b), projects[0]);
        let (a, b) = (jitter(&mut rng, 15.0), jitter(&mut rng, 18.5));
        multi_agent(&mut tape, &mut rng, &threads, local(z, day, a), local(z, day, b), projects[1]);
    }
    Persona {
        name: "t3-multi",
        blurb: "T3 Code style: four agent threads over three projects running in parallel",
        tz: z,
        tz_after_upload: false,
        devices: vec![device("t3-mac", "macos", tape, Cadence::Every(1800.0))],
    }
}

fn cursor_docs(ctx: Ctx) -> Persona {
    let z = tz("America/Los_Angeles");
    let mut rng = seed_of(ctx, "cursor-docs");
    let mut tape = Tape::new();
    let project = "obaym/cursor-site";
    for back in [2, 1] {
        let day = days_ago(z, ctx, back);
        tape.start(local(z, day, 9.9), "windows");
        let mut chat = Chat::new(project, "cursor", &format!("cursor-{back}"), Class::Editor);
        chat.run = (15.0, 90.0);
        chat.reply_words = (60, 400);
        editor_and_docs(&mut tape, &mut rng, &chat, local(z, day, 10.0), local(z, day, 12.5));
        editor_and_docs(&mut tape, &mut rng, &chat, local(z, day, 14.0), local(z, day, 17.5));
        // Docs focus as older daemons recorded it, which the live rule ignores.
        tape.focus(local(z, day, 20.0), Class::Docs, None);
        tape.touch(local(z, day, 20.0), local(z, day, 20.75));
    }
    Persona {
        name: "cursor-docs",
        blurb: "Cursor with an agent panel, plus docs and web agent focus that earns nothing",
        tz: z,
        tz_after_upload: false,
        devices: vec![device("cursor-pc", "windows", tape, Cadence::Every(1200.0))],
    }
}

fn walkaway(ctx: Ctx) -> Persona {
    let z = tz("UTC");
    let mut rng = seed_of(ctx, "walkaway");
    let mut tape = Tape::new();
    let project = "obaym/walkaway";
    let mut chat = Chat::new(project, "claude-code", "walk", Class::AgentTerminal);
    chat.edit_p = 0.0;
    chat.browse_p = 0.0;
    chat.tests_every = 0;

    let d3 = days_ago(z, ctx, 3);
    let d2 = days_ago(z, ctx, 2);
    tape.start(local(z, d3, 22.0), "linux");
    chat_loop(&mut tape, &mut rng, &chat, local(z, d3, 22.0), local(z, d3, 22.2));
    let p = local(z, d3, 22.5);
    tape.touch(p - 20.0, p);
    tape.prompt(p, project, "claude-code", "walk-night", 25, false);
    tape.agent(p + 1.0, local(z, d2, 6.5), project, "claude-code", "walk-night", 3500);
    tape.focus(p + 2.0, Class::AgentTerminal, Some(project));

    let back = local(z, d2, 9.0);
    tape.touch(back, back + 400.0);
    chat_loop(&mut tape, &mut rng, &chat, back + 400.0, back + 1500.0);
    let p = local(z, d2, 10.5);
    let mut c = chat.clone();
    c.session = "walk-day".into();
    c.run = (3.0 * 3600.0, 3.0 * 3600.0 + 1.0);
    c.reply_words = (4000, 4001);
    c.prompt_words = (30, 31);
    turn(&mut tape, &mut rng, &c, p, true);
    tape.focus(p + 60.0, Class::BrowserUnknown, None);
    let back = p + 3.0 * 3600.0 + 1500.0;
    tape.focus(back, Class::AgentTerminal, Some(project));
    tape.touch(back, back + 600.0);
    chat_loop(&mut tape, &mut rng, &chat, back + 600.0, back + 1500.0);
    Persona {
        name: "walkaway",
        blurb: "starts long agent runs and walks away: should earn almost nothing",
        tz: z,
        tz_after_upload: false,
        devices: vec![device("walk-box", "linux", tape, Cadence::Every(900.0))],
    }
}

fn pauser(ctx: Ctx) -> Persona {
    let z = tz("Australia/Sydney");
    let mut rng = seed_of(ctx, "pauser");
    let mut tape = Tape::new();
    let project = "obaym/pause-app";
    let chat = Chat::new(project, "claude-code", "pause", Class::AgentTerminal);
    let d2 = days_ago(z, ctx, 2);
    let d1 = days_ago(z, ctx, 1);
    tape.start(local(z, d2, 8.8), "macos");
    chat_loop(&mut tape, &mut rng, &chat, local(z, d2, 9.0), local(z, d2, 13.0));
    tape.pause(local(z, d2, 10.0), local(z, d2, 11.0), None);
    tape.pause(local(z, d2, 11.7), local(z, d2, 23.7), Some(local(z, d2, 11.9)));
    tape.pause(local(z, d2, 12.5), local(z, d2, 12.8), None);
    // "Until tomorrow", never resumed by hand: it ends by itself the next morning.
    tape.start(local(z, d1, 8.8), "macos");
    chat_loop(&mut tape, &mut rng, &chat, local(z, d1, 9.0), local(z, d1, 11.0));
    tape.pause(local(z, d1, 10.0), local(z, d1, 10.6), None);
    chat_loop(&mut tape, &mut rng, &chat, local(z, d1, 14.0), local(z, d1, 16.0));
    tape.pause(local(z, d1, 15.9), local(z, d1, 30.0), None);
    Persona {
        name: "pauser",
        blurb: "pauses for an hour, resumes early, lets pauses lapse, pauses until tomorrow",
        tz: z,
        tz_after_upload: false,
        devices: vec![device("pause-mac", "macos", tape, Cadence::Every(600.0))],
    }
}

fn night_owl_log(ctx: Ctx, z: Tz, name: &str, project: &str) -> Tape {
    let mut rng = seed_of(ctx, name);
    let mut tape = Tape::new();
    for back in [3, 2] {
        let day = days_ago(z, ctx, back);
        tape.start(local(z, day, 21.8), "linux");
        let chat = Chat::new(project, "claude-code", &format!("{name}-{back}"), Class::AgentTerminal);
        chat_loop(&mut tape, &mut rng, &chat, local(z, day, 22.0), local(z, day, 26.5));
        editor_burst(&mut tape, &mut rng, project, local(z, day, 23.9), local(z, day, 24.2));
    }
    tape
}

fn night_owl(ctx: Ctx) -> Persona {
    let z = tz("Asia/Kolkata");
    Persona {
        name: "night-owl",
        blurb: "codes 22:00 to 02:30 in India (UTC+5:30), across local midnight",
        tz: z,
        tz_after_upload: false,
        devices: vec![device("owl-laptop", "linux", night_owl_log(ctx, z, "night-owl", "obaym/owl-app"), Cadence::Every(1800.0))],
    }
}

fn night_owl_late_tz(ctx: Ctx) -> Persona {
    let z = tz("America/Los_Angeles");
    Persona {
        name: "night-owl-late-tz",
        blurb: "like night-owl but the account is still on UTC while uploading, timezone set afterwards",
        tz: z,
        tz_after_upload: true,
        devices: vec![device("owl2-laptop", "linux", night_owl_log(ctx, z, "night-owl-late-tz", "obaym/owl2-app"), Cadence::Every(1800.0))],
    }
}

fn dst_owl(ctx: Ctx) -> Persona {
    let z = tz("America/New_York");
    let mut rng = seed_of(ctx, "dst-owl");
    let mut tape = Tape::new();
    let project = "obaym/dst-app";
    for (y, m, d) in [(2026, 3, 7), (2026, 10, 31)] {
        let day = NaiveDate::from_ymd_opt(y, m, d).unwrap();
        tape.start(local(z, day, 21.8), "linux");
        let chat = Chat::new(project, "claude-code", &format!("dst-{m}"), Class::AgentTerminal);
        chat_loop(&mut tape, &mut rng, &chat, local(z, day, 22.0), local(z, day, 28.0));
        editor_burst(&mut tape, &mut rng, project, local(z, day, 25.8), local(z, day, 26.4));
    }
    Persona {
        name: "dst-owl",
        blurb: "two nights across the spring and fall DST changes in New York",
        tz: z,
        tz_after_upload: false,
        devices: vec![device("dst-laptop", "linux", tape, Cadence::Every(1800.0))],
    }
}

fn two_device(ctx: Ctx) -> Persona {
    let z = tz("Europe/Berlin");
    let mut rng = seed_of(ctx, "two-device");
    let mut laptop = Tape::new();
    let mut desktop = Tape::new();
    let (a, b) = ("obaym/two-a", "obaym/two-b");
    for back in [2, 1] {
        let day = days_ago(z, ctx, back);
        laptop.start(local(z, day, 8.9), "macos");
        desktop.start(local(z, day, 10.9), "windows");
        let chat = Chat::new(a, "claude-code", &format!("two-lap-{back}"), Class::AgentTerminal);
        chat_loop(&mut laptop, &mut rng, &chat, local(z, day, 9.0), local(z, day, 13.0));
        let mut chat = Chat::new(b, "cursor", &format!("two-desk-{back}"), Class::Editor);
        chat.run = (15.0, 80.0);
        editor_and_docs(&mut desktop, &mut rng, &chat, local(z, day, 11.0), local(z, day, 12.5));
        let mut chat = Chat::new(a, "cursor", &format!("two-desk2-{back}"), Class::Editor);
        chat.run = (15.0, 80.0);
        editor_and_docs(&mut desktop, &mut rng, &chat, local(z, day, 12.5), local(z, day, 15.0));
    }
    let mut p = Persona {
        name: "two-device",
        blurb: "laptop and desktop overlapping for hours; the desktop uploads late",
        tz: z,
        tz_after_upload: false,
        devices: vec![device("two-laptop", "macos", laptop, Cadence::Every(600.0)), device("two-desktop", "windows", desktop, Cadence::AtEnd)],
    };
    p.devices[1].lose_response = 0.2;
    p
}

fn flaky(ctx: Ctx) -> Persona {
    let z = tz("America/Sao_Paulo");
    let mut rng = seed_of(ctx, "flaky");
    let mut tape = Tape::new();
    let project = "obaym/flaky-net";
    for back in [8, 7, 6, 5, 4, 3, 2] {
        let day = days_ago(z, ctx, back);
        tape.start(local(z, day, 8.8), "linux");
        let mut chat = Chat::new(project, "claude-code", &format!("flaky-{back}"), Class::AgentTerminal);
        chat.edit_p = 0.45;
        chat_loop(&mut tape, &mut rng, &chat, local(z, day, 9.0), local(z, day, 13.0));
        chat_loop(&mut tape, &mut rng, &chat, local(z, day, 14.0), local(z, day, 19.0));
    }
    let mut d = device("flaky-laptop", "linux", tape, Cadence::AtEnd);
    d.lose_response = 0.3;
    Persona {
        name: "flaky",
        blurb: "a week offline, then uploads everything in 5000 event batches with lost responses",
        tz: z,
        tz_after_upload: false,
        devices: vec![d],
    }
}

pub fn build(ctx: Ctx, name: &str) -> Option<Persona> {
    Some(match name {
        "steady" => steady(ctx),
        "t3-multi" => t3_multi(ctx),
        "cursor-docs" => cursor_docs(ctx),
        "walkaway" => walkaway(ctx),
        "pauser" => pauser(ctx),
        "night-owl" => night_owl(ctx),
        "night-owl-late-tz" => night_owl_late_tz(ctx),
        "dst-owl" => dst_owl(ctx),
        "two-device" => two_device(ctx),
        "flaky" => flaky(ctx),
        _ => return None,
    })
}

pub fn select(ctx: Ctx, spec: &str) -> anyhow::Result<Vec<Persona>> {
    let names: Vec<&str> = if spec == "all" { NAMES.to_vec() } else { spec.split(',').map(str::trim).filter(|s| !s.is_empty()).collect() };
    names.into_iter().map(|n| build(ctx, n).ok_or_else(|| anyhow::anyhow!("unknown persona {n:?}, known: {}", NAMES.join(", ")))).collect()
}

pub fn live_user(rng: &mut Rng, kind: LiveKind, from: f64, to: f64) -> Vec<DeviceLog> {
    let mut tape = Tape::new();
    let name = format!("u{:x}", rng.next_u64() & 0xffffff);
    let project = format!("sim/{name}-app");
    let other = format!("sim/{name}-lib");
    tape.start(from - 30.0, "linux");
    match kind {
        LiveKind::Steady => {
            let chat = Chat::new(&project, "claude-code", &format!("{name}-s"), Class::AgentTerminal);
            work_with_breaks(rng, from, to, |rng, a, b| {
                chat_loop(&mut tape, rng, &chat, a, b);
            });
        }
        LiveKind::Multi => {
            let mk = |p: &str, h: &'static str, tag: &str| {
                let mut c = Chat::new(p, h, &format!("{name}-{tag}"), Class::AgentApp);
                c.run = (90.0, 600.0);
                c.reply_words = (150, 1200);
                c
            };
            let threads = [mk(&project, "claude-code", "a"), mk(&project, "codex", "b"), mk(&other, "claude-code", "c")];
            work_with_breaks(rng, from, to, |rng, a, b| {
                multi_agent(&mut tape, rng, &threads, a, b, &project);
            });
        }
        LiveKind::Cursor => {
            let mut chat = Chat::new(&project, "cursor", &format!("{name}-c"), Class::Editor);
            chat.run = (15.0, 90.0);
            work_with_breaks(rng, from, to, |rng, a, b| {
                editor_and_docs(&mut tape, rng, &chat, a, b);
            });
        }
        LiveKind::Walkaway => {
            let mut chat = Chat::new(&project, "claude-code", &format!("{name}-w"), Class::AgentTerminal);
            chat.tests_every = 0;
            let mid = from + (to - from) * 0.3;
            chat_loop(&mut tape, rng, &chat, from, mid.min(from + 900.0));
            chat.run = (3600.0, 3601.0);
            turn(&mut tape, rng, &chat, mid, true);
        }
        LiveKind::Pauser => {
            let chat = Chat::new(&project, "claude-code", &format!("{name}-p"), Class::AgentTerminal);
            work_with_breaks(rng, from, to, |rng, a, b| {
                chat_loop(&mut tape, rng, &chat, a, b);
            });
            let at = from + (to - from) * 0.5;
            tape.pause(at, at + 1200.0, None);
        }
    }
    vec![DeviceLog { name: format!("{name}-pc"), os: "linux", events: tape.finish(), cadence: Cadence::Every(60.0), lose_response: 0.0 }]
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LiveKind {
    Steady,
    Multi,
    Cursor,
    Walkaway,
    Pauser,
}

impl LiveKind {
    pub fn pick(rng: &mut Rng) -> LiveKind {
        match rng.int(0, 99) {
            0..=39 => LiveKind::Steady,
            40..=59 => LiveKind::Multi,
            60..=84 => LiveKind::Cursor,
            85..=93 => LiveKind::Walkaway,
            _ => LiveKind::Pauser,
        }
    }
}

fn work_with_breaks(rng: &mut Rng, from: f64, to: f64, mut f: impl FnMut(&mut Rng, f64, f64)) {
    let mut t = from;
    while t < to {
        let len = rng.range(20.0, 70.0) * 60.0;
        f(rng, t, (t + len).min(to));
        t += len + rng.range(3.0, 12.0) * 60.0;
    }
}

#[cfg(test)]
mod tests {
    use kairos_core::credit::{total_credit_secs, Rule};
    use kairos_core::event::{verify_chain, GENESIS};

    use super::*;

    // 2026-11-20 12:00 UTC.
    const NOW: f64 = 1_795_176_000.0;

    fn ctx() -> Ctx {
        Ctx { now: NOW, seed: 42 }
    }

    fn all() -> Vec<Persona> {
        select(ctx(), "all").unwrap()
    }

    fn credit_hours(d: &DeviceLog) -> f64 {
        total_credit_secs(&Rule::CURRENT.apply(&d.events)) / 3600.0
    }

    #[test]
    fn every_chain_verifies() {
        for p in all() {
            for d in &p.devices {
                let lines: Vec<String> = d.events.iter().map(|e| serde_json::to_string(e).unwrap()).collect();
                let v = verify_chain(lines.iter().map(String::as_str), Some(GENESIS)).unwrap_or_else(|b| panic!("{} {}: {b:?}", p.name, d.name));
                assert_eq!(v.events, d.events.len());
                assert!(d.events.windows(2).all(|w| w[0].ts <= w[1].ts), "{} is not time ordered", p.name);
                assert!(d.events.last().unwrap().ts <= NOW, "{} has events in the future", p.name);
            }
        }
    }

    #[test]
    fn same_seed_same_log_and_credit() {
        let (a, b) = (all(), all());
        for (x, y) in a.iter().zip(&b) {
            for (dx, dy) in x.devices.iter().zip(&y.devices) {
                assert_eq!(dx.events, dy.events, "{}", x.name);
                // Per class only: the live rule can split shared reading time between projects
                // differently on each call (see expected.rs), the totals do not move.
                let by_class = |d: &DeviceLog| {
                    let mut m = std::collections::BTreeMap::<&str, i64>::new();
                    for c in Rule::CURRENT.apply(&d.events) {
                        *m.entry(c.class.as_str()).or_default() += ((c.end - c.start) * 1000.0).round() as i64;
                    }
                    m
                };
                assert_eq!(by_class(dx), by_class(dy), "{}", x.name);
            }
        }
        let other = select(Ctx { now: NOW, seed: 43 }, "steady").unwrap();
        assert_ne!(a[0].devices[0].events, other[0].devices[0].events);
    }

    #[test]
    fn credit_is_plausible() {
        let by_name: std::collections::HashMap<_, _> = all().into_iter().map(|p| (p.name, p)).collect();
        let hours = |n: &str| by_name[n].devices.iter().map(credit_hours).sum::<f64>();
        // About 5.5 working hours a day for three days.
        let steady = hours("steady");
        assert!((4.0..16.0).contains(&steady), "steady {steady}");
        // Long agent runs with nobody there earn almost nothing.
        let walk = hours("walkaway");
        assert!(walk < 1.2, "walkaway {walk}");
        assert!(walk < steady / 5.0);
        assert!(hours("pauser") > 1.0);
        for n in NAMES {
            assert!(hours(n) > 0.0, "{n} earns nothing");
        }
    }

    #[test]
    fn pauses_hold() {
        let p = pauser(ctx());
        let ev = &p.devices[0].events;
        let credit = Rule::CURRENT.apply(ev);
        let z = p.tz;
        let d2 = days_ago(z, ctx(), 2);
        let (lo, hi) = (local(z, d2, 10.0), local(z, d2, 11.0));
        assert!(credit.iter().all(|c| c.end <= lo || c.start >= hi), "credit inside a pause");
        assert!(ev.iter().all(|e| !(e.ts > lo && e.ts < hi)), "events recorded while paused");
    }

    #[test]
    fn flaky_needs_more_than_one_batch() {
        let p = flaky(ctx());
        assert!(p.event_count() > 5000, "{}", p.event_count());
    }

    #[test]
    fn two_devices_overlap() {
        let p = two_device(ctx());
        let span = |d: &DeviceLog| (d.events.first().unwrap().ts, d.events.last().unwrap().ts);
        let (a, b) = (span(&p.devices[0]), span(&p.devices[1]));
        assert!(b.0 < a.1 && a.0 < b.1);
        assert_eq!(p.devices[1].cadence, Cadence::AtEnd);
    }

    #[test]
    fn night_owl_crosses_local_midnight() {
        let p = night_owl(ctx());
        let days: std::collections::BTreeSet<_> = p.devices[0].events.iter().map(|e| local_day(p.tz, e.ts)).collect();
        assert_eq!(days.len(), 3, "{days:?}");
    }

    #[test]
    fn live_users_work_in_the_window() {
        let mut rng = Rng::new(9);
        for kind in [LiveKind::Steady, LiveKind::Multi, LiveKind::Cursor, LiveKind::Walkaway, LiveKind::Pauser] {
            let d = &live_user(&mut rng, kind, 10_000.0, 10_000.0 + 3600.0)[0];
            assert!(d.events.len() > 10, "{kind:?}");
            assert!(d.events.iter().filter(|e| e.ts >= 10_000.0 && e.ts <= 13_600.0).count() > 5, "{kind:?}");
        }
    }
}
