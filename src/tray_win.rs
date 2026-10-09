//! Windows notification-area icon. It lives on the message-only `rustshot_tray`
//! window owned by `instance.rs`: that window's procedure calls [`handle`], and
//! its thread calls [`remove`] on shutdown.
//!
//! Uses the classic (pre-`NOTIFYICON_VERSION_4`) callback semantics: `lParam` of
//! the callback message `WM_APP + 1` is the raw mouse message
//! (`WM_LBUTTONUP`, `WM_RBUTTONUP`, ...).

use crate::hotkey::HotEvent;
use crate::tray::{self, MenuItem};
use std::sync::mpsc::Sender;
use std::sync::{Mutex, OnceLock};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

/// Notification callback message.
const WM_TRAYICON: u32 = WM_APP + 1;
/// Posted by [`spawn`]: add the icon (runs on the window thread).
const WM_TRAY_INIT: u32 = WM_APP + 2;
/// Posted by the update-check worker: show the stored message as a balloon.
const WM_TRAY_BALLOON: u32 = WM_APP + 3;
const ICON_ID: u32 = 1;

static TX: Mutex<Option<Sender<HotEvent>>> = Mutex::new(None);
static BALLOON: Mutex<Option<String>> = Mutex::new(None);

fn taskbar_created() -> u32 {
    static MSG: OnceLock<u32> = OnceLock::new();
    *MSG.get_or_init(|| unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) })
}

fn send(ev: HotEvent) {
    if let Some(tx) = TX.lock().unwrap().as_ref() {
        let _ = tx.send(ev);
    }
}

/// Start the tray: remember the event sender and ask the tray window to add the icon.
pub fn spawn(tx: Sender<HotEvent>) {
    *TX.lock().unwrap() = Some(tx);
    unsafe {
        if let Ok(hwnd) = FindWindowExW(Some(HWND_MESSAGE), None, w!("rustshot_tray"), PCWSTR::null()) {
            let _ = PostMessageW(Some(hwnd), WM_TRAY_INIT, WPARAM(0), LPARAM(0));
        }
    }
}

fn copy_wide<const N: usize>(dst: &mut [u16; N], s: &str) {
    let mut i = 0;
    for c in s.encode_utf16().take(N - 1) {
        dst[i] = c;
        i += 1;
    }
    dst[i] = 0;
}

fn base_data(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: ICON_ID,
        ..Default::default()
    }
}

unsafe fn load_icon() -> HICON {
    unsafe {
        let hinst = GetModuleHandleW(PCWSTR::null()).unwrap_or_default();
        // Resource ID 1 is the app icon embedded by build.rs.
        let img = LoadImageW(
            Some(hinst.into()),
            PCWSTR(std::ptr::without_provenance(1)),
            IMAGE_ICON,
            GetSystemMetrics(SM_CXSMICON),
            GetSystemMetrics(SM_CYSMICON),
            LR_DEFAULTCOLOR,
        );
        match img {
            Ok(h) if !h.is_invalid() => HICON(h.0),
            _ => LoadIconW(None, IDI_APPLICATION).unwrap_or_default(),
        }
    }
}

fn add_icon(hwnd: HWND) {
    unsafe {
        let mut nid = base_data(hwnd);
        nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        nid.uCallbackMessage = WM_TRAYICON;
        nid.hIcon = load_icon();
        copy_wide(&mut nid.szTip, "rustshot");
        if !Shell_NotifyIconW(NIM_ADD, &nid).as_bool() {
            eprintln!("rustshot: could not add the notification-area icon");
        }
    }
}

/// Remove the icon (called from the window thread before the window is destroyed).
pub fn remove(hwnd: HWND) {
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &base_data(hwnd));
    }
}

fn balloon(hwnd: HWND, text: &str) {
    unsafe {
        let mut nid = base_data(hwnd);
        nid.uFlags = NIF_INFO;
        nid.dwInfoFlags = NIIF_INFO;
        copy_wide(&mut nid.szInfoTitle, "rustshot");
        copy_wide(&mut nid.szInfo, text);
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

fn show_menu(hwnd: HWND) {
    unsafe {
        let items = tray::current_menu();
        let Ok(menu) = CreatePopupMenu() else { return };
        for (i, item) in items.iter().enumerate() {
            if *item == MenuItem::Quit {
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            }
            let mut flags = MF_STRING;
            if matches!(item, MenuItem::Autostart(true)) {
                flags |= MF_CHECKED;
            }
            let label: Vec<u16> = item.label().encode_utf16().chain(Some(0)).collect();
            let _ = AppendMenuW(menu, flags, i + 1, PCWSTR(label.as_ptr()));
        }
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // Required so the menu dismisses when the user clicks elsewhere.
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, Some(0), hwnd, None);
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        if cmd.0 > 0
            && let Some(item) = items.get(cmd.0 as usize - 1)
        {
            run(hwnd, *item);
        }
    }
}

fn run(hwnd: HWND, item: MenuItem) {
    match item {
        MenuItem::Capture => send(HotEvent::Capture),
        MenuItem::Quit => send(HotEvent::Quit),
        MenuItem::OpenConfig => {
            std::thread::spawn(crate::actions::open_config);
        }
        MenuItem::Autostart(_) => {
            if let Err(e) = crate::actions::toggle_autostart() {
                *BALLOON.lock().unwrap() = Some(format!("Could not change Start at login: {e}"));
                unsafe {
                    let _ = PostMessageW(Some(hwnd), WM_TRAY_BALLOON, WPARAM(0), LPARAM(0));
                }
            }
        }
        MenuItem::CheckUpdates => {
            let h = hwnd.0 as isize;
            std::thread::spawn(move || {
                let r = crate::actions::check_updates();
                if let Ok(Some(rel)) = &r {
                    crate::update::open_url(&rel.url);
                }
                *BALLOON.lock().unwrap() = Some(tray::update_message(&r));
                unsafe {
                    let _ = PostMessageW(Some(HWND(h as *mut _)), WM_TRAY_BALLOON, WPARAM(0), LPARAM(0));
                }
            });
        }
    }
}

/// Window-procedure hook; `None` = not a tray message.
pub fn handle(hwnd: HWND, m: u32, _w: WPARAM, l: LPARAM) -> Option<LRESULT> {
    match m {
        WM_TRAY_INIT => add_icon(hwnd),
        WM_TRAY_BALLOON => {
            if let Some(text) = BALLOON.lock().unwrap().take() {
                balloon(hwnd, &text);
            }
        }
        WM_TRAYICON => match l.0 as u32 {
            WM_LBUTTONUP => send(HotEvent::Capture),
            WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(hwnd),
            _ => {}
        },
        // Explorer restarted: the notification area was recreated.
        _ if m == taskbar_created() && TX.lock().unwrap().is_some() => add_icon(hwnd),
        _ => return None,
    }
    Some(LRESULT(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instance::{Instance, acquire_or_signal};

    fn icon_exists() -> bool {
        unsafe {
            let hwnd = FindWindowExW(Some(HWND_MESSAGE), None, w!("rustshot_tray"), PCWSTR::null()).unwrap();
            let mut nid = base_data(hwnd);
            nid.uFlags = NIF_TIP;
            copy_wide(&mut nid.szTip, "rustshot");
            // NIM_MODIFY only succeeds for an icon that exists.
            Shell_NotifyIconW(NIM_MODIFY, &nid).as_bool()
        }
    }

    #[test]
    fn copy_wide_truncates_and_terminates() {
        let mut b = [1u16; 4];
        copy_wide(&mut b, "abcdef");
        assert_eq!(b, [97, 98, 99, 0]);
    }

    #[test]
    #[ignore = "creates the real notification-area icon; fails if a daemon is running"]
    fn live_icon_added_and_removed() {
        let Instance::Primary(g) = acquire_or_signal() else { panic!("a daemon is already running") };
        let (tx, _rx) = std::sync::mpsc::channel();
        g.listen(tx.clone());
        spawn(tx);
        std::thread::sleep(std::time::Duration::from_millis(1500));
        assert!(icon_exists(), "icon should be present after spawn");
        drop(g);
        // The window is gone after the guard drops, which also removed the icon.
        assert!(
            unsafe { FindWindowExW(Some(HWND_MESSAGE), None, w!("rustshot_tray"), PCWSTR::null()) }.is_err()
        );
    }
}
