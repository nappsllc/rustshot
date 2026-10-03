//! macOS global hotkeys: Carbon `RegisterEventHotKey`, but registration and
//! dispatch run on the *main* thread through `wind_macos::HotkeyHook` —
//! registered hotkeys only ever dispatch on the process event loop
//! (`ReceiveNextEvent` on any other thread times out forever), and
//! `hotkey.rs` always starts this module on a dedicated parsing thread.
//! So this thread parks (specs, Sender) and publishes the hook; the pump
//! thread does the Carbon work, exactly like `hotkey_win`'s thread owns the
//! queue instead of the toolkit.

use super::*;

use crate::wind::{publish_hotkeys, HotkeyHook};
use core::ffi::c_void;
use std::sync::Mutex;

// --- Carbon FFI -----------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
struct EventTypeSpec {
    class_: u32,
    kind: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct EventHotKeyID {
    signature: u32,
    id: u32,
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn GetApplicationEventTarget() -> *mut c_void;
    fn GetEventDispatcherTarget() -> *mut c_void;
    fn InstallEventHandler(
        target: *mut c_void,
        handler: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> i32,
        num_types: u64,
        types: *const EventTypeSpec,
        user_data: *mut c_void,
        out_ref: *mut *mut c_void,
    ) -> i32;
    fn RemoveEventHandler(handler_ref: *mut c_void) -> i32;
    fn RegisterEventHotKey(
        key_code: u32,
        modifiers: u32,
        id: EventHotKeyID,
        target: *mut c_void,
        options: u32,
        out_ref: *mut *mut c_void,
    ) -> i32;
    fn UnregisterEventHotKey(hotkey_ref: *mut c_void) -> i32;
    fn ReceiveNextEvent(
        num_types: u64,
        list: *const EventTypeSpec,
        timeout: f64,
        pull: u8, // Boolean
        out_event: *mut *mut c_void,
    ) -> i32;
    fn SendEventToEventTarget(event: *mut c_void, target: *mut c_void) -> i32;
    fn GetEventParameter(
        event: *mut c_void,
        name: u32,
        type_: u32,
        out_type: *mut u32,
        size: u64,
        out_size: *mut u64,
        out_data: *mut c_void,
    ) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    // EventRef is a CFType; `ReleaseEvent` may be a macro, CFRelease is not.
    fn CFRelease(cf: *const c_void);
}

// --- Carbon constants (FourCCs are stable ABI values) --------------------

/// 'rsth' — tags our EventHotKeyIDs (and every event we register).
const SIGNATURE: u32 = 0x7273_7468;
/// 'keyb' (kEventClassKeyboard).
const KEY_CLASS: u32 = 0x6b65_7962;
/// kEventHotKeyPressed within the keyboard class.
const HOT_PRESSED: u32 = 5;
/// '----' (kEventParamDirectObject).
const PARAM_DIRECT: u32 = 0x2d2d_2d2d;
/// 'hkid' (typeEventHotKeyID).
const TYPE_HKID: u32 = 0x686b_6964;
/// eventNotHandledErr — tell the dispatcher another handler may try.
const NOT_HANDLED: i32 = -9874;

// --- process-wide state ---------------------------------------------------

/// (specs, Sender) parked by the parsing thread; consumed by `install` on
/// the pump thread. The Sender must live somewhere until then, which is why
/// the thread publishes instead of registering itself.
static PENDING: Mutex<Option<([(i32, String, HotEvent); 2], mpsc::Sender<HotEvent>)>> =
    Mutex::new(None);
/// Live registration, from `install` until wind's loop calls `shutdown`.
/// The box keeps the handler's `user_data` pointer stable.
static CTX: Mutex<Option<Box<Ctx>>> = Mutex::new(None);

struct Ctx {
    map: Vec<(u32, HotEvent)>,
    tx: mpsc::Sender<HotEvent>,
    handler: usize,
    refs: Vec<usize>,
}

// --- parsing thread -------------------------------------------------------

pub fn hotkey_thread(specs: [(i32, String, HotEvent); 2], tx: mpsc::Sender<HotEvent>) {
    // Hand everything to the main-loop pump and exit: registration must run
    // on the process event loop, so there is nothing left for this thread.
    *PENDING.lock().unwrap_or_else(|e| e.into_inner()) = Some((specs, tx));
    publish_hotkeys(HotkeyHook {
        install,
        pump,
        shutdown,
    });
}

// --- pump-thread lifecycle ------------------------------------------------

/// Parse, register the handler and the hotkeys. Runs once on the pump
/// thread (wind calls it the first pass the hook exists).
fn install() {
    let Some((specs, tx)) = PENDING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
    else {
        return;
    };
    unsafe {
        let target = GetApplicationEventTarget();
        if target.is_null() {
            eprintln!("warning: no application event target; global hotkeys disabled");
            return; // tx drops → Hotkeys::poll yields None, like win without registrations
        }

        // Phase 1: parse on this thread so the dispatch map is complete
        // before the handler exists.
        let mut regs: Vec<(u32, u32, i32, &str)> = Vec::new();
        let mut map: Vec<(u32, HotEvent)> = Vec::new();
        for (id, spec, ev) in &specs {
            match parse_hotkey(spec) {
                Some((mods, vk)) => {
                    let Some(key) = mac_keycode(vk) else {
                        eprintln!("warning: hotkey {spec:?} has no macOS equivalent");
                        continue;
                    };
                    regs.push((key as u32, carbon_mods(mods), *id, spec.as_str()));
                    map.push((*id as u32, *ev));
                }
                None => eprintln!("warning: invalid hotkey {spec:?}"),
            }
        }

        // Phase 2: handler first — queued hotkey events find it when the
        // pump starts dispatching; the ctx moves into CTX afterwards (the
        // heap address the handler holds does not move with the Box).
        let mut ctx = Box::new(Ctx {
            map,
            tx,
            handler: 0,
            refs: Vec::new(),
        });
        let user = (&mut *ctx as *mut Ctx).cast::<c_void>();
        let spec = EventTypeSpec {
            class_: KEY_CLASS,
            kind: HOT_PRESSED,
        };
        let mut handler: *mut c_void = core::ptr::null_mut();
        let status = InstallEventHandler(target, hotkey_handler, 1, &spec, user, &mut handler);
        if status != 0 {
            eprintln!("warning: could not install hotkey handler: OSStatus {status}");
            return;
        }
        ctx.handler = handler as usize;

        // Phase 3: register. Same failure reporting as hotkey_win.
        for (key, mods, id, spec) in regs {
            let mut href: *mut c_void = core::ptr::null_mut();
            let status = RegisterEventHotKey(
                key,
                mods,
                EventHotKeyID {
                    signature: SIGNATURE,
                    id: id as u32,
                },
                target,
                0,
                &mut href,
            );
            if status != 0 {
                eprintln!("warning: could not register hotkey {spec:?}: OSStatus {status}");
            } else {
                ctx.refs.push(href as usize);
            }
        }
        *CTX.lock().unwrap_or_else(|e| e.into_inner()) = Some(ctx);
    }
}

/// Drain Carbon's queue without blocking; wind's pump calls this every pass
/// *before* the Cocoa drain so hotkey events are never swallowed there.
fn pump() {
    unsafe {
        let filter = EventTypeSpec {
            class_: KEY_CLASS,
            kind: HOT_PRESSED,
        };
        let mut ev: *mut c_void = core::ptr::null_mut();
        // 0.0 = kEventDurationNoWait: wind's loop owns the blocking wait.
        // The filter keeps NSEvents queued for `nextEventMatchingMask`.
        while ReceiveNextEvent(1, &filter, 0.0, 1, &mut ev) == 0 {
            if ev.is_null() {
                break;
            }
            SendEventToEventTarget(ev, GetEventDispatcherTarget());
            CFRelease(ev); // ReceiveNextEvent returns +1
            ev = core::ptr::null_mut();
        }
    }
}

/// Unregister and close the channel (wind calls this when the loop ends).
/// Dropping the ctx/Sender ends the pipe exactly like hotkey_win's thread
/// finishes once its receiver is gone.
fn shutdown() {
    let ctx = CTX
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    if let Some(ctx) = ctx {
        unsafe {
            for r in &ctx.refs {
                UnregisterEventHotKey(*r as *mut c_void);
            }
            if ctx.handler != 0 {
                RemoveEventHandler(ctx.handler as *mut c_void);
            }
        }
    }
}

// --- dispatch -------------------------------------------------------------

/// Carbon handler installed on the application event target: read which of
/// our hotkeys fired (`hkid` parameter) and forward the mapped event.
unsafe extern "C" fn hotkey_handler(
    _next: *mut c_void,
    event: *mut c_void,
    user_data: *mut c_void,
) -> i32 {
    unsafe {
        let ctx = &*(user_data as *const Ctx);
        let mut id = EventHotKeyID::default();
        let status = GetEventParameter(
            event,
            PARAM_DIRECT,
            TYPE_HKID,
            core::ptr::null_mut(), // outActualType: not interested
            core::mem::size_of::<EventHotKeyID>() as u64,
            core::ptr::null_mut(), // outActualSize: buffer size is exact
            (&raw mut id).cast::<c_void>(),
        );
        if status != 0 {
            return NOT_HANDLED;
        }
        match ctx.map.iter().find(|(k, _)| *k == id.id) {
            Some((_, ev)) => {
                let _ = ctx.tx.send(*ev); // receiver gone = app exiting
                0
            }
            None => NOT_HANDLED,
        }
    }
}

// --- key mapping ----------------------------------------------------------

/// Win32 modifier flags from `parse_hotkey` (MOD_WIN=8, MOD_SHIFT=4,
/// MOD_ALT=1, MOD_CONTROL=2) → Carbon `EventHotKeyModifiers`
/// (cmdKey=256, shiftKey=512, optionKey=2048, controlKey=4096).
fn carbon_mods(mods: u32) -> u32 {
    let mut out = 0;
    if mods & 0x0008 != 0 {
        out |= 1 << 8; // Meta/Cmd — the mac "Win" key
    }
    if mods & 0x0004 != 0 {
        out |= 1 << 9; // Shift
    }
    if mods & 0x0001 != 0 {
        out |= 1 << 11; // Alt → Option
    }
    if mods & 0x0002 != 0 {
        out |= 1 << 12; // Control
    }
    out
}

/// Win32 virtual key (the shared `key_vk` numbering) → hardware keycode
/// (`kVK_*` from Events.h); None = cannot be registered on this keyboard.
fn mac_keycode(vk: u32) -> Option<u16> {
    let code = match vk {
        // Letters (physical layout, hence the gaps).
        0x41 => 0x00, // A
        0x53 => 0x01, // S
        0x44 => 0x02, // D
        0x46 => 0x03, // F
        0x48 => 0x04, // H
        0x47 => 0x05, // G
        0x5A => 0x06, // Z
        0x58 => 0x07, // X
        0x43 => 0x08, // C
        0x56 => 0x09, // V
        0x42 => 0x0B, // B
        0x51 => 0x0C, // Q
        0x57 => 0x0D, // W
        0x45 => 0x0E, // E
        0x52 => 0x0F, // R
        0x59 => 0x10, // Y
        0x54 => 0x11, // T
        0x4F => 0x1F, // O
        0x55 => 0x20, // U
        0x49 => 0x22, // I
        0x50 => 0x23, // P
        0x4C => 0x25, // L
        0x4A => 0x26, // J
        0x4B => 0x28, // K
        0x4E => 0x2D, // N
        0x4D => 0x2E, // M
        // Digit row.
        0x30 => 0x1D, // 0
        0x31 => 0x12, // 1
        0x32 => 0x13, // 2
        0x33 => 0x14, // 3
        0x34 => 0x15, // 4
        0x35 => 0x17, // 5
        0x36 => 0x16, // 6
        0x37 => 0x1A, // 7
        0x38 => 0x1C, // 8
        0x39 => 0x19, // 9
        // Function row (parse_hotkey only emits F1..F12).
        0x70 => 0x7A, // F1
        0x71 => 0x78, // F2
        0x72 => 0x63, // F3
        0x73 => 0x76, // F4
        0x74 => 0x60, // F5
        0x75 => 0x61, // F6
        0x76 => 0x62, // F7
        0x77 => 0x64, // F8
        0x78 => 0x65, // F9
        0x79 => 0x6D, // F10
        0x7A => 0x67, // F11
        0x7B => 0x6F, // F12
        // Named keys `key_vk` can return.
        key::SPACE => 0x31,
        key::RETURN => 0x24,
        key::ESCAPE => 0x35,
        key::TAB => 0x30,
        key::BACK => 0x33,
        key::DELETE => 0x75,
        key::INSERT => 0x72, // Help key
        key::HOME => 0x73,
        key::END => 0x77,
        key::PAGEUP => 0x74,
        key::PAGEDOWN => 0x79,
        key::UP => 0x7E,
        key::DOWN => 0x7D,
        key::LEFT => 0x7B,
        key::RIGHT => 0x7C,
        // No PrintScreen on mac keyboards: F13 plays the role.
        key::PRINTSCREEN => 0x69,
        _ => return None,
    };
    Some(code)
}
