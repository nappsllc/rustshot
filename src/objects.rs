use ab_glyph::{Font, FontArc, PxScale, ScaleFont};
use egui::{Color32, Pos2, Rect, Shape, Stroke, Vec2};
use tiny_skia::{
    BlendMode, FillRule, LineCap, LineJoin, Paint, Path, PathBuilder, Pixmap, Shader, Stroke as
    TStroke, Transform,
};

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
        color: Color32,
        width: f32,
    },
    Arrow {
        a: Pt,
        b: Pt,
        color: Color32,
        width: f32,
    },
    Rect {
        r: FRect,
        color: Color32,
        width: f32,
    },
    Ellipse {
        r: FRect,
        color: Color32,
        width: f32,
    },
    Path {
        pts: Vec<Pt>,
        color: Color32,
        width: f32,
    },
    Marker {
        a: Pt,
        b: Pt,
        color: Color32,
        width: f32,
    },
    Text {
        pos: Pt,
        text: String,
        color: Color32,
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

const KAPPA: f32 = 0.552_284_8;

fn to_tiny_color(c: Color32) -> tiny_skia::Color {
    let [r, g, b, a] = c.to_array(); // premultiplied
    let a = a as u16;
    if a == 0 {
        return tiny_skia::Color::from_rgba8(0, 0, 0, 0);
    }
    let un = |v: u8| ((v as u16 * 255) / a) as u8;
    tiny_skia::Color::from_rgba8(un(r), un(g), un(b), a as u8)
}

fn paint(c: Color32) -> Paint<'static> {
    Paint {
        shader: Shader::SolidColor(to_tiny_color(c)),
        anti_alias: true,
        ..Default::default()
    }
}

fn stroke(width: f32) -> TStroke {
    TStroke {
        width: width.max(0.5),
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Default::default()
    }
}

fn line_path(a: Pt, b: Pt) -> Option<Path> {
    let mut pb = PathBuilder::new();
    pb.move_to(a.x, a.y);
    pb.line_to(b.x, b.y);
    pb.finish()
}

/// Rounded rectangle; radius 0..=min(w,h)/2.
fn round_rect_path(r: FRect, radius: f32) -> Option<Path> {
    let rad = radius.min(r.w / 2.0).min(r.h / 2.0).max(0.0);
    if rad < 0.6 {
        let mut pb = PathBuilder::new();
        pb.push_rect(tiny_skia::Rect::from_xywh(r.x, r.y, r.w.max(1.0), r.h.max(1.0))?);
        return pb.finish();
    }
    let (x0, y0, x1, y1) = (r.x, r.y, r.x1(), r.y1());
    let k = rad * KAPPA;
    let mut pb = PathBuilder::new();
    pb.move_to(x0 + rad, y0);
    pb.line_to(x1 - rad, y0);
    pb.cubic_to(x1 - rad + k, y0, x1, y0 + rad - k, x1, y0 + rad);
    pb.line_to(x1, y1 - rad);
    pb.cubic_to(x1, y1 - rad + k, x1 - rad + k, y1, x1 - rad, y1);
    pb.line_to(x0 + rad, y1);
    pb.cubic_to(x0 + rad - k, y1, x0, y1 - rad + k, x0, y1 - rad);
    pb.line_to(x0, y0 + rad);
    pb.cubic_to(x0, y0 + rad - k, x0 + rad - k, y0, x0 + rad, y0);
    pb.finish()
}

fn ellipse_path(r: FRect) -> Option<Path> {
    let cx = r.x + r.w / 2.0;
    let cy = r.y + r.h / 2.0;
    let rx = (r.w / 2.0).max(0.5);
    let ry = (r.h / 2.0).max(0.5);
    let kx = rx * KAPPA;
    let ky = ry * KAPPA;
    let mut pb = PathBuilder::new();
    pb.move_to(cx, cy - ry);
    pb.cubic_to(cx + kx, cy - ry, cx + rx, cy - ky, cx + rx, cy);
    pb.cubic_to(cx + rx, cy + ky, cx + kx, cy + ry, cx, cy + ry);
    pb.cubic_to(cx - kx, cy + ry, cx - rx, cy + ky, cx - rx, cy);
    pb.cubic_to(cx - rx, cy - ky, cx - kx, cy - ry, cx, cy - ry);
    pb.finish()
}

fn arrow_head(a: Pt, b: Pt, width: f32) -> Option<(Path, Path)> {
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
    let mut pb = PathBuilder::new();
    pb.move_to(b.x, b.y);
    pb.line_to(p1.x, p1.y);
    pb.line_to(p2.x, p2.y);
    pb.close();
    let head = pb.finish()?;
    Some((line_path(a, base)?, head))
}

fn blend_px(data: &mut [u8], width: u32, x: i32, y: i32, h: u32, color: Color32, cov: f32) {
    if x < 0 || y < 0 || x >= width as i32 || y >= h as i32 {
        return;
    }
    let idx = (y as u32 * width + x as u32) as usize * 4;
    if idx + 3 >= data.len() {
        return;
    }
    let [sr, sg, sb, sa] = color.to_array(); // premultiplied
    let c = cov.clamp(0.0, 1.0);
    let s = [sr as f32 * c, sg as f32 * c, sb as f32 * c, sa as f32 * c];
    let a = s[3] / 255.0;
    let inv = 1.0 - a;
    for (i, sv) in s.iter().enumerate() {
        let dv = data[idx + i] as f32;
        data[idx + i] = (sv + dv * inv).round().clamp(0.0, 255.0) as u8;
    }
}

fn draw_text(pm: &mut Pixmap, font: &FontArc, size: f32, text: &str, top: Pt, color: Color32) {
    let scale = PxScale::from(size);
    let sf = font.as_scaled(scale);
    let ascent = sf.ascent();
    let descent = sf.descent(); // negative
    let gap = sf.line_gap();
    let line_height = (ascent - descent + gap).max(size * 1.1);
    let (pw, ph) = (pm.width(), pm.height());
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
                    blend_px(pm.data_mut(), pw, px, py, ph, color, cov);
                });
            }
            x += sf.h_advance(gid);
        }
        y += line_height;
    }
}

fn pixelate(pm: &mut Pixmap, r: FRect, cell: f32) {
    let (pw, ph) = (pm.width() as i32, pm.height() as i32);
    let x0 = r.x.max(0.0).floor() as i32;
    let y0 = r.y.max(0.0).floor() as i32;
    let x1 = r.x1().min(pw as f32).ceil() as i32;
    let y1 = r.y1().min(ph as f32).ceil() as i32;
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let c = cell.max(2.0).round();
    let step = (c / 4.0).max(1.0) as i32;
    let data = pm.data_mut();
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

fn invert(pm: &mut Pixmap, r: FRect) {
    let (pw, ph) = (pm.width() as i32, pm.height() as i32);
    let x0 = r.x.max(0.0).floor() as i32;
    let y0 = r.y.max(0.0).floor() as i32;
    let x1 = r.x1().min(pw as f32).ceil() as i32;
    let y1 = r.y1().min(ph as f32).ceil() as i32;
    let data = pm.data_mut();
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
    /// Bake this object into the pixmap.
    pub fn render(&self, pm: &mut Pixmap, font: Option<&FontArc>) {
        match self {
            Obj::Line { a, b, color, width } => {
                if let Some(path) = line_path(*a, *b) {
                    pm.stroke_path(&path, &paint(*color), &stroke(*width), Transform::identity(), None);
                }
            }
            Obj::Marker { a, b, color, width } => {
                if let Some(path) = line_path(*a, *b) {
                    let mut p = paint(*color);
                    p.blend_mode = BlendMode::Multiply;
                    pm.stroke_path(&path, &p, &stroke(*width), Transform::identity(), None);
                }
            }
            Obj::Arrow { a, b, color, width } => {
                if let Some((shaft, head)) = arrow_head(*a, *b, *width) {
                    pm.stroke_path(&shaft, &paint(*color), &stroke(*width), Transform::identity(), None);
                    pm.fill_path(&head, &paint(*color), FillRule::Winding, Transform::identity(), None);
                } else if let Some(path) = line_path(*a, *b) {
                    pm.stroke_path(&path, &paint(*color), &stroke(*width), Transform::identity(), None);
                }
            }
            Obj::Rect { r, color, width } => {
                let radius = *width;
                if let Some(path) = round_rect_path(*r, radius) {
                    pm.stroke_path(&path, &paint(*color), &stroke(*width), Transform::identity(), None);
                }
            }
            Obj::Ellipse { r, color, width } => {
                if let Some(path) = ellipse_path(*r) {
                    pm.stroke_path(&path, &paint(*color), &stroke(*width), Transform::identity(), None);
                }
            }
            Obj::Path { pts, color, width } => {
                if pts.len() < 2 {
                    return;
                }
                let mut pb = PathBuilder::new();
                pb.move_to(pts[0].x, pts[0].y);
                for p in &pts[1..] {
                    pb.line_to(p.x, p.y);
                }
                if let Some(path) = pb.finish() {
                    pm.stroke_path(&path, &paint(*color), &stroke(*width), Transform::identity(), None);
                }
            }
            Obj::Text {
                pos,
                text,
                color,
                size,
            } => {
                if let Some(font) = font {
                    draw_text(pm, font, *size, text, *pos, *color);
                }
            }
            Obj::Pixelate { r, cell } => pixelate(pm, *r, *cell),
            Obj::Invert { r } => invert(pm, *r),
        }
    }
}

// ---------------------------------------------------------------------------
// Live preview rendering (egui shapes) while a stroke is being drawn.
// ---------------------------------------------------------------------------

pub fn to_ui_pt(p: Pt, origin: Pos2, inv_scale: f32) -> Pos2 {
    origin + Vec2::new(p.x * inv_scale, p.y * inv_scale)
}

pub fn to_ui_rect(r: FRect, origin: Pos2, inv_scale: f32) -> Rect {
    Rect::from_min_size(
        to_ui_pt(Pt::new(r.x, r.y), origin, inv_scale),
        Vec2::new(r.w * inv_scale, r.h * inv_scale),
    )
}

/// Paint a draft object as UI overlay. `inv_scale` = 1/scale (points per px).
pub fn paint_preview(
    painter: &egui::Painter,
    obj: &Obj,
    origin: Pos2,
    inv_scale: f32,
) {
    let p = |pt: Pt| to_ui_pt(pt, origin, inv_scale);
    match obj {
        Obj::Line { a, b, color, width } => {
            painter.line_segment([p(*a), p(*b)], Stroke::new(width * inv_scale, *color));
        }
        Obj::Marker { a, b, color, width } => {
            painter.line_segment([p(*a), p(*b)], Stroke::new(width * inv_scale, *color));
        }
        Obj::Arrow { a, b, color, width } => {
            let st = Stroke::new(width * inv_scale, *color);
            if let Some((_, _)) = arrow_head(*a, *b, *width) {
                let dx = b.x - a.x;
                let dy = b.y - a.y;
                let len = (dx * dx + dy * dy).sqrt();
                if len >= 2.0 {
                    let ux = dx / len;
                    let uy = dy / len;
                    let hl = (width * 3.0).clamp(9.0, 60.0);
                    let hw = hl * 0.45;
                    let base = Pt::new(b.x - ux * hl, b.y - uy * hl);
                    let px = -uy;
                    let py = ux;
                    let tri = vec![
                        p(*b),
                        p(Pt::new(base.x + px * hw, base.y + py * hw)),
                        p(Pt::new(base.x - px * hw, base.y - py * hw)),
                    ];
                    painter.add(Shape::convex_polygon(tri, *color, Stroke::NONE));
                    painter.line_segment([p(*a), p(base)], st);
                }
            } else {
                painter.line_segment([p(*a), p(*b)], st);
            }
        }
        Obj::Rect { r, color, width } => {
            let rounding = width * inv_scale;
            painter.rect_stroke(to_ui_rect(*r, origin, inv_scale), rounding, Stroke::new(width * inv_scale, *color));
        }
        Obj::Ellipse { r, color, width } => {
            let rect = to_ui_rect(*r, origin, inv_scale);
            let radius = Vec2::new(rect.width() / 2.0, rect.height() / 2.0);
            painter.add(Shape::Ellipse(egui::epaint::EllipseShape::stroke(
                rect.center(),
                radius,
                Stroke::new(width * inv_scale, *color),
            )));
        }
        Obj::Path { pts, color, width } => {
            if pts.len() >= 2 {
                let points: Vec<Pos2> = pts.iter().map(|q| p(*q)).collect();
                painter.add(Shape::Path(egui::epaint::PathShape::line(
                    points,
                    Stroke::new(width * inv_scale, *color),
                )));
            } else if let Some(q) = pts.first() {
                painter.circle_filled(p(*q), width * inv_scale * 0.5, *color);
            }
        }
        Obj::Text { .. } => {} // edited via TextEdit widget
        Obj::Pixelate { r, .. } => {
            let rect = to_ui_rect(*r, origin, inv_scale);
            painter.rect_filled(rect, 0.0, Color32::from_black_alpha(50));
            painter.rect_stroke(rect, 0.0, Stroke::new(1.0, Color32::from_white_alpha(160)));
        }
        Obj::Invert { r } => {
            let rect = to_ui_rect(*r, origin, inv_scale);
            painter.rect_stroke(rect, 0.0, Stroke::new(1.0, Color32::from_white_alpha(200)));
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
    fn renders_line_into_pixmap() {
        let mut pm = Pixmap::new(20, 20).unwrap();
        Obj::Line {
            a: Pt::new(2.0, 2.0),
            b: Pt::new(18.0, 18.0),
            color: Color32::RED,
            width: 3.0,
        }
        .render(&mut pm, None);
        let i = ((10 * 20 + 10) * 4) as usize;
        assert_ne!(pm.data()[i], 0, "line should cover the middle pixel");
        assert_eq!(pm.data()[i + 3], 255);
    }

    #[test]
    fn pixelate_fills_region() {
        let mut pm = Pixmap::new(32, 32).unwrap();
        // gradient-ish content: set a few distinct pixels
        for i in 0..(32 * 32) {
            let v = (i % 255) as u8;
            let d = &mut pm.data_mut()[i * 4..i * 4 + 4];
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
        .render(&mut pm, None);
        // Top-left 8x8 cell must now be a solid color.
        let first = &pm.data()[0..4];
        for y in 0..8 {
            for x in 0..8 {
                let i = (y * 32 + x) as usize * 4;
                assert_eq!(&pm.data()[i..i + 4], first, "at {x},{y}");
            }
        }
    }

    #[test]
    fn text_render_does_not_panic_without_font() {
        let mut pm = Pixmap::new(10, 10).unwrap();
        Obj::Text {
            pos: Pt::new(1.0, 1.0),
            text: "hi".into(),
            color: Color32::WHITE,
            size: 8.0,
        }
        .render(&mut pm, None);
    }
}
