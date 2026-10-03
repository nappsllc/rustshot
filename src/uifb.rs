//! Software framebuffer helpers: alpha blending, rectangles, image blits,
//! and ab_glyph text rendering. All drawing happens on unpremultiplied BGRA
//! (the present format) or RGBA as requested by the caller.

use crate::pixbuf::PixBuf;
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
        C4 {
            r,
            g,
            b,
            a: 255,
        }
    }

    pub const fn black_alpha(a: u8) -> Self {
        C4 {
            r: 0,
            g: 0,
            b: 0,
            a,
        }
    }

    pub const fn from_rgba8(c: [u8; 4]) -> Self {
        C4 {
            r: c[0],
            g: c[1],
            b: c[2],
            a: c[3],
        }
    }

    pub fn to_rgba8(self) -> [u8; 4] {
        [self.r, self.g, self.b, self.a]
    }

    pub fn with_alpha(self, a: u8) -> Self {
        C4 { a, ..self }
    }

    pub fn lighten(self, amt: u8) -> Self {
        C4 {
            r: self.r.saturating_add(amt),
            g: self.g.saturating_add(amt),
            b: self.b.saturating_add(amt),
            ..self
        }
    }
}

/// `fb` is RGBA (unpremultiplied). Blend `c` over the pixel at (x, y).
pub fn blend_px(fb: &mut [u8], stride: usize, x: usize, y: usize, c: C4) {
    let i = (y * stride + x) * 4;
    if i + 3 >= fb.len() {
        return;
    }
    let a = c.a as u32;
    if a == 255 {
        fb[i] = c.r;
        fb[i + 1] = c.g;
        fb[i + 2] = c.b;
        fb[i + 3] = 255;
        return;
    }
    if a == 0 {
        return;
    }
    let ia = 255 - a;
    let (d0, d1, d2, d3) = (fb[i] as u32, fb[i + 1] as u32, fb[i + 2] as u32, fb[i + 3] as u32);
    fb[i] = ((c.r as u32 * a + d0 * ia) / 255) as u8;
    fb[i + 1] = ((c.g as u32 * a + d1 * ia) / 255) as u8;
    fb[i + 2] = ((c.b as u32 * a + d2 * ia) / 255) as u8;
    fb[i + 3] = (a + (d3 * ia) / 255).min(255) as u8;
}

pub fn fill_rect(fb: &mut [u8], stride: usize, x0: i32, y0: i32, w: i32, h: i32, c: C4) {
    if w <= 0 || h <= 0 {
        return;
    }
    for y in y0.max(0)..(y0 + h).min(fb.len() as i32 / stride as i32) {
        for x in x0.max(0)..(x0 + w).min(stride as i32) {
            blend_px(fb, stride, x as usize, y as usize, c);
        }
    }
}

/// Rectangle outline with simple sub-pixel coverage around the border.
pub fn stroke_rect(
    fb: &mut [u8],
    stride: usize,
    x0: f32,
    y0: f32,
    w: f32,
    h: f32,
    thickness: f32,
    c: C4,
) {
    if w <= 0.0 || h <= 0.0 || thickness <= 0.0 {
        return;
    }
    let (x1, y1) = (x0 + w, y0 + h);
    let half = thickness / 2.0;
    let fw = fb.len() as i32 / 4 / stride as i32;
    let ys = y0.floor().max(0.0) as i32;
    let ye = ((y1 + half).ceil() as i32).min(fw);
    let xs = x0.floor().max(0.0) as i32;
    let xe = ((x1 + half).ceil() as i32).min(stride as i32);
    for y in ys..ye {
        for x in xs..xe {
            let cx = x as f32 + 0.5;
            let cy = y as f32 + 0.5;
            // Signed distance to the rectangle (negative inside).
            let qx = (cx - x0 - w / 2.0).abs() - w / 2.0;
            let qy = (cy - y0 - h / 2.0).abs() - h / 2.0;
            let dx = qx.max(0.0);
            let dy = qy.max(0.0);
            let sdf = (dx * dx + dy * dy).sqrt() + qx.min(qy).min(0.0);
            let cov = (half + 0.5 - sdf.abs()).clamp(0.0, 1.0);
            if cov > 0.0 {
                let mut col = c;
                col.a = ((c.a as f32 * cov) as u32).min(255) as u8;
                blend_px(fb, stride, x as usize, y as usize, col);
            }
        }
    }
}

/// Filled rounded rectangle (corner radius in px).
pub fn fill_rounded(
    fb: &mut [u8],
    stride: usize,
    x0: i32,
    y0: i32,
    w: i32,
    h: i32,
    r: f32,
    c: C4,
) {
    if w <= 0 || h <= 0 {
        return;
    }
    let r = r.min(w as f32 / 2.0).min(h as f32 / 2.0);
    let fw = fb.len() as i32 / 4 / stride as i32;
    for y in y0.max(0)..(y0 + h).min(fw) {
        for x in x0.max(0)..(x0 + w).min(stride as i32) {
            let mut a = c.a;
            if r > 0.0 {
                let cx = x as f32 + 0.5;
                let cy = y as f32 + 0.5;
                let inx0 = (x0 as f32) + r;
                let inx1 = (x0 + w) as f32 - r;
                let iny0 = (y0 as f32) + r;
                let iny1 = (y0 + h) as f32 - r;
                if (cx < inx0 && cy < iny0)
                    || (cx > inx1 && cy < iny0)
                    || (cx < inx0 && cy > iny1)
                    || (cx > inx1 && cy > iny1)
                {
                    let qx = if cx < inx0 { inx0 - cx } else { cx - inx1 };
                    let qy = if cy < iny0 { iny0 - cy } else { cy - iny1 };
                    let d = (qx * qx + qy * qy).sqrt();
                    let cov = (r + 0.5 - d).clamp(0.0, 1.0);
                    if cov <= 0.0 {
                        continue;
                    }
                    a = ((a as f32) * cov) as u8;
                }
            }
            let mut col = c;
            col.a = a;
            blend_px(fb, stride, x as usize, y as usize, col);
        }
    }
}

/// Alpha-blit an RGBA source image onto the RGBA framebuffer.
pub fn blit(fb: &mut [u8], stride: usize, x0: i32, y0: i32, src: &PixBuf) {
    let (sw, sh) = src.dimensions();
    let s = src.as_raw();
    let fw = fb.len() as i32 / 4 / stride as i32;
    for y in 0..sh as i32 {
        let dy = y0 + y;
        if dy < 0 || dy >= fw {
            continue;
        }
        for x in 0..sw as i32 {
            let dx = x0 + x;
            if dx < 0 || dx >= stride as i32 {
                continue;
            }
            let si = ((y as usize) * sw as usize + x as usize) * 4;
            let c = C4::new(s[si], s[si + 1], s[si + 2], s[si + 3]);
            blend_px(fb, stride, dx as usize, dy as usize, c);
        }
    }
}

// ---------------------------------------------------------------------------
// Text
// ---------------------------------------------------------------------------

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

/// Draw `s` with the top-left corner at (x, y). Returns the advance width.
pub fn draw_text(
    fb: &mut [u8],
    stride: usize,
    font: &FontArc,
    px: f32,
    s: &str,
    x: f32,
    y: f32,
    c: C4,
) -> f32 {
    let f = scaled(font, px);
    let baseline = y + f.ascent();
    let mut pen = x;
    let fw = fb.len() as i32 / 4 / stride as i32;
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
                if sx < 0 || sy < 0 || sx >= stride as i32 || sy >= fw {
                    return;
                }
                let mut col = c;
                col.a = ((c.a as f32 * cov) as u32).min(255) as u8;
                blend_px(fb, stride, sx as usize, sy as usize, col);
            });
        }
        pen += f.h_advance(font.glyph_id(ch));
    }
    pen - x
}

/// Draw `s` centered horizontally at `cx` with vertical center `cy`.
pub fn draw_text_centered(
    fb: &mut [u8],
    stride: usize,
    font: &FontArc,
    px: f32,
    s: &str,
    cx: f32,
    cy: f32,
    c: C4,
) {
    let w = text_width(font, px, s);
    let h = text_height(font, px);
    draw_text(fb, stride, font, px, s, cx - w / 2.0, cy - h / 2.0, c);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn font() -> FontArc {
        FontArc::try_from_vec(std::fs::read("C:\\Windows\\Fonts\\segoeui.ttf").unwrap())
            .unwrap()
    }

    #[test]
    fn blend_solid_and_alpha() {
        let stride = 4;
        let mut fb = vec![100u8; 4 * 4];
        blend_px(&mut fb, stride, 0, 0, C4::rgb(200, 0, 0));
        assert_eq!(&fb[0..4], &[200, 0, 0, 255]);
        blend_px(&mut fb, stride, 1, 0, C4::new(0, 0, 0, 128));
        // 100 * (1 - 128/255) ≈ 49
        assert!(fb[4] > 35 && fb[4] < 60, "got {}", fb[4]);
    }

    #[test]
    fn fill_and_stroke() {
        let stride = 20;
        let mut fb = vec![0u8; stride * 20 * 4];
        fill_rect(&mut fb, stride, 2, 2, 5, 5, C4::rgb(255, 0, 0));
        assert_eq!(fb[(2 * stride + 2) * 4], 255);
        assert_eq!(fb[(9 * stride + 9) * 4], 0);
        stroke_rect(&mut fb, stride, 4.0, 4.0, 8.0, 8.0, 1.5, C4::rgb(0, 255, 0));
        let mut found = false;
        for i in 0..fb.len() / 4 {
            if fb[i * 4 + 1] > 100 && fb[i * 4] < 100 {
                found = true;
            }
        }
        assert!(found, "stroke produced no green pixels");
    }

    #[test]
    fn text_measures_and_draws() {
        let f = font();
        let w = text_width(&f, 14.0, "Hello");
        assert!(w > 20.0 && w < 200.0, "width {w}");
        let stride = 120;
        let mut fb = vec![0u8; stride * 30 * 4];
        let adv = draw_text(&mut fb, stride, &f, 14.0, "Hi", 4.0, 4.0, C4::rgb(255, 255, 255));
        assert!(adv > 5.0);
        let lit = fb.chunks_exact(4).filter(|p| p[0] > 128).count();
        assert!(lit > 20, "only {lit} lit pixels");
    }

    #[test]
    fn rounded_fill_covers_center() {
        let stride = 30;
        let mut fb = vec![0u8; stride * 30 * 4];
        fill_rounded(&mut fb, stride, 0, 0, 30, 30, 6.0, C4::rgb(10, 20, 30));
        assert_eq!(fb[(15 * stride + 15) * 4 + 1], 20);
        // corner pixel outside radius blend zone stays empty-ish
        assert_eq!(fb[0], 0);
    }
}
