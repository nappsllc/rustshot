//! Software framebuffer: alpha blending, rectangles, image blits, and
//! ab_glyph text rendering. Draw into an unpremultiplied RGBA buffer via [`Fb`].

use crate::objects::{FRect, Pt};
use crate::raster::{Blend, Surf};
use ab_glyph::{Font, FontArc, PxScale, ScaleFont};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct C4 {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl C4 {
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        C4 { r, g, b, a }
    }

    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        C4 { r, g, b, a: 255 }
    }

    pub fn with_alpha(self, a: u8) -> Self {
        C4 { a, ..self }
    }

    /// Same color with alpha scaled by `k` (clamped 0..=1): fades, disabled.
    pub fn fade(self, k: f32) -> Self {
        C4 {
            a: (self.a as f32 * k.clamp(0.0, 1.0)).round() as u8,
            ..self
        }
    }
}

/// Borrowed drawing surface over an RGBA8 buffer.
pub struct Fb<'a> {
    pub d: &'a mut [u8],
    pub stride: usize,
}

impl<'a> Fb<'a> {
    pub fn new(d: &'a mut [u8], stride: usize) -> Self {
        Fb { d, stride }
    }

    pub fn height(&self) -> i32 {
        self.d.len() as i32 / 4 / self.stride as i32
    }

    /// The same pixels as an AA raster surface (strokes, polygons).
    pub fn surf(&mut self) -> Surf<'_> {
        let h = self.height().max(0) as u32;
        Surf::new(&mut *self.d, self.stride as u32, h)
    }

    /// Blend `c` over the pixel at (x, y).
    pub fn blend_px(&mut self, x: usize, y: usize, c: C4) {
        let i = (y * self.stride + x) * 4;
        if i + 3 >= self.d.len() {
            return;
        }
        let a = c.a as u32;
        if a == 255 {
            self.d[i] = c.r;
            self.d[i + 1] = c.g;
            self.d[i + 2] = c.b;
            self.d[i + 3] = 255;
            return;
        }
        if a == 0 {
            return;
        }
        let ia = 255 - a;
        let (d0, d1, d2, d3) = (
            self.d[i] as u32,
            self.d[i + 1] as u32,
            self.d[i + 2] as u32,
            self.d[i + 3] as u32,
        );
        self.d[i] = ((c.r as u32 * a + d0 * ia) / 255) as u8;
        self.d[i + 1] = ((c.g as u32 * a + d1 * ia) / 255) as u8;
        self.d[i + 2] = ((c.b as u32 * a + d2 * ia) / 255) as u8;
        self.d[i + 3] = (a + (d3 * ia) / 255).min(255) as u8;
    }

    pub fn fill_rect(&mut self, x0: i32, y0: i32, w: i32, h: i32, c: C4) {
        if w <= 0 || h <= 0 {
            return;
        }
        let fh = self.height();
        for y in y0.max(0)..(y0 + h).min(fh) {
            for x in x0.max(0)..(x0 + w).min(self.stride as i32) {
                self.blend_px(x as usize, y as usize, c);
            }
        }
    }

    /// Rectangle outline with sub-pixel coverage around the border.
    pub fn stroke_rect(&mut self, x0: f32, y0: f32, w: f32, h: f32, thickness: f32, c: C4) {
        if w <= 0.0 || h <= 0.0 || thickness <= 0.0 {
            return;
        }
        let (x1, y1) = (x0 + w, y0 + h);
        let half = thickness / 2.0;
        let fh = self.height();
        let ys = (y0 - half - 1.0).floor().max(0.0) as i32;
        let ye = ((y1 + half + 1.0).ceil() as i32).min(fh);
        let xs = (x0 - half - 1.0).floor().max(0.0) as i32;
        let xe = ((x1 + half + 1.0).ceil() as i32).min(self.stride as i32);
        for y in ys..ye {
            for x in xs..xe {
                let cx = x as f32 + 0.5;
                let cy = y as f32 + 0.5;
                // Signed distance to the rectangle (negative inside).
                let qx = (cx - x0 - w / 2.0).abs() - w / 2.0;
                let qy = (cy - y0 - h / 2.0).abs() - h / 2.0;
                let dx = qx.max(0.0);
                let dy = qy.max(0.0);
                let sdf = (dx * dx + dy * dy).sqrt() + qx.max(qy).min(0.0);
                let cov = (half + 0.5 - sdf.abs()).clamp(0.0, 1.0);
                if cov > 0.0 {
                    let mut col = c;
                    col.a = ((c.a as f32 * cov) as u32).min(255) as u8;
                    self.blend_px(x as usize, y as usize, col);
                }
            }
        }
    }

    /// Filled rounded rectangle (corner radius in px).
    pub fn fill_rounded(&mut self, x0: i32, y0: i32, w: i32, h: i32, r: f32, c: C4) {
        if w <= 0 || h <= 0 {
            return;
        }
        let r = r.min(w as f32 / 2.0).min(h as f32 / 2.0);
        let fh = self.height();
        for y in y0.max(0)..(y0 + h).min(fh) {
            for x in x0.max(0)..(x0 + w).min(self.stride as i32) {
                let mut a = c.a;
                if r > 0.0 {
                    let cx = x as f32 + 0.5;
                    let cy = y as f32 + 0.5;
                    let inx0 = x0 as f32 + r;
                    let inx1 = (x0 + w) as f32 - r;
                    let iny0 = y0 as f32 + r;
                    let iny1 = (y0 + h) as f32 - r;
                    let outx = cx < inx0 || cx > inx1;
                    let outy = cy < iny0 || cy > iny1;
                    if outx && outy {
                        let qx = if cx < inx0 { inx0 - cx } else { cx - inx1 };
                        let qy = if cy < iny0 { iny0 - cy } else { cy - iny1 };
                        let dist = (qx * qx + qy * qy).sqrt();
                        let cov = (r + 0.5 - dist).clamp(0.0, 1.0);
                        if cov <= 0.0 {
                            continue;
                        }
                        a = (a as f32 * cov) as u8;
                    }
                }
                let mut col = c;
                col.a = a;
                self.blend_px(x as usize, y as usize, col);
            }
        }
    }

    /// Draw `s` with the top-left corner at (x, y). Returns the advance width.
    pub fn draw_text(
        &mut self,
        font: &FontArc,
        px: f32,
        s: &str,
        x: f32,
        y: f32,
        c: C4,
    ) -> f32 {
        let f = font.as_scaled(PxScale { x: px, y: px });
        let baseline = y + f.ascent();
        let mut pen = x;
        let fh = self.height();
        let stride = self.stride;
        for ch in s.chars() {
            let glyph = ab_glyph::Glyph {
                id: font.glyph_id(ch),
                scale: PxScale { x: px, y: px },
                position: ab_glyph::point(pen, baseline),
            };
            if let Some(outline) = f.outline_glyph(glyph) {
                let bounds = outline.px_bounds();
                outline.draw(|gx, gy, cov| {
                    if cov <= 0.0 {
                        return;
                    }
                    let sx = bounds.min.x as i32 + gx as i32;
                    let sy = bounds.min.y as i32 + gy as i32;
                    if sx < 0 || sy < 0 || sx >= stride as i32 || sy >= fh {
                        return;
                    }
                    let mut col = c;
                    col.a = ((c.a as f32 * cov) as u32).min(255) as u8;
                    self.blend_px(sx as usize, sy as usize, col);
                });
            }
            pen += f.h_advance(font.glyph_id(ch));
        }
        pen - x
    }

    /// Anti-aliased filled circle.
    pub fn fill_circle(&mut self, cx: f32, cy: f32, r: f32, c: C4) {
        let fh = self.height();
        let x0 = (cx - r - 1.0).floor().max(0.0) as i32;
        let x1 = ((cx + r + 1.0).ceil() as i32).min(self.stride as i32);
        let y0 = (cy - r - 1.0).floor().max(0.0) as i32;
        let y1 = ((cy + r + 1.0).ceil() as i32).min(fh);
        for y in y0..y1 {
            for x in x0..x1 {
                let d = (x as f32 + 0.5 - cx).hypot(y as f32 + 0.5 - cy);
                let cov = (r + 0.5 - d).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.blend_px(x as usize, y as usize, c.fade(cov));
                }
            }
        }
    }

    /// Circle outline `width` px wide, centred on radius `r`.
    pub fn stroke_circle(&mut self, cx: f32, cy: f32, r: f32, width: f32, c: C4) {
        let rect = FRect {
            x: cx - r,
            y: cy - r,
            w: 2.0 * r,
            h: 2.0 * r,
        };
        self.surf().stroke_ellipse(rect, width, c, Blend::Normal);
    }

    /// Rounded-rect outline `width` px wide, centred on the edge of `r`.
    pub fn stroke_rounded(&mut self, r: FRect, radius: f32, width: f32, c: C4) {
        self.surf()
            .stroke_round_rect(r, radius, width, c, Blend::Normal);
    }

    /// Dashed rectangle outline: `dash` on, `gap` off, clockwise from top-left.
    pub fn stroke_dashed_rect(&mut self, r: FRect, dash: f32, gap: f32, width: f32, c: C4) {
        if dash <= 0.0 {
            return;
        }
        let corners = [
            Pt::new(r.x, r.y),
            Pt::new(r.x1(), r.y),
            Pt::new(r.x1(), r.y1()),
            Pt::new(r.x, r.y1()),
            Pt::new(r.x, r.y),
        ];
        let period = dash + gap.max(0.0);
        let lerp = |a: Pt, b: Pt, t: f32| Pt::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t);
        let mut s = self.surf();
        let mut phase = 0.0f32;
        for w in corners.windows(2) {
            let (a, b) = (w[0], w[1]);
            let len = (b.x - a.x).hypot(b.y - a.y);
            let mut t = 0.0;
            while t < len {
                let into = phase % period;
                let on = into < dash;
                let seg = if on { dash - into } else { period - into }.min(len - t);
                let seg = seg.max(1e-3);
                if on {
                    s.stroke_polyline(
                        &[lerp(a, b, t / len), lerp(a, b, (t + seg) / len)],
                        width,
                        c,
                        Blend::Normal,
                    );
                }
                t += seg;
                phase += seg;
            }
        }
    }
}

fn scaled(font: &FontArc, px: f32) -> ab_glyph::PxScaleFont<&FontArc> {
    font.as_scaled(PxScale { x: px, y: px })
}

pub fn text_width(font: &FontArc, px: f32, s: &str) -> f32 {
    let f = scaled(font, px);
    let mut w = 0.0f32;
    for ch in s.chars() {
        w += f.h_advance(font.glyph_id(ch));
    }
    w
}

pub fn text_height(font: &FontArc, px: f32) -> f32 {
    let f = scaled(font, px);
    f.ascent() - f.descent()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real font for glyph metrics (system font, else embedded Inter).
    fn font() -> Option<FontArc> {
        crate::fonts::load_system_font()
    }

    #[test]
    fn blend_solid_and_alpha() {
        let mut fb = vec![100u8; 4 * 4];
        {
            let mut f = Fb::new(&mut fb, 4);
            f.blend_px(0, 0, C4::rgb(200, 0, 0));
            assert_eq!(&f.d[0..4], &[200, 0, 0, 255]);
            f.blend_px(1, 0, C4::new(0, 0, 0, 128));
            assert!(f.d[4] > 35 && f.d[4] < 60, "got {}", f.d[4]);
        }
    }

    #[test]
    fn fill_and_stroke() {
        let stride = 20;
        let mut fb = vec![0u8; stride * 20 * 4];
        {
            let mut f = Fb::new(&mut fb, stride);
            f.fill_rect(2, 2, 5, 5, C4::rgb(255, 0, 0));
            assert_eq!(f.d[(2 * stride + 2) * 4], 255);
            assert_eq!(f.d[(9 * stride + 9) * 4], 0);
            f.stroke_rect(4.0, 4.0, 8.0, 8.0, 1.5, C4::rgb(0, 255, 0));
        }
        let found = fb.as_chunks::<4>().0.iter().any(|p| p[1] > 100 && p[0] < 100);
        assert!(found, "stroke produced no green pixels");
    }

    #[test]
    fn text_measures_and_draws() {
        let Some(f) = font() else {
            eprintln!("skipping: no known system font on this host");
            return;
        };
        let w = text_width(&f, 14.0, "Hello");
        assert!(w > 20.0 && w < 200.0, "width {w}");
        let stride = 120;
        let mut fb = vec![0u8; stride * 30 * 4];
        let adv = {
            let mut fbw = Fb::new(&mut fb, stride);
            fbw.draw_text(&f, 14.0, "Hi", 4.0, 4.0, C4::rgb(255, 255, 255))
        };
        assert!(adv > 5.0);
        let lit = fb.as_chunks::<4>().0.iter().filter(|p| p[0] > 128).count();
        assert!(lit > 20, "only {lit} lit pixels");
    }

    #[test]
    fn stroke_rect_is_symmetric() {
        let stride = 20;
        let mut d = vec![0u8; stride * 20 * 4];
        Fb::new(&mut d, stride).stroke_rect(5.0, 5.0, 10.0, 10.0, 1.0, C4::rgb(255, 255, 255));
        let at = |x: usize, y: usize| d[(y * stride + x) * 4];
        // Edge at x=5 / x=15 covers half of columns 4 and 5 / 14 and 15 equally.
        assert_eq!(at(4, 10), at(15, 10), "left vs right outer half");
        assert_eq!(at(10, 4), at(10, 15), "top vs bottom outer half");
        assert!(at(4, 10) > 60, "outer half drawn: {}", at(4, 10));
        assert_eq!(at(10, 10), 0, "hollow");
    }

    #[test]
    fn rounded_fill_covers_center() {
        let stride = 30;
        let mut fb = vec![0u8; stride * 30 * 4];
        {
            let mut f = Fb::new(&mut fb, stride);
            f.fill_rounded(0, 0, 30, 30, 6.0, C4::rgb(10, 20, 30));
        }
        assert_eq!(fb[(15 * stride + 15) * 4 + 1], 20);
        assert_eq!(fb[0], 0);
    }

    #[test]
    fn fade_scales_alpha() {
        assert_eq!(C4::new(1, 2, 3, 200).fade(0.5), C4::new(1, 2, 3, 100));
        assert_eq!(C4::rgb(1, 2, 3).fade(2.0).a, 255);
    }

    #[test]
    fn circle_fills_centre_not_corner() {
        let mut d = vec![0u8; 20 * 20 * 4];
        Fb::new(&mut d, 20).fill_circle(10.0, 10.0, 5.0, C4::rgb(255, 0, 0));
        assert_eq!(d[(10 * 20 + 10) * 4], 255);
        assert_eq!(d[0], 0);
    }

    #[test]
    fn circle_stroke_is_a_ring() {
        let mut d = vec![0u8; 20 * 20 * 4];
        Fb::new(&mut d, 20).stroke_circle(10.0, 10.0, 6.0, 1.0, C4::rgb(0, 0, 255));
        assert_eq!(d[(10 * 20 + 10) * 4 + 2], 0, "hollow centre");
        assert!(d[(4 * 20 + 10) * 4 + 2] > 100, "top of ring");
    }

    #[test]
    fn rounded_stroke_is_hollow() {
        let mut d = vec![0u8; 30 * 30 * 4];
        let r = FRect { x: 5.0, y: 5.0, w: 20.0, h: 20.0 };
        Fb::new(&mut d, 30).stroke_rounded(r, 4.0, 1.0, C4::rgb(0, 255, 0));
        assert_eq!(d[(15 * 30 + 15) * 4 + 1], 0);
        assert!(d[(15 * 30 + 5) * 4 + 1] > 100);
    }

    #[test]
    fn dashes_leave_gaps() {
        let mut d = vec![0u8; 40 * 10 * 4];
        let r = FRect { x: 2.5, y: 2.5, w: 35.0, h: 5.0 };
        Fb::new(&mut d, 40).stroke_dashed_rect(r, 4.0, 4.0, 1.0, C4::rgb(255, 255, 255));
        let top: Vec<u8> = (2..38).map(|x| d[(2 * 40 + x) * 4]).collect();
        assert!(top.iter().any(|v| *v > 200), "{top:?}");
        assert!(top.iter().any(|v| *v < 30), "{top:?}");
    }

    #[test]
    fn dashed_rect_terminates_on_awkward_periods() {
        let mut d = vec![0u8; 400 * 300 * 4];
        let r = FRect { x: 1.5, y: 1.5, w: 397.0, h: 297.0 };
        Fb::new(&mut d, 400).stroke_dashed_rect(r, 3.3, 0.7, 1.0, C4::rgb(255, 255, 255));
        assert!(d.as_chunks::<4>().0.iter().any(|p| p[0] > 200));
    }
}
