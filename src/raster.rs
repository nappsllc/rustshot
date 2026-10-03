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

/// A mutable draw target: raw RGBA bytes plus its dimensions.
pub struct Surf<'a> {
    pub data: &'a mut [u8],
    w: u32,
    h: u32,
}

impl<'a> Surf<'a> {
    #[cfg(test)]
    pub fn new(data: &'a mut [u8], w: u32, h: u32) -> Self {
        Surf { data, w, h }
    }

    pub fn from_buf(buf: &'a mut PixBuf) -> Self {
        let (w, h) = (buf.width(), buf.height());
        Surf {
            data: buf.as_raw_mut(),
            w,
            h,
        }
    }

    pub fn width(&self) -> u32 {
        self.w
    }

    pub fn height(&self) -> u32 {
        self.h
    }

    /// Blend one pixel: straight-alpha source over an opaque destination.
    pub fn blend_px(&mut self, x: i32, y: i32, c: C4, cov: f32, blend: Blend) {
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
        for (i, sv) in src.iter().enumerate() {
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
        let (w, h) = (self.w, self.h);
        let (x0, y0, x1, y1) = clamp_bbox(
            (minx - margin).floor() as i32,
            (miny - margin).floor() as i32,
            (maxx + margin).ceil() as i32,
            (maxy + margin).ceil() as i32,
            w,
            h,
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
                w,
                h,
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
        let (w, h) = (self.w, self.h);
        let (x0, y0, x1, y1) = clamp_bbox(
            (cx - rx - band).floor() as i32,
            (cy - ry - band).floor() as i32,
            (cx + rx + band).ceil() as i32,
            (cy + ry + band).ceil() as i32,
            w,
            h,
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
        let (w, h) = (self.w, self.h);
        let (x0, y0, x1, y1) = clamp_bbox(
            (r.x - band).floor() as i32,
            (r.y - band).floor() as i32,
            (r.x1() + band).ceil() as i32,
            (r.y1() + band).ceil() as i32,
            w,
            h,
        );
        for y in y0..y1 {
            for x in x0..x1 {
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
        let (w, h) = (self.w, self.h);
        let (x0, y0, x1, y1) = clamp_bbox(
            minx.floor() as i32,
            miny.floor() as i32,
            maxx.ceil() as i32,
            maxy.ceil() as i32,
            w,
            h,
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

/// Intersect an integer range with the buffer bounds.
fn clamp_bbox(x0: i32, y0: i32, x1: i32, y1: i32, w: u32, h: u32) -> (i32, i32, i32, i32) {
    (x0.max(0), y0.max(0), x1.min(w as i32), y1.min(h as i32))
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
