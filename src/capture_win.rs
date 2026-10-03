//! Win32 capture: DPI awareness, monitor enumeration, GDI bit-blits.

use super::*;

use std::mem::size_of;
use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject,
    EnumDisplayMonitors, GetDC, GetDIBits, GetMonitorInfoW, ReleaseDC, SelectObject, BI_RGB,
    BITMAPINFO, DIB_RGB_COLORS, HDC, HGDIOBJ, HMONITOR, MONITORINFO, SRCCOPY,
};
use windows::Win32::UI::HiDpi::{
    GetDpiForMonitor, SetProcessDpiAwarenessContext, MDT_EFFECTIVE_DPI,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, EnumWindows, GetCursorPos, GetForegroundWindow, GetWindowThreadProcessId,
    IsWindowVisible, SetForegroundWindow, MONITORINFOF_PRIMARY,
};

/// Must be called before any window or capture happens so that all
/// coordinates (cursor, monitor rects, GDI blits) are physical pixels.
pub fn enable_dpi_awareness() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

/// Enumerate monitors via `EnumDisplayMonitors` (replaces xcap).
pub fn monitors() -> Result<Vec<MonInfo>> {
    unsafe extern "system" fn enum_cb(
        hmon: HMONITOR,
        _hdc: HDC,
        _rect: *mut RECT,
        lparam: LPARAM,
    ) -> BOOL {
        unsafe {
            let out = &mut *(lparam.0 as *mut Vec<MonInfo>);
            let mut info: MONITORINFO = std::mem::zeroed();
            info.cbSize = size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(hmon, &mut info).as_bool() {
                let (mut dpix, mut dpiy) = (96u32, 96u32);
                if GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dpix, &mut dpiy).is_err() {
                    dpix = 96;
                }
                out.push(MonInfo {
                    x: info.rcMonitor.left,
                    y: info.rcMonitor.top,
                    w: (info.rcMonitor.right - info.rcMonitor.left) as u32,
                    h: (info.rcMonitor.bottom - info.rcMonitor.top) as u32,
                    scale: (dpix as f32 / 96.0).max(0.25),
                    primary: info.dwFlags & MONITORINFOF_PRIMARY != 0,
                });
            }
            BOOL(1)
        }
    }

    let mut out: Vec<MonInfo> = Vec::new();
    let slot: *mut Vec<MonInfo> = &mut out;
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(enum_cb), LPARAM(slot as isize));
    }
    if out.is_empty() {
        return Err(anyhow!("no monitors found"));
    }
    Ok(out)
}

/// Capture a virtual-screen rect with GDI (`BitBlt` from the screen DC).
pub fn gdi_capture(x: i32, y: i32, w: u32, h: u32) -> Result<PixBuf> {
    if w == 0 || h == 0 {
        return Err(anyhow!("empty capture region"));
    }
    unsafe {
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let bmp = CreateCompatibleBitmap(screen, w as i32, h as i32);
        let old = SelectObject(mem, HGDIOBJ(bmp.0));
        let blit = BitBlt(mem, 0, 0, w as i32, h as i32, Some(screen), x, y, SRCCOPY);

        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader.biSize = size_of::<windows::Win32::Graphics::Gdi::BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = w as i32;
        bmi.bmiHeader.biHeight = -(h as i32); // top-down
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB.0;
        let mut buf = vec![0u8; (w as usize) * (h as usize) * 4];
        let got = GetDIBits(
            mem,
            bmp,
            0,
            h,
            Some(buf.as_mut_ptr() as *mut _),
            &mut bmi,
            DIB_RGB_COLORS,
        );

        let _ = SelectObject(mem, old);
        let _ = DeleteObject(HGDIOBJ(bmp.0));
        let _ = DeleteDC(mem);
        let _ = ReleaseDC(None, screen);

        blit.map_err(|e| anyhow!("screen blit failed: {e}"))?;
        if got == 0 {
            return Err(anyhow!("GetDIBits failed"));
        }
        // GDI hands back BGRA with a meaningless alpha channel.
        for px in buf.as_chunks_mut::<4>().0 {
            px.swap(0, 2);
            px[3] = 255;
        }
        PixBuf::from_raw(w, h, buf).ok_or_else(|| anyhow!("bad capture buffer"))
    }
}

pub fn cursor_pos() -> (i32, i32) {
    unsafe {
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        (p.x, p.y)
    }
}

/// Bring this process' visible window to the foreground so it receives
/// keyboard input. Windows refuses `SetForegroundWindow` for background
/// processes unless we attach to the current foreground thread's input
/// queue first — without this, editor shortcuts never reach the window.
pub fn focus_our_window() {
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};

    unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        unsafe {
            let slot = &mut *(lparam.0 as *mut HWND);
            let mut p = 0u32;
            let _ = GetWindowThreadProcessId(hwnd, Some(&mut p));
            if p == windows::Win32::System::Threading::GetCurrentProcessId()
                && IsWindowVisible(hwnd).as_bool()
            {
                *slot = hwnd;
                BOOL(0) // stop enumerating
            } else {
                BOOL(1)
            }
        }
    }

    unsafe {
        // Find our own visible top-level window.
        let mut found = HWND::default();
        let slot: *mut HWND = &mut found;
        let _ = EnumWindows(Some(enum_cb), LPARAM(slot as isize));
        if found.0.is_null() {
            return;
        }
        let fg = GetForegroundWindow();
        if fg == found {
            return;
        }
        let fg_tid = if !fg.0.is_null() {
            GetWindowThreadProcessId(fg, None)
        } else {
            0
        };
        let our_tid = GetCurrentThreadId();
        let attached = fg_tid != 0 && AttachThreadInput(our_tid, fg_tid, true).as_bool();
        let _ = BringWindowToTop(found);
        let _ = SetForegroundWindow(found);
        if attached {
            let _ = AttachThreadInput(our_tid, fg_tid, false);
        }
    }
}
