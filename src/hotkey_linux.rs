//! X11 global hotkeys: passive grabs (`XGrabKey`) on the root window, served
//! from this dedicated connection — the X equivalent of `RegisterHotKey`.

use super::*;

use core::ffi::{c_char, c_int, c_uint, c_ulong, c_void};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};

// Same Xlib symbols as wind/export declare, with this module's own event
// layout (ABI-identical) — see wind_linux.rs for the rationale.
#[allow(clashing_extern_declarations)]
#[link(name = "X11")]
unsafe extern "C" {
    fn XOpenDisplay(name: *const c_char) -> *mut c_void;
    fn XCloseDisplay(dpy: *mut c_void) -> c_int;
    fn XDefaultScreen(dpy: *mut c_void) -> c_int;
    fn XRootWindow(dpy: *mut c_void, screen: c_int) -> c_ulong;
    fn XKeysymToKeycode(dpy: *mut c_void, keysym: c_ulong) -> u8;
    fn XGrabKey(
        dpy: *mut c_void,
        keycode: c_int,
        modifiers: c_uint,
        grab_window: c_ulong,
        owner_events: c_int,
        pointer_mode: c_int,
        keyboard_mode: c_int,
    ) -> c_int;
    fn XUngrabKey(
        dpy: *mut c_void,
        keycode: c_int,
        modifiers: c_uint,
        grab_window: c_ulong,
    ) -> c_int;
    fn XNextEvent(dpy: *mut c_void, event: *mut XEvent) -> c_int;
    fn XPeekEvent(dpy: *mut c_void, event: *mut XEvent) -> c_int;
    fn XPending(dpy: *mut c_void) -> c_int;
    fn XSync(dpy: *mut c_void, discard: c_int) -> c_int;
    fn XConnectionNumber(dpy: *mut c_void) -> c_int;
    fn XSetErrorHandler(
        handler: Option<unsafe extern "C" fn(*mut c_void, *mut XErrorEvent) -> c_int>,
    ) -> Option<unsafe extern "C" fn(*mut c_void, *mut XErrorEvent) -> c_int>;
}

#[repr(C)]
#[derive(Clone, Copy)]
struct XKeyEvent {
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
struct XEvent {
    type_: c_int,
    key: XKeyEvent,
    pad: [c_ulong; 24],
}

#[repr(C)]
struct XErrorEvent {
    type_: c_int,
    display: *mut c_void,
    resourceid: c_ulong,
    serial: c_ulong,
    error_code: u8,
    major_opcode: u8,
    minor_opcode: u8,
}

const KEY_PRESS: c_int = 2;
const KEY_RELEASE: c_int = 3;

const SHIFT_MASK: c_uint = 1 << 0;
const LOCK_MASK: c_uint = 1 << 1;
const CONTROL_MASK: c_uint = 1 << 2;
const MOD1_MASK: c_uint = 1 << 3;
const MOD2_MASK: c_uint = 1 << 4;
const MOD4_MASK: c_uint = 1 << 6;

const GRAB_MODE_ASYNC: c_int = 1;

/// Modifier bits a hotkey can ask for (everything except CapsLock/NumLock).
const MOD_BASE_MASK: c_uint = SHIFT_MASK | CONTROL_MASK | MOD1_MASK | MOD4_MASK;

/// Set while the temporary X error handler below is installed.
static GRAB_FAILED: AtomicBool = AtomicBool::new(false);

unsafe extern "C" fn record_grab_error(_dpy: *mut c_void, _ev: *mut XErrorEvent) -> c_int {
    GRAB_FAILED.store(true, Ordering::SeqCst);
    0
}

#[repr(C)]
struct PollFd {
    fd: c_int,
    events: i16,
    revents: i16,
}

unsafe extern "C" {
    fn poll(fds: *mut PollFd, nfds: c_ulong, timeout: c_int) -> c_int;
}

/// A running grab thread.
pub struct Worker {
    join: std::thread::JoinHandle<()>,
    stop: std::sync::Arc<AtomicBool>,
}

impl Worker {
    pub fn start(specs: [(i32, String, HotEvent); 2], tx: mpsc::Sender<HotEvent>) -> Worker {
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        let join = std::thread::spawn(move || hotkey_thread(specs, tx, &s));
        Worker { join, stop }
    }

    /// End the thread (its grabs are released) and wait for it.
    pub fn stop(self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.join.join();
    }
}

/// Serve `specs` until the receiver goes away or `stop` is set. Mirrors
/// hotkey_win: register everything, loop on events, warn (never die) when
/// a grab is refused.
fn hotkey_thread(specs: [(i32, String, HotEvent); 2], tx: mpsc::Sender<HotEvent>, stop: &AtomicBool) {
    crate::wind::init_x11();
    unsafe {
        let dpy = XOpenDisplay(core::ptr::null());
        if dpy.is_null() {
            eprintln!("warning: cannot open X display; global hotkeys disabled");
            return;
        }
        let root = XRootWindow(dpy, XDefaultScreen(dpy));

        // A refused grab (BadAccess: e.g. the desktop already owns
        // PrintScreen) is an asynchronous protocol error, and Xlib's default
        // handler exits the process — install a recording one while grabbing.
        let prev = XSetErrorHandler(Some(record_grab_error));
        // (keycode, modifiers, index into specs)
        let mut grabs: Vec<(u8, c_uint, usize)> = Vec::new();
        for (i, (_, spec, _)) in specs.iter().enumerate() {
            if spec.trim().is_empty() {
                continue; // no hotkey for this action
            }
            let Some((mods, vk)) = parse_hotkey(spec) else {
                eprintln!("warning: invalid hotkey {spec:?}");
                continue;
            };
            let Some(sym) = keysym_of_vk(vk) else {
                eprintln!("warning: unsupported hotkey key in {spec:?}");
                continue;
            };
            let keycode = XKeysymToKeycode(dpy, sym);
            if keycode == 0 {
                eprintln!("warning: could not register hotkey {spec:?}: key not on this keyboard");
                continue;
            }
            let base = x_mods(mods);
            // Grab every CapsLock/NumLock combination so the hotkey still
            // fires with either lock held.
            let mut refused = false;
            for extra in [0, LOCK_MASK, MOD2_MASK, LOCK_MASK | MOD2_MASK] {
                let combo = base | extra;
                GRAB_FAILED.store(false, Ordering::SeqCst);
                XGrabKey(
                    dpy,
                    keycode as c_int,
                    combo,
                    root,
                    0,
                    GRAB_MODE_ASYNC,
                    GRAB_MODE_ASYNC,
                );
                XSync(dpy, 0); // errors are async: flush before checking
                if GRAB_FAILED.load(Ordering::SeqCst) {
                    eprintln!("warning: could not register hotkey {spec:?} (already grabbed?)");
                    refused = true;
                    break;
                }
                grabs.push((keycode, combo, i));
            }
            if refused {
                // Roll back this hotkey's partial grabs.
                for (k, m, idx) in grabs.iter() {
                    if *idx == i {
                        XUngrabKey(dpy, *k as c_int, *m, root);
                    }
                }
                grabs.retain(|&(_, _, idx)| idx != i);
            }
        }
        XSetErrorHandler(prev);

        // Passive grabs deliver KeyPress/KeyRelease here. `held` drops the
        // server's auto-repeat press/release pairs (MOD_NOREPEAT parity).
        let mut held: HashSet<u8> = HashSet::new();
        let fd = XConnectionNumber(dpy);
        loop {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            if XPending(dpy) == 0 {
                // Wait for the connection, waking now and then for `stop`.
                let mut p = PollFd { fd, events: 1, revents: 0 }; // POLLIN
                poll(&mut p, 1, 200);
                continue;
            }
            let mut ev: XEvent = core::mem::zeroed();
            XNextEvent(dpy, &mut ev);
            let keycode = ev.key.detail as u8;
            match ev.type_ {
                KEY_PRESS => {
                    let state = ev.key.state & MOD_BASE_MASK;
                    let Some((_, _, idx)) = grabs
                        .iter()
                        .find(|(k, m, _)| *k == keycode && (*m & MOD_BASE_MASK) == state)
                    else {
                        continue;
                    };
                    if !held.insert(keycode) {
                        continue; // repeat while held
                    }
                    if tx.send(specs[*idx].2.clone()).is_err() {
                        break; // receiver dropped
                    }
                }
                KEY_RELEASE => {
                    // A release immediately followed by a press of the same
                    // keycode is auto-repeat, not a real key-up: keep it in
                    // `held` so the repeat cannot refire the hotkey.
                    if XPending(dpy) > 0 {
                        let mut next: XEvent = core::mem::zeroed();
                        XPeekEvent(dpy, &mut next);
                        if next.type_ == KEY_PRESS && next.key.detail as u8 == keycode {
                            continue;
                        }
                    }
                    held.remove(&keycode);
                }
                _ => {}
            }
        }
        for (keycode, combo, _) in grabs {
            XUngrabKey(dpy, keycode as c_int, combo, root);
        }
        XCloseDisplay(dpy);
    }
}

/// Win32 `MOD_*` flags (from `parse_hotkey`) → X11 modifier masks.
fn x_mods(mods: u32) -> c_uint {
    let mut m = 0;
    if mods & 0x0001 != 0 {
        m |= MOD1_MASK; // Alt → Mod1
    }
    if mods & 0x0002 != 0 {
        m |= CONTROL_MASK;
    }
    if mods & 0x0004 != 0 {
        m |= SHIFT_MASK;
    }
    if mods & 0x0008 != 0 {
        m |= MOD4_MASK; // Win/Super → Mod4
    }
    m
}

/// Win32 virtual key → X keysym (so `XKeysymToKeycode` can pick the key).
fn keysym_of_vk(vk: u32) -> Option<c_ulong> {
    Some(match vk {
        0x41..=0x5a => vk as c_ulong,        // XK_A..XK_Z
        0x30..=0x39 => vk as c_ulong,        // XK_0..XK_9
        0x70..=0x7b => (0xffbe + (vk - 0x70)) as c_ulong, // XK_F1..XK_F12
        k if k == key::SPACE => 0x020,
        k if k == key::RETURN => 0xff0d,
        k if k == key::ESCAPE => 0xff1b,
        k if k == key::TAB => 0xff09,
        k if k == key::BACK => 0xff08,
        k if k == key::DELETE => 0xffff,
        k if k == key::INSERT => 0xff63,
        k if k == key::HOME => 0xff50,
        k if k == key::END => 0xff57,
        k if k == key::PAGEUP => 0xff55,
        k if k == key::PAGEDOWN => 0xff56,
        k if k == key::UP => 0xff52,
        k if k == key::DOWN => 0xff54,
        k if k == key::LEFT => 0xff51,
        k if k == key::RIGHT => 0xff53,
        k if k == key::PRINTSCREEN => 0xff61,
        _ => return None,
    })
}
