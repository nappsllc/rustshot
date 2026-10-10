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
    let (bx0, by0, bx1, by1) = sf.bounds();
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
                    if px >= bx0 && py >= by0 && px < bx1 && py < by1 {
                        sf.blend_px(px, py, color, cov, Blend::Normal);
                    }
                });
            }
            x += sfont.h_advance(gid);
        }
        y += line_height;
    }
}

/// Mosaic `r`. The cell grid is anchored at the rect's corner in image
/// coordinates; cells are clipped to the surface window (exact when the
/// window contains the rect).
fn pixelate(sf: &mut Surf, r: FRect, cell: f32) {
    let (bx0, by0, bx1, by1) = sf.bounds();
    let pw = sf.width() as i32;
    let x0 = r.x.max(0.0).floor() as i32;
    let y0 = r.y.max(0.0).floor() as i32;
    let x1 = (r.x1().min(bx1 as f32).ceil() as i32).min(bx1);
    let y1 = (r.y1().min(by1 as f32).ceil() as i32).min(by1);
    if x1 <= x0.max(bx0) || y1 <= y0.max(by0) {
        return;
    }
    let c = cell.max(2.0).round() as i32;
    let step = (c / 4).max(1);
    let idx = |x: i32, y: i32| (((y - by0) * pw + (x - bx0)) as usize) * 4;
    // First sample at or after `lo` on the grid `start + k * step`.
    let align = |start: i32, lo: i32| {
        if start >= lo {
            start
        } else {
            start + (lo - start + step - 1) / step * step
        }
    };
    let Surf { data, .. } = sf;
    // Skip whole cells left of / above the window.
    let mut cy = y0 + ((by0 - y0).max(0) / c) * c;
    while cy < y1 {
        let mut cx = x0 + ((bx0 - x0).max(0) / c) * c;
        while cx < x1 {
            let ex = (cx + c).min(x1);
            let ey = (cy + c).min(y1);
            let (mut sr, mut sg, mut sb, mut sa, mut n) = (0u32, 0u32, 0u32, 0u32, 0u32);
            let mut sy = align(cy, by0);
            while sy < ey {
                let mut sx = align(cx, bx0);
                while sx < ex {
                    let i = idx(sx, sy);
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
            for sy in cy.max(by0)..ey {
                for sx in cx.max(bx0)..ex {
                    let i = idx(sx, sy);
                    data[i] = ar as u8;
                    data[i + 1] = ag as u8;
                    data[i + 2] = ab as u8;
                    data[i + 3] = aa as u8;
                }
            }
            cx += c;
        }
        cy += c;
    }
}

fn invert(sf: &mut Surf, r: FRect) {
    let (bx0, by0, bx1, by1) = sf.bounds();
    let pw = sf.width() as i32;
    let x0 = (r.x.max(0.0).floor() as i32).max(bx0);
    let y0 = (r.y.max(0.0).floor() as i32).max(by0);
    let x1 = (r.x1().min(bx1 as f32).ceil() as i32).min(bx1);
    let y1 = (r.y1().min(by1 as f32).ceil() as i32).min(by1);
    let Surf { data, .. } = sf;
    for y in y0..y1 {
        for x in x0..x1 {
            let i = (((y - by0) * pw + (x - bx0)) as usize) * 4;
            data[i] = 255 - data[i];
            data[i + 1] = 255 - data[i + 1];
            data[i + 2] = 255 - data[i + 2];
        }
    }
}

impl Obj {
    /// Bake this object into the image.
    pub fn render(&self, buf: &mut PixBuf, font: Option<&FontArc>) {
        self.render_into(&mut Surf::from_buf(buf), font);
    }

    /// Bake this object into any surface (offset window, either channel
    /// order). Pixelate/invert read the surface's own pixels, so the caller
    /// pre-fills it with the background; pixelate is exact when the surface
    /// window contains the whole rect.
    pub fn render_into(&self, sf: &mut Surf, font: Option<&FontArc>) {
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
                    draw_text(sf, font, *size, text, *pos, *color);
                }
            }
            Obj::Pixelate { r, cell } => pixelate(sf, *r, *cell),
            Obj::Invert { r } => invert(sf, *r),
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

    #[test]
    fn every_object_matches_full_rgba_in_offset_bgra() {
        use crate::uifb::tests::equiv::check;
        let c = C4::new(250, 60, 20, 210);
        let (a, b) = (Pt::new(18.0, 62.0), Pt::new(82.0, 22.0));
        let rect = FRect { x: 24.0, y: 14.0, w: 56.0, h: 40.0 };
        let objs = vec![
            Obj::Line { a, b, color: c, width: 4.0 },
            Obj::Arrow { a, b, color: c, width: 5.0 },
            Obj::Arrow { a, b: Pt::new(18.5, 62.5), color: c, width: 5.0 },
            Obj::Rect { r: rect, color: c, width: 3.0 },
            Obj::Ellipse { r: rect, color: c, width: 3.0 },
            Obj::Path {
                pts: vec![a, Pt::new(40.0, 20.0), Pt::new(60.0, 60.0), b],
                color: c,
                width: 3.0,
            },
            Obj::Marker { a, b, color: C4::new(255, 230, 0, 160), width: 14.0 },
            Obj::Invert { r: rect },
            Obj::Pixelate { r: FRect { x: 33.0, y: 23.0, w: 30.0, h: 22.0 }, cell: 6.0 },
            Obj::Pixelate { r: FRect { x: 31.0, y: 21.0, w: 38.0, h: 28.0 }, cell: 8.0 },
        ];
        for (n, o) in objs.iter().enumerate() {
            check(&format!("obj {n}"), |s| o.render_into(s, None));
        }
        if let Some(font) = crate::fonts::load_system_font() {
            let t = Obj::Text {
                pos: Pt::new(22.0, 16.0),
                text: "Hello
World wy".into(),
                color: c,
                size: 16.0,
            };
            check("text", |s| t.render_into(s, Some(&font)));
        }
    }

    #[test]
    fn render_wrapper_matches_render_into() {
        let mut a = PixBuf::new(30, 30);
        let mut b = PixBuf::new(30, 30);
        let o = Obj::Ellipse {
            r: FRect { x: 3.0, y: 4.0, w: 20.0, h: 18.0 },
            color: C4::rgb(1, 2, 3),
            width: 2.0,
        };
        o.render(&mut a, None);
        o.render_into(&mut Surf::from_buf(&mut b), None);
        assert_eq!(a.as_raw(), b.as_raw());
    }
}
