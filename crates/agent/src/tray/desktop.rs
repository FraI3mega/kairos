use std::sync::Arc;

use anyhow::Result;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};

use super::icon;
use super::menu::{Action, Item};
use super::{Controller, View};
use crate::diag::diag;

struct Tray {
    icon: TrayIcon,
    shown: View,
}

/// `&` marks a mnemonic in menu text; `&&` shows one.
fn escape(text: &str) -> String {
    text.replace('&', "&&")
}

fn build_menu(items: &[Item]) -> Result<Menu> {
    let menu = Menu::new();
    for item in items {
        match item {
            Item::Label(text) => menu.append(&MenuItem::new(escape(text), false, None))?,
            Item::Separator => menu.append(&PredefinedMenuItem::separator())?,
            Item::Entry(action, text) => menu.append(&MenuItem::with_id(action.id(), escape(text), true, None))?,
        }
    }
    Ok(menu)
}

#[cfg(windows)]
fn image(paused: bool) -> Result<Icon> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSMICON};

    // SAFETY: plain query with no pointers.
    let size = match unsafe { GetSystemMetrics(SM_CXSMICON) } {
        s if s > 0 => s as u32,
        _ => 32,
    };
    let img = icon::on_field(paused, size);
    Ok(Icon::from_rgba(img.rgba, img.size, img.size)?)
}

#[cfg(target_os = "macos")]
fn image(paused: bool) -> Result<Icon> {
    let img = icon::template(paused, 64);
    Ok(Icon::from_rgba(img.rgba, img.size, img.size)?)
}

impl Tray {
    fn new(view: View) -> Result<Tray> {
        let builder = TrayIconBuilder::new().with_menu(Box::new(build_menu(&view.items)?)).with_tooltip(&view.tooltip);
        #[cfg(windows)]
        let builder = builder.with_icon(image(view.paused)?);
        #[cfg(target_os = "macos")]
        let builder = builder.with_icon_templated(image(view.paused)?);
        Ok(Tray { icon: builder.build()?, shown: view })
    }

    fn refresh(&mut self, ctrl: &Controller) {
        let view = ctrl.view();
        if view == self.shown {
            return;
        }
        if let Err(e) = self.apply(&view) {
            diag!("kairosd: tray update failed: {e:#}");
        }
        self.shown = view;
    }

    fn apply(&self, view: &View) -> Result<()> {
        if view.items != self.shown.items {
            self.icon.set_menu(Some(Box::new(build_menu(&view.items)?)));
        }
        if view.tooltip != self.shown.tooltip {
            self.icon.set_tooltip(Some(&view.tooltip))?;
        }
        if view.paused != self.shown.paused {
            #[cfg(windows)]
            self.icon.set_icon(Some(image(view.paused)?))?;
            #[cfg(target_os = "macos")]
            self.icon.set_icon_templated(Some(image(view.paused)?))?;
        }
        Ok(())
    }
}

fn route_events(ctrl: &Arc<Controller>) {
    let ctrl = ctrl.clone();
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| match Action::from_id(e.id.as_ref()) {
        Some(action) => ctrl.act(action),
        None => diag!("kairosd: unknown tray menu id {:?}", e.id.as_ref()),
    }));
    // Without a handler these pile up in a channel nobody reads.
    TrayIconEvent::set_event_handler(Some(|_| {}));
}

#[cfg(windows)]
pub fn run(ctrl: Arc<Controller>) -> Result<()> {
    use std::ptr::null_mut;
    use windows_sys::Win32::UI::WindowsAndMessaging::{DispatchMessageW, GetMessageW, SetTimer, TranslateMessage, MSG, WM_TIMER};

    route_events(&ctrl);
    let mut tray = Tray::new(ctrl.view())?;
    let interval = super::REFRESH.as_millis() as u32;
    // SAFETY: a thread timer with no window and no callback; its ticks arrive in this loop.
    let timer = unsafe { SetTimer(null_mut(), 0, interval, None) };
    if timer == 0 {
        diag!("kairosd: tray refresh timer failed, the menu will not update");
    }
    let mut msg = MSG::default();
    loop {
        // SAFETY: `msg` is a valid MSG for the calls below; null window means this thread's queue.
        let got = unsafe { GetMessageW(&mut msg, null_mut(), 0, 0) };
        if got == 0 {
            return Ok(());
        }
        if got < 0 {
            anyhow::bail!("GetMessageW failed");
        }
        if msg.message == WM_TIMER && msg.hwnd.is_null() && msg.wParam == timer {
            tray.refresh(&ctrl);
            continue;
        }
        // SAFETY: `msg` came from GetMessageW.
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

#[cfg(target_os = "macos")]
pub fn run(ctrl: Arc<Controller>) -> Result<()> {
    use std::cell::RefCell;
    use std::ptr::NonNull;

    use anyhow::anyhow;
    use block2::RcBlock;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    use objc2_foundation::NSTimer;

    let mtm = MainThreadMarker::new().ok_or_else(|| anyhow!("the tray needs the main thread"))?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    route_events(&ctrl);

    // The icon is created from inside the running loop (tray-icon asks for that).
    let tray: RefCell<Option<Tray>> = RefCell::new(None);
    let gave_up = std::cell::Cell::new(false);
    let tick = RcBlock::new(move |_: NonNull<NSTimer>| {
        if gave_up.get() {
            return;
        }
        let mut slot = tray.borrow_mut();
        match slot.as_mut() {
            Some(t) => t.refresh(&ctrl),
            None => match Tray::new(ctrl.view()) {
                Ok(t) => *slot = Some(t),
                Err(e) => {
                    diag!("kairosd: tray unavailable: {e:#}");
                    gave_up.set(true);
                }
            },
        }
    });
    // SAFETY: the block only runs on the main thread's run loop, where everything it touches lives.
    unsafe {
        NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.05, false, &tick);
        NSTimer::scheduledTimerWithTimeInterval_repeats_block(super::REFRESH.as_secs_f64(), true, &tick);
    }
    app.run();
    Ok(())
}
