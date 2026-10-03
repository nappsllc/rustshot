use anyhow::{anyhow, Context, Result};
use image::RgbaImage;
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

#[derive(Debug, Clone)]
pub struct MonInfo {
    pub index: usize,
    /// Physical position in virtual-screen coordinates.
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub scale: f32,
    pub primary: bool,
}

impl MonInfo {
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.x + self.w as i32 && y >= self.y && y < self.y + self.h as i32
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
                    index: out.len(),
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
fn gdi_capture(x: i32, y: i32, w: u32, h: u32) -> Result<RgbaImage> {
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
        for px in buf.chunks_exact_mut(4) {
            px.swap(0, 2);
            px[3] = 255;
        }
        RgbaImage::from_raw(w, h, buf).ok_or_else(|| anyhow!("bad capture buffer"))
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

pub fn monitor_at_cursor(mons: &[MonInfo]) -> usize {
    let (cx, cy) = cursor_pos();
    mons
        .iter()
        .position(|m| m.contains(cx, cy))
        .unwrap_or_else(|| mons.iter().position(|m| m.primary).unwrap_or(0))
}

/// `Some(scale)` when every monitor reports the same scale factor.
pub fn uniform_scale(mons: &[MonInfo]) -> Option<f32> {
    let first = mons.first()?.scale;
    mons.iter().all(|m| (m.scale - first).abs() < 0.01).then_some(first)
}

/// A captured region of the desktop in physical pixels.
#[derive(Clone)]
pub struct Shot {
    /// Physical position of the top-left corner in virtual-screen coords.
    pub origin: (i32, i32),
    pub size: (u32, u32),
    /// Scale factor that maps physical pixels to logical points for the
    /// window that will display this shot.
    pub scale: f32,
    pub image: RgbaImage,
}

impl Shot {
    /// Convert a global (virtual-screen physical) point to image coordinates.
    pub fn global_to_image(&self, p: (i32, i32)) -> (i32, i32) {
        (p.0 - self.origin.0, p.1 - self.origin.1)
    }
}

fn union_rect(mons: &[MonInfo]) -> (i32, i32, u32, u32) {
    let min_x = mons.iter().map(|m| m.x).min().unwrap_or(0);
    let min_y = mons.iter().map(|m| m.y).min().unwrap_or(0);
    let max_x = mons
        .iter()
        .map(|m| m.x + m.w as i32)
        .max()
        .unwrap_or(0);
    let max_y = mons
        .iter()
        .map(|m| m.y + m.h as i32)
        .max()
        .unwrap_or(0);
    (min_x, min_y, (max_x - min_x) as u32, (max_y - min_y) as u32)
}

/// Capture a single monitor.
pub fn grab_monitor(index: usize) -> Result<Shot> {
    let mons = monitors()?;
    let m = mons
        .get(index)
        .ok_or_else(|| anyhow!("monitor index {index} out of range"))?;
    let image = gdi_capture(m.x, m.y, m.w, m.h)?;
    Ok(Shot {
        origin: (m.x, m.y),
        size: (m.w, m.h),
        scale: m.scale,
        image,
    })
}

/// Capture the whole virtual desktop (all monitors composited).
/// Only valid when all monitors share one scale factor (caller checks).
pub fn grab_span(mons: &[MonInfo]) -> Result<Shot> {
    let (ox, oy, w, h) = union_rect(mons);
    let scale =
        uniform_scale(mons).unwrap_or_else(|| mons.first().map(|m| m.scale).unwrap_or(1.0));
    let image = gdi_capture(ox, oy, w, h)?;
    Ok(Shot {
        origin: (ox, oy),
        size: (w, h),
        scale,
        image,
    })
}

/// Decide what the interactive editor should capture:
/// a specific monitor, the monitor under the cursor, or the full span.
pub fn grab_edit(screen: Option<u32>, active_monitor_only: bool) -> Result<Shot> {
    let mons = monitors()?;
    let span_ok = uniform_scale(&mons).is_some() && !active_monitor_only;
    match screen {
        Some(n) => grab_monitor(n as usize),
        None if span_ok => grab_span(&mons),
        None => grab_monitor(monitor_at_cursor(&mons)),
    }
}

/// Global rect (virtual-screen physical) of a monitor or `all`.
pub fn region_of(spec: &str) -> Result<(i32, i32, u32, u32)> {
    let mons = monitors()?;
    if spec == "all" {
        return Ok(union_rect(&mons));
    }
    if let Some(n) = spec.strip_prefix("screen") {
        let n: usize = n.parse().context("bad screen number")?;
        let m = mons
            .get(n)
            .ok_or_else(|| anyhow!("monitor index {n} out of range"))?;
        return Ok((m.x, m.y, m.w, m.h));
    }
    parse_geometry(spec)
}

/// Parse `WxH+X+Y` (Flameshot `--region` format) into a global rect.
/// Signs belong to the numbers: `100x100+-20+50` or `100x100-20-30` both
/// mean x = -20, y = -30 in the second case only because the separator `-`
/// doubles as the sign; `+0+0` style is the canonical form.
pub fn parse_geometry(spec: &str) -> Result<(i32, i32, u32, u32)> {
    let x_pos = spec
        .find('x')
        .ok_or_else(|| anyhow!("region must look like WxH+X+Y, got {spec:?}"))?;
    let rest_start = spec[x_pos + 1..]
        .find(['+', '-'])
        .map(|i| x_pos + 1 + i)
        .ok_or_else(|| anyhow!("region must look like WxH+X+Y, got {spec:?}"))?;
    let (wh, rest) = spec.split_at(rest_start);
    let (w, h) = wh
        .split_once('x')
        .ok_or_else(|| anyhow!("region must look like WxH+X+Y, got {spec:?}"))?;
    let w: u32 = w.parse().context("bad region width")?;
    let h: u32 = h.parse().context("bad region height")?;
    let (x, y) = parse_signed_pair(rest)?;
    Ok((x, y, w, h))
}

/// Parse something like `+100+50`, `-20-30`, `100+50` into two signed ints.
fn parse_signed_pair(s: &str) -> Result<(i32, i32)> {
    let b = s.as_bytes();
    let mut i = 0;
    let mut nums = Vec::new();
    while i < b.len() {
        let mut sign = 1i32;
        while i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            if b[i] == b'-' {
                sign = -sign;
            }
            i += 1;
        }
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if start == i {
            return Err(anyhow!("bad region offsets in {s:?}"));
        }
        nums.push(sign * s[start..i].parse::<i32>()?);
    }
    match nums.len() {
        2 => Ok((nums[0], nums[1])),
        _ => Err(anyhow!("region must have exactly two offsets, got {s:?}")),
    }
}

/// Crop a global rect out of a shot, clamped to its bounds.
pub fn crop_global(shot: &Shot, rect: (i32, i32, u32, u32)) -> Result<RgbaImage> {
    let (lx, ly) = shot.global_to_image((rect.0, rect.1));
    let x = lx.clamp(0, shot.image.width() as i32 - 1) as u32;
    let y = ly.clamp(0, shot.image.height() as i32 - 1) as u32;
    let w = rect.2.min(shot.image.width() - x);
    let h = rect.3.min(shot.image.height() - y);
    if w == 0 || h == 0 {
        return Err(anyhow!("region is outside the captured screen"));
    }
    Ok(image::imageops::crop_imm(&shot.image, x, y, w, h).to_image())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_geometry() {
        assert_eq!(parse_geometry("800x600+100+50").unwrap(), (100, 50, 800, 600));
        assert_eq!(parse_geometry("10x10-20-30").unwrap(), (-20, -30, 10, 10));
        assert!(parse_geometry("nope").is_err());
    }

    #[test]
    #[ignore = "live display access"]
    fn live_monitor_capture() {
        enable_dpi_awareness();
        let mons = monitors().expect("monitors");
        assert!(!mons.is_empty());
        let shot = grab_monitor(0).expect("grab monitor 0");
        assert_eq!(shot.size, (mons[0].w, mons[0].h));
        assert_eq!(shot.image.dimensions(), shot.size);
        assert!(shot.scale >= 0.25);
    }
}
