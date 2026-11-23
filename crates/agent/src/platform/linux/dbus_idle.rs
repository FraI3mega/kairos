use std::sync::mpsc::Sender;
use std::time::Duration;

use anyhow::{anyhow, Result};
use zbus::blocking::Proxy;

use super::backend::Running;
use super::dbus;
use super::presence::poll;
use super::session::SessionEnv;
use crate::Msg;

pub const MUTTER: &str = "mutter";
pub const SCREENSAVER: &str = "screensaver-dbus";

pub fn start_mutter(env: &SessionEnv, tx: Sender<Msg>, threshold: Duration) -> Result<Running> {
    let conn = dbus::connect(env)?;
    let proxy = Proxy::new(&conn, "org.gnome.Mutter.IdleMonitor", "/org/gnome/Mutter/IdleMonitor/Core", "org.gnome.Mutter.IdleMonitor")?;
    poll(MUTTER, threshold, tx, move || {
        let ms: u64 = proxy.call("GetIdletime", &())?;
        Ok(Duration::from_millis(ms))
    })
}

pub fn start_screensaver(env: &SessionEnv, tx: Sender<Msg>, threshold: Duration) -> Result<Running> {
    let conn = dbus::connect(env)?;
    // KDE serves the interface on both paths; other desktops may serve it on one.
    let proxies = ["/org/freedesktop/ScreenSaver", "/ScreenSaver"]
        .into_iter()
        .map(|path| Proxy::new(&conn, "org.freedesktop.ScreenSaver", path, "org.freedesktop.ScreenSaver"))
        .collect::<zbus::Result<Vec<_>>>()?;
    let proxy = proxies
        .into_iter()
        .find(|p| p.call::<_, _, u32>("GetSessionIdleTime", &()).is_ok())
        .ok_or_else(|| anyhow!("org.freedesktop.ScreenSaver does not report idle time"))?;
    poll(SCREENSAVER, threshold, tx, move || {
        let ms: u32 = proxy.call("GetSessionIdleTime", &())?;
        Ok(Duration::from_millis(ms.into()))
    })
}
