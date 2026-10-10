//! Linux annotation text: the first system font that parses (Inter, which
//! is Latin only, when none does), rasterised with ab_glyph.

use crate::objects::Pt;
use ab_glyph::{Font, FontArc, PxScale, ScaleFont};

static INTER: &[u8] = include_bytes!("../assets/fonts/Inter-Medium.subset.ttf");

const CANDIDATES: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
    "/usr/share/fonts/truetype/freefont/FreeSans.ttf",
];

#[derive(Clone)]
pub struct Face(FontArc);

impl Face {
    pub fn load() -> Option<Self> {
        CANDIDATES
            .iter()
            .find_map(|p| std::fs::read(p).ok().and_then(|b| FontArc::try_from_vec(b).ok()))
            .or_else(|| FontArc::try_from_slice(INTER).ok())
            .map(Face)
    }

    pub fn metrics(&self, px: f32) -> (f32, f32, f32) {
        let f = self.0.as_scaled(PxScale::from(px));
        (f.ascent(), -f.descent(), f.line_gap())
    }

    pub fn width(&self, line: &str, px: f32) -> f32 {
        let f = self.0.as_scaled(PxScale::from(px));
        line.chars().map(|c| f.h_advance(self.0.glyph_id(c))).sum()
    }

    pub fn draw(
        &self,
        line: &str,
        px: f32,
        top: Pt,
        ascent: f32,
        clip: (i32, i32, i32, i32),
        f: &mut dyn FnMut(i32, i32, f32),
    ) {
        let (bx0, by0, bx1, by1) = clip;
        let scale = PxScale::from(px);
        let sf = self.0.as_scaled(scale);
        let y = top.y + ascent;
        let mut x = top.x;
        for ch in line.chars() {
            let gid = self.0.glyph_id(ch);
            let glyph = gid.with_scale_and_position(scale, ab_glyph::point(x, y));
            if let Some(og) = self.0.outline_glyph(glyph) {
                let b = og.px_bounds();
                og.draw(|gx, gy, cov| {
                    let px = b.min.x.floor() as i32 + gx as i32;
                    let py = b.min.y.floor() as i32 + gy as i32;
                    if px >= bx0 && py >= by0 && px < bx1 && py < by1 {
                        f(px, py, cov);
                    }
                });
            }
            x += sf.h_advance(gid);
        }
    }
}
