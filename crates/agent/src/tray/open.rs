use crate::diag::diag;

pub fn url(url: &str) {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return diag!("kairosd: not opening {url:?}, only http(s) links are opened");
    }
    launch(url);
}

#[cfg(windows)]
fn launch(url: &str) {
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let wide = |s: &str| std::ffi::OsStr::new(s).encode_wide().chain(Some(0)).collect::<Vec<u16>>();
    let (verb, target) = (wide("open"), wide(url));
    // SAFETY: both strings are NUL-terminated and outlive the call.
    let result = unsafe { ShellExecuteW(null_mut(), verb.as_ptr(), target.as_ptr(), null(), null(), SW_SHOWNORMAL) };
    // Values up to 32 are error codes.
    if (result as usize) <= 32 {
        diag!("kairosd: could not open the browser (ShellExecute returned {})", result as usize);
    }
}

#[cfg(not(windows))]
fn launch(url: &str) {
    use std::process::{Command, Stdio};

    let program = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    let child = Command::new(program).arg(url).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
    match child {
        // Waiting keeps the finished opener from lingering as a zombie.
        Ok(mut child) => {
            std::thread::spawn(move || match child.wait() {
                Ok(s) if !s.success() => diag!("kairosd: {program} exited with {s}"),
                Ok(_) => {}
                Err(e) => diag!("kairosd: waiting for {program}: {e}"),
            });
        }
        Err(e) => diag!("kairosd: could not run {program}: {e}"),
    }
}
