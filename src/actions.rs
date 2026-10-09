//! Tray actions shared by every backend.
#![cfg_attr(not(windows), allow(dead_code))] // used by the macOS/Linux trays (later tasks)

use crate::update::Release;
use std::path::Path;

pub fn open_config() {
    match crate::config::ensure_config_file() {
        Ok(p) => open_path(&p),
        Err(e) => eprintln!("cannot create the config file: {e}"),
    }
}

/// Open a local file with the OS default handler (never a URL).
pub fn open_path(path: &Path) {
    use std::process::Command;
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        use windows::core::{PCWSTR, w};
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let r = unsafe {
            ShellExecuteW(None, w!("open"), PCWSTR(wide.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL)
        };
        if r.0 as isize <= 32 {
            // No association (e.g. for .toml): fall back to Notepad.
            let _ = Command::new("notepad.exe").arg(path).spawn();
        }
    }
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = Command::new("open");
        c.arg(path);
        c
    };
    #[cfg(not(any(windows, target_os = "macos")))]
    let mut cmd = {
        let mut c = Command::new("xdg-open");
        c.arg(path);
        c
    };
    #[cfg(not(windows))]
    let _ = cmd.spawn();
}

pub fn check_updates() -> Result<Option<Release>, String> {
    crate::update::check_now()
}

/// Flip "Start at login"; returns the new state.
pub fn toggle_autostart() -> Result<bool, String> {
    let on = !crate::autostart::is_enabled();
    crate::autostart::set(on)?;
    Ok(on)
}
