//! Win32 overlay window implementation: class registration, message pump,
//! input event delivery, and framebuffer presentation (StretchDIBits).

use super::*;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateRectRgn, DeleteObject, EndPaint, GetRegionData, GetUpdateRgn,
    InvalidateRect, ScreenToClient, StretchDIBits, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS, PAINTSTRUCT, RGNDATA, RGNDATAHEADER, SRCCOPY,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::*;

fn cursor_id(c: Cursor) -> PCWSTR {
    match c {
        Cursor::Arrow => IDC_ARROW,
        Cursor::Cross => IDC_CROSS,
        Cursor::IBeam => IDC_IBEAM,
        Cursor::SizeNS => IDC_SIZENS,
        Cursor::SizeWE => IDC_SIZEWE,
        Cursor::SizeNWSE => IDC_SIZENWSE,
        Cursor::SizeNESW => IDC_SIZENESW,
        Cursor::Move => IDC_SIZEALL,
    }
}

static CLASS_REGISTERED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

unsafe fn module_handle() -> windows::core::Result<*mut core::ffi::c_void> {
    unsafe {
        GetModuleHandleW(PCWSTR(core::ptr::null()))
            .map(|h| h.0)
    }
}

unsafe fn register_class() {
    if CLASS_REGISTERED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    unsafe {
        let h = module_handle().unwrap_or(core::ptr::null_mut());
        let mut wc: WNDCLASSEXW = std::mem::zeroed();
        wc.cbSize = std::mem::size_of::<WNDCLASSEXW>() as u32;
        wc.style = CS_HREDRAW | CS_VREDRAW;
        wc.lpfnWndProc = Some(wndproc);
        wc.hInstance = HINSTANCE(h);
        wc.hCursor = LoadCursorW(None, IDC_ARROW).unwrap_or_default();
        wc.lpszClassName = w!("rustshot_overlay");
        let _ = RegisterClassExW(&wc);
    }
}

unsafe fn x_of(lp: LPARAM) -> i32 {
    (lp.0 & 0xffff) as u16 as i16 as i32
}

unsafe fn y_of(lp: LPARAM) -> i32 {
    ((lp.0 >> 16) & 0xffff) as u16 as i16 as i32
}

/// Ask for a repaint of the window's client area.
pub fn invalidate(hwnd: HWND) {
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

/// Invalidate rects `[x0, y0, x1, y1]` of the client area (no erase).
pub fn invalidate_rects(hwnd: HWND, rects: &[[i32; 4]]) {
    for r in rects {
        let rc = RECT { left: r[0], top: r[1], right: r[2], bottom: r[3] };
        unsafe {
            let _ = InvalidateRect(Some(hwnd), Some(&rc), false);
        }
    }
}

/// The update region as rects (before `BeginPaint` validates it); its
/// bounding box when it has many pieces.
unsafe fn update_rects(hwnd: HWND) -> Vec<[i32; 4]> {
    unsafe {
        let rgn = CreateRectRgn(0, 0, 0, 0);
        let mut out = Vec::new();
        // 0 = ERROR, 1 = NULLREGION: nothing to read.
        if GetUpdateRgn(hwnd, rgn, false).0 > 1 {
            let n = GetRegionData(rgn, 0, None);
            if n as usize >= std::mem::size_of::<RGNDATAHEADER>() {
                // u64 storage keeps the RECTs after the header aligned.
                let mut buf = vec![0u64; (n as usize).div_ceil(8)];
                let data = buf.as_mut_ptr() as *mut RGNDATA;
                if GetRegionData(rgn, n, Some(data)) != 0 {
                    let hdr = &(*data).rdh;
                    let rects = std::slice::from_raw_parts(
                        (*data).Buffer.as_ptr() as *const RECT,
                        hdr.nCount as usize,
                    );
                    if rects.len() > 32 {
                        let b = hdr.rcBound;
                        out.push([b.left, b.top, b.right, b.bottom]);
                    } else {
                        out.extend(rects.iter().map(|r| [r.left, r.top, r.right, r.bottom]));
                    }
                }
            }
        }
        let _ = DeleteObject(rgn.into());
        out
    }
}

/// Re-arm the window timer (same id replaces the old interval).
pub fn retime(hwnd: HWND, ms: u64) {
    if hwnd.is_invalid() {
        return;
    }
    unsafe {
        let _ = SetTimer(Some(hwnd), 1, ms as u32, None);
    }
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
) -> LRESULT {
    unsafe {
        if msg == WM_NCCREATE {
            let cs = &*(lp.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
            return DefWindowProcW(hwnd, msg, wp, lp);
        }
        let slot = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut *mut dyn Driver;
        if slot.is_null() {
            return DefWindowProcW(hwnd, msg, wp, lp);
        }
        let drv = &mut **slot;
        match msg {
            WM_MOUSEMOVE => {
                // No automatic repaint: the driver calls `invalidate` itself
                // when a move actually changes what should be on screen.
                drv.on_event(Ev::Move {
                    x: x_of(lp),
                    y: y_of(lp),
                });
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let _ = SetCapture(hwnd);
                drv.on_event(Ev::Down {
                    x: x_of(lp),
                    y: y_of(lp),
                });
                request(hwnd, drv);
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let _ = ReleaseCapture();
                drv.on_event(Ev::Up {
                    x: x_of(lp),
                    y: y_of(lp),
                });
                request(hwnd, drv);
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                let delta = ((wp.0 >> 16) & 0xffff) as u16 as i16 as i32;
                let mut pt = POINT {
                    x: x_of(lp),
                    y: y_of(lp),
                };
                let _ = ScreenToClient(hwnd, &mut pt);
                drv.on_event(Ev::Wheel {
                    delta,
                    x: pt.x,
                    y: pt.y,
                });
                request(hwnd, drv);
                LRESULT(0)
            }
            WM_KEYDOWN | WM_KEYUP => {
                let repeat = msg == WM_KEYDOWN && (lp.0 & (1 << 30)) != 0;
                drv.on_event(Ev::Key {
                    vk: wp.0 as u32,
                    up: msg == WM_KEYUP,
                    repeat,
                    mods: Mods::current(),
                });
                request(hwnd, drv);
                LRESULT(0)
            }
            WM_CHAR => {
                drv.on_event(Ev::Char(wp.0 as u16));
                request(hwnd, drv);
                LRESULT(0)
            }
            WM_TIMER => {
                // Idle ticks (nothing animating) cost no frame.
                if drv.on_event(Ev::Timer) {
                    request(hwnd, drv);
                }
                LRESULT(0)
            }
            WM_PAINT => {
                let rects = update_rects(hwnd);
                let mut ps: PAINTSTRUCT = std::mem::zeroed();
                let hdc = BeginPaint(hwnd, &mut ps);
                let rc = ps.rcPaint;
                let rects = if rects.is_empty() { vec![[rc.left, rc.top, rc.right, rc.bottom]] } else { rects };
                if !drv.paint(hdc, &rects)
                    && let Some(fb) = drv.frame()
                {
                    present(hdc, fb);
                }
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_SETCURSOR => {
                if let Ok(c) = LoadCursorW(None, cursor_id(drv.cursor())) {
                    let _ = SetCursor(Some(c));
                }
                LRESULT(1)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_DESTROY => {
                drv.on_quit();
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

/// RGBA <-> BGRA: swap the R and B bytes of every pixel in place (its own
/// inverse), one pass, no allocation.
/// (`BI_BITFIELDS` masks would let GDI read RGBA directly, but its
/// conversion path is ~40x slower than this swap plus a BI_RGB blit.)
pub(crate) fn swap_rb(buf: &mut [u8]) {
    // Two pixels per step (the release profile is size-optimised, so no
    // auto-vectorisation): keep G/A, exchange the R and B bytes.
    let (b8, bt) = buf.as_chunks_mut::<8>();
    for o in b8.iter_mut() {
        let v = u64::from_le_bytes(*o);
        let ga = v & 0xFF00_FF00_FF00_FF00;
        let r = v & 0x0000_00FF_0000_00FF;
        let b = v & 0x00FF_0000_00FF_0000;
        *o = (ga | (r << 16) | (b >> 16)).to_le_bytes();
    }
    for o in bt.as_chunks_mut::<4>().0.iter_mut() {
        *o = [o[2], o[1], o[0], o[3]];
    }
}

/// Present an unpremultiplied RGBA framebuffer as top-down BGRA.
///
/// Converts `fb` to BGRA in place (no staging copy), so its contents are
/// BGRA afterwards. Sound because every paint (WM_PAINT) calls
/// `Driver::frame()` first, which rewrites the whole buffer; nothing
/// presents the same frame twice.
fn present(hdc: windows::Win32::Graphics::Gdi::HDC, fb: &mut PixBuf) {
    let (w, h) = fb.dimensions();
    if w == 0 || h == 0 {
        return;
    }
    let mut bmi: BITMAPINFO = unsafe { std::mem::zeroed() };
    bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    bmi.bmiHeader.biWidth = w as i32;
    bmi.bmiHeader.biHeight = -(h as i32); // top-down
    bmi.bmiHeader.biPlanes = 1;
    bmi.bmiHeader.biBitCount = 32;
    bmi.bmiHeader.biCompression = BI_RGB.0;
    swap_rb(fb.as_raw_mut());
    unsafe {
        StretchDIBits(
            hdc,
            0,
            0,
            w as i32,
            h as i32,
            0,
            0,
            w as i32,
            h as i32,
            Some(fb.as_raw().as_ptr() as *const _),
            &bmi,
            DIB_RGB_COLORS,
            SRCCOPY,
        );
    }
}

/// Position + show the overlay at an exact physical rect and take focus.
pub fn show_at(hwnd: HWND, x: i32, y: i32, w: i32, h: i32) {
    unsafe {
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_SHOWWINDOW);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
}

pub fn hide(hwnd: HWND) {
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
}

pub fn close(hwnd: HWND) {
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

/// Create the (initially hidden) overlay and pump messages until quit.
/// Returns when the window is destroyed.
pub fn run(driver: &mut dyn Driver) -> i32 {
    unsafe {
        register_class();
        let h = module_handle().unwrap_or(core::ptr::null_mut());
        let slot: Box<*mut dyn Driver> = Box::new(driver);
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            w!("rustshot_overlay"),
            w!("rustshot"),
            WS_POPUP,
            -32000,
            -32000,
            400,
            300,
            None,
            None,
            Some(HINSTANCE(h)),
            Some((&raw const *slot) as *const core::ffi::c_void),
        );
        match hwnd {
            Ok(hwnd) => {
                driver.on_create(hwnd);
                let _ = SetTimer(Some(hwnd), 1, SLOW_TICK_MS as u32, None);
                let mut msg = MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                let _ = DestroyWindow(hwnd);
            }
            Err(e) => {
                eprintln!("failed to create overlay window: {e}");
                return 1;
            }
        }
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GdiFlush,
        SelectObject,
    };

    /// `present` into a BGRA DIB section: every channel lands in place.
    #[test]
    #[ignore = "GDI rendering check (no window)"]
    fn present_colors_round_trip() {
        let px: [[u8; 4]; 8] = [
            [255, 0, 0, 255],
            [0, 255, 0, 255],
            [0, 0, 255, 255],
            [255, 255, 255, 255],
            [12, 34, 56, 255],
            [200, 100, 50, 255],
            [1, 2, 3, 255],
            [250, 128, 7, 255],
        ];
        let mut fb = PixBuf::from_raw(4, 2, px.concat()).unwrap();
        unsafe {
            let dc = CreateCompatibleDC(None);
            assert!(!dc.is_invalid());
            let mut bmi: BITMAPINFO = std::mem::zeroed();
            bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bmi.bmiHeader.biWidth = 4;
            bmi.bmiHeader.biHeight = -2;
            bmi.bmiHeader.biPlanes = 1;
            bmi.bmiHeader.biBitCount = 32;
            bmi.bmiHeader.biCompression = BI_RGB.0;
            let mut bits: *mut core::ffi::c_void = core::ptr::null_mut();
            let bmp = CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0).unwrap();
            let old = SelectObject(dc, bmp.into());
            present(dc, &mut fb);
            let _ = GdiFlush();
            let out = std::slice::from_raw_parts(bits as *const u8, 4 * 2 * 4).to_vec();
            SelectObject(dc, old);
            let _ = DeleteObject(bmp.into());
            let _ = DeleteDC(dc);
            for (i, p) in px.iter().enumerate() {
                let q = &out[i * 4..i * 4 + 3];
                assert_eq!([q[2], q[1], q[0]], [p[0], p[1], p[2]], "pixel {i}: BGRA {q:?}");
            }
        }
    }
}
