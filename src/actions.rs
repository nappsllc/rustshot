//! Tray actions shared by every backend.
#![cfg_attr(not(windows), allow(dead_code))] // used by the macOS/Linux trays (later tasks)

use crate::update_ui::DialogState;
use std::path::Path;

pub fn open_config() {
    match crate::config::ensure_config_file() {
        Ok(p) => open_path(&p),
        Err(e) => eprintln!("cannot create the config file: {e}"),
    }
}

/// Open a local file with the OS default handler (never a URL).
pub fn open_path(path: &Path) {
    #[cfg(not(windows))]
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
            if let Some(exe) = crate::proc_win::system_exe("notepad.exe") {
                let _ = crate::proc_win::spawn_detached(&exe, &[path.as_os_str()], &[]);
            }
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

/// Tray "Check for updates": the update dialog opens at once and shows
/// the result of the check (store installs: who updates them).
pub fn check_updates() {
    match crate::update::update_channel() {
        Some(store) => crate::update_ui::show(DialogState::Managed(store)),
        None => crate::update_ui::check(),
    }
}

/// Flip "Start at login"; returns the new state.
pub fn toggle_autostart() -> Result<bool, String> {
    let on = !crate::autostart::is_enabled();
    crate::autostart::set(on)?;
    Ok(on)
}
