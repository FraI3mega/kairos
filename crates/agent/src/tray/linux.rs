use std::sync::Arc;
use std::thread;

use anyhow::{anyhow, bail, Result};
use ksni::blocking::TrayMethods;
use ksni::menu::StandardItem;
use ksni::{MenuItem, OfflineReason, ToolTip};

use super::icon::{self, Image};
use super::menu::Item;
use super::{Controller, View, REFRESH};
use crate::diag::diag;

const SIZES: [u32; 2] = [32, 64];

struct Tray {
    ctrl: Arc<Controller>,
    view: View,
}

fn argb(img: Image) -> ksni::Icon {
    let mut data = img.rgba;
    for px in data.chunks_exact_mut(4) {
        px.rotate_right(1);
    }
    ksni::Icon { width: img.size as i32, height: img.size as i32, data }
}

/// Labels treat `_` as an access key marker; `__` shows one.
fn label(text: &str) -> String {
    text.replace('_', "__")
}

impl ksni::Tray for Tray {
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "kairos".into()
    }

    fn title(&self) -> String {
        "Kairos".into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        SIZES.iter().map(|s| argb(icon::on_field(self.view.paused, *s))).collect()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip { title: self.view.tooltip.clone(), ..ToolTip::default() }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        self.view
            .items
            .iter()
            .map(|item| match item {
                Item::Separator => MenuItem::Separator,
                Item::Label(text) => StandardItem { label: label(text), enabled: false, ..StandardItem::default() }.into(),
                Item::Entry(action, text) => {
                    let action = *action;
                    StandardItem {
                        label: label(text),
                        activate: Box::new(move |t: &mut Tray| t.ctrl.act(action)),
                        ..StandardItem::default()
                    }
                    .into()
                }
            })
            .collect()
    }

    fn menu_about_to_show(&mut self) {
        self.view = self.ctrl.view();
    }

    fn watcher_online(&self) {
        diag!("kairosd: tray is showing");
    }

    fn watcher_offline(&self, reason: OfflineReason) -> bool {
        diag!("kairosd: no StatusNotifierWatcher right now ({reason:?}), the tray appears when one starts");
        true
    }
}

pub fn run(ctrl: Arc<Controller>) -> Result<()> {
    let view = ctrl.view();
    // Soft-fail a missing watcher so a tray that starts before the panel still shows up later;
    // only a missing session bus is a hard error.
    let tray = Tray { ctrl: ctrl.clone(), view: view.clone() };
    let handle = tray.assume_sni_available(true).spawn().map_err(|e| anyhow!("{e}"))?;
    let mut shown = view;
    loop {
        thread::sleep(REFRESH);
        if handle.is_closed() {
            bail!("the tray service stopped");
        }
        let next = ctrl.view();
        if next != shown {
            let view = next.clone();
            handle.update(move |t| t.view = view);
            shown = next;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argb_moves_alpha_first() {
        let img = Image { size: 1, rgba: vec![1, 2, 3, 4] };
        let icon = argb(img);
        assert_eq!((icon.width, icon.height), (1, 1));
        assert_eq!(icon.data, [4, 1, 2, 3]);
    }

    #[test]
    fn underscores_are_escaped() {
        assert_eq!(label("a_b"), "a__b");
    }
}
