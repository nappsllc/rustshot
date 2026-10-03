//! X11 overlay window: override-redirect surface, event pump (Key/Button/
//! Motion/Expose → `Ev`), 150 ms timer cadence and framebuffer presentation.
//! X11/Xorg only — Wayland has no client-side override-redirect overlay.

use super::*;

use core::cell::Cell;
use core::ffi::{c_char, c_int, c_long, c_uint, c_ulong, c_void};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Once;

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
union XEvent {
    type_: c_int,
    key: XKeyButtonEvent,
    expose: XExposeEvent,
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

/// Ask for a repaint of the window's client area.
pub fn invalidate(_hwnd: Hwnd) {
    PRESENT.store(true, Ordering::SeqCst);
}

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

pub fn close(_hwnd: Hwnd) {
    // Never destroy the window from here: callers run inside event handling
    // on the pump thread, exactly like DestroyWindow on Win32 (whose effect
    // is also deferred until the current dispatch returns).
    QUIT.store(true, Ordering::SeqCst);
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
        loop {
            if QUIT.load(Ordering::SeqCst) {
                break;
            }
            let mut repaint = false;
            if XPending(dpy) == 0 {
                // Same 150 ms cadence as wind_win's SetTimer(hwnd, 1, 150).
                let mut pfd = PollFd {
                    fd: XConnectionNumber(dpy),
                    events: POLLIN,
                    revents: 0,
                };
                let n = poll(&mut pfd, 1, 150);
                if pfd.revents & (POLLERR | POLLHUP | POLLNVAL) != 0 {
                    break; // X connection lost
                }
                if n == 0 {
                    driver.on_event(Ev::Timer);
                    repaint = true; // wndproc invalidates after WM_TIMER
                }
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
                present(dpy, win, gc, &fb);
            }
            let want = driver.cursor();
            if cursor != Some(want) {
                set_cursor(dpy, win, want, &mut cursors);
                cursor = Some(want);
            }
        }
        driver.on_quit();
        XFreeGC(dpy, gc);
        XDestroyWindow(dpy, win);
        XFlush(dpy);
        RUN_DPY.with(|slot| slot.set(core::ptr::null_mut()));
        MAIN_WINDOW.store(0, Ordering::SeqCst);
        XCloseDisplay(dpy);
        0
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

/// Present an unpremultiplied RGBA framebuffer (converted to B,G,R,X for the
/// server's little-endian 32 bpp ZPixmap format, like StretchDIBits does).
fn present(dpy: *mut c_void, win: c_ulong, gc: *mut c_void, fb: &PixBuf) {
    let (w, h) = fb.dimensions();
    if w == 0 || h == 0 {
        return;
    }
    let mut bgrx = fb.as_raw().clone();
    for px in bgrx.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
        px[3] = 255;
    }
    let (len, cap) = (bgrx.len(), bgrx.capacity());
    let data = bgrx.as_mut_ptr();
    std::mem::forget(bgrx);
    unsafe {
        let screen = XDefaultScreen(dpy);
        // Standard Xorg TrueColor: depth 24 with 32 bpp pads.
        let image = XCreateImage(
            dpy,
            XDefaultVisual(dpy, screen),
            XDefaultDepth(dpy, screen) as c_uint,
            Z_PIXMAP,
            0,
            data as *mut c_char,
            w as c_uint,
            h as c_uint,
            32,
            (w * 4) as c_int,
        );
        if image.is_null() {
            drop(Vec::from_raw_parts(data, len, cap));
            return;
        }
        XPutImage(dpy, win, gc, image, 0, 0, 0, 0, w as c_uint, h as c_uint);
        // XPutImage copied the pixels synchronously; XDestroyImage frees the
        // malloc'd buffer together with the XImage (Xlib and Rust's System
        // allocator both use malloc/free here).
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
