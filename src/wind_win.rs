//! Win32 window implementation: the overlay and the decorated `run_window`
//! (class registration, message pumps, input event delivery) and
//! framebuffer presentation (StretchDIBits).

use super::*;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    GetLastError, ERROR_CLASS_ALREADY_EXISTS, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateRectRgn, DeleteObject, EndPaint, GetMonitorInfoW, GetRegionData,
    GetUpdateRgn, InvalidateRect, MonitorFromPoint, ScreenToClient, StretchDIBits, UpdateWindow,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    PAINTSTRUCT, RGNDATA, RGNDATAHEADER, SRCCOPY,
};
use windows::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, GetDpiForMonitor, GetDpiForWindow, SetThreadDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, MDT_EFFECTIVE_DPI,
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

/// RegisterClassExW succeeded, or the class exists already (another thread
/// won the race): either way the class is usable.
unsafe fn class_ok(atom: u16) -> bool {
    atom != 0 || unsafe { GetLastError() } == ERROR_CLASS_ALREADY_EXISTS
}

unsafe fn register_class() {
    // Only a success is remembered: a failed registration is retried by
    // the next call.
    if CLASS_REGISTERED.load(std::sync::atomic::Ordering::Relaxed) {
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
        if class_ok(RegisterClassExW(&wc)) {
            CLASS_REGISTERED.store(true, std::sync::atomic::Ordering::Relaxed);
        }
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
    // A null handle would invalidate every window on the desktop.
    if hwnd.is_invalid() {
        return;
    }
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

/// Invalidate rects `[x0, y0, x1, y1]` of the client area (no erase).
pub fn invalidate_rects(hwnd: HWND, rects: &[[i32; 4]]) {
    if hwnd.is_invalid() {
        return;
    }
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
    #[cfg(test)]
    tests::RETIMES.lock().unwrap_or_else(|e| e.into_inner()).push((hwnd.0 as isize, ms));
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
        if msg == WM_DESTROY {
            drv.on_quit();
            forget_tick(hwnd);
            PostQuitMessage(0);
            return LRESULT(0);
        }
        common(hwnd, msg, wp, lp, drv).unwrap_or_else(|| DefWindowProcW(hwnd, msg, wp, lp))
    }
}

/// Input, timer and paint messages shared by the overlay and `run_window`;
/// `None` = not handled here.
unsafe fn common(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM, drv: &mut dyn Driver) -> Option<LRESULT> {
    unsafe {
        Some(match msg {
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
            _ => return None,
        })
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
    if hwnd.is_invalid() {
        return;
    }
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
            w!("Rustshot"),
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
                let _ = SetTimer(Some(hwnd), 1, tick_ms(hwnd) as u32, None);
                let mut msg = MSG::default();
                let mut code = 0;
                loop {
                    match GetMessageW(&mut msg, None, 0, 0).0 {
                        0 => break,
                        -1 => {
                            eprintln!("overlay message loop failed: {}", windows::core::Error::from_thread());
                            code = 1;
                            break;
                        }
                        _ => {
                            let _ = TranslateMessage(&msg);
                            DispatchMessageW(&msg);
                        }
                    }
                }
                let _ = DestroyWindow(hwnd);
                code
            }
            Err(e) => {
                eprintln!("failed to create overlay window: {e}");
                1
            }
        }
    }
}

// --- decorated window (run_window) ----------------------------------------

/// State behind GWLP_USERDATA of a `run_window` window (lives on
/// `run_window`'s stack frame for the whole loop).
struct WinState<'a> {
    drv: *mut (dyn Driver + 'a),
    /// Set right before `on_create`: CreateWindowEx already sends WM_SIZE
    /// etc., which must not reach a driver that has no handle yet.
    ready: core::cell::Cell<bool>,
    /// WM_DESTROY seen: the loop ends.
    done: core::cell::Cell<bool>,
    /// Last size sent as `Ev::Resize` (WM_SIZE repeats it on show/restore).
    size: core::cell::Cell<(u32, u32)>,
    min: (u32, u32),
    resizable: bool,
    style: WINDOW_STYLE,
    ex_style: WINDOW_EX_STYLE,
}

const WINDOW_CLASS: PCWSTR = w!("rustshot_window");
/// Registered successfully (a failure is not cached: the next call retries).
static WINDOW_CLASS_OK: std::sync::Mutex<bool> = std::sync::Mutex::new(false);

/// Register the decorated-window class once (any thread may be first).
/// Icons come from the exe's icon resource (ID 1, embedded by build.rs);
/// shared handles, so nothing is destroyed. Builds without the resource
/// (tests, non-MSVC) fall back to the default icon.
unsafe fn register_window_class(h: HINSTANCE) -> anyhow::Result<()> {
    let mut ok = WINDOW_CLASS_OK.lock().unwrap_or_else(|e| e.into_inner());
    if *ok {
        return Ok(());
    }
    unsafe {
        let icon = |cx, cy| {
            LoadImageW(Some(h), PCWSTR(std::ptr::without_provenance(1)) /* MAKEINTRESOURCE(1) */, IMAGE_ICON, cx, cy, LR_SHARED)
                .map(|i| HICON(i.0))
                .unwrap_or_default()
        };
        let mut wc: WNDCLASSEXW = std::mem::zeroed();
        wc.cbSize = std::mem::size_of::<WNDCLASSEXW>() as u32;
        wc.style = CS_HREDRAW | CS_VREDRAW;
        wc.lpfnWndProc = Some(wndproc_window);
        wc.hInstance = h;
        wc.hCursor = LoadCursorW(None, IDC_ARROW).unwrap_or_default();
        wc.hIcon = icon(GetSystemMetrics(SM_CXICON), GetSystemMetrics(SM_CYICON));
        wc.hIconSm = icon(GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON));
        wc.lpszClassName = WINDOW_CLASS;
        if !class_ok(RegisterClassExW(&wc)) {
            anyhow::bail!("RegisterClassExW failed: {}", windows::core::Error::from_thread());
        }
    }
    *ok = true;
    Ok(())
}

/// DPI scale of a window (1.0 = 96 dpi): logical -> physical px factor.
pub fn scale(hwnd: HWND) -> f32 {
    match unsafe { GetDpiForWindow(hwnd) } {
        0 => 1.0,
        dpi => dpi as f32 / 96.0,
    }
}

unsafe extern "system" fn wndproc_window(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_NCCREATE {
            let cs = &*(lp.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
            return DefWindowProcW(hwnd, msg, wp, lp);
        }
        let st = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const WinState<'static>;
        if st.is_null() {
            return DefWindowProcW(hwnd, msg, wp, lp);
        }
        let st = &*st;
        match msg {
            WM_GETMINMAXINFO => {
                if st.resizable {
                    let dpi = GetDpiForWindow(hwnd).max(96);
                    let s = dpi as f32 / 96.0;
                    let mut rc = RECT {
                        left: 0,
                        top: 0,
                        right: to_phys(st.min.0, s) as i32,
                        bottom: to_phys(st.min.1, s) as i32,
                    };
                    let _ = AdjustWindowRectExForDpi(&mut rc, st.style, false, st.ex_style, dpi);
                    let mmi = &mut *(lp.0 as *mut MINMAXINFO);
                    mmi.ptMinTrackSize = POINT { x: rc.right - rc.left, y: rc.bottom - rc.top };
                }
                return LRESULT(0);
            }
            WM_DPICHANGED => {
                // Moved onto a monitor with another scale: take the size
                // Windows suggests (keeps the logical size); WM_SIZE follows.
                let r = &*(lp.0 as *const RECT);
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                return LRESULT(0);
            }
            WM_NCDESTROY => {
                // The state dies with run_window's frame: never reach it again.
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                return DefWindowProcW(hwnd, msg, wp, lp);
            }
            _ => {}
        }
        if !st.ready.get() {
            return DefWindowProcW(hwnd, msg, wp, lp);
        }
        let drv = &mut *st.drv;
        match msg {
            WM_CLOSE => {
                // Never DefWindowProc (it would destroy): the driver decides.
                drv.on_event(Ev::Close);
                if !st.done.get() {
                    request(hwnd, drv);
                }
                LRESULT(0)
            }
            WM_SIZE => {
                if wp.0 != SIZE_MINIMIZED as usize {
                    let size = ((lp.0 & 0xffff) as u32, ((lp.0 >> 16) & 0xffff) as u32);
                    if st.size.replace(size) != size {
                        drv.on_event(Ev::Resize(size.0, size.1));
                    }
                    invalidate(hwnd);
                }
                LRESULT(0)
            }
            WM_SETFOCUS | WM_KILLFOCUS => {
                drv.on_event(Ev::Focus(msg == WM_SETFOCUS));
                request(hwnd, drv);
                LRESULT(0)
            }
            // Borders/caption keep their resize/arrow cursors.
            WM_SETCURSOR if (lp.0 & 0xffff) as u32 != HTCLIENT => DefWindowProcW(hwnd, msg, wp, lp),
            WM_DESTROY => {
                st.done.set(true);
                drv.on_quit();
                forget_tick(hwnd);
                LRESULT(0)
            }
            _ => common(hwnd, msg, wp, lp, drv).unwrap_or_else(|| DefWindowProcW(hwnd, msg, wp, lp)),
        }
    }
}

/// Bring a `run_window` window to the front, restoring it if minimised
/// (best effort: Windows may only flash its taskbar button).
pub fn raise(hwnd: HWND) {
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let _ = SetForegroundWindow(hwnd);
    }
}

/// Open a normal decorated, per-monitor-DPI-aware top-level window centred
/// on the monitor under the cursor and pump this thread's messages until
/// it is destroyed. Callable from any thread (each thread has its own
/// queue): the close button only sends `Ev::Close`; the driver ends the
/// loop with `wind::close(hwnd)` from one of its callbacks. The driver
/// gets `Ev::Resize` (physical px) right after `on_create` and on every
/// resize; `wind::scale(hwnd)` gives the DPI factor. A `WM_QUIT` that
/// arrives meanwhile (another window on this thread quitting) closes the
/// window and is re-posted for the caller's own loop.
pub fn run_window(spec: WindowSpec, drv: &mut dyn Driver) -> anyhow::Result<()> {
    unsafe {
        // Per thread, so it holds even where the process never opted in.
        let prev = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let r = window_loop(&spec, drv);
        if !prev.0.is_null() {
            SetThreadDpiAwarenessContext(prev);
        }
        r
    }
}

unsafe fn window_loop(spec: &WindowSpec, drv: &mut dyn Driver) -> anyhow::Result<()> {
    unsafe {
        let h = HINSTANCE(module_handle()?);
        register_window_class(h)?;
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut mi);
        let (mut dx, mut dy) = (96u32, 96u32);
        let dpi = if GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy).is_ok() { dx.max(96) } else { 96 };
        let s = dpi as f32 / 96.0;
        let style = if spec.resizable {
            WS_OVERLAPPEDWINDOW
        } else {
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX
        };
        let ex_style = WS_EX_APPWINDOW;
        let mut rc = RECT { left: 0, top: 0, right: to_phys(spec.w, s) as i32, bottom: to_phys(spec.h, s) as i32 };
        let _ = AdjustWindowRectExForDpi(&mut rc, style, false, ex_style, dpi);
        let (ow, oh) = (rc.right - rc.left, rc.bottom - rc.top);
        let wa = mi.rcWork;
        let (x, y) = centre_in((wa.left, wa.top, wa.right - wa.left, wa.bottom - wa.top), ow, oh);
        let title: Vec<u16> = spec.title.encode_utf16().chain(Some(0)).collect();
        let st = WinState {
            drv: drv as *mut (dyn Driver + '_),
            ready: core::cell::Cell::new(false),
            done: core::cell::Cell::new(false),
            size: core::cell::Cell::new((0, 0)),
            min: spec.min,
            resizable: spec.resizable,
            style,
            ex_style,
        };
        let hwnd = CreateWindowExW(
            ex_style,
            WINDOW_CLASS,
            PCWSTR(title.as_ptr()),
            style,
            x,
            y,
            ow,
            oh,
            None,
            None,
            Some(h),
            Some(&raw const st as *const core::ffi::c_void),
        )?;
        st.ready.set(true);
        (*st.drv).on_create(hwnd);
        if !st.done.get() {
            let mut cr = RECT::default();
            let _ = GetClientRect(hwnd, &mut cr);
            let size = (cr.right.max(0) as u32, cr.bottom.max(0) as u32);
            st.size.set(size);
            (*st.drv).on_event(Ev::Resize(size.0, size.1));
        }
        if !st.done.get() {
            let _ = SetTimer(Some(hwnd), 1, tick_ms(hwnd) as u32, None);
            // Tests never steal the user's focus.
            if cfg!(test) {
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            } else {
                let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
                let _ = SetForegroundWindow(hwnd);
            }
            let _ = UpdateWindow(hwnd);
        }
        let mut quit = None;
        let mut failed = None;
        let mut msg = MSG::default();
        while !st.done.get() {
            match GetMessageW(&mut msg, None, 0, 0).0 {
                0 => {
                    quit = Some(msg.wParam.0 as i32);
                    break;
                }
                -1 => {
                    failed = Some(windows::core::Error::from_thread());
                    break;
                }
                _ => {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
        if !st.done.get() {
            let _ = DestroyWindow(hwnd);
        }
        if let Some(code) = quit {
            PostQuitMessage(code);
        }
        if let Some(e) = failed {
            anyhow::bail!("run_window message loop failed: {e}");
        }
        Ok(())
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

    /// Every `(hwnd, ms)` the backend's `retime` was asked for.
    pub(super) static RETIMES: std::sync::Mutex<Vec<(isize, u64)>> = std::sync::Mutex::new(Vec::new());

    use crate::wind::test_window_lock as windows_lock;

    /// Resize factors applied to the initial outer rect (pairwise distinct,
    /// DPI independent: `min` is 1x1 in the test spec).
    const RESIZES: [f32; 3] = [1.1, 1.25, 0.9];

    /// Records what `run_window` delivers; ignores the first close request
    /// (the window must stay open) and closes on the second. On the first
    /// close a helper thread resizes and repaints the window (cross-thread,
    /// so the messages arrive through this loop, never re-entrantly inside
    /// a driver callback), then posts the second WM_CLOSE.
    struct Closer {
        hwnd: HWND,
        fb: PixBuf,
        created: bool,
        first: Option<&'static str>,
        first_size: Option<(u32, u32)>,
        dpi: u32,
        size: (u32, u32),
        resizes: u32,
        closes: u32,
        frames: u32,
        quit: bool,
        helper: Option<std::thread::JoinHandle<()>>,
    }

    impl Driver for Closer {
        fn on_create(&mut self, hwnd: HWND) {
            self.hwnd = hwnd;
            self.created = true;
            unsafe {
                self.dpi = GetDpiForWindow(hwnd);
                let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
        fn on_event(&mut self, ev: Ev) -> bool {
            self.first.get_or_insert(match ev {
                Ev::Resize(..) => "resize",
                _ => "other",
            });
            match ev {
                Ev::Resize(w, h) => {
                    self.resizes += 1;
                    self.first_size.get_or_insert((w, h));
                    self.size = (w, h);
                    self.fb = PixBuf::new(w, h);
                }
                Ev::Close => {
                    self.closes += 1;
                    if self.closes == 1 {
                        let raw = self.hwnd.0 as usize;
                        self.helper = Some(std::thread::spawn(move || unsafe {
                            use windows::Win32::Graphics::Gdi::{RedrawWindow, RDW_INVALIDATE, RDW_UPDATENOW};
                            let hwnd = HWND(raw as *mut core::ffi::c_void);
                            let mut r = RECT::default();
                            let _ = GetWindowRect(hwnd, &mut r);
                            let (ow, oh) = ((r.right - r.left) as f32, (r.bottom - r.top) as f32);
                            for f in RESIZES {
                                let (w, h) = ((ow * f).round() as i32, (oh * f).round() as i32);
                                let _ = SetWindowPos(hwnd, None, 0, 0, w, h, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
                                let _ = RedrawWindow(Some(hwnd), None, None, RDW_INVALIDATE | RDW_UPDATENOW);
                            }
                            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                        }));
                    } else {
                        close(self.hwnd);
                    }
                }
                _ => {}
            }
            false
        }
        fn frame(&mut self) -> Option<&mut PixBuf> {
            self.frames += 1;
            self.fb.as_raw_mut().fill(0x80);
            Some(&mut self.fb)
        }
        fn cursor(&self) -> Cursor {
            Cursor::Arrow
        }
        fn on_quit(&mut self) {
            self.quit = true;
        }
    }

    fn run_closer() -> Closer {
        let mut d = Closer {
            hwnd: HWND::default(),
            fb: PixBuf::new(1, 1),
            created: false,
            first: None,
            first_size: None,
            dpi: 0,
            size: (0, 0),
            resizes: 0,
            closes: 0,
            frames: 0,
            quit: false,
            helper: None,
        };
        let spec = WindowSpec {
            title: "rustshot wind test".into(),
            w: 400,
            h: 300,
            resizable: true,
            min: (1, 1),
        };
        run_window(spec, &mut d).expect("window loop");
        if let Some(h) = d.helper.take() {
            h.join().expect("resize helper");
        }
        d
    }

    /// `run_window` on a non-main thread: the loop survives an ignored
    /// WM_CLOSE, ends when the driver closes, paints at least once, and
    /// repeated windows leave the GDI/USER object counts where they were.
    /// The counts are per process and every other test in this one may
    /// create GDI objects at any time, so they are taken in a child
    /// process running only `window_leak_probe`.
    #[test]
    fn run_window_closes_on_request_without_leaks() {
        let _guard = windows_lock();
        std::thread::spawn(|| {
            let d = run_closer();
            assert!(d.created);
            assert_eq!(d.first, Some("resize"), "first event is the initial size");
            let first = d.first_size.expect("initial Resize");
            let s = d.dpi.max(96) as f32 / 96.0;
            assert!(
                first.0 as f32 >= (400.0 * s).floor() && first.1 as f32 >= (300.0 * s).floor(),
                "initial client {first:?} at scale {s}"
            );
            assert_eq!(d.closes, 2, "the first close was ignored");
            assert_eq!(d.resizes, 1 + RESIZES.len() as u32, "initial size plus each SetWindowPos");
            assert!(d.quit, "on_quit ran");
            assert!(d.frames > RESIZES.len() as u32, "painted after every resize: {} frames", d.frames);
            assert!(unsafe { !IsWindow(Some(d.hwnd)).as_bool() }, "window destroyed");
        })
        .join()
        .expect("window thread");
        let here = module_path!().split_once("::").map_or(module_path!(), |m| m.1);
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([&format!("{here}::window_leak_probe"), "--exact", "--ignored", "--nocapture", "--test-threads=1"])
            .env("RUSTSHOT_LEAK_PROBE", "1")
            .output()
            .expect("run the leak probe");
        let text = String::from_utf8_lossy(&out.stdout);
        println!("{text}");
        assert!(out.status.success(), "leak probe failed:\n{text}\n{}", String::from_utf8_lossy(&out.stderr));
        assert!(text.contains("1 passed"), "the probe ran:\n{text}");
    }

    /// Run alone in a child process by the test above: five windows in a
    /// row leave the GDI/USER object counts where they were (three rounds:
    /// the first window may allocate process-wide caches).
    #[test]
    #[ignore = "helper: run as a child process by run_window_closes_on_request_without_leaks"]
    fn window_leak_probe() {
        use windows::Win32::System::Threading::{
            GetGuiResources, OpenProcess, GR_GDIOBJECTS, GR_USEROBJECTS, PROCESS_QUERY_INFORMATION,
        };
        if std::env::var_os("RUSTSHOT_LEAK_PROBE").is_none() {
            return;
        }
        // A real handle: the GetCurrentProcess pseudo handle reads 0.
        let me = unsafe { OpenProcess(PROCESS_QUERY_INFORMATION, false, std::process::id()) }.expect("process handle");
        let count = || unsafe { (GetGuiResources(me, GR_GDIOBJECTS), GetGuiResources(me, GR_USEROBJECTS)) };
        let mut seen = Vec::new();
        let ok = (0..3).any(|_| {
            let before = count();
            for _ in 0..5 {
                let d = run_closer();
                assert_eq!(d.resizes, 1 + RESIZES.len() as u32);
            }
            let after = count();
            seen.push((before, after));
            after.0 <= before.0 && after.1 <= before.1
        });
        let _ = unsafe { windows::Win32::Foundation::CloseHandle(me) };
        println!("(gdi, user) objects before -> after 5 windows: {seen:?}");
        assert!(ok, "GDI/USER objects grew over 5 windows ((gdi, user) before -> after): {seen:?}");
    }

    /// Two `run_window` threads at once: the "overlay-style" window goes
    /// fast, the dialog goes fast then idle again; each keeps its own
    /// cadence (the dialog going idle must not slow the other window).
    #[test]
    fn run_window_cadence_is_per_window() {
        use std::sync::mpsc;
        use std::time::{Duration, Instant};
        let _guard = windows_lock();
        RETIMES.lock().unwrap_or_else(|e| e.into_inner()).clear();
        struct Ticker {
            hwnd: HWND,
            fb: PixBuf,
            idle_again: bool,
            ready: mpsc::Sender<()>,
            go: mpsc::Receiver<()>,
            cadence: u64,
            start: Option<Instant>,
            ticks: u32,
        }
        impl Driver for Ticker {
            fn on_create(&mut self, hwnd: HWND) {
                self.hwnd = hwnd;
                set_fast_timer(hwnd, true);
                if self.idle_again {
                    set_fast_timer(hwnd, false);
                }
                // Both windows have toggled once the main thread says go;
                // a window that never reports makes the test fail, not hang.
                let _ = self.ready.send(());
                let _ = self.go.recv_timeout(Duration::from_secs(5));
                self.cadence = tick_ms(hwnd);
                self.start = Some(Instant::now());
            }
            fn on_event(&mut self, ev: Ev) -> bool {
                if let Ev::Timer = ev {
                    self.ticks += 1;
                    if self.start.is_some_and(|s| s.elapsed() >= Duration::from_millis(450)) {
                        close(self.hwnd);
                    }
                }
                false
            }
            fn frame(&mut self) -> Option<&mut PixBuf> {
                Some(&mut self.fb)
            }
            fn cursor(&self) -> Cursor {
                Cursor::Arrow
            }
        }
        let (ready_tx, ready_rx) = mpsc::channel();
        let mut gos = Vec::new();
        let (hw_tx, hw_rx) = mpsc::channel();
        let mut spawn = |idle_again: bool| {
            let (go_tx, go_rx) = mpsc::channel();
            gos.push(go_tx);
            let (ready, hw_tx) = (ready_tx.clone(), hw_tx.clone());
            std::thread::spawn(move || {
                let mut d = Ticker {
                    hwnd: HWND::default(),
                    fb: PixBuf::new(1, 1),
                    idle_again,
                    ready,
                    go: go_rx,
                    cadence: 0,
                    start: None,
                    ticks: 0,
                };
                let title = if idle_again { "rustshot dialog" } else { "rustshot animating" };
                let spec = WindowSpec { title: title.into(), w: 200, h: 150, resizable: false, min: (0, 0) };
                run_window(spec, &mut d).expect("window loop");
                let _ = hw_tx.send((idle_again, d.hwnd.0 as isize));
                (d.cadence, d.ticks, tick_ms(d.hwnd))
            })
        };
        let (fast, dialog) = (spawn(false), spawn(true));
        drop(hw_tx);
        for i in 0..2 {
            ready_rx.recv_timeout(Duration::from_secs(5)).unwrap_or_else(|_| panic!("window {i} never became ready"));
        }
        for g in &gos {
            let _ = g.send(());
        }
        let (fast, dialog) = (fast.join().expect("fast window"), dialog.join().expect("dialog"));
        let hwnds: std::collections::HashMap<bool, isize> = hw_rx.try_iter().collect();
        let log = RETIMES.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let of = |h: isize| log.iter().filter(|r| r.0 == h).map(|r| r.1).collect::<Vec<_>>();
        assert_eq!(of(hwnds[&true]), [FAST_TICK_MS, SLOW_TICK_MS], "dialog retimed fast then slow");
        assert_eq!(of(hwnds[&false]), [FAST_TICK_MS], "the animating window was retimed once, never slowed");
        assert!(log.iter().all(|r| r.0 == hwnds[&true] || r.0 == hwnds[&false]), "no other window retimed: {log:?}");
        println!("(cadence, ticks in 450 ms, cadence after destroy): animating {fast:?}, dialog {dialog:?}");
        assert_eq!(fast.0, FAST_TICK_MS, "the dialog going idle left the other window fast");
        assert_eq!(dialog.0, SLOW_TICK_MS);
        assert!(fast.1 >= dialog.1 + 2, "fast window ticked {} times, dialog {}", fast.1, dialog.1);
        assert_eq!((fast.2, dialog.2), (SLOW_TICK_MS, SLOW_TICK_MS), "destroyed windows are forgotten");
    }

    /// Interactive: a 400x300 window rendering a filled rect; close it
    /// with its close button or Esc. `RUSTSHOT_TEST_AUTOCLOSE_MS=1500`
    /// samples the rect's centre pixel from the window DC, then posts
    /// WM_CLOSE (the close-button path) instead of waiting for a human.
    /// `cargo test interactive_window -- --ignored --nocapture`
    #[test]
    #[ignore = "interactive: opens a window"]
    fn interactive_window_renders_rect() {
        use windows::Win32::Graphics::Gdi::{GetDC, GetPixel, ReleaseDC};
        const FILL: [u8; 4] = [64, 128, 255, 255];
        struct Rect {
            hwnd: HWND,
            fb: PixBuf,
            deadline: Option<std::time::Instant>,
            sampled: Option<u32>,
            painted: bool,
        }
        impl Driver for Rect {
            fn on_create(&mut self, hwnd: HWND) {
                self.hwnd = hwnd;
                let icon = unsafe { GetClassLongPtrW(hwnd, GCLP_HICON) } != 0;
                println!("created, scale {}, class icon from the exe resource: {icon}", scale(hwnd));
            }
            fn on_event(&mut self, ev: Ev) -> bool {
                match ev {
                    Ev::Resize(w, h) => {
                        println!("resize {w}x{h}");
                        self.fb = PixBuf::new(w, h);
                    }
                    Ev::Focus(f) => println!("focus {f}"),
                    Ev::Key { vk: key::ESCAPE, up: false, .. } | Ev::Close => close(self.hwnd),
                    Ev::Timer
                        if self.painted
                            && self.sampled.is_none()
                            && self.deadline.is_some_and(|d| std::time::Instant::now() >= d) =>
                    {
                        let (w, h) = self.fb.dimensions();
                        unsafe {
                            let dc = GetDC(Some(self.hwnd));
                            self.sampled = Some(GetPixel(dc, w as i32 / 2, h as i32 / 2).0);
                            ReleaseDC(Some(self.hwnd), dc);
                            let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                        }
                    }
                    _ => {}
                }
                false
            }
            fn frame(&mut self) -> Option<&mut PixBuf> {
                let (w, h) = self.fb.dimensions();
                for (i, p) in self.fb.as_raw_mut().as_chunks_mut::<4>().0.iter_mut().enumerate() {
                    let (x, y) = (i as u32 % w, i as u32 / w);
                    let inside = x >= w / 4 && x < w * 3 / 4 && y >= h / 4 && y < h * 3 / 4;
                    *p = if inside { FILL } else { [24, 24, 28, 255] };
                }
                self.painted = true;
                Some(&mut self.fb)
            }
            fn cursor(&self) -> Cursor {
                Cursor::Arrow
            }
        }
        let deadline = std::env::var("RUSTSHOT_TEST_AUTOCLOSE_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .map(|ms| std::time::Instant::now() + std::time::Duration::from_millis(ms));
        let mut d = Rect { hwnd: HWND::default(), fb: PixBuf::new(1, 1), deadline, sampled: None, painted: false };
        let spec = WindowSpec { title: "rustshot window test".into(), w: 400, h: 300, resizable: true, min: (200, 150) };
        let _guard = windows_lock();
        run_window(spec, &mut d).expect("window loop");
        assert!(d.painted);
        if deadline.is_some() {
            // COLORREF is 0x00BBGGRR.
            let want = u32::from_le_bytes([FILL[0], FILL[1], FILL[2], 0]);
            println!("centre pixel {:#08x?}, want {want:#08x}", d.sampled);
            assert_eq!(d.sampled, Some(want));
        }
    }
}
