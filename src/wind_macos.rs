//! macOS overlay stubs; real CoreGraphics implementation comes later.

use super::*;

/// Ask for a repaint of the window's client area.
pub fn invalidate(_hwnd: Hwnd) {}

/// Position + show the overlay at an exact physical rect and take focus.
pub fn show_at(_hwnd: Hwnd, _x: i32, _y: i32, _w: i32, _h: i32) {}

pub fn hide(_hwnd: Hwnd) {}

pub fn close(_hwnd: Hwnd) {}

/// Create the (initially hidden) overlay and pump messages until quit.
/// Returns when the window is destroyed.
pub fn run(driver: &mut dyn Driver) -> i32 {
    let _ = driver;
    // No window exists yet; reference the interface (never invoke it) so the
    // rest of the crate's call graph stays live for dead-code analysis.
    let _iface: (
        fn(&mut (dyn Driver + 'static), Hwnd),
        fn(&mut (dyn Driver + 'static), Ev),
        fn(&mut (dyn Driver + 'static)) -> Option<PixBuf>,
        fn(&(dyn Driver + 'static)) -> Cursor,
        fn(&mut (dyn Driver + 'static)),
    ) = (
        <dyn Driver>::on_create,
        <dyn Driver>::on_event,
        <dyn Driver>::frame,
        <dyn Driver>::cursor,
        <dyn Driver>::on_quit,
    );
    // Only the Windows pump builds events; construct each variant once so
    // stub builds don't report them as dead code.
    let _events = [
        Ev::Move { x: 0, y: 0 },
        Ev::Down { x: 0, y: 0 },
        Ev::Up { x: 0, y: 0 },
        Ev::Wheel { delta: 0, x: 0, y: 0 },
        Ev::Key {
            vk: 0,
            up: false,
            repeat: false,
            mods: Mods::default(),
        },
        Ev::Char(0),
        Ev::Timer,
    ];
    1
}
