use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Settings, mirroring Flameshot's key names where they apply.
#[derive(Debug, Clone)]
pub struct Config {
    /// Directory to save captures into. Empty = `<Pictures>/rustshot`.
    pub save_path: String,
    /// Put each capture in a subfolder of `save_path` named by
    /// `subfolder_pattern` (one folder per day by default).
    pub save_subfolder: bool,
    /// Pattern for that subfolder (same tokens as `filename_pattern`;
    /// `/` nests folders, `..` is dropped).
    pub subfolder_pattern: String,
    /// strftime-like pattern, e.g. `%F_%H-%M`.
    pub filename_pattern: String,
    /// Format for Ctrl+S / direct captures: "png", "jpg" or "bmp".
    pub save_format: String,
    /// JPEG quality, 1-100.
    pub jpeg_quality: u8,
    /// Ctrl+S always asks with a Save As dialog (the old behaviour).
    pub save_dialog: bool,
    /// Accent override for the overlay UI (Flameshot `uiColor`); empty =
    /// the theme's own accent.
    pub ui_color: String,
    /// Overlay theme: "auto" (follow the OS), "dark" or "light".
    pub theme: String,
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
    /// If true, GUI captures only the monitor under the cursor even when
    /// all monitors share the same scale factor.
    pub capture_active_monitor: bool,
    /// Let the daemon check GitHub for a newer release once a day.
    pub check_updates: bool,
    /// Overlay renderer on Windows: "gdi" (screenshot kept in GDI bitmaps,
    /// only changed rects repainted; the default) or "software" (whole
    /// frame composed in memory). Linux/macOS always use software.
    pub renderer: String,
    /// Editor shortcut overrides from the `[shortcuts]` table
    /// (`action id -> "Ctrl+Shift+S"`; see `keymap`).
    pub shortcuts: BTreeMap<String, String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            save_path: String::new(),
            save_subfolder: true,
            subfolder_pattern: "%F".into(),
            filename_pattern: "%F_%H-%M".into(),
            save_format: "png".into(),
            jpeg_quality: 90,
            save_dialog: false,
            ui_color: String::new(),
            theme: "auto".into(),
            contrast_opacity: 148,
            draw_color: "#f04438".into(),
            draw_thickness: 3.0,
            draw_marker_size: 15.0,
            draw_pixelate_size: 12.0,
            draw_font_size: 16.0,
            undo_limit: 100,
            capture_hotkey: "Meta+Shift+X".into(),
            quit_hotkey: "Ctrl+Alt+Shift+Q".into(),
            user_colors: [
                "#f04438", "#ff8a1f", "#ffc532", "#2dc06f", "#19b5d6", "#3b82f6", "#8b5cf6",
                "#ec4899", "#ffffff", "#111318",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            upload_client_id: "313baf0c7b4d3ff".into(),
            copy_url_after_upload: true,
            capture_active_monitor: false,
            check_updates: true,
            renderer: "gdi".into(),
            shortcuts: BTreeMap::new(),
        }
    }
}

impl Config {
    /// Whether captures use the GDI overlay renderer: Windows only, and
    /// any value but "software" (unknown values fall back to gdi).
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn use_gdi(&self) -> bool {
        cfg!(windows) && !self.renderer.trim().eq_ignore_ascii_case("software")
    }
}

pub fn config_dir() -> PathBuf {
    #[cfg(windows)]
    if let Ok(v) = std::env::var("APPDATA")
        && !v.is_empty()
    {
        return PathBuf::from(v).join("rustshot");
    }
    #[cfg(unix)]
    if let Ok(v) = std::env::var("XDG_CONFIG_HOME")
        && !v.is_empty()
    {
        return PathBuf::from(v).join("rustshot");
    }
    #[cfg(unix)]
    if let Ok(v) = std::env::var("HOME")
        && !v.is_empty()
    {
        return PathBuf::from(v).join(".config").join("rustshot");
    }
    PathBuf::from(".")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

/// Make sure `config.toml` exists (written with defaults if missing); returns its path.
pub fn ensure_config_file() -> std::io::Result<PathBuf> {
    let path = config_path();
    std::fs::create_dir_all(config_dir())?;
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut f) => {
            use std::io::Write;
            f.write_all(to_toml(&Config::default()).as_bytes())?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    Ok(path)
}

pub fn load() -> Config {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => match parse_config(&text) {
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
    let cfg = parse_config(&text)
        .map_err(anyhow::Error::msg)
        .context("parse config")?;
    println!("config OK: {}", path.display());
    println!("save_path = {:?}", cfg.save_path);
    println!("filename_pattern = {:?}", cfg.filename_pattern);
    println!("capture_hotkey = {:?}", cfg.capture_hotkey);
    if let Some(w) = renderer_warning(&cfg.renderer) {
        println!("warning: {w}");
    }
    for w in shortcut_warnings(&cfg) {
        println!("warning: {w}");
    }
    Ok(())
}

/// `[shortcuts]` problems: unknown actions, bad chords, and chords bound
/// to several actions (the first in table order wins).
pub fn shortcut_warnings(cfg: &Config) -> Vec<String> {
    let (km, mut out) = crate::keymap::Keymap::from_config(&cfg.shortcuts);
    for (c, acts) in km.conflicts() {
        let ids: Vec<&str> = acts.iter().map(|a| a.id()).collect();
        out.push(format!(
            "[shortcuts] {} is bound to {}; {} wins",
            c.display(),
            ids.join(", "),
            ids[0]
        ));
    }
    out
}

/// A warning for a `renderer` value other than "gdi" / "software" (the
/// overlay then uses gdi on Windows).
pub fn renderer_warning(v: &str) -> Option<String> {
    let t = v.trim();
    (!t.eq_ignore_ascii_case("gdi") && !t.eq_ignore_ascii_case("software"))
        .then(|| format!("unknown renderer {v:?} (expected \"gdi\" or \"software\"); using \"gdi\" on Windows"))
}

/// Hand-rolled parser for the flat `key = value` subset of TOML this config
/// uses: quoted strings, numbers, booleans, and arrays of strings.
pub fn parse_config(text: &str) -> Result<Config, String> {
    let mut cfg = Config::default();
    // Current `[table]`: "" = top level; keys of other tables are ignored.
    let mut table = String::new();
    for (idx, raw) in text.lines().enumerate() {
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            table = name.trim().to_string();
            continue;
        }
        let Some((key, val)) = line.split_once('=') else {
            return Err(format!("line {}: expected key = value", idx + 1));
        };
        let key = key.trim();
        let val = val.trim();
        let bad = |kind: &str, v: &str| format!("line {}: invalid {kind} for '{key}': {v}", idx + 1);
        if table == "shortcuts" {
            let key = key.trim_matches('"');
            let v = as_string(val).map_err(|e| bad("string", &e))?;
            cfg.shortcuts.insert(key.to_string(), v);
            continue;
        }
        if !table.is_empty() {
            continue;
        }
        match key {
            "save_path" => cfg.save_path = as_string(val).map_err(|e| bad("string", &e))?,
            "filename_pattern" => {
                cfg.filename_pattern = as_string(val).map_err(|e| bad("string", &e))?
            }
            "ui_color" => cfg.ui_color = as_string(val).map_err(|e| bad("string", &e))?,
            "theme" => cfg.theme = as_string(val).map_err(|e| bad("string", &e))?,
            "draw_color" => cfg.draw_color = as_string(val).map_err(|e| bad("string", &e))?,
            "capture_hotkey" => {
                cfg.capture_hotkey = as_string(val).map_err(|e| bad("string", &e))?
            }
            "quit_hotkey" => cfg.quit_hotkey = as_string(val).map_err(|e| bad("string", &e))?,
            "upload_client_id" => {
                cfg.upload_client_id = as_string(val).map_err(|e| bad("string", &e))?
            }
            "contrast_opacity" => {
                cfg.contrast_opacity = as_u64(val)
                    .map_err(|e| bad("number", &e))?
                    .try_into()
                    .map_err(|_| bad("number", "out of range"))?
            }
            "save_subfolder" => {
                cfg.save_subfolder = as_bool(val).map_err(|e| bad("boolean", &e))?
            }
            "subfolder_pattern" => {
                cfg.subfolder_pattern = as_string(val).map_err(|e| bad("string", &e))?
            }
            "save_format" => {
                let v = as_string(val).map_err(|e| bad("string", &e))?;
                let v = v.trim().to_ascii_lowercase();
                if !matches!(v.as_str(), "png" | "jpg" | "jpeg" | "bmp") {
                    return Err(bad("value (expected \"png\", \"jpg\" or \"bmp\")", &v));
                }
                cfg.save_format = v;
            }
            "jpeg_quality" => {
                cfg.jpeg_quality = as_u64(val)
                    .ok()
                    .filter(|q| (1..=100).contains(q))
                    .ok_or_else(|| bad("number (1-100)", val))? as u8
            }
            "save_dialog" => cfg.save_dialog = as_bool(val).map_err(|e| bad("boolean", &e))?,
            "draw_thickness" => cfg.draw_thickness = as_f64(val)? as f32,
            "draw_marker_size" => cfg.draw_marker_size = as_f64(val)? as f32,
            "draw_pixelate_size" => cfg.draw_pixelate_size = as_f64(val)? as f32,
            "draw_font_size" => cfg.draw_font_size = as_f64(val)? as f32,
            "undo_limit" => {
                cfg.undo_limit = as_u64(val)
                    .map_err(|e| bad("number", &e))?
                    .try_into()
                    .map_err(|_| bad("number", "out of range"))?
            }
            "user_colors" => cfg.user_colors = as_array(val).map_err(|e| bad("array", &e))?,
            "copy_url_after_upload" => {
                cfg.copy_url_after_upload =
                    as_bool(val).map_err(|e| bad("boolean", &e))?
            }
            "capture_active_monitor" => {
                cfg.capture_active_monitor = as_bool(val).map_err(|e| bad("boolean", &e))?
            }
            "check_updates" => {
                cfg.check_updates = as_bool(val).map_err(|e| bad("boolean", &e))?
            }
            "renderer" => cfg.renderer = as_string(val).map_err(|e| bad("string", &e))?,
            _ => {} // unknown keys are ignored, as with serde's default
        }
    }
    Ok(cfg)
}

/// Render the config as the same flat TOML subset.
pub fn to_toml(c: &Config) -> String {
    fn q(s: &str) -> String {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    }
    let colors: Vec<String> = c.user_colors.iter().map(|s| q(s)).collect();
    let mut out = format!(
        "save_path = {}\n\
         save_subfolder = {}\n\
         subfolder_pattern = {}\n\
         filename_pattern = {}\n\
         save_format = {}\n\
         jpeg_quality = {}\n\
         save_dialog = {}\n\
         ui_color = {}\n\
         theme = {}\n\
         contrast_opacity = {}\n\
         draw_color = {}\n\
         draw_thickness = {}\n\
         draw_marker_size = {}\n\
         draw_pixelate_size = {}\n\
         draw_font_size = {}\n\
         undo_limit = {}\n\
         capture_hotkey = {}\n\
         quit_hotkey = {}\n\
         user_colors = [{}]\n\
         upload_client_id = {}\n\
         copy_url_after_upload = {}\n\
         capture_active_monitor = {}\n\
         check_updates = {}\n\
         renderer = {}\n",
        q(&c.save_path),
        c.save_subfolder,
        q(&c.subfolder_pattern),
        q(&c.filename_pattern),
        q(&c.save_format),
        c.jpeg_quality,
        c.save_dialog,
        q(&c.ui_color),
        q(&c.theme),
        c.contrast_opacity,
        q(&c.draw_color),
        c.draw_thickness,
        c.draw_marker_size,
        c.draw_pixelate_size,
        c.draw_font_size,
        c.undo_limit,
        q(&c.capture_hotkey),
        q(&c.quit_hotkey),
        colors.join(", "),
        q(&c.upload_client_id),
        c.copy_url_after_upload,
        c.capture_active_monitor,
        c.check_updates,
        q(&c.renderer),
    );
    if !c.shortcuts.is_empty() {
        out.push_str("
[shortcuts]
");
        for (k, v) in &c.shortcuts {
            out.push_str(&format!("{k} = {}
", q(v)));
        }
    }
    out
}

/// Cut a trailing `# comment`, respecting quotes.
fn strip_comment(line: &str) -> &str {
    let mut in_str = false;
    let mut esc = false;
    for (i, c) in line.char_indices() {
        if esc {
            esc = false;
            continue;
        }
        match c {
            '\\' if in_str => esc = true,
            '"' => in_str = !in_str,
            '#' if !in_str => return &line[..i],
            _ => {}
        }
    }
    line
}

fn as_string(v: &str) -> Result<String, String> {
    let v = v.trim();
    if v.len() < 2 || !v.starts_with('"') || !v.ends_with('"') {
        return Err(format!("expected quoted string, got {v}"));
    }
    let inner = &v[1..v.len() - 1];
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some(o) => out.push(o),
            None => return Err("trailing backslash".into()),
        }
    }
    Ok(out)
}

fn as_u64(v: &str) -> Result<u64, String> {
    v.trim()
        .parse::<u64>()
        .map_err(|_| format!("expected number, got {v}"))
}

fn as_f64(v: &str) -> Result<f64, String> {
    v.trim()
        .parse::<f64>()
        .map_err(|_| format!("expected number, got {v}"))
}

fn as_bool(v: &str) -> Result<bool, String> {
    match v.trim() {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(format!("expected true/false, got {other}")),
    }
}

fn as_array(v: &str) -> Result<Vec<String>, String> {
    let v = v.trim();
    if v.len() < 2 || !v.starts_with('[') || !v.ends_with(']') {
        return Err(format!("expected [array], got {v}"));
    }
    let inner = &v[1..v.len() - 1];
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_str = false;
    let mut esc = false;
    for c in inner.chars() {
        if esc {
            cur.push(c);
            esc = false;
            continue;
        }
        match c {
            '\\' if in_str => {
                cur.push(c);
                esc = true;
            }
            '"' => {
                in_str = !in_str;
                cur.push(c);
            }
            ',' if !in_str => {
                let item = cur.trim();
                if !item.is_empty() {
                    out.push(as_string(item)?);
                }
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    let item = cur.trim();
    if !item.is_empty() {
        out.push(as_string(item)?);
    }
    if in_str {
        return Err("unterminated string in array".into());
    }
    Ok(out)
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
    fn shortcuts_table() {
        let text = "theme = \"dark\"\n[shortcuts]\nsave_as = \"Ctrl+Alt+S\"\ncopy = \"\" # unbound\nbogus = \"N\"\n[other]\ntheme = \"light\"\n";
        let cfg = parse_config(text).unwrap();
        assert_eq!(cfg.theme, "dark", "keys of other tables are ignored");
        assert_eq!(cfg.shortcuts.len(), 3);
        assert_eq!(cfg.shortcuts["save_as"], "Ctrl+Alt+S");
        assert_eq!(cfg.shortcuts["copy"], "");
        let back = parse_config(&to_toml(&cfg)).unwrap();
        assert_eq!(back.shortcuts, cfg.shortcuts);
        assert!(!to_toml(&Config::default()).contains("[shortcuts]"));
        let w = shortcut_warnings(&cfg);
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("bogus"));
        let clash = Config {
            shortcuts: [("cancel".to_string(), "P".to_string())].into_iter().collect(),
            ..Config::default()
        };
        let w = shortcut_warnings(&clash);
        assert_eq!(w, ["[shortcuts] P is bound to tool_pencil, cancel; tool_pencil wins"]);
    }

    #[test]
    fn default_config_roundtrips() {
        let text = to_toml(&Config::default());
        let back = parse_config(&text).unwrap();
        assert_eq!(back.filename_pattern, "%F_%H-%M");
        assert_eq!(back.draw_thickness, 3.0);
        assert_eq!(back.contrast_opacity, 148);
        assert_eq!(back.user_colors.len(), 10);
        assert!(back.copy_url_after_upload);
        assert!(back.check_updates);
        let off = to_toml(&Config { check_updates: false, ..Config::default() });
        assert!(!parse_config(&off).unwrap().check_updates);
        assert_eq!(back.renderer, "gdi");
        let sw = to_toml(&Config { renderer: "software".into(), ..Config::default() });
        assert_eq!(parse_config(&sw).unwrap().renderer, "software");
        assert!(!parse_config(&sw).unwrap().use_gdi());
        assert_eq!(back.use_gdi(), cfg!(windows));
        let odd = Config { renderer: "vulkan".into(), ..Config::default() };
        assert_eq!(odd.use_gdi(), cfg!(windows), "unknown values fall back to gdi");
        assert!(renderer_warning("vulkan").is_some_and(|w| w.contains("\"vulkan\"")));
        for ok in ["gdi", "software", " Software "] {
            assert_eq!(renderer_warning(ok), None, "{ok}");
        }
    }

    #[test]
    fn parses_comments_and_escapes() {
        let text = concat!(
            "# full-line comment\n",
            "ui_color = \"#ffffff\"  # inline comment after a color\n",
            "save_path = \"C:\\\\shots #1\"\n",
            "user_colors = [\"#fff\", \"#000\"]\n",
            "unknown_key = 42\n",
        );
        let cfg = parse_config(text).unwrap();
        assert_eq!(cfg.ui_color, "#ffffff");
        assert_eq!(cfg.save_path, "C:\\shots #1");
        assert_eq!(cfg.user_colors, vec!["#fff", "#000"]);
    }

    #[test]
    fn rejects_bad_values() {
        assert!(parse_config("draw_thickness = \"x\"").is_err());
        assert!(parse_config("copy_url_after_upload = 1").is_err());
        assert!(parse_config("contrast_opacity = 999").is_err());
        assert!(parse_config("not a assignment").is_err());
        assert!(parse_config("save_format = \"gif\"").is_err());
        assert!(parse_config("jpeg_quality = 0").is_err());
        assert!(parse_config("jpeg_quality = 101").is_err());
        assert!(parse_config("save_subfolder = 1").is_err());
    }

    #[test]
    fn saving_keys_roundtrip() {
        let d = Config::default();
        assert!(d.save_subfolder);
        assert_eq!(d.subfolder_pattern, "%F");
        assert_eq!(d.save_format, "png");
        assert_eq!(d.jpeg_quality, 90);
        assert!(!d.save_dialog);
        let back = parse_config(&to_toml(&d)).unwrap();
        assert!(back.save_subfolder);
        assert_eq!(back.subfolder_pattern, "%F");
        assert_eq!(back.save_format, "png");
        assert_eq!(back.jpeg_quality, 90);
        assert!(!back.save_dialog);
        let c = Config {
            save_subfolder: false,
            subfolder_pattern: "%Y/%m".into(),
            save_format: "jpg".into(),
            jpeg_quality: 1,
            save_dialog: true,
            ..Config::default()
        };
        let back = parse_config(&to_toml(&c)).unwrap();
        assert!(!back.save_subfolder);
        assert_eq!(back.subfolder_pattern, "%Y/%m");
        assert_eq!(back.save_format, "jpg");
        assert_eq!(back.jpeg_quality, 1);
        assert!(back.save_dialog);
        assert_eq!(parse_config("save_format = \" BMP \"").unwrap().save_format, "bmp");
        assert_eq!(parse_config("jpeg_quality = 100").unwrap().jpeg_quality, 100);
    }
}
