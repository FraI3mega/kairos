use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Result};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::ext::idle_notify::v1::client::ext_idle_notification_v1::{self, ExtIdleNotificationV1};
use wayland_protocols::ext::idle_notify::v1::client::ext_idle_notifier_v1::ExtIdleNotifierV1;

use super::backend::{running, Running};
use super::session::SessionEnv;
use crate::{now, Msg};

pub const NAME: &str = "ext-idle-notify";

// How long without input before the second notification fires, which is also how far `Idled` trails the last input.
const INPUT_GAP: Duration = Duration::from_secs(1);

#[derive(Clone, Copy)]
enum Watch {
    Presence,
    Input,
}

struct State {
    tx: Sender<Msg>,
    // Set from `Resumed` to `Idled` of the second notification: input in every second of that stretch.
    typing: Arc<AtomicBool>,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(_: &mut Self, _: &wl_seat::WlSeat, _: wl_seat::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ExtIdleNotifierV1, ()> for State {
    fn event(_: &mut Self, _: &ExtIdleNotifierV1, _: <ExtIdleNotifierV1 as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ExtIdleNotificationV1, Watch> for State {
    fn event(state: &mut Self, _: &ExtIdleNotificationV1, event: ext_idle_notification_v1::Event, watch: &Watch, _: &Connection, _: &QueueHandle<Self>) {
        let msg = match (watch, event) {
            (Watch::Presence, ext_idle_notification_v1::Event::Idled) => Some(Msg::Idled),
            (Watch::Presence, ext_idle_notification_v1::Event::Resumed) => Some(Msg::Resumed),
            (Watch::Input, ext_idle_notification_v1::Event::Resumed) => {
                state.typing.store(true, Ordering::Relaxed);
                Some(Msg::Input(now()))
            }
            // Nothing before the first `Resumed` was seen: a user who is idle when the daemon starts gets `Idled` straight away.
            (Watch::Input, ext_idle_notification_v1::Event::Idled) => {
                state.typing.swap(false, Ordering::Relaxed).then(|| Msg::Input(now() - INPUT_GAP.as_secs_f64()))
            }
            _ => None,
        };
        if let Some(m) = msg {
            let _ = state.tx.send(m);
        }
    }
}

// While input keeps coming there is no event, so each second of it is announced from here. Input happened some time in
// the last INPUT_GAP, so its middle is the best guess and is off by at most half a second.
fn announce_typing(typing: &Arc<AtomicBool>, tx: Sender<Msg>) {
    let typing = Arc::downgrade(typing);
    thread::spawn(move || loop {
        thread::sleep(INPUT_GAP);
        let Some(typing) = typing.upgrade() else { return };
        if typing.load(Ordering::Relaxed) && tx.send(Msg::Input(now() - INPUT_GAP.as_secs_f64() / 2.0)).is_err() {
            return;
        }
    });
}

pub fn start(env: &SessionEnv, tx: Sender<Msg>, threshold: Duration) -> Result<Running> {
    let socket = env.wayland_socket().ok_or_else(|| anyhow!("no Wayland display"))?;
    let conn = Connection::from_socket(UnixStream::connect(socket)?).map_err(|e| anyhow!("no wayland connection: {e}"))?;
    let (globals, mut queue) = registry_queue_init::<State>(&conn)?;
    let qh = queue.handle();
    let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=1, ()).map_err(|e| anyhow!("no wl_seat: {e}"))?;
    let notifier: ExtIdleNotifierV1 = globals
        .bind(&qh, 1..=2, ())
        .map_err(|e| anyhow!("ext-idle-notify-v1 not offered by the compositor: {e}"))?;
    let millis = u32::try_from(threshold.as_millis())?;
    let notify = |millis: u32, watch: Watch| {
        if notifier.version() >= 2 {
            notifier.get_input_idle_notification(millis, &seat, &qh, watch)
        } else {
            notifier.get_idle_notification(millis, &seat, &qh, watch)
        }
    };
    let notification = notify(millis, Watch::Presence);
    let input_notification = notify(u32::try_from(INPUT_GAP.as_millis())?, Watch::Input);
    // A user who is already idle gets `Idled` during the roundtrip, which must come after the presence
    // announcement.
    let (early_tx, early) = mpsc::channel();
    let mut state = State { tx: early_tx, typing: Arc::new(AtomicBool::new(false)) };
    queue.roundtrip(&mut state)?;
    let _ = tx.send(Msg::PresenceReady);
    let _ = tx.send(Msg::InputReady);
    early.try_iter().for_each(|m| {
        let _ = tx.send(m);
    });
    state.tx = tx.clone();
    announce_typing(&state.typing, tx);
    let (handle, life) = running(NAME);
    thread::spawn(move || {
        let _life = life;
        let _keep = (conn, seat, notifier, notification, input_notification);
        while queue.blocking_dispatch(&mut state).is_ok() {}
    });
    Ok(handle)
}
