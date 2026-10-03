//! Hand-rolled Win32 overlay window: class registration, message pump,
//! input event delivery, and framebuffer presentation (StretchDIBits).

use crate::pixbuf::PixBuf;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BITMAPINFO, BI_RGB, DIB_RGB_COLORS, EndPaint, InvalidateRect, PAINTSTRUCT,
    ScreenToClient, SRCCOPY, StretchDIBits,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, VK_CONTROL, VK_MENU, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::*;

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

impl Mods {
    pub fn current() -> Self {
        unsafe {
            Mods {
                shift: GetKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000 != 0,
                ctrl: GetKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000 != 0,
                alt: GetKeyState(VK_MENU.0 as i32) as u16 & 0x8000 != 0,
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Ev {
    Move { x: i32, y: i32 },
    Down { x: i32, y: i32 },
    Up { x: i32, y: i32 },
    Wheel { delta: i32, x: i32, y: i32 },
    Key { vk: u32, up: bool, repeat: bool, mods: Mods },
    Char(u16),
    Timer,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cursor {
    Arrow,
    Cross,
    IBeam,
    SizeNS,
    SizeWE,
    SizeNWSE,
    SizeNESW,
    Move,
}

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

pub trait Driver {
    /// Window created; stash the handle.
    fn on_create(&mut self, hwnd: HWND);
    /// Input/timer event (client coordinates).
    fn on_event(&mut self, ev: Ev);
    /// Compose the current frame as unpremultiplied RGBA, top-down.
    fn frame(&mut self) -> Option<PixBuf>;
    /// Cursor for WM_SETCURSOR.
    fn cursor(&self) -> Cursor;
    /// Window is being destroyed (loop ends after this).
    fn on_quit(&mut self) {}
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
                invalidate(hwnd);
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let _ = ReleaseCapture();
                drv.on_event(Ev::Up {
                    x: x_of(lp),
                    y: y_of(lp),
                });
                invalidate(hwnd);
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
                invalidate(hwnd);
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
                invalidate(hwnd);
                LRESULT(0)
            }
            WM_CHAR => {
                drv.on_event(Ev::Char(wp.0 as u16));
                invalidate(hwnd);
                LRESULT(0)
            }
            WM_TIMER => {
                drv.on_event(Ev::Timer);
                invalidate(hwnd);
                LRESULT(0)
            }
            WM_PAINT => {
                let mut ps: PAINTSTRUCT = std::mem::zeroed();
                let hdc = BeginPaint(hwnd, &mut ps);
                if let Some(fb) = drv.frame() {
                    present(hdc, &fb);
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

/// Present an unpremultiplied RGBA framebuffer by converting to top-down BGRA.
fn present(hdc: windows::Win32::Graphics::Gdi::HDC, fb: &PixBuf) {
    let (w, h) = fb.dimensions();
    if w == 0 || h == 0 {
        return;
    }
    let mut bgra = fb.as_raw().clone();
    for c in bgra.chunks_exact_mut(4) {
        c.swap(0, 2);
    }
    let mut bmi: BITMAPINFO = unsafe { std::mem::zeroed() };
    bmi.bmiHeader.biSize = std::mem::size_of::<windows::Win32::Graphics::Gdi::BITMAPINFOHEADER>()
        as u32;
    bmi.bmiHeader.biWidth = w as i32;
    bmi.bmiHeader.biHeight = -(h as i32); // top-down
    bmi.bmiHeader.biPlanes = 1;
    bmi.bmiHeader.biBitCount = 32;
    bmi.bmiHeader.biCompression = BI_RGB.0;
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
            Some(bgra.as_ptr() as *const _),
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
                let _ = SetTimer(Some(hwnd), 1, 150, None);
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

    struct QuitSoon {
        created: bool,
    }

    impl Driver for QuitSoon {
        fn on_create(&mut self, hwnd: HWND) {
            self.created = true;
            show_at(hwnd, 0, 0, 320, 240);
            close(hwnd);
        }
        fn on_event(&mut self, _ev: Ev) {}
        fn frame(&mut self) -> Option<PixBuf> {
            Some(PixBuf::new(320, 240))
        }
        fn cursor(&self) -> Cursor {
            Cursor::Arrow
        }
    }

    #[test]
    #[ignore = "live display access"]
    fn live_window_lifecycle() {
        let mut d = QuitSoon { created: false };
        let code = run(&mut d);
        assert!(d.created);
        assert_eq!(code, 0);
    }
}
