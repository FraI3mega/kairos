//! The session may not exist yet when the daemon starts (autostart before login) and a compositor can restart, so missing
//! backends are probed again every 30 s and a backend that dies is replaced.

use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;

use super::backend::Running;
use super::presence::IDLE_AFTER;
use super::session::{self, SessionEnv};
use super::{dbus_idle, gnome, hyprland, kwin, niri, sway, wayland_idle, x11};
use crate::diag::diag;
use crate::platform::{Backends, Foreground, Sinks};
use crate::Msg;

const REPROBE: Duration = Duration::from_secs(30);
const WATCH: Duration = Duration::from_secs(2);
// A backend that dies sooner than this after starting is not retried before the next regular probe,
// so one that cannot stay up does not turn into a restart loop.
const STABLE: Duration = Duration::from_secs(10);

type FocusStart = Box<dyn Fn(&SessionEnv, Sender<Option<Foreground>>) -> Result<Running> + Send>;
type IdleStart = Box<dyn Fn(&SessionEnv, Sender<Msg>) -> Result<Running> + Send>;

struct Active {
    run: Running,
    since: Instant,
}

pub struct Supervisor {
    sinks: Sinks,
    discover: Box<dyn Fn() -> Option<SessionEnv> + Send>,
    focus_candidates: Vec<FocusStart>,
    idle_candidates: Vec<IdleStart>,
    focus: Option<Active>,
    idle: Option<Active>,
    reported: Backends,
    next_probe: Instant,
}

impl Supervisor {
    fn new(
        sinks: Sinks,
        discover: Box<dyn Fn() -> Option<SessionEnv> + Send>,
        focus_candidates: Vec<FocusStart>,
        idle_candidates: Vec<IdleStart>,
        now: Instant,
    ) -> Supervisor {
        Supervisor { sinks, discover, focus_candidates, idle_candidates, focus: None, idle: None, reported: Backends::NONE, next_probe: now }
    }

    fn names(&self) -> Backends {
        Backends {
            focus: self.focus.as_ref().map_or("none", |a| a.run.name),
            idle: self.idle.as_ref().map_or("none", |a| a.run.name),
        }
    }

    fn probe(&mut self, now: Instant) {
        self.next_probe = now + REPROBE;
        let Some(env) = (self.discover)() else { return };
        if self.focus.is_none() {
            let sink = &self.sinks.foreground;
            self.focus = self
                .focus_candidates
                .iter()
                .find_map(|start| start(&env, sink.clone()).ok())
                .map(|run| Active { run, since: now });
        }
        if self.idle.is_none() {
            let sink = &self.sinks.events;
            self.idle = self.idle_candidates.iter().find_map(|start| start(&env, sink.clone()).ok()).map(|run| Active { run, since: now });
        }
    }

    fn reap(&mut self, now: Instant) -> bool {
        let mut lost_stable = false;
        if let Some(a) = self.focus.take_if(|a| !a.run.is_alive()) {
            lost_stable |= now.saturating_duration_since(a.since) >= STABLE;
            let _ = self.sinks.foreground.send(None);
        }
        if let Some(a) = self.idle.take_if(|a| !a.run.is_alive()) {
            lost_stable |= now.saturating_duration_since(a.since) >= STABLE;
            let _ = self.sinks.events.send(Msg::PresenceLost);
        }
        lost_stable
    }

    fn tick(&mut self, now: Instant) -> Option<Backends> {
        let lost = self.reap(now);
        let missing = self.focus.is_none() || self.idle.is_none();
        if lost || (missing && now >= self.next_probe) {
            self.probe(now);
        }
        let names = self.names();
        (names != self.reported).then(|| {
            self.reported = names.clone();
            names
        })
    }
}

fn log(b: &Backends) {
    diag!("kairosd: focus backend: {}, idle backend: {}", b.focus, b.idle);
    if b.focus == "none" {
        diag!("kairosd: warning: focus is not tracked (no supported desktop found)");
    }
    if b.idle == "none" {
        diag!("kairosd: warning: presence unknown, emitting no present events");
    }
}

pub fn spawn(sinks: Sinks) -> Backends {
    let events = sinks.events.clone();
    let focus: Vec<FocusStart> = vec![
        Box::new(hyprland::start),
        Box::new(sway::start_sway),
        Box::new(sway::start_i3),
        Box::new(niri::start),
        Box::new(kwin::start),
        Box::new(gnome::start),
        Box::new(x11::start),
    ];
    let idle: Vec<IdleStart> = vec![
        Box::new(|env, tx| wayland_idle::start(env, tx, IDLE_AFTER)),
        Box::new(|env, tx| dbus_idle::start_mutter(env, tx, IDLE_AFTER)),
        Box::new(|env, tx| dbus_idle::start_screensaver(env, tx, IDLE_AFTER)),
        Box::new(|env, tx| x11::start_idle(env, tx, IDLE_AFTER)),
    ];
    let mut sup = Supervisor::new(sinks, Box::new(session::discover), focus, idle, Instant::now());
    let initial = sup.tick(Instant::now()).unwrap_or(Backends::NONE);
    log(&initial);
    thread::spawn(move || loop {
        thread::sleep(WATCH);
        if let Some(b) = sup.tick(Instant::now()) {
            log(&b);
            if events.send(Msg::Backends(b)).is_err() {
                return;
            }
        }
    });
    initial
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::{self, Receiver};
    use std::sync::{Arc, Mutex};

    use anyhow::anyhow;

    use super::super::backend::{running, Lifeline};
    use super::*;

    struct Fixture {
        sup: Supervisor,
        foreground: Receiver<Option<Foreground>>,
        session: Arc<Mutex<bool>>,
        lives: Arc<Mutex<Vec<Lifeline>>>,
        focus_starts: Arc<AtomicUsize>,
    }

    fn fixture(wm_up: Arc<Mutex<bool>>, now: Instant) -> Fixture {
        let (ftx, foreground) = mpsc::channel();
        let (etx, _events) = mpsc::channel();
        let session = Arc::new(Mutex::new(true));
        let lives = Arc::new(Mutex::new(Vec::new()));
        let focus_starts = Arc::new(AtomicUsize::new(0));
        let focus: Vec<FocusStart> = vec![
            Box::new(|_, _| Err(anyhow!("not this one"))),
            Box::new({
                let lives = lives.clone();
                let starts = focus_starts.clone();
                move |_, _| {
                    if !*wm_up.lock().unwrap() {
                        return Err(anyhow!("down"));
                    }
                    starts.fetch_add(1, Ordering::Relaxed);
                    let (run, life) = running("wm");
                    lives.lock().unwrap().push(life);
                    Ok(run)
                }
            }),
        ];
        let idle: Vec<IdleStart> = vec![Box::new(|_, _| Err(anyhow!("no idle source")))];
        let up = session.clone();
        let discover = Box::new(move || (*up.lock().unwrap()).then(SessionEnv::default));
        let sup = Supervisor::new(Sinks { foreground: ftx, events: etx }, discover, focus, idle, now);
        Fixture { sup, foreground, session, lives, focus_starts }
    }

    fn secs(t: Instant, n: u64) -> Instant {
        t + Duration::from_secs(n)
    }

    #[test]
    fn picks_the_first_candidate_that_starts_and_reports_the_names_once() {
        let t = Instant::now();
        let mut f = fixture(Arc::new(Mutex::new(true)), t);
        assert_eq!(f.sup.tick(t), Some(Backends { focus: "wm", idle: "none" }));
        assert_eq!(f.sup.tick(secs(t, 2)), None);
    }

    #[test]
    fn without_a_session_nothing_is_started_and_nothing_is_reported() {
        let t = Instant::now();
        let mut f = fixture(Arc::new(Mutex::new(true)), t);
        *f.session.lock().unwrap() = false;
        assert_eq!(f.sup.tick(t), None);
        assert_eq!(f.focus_starts.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn a_backend_that_is_missing_is_probed_again_after_30_seconds() {
        let t = Instant::now();
        let wm_up = Arc::new(Mutex::new(false));
        let mut f = fixture(wm_up.clone(), t);
        assert_eq!(f.sup.tick(t), None);
        *wm_up.lock().unwrap() = true;
        assert_eq!(f.sup.tick(secs(t, 29)), None);
        assert_eq!(f.sup.tick(secs(t, 30)), Some(Backends { focus: "wm", idle: "none" }));
    }

    #[test]
    fn a_stable_backend_that_dies_is_replaced_at_once_and_clears_the_focus() {
        let t = Instant::now();
        let mut f = fixture(Arc::new(Mutex::new(true)), t);
        f.sup.tick(t);
        f.lives.lock().unwrap().clear();
        assert_eq!(f.sup.tick(secs(t, 60)), None);
        assert_eq!(f.focus_starts.load(Ordering::Relaxed), 2);
        assert_eq!(f.foreground.try_recv().unwrap(), None);
    }

    #[test]
    fn a_dead_backend_is_reported_lost_when_it_cannot_be_replaced() {
        let t = Instant::now();
        let wm_up = Arc::new(Mutex::new(true));
        let mut f = fixture(wm_up.clone(), t);
        f.sup.tick(t);
        *wm_up.lock().unwrap() = false;
        f.lives.lock().unwrap().clear();
        assert_eq!(f.sup.tick(secs(t, 60)), Some(Backends::NONE));
    }

    #[test]
    fn a_backend_that_dies_right_after_starting_waits_for_the_next_probe() {
        let t = Instant::now();
        let mut f = fixture(Arc::new(Mutex::new(true)), t);
        f.sup.tick(t);
        f.lives.lock().unwrap().clear();
        assert_eq!(f.sup.tick(secs(t, 2)), Some(Backends::NONE));
        assert_eq!(f.focus_starts.load(Ordering::Relaxed), 1);
        assert_eq!(f.sup.tick(secs(t, 30)), Some(Backends { focus: "wm", idle: "none" }));
    }

    #[test]
    fn a_lost_idle_backend_announces_the_loss_of_presence() {
        let t = Instant::now();
        let (ftx, _foreground) = mpsc::channel();
        let (etx, events) = mpsc::channel();
        let lives = Arc::new(Mutex::new(Vec::new()));
        let idle: Vec<IdleStart> = vec![Box::new({
            let lives = lives.clone();
            move |_, _| {
                let (run, life) = running("idle");
                lives.lock().unwrap().push(life);
                Ok(run)
            }
        })];
        let mut sup = Supervisor::new(Sinks { foreground: ftx, events: etx }, Box::new(|| Some(SessionEnv::default())), Vec::new(), idle, t);
        assert_eq!(sup.tick(t), Some(Backends { focus: "none", idle: "idle" }));
        lives.lock().unwrap().clear();
        sup.tick(secs(t, 60));
        assert!(matches!(events.try_recv(), Ok(Msg::PresenceLost)));
    }
}
