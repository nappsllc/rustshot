//! Windows notification-area icon. It lives on the hidden top-level `rustshot_tray`
//! window (never shown; message-only windows get no `TaskbarCreated` broadcast and
//! cannot become foreground for menu dismissal) owned by `instance.rs`: that window's procedure calls [`handle`], and
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
/// Posted when an action failed: show the stored message as a balloon.
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
        if let Ok(hwnd) = FindWindowW(crate::instance::tray_class(), PCWSTR::null()) {
            let _ = PostMessageW(Some(hwnd), WM_TRAY_INIT, WPARAM(0), LPARAM(0));
        }
    }
}

/// Drop the event sender (called when the instance guard goes away).
pub fn clear() {
    *TX.lock().unwrap() = None;
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

/// Cached glyph HICON (as usize; 0 = none yet). Replaced on theme/DPI change.
static GLYPH: Mutex<usize> = Mutex::new(0);

/// True when the taskbar uses the light theme (missing value = dark).
fn taskbar_is_light() -> bool {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    let mut val: u32 = 0;
    let mut len = std::mem::size_of::<u32>() as u32;
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("SystemUsesLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut val).cast()),
            Some(&raw mut len),
        )
    };
    rc.is_ok() && val == 1
}

/// Render the glyph at the tray size for `hwnd`'s DPI into a new HICON.
fn build_glyph(hwnd: HWND) -> Option<HICON> {
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetDpiForWindow, GetSystemMetricsForDpi};
    unsafe {
        let mut dpi = GetDpiForWindow(hwnd);
        if dpi == 0 {
            dpi = GetDpiForSystem();
        }
        let size = GetSystemMetricsForDpi(SM_CXSMICON, dpi.max(96)).max(16) as u32;
        let rgb = if taskbar_is_light() { (0x1B, 0x1C, 0x20) } else { (255, 255, 255) };
        let rgba = tray::tray_glyph_rgba(size, rgb);
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size as i32,
                biHeight: -(size as i32), // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let color = CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        if bits.is_null() {
            let _ = DeleteObject(color.into());
            return None;
        }
        let dst = std::slice::from_raw_parts_mut(bits.cast::<u8>(), rgba.len());
        for (d, s) in dst.as_chunks_mut::<4>().0.iter_mut().zip(rgba.as_chunks::<4>().0) {
            *d = [s[2], s[1], s[0], s[3]]; // RGBA -> BGRA, straight alpha
        }
        let mask_bytes = vec![0u8; (size as usize).div_ceil(16) * 2 * size as usize];
        let mask = CreateBitmap(size as i32, size as i32, 1, 1, Some(mask_bytes.as_ptr().cast()));
        let info = ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&info).ok();
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        icon
    }
}

/// The app icon (resource 1) at the small-icon size: fallback if the glyph fails.
unsafe fn load_icon() -> HICON {
    unsafe {
        let hinst = GetModuleHandleW(PCWSTR::null()).unwrap_or_default();
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

/// Cached glyph icon, rendering it on first use.
fn icon(hwnd: HWND) -> HICON {
    let mut g = GLYPH.lock().unwrap();
    if *g == 0 {
        *g = match build_glyph(hwnd) {
            Some(h) => h.0 as usize,
            None => return unsafe { load_icon() },
        };
    }
    HICON(*g as *mut _)
}

/// Theme or DPI changed: re-render the glyph and swap it into the tray icon.
fn refresh_icon(hwnd: HWND) {
    if *GLYPH.lock().unwrap() == 0 {
        return; // not added yet
    }
    let Some(new) = build_glyph(hwnd) else { return };
    let old = std::mem::replace(&mut *GLYPH.lock().unwrap(), new.0 as usize);
    unsafe {
        let mut nid = base_data(hwnd);
        nid.uFlags = NIF_ICON;
        nid.hIcon = new;
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
        if old != 0 {
            let _ = DestroyIcon(HICON(old as *mut _));
        }
    }
}

fn add_icon(hwnd: HWND) {
    unsafe {
        let mut nid = base_data(hwnd);
        nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        nid.uCallbackMessage = WM_TRAYICON;
        nid.hIcon = icon(hwnd);
        copy_wide(&mut nid.szTip, "Rustshot");
        if !Shell_NotifyIconW(NIM_ADD, &nid).as_bool() {
            eprintln!("Rustshot: could not add the notification-area icon");
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
        copy_wide(&mut nid.szInfoTitle, "Rustshot");
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
        MenuItem::Settings => crate::settings_ui::show(),
        MenuItem::Autostart(_) => {
            if let Err(e) = crate::actions::toggle_autostart() {
                *BALLOON.lock().unwrap() = Some(format!("Could not change Start at login: {e}"));
                unsafe {
                    let _ = PostMessageW(Some(hwnd), WM_TRAY_BALLOON, WPARAM(0), LPARAM(0));
                }
            }
        }
        MenuItem::CheckUpdates => crate::actions::check_updates(),
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
        WM_DPICHANGED => refresh_icon(hwnd),
        WM_SETTINGCHANGE => {
            // lParam is a wide string naming the changed setting; theme switches send this one.
            let is_theme = l.0 != 0 && {
                let p = l.0 as *const u16;
                let want: Vec<u16> = "ImmersiveColorSet".encode_utf16().collect();
                (0..want.len()).all(|i| unsafe { *p.add(i) } == want[i])
                    && unsafe { *p.add(want.len()) } == 0
            };
            if !is_theme {
                return None;
            }
            refresh_icon(hwnd);
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
            let hwnd = FindWindowW(crate::instance::tray_class(), PCWSTR::null()).unwrap();
            let mut nid = base_data(hwnd);
            nid.uFlags = NIF_TIP;
            copy_wide(&mut nid.szTip, "Rustshot");
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
        assert!(unsafe { FindWindowW(crate::instance::tray_class(), PCWSTR::null()) }.is_err());
    }
}
