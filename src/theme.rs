//! Overlay design tokens (dark + light) and OS light/dark detection.

use crate::config::{self, Config};
use crate::uifb::C4;

/// `0xRRGGBB` at `pct10` tenths of a percent opacity (960 = 96 %).
pub const fn rgba(hex: u32, pct10: u32) -> C4 {
    C4::new(
        (hex >> 16) as u8,
        (hex >> 8) as u8,
        hex as u8,
        ((pct10 * 255 + 500) / 1000) as u8,
    )
}

const fn rgb(hex: u32) -> C4 {
    rgba(hex, 1000)
}

/// Shadow layers as (y offset, spread) in logical px; index 0 is the contact
/// shadow, 3 the ambient one. Draw 3 → 0.
pub const SHADOW: [(f32, f32); 4] = [(1.0, 0.0), (3.0, 2.0), (7.0, 5.0), (12.0, 9.0)];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Theme {
    pub dark: bool,
    pub surface: C4,
    pub border: C4,
    pub separator: C4,
    pub icon: C4,
    pub icon_hover: C4,
    pub bg_hover: C4,
    pub bg_pressed: C4,
    /// Recessed fill of input fields (text field, dropdown, shortcut box).
    pub field_bg: C4,
    pub text: C4,
    pub text_muted: C4,
    pub accent: C4,
    pub accent_bg: C4,
    pub accent_bg_hover: C4,
    pub accent_ring: C4,
    pub accent_fg: C4,
    pub tooltip_bg: C4,
    pub tooltip_text: C4,
    pub key_bg: C4,
    pub key_text: C4,
    pub danger_bg: C4,
    pub danger_fg: C4,
    pub success: C4,
    pub error: C4,
    pub dim: C4,
    pub rim: C4,
    pub sel_outer: C4,
    pub sel_inner: C4,
    pub handle_halo: C4,
    pub shadow: [C4; 4],
}

pub const DARK: Theme = Theme {
    dark: true,
    surface: rgba(0x1B1C20, 960),
    border: rgba(0xFFFFFF, 90),
    separator: rgba(0xFFFFFF, 100),
    icon: rgb(0xA3A7B3),
    icon_hover: rgb(0xECEEF3),
    bg_hover: rgba(0xFFFFFF, 60),
    bg_pressed: rgba(0xFFFFFF, 110),
    field_bg: rgba(0x000000, 220),
    text: rgb(0xECEEF3),
    text_muted: rgb(0x9296A3),
    accent: rgb(0x8B93FF),
    accent_bg: rgba(0x8B93FF, 180),
    accent_bg_hover: rgba(0x8B93FF, 240),
    accent_ring: rgba(0x8B93FF, 300),
    accent_fg: rgb(0xB7BCFF),
    tooltip_bg: rgba(0x0E0F12, 970),
    tooltip_text: rgb(0xF2F3F6),
    key_bg: rgba(0xFFFFFF, 140),
    key_text: rgb(0xC4C7D0),
    danger_bg: rgba(0xFF6B6B, 160),
    danger_fg: rgb(0xFF8F8F),
    success: rgb(0x4ADE9A),
    error: rgb(0xFF7A7A),
    dim: rgba(0x07080B, 580),
    rim: rgba(0xFFFFFF, 220),
    sel_outer: rgba(0x000000, 400),
    sel_inner: rgba(0x000000, 280),
    handle_halo: rgba(0x000000, 450),
    shadow: [
        rgba(0x000000, 260),
        rgba(0x000000, 150),
        rgba(0x000000, 80),
        rgba(0x000000, 40),
    ],
};

pub const LIGHT: Theme = Theme {
    dark: false,
    surface: rgba(0xFAFAFC, 970),
    border: rgba(0x000000, 80),
    separator: rgba(0x000000, 100),
    icon: rgb(0x5F6473),
    icon_hover: rgb(0x15171E),
    bg_hover: rgba(0x000000, 60),
    bg_pressed: rgba(0x000000, 100),
    field_bg: rgb(0xFFFFFF),
    text: rgb(0x15171E),
    text_muted: rgb(0x737889),
    accent: rgb(0x5B63F5),
    accent_bg: rgba(0x5B63F5, 120),
    accent_bg_hover: rgba(0x5B63F5, 180),
    accent_ring: rgba(0x5B63F5, 320),
    accent_fg: rgb(0x4047D6),
    tooltip_bg: rgba(0x16171C, 970),
    tooltip_text: rgb(0xF2F3F6),
    key_bg: rgba(0xFFFFFF, 140),
    key_text: rgb(0xC4C7D0),
    danger_bg: rgba(0xDC2626, 100),
    danger_fg: rgb(0xC62828),
    success: rgb(0x1E9E63),
    error: rgb(0xD64545),
    dim: rgba(0x07080B, 460),
    rim: rgba(0x000000, 180),
    sel_outer: rgba(0xFFFFFF, 550),
    sel_inner: rgba(0xFFFFFF, 350),
    handle_halo: rgba(0x000000, 450),
    shadow: [
        rgba(0x000000, 140),
        rgba(0x000000, 90),
        rgba(0x000000, 50),
        rgba(0x000000, 25),
    ],
};

impl Theme {
    /// Replace the accent and re-derive its tints (spec: 18/24/30 % dark,
    /// 12/18/32 % light).
    pub fn with_accent(mut self, c: C4) -> Self {
        let (bg, hover, ring) = if self.dark { (180, 240, 300) } else { (120, 180, 320) };
        let tint = |pct10: u32| C4 {
            a: ((pct10 * 255 + 500) / 1000) as u8,
            ..c
        };
        self.accent = c;
        self.accent_bg = tint(bg);
        self.accent_bg_hover = tint(hover);
        self.accent_ring = tint(ring);
        self.accent_fg = c;
        self
    }

    /// Dim opacity for the configured `contrast_opacity` (default 148 = 58 %);
    /// the light theme dims 46/58 as much so the light bar still floats.
    pub fn dim_alpha(&self, cfg_opacity: u8) -> u8 {
        if self.dark {
            cfg_opacity
        } else {
            (cfg_opacity as u32 * 46 / 58) as u8
        }
    }
}

/// Theme for this capture: config `theme` first, then the OS setting.
pub fn resolve(cfg: &Config) -> Theme {
    resolve_with(cfg, os_prefers_dark)
}

pub fn resolve_with(cfg: &Config, probe: impl FnOnce() -> Option<bool>) -> Theme {
    let dark = match cfg.theme.trim().to_ascii_lowercase().as_str() {
        "dark" => true,
        "light" => false,
        _ => probe().unwrap_or(true),
    };
    let base = if dark { DARK } else { LIGHT };
    match config::parse_color(&cfg.ui_color) {
        Some((r, g, b, _)) => base.with_accent(C4::rgb(r, g, b)),
        None => base,
    }
}

#[cfg(windows)]
fn os_prefers_dark() -> Option<bool> {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    use windows::core::w;
    let mut val: u32 = 1;
    let mut len = std::mem::size_of::<u32>() as u32;
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut val).cast()),
            Some(&raw mut len),
        )
    };
    rc.is_ok().then_some(val == 0)
}

#[cfg(target_os = "macos")]
fn os_prefers_dark() -> Option<bool> {
    use crate::wind::{msg0, msg1, ns_string, objc_cls, objc_sel};
    use std::ffi::{CStr, c_char, c_void};
    // NSUserDefaults (not the `defaults` tool) so this works in the App Sandbox.
    unsafe {
        let defaults: *mut c_void =
            msg0(objc_cls(c"NSUserDefaults"), objc_sel(c"standardUserDefaults"));
        if defaults.is_null() {
            return None;
        }
        let v: *mut c_void = msg1(
            defaults,
            objc_sel(c"stringForKey:"),
            ns_string("AppleInterfaceStyle"),
        );
        if v.is_null() {
            return Some(false); // key absent = light mode
        }
        let p: *const c_char = msg0(v, objc_sel(c"UTF8String"));
        if p.is_null() {
            return Some(false);
        }
        Some(parse_macos_style(true, &CStr::from_ptr(p).to_string_lossy()))
    }
}

#[cfg(target_os = "linux")]
fn os_prefers_dark() -> Option<bool> {
    let out = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "color-scheme"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_gsettings(&String::from_utf8_lossy(&out.stdout))
}

/// `AppleInterfaceStyle` is "Dark" in dark mode and absent in light mode.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_macos_style(present: bool, value: &str) -> bool {
    present && value.trim() == "Dark"
}

/// `gsettings get org.gnome.desktop.interface color-scheme` output.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_gsettings(stdout: &str) -> Option<bool> {
    match stdout.trim().trim_matches('\'') {
        "prefer-dark" => Some(true),
        "default" | "prefer-light" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::uifb::C4;

    fn cfg(theme: &str, ui: &str) -> Config {
        Config {
            theme: theme.into(),
            ui_color: ui.into(),
            ..Config::default()
        }
    }

    #[test]
    fn percent_alpha_rounds() {
        assert_eq!(rgba(0x1B1C20, 960).a, 245);
        assert_eq!(rgba(0, 25).a, 6);
        assert_eq!(rgba(0xABCDEF, 1000), C4::rgb(0xAB, 0xCD, 0xEF));
    }

    #[test]
    fn explicit_mode_beats_os() {
        assert!(!resolve_with(&cfg("light", ""), || Some(true)).dark);
        assert!(resolve_with(&cfg("dark", ""), || Some(false)).dark);
    }

    #[test]
    fn auto_follows_os_and_defaults_dark() {
        assert!(!resolve_with(&cfg("auto", ""), || Some(false)).dark);
        assert!(resolve_with(&cfg("auto", ""), || None).dark);
        assert!(resolve_with(&cfg("bogus", ""), || None).dark);
    }

    #[test]
    fn default_accent_is_periwinkle() {
        assert_eq!(resolve_with(&cfg("dark", ""), || None).accent, C4::rgb(0x8B, 0x93, 0xFF));
        assert_eq!(resolve_with(&cfg("light", ""), || None).accent, C4::rgb(0x5B, 0x63, 0xF5));
    }

    #[test]
    fn ui_color_overrides_accent_and_tints() {
        let t = resolve_with(&cfg("dark", "#00ff00"), || None);
        assert_eq!(t.accent, C4::rgb(0, 255, 0));
        assert_eq!(t.accent_fg, C4::rgb(0, 255, 0));
        assert_eq!(t.accent_bg, C4::new(0, 255, 0, 46));
        assert_eq!(t.accent_ring, C4::new(0, 255, 0, 77));
    }

    #[test]
    fn light_dim_is_lighter() {
        assert_eq!(DARK.dim_alpha(148), 148);
        assert_eq!(LIGHT.dim_alpha(148), 117);
    }

    #[test]
    fn os_output_parsing() {
        assert_eq!(parse_gsettings("'prefer-dark'\n"), Some(true));
        assert_eq!(parse_gsettings("'default'"), Some(false));
        assert_eq!(parse_gsettings("weird"), None);
        assert!(parse_macos_style(true, "Dark"));
        assert!(!parse_macos_style(false, ""));
    }
}
