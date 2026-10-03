//! X11 capture: the root window is the single monitor, `XGetImage` replaces
//! the GDI blit, plus pointer position and focus for the overlay.

use super::*;

use core::ffi::{c_char, c_int, c_uint, c_ulong, c_void};
use std::sync::atomic::Ordering;

// The head of XImage (ABI-stable); pixel data is converted per pixbuf.rs
// conventions (RGBA, alpha 255), like capture_win does for GDI's BGRA.
#[repr(C)]
struct XImage {
    width: c_int,
    height: c_int,
    xoffset: c_int,
    format: c_int,
    data: *mut u8,
    byte_order: c_int,
    bitmap_unit: c_int,
    bitmap_bit_order: c_int,
    bitmap_pad: c_int,
    depth: c_int,
    bytes_per_line: c_int,
    bits_per_pixel: c_int,
    red_mask: c_ulong,
    green_mask: c_ulong,
    blue_mask: c_ulong,
}

// Same Xlib symbols as wind/hotkey/export declare, with this module's own
// XImage layout (ABI-identical) — see wind_linux.rs for the rationale.
#[allow(clashing_extern_declarations)]
#[link(name = "X11")]
unsafe extern "C" {
    fn XOpenDisplay(name: *const c_char) -> *mut c_void;
    fn XCloseDisplay(dpy: *mut c_void) -> c_int;
    fn XDefaultScreen(dpy: *mut c_void) -> c_int;
    fn XRootWindow(dpy: *mut c_void, screen: c_int) -> c_ulong;
    fn XDisplayWidth(dpy: *mut c_void, screen: c_int) -> c_int;
    fn XDisplayHeight(dpy: *mut c_void, screen: c_int) -> c_int;
    fn XGetImage(
        dpy: *mut c_void,
        drawable: c_ulong,
        x: c_int,
        y: c_int,
        width: c_uint,
        height: c_uint,
        plane_mask: c_ulong,
        format: c_int,
    ) -> *mut XImage;
    fn XDestroyImage(image: *mut XImage) -> c_int;
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
    fn XRaiseWindow(dpy: *mut c_void, w: c_ulong) -> c_int;
    fn XSetInputFocus(dpy: *mut c_void, w: c_ulong, revert_to: c_int, time: c_ulong) -> c_int;
    fn XFlush(dpy: *mut c_void) -> c_int;
}

const ALL_PLANES: c_ulong = !0u64;
const Z_PIXMAP: c_int = 2;
const REVERT_TO_PARENT: c_int = 2;
const CURRENT_TIME: c_ulong = 0;

/// X11 clients always see physical pixels; there is no per-process DPI
/// awareness switch to make.
pub fn enable_dpi_awareness() {}

/// One `MonInfo` covering the whole root window: X11 has no multi-monitor
/// concept below the XRandR level, and every coordinate the app uses is
/// root-relative anyway. Scale is 1.0 (physical pixels).
pub fn monitors() -> Result<Vec<MonInfo>> {
    crate::wind::init_x11();
    unsafe {
        let dpy = XOpenDisplay(core::ptr::null());
        if dpy.is_null() {
            return Err(anyhow!("cannot open X display"));
        }
        let screen = XDefaultScreen(dpy);
        let w = XDisplayWidth(dpy, screen);
        let h = XDisplayHeight(dpy, screen);
        XCloseDisplay(dpy);
        if w <= 0 || h <= 0 {
            return Err(anyhow!("no monitors found"));
        }
        Ok(vec![MonInfo {
            x: 0,
            y: 0,
            w: w as u32,
            h: h as u32,
            scale: 1.0,
            primary: true,
        }])
    }
}

/// Capture a root-window rect with `XGetImage` (the `BitBlt` equivalent).
pub fn gdi_capture(x: i32, y: i32, w: u32, h: u32) -> Result<PixBuf> {
    if w == 0 || h == 0 {
        return Err(anyhow!("empty capture region"));
    }
    crate::wind::init_x11();
    unsafe {
        let dpy = XOpenDisplay(core::ptr::null());
        if dpy.is_null() {
            return Err(anyhow!("cannot open X display"));
        }
        let root = XRootWindow(dpy, XDefaultScreen(dpy));
        let img = XGetImage(dpy, root, x, y, w as c_uint, h as c_uint, ALL_PLANES, Z_PIXMAP);
        if img.is_null() {
            XCloseDisplay(dpy);
            return Err(anyhow!("XGetImage failed (region outside the screen?)"));
        }
        let out = image_to_pixbuf(img);
        XDestroyImage(img);
        XCloseDisplay(dpy);
        out
    }
}

/// Convert a ZPixmap XImage into an RGBA `PixBuf` (alpha forced to 255,
/// same contract as capture_win's GDI conversion).
fn image_to_pixbuf(img: *mut XImage) -> Result<PixBuf> {
    unsafe {
        let (iw, ih) = ((*img).width, (*img).height);
        let data = (*img).data;
        if iw <= 0 || ih <= 0 || data.is_null() {
            return Err(anyhow!("bad XImage"));
        }
        let (w, h) = (iw as u32, ih as u32);
        let stride = (*img).bytes_per_line as usize;
        let bpp = (*img).bits_per_pixel;
        if bpp != 8 && bpp != 16 && bpp != 32 {
            return Err(anyhow!("unsupported XImage bits per pixel {bpp}"));
        }
        let bytes_pp = (bpp / 8) as usize;
        let len = stride * h as usize;
        let data = core::slice::from_raw_parts(data, len);
        let masks = (
            (*img).red_mask as u32,
            (*img).green_mask as u32,
            (*img).blue_mask as u32,
        );
        let little = (*img).byte_order == 0; // LSBFirst
        let mut out = vec![0u8; (w as usize) * (h as usize) * 4];
        for row in 0..h as usize {
            for col in 0..w as usize {
                let off = row * stride + col * bytes_pp;
                let pix = match bpp {
                    32 => {
                        let b: [u8; 4] = data[off..off + 4].try_into().unwrap();
                        if little {
                            u32::from_le_bytes(b)
                        } else {
                            u32::from_be_bytes(b)
                        }
                    }
                    16 => {
                        let b: [u8; 2] = data[off..off + 2].try_into().unwrap();
                        if little {
                            u16::from_le_bytes(b) as u32
                        } else {
                            u16::from_be_bytes(b) as u32
                        }
                    }
                    _ => data[off] as u32,
                };
                let i = (row * w as usize + col) * 4;
                out[i] = channel(pix, masks.0);
                out[i + 1] = channel(pix, masks.1);
                out[i + 2] = channel(pix, masks.2);
                out[i + 3] = 255; // no meaningful alpha in screen pixels
            }
        }
        PixBuf::from_raw(w, h, out).ok_or_else(|| anyhow!("bad capture buffer"))
    }
}

/// Extract one 0..=255 channel from a packed pixel given its bit mask.
fn channel(pix: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 0;
    }
    let shift = mask.trailing_zeros();
    let bits = mask.count_ones();
    let v = (pix & mask) >> shift;
    if bits >= 8 {
        (v >> (bits - 8)) as u8
    } else {
        ((v * 255) / ((1 << bits) - 1)) as u8
    }
}

/// Pointer position in root (global) coordinates.
pub fn cursor_pos() -> (i32, i32) {
    crate::wind::init_x11();
    unsafe {
        let dpy = XOpenDisplay(core::ptr::null());
        if dpy.is_null() {
            return (0, 0);
        }
        let root = XRootWindow(dpy, XDefaultScreen(dpy));
        let (mut rw, mut cw) = (0 as c_ulong, 0 as c_ulong);
        let (mut rx, mut ry, mut wx, mut wy) = (0 as c_int, 0 as c_int, 0 as c_int, 0 as c_int);
        let mut mask = 0 as c_uint;
        let _ = XQueryPointer(
            dpy,
            root,
            &mut rw,
            &mut cw,
            &mut rx,
            &mut ry,
            &mut wx,
            &mut wy,
            &mut mask,
        );
        XCloseDisplay(dpy);
        (rx, ry)
    }
}

/// Bring the overlay to the front so it receives keyboard input (called a few
/// times right after `show_at`, mirroring capture_win's foreground retry).
/// Runs on the pump thread; a short-lived private connection keeps it legal
/// without sharing the pump's Display across threads.
pub fn focus_our_window() {
    let win = crate::wind::MAIN_WINDOW.load(Ordering::SeqCst);
    if win == 0 {
        return;
    }
    crate::wind::init_x11();
    unsafe {
        let dpy = XOpenDisplay(core::ptr::null());
        if dpy.is_null() {
            return;
        }
        XRaiseWindow(dpy, win);
        XSetInputFocus(dpy, win, REVERT_TO_PARENT, CURRENT_TIME);
        XFlush(dpy);
        XCloseDisplay(dpy);
    }
}
