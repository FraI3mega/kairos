use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use anyhow::Result;

use super::backend::{running, Running};
use crate::platform::InputWatch;
use crate::{now, Msg};

pub const IDLE_AFTER: Duration = Duration::from_secs(60);

struct IdleState {
    threshold: Duration,
    idle: bool,
}

impl IdleState {
    fn observe(&mut self, idle_for: Duration) -> Option<Msg> {
        let now_idle = idle_for >= self.threshold;
        if now_idle == self.idle {
            return None;
        }
        self.idle = now_idle;
        Some(if now_idle { Msg::Idled } else { Msg::Resumed })
    }
}

pub fn poll(
    name: &'static str,
    threshold: Duration,
    tx: Sender<Msg>,
    mut idle_time: impl FnMut() -> Result<Duration> + Send + 'static,
) -> Result<Running> {
    let mut state = IdleState { threshold, idle: false };
    let mut input = InputWatch::default();
    let first = idle_time()?;
    let at = now();
    let (handle, life) = running(name);
    if tx.send(Msg::PresenceReady).is_err()
        || tx.send(Msg::InputReady).is_err()
        || input.observe(at, first.as_secs_f64()).is_some_and(|t| tx.send(Msg::Input(t)).is_err())
        || state.observe(first).is_some_and(|m| tx.send(m).is_err())
    {
        return Ok(handle);
    }
    thread::spawn(move || {
        let _life = life;
        loop {
            thread::sleep(InputWatch::poll_every(state.idle));
            let Ok(idle_for) = idle_time() else { return };
            let at = now();
            if input.observe(at, idle_for.as_secs_f64()).is_some_and(|t| tx.send(Msg::Input(t)).is_err()) {
                return;
            }
            if state.observe(idle_for).is_some_and(|m| tx.send(m).is_err()) {
                return;
            }
        }
    });
    Ok(handle)
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn next_state(rx: &mpsc::Receiver<Msg>) -> Msg {
        loop {
            match rx.recv_timeout(secs(5)).unwrap() {
                Msg::Input(_) => {}
                m => return m,
            }
        }
    }

    #[test]
    fn idles_at_the_threshold_and_resumes_on_input() {
        let mut s = IdleState { threshold: secs(60), idle: false };
        assert!(s.observe(secs(0)).is_none());
        assert!(s.observe(secs(59)).is_none());
        assert!(matches!(s.observe(secs(60)), Some(Msg::Idled)));
        assert!(s.observe(secs(300)).is_none());
        assert!(matches!(s.observe(secs(0)), Some(Msg::Resumed)));
        assert!(s.observe(secs(1)).is_none());
    }

    #[test]
    fn reports_the_time_of_each_new_input_from_the_idle_readings() {
        let (tx, rx) = mpsc::channel();
        let mut readings = vec![Ok(secs(0)), Ok(secs(1)), Ok(secs(0))];
        readings.reverse();
        poll("test", secs(60), tx, move || readings.pop().unwrap_or_else(|| Err(anyhow::anyhow!("gone")))).unwrap();
        let before = crate::now();
        let inputs: Vec<f64> = rx.iter().filter_map(|m| if let Msg::Input(t) = m { Some(t) } else { None }).collect();
        assert_eq!(inputs.len(), 2, "{inputs:?}");
        let two_polls = 2.0 * InputWatch::poll_every(false).as_secs_f64();
        assert!(inputs[0] <= before && inputs[1] > inputs[0] + two_polls - 0.05, "{inputs:?} {before}");
    }

    #[test]
    fn an_unreadable_source_is_an_error_not_presence() {
        let (tx, rx) = mpsc::channel();
        assert!(poll("test", secs(60), tx, || Err(anyhow::anyhow!("no"))).is_err());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn announces_presence_then_follows_the_idle_time_until_the_source_fails() {
        let (tx, rx) = mpsc::channel();
        let mut readings = vec![Ok(secs(0)), Ok(secs(5)), Ok(secs(0))];
        readings.reverse();
        let run = poll("test", secs(2), tx, move || readings.pop().unwrap_or_else(|| Err(anyhow::anyhow!("gone")))).unwrap();
        assert!(matches!(rx.recv().unwrap(), Msg::PresenceReady));
        assert!(matches!(rx.recv().unwrap(), Msg::InputReady));
        assert!(matches!(next_state(&rx), Msg::Idled));
        assert!(matches!(next_state(&rx), Msg::Resumed));
        while run.is_alive() {
            thread::sleep(Duration::from_millis(50));
        }
    }
}
