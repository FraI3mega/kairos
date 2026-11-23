mod backend;
mod dbus;
mod dbus_idle;
#[cfg(test)]
mod desktop_tests;
mod gnome;
mod hyprland;
mod kwin;
mod niri;
mod presence;
mod procs;
mod session;
mod supervisor;
mod sway;
mod wayland_idle;
mod x11;

use std::path::PathBuf;

use kairos_core::exclude::Exclusions;

use crate::classify::{classify_with, Classified};
use crate::platform::{Backends, Foreground, Sinks};

pub fn process_cwd(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

pub fn classify(fg: &Foreground, ex: &Exclusions) -> Classified {
    classify_with(&procs::ProcFs::scan(), fg.pid, &fg.wclass, &fg.title, ex)
}

pub fn spawn(sinks: Sinks) -> Backends {
    supervisor::spawn(sinks)
}
