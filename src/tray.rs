//! Tray menu model shared by the per-OS backends.
#![cfg_attr(not(windows), allow(dead_code))] // used by the macOS/Linux trays (later tasks)

use crate::hotkey::HotEvent;
use std::sync::mpsc::Sender;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuItem {
    Capture,
    OpenConfig,
    CheckUpdates,
    /// Start at login; payload = currently enabled.
    Autostart(bool),
    Quit,
}

impl MenuItem {
    pub fn label(self) -> &'static str {
        match self {
            MenuItem::Capture => "Capture",
            MenuItem::OpenConfig => "Open config file",
            MenuItem::CheckUpdates => "Check for updates",
            MenuItem::Autostart(_) => "Start at login",
            MenuItem::Quit => "Quit rustshot",
        }
    }
}

/// Menu contents; `autostart` = None hides "Start at login" (managed installs).
pub fn menu(autostart: Option<bool>) -> Vec<MenuItem> {
    let mut v = vec![MenuItem::Capture, MenuItem::OpenConfig, MenuItem::CheckUpdates];
    if let Some(on) = autostart {
        v.push(MenuItem::Autostart(on));
    }
    v.push(MenuItem::Quit);
    v
}

/// Current menu for this install.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn current_menu() -> Vec<MenuItem> {
    let auto = crate::update::managed_install().is_none().then(crate::autostart::is_enabled);
    menu(auto)
}

/// Text for the result of an update check.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn update_message(r: &Result<Option<crate::update::Release>, String>) -> String {
    match r {
        Ok(Some(rel)) => format!("rustshot {} is available. Opening the download page.", rel.version),
        Ok(None) => "rustshot is up to date.".to_string(),
        Err(e) => format!("Update check failed: {e}"),
    }
}

/// Start the tray for the running daemon. `tx` receives Capture/Quit.
pub fn spawn(tx: Sender<HotEvent>) {
    #[cfg(windows)]
    crate::tray_win::spawn(tx);
    #[cfg(not(windows))]
    let _ = tx; // macOS / Linux backends arrive in later tasks.
}

/// Tray glyph on a 16-unit grid: four rounded corner brackets (stroked) and a centre dot.
const GLYPH_PATH: &str = "M1.5 6V3.5a2 2 0 0 1 2-2H6 M10 1.5h2.5a2 2 0 0 1 2 2V6 M14.5 10v2.5a2 2 0 0 1-2 2H10 M6 14.5H3.5a2 2 0 0 1-2-2V10";
const GLYPH_STROKE: f32 = 1.6;
const GLYPH_DOT_R: f32 = 2.4;

/// Monochrome tray glyph as straight-alpha RGBA, `size`x`size`, in colour `rgb`.
/// Coverage is rasterised white-on-black into an opaque scratch buffer (the
/// rasterizer assumes an opaque destination) and its red channel becomes alpha.
#[allow(dead_code)] // Windows today; the macOS/Linux trays reuse it later
pub fn tray_glyph_rgba(size: u32, rgb: (u8, u8, u8)) -> Vec<u8> {
    use crate::raster::Blend;
    use crate::uifb::{C4, Fb};
    let n = size as usize;
    let k = size as f32 / 16.0;
    let mut scratch = [0u8, 0, 0, 255].repeat(n * n);
    let white = C4::rgb(255, 255, 255);
    {
        let mut fb = Fb::new(&mut scratch, n);
        if let Some(lines) = crate::icon_path::parse_path(GLYPH_PATH) {
            let mut surf = fb.surf();
            for mut line in lines {
                for p in &mut line {
                    p.x *= k;
                    p.y *= k;
                }
                surf.stroke_polyline(&line, GLYPH_STROKE * k, white, Blend::Normal);
            }
        }
        fb.fill_circle(8.0 * k, 8.0 * k, GLYPH_DOT_R * k, white);
    }
    let mut out = Vec::with_capacity(n * n * 4);
    for px in scratch.as_chunks::<4>().0 {
        out.extend_from_slice(&[rgb.0, rgb.1, rgb.2, px[0]]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_composition() {
        assert_eq!(
            menu(Some(true)),
            vec![
                MenuItem::Capture,
                MenuItem::OpenConfig,
                MenuItem::CheckUpdates,
                MenuItem::Autostart(true),
                MenuItem::Quit
            ]
        );
        assert_eq!(menu(None).len(), 4);
        assert!(!menu(None).iter().any(|m| matches!(m, MenuItem::Autostart(_))));
        assert_eq!(*menu(None).last().unwrap(), MenuItem::Quit);
    }

    #[test]
    fn labels() {
        assert_eq!(MenuItem::Capture.label(), "Capture");
        assert_eq!(MenuItem::OpenConfig.label(), "Open config file");
        assert_eq!(MenuItem::CheckUpdates.label(), "Check for updates");
        assert_eq!(MenuItem::Autostart(false).label(), "Start at login");
        assert_eq!(MenuItem::Quit.label(), "Quit rustshot");
    }

    #[test]
    fn update_messages() {
        assert!(update_message(&Ok(None)).contains("up to date"));
        assert!(update_message(&Err("boom".into())).contains("boom"));
        let r = crate::update::Release { version: "9.9.9".into(), ..Default::default() };
        assert!(update_message(&Ok(Some(r))).contains("9.9.9"));
    }

    fn alpha(buf: &[u8], size: u32, x: u32, y: u32) -> u8 {
        buf[((y * size + x) * 4 + 3) as usize]
    }

    #[test]
    fn glyph_shape() {
        for size in [16u32, 32] {
            let b = tray_glyph_rgba(size, (27, 28, 32));
            assert_eq!(b.len(), (size * size * 4) as usize);
            let c = size / 2;
            let i = ((c * size + c) * 4) as usize;
            assert_eq!(&b[i..i + 4], &[27, 28, 32, 255]);
            let u = size as f32 / 16.0;
            // Corner brackets: the arc apex and the straight runs have coverage.
            assert!(alpha(&b, size, (2.1 * u) as u32, (2.1 * u) as u32) > 100);
            assert!(alpha(&b, size, (13.9 * u) as u32, (13.9 * u) as u32) > 100);
            // Gap between bracket and dot, and the image corners, are empty.
            assert_eq!(alpha(&b, size, (4.5 * u) as u32, (4.5 * u) as u32), 0);
            for (x, y) in [(0, 0), (size - 1, 0), (0, size - 1), (size - 1, size - 1)] {
                assert_eq!(alpha(&b, size, x, y), 0);
            }
        }
    }

    /// Writes enlarged previews for eyeballing; run with --ignored.
    #[test]
    #[ignore = "writes preview PNGs to the scratchpad"]
    fn write_glyph_previews() {
        let dir = std::path::Path::new(
            "C:/Users/plato/AppData/Local/Temp/claude/E--Personal-rustshot/388c4929-22ab-4934-9dbf-1b81d7818b10/scratchpad/glyph",
        );
        std::fs::create_dir_all(dir).unwrap();
        for size in [16u32, 20, 24, 32] {
            for (name, fg, bg) in [("dark", (255u8, 255u8, 255u8), 0x20u8), ("light", (27, 28, 32), 0xF3)] {
                let g = tray_glyph_rgba(size, fg);
                let scale = 8u32;
                let w = size * scale;
                let mut img = Vec::with_capacity((w * w * 3) as usize);
                for y in 0..w {
                    for x in 0..w {
                        let i = (((y / scale) * size + x / scale) * 4) as usize;
                        let a = g[i + 3] as u32;
                        for ch in 0..3 {
                            let v = (g[i + ch] as u32 * a + bg as u32 * (255 - a) + 127) / 255;
                            img.push(v as u8);
                        }
                    }
                }
                let f = std::fs::File::create(dir.join(format!("glyph_{size}_{name}.png"))).unwrap();
                let mut enc = png::Encoder::new(std::io::BufWriter::new(f), w, w);
                enc.set_color(png::ColorType::Rgb);
                enc.set_depth(png::BitDepth::Eight);
                enc.write_header().unwrap().write_image_data(&img).unwrap();
            }
        }
    }
}
