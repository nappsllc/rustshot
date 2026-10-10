//! Hand-rolled anti-aliased raster primitives for baking annotation objects
//! into an opaque RGBA buffer. Replaces the `tiny-skia` dependency.
//!
//! Strokes use signed-distance coverage (`band - dist`, band = half-width + 0.5
//! at pixel centers), which yields round caps and joins for free. Polylines
//! accumulate coverage into a scratch mask with `max`, so joints blend exactly
//! once with the minimum distance to any segment.

use crate::objects::{FRect, Pt};
use crate::pixbuf::PixBuf;
use crate::uifb::C4;

/// How a source color combines with the destination.
#[derive(Clone, Copy, PartialEq)]
pub enum Blend {
    Normal,
    /// PDF-style multiply weighted by source alpha (the highlighter marker).
    Multiply,
}

/// Byte order of a 4-byte pixel.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Order {
    #[default]
    Rgba,
    /// Windows DIB sections (the GDI overlay); built by the overlay path.
    #[cfg_attr(not(test), allow(dead_code))]
    Bgra,
}

impl Order {
    /// Byte index of the r, g, b channels within a pixel.
    #[inline(always)]
    pub fn rgb_idx(self) -> [usize; 3] {
        match self {
            Order::Rgba => [0, 1, 2],
            Order::Bgra => [2, 1, 0],
        }
    }
}

/// A mutable draw target: raw pixel bytes plus its dimensions. The buffer
/// covers image coordinates `ox..ox + w` x `oy..oy + h`; every draw call
/// takes image coordinates and clips to that window.
pub struct Surf<'a> {
    pub data: &'a mut [u8],
    w: u32,
    h: u32,
    ox: i32,
    oy: i32,
    order: Order,
}

impl<'a> Surf<'a> {
    pub fn new(data: &'a mut [u8], w: u32, h: u32) -> Self {
        Self::with_origin(data, w, h, 0, 0, Order::Rgba)
    }

    /// A surface whose top-left pixel is image pixel (`ox`, `oy`).
    pub fn with_origin(
        data: &'a mut [u8],
        w: u32,
        h: u32,
        ox: i32,
        oy: i32,
        order: Order,
    ) -> Self {
        Surf {
            data,
            w,
            h,
            ox,
            oy,
            order,
        }
    }

    pub fn from_buf(buf: &'a mut PixBuf) -> Self {
        let (w, h) = (buf.width(), buf.height());
        Surf::new(buf.as_raw_mut(), w, h)
    }

    /// The window in image coordinates: (x0, y0, x1, y1), end-exclusive.
    pub fn bounds(&self) -> (i32, i32, i32, i32) {
        (self.ox, self.oy, self.ox + self.w as i32, self.oy + self.h as i32)
    }

    pub fn width(&self) -> u32 {
        self.w
    }

    /// Blend one pixel: straight-alpha source over an opaque destination.
    pub fn blend_px(&mut self, x: i32, y: i32, c: C4, cov: f32, blend: Blend) {
        let (x, y) = (x - self.ox, y - self.oy);
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
            return;
        }
        let idx = (y as u32 * self.w + x as u32) as usize * 4;
        if idx + 3 >= self.data.len() {
            return;
        }
        let a = (c.a as f32 * cov.clamp(0.0, 1.0)) / 255.0;
        if a <= 0.0 {
            return;
        }
        let inv = 1.0 - a;
        let src = [c.r, c.g, c.b];
        let ch = self.order.rgb_idx();
        for (sv, &i) in src.iter().zip(&ch) {
            let dv = self.data[idx + i] as f32;
            let sv = *sv as f32;
            let v = match blend {
                Blend::Normal => sv * a + dv * inv,
                Blend::Multiply => dv * inv + dv * (sv / 255.0) * a,
            };
            self.data[idx + i] = v.round().clamp(0.0, 255.0) as u8;
        }
        self.data[idx + 3] = 255;
    }

    /// Stroke a polyline (or a single point) with round caps/joins and AA.
    pub fn stroke_polyline(&mut self, pts: &[Pt], width: f32, c: C4, blend: Blend) {
        if pts.is_empty() {
            return;
        }
        let half = width.max(0.5) / 2.0;
        let margin = half + 1.0;
        let mut minx = f32::MAX;
        let mut miny = f32::MAX;
        let mut maxx = f32::MIN;
        let mut maxy = f32::MIN;
        for p in pts {
            minx = minx.min(p.x);
            miny = miny.min(p.y);
            maxx = maxx.max(p.x);
            maxy = maxy.max(p.y);
        }
        let bounds = self.bounds();
        let (x0, y0, x1, y1) = clamp_bbox(
            (minx - margin).floor() as i32,
            (miny - margin).floor() as i32,
            (maxx + margin).ceil() as i32,
            (maxy + margin).ceil() as i32,
            bounds,
        );
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let (mw, mh) = ((x1 - x0) as u32, (y1 - y0) as u32);
        let mut mask = vec![0u8; (mw * mh) as usize];
        let band = half + 0.5;
        let mut segs: Vec<(Pt, Pt)> = Vec::with_capacity(pts.len());
        if pts.len() == 1 {
            segs.push((pts[0], pts[0]));
        } else {
            for s in pts.windows(2) {
                segs.push((s[0], s[1]));
            }
        }
        for (a, b) in segs {
            let (lx0, ly0, lx1, ly1) = clamp_bbox(
                (a.x.min(b.x) - margin).floor() as i32,
                (a.y.min(b.y) - margin).floor() as i32,
                (a.x.max(b.x) + margin).ceil() as i32,
                (a.y.max(b.y) + margin).ceil() as i32,
                bounds,
            );
            for y in ly0.max(y0)..ly1.min(y1) {
                for x in lx0.max(x0)..lx1.min(x1) {
                    let d = dist_seg(x as f32 + 0.5, y as f32 + 0.5, a.x, a.y, b.x, b.y);
                    let cov = (band - d).clamp(0.0, 1.0);
                    if cov > 0.0 {
                        let mi = ((y - y0) as u32 * mw + (x - x0) as u32) as usize;
                        let v = (cov * 255.0).round() as u8;
                        if v > mask[mi] {
                            mask[mi] = v;
                        }
                    }
                }
            }
        }
        for y in 0..mh {
            for x in 0..mw {
                let v = mask[(y * mw + x) as usize];
                if v > 0 {
                    self.blend_px(x0 + x as i32, y0 + y as i32, c, v as f32 / 255.0, blend);
                }
            }
        }
    }

    /// Stroke an ellipse via its signed-distance field.
    pub fn stroke_ellipse(&mut self, r: FRect, width: f32, c: C4, blend: Blend) {
        let rx = (r.w / 2.0).max(0.5);
        let ry = (r.h / 2.0).max(0.5);
        let cx = r.x + r.w / 2.0;
        let cy = r.y + r.h / 2.0;
        let band = width.max(0.5) / 2.0 + 0.5;
        let bounds = self.bounds();
        let (x0, y0, x1, y1) = clamp_bbox(
            (cx - rx - band).floor() as i32,
            (cy - ry - band).floor() as i32,
            (cx + rx + band).ceil() as i32,
            (cy + ry + band).ceil() as i32,
            bounds,
        );
        for y in y0..y1 {
            for x in x0..x1 {
                let (px, py) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
                let (gx, gy) = (px / (rx * rx), py / (ry * ry));
                let gnorm = 2.0 * (gx * gx + gy * gy).sqrt();
                let k = (px / rx) * (px / rx) + (py / ry) * (py / ry);
                let d = if gnorm < 1e-9 {
                    f32::MAX
                } else {
                    (k - 1.0) / gnorm
                };
                let cov = (band - d.abs()).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.blend_px(x, y, c, cov, blend);
                }
            }
        }
    }

    /// Stroke a rounded rectangle via the exact rounded-box SDF.
    pub fn stroke_round_rect(
        &mut self,
        r: FRect,
        radius: f32,
        width: f32,
        c: C4,
        blend: Blend,
    ) {
        let rad = radius.min(r.w / 2.0).min(r.h / 2.0).max(0.0);
        let band = width.max(0.5) / 2.0 + 0.5;
        let cx = r.x + r.w / 2.0;
        let cy = r.y + r.h / 2.0;
        let hx = (r.w / 2.0 - rad).max(0.0);
        let hy = (r.h / 2.0 - rad).max(0.0);
        let bounds = self.bounds();
        let (x0, y0, x1, y1) = clamp_bbox(
            (r.x - band).floor() as i32,
            (r.y - band).floor() as i32,
            (r.x1() + band).ceil() as i32,
            (r.y1() + band).ceil() as i32,
            bounds,
        );
        // Interior pixels well inside the outline (q < lim on both axes:
        // d <= -band - 0.5, zero coverage) are skipped, so a large shape
        // (a rect being dragged out) costs its border, not its area.
        let lim = 0.0f32.min(rad - band) - 0.5;
        for y in y0..y1 {
            let qy_row = (y as f32 + 0.5 - cy).abs() - hy;
            let (skip0, skip1) = if qy_row < lim {
                // |x + 0.5 - cx| - hx < lim for every x in [skip0, skip1).
                (((cx - hx - lim).ceil() as i32).max(x0), ((cx + hx + lim).floor() as i32).min(x1))
            } else {
                (x1, x1)
            };
            for x in (x0..skip0.min(x1)).chain(skip1.max(skip0).max(x0)..x1) {
                let px = (x as f32 + 0.5 - cx).abs();
                let py = (y as f32 + 0.5 - cy).abs();
                let qx = px - hx;
                let qy = py - hy;
                let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
                let inside = qx.max(qy).min(0.0);
                let d = outside + inside - rad;
                let cov = (band - d.abs()).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.blend_px(x, y, c, cov, blend);
                }
            }
        }
    }

    /// Fill a convex polygon with analytic edge coverage.
    pub fn fill_convex(&mut self, pts: &[Pt], c: C4, blend: Blend) {
        if pts.len() < 3 {
            return;
        }
        let n = pts.len();
        let mut area = 0.0;
        for i in 0..n {
            let a = pts[i];
            let b = pts[(i + 1) % n];
            area += a.x * b.y - b.x * a.y;
        }
        if area == 0.0 {
            return;
        }
        let orient = if area > 0.0 { 1.0 } else { -1.0 };
        let mut minx = f32::MAX;
        let mut miny = f32::MAX;
        let mut maxx = f32::MIN;
        let mut maxy = f32::MIN;
        for p in pts {
            minx = minx.min(p.x);
            miny = miny.min(p.y);
            maxx = maxx.max(p.x);
            maxy = maxy.max(p.y);
        }
        let bounds = self.bounds();
        let (x0, y0, x1, y1) = clamp_bbox(
            minx.floor() as i32,
            miny.floor() as i32,
            maxx.ceil() as i32,
            maxy.ceil() as i32,
            bounds,
        );
        for y in y0..y1 {
            for x in x0..x1 {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let mut min_d = f32::MAX;
                for i in 0..n {
                    let a = pts[i];
                    let b = pts[(i + 1) % n];
                    let (ex, ey) = (b.x - a.x, b.y - a.y);
                    let len = (ex * ex + ey * ey).sqrt().max(1e-6);
                    let cross = ex * (py - a.y) - ey * (px - a.x);
                    let d = orient * cross / len;
                    if d < min_d {
                        min_d = d;
                    }
                }
                let cov = (min_d + 0.5).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.blend_px(x, y, c, cov, blend);
                }
            }
        }
    }
}

/// Filled-outline coverage on a `w` x `h` grid (nonzero winding, exact
/// area coverage): each edge deposits signed area into an accumulation
/// buffer, and a running prefix sum yields per-pixel coverage. This is the
/// font-rs / ab_glyph_rasterizer algorithm (same arithmetic, so glyphs
/// come out as they did with ab_glyph); used for the baked UI font.
pub struct Coverage {
    w: usize,
    h: usize,
    a: Vec<f32>,
}

impl Coverage {
    pub fn new(w: usize, h: usize) -> Self {
        Coverage { w, h, a: vec![0.0; w * h + 4] }
    }

    fn add(&mut self, i: usize, v: f32) -> bool {
        match self.a.get_mut(i) {
            Some(p) => {
                *p += v;
                true
            }
            None => false,
        }
    }

    /// Edge `p0` -> `p1` in grid coordinates (y down).
    pub fn line(&mut self, p0: Pt, p1: Pt) {
        if (p0.y - p1.y).abs() <= f32::EPSILON {
            return;
        }
        let (dir, p0, p1) = if p0.y < p1.y { (1.0, p0, p1) } else { (-1.0, p1, p0) };
        let dxdy = (p1.x - p0.x) / (p1.y - p0.y);
        let mut x = p0.x;
        if p0.y < 0.0 {
            x -= p0.y * dxdy;
        }
        // `as usize` saturates negative rows to 0.
        for y in p0.y as usize..self.h.min(p1.y.ceil() as usize) {
            let row = y * self.w;
            let dy = ((y + 1) as f32).min(p1.y) - (y as f32).max(p0.y);
            let xnext = x + dxdy * dy;
            let d = dy * dir;
            let (x0, x1) = if x < xnext { (x, xnext) } else { (xnext, x) };
            let x0floor = x0.floor();
            let x0i = x0floor as i32;
            let x1ceil = x1.ceil();
            let x1i = x1ceil as i32;
            let start = row as isize + x0i as isize;
            // An out-of-grid index abandons the rest of the row (and, as in
            // the reference rasteriser, keeps `x` where it was).
            if start < 0 {
                continue;
            }
            let start = start as usize;
            let at = |xi: i32| row.wrapping_add(xi as usize);
            if x1i <= x0i + 1 {
                let xmf = 0.5 * (x + xnext) - x0floor;
                if !self.add(start, d - d * xmf) || !self.add(start + 1, d * xmf) {
                    continue;
                }
            } else {
                let s = (x1 - x0).recip();
                let x0f = x0 - x0floor;
                let a0 = 0.5 * s * (1.0 - x0f) * (1.0 - x0f);
                let x1f = x1 - x1ceil + 1.0;
                let am = 0.5 * s * x1f * x1f;
                if !self.add(start, d * a0) {
                    continue;
                }
                if x1i == x0i + 2 {
                    if !self.add(start + 1, d * (1.0 - a0 - am)) {
                        continue;
                    }
                } else {
                    let a1 = s * (1.5 - x0f);
                    if !self.add(start + 1, d * (a1 - a0)) {
                        continue;
                    }
                    for xi in x0i + 2..x1i - 1 {
                        self.add(at(xi), d * s);
                    }
                    let a2 = a1 + (x1i - x0i - 3) as f32 * s;
                    if !self.add(at(x1i - 1), d * (1.0 - a2 - am)) {
                        continue;
                    }
                }
                if !self.add(at(x1i), d * am) {
                    continue;
                }
            }
            x = xnext;
        }
    }

    /// Quadratic Bézier `p0` -> `p2` with control `p1`, flattened.
    pub fn quad(&mut self, p0: Pt, p1: Pt, p2: Pt) {
        let devx = p0.x - 2.0 * p1.x + p2.x;
        let devy = p0.y - 2.0 * p1.y + p2.y;
        let devsq = devx * devx + devy * devy;
        if devsq < 0.333 {
            self.line(p0, p2);
            return;
        }
        let n = 1 + (3.0 * devsq).sqrt().sqrt().floor() as usize;
        let lerp = |t: f32, a: Pt, b: Pt| Pt::new(a.x + t * (b.x - a.x), a.y + t * (b.y - a.y));
        let (mut p, step, mut t) = (p0, (n as f32).recip(), 0.0);
        for _ in 0..n - 1 {
            t += step;
            let pn = lerp(t, lerp(t, p0, p1), lerp(t, p1, p2));
            self.line(p, pn);
            p = pn;
        }
        self.line(p, p2);
    }

    /// Visit every cell with its coverage (0 = empty, 1 or more = full).
    pub fn for_each(&self, mut f: impl FnMut(u32, u32, f32)) {
        let mut acc = 0.0f32;
        for (i, c) in self.a[..self.w * self.h].iter().enumerate() {
            acc += c;
            f((i % self.w) as u32, (i / self.w) as u32, acc.abs());
        }
    }
}

/// Signed distance from `p` to segment `a..b` (endpoints clamped).
fn dist_seg(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let (vx, vy) = (bx - ax, by - ay);
    let (wx, wy) = (px - ax, py - ay);
    let len2 = vx * vx + vy * vy;
    let t = if len2 <= 1e-12 {
        0.0
    } else {
        ((wx * vx + wy * vy) / len2).clamp(0.0, 1.0)
    };
    let (dx, dy) = (wx - t * vx, wy - t * vy);
    (dx * dx + dy * dy).sqrt()
}

/// Intersect an integer range with the surface window (image coordinates).
fn clamp_bbox(
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    b: (i32, i32, i32, i32),
) -> (i32, i32, i32, i32) {
    (x0.max(b.0), y0.max(b.1), x1.min(b.2), y1.min(b.3))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(data: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * w + x) as usize) * 4;
        [data[i], data[i + 1], data[i + 2], data[i + 3]]
    }

    #[test]
    fn line_covers_center_spares_far_corner() {
        let mut d = vec![0u8; 20 * 20 * 4];
        Surf::new(&mut d, 20, 20).stroke_polyline(
            &[Pt::new(2.0, 2.0), Pt::new(17.0, 17.0)],
            3.0,
            C4::rgb(255, 0, 0),
            Blend::Normal,
        );
        let mid = px(&d, 20, 10, 10);
        assert!(mid[0] > 200, "center covered: {mid:?}");
        assert_eq!(mid[3], 255);
        assert_eq!(px(&d, 20, 18, 1), [0, 0, 0, 0], "far corner untouched");
    }

    #[test]
    fn polyline_joint_blends_once() {
        // Two segments meeting at (10,10): joint pixels must not double-blend.
        let mut d = vec![0u8; 20 * 20 * 4];
        Surf::new(&mut d, 20, 20).stroke_polyline(
            &[
                Pt::new(2.0, 10.0),
                Pt::new(10.0, 10.0),
                Pt::new(10.0, 2.0),
            ],
            4.0,
            C4::rgb(0, 255, 0),
            Blend::Normal,
        );
        let joint = px(&d, 20, 10, 10);
        assert_eq!(joint[1], 255, "joint fully covered: {joint:?}");
        assert_eq!(joint[0], 0, "no double-darkening");
    }

    #[test]
    fn ellipse_ring_not_center() {
        let mut d = vec![0u8; 30 * 30 * 4];
        Surf::new(&mut d, 30, 30).stroke_ellipse(
            FRect {
                x: 5.0,
                y: 5.0,
                w: 20.0,
                h: 20.0,
            },
            3.0,
            C4::rgb(0, 0, 255),
            Blend::Normal,
        );
        assert_eq!(px(&d, 30, 15, 15), [0, 0, 0, 0], "center is hollow");
        let ring = px(&d, 30, 15, 5);
        assert!(ring[2] > 150, "top of ring covered: {ring:?}");
        assert_eq!(px(&d, 30, 1, 1), [0, 0, 0, 0], "outside untouched");
    }

    /// Skipping the hollow interior paints exactly what a full sweep does.
    #[test]
    fn round_rect_stroke_matches_full_sweep() {
        let c = C4::new(10, 200, 90, 220);
        for &(r, rad, width) in &[
            (FRect { x: 5.0, y: 5.0, w: 20.0, h: 20.0 }, 3.0, 2.0),
            (FRect { x: 3.4, y: 6.6, w: 110.3, h: 70.1 }, 8.0, 3.0),
            (FRect { x: 10.0, y: 10.0, w: 90.0, h: 4.0 }, 6.0, 5.0),
            (FRect { x: 2.0, y: 2.0, w: 100.0, h: 60.0 }, 0.0, 1.0),
        ] {
            let (w, h) = (120u32, 90u32);
            let mut a = vec![0u8; (w * h * 4) as usize];
            Surf::new(&mut a, w, h).stroke_round_rect(r, rad, width, c, Blend::Normal);
            let mut b = vec![0u8; (w * h * 4) as usize];
            let mut s = Surf::new(&mut b, w, h);
            let rad = rad.min(r.w / 2.0).min(r.h / 2.0).max(0.0);
            let band = width.max(0.5) / 2.0 + 0.5;
            let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
            let (hx, hy) = ((r.w / 2.0 - rad).max(0.0), (r.h / 2.0 - rad).max(0.0));
            for y in 0..h as i32 {
                for x in 0..w as i32 {
                    let qx = (x as f32 + 0.5 - cx).abs() - hx;
                    let qy = (y as f32 + 0.5 - cy).abs() - hy;
                    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
                    let d = outside + qx.max(qy).min(0.0) - rad;
                    let cov = (band - d.abs()).clamp(0.0, 1.0);
                    if cov > 0.0 {
                        s.blend_px(x, y, c, cov, Blend::Normal);
                    }
                }
            }
            assert!(a == b, "{r:?} rad {rad} width {width}");
        }
    }

    #[test]
    fn round_rect_stroke_band() {
        let mut d = vec![0u8; 30 * 30 * 4];
        Surf::new(&mut d, 30, 30).stroke_round_rect(
            FRect {
                x: 5.0,
                y: 5.0,
                w: 20.0,
                h: 20.0,
            },
            3.0,
            2.0,
            C4::rgb(255, 255, 0),
            Blend::Normal,
        );
        assert_eq!(px(&d, 30, 15, 15), [0, 0, 0, 0], "interior hollow");
        assert!(
            px(&d, 30, 5, 15)[0] > 150,
            "left edge covered: {:?}",
            px(&d, 30, 5, 15)
        );
        assert_eq!(px(&d, 30, 1, 1), [0, 0, 0, 0], "outside untouched");
    }

    #[test]
    fn triangle_fill_inside_outside() {
        let mut d = vec![0u8; 20 * 20 * 4];
        Surf::new(&mut d, 20, 20).fill_convex(
            &[
                Pt::new(2.0, 2.0),
                Pt::new(18.0, 2.0),
                Pt::new(10.0, 18.0),
            ],
            C4::rgb(10, 200, 10),
            Blend::Normal,
        );
        assert!(px(&d, 20, 10, 6)[1] > 180, "inside filled");
        assert_eq!(px(&d, 20, 1, 19), [0, 0, 0, 0], "outside empty");
        assert_eq!(px(&d, 20, 1, 1), [0, 0, 0, 0], "outside empty");
    }

    #[test]
    fn multiply_darkens_destination() {
        let mut d = [100u8, 100, 100, 255, 0, 0, 0, 0];
        Surf::new(&mut d, 1, 1).blend_px(
            0,
            0,
            C4::rgb(200, 200, 200),
            1.0,
            Blend::Multiply,
        );
        let expect = (100.0f32 * (200.0 / 255.0)).round() as u8;
        assert_eq!(d[0], expect, "pdf multiply: {d:?}");
        assert_eq!(d[3], 255);
    }
}
