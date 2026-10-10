//! X11 overlay window: override-redirect surface, event pump (Key/Button/
//! Motion/Expose → `Ev`), variable timer cadence (`wind::tick_ms`) and framebuffer presentation.
//! X11/Xorg only — Wayland has no client-side override-redirect overlay.
//! Also the decorated `run_window` (dialogs/Settings), on its own Display.

use super::*;

use core::cell::Cell;
use core::ffi::{c_char, c_int, c_long, c_uint, c_ulong, c_void};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Once;
use std::time::{Duration, Instant};

// No `x11` crate: libX11 is declared directly (edition 2024 needs `unsafe`
// around extern blocks). `cargo check` does not link, so this works without
// libX11-dev installed. The same symbols are declared in capture/hotkey/
// export with their own module-private (ABI-identical) event layouts, which
// trips clashing_extern_declarations by design — hence the allow.
#[allow(clashing_extern_declarations)]
#[link(name = "X11")]
unsafe extern "C" {
    fn XOpenDisplay(name: *const c_char) -> *mut c_void;
    fn XCloseDisplay(dpy: *mut c_void) -> c_int;
    fn XInitThreads() -> c_int;
    fn XConnectionNumber(dpy: *mut c_void) -> c_int;
    fn XDefaultScreen(dpy: *mut c_void) -> c_int;
    fn XRootWindow(dpy: *mut c_void, screen: c_int) -> c_ulong;
    fn XQueryPointer(
        dpy: *mut c_void,
        w: c_ulong,
        root_return: *mut c_ulong,
        child_return: *mut c_ulong,
        root_x: *mut c_int,
        root_y: *mut c_int,
        win_x: *mut c_int,
        win_y: *mut c_int,
        mask_return: *mut c_uint,
    ) -> c_int;
    fn XDefaultVisual(dpy: *mut c_void, screen: c_int) -> *mut c_void;
    fn XDefaultDepth(dpy: *mut c_void, screen: c_int) -> c_int;
    fn XCreateWindow(
        dpy: *mut c_void,
        parent: c_ulong,
        x: c_int,
        y: c_int,
        width: c_uint,
        height: c_uint,
        border_width: c_uint,
        depth: c_int,
        class: c_uint,
        visual: *mut c_void,
        valuemask: c_ulong,
        attributes: *mut XSetWindowAttributes,
    ) -> c_ulong;
    fn XDestroyWindow(dpy: *mut c_void, w: c_ulong) -> c_int;
    fn XMapRaised(dpy: *mut c_void, w: c_ulong) -> c_int;
    fn XUnmapWindow(dpy: *mut c_void, w: c_ulong) -> c_int;
    fn XMoveResizeWindow(
        dpy: *mut c_void,
        w: c_ulong,
        x: c_int,
        y: c_int,
        width: c_uint,
        height: c_uint,
    ) -> c_int;
    fn XSetInputFocus(dpy: *mut c_void, w: c_ulong, revert_to: c_int, time: c_ulong) -> c_int;
    fn XFlush(dpy: *mut c_void) -> c_int;
    fn XPending(dpy: *mut c_void) -> c_int;
    fn XNextEvent(dpy: *mut c_void, event: *mut XEvent) -> c_int;
    fn XPeekEvent(dpy: *mut c_void, event: *mut XEvent) -> c_int;
    fn XkbKeycodeToKeysym(
        dpy: *mut c_void,
        keycode: u8,
        group: c_uint,
        level: c_uint,
    ) -> c_ulong;
    fn XCreateGC(
        dpy: *mut c_void,
        d: c_ulong,
        valuemask: c_ulong,
        values: *mut c_void,
    ) -> *mut c_void;
    fn XFreeGC(dpy: *mut c_void, gc: *mut c_void) -> c_int;
    fn XCreateImage(
        dpy: *mut c_void,
        visual: *mut c_void,
        depth: c_uint,
        format: c_int,
        offset: c_int,
        data: *mut c_char,
        width: c_uint,
        height: c_uint,
        bitmap_pad: c_int,
        bytes_per_line: c_int,
    ) -> *mut c_void;
    fn XPutImage(
        dpy: *mut c_void,
        d: c_ulong,
        gc: *mut c_void,
        image: *mut c_void,
        src_x: c_int,
        src_y: c_int,
        dest_x: c_int,
        dest_y: c_int,
        width: c_uint,
        height: c_uint,
    ) -> c_int;
    fn XDestroyImage(image: *mut c_void) -> c_int;
    fn XCreateFontCursor(dpy: *mut c_void, shape: c_uint) -> c_ulong;
    fn XDefineCursor(dpy: *mut c_void, w: c_ulong, cursor: c_ulong) -> c_int;
    fn XGrabPointer(
        dpy: *mut c_void,
        w: c_ulong,
        owner_events: c_int,
        event_mask: c_uint,
        pointer_mode: c_int,
        keyboard_mode: c_int,
        confine_to: c_ulong,
        cursor: c_ulong,
        time: c_ulong,
    ) -> c_int;
    fn XUngrabPointer(dpy: *mut c_void, time: c_ulong) -> c_int;
}

#[repr(C)]
struct PollFd {
    fd: c_int,
    events: i16,
    revents: i16,
}

unsafe extern "C" {
    fn poll(fds: *mut PollFd, nfds: u64, timeout: c_int) -> c_int;
}

// --- X.h constants (values are ABI-stable) --------------------------------

const KEY_PRESS_MASK: c_ulong = 1 << 0;
const KEY_RELEASE_MASK: c_ulong = 1 << 1;
const BUTTON_PRESS_MASK: c_ulong = 1 << 2;
const BUTTON_RELEASE_MASK: c_ulong = 1 << 3;
const POINTER_MOTION_MASK: c_ulong = 1 << 6;
const BUTTON_MOTION_MASK: c_ulong = 1 << 13;
const EXPOSURE_MASK: c_ulong = 1 << 15;
const STRUCTURE_NOTIFY_MASK: c_ulong = 1 << 17;
const FOCUS_CHANGE_MASK: c_ulong = 1 << 21;

const CW_BACK_PIXEL: c_ulong = 1 << 1;
const CW_OVERRIDE_REDIRECT: c_ulong = 1 << 9;
const CW_EVENT_MASK: c_ulong = 1 << 11;

const KEY_PRESS: c_int = 2;
const KEY_RELEASE: c_int = 3;
const BUTTON_PRESS: c_int = 4;
const BUTTON_RELEASE: c_int = 5;
const MOTION_NOTIFY: c_int = 6;
const EXPOSE: c_int = 12;

const SHIFT_MASK: c_uint = 1 << 0;
const LOCK_MASK: c_uint = 1 << 1;
const CONTROL_MASK: c_uint = 1 << 2;
const MOD1_MASK: c_uint = 1 << 3;

const GRAB_MODE_ASYNC: c_int = 1;
const REVERT_TO_PARENT: c_int = 2;
const CURRENT_TIME: c_ulong = 0;
const Z_PIXMAP: c_int = 2;
const INPUT_OUTPUT: c_uint = 1;

const POLLIN: i16 = 1;
const POLLERR: i16 = 8;
const POLLHUP: i16 = 16;
const POLLNVAL: i16 = 32;

const EVENT_MASK: c_ulong = KEY_PRESS_MASK
    | KEY_RELEASE_MASK
    | BUTTON_PRESS_MASK
    | BUTTON_RELEASE_MASK
    | POINTER_MOTION_MASK
    | BUTTON_MOTION_MASK
    | EXPOSURE_MASK
    | STRUCTURE_NOTIFY_MASK
    | FOCUS_CHANGE_MASK;

// --- Xlib structures ------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
struct XSetWindowAttributes {
    background_pixmap: c_ulong,
    background_pixel: c_ulong,
    border_pixmap: c_ulong,
    border_pixel: c_ulong,
    bit_gravity: c_int,
    win_gravity: c_int,
    backing_store: c_int,
    backing_planes: c_ulong,
    backing_pixel: c_ulong,
    save_under: c_int,
    event_mask: c_long,
    do_not_propagate_mask: c_long,
    override_redirect: c_int,
    colormap: c_ulong,
    cursor: c_ulong,
}

/// XKeyEvent, XButtonEvent and XMotionEvent share this layout (the field
/// after `state` is keycode/button/is_hint); we only ever read x, y, state,
/// keycode and time, which line up in all three.
#[repr(C)]
#[derive(Clone, Copy)]
struct XKeyButtonEvent {
    type_: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: *mut c_void,
    window: c_ulong,
    root: c_ulong,
    subwindow: c_ulong,
    time: c_ulong,
    x: c_int,
    y: c_int,
    x_root: c_int,
    y_root: c_int,
    state: c_uint,
    detail: c_uint,
    same_screen: c_int,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct XExposeEvent {
    type_: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: *mut c_void,
    window: c_ulong,
    x: c_int,
    y: c_int,
    width: c_int,
    height: c_int,
    count: c_int,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct XConfigureEvent {
    type_: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: *mut c_void,
    event: c_ulong,
    window: c_ulong,
    x: c_int,
    y: c_int,
    width: c_int,
    height: c_int,
    border_width: c_int,
    above: c_ulong,
    override_redirect: c_int,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct XClientMessageEvent {
    type_: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: *mut c_void,
    window: c_ulong,
    message_type: c_ulong,
    format: c_int,
    data: [c_long; 5],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct XFocusChangeEvent {
    type_: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: *mut c_void,
    window: c_ulong,
    mode: c_int,
    detail: c_int,
}

#[repr(C)]
#[derive(Clone, Copy)]
union XEvent {
    type_: c_int,
    key: XKeyButtonEvent,
    expose: XExposeEvent,
    configure: XConfigureEvent,
    client: XClientMessageEvent,
    focus: XFocusChangeEvent,
    pad: [c_ulong; 24],
}

// --- process-wide state ---------------------------------------------------

/// Set by `close` (always inside a driver callback on the pump thread, like
/// `DestroyWindow` on Win32); an atomic also makes a cross-thread close safe.
static QUIT: AtomicBool = AtomicBool::new(false);
/// Set by `invalidate`/`show_at`, consumed once per loop iteration.
static PRESENT: AtomicBool = AtomicBool::new(false);
/// Window of the running overlay (0 = none); read by `capture_linux`.
pub static MAIN_WINDOW: AtomicU64 = AtomicU64::new(0);

static XLIB_INIT: Once = Once::new();

/// Xlib must learn about threads before its first call: every thread here
/// (pump, hotkeys, clipboard) opens its own Display, but Xlib still keeps
/// global state between connections. Must run before any other Xlib call.
pub fn init_x11() {
    XLIB_INIT.call_once(|| unsafe {
        XInitThreads();
    });
}

/// Live modifier state via `XQueryPointer` (ShiftMask=1, ControlMask=4,
/// Mod1Mask=8). Only called from driver callbacks on the pump thread, whose
/// thread-local display is therefore open; any other caller sees defaults.
pub fn current_mods() -> Mods {
    RUN_DPY.with(|slot| {
        let dpy = slot.get();
        if dpy.is_null() {
            return Mods::default();
        }
        unsafe {
            let root = XRootWindow(dpy, XDefaultScreen(dpy));
            let mut mask = 0u32;
            if XQueryPointer(
                dpy,
                root,
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                &mut mask,
            ) == 0
            {
                return Mods::default();
            }
            Mods {
                shift: mask & 1 != 0,
                ctrl: mask & 4 != 0,
                alt: mask & 8 != 0,
            }
        }
    })
}

std::thread_local! {
    /// Display of the thread running `run()`. `show_at`/`hide` are only ever
    /// called from driver callbacks on that thread (mirrors Win32, where the
    /// window handle is enough because everything shares one message queue).
    static RUN_DPY: Cell<*mut c_void> = const { Cell::new(core::ptr::null_mut()) };
}

// --- public API -----------------------------------------------------------

/// Whether `hwnd` is the running overlay's window (not 0, not stale).
fn is_overlay(hwnd: Hwnd) -> bool {
    hwnd.0 != 0 && MAIN_WINDOW.load(Ordering::SeqCst) == hwnd.0
}

/// Ask for a repaint of the window's client area. An unknown or stale
/// handle is a no-op.
pub fn invalidate(hwnd: Hwnd) {
    if !with_slot(hwnd, |s| s.present = true) && is_overlay(hwnd) {
        PRESENT.store(true, Ordering::SeqCst);
    }
}

/// The event loops read `tick_ms(win)` each pass; nothing to re-arm.
pub fn retime(_hwnd: Hwnd, _ms: u64) {}

/// Position + show the overlay at an exact physical rect and take focus.
pub fn show_at(hwnd: Hwnd, x: i32, y: i32, w: i32, h: i32) {
    RUN_DPY.with(|slot| {
        let dpy = slot.get();
        if dpy.is_null() {
            return;
        }
        unsafe {
            XMoveResizeWindow(dpy, hwnd.0, x, y, w as c_uint, h as c_uint);
            XMapRaised(dpy, hwnd.0);
            XSetInputFocus(dpy, hwnd.0, REVERT_TO_PARENT, CURRENT_TIME);
            XFlush(dpy);
        }
    });
    // The server answers a map with Expose, but force a frame as well so a
    // re-positioned window repaints even when no Expose arrives.
    PRESENT.store(true, Ordering::SeqCst);
}

pub fn hide(hwnd: Hwnd) {
    RUN_DPY.with(|slot| {
        let dpy = slot.get();
        if dpy.is_null() {
            return;
        }
        unsafe {
            XUnmapWindow(dpy, hwnd.0);
            XFlush(dpy);
        }
    });
}

pub fn close(hwnd: Hwnd) {
    // Never destroy the window from here: callers run inside event handling
    // on the pump thread, exactly like DestroyWindow on Win32 (whose effect
    // is also deferred until the current dispatch returns). An unknown or
    // stale handle is a no-op.
    if !with_slot(hwnd, |s| s.quit = true) && is_overlay(hwnd) {
        QUIT.store(true, Ordering::SeqCst);
    }
}

/// Create the (initially hidden) overlay and pump events until quit.
/// Returns when the window is destroyed.
pub fn run(driver: &mut dyn Driver) -> i32 {
    init_x11();
    QUIT.store(false, Ordering::SeqCst);
    PRESENT.store(false, Ordering::SeqCst);
    unsafe {
        let dpy = XOpenDisplay(core::ptr::null());
        if dpy.is_null() {
            eprintln!("failed to open X display");
            return 1;
        }
        let screen = XDefaultScreen(dpy);
        let root = XRootWindow(dpy, screen);
        let mut attrs: XSetWindowAttributes = core::mem::zeroed();
        attrs.background_pixel = 0;
        // override_redirect is a window *attribute*, not a property (it can
        // only be set at creation): it keeps the WM from decorating or
        // listing the window — the X11 twin of WS_EX_TOOLWINDOW|WS_EX_TOPMOST.
        attrs.override_redirect = 1;
        attrs.event_mask = EVENT_MASK as c_long;
        let win = XCreateWindow(
            dpy,
            root,
            0,
            0,
            1,
            1,
            0,
            0,                            // depth: CopyFromParent
            INPUT_OUTPUT,                 // class
            core::ptr::null_mut(),        // visual: CopyFromParent
            CW_BACK_PIXEL | CW_OVERRIDE_REDIRECT | CW_EVENT_MASK,
            &mut attrs,
        );
        let gc = XCreateGC(dpy, win, 0, core::ptr::null_mut());
        RUN_DPY.with(|slot| slot.set(dpy));
        MAIN_WINDOW.store(win, Ordering::SeqCst);
        driver.on_create(Hwnd(win));

        let mut down: HashSet<u32> = HashSet::new();
        let mut cursor: Option<Cursor> = None;
        let mut cursors: [c_ulong; 8] = [0; 8];
        let me = Hwnd(win);
        let mut next_tick = Instant::now() + Duration::from_millis(tick_ms(me));
        loop {
            if QUIT.load(Ordering::SeqCst) {
                break;
            }
            // Honour a slow->fast switch made during the previous pass.
            next_tick = next_tick.min(Instant::now() + Duration::from_millis(tick_ms(me)));
            let mut repaint = false;
            if XPending(dpy) == 0 {
                // Sleep until the next tick (150 ms idle, 16 ms animating)
                // or until X has input.
                let wait = next_tick
                    .saturating_duration_since(Instant::now())
                    .as_micros()
                    .div_ceil(1000) as c_int;
                let mut pfd = PollFd {
                    fd: XConnectionNumber(dpy),
                    events: POLLIN,
                    revents: 0,
                };
                poll(&mut pfd, 1, wait);
                if pfd.revents & (POLLERR | POLLHUP | POLLNVAL) != 0 {
                    break; // X connection lost
                }
            }
            if Instant::now() >= next_tick {
                next_tick = Instant::now() + Duration::from_millis(tick_ms(me));
                // Idle ticks (nothing animating) cost no frame.
                repaint |= driver.on_event(Ev::Timer);
            }
            while XPending(dpy) > 0 && !QUIT.load(Ordering::SeqCst) {
                let mut ev: XEvent = core::mem::zeroed();
                XNextEvent(dpy, &mut ev);
                repaint |= dispatch(dpy, win, &mut ev, driver, &mut down);
            }
            repaint |= PRESENT.swap(false, Ordering::SeqCst);
            if repaint && !QUIT.load(Ordering::SeqCst)
                && let Some(fb) = driver.frame()
            {
                present(dpy, win, gc, fb);
            }
            let want = driver.cursor();
            if cursor != Some(want) {
                set_cursor(dpy, win, want, &mut cursors);
                cursor = Some(want);
            }
        }
        driver.on_quit();
        forget_tick(me);
        XFreeGC(dpy, gc);
        XDestroyWindow(dpy, win);
        XFlush(dpy);
        RUN_DPY.with(|slot| slot.set(core::ptr::null_mut()));
        MAIN_WINDOW.store(0, Ordering::SeqCst);
        XCloseDisplay(dpy);
        0
    }
}

// --- decorated window (run_window) ----------------------------------------

/// Per-thread state of a running `run_window` (win 0 = none). `close`,
/// `invalidate` and `scale` route here for that window, so a dialog never
/// touches the overlay's process-wide QUIT/PRESENT flags.
#[derive(Clone, Copy)]
struct WinSlot {
    win: c_ulong,
    quit: bool,
    present: bool,
    scale: f32,
}

const NO_SLOT: WinSlot = WinSlot { win: 0, quit: false, present: false, scale: 1.0 };

std::thread_local! {
    static WINDOWED: Cell<WinSlot> = const { Cell::new(NO_SLOT) };
}

/// Apply `f` to this thread's `run_window` slot when `hwnd` is that window.
fn with_slot(hwnd: Hwnd, f: impl FnOnce(&mut WinSlot)) -> bool {
    WINDOWED.with(|c| {
        let mut s = c.get();
        if s.win == 0 || s.win != hwnd.0 {
            return false;
        }
        f(&mut s);
        c.set(s);
        true
    })
}

pub use window::{run_window, scale};

/// The decorated window proper (`run_window`: update dialog, Settings).
mod window {
    use super::*;

    #[allow(clashing_extern_declarations)]
    #[link(name = "X11")]
    unsafe extern "C" {
        fn XInternAtom(dpy: *mut c_void, name: *const c_char, only_if_exists: c_int) -> c_ulong;
        fn XChangeProperty(
            dpy: *mut c_void,
            w: c_ulong,
            property: c_ulong,
            type_: c_ulong,
            format: c_int,
            mode: c_int,
            data: *const u8,
            nelements: c_int,
        ) -> c_int;
        fn XSetWMProtocols(dpy: *mut c_void, w: c_ulong, protocols: *mut c_ulong, count: c_int) -> c_int;
        fn XSetWMNormalHints(dpy: *mut c_void, w: c_ulong, hints: *mut XSizeHints);
        fn XSetWMHints(dpy: *mut c_void, w: c_ulong, hints: *mut XWMHints) -> c_int;
        fn XResourceManagerString(dpy: *mut c_void) -> *mut c_char;
    }

    const FOCUS_IN: c_int = 9;
    const FOCUS_OUT: c_int = 10;
    const CONFIGURE_NOTIFY: c_int = 22;
    const CLIENT_MESSAGE: c_int = 33;

    /// DPI scale of a `run_window` window (from `Xft.dpi`, 1.0 = 96 dpi); 1.0
    /// for anything else (the overlay works in physical pixels).
    pub fn scale(hwnd: Hwnd) -> f32 {
        let mut out = 1.0;
        with_slot(hwnd, |s| out = s.scale);
        out
    }

    /// XSizeHints (Xutil.h).
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct XSizeHints {
        flags: c_long,
        x: c_int,
        y: c_int,
        width: c_int,
        height: c_int,
        min_width: c_int,
        min_height: c_int,
        max_width: c_int,
        max_height: c_int,
        width_inc: c_int,
        height_inc: c_int,
        min_aspect: [c_int; 2],
        max_aspect: [c_int; 2],
        base_width: c_int,
        base_height: c_int,
        win_gravity: c_int,
    }

    /// XWMHints (Xutil.h).
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct XWMHints {
        flags: c_long,
        input: c_int,
        initial_state: c_int,
        icon_pixmap: c_ulong,
        icon_window: c_ulong,
        icon_x: c_int,
        icon_y: c_int,
        icon_mask: c_ulong,
        window_group: c_ulong,
    }

    const US_POSITION: c_long = 1 << 0;
    const P_POSITION: c_long = 1 << 2;
    const P_MIN_SIZE: c_long = 1 << 4;
    const P_MAX_SIZE: c_long = 1 << 5;
    const INPUT_HINT: c_long = 1 << 0;
    const XA_ATOM: c_ulong = 4;
    const XA_CARDINAL: c_ulong = 6;
    const PROP_MODE_REPLACE: c_int = 0;
    const CW_BIT_GRAVITY: c_ulong = 1 << 4;
    const NORTH_WEST_GRAVITY: c_int = 1;
    const NOTIFY_GRAB: c_int = 1;
    const NOTIFY_UNGRAB: c_int = 2;
    const NOTIFY_POINTER: c_int = 5;

    /// 48 px app icon for `_NET_WM_ICON` (decoded once per window).
    const ICON_PNG: &[u8] = include_bytes!("../packaging/icons/rustshot-48.png");

    unsafe fn atom(dpy: *mut c_void, name: &core::ffi::CStr) -> c_ulong {
        unsafe { XInternAtom(dpy, name.as_ptr(), 0) }
    }

    /// Format-32 property data travels as C longs (64-bit on LP64).
    unsafe fn set_prop32(dpy: *mut c_void, win: c_ulong, prop: c_ulong, ty: c_ulong, data: &[c_ulong]) {
        unsafe {
            XChangeProperty(dpy, win, prop, ty, 32, PROP_MODE_REPLACE, data.as_ptr().cast(), data.len() as c_int);
        }
    }

    /// Monitor rect (x, y, w, h) under the pointer: primary, then any, then
    /// a 1920x1080 guess when enumeration fails entirely.
    fn monitor_under_cursor() -> (i32, i32, i32, i32) {
        let (cx, cy) = crate::capture::cursor_pos();
        let mons = crate::capture::monitors().unwrap_or_default();
        mons.iter()
            .find(|m| m.contains(cx, cy))
            .or_else(|| mons.iter().find(|m| m.primary))
            .or(mons.first())
            .map(|m| (m.x, m.y, m.w as i32, m.h as i32))
            .unwrap_or((0, 0, 1920, 1080))
    }

    /// Open a normal decorated top-level window (WM_DELETE_WINDOW, size hints,
    /// `_NET_WM_NAME`, `_NET_WM_ICON`) centred on the monitor under the
    /// pointer and pump its events until the driver calls `wind::close(hwnd)`.
    /// Callable from any thread: every call opens its own Display connection
    /// (`init_x11` has run `XInitThreads`). The close button only sends
    /// `Ev::Close`. `Ev::Resize` (physical px) follows `on_create` and every
    /// size change; `wind::scale(hwnd)` is `Xft.dpi / 96`.
    pub fn run_window(spec: WindowSpec, driver: &mut dyn Driver) -> anyhow::Result<()> {
        init_x11();
        unsafe {
            let dpy = XOpenDisplay(core::ptr::null());
            if dpy.is_null() {
                anyhow::bail!("failed to open X display");
            }
            let rm = XResourceManagerString(dpy);
            let s = if rm.is_null() {
                None
            } else {
                xft_scale(&core::ffi::CStr::from_ptr(rm).to_string_lossy())
            }
            .unwrap_or(1.0);
            let (pw, ph) = (to_phys(spec.w, s), to_phys(spec.h, s));
            let (x, y) = centre_in(monitor_under_cursor(), pw as i32, ph as i32);
            let screen = XDefaultScreen(dpy);
            let root = XRootWindow(dpy, screen);
            let mut attrs: XSetWindowAttributes = core::mem::zeroed();
            attrs.event_mask = EVENT_MASK as c_long;
            // No background: the server never clears to black before a frame.
            attrs.bit_gravity = NORTH_WEST_GRAVITY;
            let win = XCreateWindow(
                dpy,
                root,
                x,
                y,
                pw,
                ph,
                0,
                0,
                INPUT_OUTPUT,
                core::ptr::null_mut(),
                CW_EVENT_MASK | CW_BIT_GRAVITY,
                &mut attrs,
            );
            let utf8 = atom(dpy, c"UTF8_STRING");
            for prop in [atom(dpy, c"_NET_WM_NAME"), atom(dpy, c"WM_NAME")] {
                XChangeProperty(
                    dpy,
                    win,
                    prop,
                    utf8,
                    8,
                    PROP_MODE_REPLACE,
                    spec.title.as_ptr(),
                    spec.title.len() as c_int,
                );
            }
            if let Some(icon) = icon_argb(ICON_PNG) {
                let data: Vec<c_ulong> = icon.iter().map(|&v| v as c_ulong).collect();
                set_prop32(dpy, win, atom(dpy, c"_NET_WM_ICON"), XA_CARDINAL, &data);
            }
            let normal = atom(dpy, c"_NET_WM_WINDOW_TYPE_NORMAL");
            set_prop32(dpy, win, atom(dpy, c"_NET_WM_WINDOW_TYPE"), XA_ATOM, &[normal]);
            let wm_protocols = atom(dpy, c"WM_PROTOCOLS");
            let mut wm_delete = atom(dpy, c"WM_DELETE_WINDOW");
            XSetWMProtocols(dpy, win, &mut wm_delete, 1);
            let mut hints = XSizeHints { flags: US_POSITION | P_POSITION | P_MIN_SIZE, x, y, ..Default::default() };
            if spec.resizable {
                (hints.min_width, hints.min_height) = (to_phys(spec.min.0, s) as c_int, to_phys(spec.min.1, s) as c_int);
            } else {
                hints.flags |= P_MAX_SIZE;
                (hints.min_width, hints.min_height) = (pw as c_int, ph as c_int);
                (hints.max_width, hints.max_height) = (pw as c_int, ph as c_int);
            }
            XSetWMNormalHints(dpy, win, &mut hints);
            let mut wmh = XWMHints { flags: INPUT_HINT, input: 1, ..Default::default() };
            XSetWMHints(dpy, win, &mut wmh);
            let gc = XCreateGC(dpy, win, 0, core::ptr::null_mut());

            let prev_dpy = RUN_DPY.with(|c| c.replace(dpy));
            let prev_slot = WINDOWED.with(|c| c.replace(WinSlot { win, quit: false, present: false, scale: s }));
            let quit = || WINDOWED.with(|c| c.get().quit);
            let mut size = (pw, ph);
            driver.on_create(Hwnd(win));
            if !quit() {
                driver.on_event(Ev::Resize(pw, ph));
            }
            if !quit() {
                XMapRaised(dpy, win);
                XFlush(dpy);
            }

            let mut lost = false;
            let mut down: HashSet<u32> = HashSet::new();
            let mut cursor: Option<Cursor> = None;
            let mut cursors: [c_ulong; 8] = [0; 8];
            let me = Hwnd(win);
            let mut next_tick = Instant::now() + Duration::from_millis(tick_ms(me));
            while !quit() {
                next_tick = next_tick.min(Instant::now() + Duration::from_millis(tick_ms(me)));
                let mut repaint = false;
                if XPending(dpy) == 0 {
                    let wait = next_tick.saturating_duration_since(Instant::now()).as_micros().div_ceil(1000) as c_int;
                    let mut pfd = PollFd { fd: XConnectionNumber(dpy), events: POLLIN, revents: 0 };
                    poll(&mut pfd, 1, wait);
                    if pfd.revents & (POLLERR | POLLHUP | POLLNVAL) != 0 {
                        lost = true;
                        break;
                    }
                }
                if Instant::now() >= next_tick {
                    next_tick = Instant::now() + Duration::from_millis(tick_ms(me));
                    repaint |= driver.on_event(Ev::Timer);
                }
                while XPending(dpy) > 0 && !quit() {
                    let mut ev: XEvent = core::mem::zeroed();
                    XNextEvent(dpy, &mut ev);
                    match ev.type_ {
                        CLIENT_MESSAGE => {
                            let c = ev.client;
                            if c.message_type == wm_protocols && c.data[0] as c_ulong == wm_delete {
                                driver.on_event(Ev::Close);
                                repaint = true;
                            }
                        }
                        CONFIGURE_NOTIFY => {
                            let c = ev.configure;
                            let now = (c.width.max(1) as u32, c.height.max(1) as u32);
                            if c.window == win && now != size {
                                size = now;
                                driver.on_event(Ev::Resize(now.0, now.1));
                                repaint = true;
                            }
                        }
                        FOCUS_IN | FOCUS_OUT => {
                            let f = ev.focus;
                            if f.mode != NOTIFY_GRAB && f.mode != NOTIFY_UNGRAB && f.detail != NOTIFY_POINTER {
                                driver.on_event(Ev::Focus(ev.type_ == FOCUS_IN));
                                repaint = true;
                            }
                        }
                        _ => repaint |= dispatch(dpy, win, &mut ev, driver, &mut down),
                    }
                }
                let mut present_req = false;
                with_slot(Hwnd(win), |s| present_req = core::mem::take(&mut s.present));
                repaint |= present_req;
                if repaint
                    && !quit()
                    && let Some(fb) = driver.frame()
                {
                    present(dpy, win, gc, fb);
                }
                let want = driver.cursor();
                if cursor != Some(want) {
                    set_cursor(dpy, win, want, &mut cursors);
                    cursor = Some(want);
                }
            }
            driver.on_quit();
            forget_tick(me);
            XFreeGC(dpy, gc);
            XDestroyWindow(dpy, win);
            XFlush(dpy);
            WINDOWED.with(|c| c.set(prev_slot));
            RUN_DPY.with(|c| c.set(prev_dpy));
            XCloseDisplay(dpy);
            if lost {
                anyhow::bail!("X connection lost");
            }
            Ok(())
        }
    }
}

// --- event pump -----------------------------------------------------------

/// Map one X event onto `Ev` calls; returns whether a repaint is due
/// (the wndproc invalidates after every input/timer message, never after
/// WM_MOUSEMOVE).
fn dispatch(
    dpy: *mut c_void,
    win: c_ulong,
    ev: &mut XEvent,
    driver: &mut dyn Driver,
    down: &mut HashSet<u32>,
) -> bool {
    unsafe {
        match ev.type_ {
            MOTION_NOTIFY => {
                driver.on_event(Ev::Move {
                    x: ev.key.x,
                    y: ev.key.y,
                });
                false
            }
            BUTTON_PRESS => {
                let (x, y, button) = (ev.key.x, ev.key.y, ev.key.detail);
                match button {
                    4 | 5 => {
                        // X wheel is buttons 4/5; Win32 delivers one
                        // WM_MOUSEWHEEL per notch with delta ±WHEEL_DELTA (120).
                        let delta = if button == 4 { 120 } else { -120 };
                        driver.on_event(Ev::Wheel { delta, x, y });
                        true
                    }
                    1..=3 => {
                        if button == 1 {
                            // Capture like SetCapture: Up/Motion still arrive
                            // when the pointer leaves the overlay mid-drag.
                            XGrabPointer(
                                dpy,
                                win,
                                0,
                                (BUTTON_PRESS_MASK
                                    | BUTTON_RELEASE_MASK
                                    | POINTER_MOTION_MASK)
                                    as c_uint,
                                GRAB_MODE_ASYNC,
                                GRAB_MODE_ASYNC,
                                0,
                                0,
                                CURRENT_TIME,
                            );
                        }
                        driver.on_event(Ev::Down { x, y });
                        true
                    }
                    _ => false,
                }
            }
            BUTTON_RELEASE => {
                let (x, y, button) = (ev.key.x, ev.key.y, ev.key.detail);
                match button {
                    4 | 5 => false, // wheel is handled on press only
                    1..=3 => {
                        if button == 1 {
                            XUngrabPointer(dpy, CURRENT_TIME);
                        }
                        driver.on_event(Ev::Up { x, y });
                        true
                    }
                    _ => false,
                }
            }
            KEY_PRESS => {
                let keycode = ev.key.detail as u8;
                let state = ev.key.state;
                let base = XkbKeycodeToKeysym(dpy, keycode, 0, 0);
                let vk = vk_of(base);
                let shift = state & SHIFT_MASK != 0;
                let caps = state & LOCK_MASK != 0;
                let alpha = (0x41..=0x5a).contains(&base) || (0x61..=0x7a).contains(&base);
                let level = u32::from(shift ^ (caps && alpha));
                let mut sym = XkbKeycodeToKeysym(dpy, keycode, 0, level);
                if sym == 0 {
                    sym = base;
                }
                let mut repaint = false;
                if vk != 0 {
                    // X11 has no repeat flag: a press while the key is still
                    // tracked (release dropped below, or detectable repeat)
                    // is the auto-repeat.
                    let repeat = down.contains(&vk);
                    down.insert(vk);
                    driver.on_event(Ev::Key {
                        vk,
                        up: false,
                        repeat,
                        mods: mods_of(state),
                    });
                    repaint = true;
                    if let Some(c) = char_of(sym) {
                        driver.on_event(Ev::Char(c));
                        repaint = true;
                    }
                } else if let Some(c) = char_of(sym) {
                    // Unmapped but printable keysym (dead keys, punctuation
                    // on layouts whose base key we don't translate).
                    driver.on_event(Ev::Char(c));
                    repaint = true;
                }
                repaint
            }
            KEY_RELEASE => {
                // Without detectable auto-repeat the server synthesizes
                // Release+Press pairs for held keys; swallow the fake release
                // so the following press reports repeat: true.
                if XPending(dpy) > 0 {
                    let mut next: XEvent = core::mem::zeroed();
                    XPeekEvent(dpy, &mut next);
                    if next.type_ == KEY_PRESS
                        && next.key.detail == ev.key.detail
                        && next.key.time == ev.key.time
                    {
                        return false;
                    }
                }
                let vk = vk_of(XkbKeycodeToKeysym(dpy, ev.key.detail as u8, 0, 0));
                if vk == 0 {
                    return false;
                }
                down.remove(&vk);
                driver.on_event(Ev::Key {
                    vk,
                    up: true,
                    repeat: false,
                    mods: mods_of(ev.key.state),
                });
                true
            }
            // Expose arrives in batches; paint once the last of them is read.
            EXPOSE => ev.expose.count == 0,
            _ => false,
        }
    }
}

fn mods_of(state: c_uint) -> Mods {
    Mods {
        shift: state & SHIFT_MASK != 0,
        ctrl: state & CONTROL_MASK != 0,
        alt: state & MOD1_MASK != 0,
    }
}

/// Keysym → Win32-style virtual key (shared numbering from `wind::key`).
fn vk_of(ks: c_ulong) -> u32 {
    match ks {
        0xff0d => key::RETURN,
        0xff1b => key::ESCAPE,
        0xff08 => key::BACK,
        0xffff => key::DELETE,
        0xff50 => key::HOME,
        0xff57 => key::END,
        0xff55 => key::PAGEUP,
        0xff56 => key::PAGEDOWN,
        0xff51 => key::LEFT,
        0xff52 => key::UP,
        0xff53 => key::RIGHT,
        0xff54 => key::DOWN,
        0xff63 => key::INSERT,
        0xff61 => key::PRINTSCREEN,
        0xff09 => key::TAB,
        0x020 => key::SPACE,
        0xffbe..=0xffc9 => 0x70 + (ks - 0xffbe) as u32, // XK_F1..XK_F12
        0x41..=0x5a => ks as u32,                       // A..Z
        0x61..=0x7a => ks as u32 - 0x20,                // a..z → 'A'..'Z'
        0x30..=0x39 => ks as u32,                       // 0..9
        _ => 0,
    }
}

/// Keysym → `Ev::Char` payload; control keysyms (Return, Tab, …) map to
/// nothing because Win32's WM_CHAR equivalents are filtered by the editor.
fn char_of(ks: c_ulong) -> Option<u16> {
    match ks {
        0x20..=0x7e | 0xa0..=0xff => Some(ks as u16),
        // Unicode keysym: 0x01000000 | codepoint; `Ev::Char` is u16.
        _ if ks & 0xff00_0000 == 0x0100_0000 => {
            let cp = (ks & 0x00ff_ffff) as u32;
            if (0x20..=0xffff).contains(&cp) {
                char::from_u32(cp).map(|c| c as u16)
            } else {
                None
            }
        }
        _ => None,
    }
}

// --- presentation ---------------------------------------------------------

/// Leading fields of Xlib's `XImage` (only `data` is touched).
#[repr(C)]
struct XImageHead {
    width: c_int,
    height: c_int,
    xoffset: c_int,
    format: c_int,
    data: *mut c_char,
}

/// Present an unpremultiplied RGBA framebuffer (converted to B,G,R,X for the
/// server's little-endian 32 bpp ZPixmap format, like StretchDIBits does).
///
/// Converts `fb` in place (no staging copy), so it holds B,G,R,X afterwards.
/// Sound because every repaint calls `Driver::frame()` first, which rewrites
/// the whole buffer; nothing presents the same frame twice.
fn present(dpy: *mut c_void, win: c_ulong, gc: *mut c_void, fb: &mut PixBuf) {
    let (w, h) = fb.dimensions();
    if w == 0 || h == 0 {
        return;
    }
    for p in fb.as_raw_mut().as_chunks_mut::<4>().0.iter_mut() {
        *p = [p[2], p[1], p[0], 255];
    }
    unsafe {
        let screen = XDefaultScreen(dpy);
        // Standard Xorg TrueColor: depth 24 with 32 bpp pads.
        let image = XCreateImage(
            dpy,
            XDefaultVisual(dpy, screen),
            XDefaultDepth(dpy, screen) as c_uint,
            Z_PIXMAP,
            0,
            fb.as_raw_mut().as_mut_ptr() as *mut c_char,
            w as c_uint,
            h as c_uint,
            32,
            (w * 4) as c_int,
        );
        if image.is_null() {
            return;
        }
        XPutImage(dpy, win, gc, image, 0, 0, 0, 0, w as c_uint, h as c_uint);
        // XPutImage copied the pixels synchronously. The buffer stays
        // the frame's: detach it so XDestroyImage frees only the header.
        (*(image as *mut XImageHead)).data = core::ptr::null_mut();
        XDestroyImage(image);
    }
}

/// Map `Cursor` to an XC_* shape from cursorfont.h (cached per run).
fn cursor_shape(c: Cursor) -> (usize, c_uint) {
    match c {
        Cursor::Arrow => (0, 68),     // XC_left_ptr
        Cursor::Cross => (1, 34),     // XC_crosshair
        Cursor::IBeam => (2, 152),    // XC_xterm
        Cursor::SizeNS => (3, 116),   // XC_sb_v_double_arrow
        Cursor::SizeWE => (4, 108),   // XC_sb_h_double_arrow
        Cursor::SizeNWSE => (5, 134), // XC_top_left_corner
        Cursor::SizeNESW => (6, 12),  // XC_bottom_left_corner
        Cursor::Move => (7, 52),      // XC_fleur
    }
}

fn set_cursor(dpy: *mut c_void, win: c_ulong, c: Cursor, cache: &mut [c_ulong; 8]) {
    let (slot, shape) = cursor_shape(c);
    unsafe {
        if cache[slot] == 0 {
            cache[slot] = XCreateFontCursor(dpy, shape);
        }
        XDefineCursor(dpy, win, cache[slot]);
    }
}
