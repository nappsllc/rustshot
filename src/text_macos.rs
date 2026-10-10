//! macOS annotation text: CoreText (Helvetica, with the system cascade list
//! for other scripts). Each line is a `CTLine` drawn into an 8-bit gray
//! `CGBitmapContext`; the gray level is the coverage.

use crate::objects::Pt;
use std::ffi::c_void;

type Ref = *const c_void;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFTypeDictionaryKeyCallBacks: u8;
    static kCFTypeDictionaryValueCallBacks: u8;
    static kCFBooleanTrue: Ref;
    fn CFRelease(cf: Ref);
    fn CFStringCreateWithBytes(alloc: Ref, bytes: *const u8, len: isize, enc: u32, external: u8) -> Ref;
    fn CFDictionaryCreate(
        alloc: Ref,
        keys: *const Ref,
        values: *const Ref,
        n: isize,
        key_cb: *const u8,
        value_cb: *const u8,
    ) -> Ref;
    fn CFAttributedStringCreate(alloc: Ref, s: Ref, attrs: Ref) -> Ref;
}

#[link(name = "CoreText", kind = "framework")]
unsafe extern "C" {
    static kCTFontAttributeName: Ref;
    static kCTForegroundColorFromContextAttributeName: Ref;
    fn CTFontCreateWithName(name: Ref, size: f64, matrix: *const c_void) -> Ref;
    fn CTFontGetAscent(font: Ref) -> f64;
    fn CTFontGetDescent(font: Ref) -> f64;
    fn CTFontGetLeading(font: Ref) -> f64;
    fn CTLineCreateWithAttributedString(s: Ref) -> Ref;
    fn CTLineGetTypographicBounds(line: Ref, ascent: *mut f64, descent: *mut f64, leading: *mut f64) -> f64;
    fn CTLineDraw(line: Ref, ctx: Ref);
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGColorSpaceCreateDeviceGray() -> *mut c_void;
    fn CGColorSpaceRelease(space: *mut c_void);
    fn CGBitmapContextCreate(
        data: *mut c_void,
        w: usize,
        h: usize,
        bits_per_component: usize,
        bytes_per_row: usize,
        space: Ref,
        info: u32,
    ) -> Ref;
    fn CGContextRelease(ctx: Ref);
    fn CGContextSetGrayFillColor(ctx: Ref, gray: f64, alpha: f64);
    fn CGContextSetTextPosition(ctx: Ref, x: f64, y: f64);
    fn CGContextSetShouldSmoothFonts(ctx: Ref, on: bool);
    fn CGContextSetAllowsAntialiasing(ctx: Ref, on: bool);
}

const UTF8: u32 = 0x0800_0100;
const ALPHA_NONE: u32 = 0;

fn cfstr(s: &str) -> Ref {
    unsafe { CFStringCreateWithBytes(std::ptr::null(), s.as_ptr(), s.len() as isize, UTF8, 0) }
}

/// A `CTLine` for `s` at the size that makes ascent + descent = `px`.
struct Line {
    line: Ref,
    ascent: f64,
    descent: f64,
    leading: f64,
    width: f64,
}

impl Line {
    fn new(s: &str, px: f32) -> Option<Self> {
        unsafe {
            let name = cfstr("Helvetica");
            if name.is_null() {
                return None;
            }
            let probe = CTFontCreateWithName(name, 100.0, std::ptr::null());
            if probe.is_null() {
                CFRelease(name);
                return None;
            }
            let cell = (CTFontGetAscent(probe) + CTFontGetDescent(probe)).max(1.0);
            CFRelease(probe);
            let font = CTFontCreateWithName(name, px as f64 * 100.0 / cell, std::ptr::null());
            CFRelease(name);
            if font.is_null() {
                return None;
            }
            let keys = [kCTFontAttributeName, kCTForegroundColorFromContextAttributeName];
            let values = [font, kCFBooleanTrue];
            let attrs = CFDictionaryCreate(
                std::ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                2,
                &raw const kCFTypeDictionaryKeyCallBacks,
                &raw const kCFTypeDictionaryValueCallBacks,
            );
            let (ascent, descent, leading) = (CTFontGetAscent(font), CTFontGetDescent(font), CTFontGetLeading(font));
            CFRelease(font);
            let text = cfstr(s);
            let astr = if attrs.is_null() || text.is_null() {
                std::ptr::null()
            } else {
                CFAttributedStringCreate(std::ptr::null(), text, attrs)
            };
            for r in [text, attrs] {
                if !r.is_null() {
                    CFRelease(r);
                }
            }
            if astr.is_null() {
                return None;
            }
            let line = CTLineCreateWithAttributedString(astr);
            CFRelease(astr);
            if line.is_null() {
                return None;
            }
            let width = CTLineGetTypographicBounds(line, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut());
            Some(Line { line, ascent, descent, leading, width })
        }
    }
}

impl Drop for Line {
    fn drop(&mut self) {
        unsafe { CFRelease(self.line) }
    }
}

#[derive(Clone)]
pub struct Face;

impl Face {
    pub fn load() -> Option<Self> {
        Line::new("a", 16.0).map(|_| Face)
    }

    pub fn metrics(&self, px: f32) -> (f32, f32, f32) {
        match Line::new(" ", px) {
            Some(l) => (l.ascent as f32, l.descent as f32, l.leading as f32),
            None => (px * 0.8, px * 0.2, 0.0),
        }
    }

    pub fn width(&self, line: &str, px: f32) -> f32 {
        Line::new(line, px).map_or(0.0, |l| l.width as f32)
    }

    pub fn draw(
        &self,
        line: &str,
        px: f32,
        top: Pt,
        ascent: f32,
        clip: (i32, i32, i32, i32),
        f: &mut dyn FnMut(i32, i32, f32),
    ) {
        let Some(l) = Line::new(line, px) else { return };
        let pad = (px / 3.0).ceil() as i32 + 2;
        let (ox, oy) = (top.x.round() as i32, top.y.round() as i32);
        let x0 = (ox - pad).max(clip.0);
        let y0 = (oy - pad).max(clip.1);
        let x1 = (ox + l.width.ceil() as i32 + pad).min(clip.2);
        let y1 = (oy + (l.ascent + l.descent).ceil() as i32 + pad).min(clip.3);
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
        let mut buf = vec![0u8; w * h];
        unsafe {
            let space = CGColorSpaceCreateDeviceGray();
            let ctx = CGBitmapContextCreate(buf.as_mut_ptr() as *mut c_void, w, h, 8, w, space as Ref, ALPHA_NONE);
            CGColorSpaceRelease(space);
            if ctx.is_null() {
                return;
            }
            CGContextSetAllowsAntialiasing(ctx, true);
            CGContextSetShouldSmoothFonts(ctx, false);
            CGContextSetGrayFillColor(ctx, 1.0, 1.0);
            // CG's origin is the bottom-left; buffer row 0 is the top.
            let base_y = (oy - y0) as f64 + ascent as f64;
            CGContextSetTextPosition(ctx, (ox - x0) as f64, h as f64 - base_y);
            CTLineDraw(l.line, ctx);
            CGContextRelease(ctx);
        }
        for (i, &v) in buf.iter().enumerate() {
            if v > 0 {
                f(x0 + (i % w) as i32, y0 + (i / w) as i32, v as f32 / 255.0);
            }
        }
    }
}
