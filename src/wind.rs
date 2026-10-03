//! Overlay window surface shared by every platform: input events, modifier
//! state, the framebuffer driver trait, and virtual-key constants.
//! Per-OS implementations live in `wind_win.rs` / `wind_linux.rs` /
//! `wind_macos.rs`.

use crate::pixbuf::PixBuf;

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

#[cfg(windows)]
impl Mods {
    pub fn current() -> Self {
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            GetKeyState, VK_CONTROL, VK_MENU, VK_SHIFT,
        };
        unsafe {
            Mods {
                shift: GetKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000 != 0,
                ctrl: GetKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000 != 0,
                alt: GetKeyState(VK_MENU.0 as i32) as u16 & 0x8000 != 0,
            }
        }
    }
}

#[cfg(target_os = "linux")]
impl Mods {
    // TODO: read X11 pointer modifier state (XQueryPointer).
    pub fn current() -> Self {
        Mods::default()
    }
}

#[cfg(target_os = "macos")]
impl Mods {
    // TODO: read CGEventSourceFlagsState modifier state.
    pub fn current() -> Self {
        Mods::default()
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

#[cfg(windows)]
pub type Hwnd = windows::Win32::Foundation::HWND;

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Hwnd(pub u64);

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Hwnd(pub usize);

pub trait Driver {
    /// Window created; stash the handle.
    fn on_create(&mut self, hwnd: Hwnd);
    /// Input/timer event (client coordinates).
    fn on_event(&mut self, ev: Ev);
    /// Compose the current frame as unpremultiplied RGBA, top-down.
    fn frame(&mut self) -> Option<PixBuf>;
    /// Cursor for WM_SETCURSOR.
    fn cursor(&self) -> Cursor;
    /// Window is being destroyed (loop ends after this).
    fn on_quit(&mut self) {}
}

/// Virtual-key codes (Win32 numbering, shared by every platform backend).
pub mod key {
    pub const BACK: u32 = 0x08;
    pub const TAB: u32 = 0x09;
    pub const RETURN: u32 = 0x0D;
    pub const ESCAPE: u32 = 0x1B;
    pub const SPACE: u32 = 0x20;
    pub const PAGEUP: u32 = 0x21;
    pub const PAGEDOWN: u32 = 0x22;
    pub const END: u32 = 0x23;
    pub const HOME: u32 = 0x24;
    pub const LEFT: u32 = 0x25;
    pub const UP: u32 = 0x26;
    pub const RIGHT: u32 = 0x27;
    pub const DOWN: u32 = 0x28;
    pub const DELETE: u32 = 0x2E;
    pub const INSERT: u32 = 0x2D;
    pub const PRINTSCREEN: u32 = 0x2C;
}

#[cfg(windows)]
#[path = "wind_win.rs"]
mod imp;
#[cfg(target_os = "linux")]
#[path = "wind_linux.rs"]
mod imp;
#[cfg(target_os = "macos")]
#[path = "wind_macos.rs"]
mod imp;

pub use imp::*;

#[cfg(test)]
mod tests {
    use super::*;

    struct QuitSoon {
        created: bool,
    }

    impl Driver for QuitSoon {
        fn on_create(&mut self, hwnd: Hwnd) {
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
