//! Lucide icons (ISC license, https://lucide.dev) as SVG path data, flattened
//! to polylines in the 24-unit grid and stroked with the AA rasterizer.

use crate::objects::Pt;
use crate::raster::Blend;
use crate::uifb::{Fb, C4};
use std::f32::consts::{FRAC_PI_2, PI, TAU};

pub enum Shape {
    Path(&'static str),
    Circle(f32, f32, f32),
    Rect { x: f32, y: f32, w: f32, h: f32, r: f32 },
}

use Shape::{Circle, Path, Rect};

/// Path data copied from the design canvas (Lucide names in comments).
pub const ICONS: &[(&str, &[Shape])] = &[
    ("pencil", &[ // pencil
        Path("M21.174 6.812a1 1 0 0 0-3.986-3.987L3.842 16.174a2 2 0 0 0-.5.83l-1.321 4.352a.5.5 0 0 0 .623.622l4.353-1.32a2 2 0 0 0 .83-.497z"),
        Path("m15 5 4 4"),
    ]),
    ("line", &[Path("M20 4 4 20")]), // slash
    ("arrow", &[Path("M7 7h10v10"), Path("M7 17 17 7")]), // arrow-up-right
    ("rect", &[Rect { x: 2.0, y: 5.0, w: 20.0, h: 14.0, r: 2.0 }]), // rectangle-horizontal
    ("circle", &[Circle(12.0, 12.0, 10.0)]), // circle
    ("marker", &[ // highlighter
        Path("m9 11-6 6v3h9l3-3"),
        Path("m22 12-4.6 4.6a2 2 0 0 1-2.8 0l-5.2-5.2a2 2 0 0 1 0-2.8L14 4"),
    ]),
    ("text", &[Path("M4 7V4h16v3"), Path("M9 20h6"), Path("M12 4v16")]), // type
    ("pixel", &[ // grid-3x3
        Rect { x: 3.0, y: 3.0, w: 18.0, h: 18.0, r: 2.0 },
        Path("M3 9h18"),
        Path("M3 15h18"),
        Path("M9 3v18"),
        Path("M15 3v18"),
    ]),
    ("invert", &[Circle(12.0, 12.0, 10.0), Path("M12 18a6 6 0 0 0 0-12v12z")]), // contrast
    ("undo", &[ // undo-2
        Path("M9 14 4 9l5-5"),
        Path("M4 9h10.5a5.5 5.5 0 0 1 5.5 5.5a5.5 5.5 0 0 1-5.5 5.5H11"),
    ]),
    ("redo", &[ // redo-2
        Path("m15 14 5-5-5-5"),
        Path("M20 9H9.5A5.5 5.5 0 0 0 4 14.5A5.5 5.5 0 0 0 9.5 20H13"),
    ]),
    ("minus", &[Path("M5 12h14")]),
    ("plus", &[Path("M5 12h14"), Path("M12 5v14")]),
    ("copy", &[ // copy
        Rect { x: 8.0, y: 8.0, w: 14.0, h: 14.0, r: 2.0 },
        Path("M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"),
    ]),
    ("save", &[ // download
        Path("M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"),
        Path("m7 10 5 5 5-5"),
        Path("M12 15V3"),
    ]),
    ("cloud", &[ // cloud-upload
        Path("M12 13v8"),
        Path("M4 14.899A7 7 0 1 1 15.71 8h1.79a4.5 4.5 0 0 1 2.5 8.242"),
        Path("m8 17 4-4 4 4"),
    ]),
    ("x", &[Path("M18 6 6 18"), Path("m6 6 12 12")]),
    ("check", &[Path("M20 6 9 17l-5-5")]),
    ("chevron", &[Path("m6 9 6 6 6-6")]), // chevron-down
    ("okc", &[Circle(12.0, 12.0, 10.0), Path("m9 12 2 2 4-4")]), // circle-check
    ("info", &[Circle(12.0, 12.0, 10.0), Path("M12 16v-4"), Path("M12 8h.01")]),
    ("alert", &[Circle(12.0, 12.0, 10.0), Path("M12 8v4"), Path("M12 16h.01")]), // circle-alert
    ("triangle-alert", &[
        Path("m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3"),
        Path("M12 9v4"),
        Path("M12 17h.01"),
    ]),
];

/// SVG path-data tokenizer (numbers, single-char flags, command letters).
struct Tok<'a> {
    s: &'a [u8],
    i: usize,
}

impl Tok<'_> {
    fn skip_sep(&mut self) {
        while self.i < self.s.len() && (self.s[self.i].is_ascii_whitespace() || self.s[self.i] == b',') {
            self.i += 1;
        }
    }

    fn cmd(&mut self) -> Option<u8> {
        self.skip_sep();
        let c = *self.s.get(self.i)?;
        if c.is_ascii_alphabetic() {
            self.i += 1;
            Some(c)
        } else {
            None
        }
    }

    fn at_number(&mut self) -> bool {
        self.skip_sep();
        matches!(self.s.get(self.i), Some(b'0'..=b'9' | b'-' | b'+' | b'.'))
    }

    fn num(&mut self) -> Option<f32> {
        self.skip_sep();
        let start = self.i;
        if matches!(self.s.get(self.i), Some(b'-' | b'+')) {
            self.i += 1;
        }
        let mut dot = false;
        while let Some(&c) = self.s.get(self.i) {
            match c {
                b'0'..=b'9' => self.i += 1,
                b'.' if !dot => {
                    dot = true;
                    self.i += 1;
                }
                _ => break,
            }
        }
        std::str::from_utf8(&self.s[start..self.i]).ok()?.parse().ok()
    }

    fn flag(&mut self) -> Option<bool> {
        self.skip_sep();
        let c = *self.s.get(self.i)?;
        self.i += 1;
        match c {
            b'0' => Some(false),
            b'1' => Some(true),
            _ => None,
        }
    }
}

fn ensure_open(out: &mut Vec<Vec<Pt>>, open: &mut bool, cur: Pt) {
    if !*open {
        out.push(vec![cur]);
        *open = true;
    }
}

fn cubic(p0: Pt, c1: Pt, c2: Pt, p1: Pt, out: &mut Vec<Pt>) {
    for i in 1..=12 {
        let t = i as f32 / 12.0;
        let mt = 1.0 - t;
        let (a, b, c, d) = (mt * mt * mt, 3.0 * mt * mt * t, 3.0 * mt * t * t, t * t * t);
        out.push(Pt::new(
            a * p0.x + b * c1.x + c * c2.x + d * p1.x,
            a * p0.y + b * c1.y + c * c2.y + d * p1.y,
        ));
    }
}

/// Elliptical arc, SVG endpoint parameterization (SVG 1.1 F.6.5/F.6.6).
#[allow(clippy::too_many_arguments)]
fn arc(p0: Pt, rx: f32, ry: f32, rot_deg: f32, large: bool, sweep: bool, p1: Pt, out: &mut Vec<Pt>) {
    if (p0.x - p1.x).abs() < 1e-6 && (p0.y - p1.y).abs() < 1e-6 {
        return;
    }
    let (mut rx, mut ry) = (rx.abs(), ry.abs());
    if rx < 1e-6 || ry < 1e-6 {
        out.push(p1);
        return;
    }
    let (s, c) = rot_deg.to_radians().sin_cos();
    let (dx, dy) = ((p0.x - p1.x) / 2.0, (p0.y - p1.y) / 2.0);
    let (x1, y1) = (c * dx + s * dy, -s * dx + c * dy);
    let lam = (x1 * x1) / (rx * rx) + (y1 * y1) / (ry * ry);
    if lam > 1.0 {
        rx *= lam.sqrt();
        ry *= lam.sqrt();
    }
    let num = rx * rx * ry * ry - rx * rx * y1 * y1 - ry * ry * x1 * x1;
    let den = rx * rx * y1 * y1 + ry * ry * x1 * x1;
    let mut co = (num / den).max(0.0).sqrt();
    if large == sweep {
        co = -co;
    }
    let (cxp, cyp) = (co * rx * y1 / ry, -co * ry * x1 / rx);
    let cx = c * cxp - s * cyp + (p0.x + p1.x) / 2.0;
    let cy = s * cxp + c * cyp + (p0.y + p1.y) / 2.0;
    let ang = |ux: f32, uy: f32, vx: f32, vy: f32| (ux * vy - uy * vx).atan2(ux * vx + uy * vy);
    let (ux, uy) = ((x1 - cxp) / rx, (y1 - cyp) / ry);
    let (vx, vy) = ((-x1 - cxp) / rx, (-y1 - cyp) / ry);
    let th1 = ang(1.0, 0.0, ux, uy);
    let mut dth = ang(ux, uy, vx, vy);
    if !sweep && dth > 0.0 {
        dth -= TAU;
    } else if sweep && dth < 0.0 {
        dth += TAU;
    }
    let n = ((dth.abs() / (PI / 16.0)).ceil() as usize).max(2);
    for i in 1..n {
        let (st, ct) = (th1 + dth * i as f32 / n as f32).sin_cos();
        out.push(Pt::new(c * rx * ct - s * ry * st + cx, s * rx * ct + c * ry * st + cy));
    }
    out.push(p1);
}

/// Parse SVG path data (M L H V C A Z, absolute and relative) into polylines.
pub fn parse_path(d: &str) -> Option<Vec<Vec<Pt>>> {
    let mut t = Tok { s: d.as_bytes(), i: 0 };
    let mut out: Vec<Vec<Pt>> = Vec::new();
    let (mut cur, mut start) = (Pt::new(0.0, 0.0), Pt::new(0.0, 0.0));
    let mut open = false;
    let mut cmd = 0u8;
    loop {
        match t.cmd() {
            Some(c) => cmd = c,
            None if cmd != 0 && t.at_number() => {} // implicit repeat
            None => break,
        }
        let rel = cmd.is_ascii_lowercase();
        let o = if rel { cur } else { Pt::new(0.0, 0.0) };
        let pt = |t: &mut Tok<'_>| -> Option<Pt> { Some(Pt::new(o.x + t.num()?, o.y + t.num()?)) };
        match cmd.to_ascii_uppercase() {
            b'M' => {
                cur = pt(&mut t)?;
                start = cur;
                open = false;
                cmd = if rel { b'l' } else { b'L' };
            }
            b'L' => {
                let p = pt(&mut t)?;
                ensure_open(&mut out, &mut open, cur);
                out.last_mut()?.push(p);
                cur = p;
            }
            b'H' => {
                let p = Pt::new(t.num()? + o.x, cur.y);
                ensure_open(&mut out, &mut open, cur);
                out.last_mut()?.push(p);
                cur = p;
            }
            b'V' => {
                let p = Pt::new(cur.x, t.num()? + o.y);
                ensure_open(&mut out, &mut open, cur);
                out.last_mut()?.push(p);
                cur = p;
            }
            b'C' => {
                let (c1, c2, p) = (pt(&mut t)?, pt(&mut t)?, pt(&mut t)?);
                ensure_open(&mut out, &mut open, cur);
                cubic(cur, c1, c2, p, out.last_mut()?);
                cur = p;
            }
            b'A' => {
                let (rx, ry, rot) = (t.num()?, t.num()?, t.num()?);
                let (large, sweep) = (t.flag()?, t.flag()?);
                let p = pt(&mut t)?;
                ensure_open(&mut out, &mut open, cur);
                arc(cur, rx, ry, rot, large, sweep, p, out.last_mut()?);
                cur = p;
            }
            b'Z' => {
                if open {
                    out.last_mut()?.push(start);
                }
                open = false;
                cur = start;
                cmd = 0;
            }
            _ => return None,
        }
    }
    Some(out)
}

fn rect_pts(x: f32, y: f32, w: f32, h: f32, r: f32) -> Vec<Pt> {
    let corners = [
        (x + w - r, y + r, -FRAC_PI_2),
        (x + w - r, y + h - r, 0.0),
        (x + r, y + h - r, FRAC_PI_2),
        (x + r, y + r, PI),
    ];
    let mut v = Vec::with_capacity(21);
    for (cx, cy, a0) in corners {
        for i in 0..=4 {
            let (s, c) = (a0 + FRAC_PI_2 * i as f32 / 4.0).sin_cos();
            v.push(Pt::new(cx + r * c, cy + r * s));
        }
    }
    v.push(v[0]);
    v
}

/// Polylines (24-unit grid) for a bundled icon.
pub fn polylines(name: &str) -> Option<Vec<Vec<Pt>>> {
    let (_, shapes) = ICONS.iter().find(|(n, _)| *n == name)?;
    let mut out = Vec::new();
    for sh in shapes.iter() {
        match *sh {
            Path(d) => out.extend(parse_path(d)?),
            Circle(cx, cy, r) => out.push(
                (0..=48)
                    .map(|i| {
                        let (s, c) = (i as f32 * TAU / 48.0).sin_cos();
                        Pt::new(cx + r * c, cy + r * s)
                    })
                    .collect(),
            ),
            Rect { x, y, w, h, r } => out.push(rect_pts(x, y, w, h, r)),
        }
    }
    Some(out)
}

/// Stroke icon `name` into a `size`×`size` box at (x, y).
pub fn draw_icon(fb: &mut Fb, name: &str, x: f32, y: f32, size: f32, c: C4) {
    let Some(lines) = polylines(name) else { return };
    let k = size / 24.0;
    let width = (1.8 * k).max(1.0);
    let mut s = fb.surf();
    for pl in &lines {
        let pts: Vec<Pt> = pl.iter().map(|p| Pt::new(x + p.x * k, y + p.y * k)).collect();
        s.stroke_polyline(&pts, width, c, Blend::Normal);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_numbers() {
        let mut t = Tok { s: b" 1.5.5-2,.25", i: 0 };
        assert_eq!(t.num(), Some(1.5));
        assert_eq!(t.num(), Some(0.5));
        assert_eq!(t.num(), Some(-2.0));
        assert_eq!(t.num(), Some(0.25));
        assert_eq!(t.num(), None);
    }

    #[test]
    fn relative_lines_and_close() {
        let p = parse_path("m2 2 3 0v3h-3z").unwrap();
        assert_eq!(p.len(), 1);
        let v: Vec<(f32, f32)> = p[0].iter().map(|q| (q.x, q.y)).collect();
        assert_eq!(v, vec![(2.0, 2.0), (5.0, 2.0), (5.0, 5.0), (2.0, 5.0), (2.0, 2.0)]);
    }

    #[test]
    fn dot_subpath_is_kept() {
        let p = parse_path("M12 8h.01").unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].len(), 2);
    }

    #[test]
    fn arc_semicircle_bulges_upward() {
        let p = parse_path("M0 12A12 12 0 0 1 24 12").unwrap();
        let v = &p[0];
        let last = v.last().unwrap();
        assert!((last.x - 24.0).abs() < 1e-3 && (last.y - 12.0).abs() < 1e-3);
        for q in v {
            let r = (q.x - 12.0).hypot(q.y - 12.0);
            assert!((r - 12.0).abs() < 0.05, "r = {r}");
        }
        assert!(v.iter().any(|q| q.y < 0.5), "sweep=1 in y-down goes through the top");
    }

    #[test]
    fn every_icon_parses_inside_grid() {
        for (name, _) in ICONS {
            let pl = polylines(name).unwrap_or_else(|| panic!("{name} failed to parse"));
            assert!(!pl.is_empty(), "{name} is empty");
            for p in pl.iter().flatten() {
                assert!(
                    (-0.6..=24.6).contains(&p.x) && (-0.6..=24.6).contains(&p.y),
                    "{name}: {p:?}"
                );
            }
        }
    }

    #[test]
    fn draw_icon_lights_pixels() {
        let mut d = vec![0u8; 20 * 20 * 4];
        for p in d.as_chunks_mut::<4>().0 {
            p[3] = 255;
        }
        draw_icon(&mut Fb::new(&mut d, 20), "x", 0.0, 0.0, 20.0, C4::rgb(255, 255, 255));
        let lit = d.as_chunks::<4>().0.iter().filter(|p| p[0] > 128).count();
        assert!(lit > 20, "only {lit} lit pixels");
        assert!(d[(10 * 20 + 10) * 4] > 200, "centre of the X is stroked");
    }
}
