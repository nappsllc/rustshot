//! Windows annotation text: GDI with "Segoe UI". `DrawTextW` brings
//! Uniscribe shaping (Arabic, Indic) and font linking (CJK) for free. Each
//! line is drawn white-on-black into a 32-bit DIB with grayscale
//! anti-aliasing; the gray level is the coverage.

use crate::objects::Pt;
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject, DrawTextW, GdiFlush,
    GetTextMetricsW, SelectObject, SetBkMode, SetTextColor, ANTIALIASED_QUALITY, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_PITCH, DIB_RGB_COLORS,
    DRAW_TEXT_FORMAT, DT_CALCRECT, DT_NOCLIP, DT_NOPREFIX, DT_SINGLELINE, FW_NORMAL, HDC, HFONT,
    HGDIOBJ, OUT_TT_PRECIS, TEXTMETRICW, TRANSPARENT,
};

#[derive(Clone)]
pub struct Face;

/// A memory DC with the face selected at a pixel size; freed on drop.
struct FontDc {
    dc: HDC,
    font: HFONT,
    old: HGDIOBJ,
}

impl FontDc {
    fn new(px: f32) -> Option<Self> {
        unsafe {
            let dc = CreateCompatibleDC(None);
            if dc.is_invalid() {
                return None;
            }
            // Positive height: the cell (ascent + descent) height, which is
            // what `px` meant for the old ab_glyph renderer.
            let font = CreateFontW(
                px.round().max(1.0) as i32,
                0,
                0,
                0,
                FW_NORMAL.0 as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_TT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                ANTIALIASED_QUALITY,
                DEFAULT_PITCH.0 as u32,
                w!("Segoe UI"),
            );
            if font.is_invalid() {
                let _ = DeleteDC(dc);
                return None;
            }
            let old = SelectObject(dc, font.into());
            Some(FontDc { dc, font, old })
        }
    }

    fn text(&self, line: &str, r: &mut RECT, flags: DRAW_TEXT_FORMAT) {
        let mut wide: Vec<u16> = line.encode_utf16().collect();
        unsafe {
            DrawTextW(self.dc, &mut wide, r, flags | DT_SINGLELINE | DT_NOPREFIX | DT_NOCLIP);
        }
    }
}

impl Drop for FontDc {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.font.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

impl Face {
    pub fn load() -> Option<Self> {
        FontDc::new(16.0).map(|_| Face)
    }

    pub fn metrics(&self, px: f32) -> (f32, f32, f32) {
        let Some(fd) = FontDc::new(px) else { return (px * 0.8, px * 0.2, 0.0) };
        let mut tm = TEXTMETRICW::default();
        if !unsafe { GetTextMetricsW(fd.dc, &mut tm) }.as_bool() {
            return (px * 0.8, px * 0.2, 0.0);
        }
        (tm.tmAscent as f32, tm.tmDescent as f32, tm.tmExternalLeading as f32)
    }

    pub fn width(&self, line: &str, px: f32) -> f32 {
        let Some(fd) = FontDc::new(px) else { return 0.0 };
        let mut r = RECT::default();
        fd.text(line, &mut r, DT_CALCRECT);
        (r.right - r.left).max(0) as f32
    }

    pub fn draw(
        &self,
        line: &str,
        px: f32,
        top: Pt,
        _ascent: f32,
        clip: (i32, i32, i32, i32),
        f: &mut dyn FnMut(i32, i32, f32),
    ) {
        let Some(fd) = FontDc::new(px) else { return };
        let mut ext = RECT::default();
        fd.text(line, &mut ext, DT_CALCRECT);
        // Overhang (italic-ish fallback glyphs, combining marks) beyond the
        // logical box.
        let pad = (px / 3.0).ceil() as i32 + 2;
        let (ox, oy) = (top.x.round() as i32, top.y.round() as i32);
        let x0 = (ox - pad).max(clip.0);
        let y0 = (oy - pad).max(clip.1);
        let x1 = (ox + ext.right + pad).min(clip.2);
        let y1 = (oy + ext.bottom + pad).min(clip.3);
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let (w, h) = (x1 - x0, y1 - y0);
        unsafe {
            let mut bmi: BITMAPINFO = std::mem::zeroed();
            bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bmi.bmiHeader.biWidth = w;
            bmi.bmiHeader.biHeight = -h; // top-down
            bmi.bmiHeader.biPlanes = 1;
            bmi.bmiHeader.biBitCount = 32;
            bmi.bmiHeader.biCompression = BI_RGB.0;
            let mut bits = std::ptr::null_mut();
            let Ok(bmp) = CreateDIBSection(Some(fd.dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) else {
                return;
            };
            if bits.is_null() {
                let _ = DeleteObject(bmp.into());
                return;
            }
            let old = SelectObject(fd.dc, bmp.into());
            SetBkMode(fd.dc, TRANSPARENT);
            SetTextColor(fd.dc, COLORREF(0x00FF_FFFF));
            let mut r = RECT { left: ox - x0, top: oy - y0, right: ox - x0 + ext.right, bottom: oy - y0 + ext.bottom };
            fd.text(line, &mut r, DRAW_TEXT_FORMAT(0));
            let _ = GdiFlush();
            // A fresh DIB section is zero-filled: black, i.e. no coverage.
            let px_data = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize);
            for (i, p) in px_data.as_chunks::<4>().0.iter().enumerate() {
                let v = p[0].max(p[1]).max(p[2]);
                if v > 0 {
                    let i = i as i32;
                    f(x0 + i % w, y0 + i / w, v as f32 / 255.0);
                }
            }
            SelectObject(fd.dc, old);
            let _ = DeleteObject(bmp.into());
        }
    }
}
