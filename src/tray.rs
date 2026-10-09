//! Tray menu model shared by the per-OS backends.
#![cfg_attr(not(windows), allow(dead_code))] // used by the macOS/Linux trays (later tasks)

use crate::hotkey::HotEvent;
use std::sync::mpsc::Sender;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuItem {
    Capture,
    OpenConfig,
    CheckUpdates,
    /// Start at login; payload = currently enabled.
    Autostart(bool),
    Quit,
}

impl MenuItem {
    pub fn label(self) -> &'static str {
        match self {
            MenuItem::Capture => "Capture",
            MenuItem::OpenConfig => "Open config file",
            MenuItem::CheckUpdates => "Check for updates",
            MenuItem::Autostart(_) => "Start at login",
            MenuItem::Quit => "Quit rustshot",
        }
    }
}

/// Menu contents; `autostart` = None hides "Start at login" (managed installs).
pub fn menu(autostart: Option<bool>) -> Vec<MenuItem> {
    let mut v = vec![MenuItem::Capture, MenuItem::OpenConfig, MenuItem::CheckUpdates];
    if let Some(on) = autostart {
        v.push(MenuItem::Autostart(on));
    }
    v.push(MenuItem::Quit);
    v
}

/// Current menu for this install.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn current_menu() -> Vec<MenuItem> {
    let auto = crate::update::managed_install().is_none().then(crate::autostart::is_enabled);
    menu(auto)
}

/// Text for the result of an update check.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn update_message(r: &Result<Option<crate::update::Release>, String>) -> String {
    match r {
        Ok(Some(rel)) => format!("rustshot {} is available. Opening the download page.", rel.version),
        Ok(None) => "rustshot is up to date.".to_string(),
        Err(e) => format!("Update check failed: {e}"),
    }
}

/// Start the tray for the running daemon. `tx` receives Capture/Quit.
pub fn spawn(tx: Sender<HotEvent>) {
    #[cfg(windows)]
    crate::tray_win::spawn(tx);
    #[cfg(not(windows))]
    let _ = tx; // macOS / Linux backends arrive in later tasks.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_composition() {
        assert_eq!(
            menu(Some(true)),
            vec![
                MenuItem::Capture,
                MenuItem::OpenConfig,
                MenuItem::CheckUpdates,
                MenuItem::Autostart(true),
                MenuItem::Quit
            ]
        );
        assert_eq!(menu(None).len(), 4);
        assert!(!menu(None).iter().any(|m| matches!(m, MenuItem::Autostart(_))));
        assert_eq!(*menu(None).last().unwrap(), MenuItem::Quit);
    }

    #[test]
    fn labels() {
        assert_eq!(MenuItem::Capture.label(), "Capture");
        assert_eq!(MenuItem::OpenConfig.label(), "Open config file");
        assert_eq!(MenuItem::CheckUpdates.label(), "Check for updates");
        assert_eq!(MenuItem::Autostart(false).label(), "Start at login");
        assert_eq!(MenuItem::Quit.label(), "Quit rustshot");
    }

    #[test]
    fn update_messages() {
        assert!(update_message(&Ok(None)).contains("up to date"));
        assert!(update_message(&Err("boom".into())).contains("boom"));
        let r = crate::update::Release { version: "9.9.9".into(), url: String::new() };
        assert!(update_message(&Ok(Some(r))).contains("9.9.9"));
    }
}
