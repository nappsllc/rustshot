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
    let mut cmd = {
        let mut c = Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler").arg(path);
        c
    };
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
