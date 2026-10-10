//! Software framebuffer: alpha blending, rectangles, image blits, and
//! baked UI-font text rendering. Draw into an unpremultiplied RGBA buffer via [`Fb`].

use crate::objects::{FRect, Pt};
use crate::raster::{Blend, Order, Surf};
use crate::fonts::UiFont;

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

/// `x / 255` for `x <= 255 * 255` without a divide (the size-optimised
/// release profile keeps real `div` instructions for `/ 255`).
#[inline(always)]
fn div255(x: u32) -> u32 {
    (x + 1 + (x >> 8)) >> 8
}

/// Borrowed drawing surface over a 4-byte-per-pixel buffer. The buffer
/// covers image coordinates `ox..ox + stride` x `oy..oy + height`; every
/// draw call takes image coordinates and clips to that window.
pub struct Fb<'a> {
    pub d: &'a mut [u8],
    pub stride: usize,
    pub ox: i32,
    pub oy: i32,
    pub order: Order,
}

impl<'a> Fb<'a> {
    /// A whole-image RGBA surface (origin 0,0).
    pub fn new(d: &'a mut [u8], stride: usize) -> Self {
        Self::with_origin(d, stride, 0, 0, Order::Rgba)
    }

    /// A surface whose top-left pixel is image pixel (`ox`, `oy`).
    pub fn with_origin(d: &'a mut [u8], stride: usize, ox: i32, oy: i32, order: Order) -> Self {
        Fb {
            d,
            stride,
            ox,
            oy,
            order,
        }
    }

    /// Buffer height in pixels.
    pub fn height(&self) -> i32 {
        self.d.len() as i32 / 4 / self.stride as i32
    }

    /// The window in image coordinates: (x0, y0, x1, y1), end-exclusive.
    fn bounds(&self) -> (i32, i32, i32, i32) {
        (
            self.ox,
            self.oy,
            self.ox + self.stride as i32,
            self.oy + self.height(),
        )
    }

    /// The same pixels as an AA raster surface (strokes, polygons).
    pub fn surf(&mut self) -> Surf<'_> {
        let h = self.height().max(0) as u32;
        Surf::with_origin(
            &mut *self.d,
            self.stride as u32,
            h,
            self.ox,
            self.oy,
            self.order,
        )
    }

    /// Blend `c` over the pixel at image coordinates (x, y); outside the
    /// window is a no-op.
    pub fn blend_px(&mut self, x: i32, y: i32, c: C4) {
        let (x, y) = (x - self.ox, y - self.oy);
        if x < 0 || y < 0 || x >= self.stride as i32 {
            return;
        }
        let i = (y as usize * self.stride + x as usize) * 4;
        let Some(px) = self.d.get_mut(i..i + 4) else {
            return;
        };
        let bgra = self.order == Order::Bgra;
        let a = c.a as u32;
        if a == 255 {
            px.copy_from_slice(&if bgra { [c.b, c.g, c.r, 255] } else { [c.r, c.g, c.b, 255] });
            return;
        }
        if a == 0 {
            return;
        }
        if bgra {
            px.swap(0, 2); // blend in RGBA, swap back
        }
        let ia = 255 - a;
        px[0] = div255(c.r as u32 * a + px[0] as u32 * ia) as u8;
        px[1] = div255(c.g as u32 * a + px[1] as u32 * ia) as u8;
        px[2] = div255(c.b as u32 * a + px[2] as u32 * ia) as u8;
        px[3] = (a + div255(px[3] as u32 * ia)).min(255) as u8;
        if bgra {
            px.swap(0, 2);
        }
    }

    pub fn fill_rect(&mut self, x0: i32, y0: i32, w: i32, h: i32, c: C4) {
        if w <= 0 || h <= 0 {
            return;
        }
        let (bx0, by0, bx1, by1) = self.bounds();
        for y in y0.max(by0)..(y0 + h).min(by1) {
            for x in x0.max(bx0)..(x0 + w).min(bx1) {
                self.blend_px(x, y, c);
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
        let (bx0, by0, bx1, by1) = self.bounds();
        let ys = ((y0 - half - 1.0).floor() as i32).max(by0);
        let ye = ((y1 + half + 1.0).ceil() as i32).min(by1);
        let xs = ((x0 - half - 1.0).floor() as i32).max(bx0);
        let xe = ((x1 + half + 1.0).ceil() as i32).min(bx1);
        // Coverage is zero farther than `half + 1` from the outline, so only
        // the four border strips are visited, never the hollow interior:
        // rows near the top/bottom edge span the full width; other rows
        // visit just the columns near the left/right edges.
        let band = |e: f32| ((e - half - 1.0).floor() as i32, (e + half + 1.0).ceil() as i32);
        let (top, bottom, left, right) = (band(y0), band(y1), band(x0), band(x1));
        let in_band = |v: i32, b: (i32, i32)| v >= b.0 && v < b.1;
        for y in ys..ye {
            let cols = if in_band(y, top) || in_band(y, bottom) || left.1 >= right.0 {
                [(xs, xe), (xe, xe)]
            } else {
                [(xs, left.1.min(xe)), (right.0.max(xs), xe)]
            };
            for x in cols.into_iter().flat_map(|(a, b)| a..b) {
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
                    self.blend_px(x, y, col);
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
        let (bx0, by0, bx1, by1) = self.bounds();
        for y in y0.max(by0)..(y0 + h).min(by1) {
            for x in x0.max(bx0)..(x0 + w).min(bx1) {
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
                self.blend_px(x, y, col);
            }
        }
    }

    /// Draw `s` with the top-left corner at (x, y). Returns the advance width.
    pub fn draw_text(
        &mut self,
        font: &UiFont,
        px: f32,
        s: &str,
        x: f32,
        y: f32,
        c: C4,
    ) -> f32 {
        let (bx0, by0, bx1, by1) = self.bounds();
        font.draw(s, px, x, y, |sx, sy, cov| {
            if cov <= 0.0 || sx < bx0 || sy < by0 || sx >= bx1 || sy >= by1 {
                return;
            }
            let mut col = c;
            col.a = ((c.a as f32 * cov) as u32).min(255) as u8;
            self.blend_px(sx, sy, col);
        })
    }

    /// Anti-aliased filled circle.
    pub fn fill_circle(&mut self, cx: f32, cy: f32, r: f32, c: C4) {
        let (bx0, by0, bx1, by1) = self.bounds();
        let x0 = ((cx - r - 1.0).floor() as i32).max(bx0);
        let x1 = ((cx + r + 1.0).ceil() as i32).min(bx1);
        let y0 = ((cy - r - 1.0).floor() as i32).max(by0);
        let y1 = ((cy + r + 1.0).ceil() as i32).min(by1);
        for y in y0..y1 {
            for x in x0..x1 {
                let d = (x as f32 + 0.5 - cx).hypot(y as f32 + 0.5 - cy);
                let cov = (r + 0.5 - d).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.blend_px(x, y, c.fade(cov));
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

pub fn text_width(font: &UiFont, px: f32, s: &str) -> f32 {
    font.width(s, px)
}

pub fn text_height(font: &UiFont, px: f32) -> f32 {
    font.height(px)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn font() -> Option<&'static UiFont> {
        Some(crate::fonts::ui_font())
    }

    #[test]
    fn div255_is_exact() {
        for x in 0..=255 * 255 {
            assert_eq!(div255(x), x / 255, "{x}");
        }
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
        let w = text_width(f, 14.0, "Hello");
        assert!(w > 20.0 && w < 200.0, "width {w}");
        let stride = 120;
        let mut fb = vec![0u8; stride * 30 * 4];
        let adv = {
            let mut fbw = Fb::new(&mut fb, stride);
            fbw.draw_text(f, 14.0, "Hi", 4.0, 4.0, C4::rgb(255, 255, 255))
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
    fn stroke_rect_leaves_large_interior_untouched() {
        let (w, h) = (300usize, 200usize);
        let mut d = vec![7u8; w * h * 4];
        let mut f = Fb::new(&mut d, w);
        f.stroke_rect(20.5, 10.25, 250.0, 170.0, 3.0, C4::new(255, 255, 255, 200));
        let at = |x: usize, y: usize| d[(y * w + x) * 4];
        for y in 0..h {
            for x in 0..w {
                // More than half + 1 px from the outline: never written.
                let inside = (25..266).contains(&x) && (15..176).contains(&y);
                let outside = !(17..275).contains(&x) || !(7..184).contains(&y);
                if inside || outside {
                    assert_eq!(at(x, y), 7, "touched at ({x}, {y})");
                }
            }
        }
        assert!(at(20, 100) > 100 && at(270, 100) > 100 && at(150, 10) > 100 && at(150, 180) > 100);
    }

    /// The strip walk paints exactly what a full bounding-box sweep paints.
    #[test]
    fn stroke_rect_matches_full_sweep() {
        for &(x0, y0, rw, rh, t) in &[(5.0, 5.0, 10.0, 10.0, 1.0), (3.3, 4.7, 40.2, 2.0, 2.5), (10.5, 8.0, 60.0, 30.0, 4.0)] {
            let (w, h) = (90usize, 50usize);
            let mut a = vec![0u8; w * h * 4];
            Fb::new(&mut a, w).stroke_rect(x0, y0, rw, rh, t, C4::new(200, 100, 50, 180));
            let mut b = vec![0u8; w * h * 4];
            let half = t / 2.0;
            for y in 0..h {
                for x in 0..w {
                    let (cx, cy) = (x as f32 + 0.5, y as f32 + 0.5);
                    let qx = (cx - x0 - rw / 2.0).abs() - rw / 2.0;
                    let qy = (cy - y0 - rh / 2.0).abs() - rh / 2.0;
                    let (dx, dy) = (qx.max(0.0), qy.max(0.0));
                    let sdf = (dx * dx + dy * dy).sqrt() + qx.max(qy).min(0.0);
                    let cov = (half + 0.5 - sdf.abs()).clamp(0.0, 1.0);
                    if cov > 0.0 {
                        let c = C4::new(200, 100, 50, ((180.0 * cov) as u32).min(255) as u8);
                        Fb::new(&mut b, w).blend_px(x as i32, y as i32, c);
                    }
                }
            }
            assert_eq!(a, b, "rect {x0},{y0} {rw}x{rh} t{t}");
        }
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

    /// Offset BGRA sub-buffer drawing equals full RGBA drawing, cropped and
    /// channel-swapped (byte-exact). Shared with the object tests.
    pub(crate) mod equiv {
        use crate::raster::{Order, Surf};
        use crate::uifb::Fb;

        pub const W: usize = 100;
        pub const H: usize = 80;
        /// Sub-buffer window (x, y, w, h) inside the image.
        pub const CROP: (usize, usize, usize, usize) = (30, 20, 40, 30);

        pub fn background() -> Vec<u8> {
            let mut d = vec![0u8; W * H * 4];
            for (i, p) in d.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let (x, y) = (i % W, i / W);
                p[0] = (x * 7 + y * 3) as u8;
                p[1] = (x * 2 + y * 11 + 40) as u8;
                p[2] = ((x ^ y) * 5) as u8;
                p[3] = 255;
            }
            d
        }

        /// Swapped crop of `full` (BGRA, CROP-sized).
        pub fn crop_bgra(full: &[u8]) -> Vec<u8> {
            let (cx, cy, cw, ch) = CROP;
            let mut out = Vec::with_capacity(cw * ch * 4);
            for y in cy..cy + ch {
                for x in cx..cx + cw {
                    let i = (y * W + x) * 4;
                    out.extend_from_slice(&[full[i + 2], full[i + 1], full[i], full[i + 3]]);
                }
            }
            out
        }

        /// Run `draw` on a full RGBA surface and on an offset BGRA window
        /// over the same background; the results must agree byte for byte.
        pub fn check(name: &str, draw: impl Fn(&mut Surf)) {
            let mut full = background();
            draw(&mut Surf::new(&mut full, W as u32, H as u32));
            let mut sub = crop_bgra(&background());
            let (cx, cy, cw, ch) = CROP;
            draw(&mut Surf::with_origin(
                &mut sub,
                cw as u32,
                ch as u32,
                cx as i32,
                cy as i32,
                Order::Bgra,
            ));
            assert!(sub == crop_bgra(&full), "{name}: surf mismatch");
        }

        /// Same for the integer-blend `Fb` paths.
        pub fn check_fb(name: &str, draw: impl Fn(&mut Fb)) {
            let mut full = background();
            draw(&mut Fb::new(&mut full, W));
            let mut sub = crop_bgra(&background());
            let (cx, cy, cw, _) = CROP;
            draw(&mut Fb::with_origin(
                &mut sub,
                cw,
                cx as i32,
                cy as i32,
                Order::Bgra,
            ));
            assert!(sub == crop_bgra(&full), "{name}: fb mismatch");
            // The drawing must also have changed something inside the window.
            assert!(sub != crop_bgra(&background()) || name.contains("outside"), "{name}: no-op");
        }
    }

    #[test]
    fn offset_bgra_fb_matches_full_rgba() {
        use equiv::check_fb;
        let c = C4::new(220, 40, 90, 200);
        let solid = C4::rgb(10, 250, 120);
        check_fb("fill_rect straddling", |f| f.fill_rect(20, 10, 30, 20, c));
        check_fb("fill_rect solid", |f| f.fill_rect(55, 35, 30, 30, solid));
        check_fb("fill_rect all edges", |f| f.fill_rect(10, 5, 80, 60, c));
        check_fb("stroke_rect", |f| f.stroke_rect(25.5, 15.25, 30.0, 25.0, 3.0, c));
        check_fb("stroke_rect big outside", |f| f.stroke_rect(10.0, 5.0, 80.0, 60.0, 2.0, c));
        check_fb("fill_rounded", |f| f.fill_rounded(22, 12, 30, 20, 6.0, c));
        check_fb("fill_rounded solid", |f| f.fill_rounded(50, 30, 40, 30, 8.0, solid));
        check_fb("fill_circle", |f| f.fill_circle(34.0, 24.0, 9.5, c));
        check_fb("stroke_circle", |f| f.stroke_circle(68.0, 48.0, 11.0, 2.5, c));
        check_fb("stroke_rounded", |f| {
            f.stroke_rounded(FRect { x: 40.0, y: 15.0, w: 50.0, h: 30.0 }, 7.0, 2.0, c)
        });
        check_fb("stroke_dashed_rect", |f| {
            f.stroke_dashed_rect(FRect { x: 38.5, y: 15.5, w: 55.0, h: 28.0 }, 5.0, 3.0, 1.5, c)
        });
        // Entirely outside the window: still identical (and untouched).
        check_fb("outside", |f| f.fill_rect(0, 0, 10, 10, c));
        if let Some(font) = font() {
            check_fb("text", |f| {
                f.draw_text(font, 14.0, "Hello wy", 24.0, 12.0, c);
            });
        }
    }

    #[test]
    fn offset_bgra_surf_primitives_match_full_rgba() {
        use crate::raster::{Blend, Surf};
        use equiv::check;
        let c = C4::new(30, 200, 240, 190);
        let pts = [Pt::new(15.0, 60.0), Pt::new(45.0, 18.0), Pt::new(85.0, 50.0)];
        for (blend, name) in [(Blend::Normal, "normal"), (Blend::Multiply, "multiply")] {
            check(&format!("polyline {name}"), |s: &mut Surf| {
                s.stroke_polyline(&pts, 6.0, c, blend)
            });
            check(&format!("ellipse {name}"), |s: &mut Surf| {
                s.stroke_ellipse(FRect { x: 20.0, y: 10.0, w: 60.0, h: 40.0 }, 3.0, c, blend)
            });
            check(&format!("round_rect {name}"), |s: &mut Surf| {
                s.stroke_round_rect(FRect { x: 22.0, y: 12.0, w: 70.0, h: 50.0 }, 6.0, 3.0, c, blend)
            });
            check(&format!("convex {name}"), |s: &mut Surf| s.fill_convex(&pts, c, blend));
        }
    }
}
