use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use kairos_core::event::{Class, Event, Kind};

use crate::browser::BrowserState;
use crate::classify::runs_children;
use crate::exclusions::Excluded;
use crate::platform::{self, Foreground};
use crate::watch::Watch;
use crate::{now, Msg};

const RECHECK: Duration = Duration::from_secs(15);
// An agent or a dev server started in the terminal or editor in front changes its class without changing the window.
const RECHECK_HOST: Duration = Duration::from_secs(3);

pub fn spawn(reports: Receiver<Option<Foreground>>, events: Sender<Msg>, watch: Sender<Watch>, browser: Arc<BrowserState>, excluded: Arc<Excluded>) {
    thread::spawn(move || run(&reports, &events, &watch, &browser, &excluded));
}

fn run(reports: &Receiver<Option<Foreground>>, events: &Sender<Msg>, watch: &Sender<Watch>, browser: &BrowserState, excluded: &Excluded) {
    let mut current: Option<Foreground> = None;
    let mut last: Option<(Class, Option<String>)> = None;
    let mut recheck = RECHECK;
    loop {
        match reports.recv_timeout(recheck) {
            Ok(report) => {
                current = report;
                while let Ok(newer) = reports.try_recv() {
                    current = newer;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        recheck = RECHECK;
        let state = match &current {
            None => (Class::Other, None),
            Some(fg) => {
                let c = platform::classify(fg, &excluded.current());
                if runs_children(&c.app, &fg.wclass) {
                    recheck = RECHECK_HOST;
                }
                if let Some(p) = &c.project {
                    if watch.send(Watch::Root(p.root.clone(), p.id.clone())).is_err() {
                        return;
                    }
                }
                match (c.class, browser.current(&c.app)) {
                    (Class::BrowserUnknown, Some(tab)) => (tab.class, tab.project),
                    (class, _) => (class, c.project.map(|p| p.id)),
                }
            }
        };
        if last.as_ref() != Some(&state) {
            let e = Event::new(now(), state.1.clone(), Kind::Focus { class: state.0 });
            if events.send(Msg::Event(e)).is_err() {
                return;
            }
            last = Some(state);
        }
    }
}
