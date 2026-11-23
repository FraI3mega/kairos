use std::path::Path;

use objc2_core_foundation::{CFBundle, CFDictionary, CFNumber, CFRetained, CFString, CFType, CFURL};
use objc2_core_graphics::{
    kCGNullWindowID, kCGWindowAlpha, kCGWindowLayer, kCGWindowOwnerPID, CGWindowListCopyWindowInfo, CGWindowListOption,
};

/// Owner pids need no Screen Recording permission (only window names do, and they are never asked for).
pub fn front_pid() -> Option<u32> {
    let options = CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements;
    let list = CGWindowListCopyWindowInfo(options, kCGNullWindowID)?;
    // SAFETY: CGWindowListCopyWindowInfo returns an array of window dictionaries keyed by CFString.
    let windows = unsafe { list.cast_unchecked::<CFDictionary<CFString, CFType>>() };
    // SAFETY: immutable CFString constants exported by CoreGraphics.
    let (layer, alpha, owner) = unsafe { (kCGWindowLayer, kCGWindowAlpha, kCGWindowOwnerPID) };
    windows.iter().find_map(|w| {
        let number = |key: &CFString| w.get(key).and_then(|v| v.downcast::<CFNumber>().ok());
        let visible = number(alpha).and_then(|a| a.as_f64()).is_none_or(|a| a > 0.0);
        if number(layer)?.as_i64()? != 0 || !visible {
            return None;
        }
        number(owner)?.as_i64().and_then(|pid| u32::try_from(pid).ok())
    })
}

pub fn bundle_id(exe: &Path) -> Option<String> {
    let app = exe.ancestors().find(|p| p.extension().is_some_and(|e| e == "app"))?;
    let url = CFURL::from_directory_path(app)?;
    let bundle: CFRetained<CFBundle> = CFBundle::new(None, Some(&url))?;
    bundle.identifier().map(|id| id.to_string())
}
