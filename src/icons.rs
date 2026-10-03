use crate::pixbuf::PixBuf;
use std::collections::HashMap;

/// Material icons from Flameshot (`data/img/material/white`), pre-rasterized
/// to 48px PNG by `gen_icons` (SVG originals kept alongside for regeneration).
const ICONS: [(&str, &[u8]); 19] = [
    ("pencil", include_bytes!("../assets/icons/pencil.png")),
    ("line", include_bytes!("../assets/icons/line.png")),
    (
        "arrow-bottom-left",
        include_bytes!("../assets/icons/arrow-bottom-left.png"),
    ),
    (
        "square-outline",
        include_bytes!("../assets/icons/square-outline.png"),
    ),
    (
        "circle-outline",
        include_bytes!("../assets/icons/circle-outline.png"),
    ),
    ("marker", include_bytes!("../assets/icons/marker.png")),
    ("text", include_bytes!("../assets/icons/text.png")),
    ("pixelate", include_bytes!("../assets/icons/pixelate.png")),
    ("invert", include_bytes!("../assets/icons/invert.png")),
    (
        "undo-variant",
        include_bytes!("../assets/icons/undo-variant.png"),
    ),
    (
        "redo-variant",
        include_bytes!("../assets/icons/redo-variant.png"),
    ),
    ("minus", include_bytes!("../assets/icons/minus.png")),
    ("plus", include_bytes!("../assets/icons/plus.png")),
    (
        "content-copy",
        include_bytes!("../assets/icons/content-copy.png"),
    ),
    (
        "content-save",
        include_bytes!("../assets/icons/content-save.png"),
    ),
    (
        "cloud-upload",
        include_bytes!("../assets/icons/cloud-upload.png"),
    ),
    ("close", include_bytes!("../assets/icons/close.png")),
    ("accept", include_bytes!("../assets/icons/accept.png")),
    (
        "size_indicator",
        include_bytes!("../assets/icons/size_indicator.png"),
    ),
];

/// Toolbar icon size, in pixels.
const ICON_PX: u32 = 20;

pub struct Icons {
    map: HashMap<&'static str, PixBuf>,
}

impl Icons {
    pub fn load() -> Self {
        let mut map = HashMap::new();
        for (name, bytes) in ICONS {
            match PixBuf::from_png(bytes) {
                Ok(img) => {
                    map.insert(name, scale_to(&img, ICON_PX, ICON_PX));
                }
                Err(e) => eprintln!("warning: could not load icon {name}: {e:#}"),
            }
        }
        Self { map }
    }

    pub fn get(&self, name: &str) -> Option<&PixBuf> {
        self.map.get(name)
    }
}

/// Box-filter resample to exactly `w` x `h`.
fn scale_to(src: &PixBuf, w: u32, h: u32) -> PixBuf {
    let (sw, sh) = src.dimensions();
    let mut out = PixBuf::new(w, h);
    if sw == 0 || sh == 0 || w == 0 || h == 0 {
        return out;
    }
    let s = src.as_raw();
    let d = out.as_raw_mut();
    for y in 0..h {
        let sy0 = (y as u64 * sh as u64) / h as u64;
        let sy1 = (((y as u64 + 1) * sh as u64).div_ceil(h as u64))
            .max(sy0 + 1)
            .min(sh as u64);
        for x in 0..w {
            let sx0 = (x as u64 * sw as u64) / w as u64;
            let sx1 = (((x as u64 + 1) * sw as u64).div_ceil(w as u64))
                .max(sx0 + 1)
                .min(sw as u64);
            let (mut pr, mut pg, mut pb, mut pa) = (0u32, 0u32, 0u32, 0u32);
            for yy in sy0..sy1 {
                for xx in sx0..sx1 {
                    let i = (yy as usize * sw as usize + xx as usize) * 4;
                    let a = s[i + 3] as u32;
                    pr += s[i] as u32 * a;
                    pg += s[i + 1] as u32 * a;
                    pb += s[i + 2] as u32 * a;
                    pa += a;
                }
            }
            let di = (y as usize * w as usize + x as usize) * 4;
            let cells = (sx1 - sx0) * (sy1 - sy0);
            d[di + 3] = (pa / cells as u32) as u8;
            if let (Some(r), Some(g), Some(b)) = (
                pr.checked_div(pa),
                pg.checked_div(pa),
                pb.checked_div(pa),
            ) {
                d[di] = r.min(255) as u8;
                d[di + 1] = g.min(255) as u8;
                d[di + 2] = b.min(255) as u8;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_and_scales_all_icons() {
        let icons = Icons::load();
        for (name, _) in ICONS {
            let ic = icons.get(name).unwrap_or_else(|| panic!("missing {name}"));
            assert_eq!(ic.dimensions(), (ICON_PX, ICON_PX), "{name}");
        }
    }
}
