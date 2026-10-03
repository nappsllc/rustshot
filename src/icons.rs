use crate::pixbuf::PixBuf;
use eframe::egui::{ColorImage, Context, TextureHandle, TextureOptions};
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

pub struct Icons {
    map: HashMap<&'static str, TextureHandle>,
}

impl Icons {
    pub fn load(ctx: &Context) -> Self {
        let mut map = HashMap::new();
        for (name, bytes) in ICONS {
            if let Some(tex) = load_png(ctx, name, bytes) {
                map.insert(name, tex);
            } else {
                eprintln!("warning: could not load icon {name}");
            }
        }
        Self { map }
    }

    pub fn get(&self, name: &str) -> Option<&TextureHandle> {
        self.map.get(name)
    }
}

fn load_png(_ctx: &Context, name: &str, bytes: &[u8]) -> Option<TextureHandle> {
    let img = PixBuf::from_png(bytes).ok()?;
    let size = [img.width() as usize, img.height() as usize];
    let ci = ColorImage::from_rgba_unmultiplied(size, img.as_raw());
    Some(_ctx.load_texture(format!("icon_{name}"), ci, TextureOptions::LINEAR))
}
