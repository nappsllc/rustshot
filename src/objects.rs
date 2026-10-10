use crate::text::AnnotFont;
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

/// A pixelate object's cell grid (see [`Obj::pixel_grid`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellGrid {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
    pub c: i32,
}

impl CellGrid {
    /// `v` moved down to a cell edge where it falls inside `[a0, a1)`.
    fn floor(v: i32, a0: i32, a1: i32, c: i32) -> i32 {
        if v > a0 && v < a1 { a0 + (v - a0) / c * c } else { v }
    }

    /// `v` moved up to a cell edge (or `a1`) where it falls inside.
    fn ceil(v: i32, a0: i32, a1: i32, c: i32) -> i32 {
        if v > a0 && v < a1 { (a0 + (v - a0 + c - 1) / c * c).min(a1) } else { v }
    }

    /// Window `(x0, y0, x1, y1)` grown to whole cells where it cuts the
    /// pixelated area, with the grid clipped to `w` x `h`.
    pub fn align(&self, r: (i32, i32, i32, i32), w: i32, h: i32) -> (i32, i32, i32, i32) {
        let (gx1, gy1, c) = (self.x1.min(w), self.y1.min(h), self.c);
        (
            Self::floor(r.0, self.x0, gx1, c),
            Self::floor(r.1, self.y0, gy1, c),
            Self::ceil(r.2, self.x0, gx1, c),
            Self::ceil(r.3, self.y0, gy1, c),
        )
    }

    /// Next row at or after `y` that is a cell edge (or the area's
    /// bottom), for `y` inside the area; `y` itself otherwise.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn row_edge(&self, y: i32, h: i32) -> i32 {
        Self::ceil(y, self.y0, self.y1.min(h), self.c)
    }

    /// The cells two grids with this origin and cell size both render
    /// identically (whole cells inside both areas), as `(x0, y0, x1, y1)`
    /// clipped to `w` x `h`; `None` when they differ in origin or cell.
    pub fn stable_with(&self, o: &CellGrid, w: i32, h: i32) -> Option<(i32, i32, i32, i32)> {
        if (self.x0, self.y0, self.c) != (o.x0, o.y0, o.c) {
            return None;
        }
        let c = self.c;
        let whole = |a0: i32, a1: i32| a0 + (a1 - a0).max(0) / c * c;
        let x1 = whole(self.x0, self.x1.min(o.x1).min(w));
        let y1 = whole(self.y0, self.y1.min(o.y1).min(h));
        Some((self.x0, self.y0, x1, y1))
    }
}

#[derive(Clone, Debug, PartialEq)]
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
    // Average of the in-window samples of the cell at (`cx`, `cy`); `None`
    // when the window clips them all (only when it does not contain the
    // rect): that cell keeps its pixels.
    let avg = |data: &[u8], cx: i32, cy: i32| {
        let ex = (cx + c).min(x1);
        let ey = (cy + c).min(y1);
        let (mut sr, mut sg, mut sb, mut sa, mut n) = (0u32, 0u32, 0u32, 0u32, 0u32);
        let sx = align(cx, bx0);
        if sx < ex {
            let mut sy = align(cy, by0);
            while sy < ey {
                let row = &data[idx(sx, sy)..idx(ex, sy)];
                for p in row.as_chunks::<4>().0.iter().step_by(step as usize) {
                    sr += p[0] as u32;
                    sg += p[1] as u32;
                    sb += p[2] as u32;
                    sa += p[3] as u32;
                    n += 1;
                }
                sy += step;
            }
        }
        (n > 0).then(|| [(sr / n) as u8, (sg / n) as u8, (sb / n) as u8, (sa / n) as u8])
    };
    // Skip whole cells left of / above the window.
    let cx_first = x0 + ((bx0 - x0).max(0) / c) * c;
    let mut cy = y0 + ((by0 - y0).max(0) / c) * c;
    while cy < y1 {
        let ey = (cy + c).min(y1);
        let top = cy.max(by0);
        // One row of the band of cells, then copied down (all its samples
        // are read by then; cells never sample each other).
        let mut whole = true;
        let mut cx = cx_first;
        while cx < x1 {
            match avg(data, cx, cy) {
                Some(v) => {
                    let ex = (cx + c).min(x1);
                    for p in data[idx(cx.max(bx0), top)..idx(ex, top)].as_chunks_mut::<4>().0 {
                        *p = v;
                    }
                }
                None => whole = false,
            }
            cx += c;
        }
        let (a, b) = (idx(cx_first.max(bx0), top), idx(x1, top));
        if whole {
            for sy in top + 1..ey {
                data.copy_within(a..b, idx(cx_first.max(bx0), sy));
            }
        } else {
            // Rare (clipped window): cell by cell, skipping empty ones.
            let mut cx = cx_first;
            while cx < x1 {
                if avg(data, cx, cy).is_some() {
                    let (xa, ex) = (idx(cx.max(bx0), top), idx((cx + c).min(x1), top));
                    for sy in top + 1..ey {
                        data.copy_within(xa..ex, idx(cx.max(bx0), sy));
                    }
                }
                cx += c;
            }
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
    /// Conservative bounds of every pixel `render` may touch (half the
    /// stroke width, arrowhead, AA and glyph overhang included); `None`
    /// when it draws nothing.
    pub fn bounds(&self, font: Option<&AnnotFont>) -> Option<FRect> {
        fn bbox(pts: &[Pt], m: f32) -> Option<FRect> {
            let first = pts.first()?;
            let (mut x0, mut y0, mut x1, mut y1) = (first.x, first.y, first.x, first.y);
            for p in pts {
                (x0, y0, x1, y1) = (x0.min(p.x), y0.min(p.y), x1.max(p.x), y1.max(p.y));
            }
            Some(FRect { x: x0 - m, y: y0 - m, w: x1 - x0 + 2.0 * m, h: y1 - y0 + 2.0 * m })
        }
        let m = |w: f32| w.max(0.5) / 2.0 + 2.0;
        let grow = |r: &FRect, m: f32| FRect { x: r.x - m, y: r.y - m, w: r.w + 2.0 * m, h: r.h + 2.0 * m };
        match self {
            Obj::Line { a, b, width, .. } | Obj::Marker { a, b, width, .. } => bbox(&[*a, *b], m(*width)),
            Obj::Arrow { a, b, width, .. } => match arrow_geometry(*a, *b, *width) {
                Some((_, head)) => bbox(&[*a, *b, head[1], head[2]], m(*width)),
                None => bbox(&[*a, *b], m(*width)),
            },
            Obj::Path { pts, width, .. } => (pts.len() >= 2).then(|| bbox(pts, m(*width))).flatten(),
            Obj::Rect { r, width, .. } | Obj::Ellipse { r, width, .. } => Some(grow(r, m(*width))),
            Obj::Pixelate { r, .. } | Obj::Invert { r } => Some(grow(r, 1.0)),
            Obj::Text { pos, text, size, .. } => {
                let (w, h, _) = font?.measure(text, *size);
                // Overhang, rounding to whole pixels and AA bleed.
                let m = size + 2.0;
                Some(FRect { x: pos.x - m, y: pos.y - m, w: w + 2.0 * m, h: h + 2.0 * m })
            }
        }
    }

    /// Pixelate only: the mosaic grid as rendered into a window covering
    /// the whole image: the pixelated area `[x0, x1) x [y0, y1)` (`x1`/`y1`
    /// not yet clipped to the image) and the cell size, cells anchored at
    /// (`x0`, `y0`). A window whose edges inside that area fall on cell
    /// edges renders those pixels exactly as the whole image does.
    pub fn pixel_grid(&self) -> Option<CellGrid> {
        let Obj::Pixelate { r, cell } = self else { return None };
        Some(CellGrid {
            x0: r.x.max(0.0).floor() as i32,
            y0: r.y.max(0.0).floor() as i32,
            x1: r.x1().ceil() as i32,
            y1: r.y1().ceil() as i32,
            c: cell.max(2.0).round() as i32,
        })
    }

    /// Bake this object into the image.
    pub fn render(&self, buf: &mut PixBuf, font: Option<&AnnotFont>) {
        self.render_into(&mut Surf::from_buf(buf), font);
    }

    /// Bake this object into any surface (offset window, either channel
    /// order). Pixelate/invert read the surface's own pixels, so the caller
    /// pre-fills it with the background; pixelate is exact when the surface
    /// window contains the whole rect.
    pub fn render_into(&self, sf: &mut Surf, font: Option<&AnnotFont>) {
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
                    font.render(sf, text, *size, *pos, *color);
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
        if let Some(font) = AnnotFont::load() {
            let t = Obj::Text {
                pos: Pt::new(22.0, 16.0),
                text: "Hello\nWorld wy".into(),
                color: c,
                size: 16.0,
            };
            check("text", |s| t.render_into(s, Some(&font)));
        }
    }

    /// A window that clips the rect mid-cell (the overlay's band edges):
    /// no panic, and cells left without samples keep their pixels instead
    /// of turning transparent black.
    #[test]
    fn pixelate_straddling_window_skips_empty_cells() {
        let (w, h) = (64u32, 64u32);
        let mut full = PixBuf::new(w, h);
        for (i, p) in full.as_raw_mut().as_chunks_mut::<4>().0.iter_mut().enumerate() {
            *p = [(i * 7) as u8, (i * 3) as u8, 200, 255];
        }
        let o = Obj::Pixelate { r: FRect { x: 3.0, y: 5.0, w: 50.0, h: 47.0 }, cell: 12.0 };
        // Windows starting just before a cell boundary leave the clipped
        // cell's samples outside.
        for (wx, wy) in [(14, 16), (13, 15), (0, 0), (26, 28), (40, 2)] {
            let (ww, wh) = (20u32, 21u32);
            let mut win = vec![0u8; (ww * wh * 4) as usize];
            for y in 0..wh {
                for x in 0..ww {
                    let s = full.get_pixel((wx + x).min(w - 1), (wy + y).min(h - 1));
                    win[((y * ww + x) * 4) as usize..][..4].copy_from_slice(&s);
                }
            }
            o.render_into(&mut Surf::with_origin(&mut win, ww, wh, wx as i32, wy as i32, crate::raster::Order::Rgba), None);
            for (i, p) in win.as_chunks::<4>().0.iter().enumerate() {
                assert_ne!(*p, [0, 0, 0, 0], "window ({wx}, {wy}): pixel {i} cleared");
            }
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
