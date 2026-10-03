use ab_glyph::{Font, FontArc, PxScale, ScaleFont};
use crate::pixbuf::PixBuf;
use crate::raster::{Blend, Surf};
use crate::uifb::C4;

/// A point in image (physical pixel) coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pt {
    pub x: f32,
    pub y: f32,
}

impl Pt {
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// A rectangle in image coordinates, normalized (w/h >= 0).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl FRect {
    pub fn from_pts(a: Pt, b: Pt) -> Self {
        Self {
            x: a.x.min(b.x),
            y: a.y.min(b.y),
            w: (a.x - b.x).abs(),
            h: (a.y - b.y).abs(),
        }
    }
    pub fn x1(&self) -> f32 {
        self.x + self.w
    }
    pub fn y1(&self) -> f32 {
        self.y + self.h
    }
    pub fn is_trivial(&self) -> bool {
        self.w < 1.0 || self.h < 1.0
    }
    pub fn clamp_to(&mut self, w: f32, h: f32) {
        self.w = self.w.min(w);
        self.h = self.h.min(h);
        self.x = self.x.clamp(0.0, (w - self.w).max(0.0));
        self.y = self.y.clamp(0.0, (h - self.h).max(0.0));
    }
}

#[derive(Clone, Debug)]
pub enum Obj {
    Line {
        a: Pt,
        b: Pt,
        color: C4,
        width: f32,
    },
    Arrow {
        a: Pt,
        b: Pt,
        color: C4,
        width: f32,
    },
    Rect {
        r: FRect,
        color: C4,
        width: f32,
    },
    Ellipse {
        r: FRect,
        color: C4,
        width: f32,
    },
    Path {
        pts: Vec<Pt>,
        color: C4,
        width: f32,
    },
    Marker {
        a: Pt,
        b: Pt,
        color: C4,
        width: f32,
    },
    Text {
        pos: Pt,
        text: String,
        color: C4,
        size: f32,
    },
    Pixelate {
        r: FRect,
        cell: f32,
    },
    Invert {
        r: FRect,
    },
}

/// Arrow geometry: shaft endpoints plus the filled head triangle.
fn arrow_geometry(a: Pt, b: Pt, width: f32) -> Option<([Pt; 2], [Pt; 3])> {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 2.0 {
        return None;
    }
    let ux = dx / len;
    let uy = dy / len;
    let hl = (width * 3.0).clamp(9.0, 60.0);
    let hw = hl * 0.45;
    let base = Pt::new(b.x - ux * hl, b.y - uy * hl);
    let px = -uy;
    let py = ux;
    let p1 = Pt::new(base.x + px * hw, base.y + py * hw);
    let p2 = Pt::new(base.x - px * hw, base.y - py * hw);
    Some(([a, base], [b, p1, p2]))
}

fn draw_text(
    sf: &mut Surf,
    font: &FontArc,
    size: f32,
    text: &str,
    top: Pt,
    color: C4,
) {
    let (w, h) = (sf.width(), sf.height());
    let scale = PxScale::from(size);
    let sfont = font.as_scaled(scale);
    let ascent = sfont.ascent();
    let descent = sfont.descent(); // negative
    let gap = sfont.line_gap();
    let line_height = (ascent - descent + gap).max(size * 1.1);
    let mut y = top.y + ascent;
    for line in text.split('\n') {
        let mut x = top.x;
        for ch in line.chars() {
            let gid = font.glyph_id(ch);
            let glyph = gid.with_scale_and_position(scale, ab_glyph::point(x, y));
            if let Some(og) = font.outline_glyph(glyph) {
                let bounds = og.px_bounds();
                og.draw(|gx, gy, cov| {
                    let px = bounds.min.x.floor() as i32 + gx as i32;
                    let py = bounds.min.y.floor() as i32 + gy as i32;
                    if px >= 0 && py >= 0 && (px as u32) < w && (py as u32) < h {
                        sf.blend_px(px, py, color, cov, Blend::Normal);
                    }
                });
            }
            x += sfont.h_advance(gid);
        }
        y += line_height;
    }
}

fn pixelate(sf: &mut Surf, r: FRect, cell: f32) {
    let (pw, ph) = (sf.width() as i32, sf.height() as i32);
    let x0 = r.x.max(0.0).floor() as i32;
    let y0 = r.y.max(0.0).floor() as i32;
    let x1 = r.x1().min(pw as f32).ceil() as i32;
    let y1 = r.y1().min(ph as f32).ceil() as i32;
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let c = cell.max(2.0).round();
    let step = (c / 4.0).max(1.0) as i32;
    let Surf { data, .. } = sf;
    let mut cy = y0;
    while cy < y1 {
        let mut cx = x0;
        while cx < x1 {
            let ex = (cx + c as i32).min(x1);
            let ey = (cy + c as i32).min(y1);
            let (mut sr, mut sg, mut sb, mut sa, mut n) = (0u32, 0u32, 0u32, 0u32, 0u32);
            let mut sy = cy;
            while sy < ey {
                let mut sx = cx;
                while sx < ex {
                    let i = ((sy * pw + sx) as usize) * 4;
                    sr += data[i] as u32;
                    sg += data[i + 1] as u32;
                    sb += data[i + 2] as u32;
                    sa += data[i + 3] as u32;
                    n += 1;
                    sx += step;
                }
                sy += step;
            }
            if n == 0 {
                n = 1;
            }
            let (ar, ag, ab, aa) = (sr / n, sg / n, sb / n, sa / n);
            let mut sy = cy;
            while sy < ey {
                let mut sx = cx;
                while sx < ex {
                    let i = ((sy * pw + sx) as usize) * 4;
                    data[i] = ar as u8;
                    data[i + 1] = ag as u8;
                    data[i + 2] = ab as u8;
                    data[i + 3] = aa as u8;
                    sx += 1;
                }
                sy += 1;
            }
            cx += c as i32;
        }
        cy += c as i32;
    }
}

fn invert(sf: &mut Surf, r: FRect) {
    let (pw, ph) = (sf.width() as i32, sf.height() as i32);
    let x0 = r.x.max(0.0).floor() as i32;
    let y0 = r.y.max(0.0).floor() as i32;
    let x1 = r.x1().min(pw as f32).ceil() as i32;
    let y1 = r.y1().min(ph as f32).ceil() as i32;
    let Surf { data, .. } = sf;
    for y in y0.max(0)..y1.min(ph) {
        for x in x0.max(0)..x1.min(pw) {
            let i = ((y * pw + x) as usize) * 4;
            data[i] = 255 - data[i];
            data[i + 1] = 255 - data[i + 1];
            data[i + 2] = 255 - data[i + 2];
        }
    }
}

impl Obj {
    /// Bake this object into the image.
    pub fn render(&self, buf: &mut PixBuf, font: Option<&FontArc>) {
        let mut sf = Surf::from_buf(buf);
        match self {
            Obj::Line { a, b, color, width } => {
                sf.stroke_polyline(&[*a, *b], *width, *color, Blend::Normal);
            }
            Obj::Marker { a, b, color, width } => {
                sf.stroke_polyline(&[*a, *b], *width, *color, Blend::Multiply);
            }
            Obj::Arrow { a, b, color, width } => {
                if let Some((shaft, head)) = arrow_geometry(*a, *b, *width) {
                    sf.stroke_polyline(&shaft, *width, *color, Blend::Normal);
                    sf.fill_convex(&head, *color, Blend::Normal);
                } else {
                    sf.stroke_polyline(&[*a, *b], *width, *color, Blend::Normal);
                }
            }
            Obj::Rect { r, color, width } => {
                sf.stroke_round_rect(*r, *width, *width, *color, Blend::Normal);
            }
            Obj::Ellipse { r, color, width } => {
                sf.stroke_ellipse(*r, *width, *color, Blend::Normal);
            }
            Obj::Path { pts, color, width } => {
                if pts.len() < 2 {
                    return;
                }
                sf.stroke_polyline(pts, *width, *color, Blend::Normal);
            }
            Obj::Text {
                pos,
                text,
                color,
                size,
            } => {
                if let Some(font) = font {
                    draw_text(&mut sf, font, *size, text, *pos, *color);
                }
            }
            Obj::Pixelate { r, cell } => pixelate(&mut sf, *r, *cell),
            Obj::Invert { r } => invert(&mut sf, *r),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frect_from_pts_normalizes() {
        let r = FRect::from_pts(Pt::new(10.0, 20.0), Pt::new(4.0, 8.0));
        assert_eq!(r.x, 4.0);
        assert_eq!(r.y, 8.0);
        assert_eq!(r.w, 6.0);
        assert_eq!(r.h, 12.0);
    }

    #[test]
    fn renders_line_into_buffer() {
        let mut img = PixBuf::new(20, 20);
        Obj::Line {
            a: Pt::new(2.0, 2.0),
            b: Pt::new(18.0, 18.0),
            color: C4::rgb(255, 0, 0),
            width: 3.0,
        }
        .render(&mut img, None);
        let i = ((10 * 20 + 10) * 4) as usize;
        let d = img.as_raw();
        assert_ne!(d[i], 0, "line should cover the middle pixel");
        assert_eq!(d[i + 3], 255);
    }

    #[test]
    fn pixelate_fills_region() {
        let mut img = PixBuf::new(32, 32);
        // gradient-ish content: set a few distinct pixels
        for i in 0..(32 * 32) {
            let v = (i % 255) as u8;
            let d = &mut img.as_raw_mut()[i * 4..i * 4 + 4];
            d[0] = v;
            d[1] = 255 - v;
            d[2] = 100;
            d[3] = 255;
        }
        Obj::Pixelate {
            r: FRect {
                x: 0.0,
                y: 0.0,
                w: 16.0,
                h: 16.0,
            },
            cell: 8.0,
        }
        .render(&mut img, None);
        // Top-left 8x8 cell must now be a solid color.
        let d = img.as_raw();
        let first = &d[0..4];
        for y in 0..8 {
            for x in 0..8 {
                let i = (y * 32 + x) as usize * 4;
                assert_eq!(&d[i..i + 4], first, "at {x},{y}");
            }
        }
    }

    #[test]
    fn text_render_does_not_panic_without_font() {
        let mut img = PixBuf::new(10, 10);
        Obj::Text {
            pos: Pt::new(1.0, 1.0),
            text: "hi".into(),
            color: C4::rgb(255, 255, 255),
            size: 8.0,
        }
        .render(&mut img, None);
    }
}
