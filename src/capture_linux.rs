//! Linux capture stubs; real X11/Wayland implementation comes later.

use super::*;

pub fn enable_dpi_awareness() {}

pub fn monitors() -> Result<Vec<MonInfo>> {
    Err(anyhow!("not implemented on this platform"))
}

pub fn gdi_capture(x: i32, y: i32, w: u32, h: u32) -> Result<PixBuf> {
    let _ = (x, y, w, h);
    Err(anyhow!("not implemented on this platform"))
}

pub fn cursor_pos() -> (i32, i32) {
    (0, 0)
}

pub fn focus_our_window() {}
