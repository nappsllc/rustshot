use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Settings, mirroring Flameshot's key names where they apply.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Directory to save captures into. Empty = user Pictures folder.
    pub save_path: String,
    /// strftime-like pattern, e.g. `%F_%H-%M`.
    pub filename_pattern: String,
    /// Accent color for the overlay UI (Flameshot `uiColor`).
    pub ui_color: String,
    /// Darkness of the dimmed area outside the selection (0-255).
    pub contrast_opacity: u8,
    pub draw_color: String,
    pub draw_thickness: f32,
    pub draw_marker_size: f32,
    pub draw_pixelate_size: f32,
    pub draw_font_size: f32,
    pub undo_limit: usize,
    /// Global hotkey that starts a capture (daemon mode).
    pub capture_hotkey: String,
    /// Global hotkey that quits the daemon.
    pub quit_hotkey: String,
    /// Palette shown in the color picker.
    pub user_colors: Vec<String>,
    /// Imgur client id used for uploads.
    pub upload_client_id: String,
    /// Copy the resulting URL to the clipboard after upload.
    pub copy_url_after_upload: bool,
    /// JPEG quality used when saving/copying as JPEG.
    pub jpeg_quality: u8,
    /// If true, GUI captures only the monitor under the cursor even when
    /// all monitors share the same scale factor.
    pub capture_active_monitor: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            save_path: String::new(),
            filename_pattern: "%F_%H-%M".into(),
            ui_color: "#740096".into(),
            contrast_opacity: 190,
            draw_color: "#ff0000".into(),
            draw_thickness: 3.0,
            draw_marker_size: 15.0,
            draw_pixelate_size: 12.0,
            draw_font_size: 16.0,
            undo_limit: 100,
            capture_hotkey: "Meta+Shift+X".into(),
            quit_hotkey: "Ctrl+Alt+Shift+Q".into(),
            user_colors: [
                "#ffffff", "#ff0000", "#ffff00", "#00ff00", "#008000", "#00ffff", "#0000ff",
                "#ff00ff", "#800080", "#800000", "#000000",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            upload_client_id: "313baf0c7b4d3ff".into(),
            copy_url_after_upload: true,
            jpeg_quality: 75,
            capture_active_monitor: false,
        }
    }
}

pub fn config_dir() -> PathBuf {
    match std::env::var("APPDATA") {
        Ok(v) if !v.is_empty() => PathBuf::from(v).join("rustshot"),
        _ => PathBuf::from("."),
    }
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

pub fn load() -> Config {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => match toml::from_str::<Config>(&text) {
            Ok(cfg) => cfg,
            Err(e) => {
                eprintln!("warning: ignoring invalid config {}: {e}", path.display());
                Config::default()
            }
        },
        Err(_) => Config::default(),
    }
}

/// Validate that the config parses; used by `rustshot config --check`.
pub fn check() -> Result<()> {
    let path = config_path();
    if !path.exists() {
        println!("config file does not exist (defaults will be used): {}", path.display());
        return Ok(());
    }
    let text = std::fs::read_to_string(&path).context("read config")?;
    let cfg: Config = toml::from_str(&text).context("parse config")?;
    println!("config OK: {}", path.display());
    println!("save_path = {:?}", cfg.save_path);
    println!("filename_pattern = {:?}", cfg.filename_pattern);
    println!("capture_hotkey = {:?}", cfg.capture_hotkey);
    Ok(())
}

/// Parse `#rrggbb` / `#rgb` into (r, g, b, a).
pub fn parse_color(s: &str) -> Option<(u8, u8, u8, u8)> {
    let s = s.trim();
    let hex = s.strip_prefix('#')?;
    match hex.len() {
        3 => {
            let r = u8::from_str_radix(&hex[0..1].repeat(2), 16).ok()?;
            let g = u8::from_str_radix(&hex[1..2].repeat(2), 16).ok()?;
            let b = u8::from_str_radix(&hex[2..3].repeat(2), 16).ok()?;
            Some((r, g, b, 255))
        }
        6 | 8 => {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            let a = if hex.len() == 8 {
                u8::from_str_radix(&hex[6..8], 16).ok()?
            } else {
                255
            };
            Some((r, g, b, a))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_colors() {
        assert_eq!(parse_color("#ff0000"), Some((255, 0, 0, 255)));
        assert_eq!(parse_color("#f00"), Some((255, 0, 0, 255)));
        assert_eq!(parse_color("#10203040"), Some((16, 32, 48, 64)));
        assert_eq!(parse_color("red"), None);
    }

    #[test]
    fn default_config_roundtrips() {
        let text = toml::to_string(&Config::default()).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.filename_pattern, "%F_%H-%M");
    }
}
