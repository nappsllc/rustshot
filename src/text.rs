//! Annotation text (user-typed, any script): measured and rasterised by the
//! OS text stack so shaping and font fallback come for free: GDI on Windows
//! (`text_win.rs`), CoreText on macOS (`text_macos.rs`); Linux rasterises a
//! system font with ab_glyph (`text_linux.rs`). `px` is the line's
//! ascent-to-descent height, as before.
//!
//! Lines are split on `\n`, stacked `line_height` apart from `top`, and each
//! line starts at `top.x`. Coverage is blended with the object colour by
//! [`Surf::blend_px`]; the backend only produces a grayscale mask.

use crate::objects::Pt;
use crate::raster::{Blend, Surf};
use crate::uifb::C4;

#[cfg(windows)]
#[path = "text_win.rs"]
mod imp;
#[cfg(target_os = "linux")]
#[path = "text_linux.rs"]
mod imp;
#[cfg(target_os = "macos")]
#[path = "text_macos.rs"]
mod imp;

/// Vertical metrics of one line at a pixel size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LineMetrics {
    /// Baseline below the line top.
    pub ascent: f32,
    /// Below the baseline (positive).
    pub descent: f32,
    /// Distance between consecutive line tops.
    pub line_height: f32,
}

/// The annotation font. Cheap to clone.
#[derive(Clone)]
pub struct AnnotFont(imp::Face);

impl AnnotFont {
    /// The platform font; `None` when none is available (text objects then
    /// draw nothing).
    pub fn load() -> Option<Self> {
        imp::Face::load().map(AnnotFont)
    }

    pub fn metrics(&self, px: f32) -> LineMetrics {
        let (ascent, descent, gap) = self.0.metrics(px);
        LineMetrics { ascent, descent, line_height: (ascent + descent + gap).max(px * 1.1) }
    }

    /// Advance width of one line (no `\n`); caret offsets measure prefixes.
    pub fn line_width(&self, line: &str, px: f32) -> f32 {
        if line.is_empty() { 0.0 } else { self.0.width(line, px) }
    }

    /// Widest line and total height of `text`, with the line metrics.
    pub fn measure(&self, text: &str, px: f32) -> (f32, f32, LineMetrics) {
        let m = self.metrics(px);
        let (mut w, mut n) = (0.0f32, 0);
        for line in text.split('\n') {
            w = w.max(self.line_width(line, px));
            n += 1;
        }
        (w, m.line_height * n as f32, m)
    }

    /// Draw `text` with its top-left at `top`, clipped to the surface.
    pub fn render(&self, sf: &mut Surf, text: &str, px: f32, top: Pt, color: C4) {
        let m = self.metrics(px);
        let clip = sf.bounds();
        let mut y = top.y;
        for line in text.split('\n') {
            if !line.trim().is_empty() {
                self.0.draw(line, px, Pt::new(top.x, y), m.ascent, clip, &mut |x, y, cov| {
                    sf.blend_px(x, y, color, cov, Blend::Normal);
                });
            }
            y += m.line_height;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn font() -> AnnotFont {
        AnnotFont::load().expect("annotation font")
    }

    fn lit(text: &str, px: f32) -> usize {
        let f = font();
        let (w, h, _) = f.measure(text, px);
        let (w, h) = (w as u32 + 40, h as u32 + 20);
        let mut d = vec![0u8; (w * h * 4) as usize];
        f.render(&mut Surf::new(&mut d, w, h), text, px, Pt::new(10.0, 5.0), C4::rgb(255, 255, 255));
        d.as_chunks::<4>().0.iter().filter(|p| p[0] > 64).count()
    }

    #[test]
    fn renders_every_script() {
        for s in ["Hello, world", "Привет, мир", "مرحبا بالعالم", "你好世界"] {
            let n = lit(s, 24.0);
            assert!(n > 30, "{s:?}: only {n} lit pixels");
        }
    }

    #[test]
    fn measure_is_monotonic_in_length() {
        let f = font();
        for s in ["Annotation text", "Привет, мир", "Mixed: abc 123 xyz"] {
            let mut prev = 0.0f32;
            let mut end = 0;
            for (i, ch) in s.char_indices() {
                end = i + ch.len_utf8();
                let w = f.line_width(&s[..end], 20.0);
                assert!(w >= prev, "{s:?} up to {end}: {w} < {prev}");
                prev = w;
            }
            assert!(prev > 20.0 && end == s.len(), "{s:?}: {prev}");
        }
    }

    #[test]
    fn metrics_scale_with_size() {
        let f = font();
        let (a, b) = (f.metrics(12.0), f.metrics(36.0));
        assert!(a.ascent > 5.0 && a.descent > 0.5, "{a:?}");
        assert!(b.line_height > a.line_height * 2.5, "{a:?} {b:?}");
        assert!(a.line_height >= 12.0 * 1.1);
        let (w1, h1, _) = f.measure("ab\ncdef", 20.0);
        let (w2, h2, _) = f.measure("cdef", 20.0);
        assert_eq!(w1, w2);
        assert_eq!(h1, 2.0 * h2);
    }

    #[test]
    fn clipped_render_matches_full_render() {
        // A window over part of the text paints what the full surface has there.
        let f = font();
        let (w, h) = (200u32, 60u32);
        let mut full = vec![0u8; (w * h * 4) as usize];
        let c = C4::new(250, 60, 20, 210);
        f.render(&mut Surf::new(&mut full, w, h), "Hello Wy\nПривет", 20.0, Pt::new(7.3, 4.6), c);
        let (cx, cy, cw, ch) = (31u32, 9u32, 50u32, 30u32);
        let mut win = vec![0u8; (cw * ch * 4) as usize];
        f.render(
            &mut Surf::with_origin(&mut win, cw, ch, cx as i32, cy as i32, crate::raster::Order::Rgba),
            "Hello Wy\nПривет",
            20.0,
            Pt::new(7.3, 4.6),
            c,
        );
        for y in 0..ch {
            for x in 0..cw {
                let a = ((y * cw + x) * 4) as usize;
                let b = (((y + cy) * w + x + cx) * 4) as usize;
                assert_eq!(win[a..a + 4], full[b..b + 4], "({x}, {y})");
            }
        }
        assert!(win.iter().any(|&v| v != 0));
    }
}
