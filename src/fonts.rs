//! UI font (embedded Inter Medium subset, see tools/subset-font.sh) and the
//! system font used for annotation text, which may be in any script.

use ab_glyph::FontArc;

static INTER: &[u8] = include_bytes!("../assets/fonts/Inter-Medium.subset.ttf");

/// Annotation-text font candidates: Windows, macOS, then common Linux paths.
const SYSTEM_CANDIDATES: &[&str] = &[
    r"C:\Windows\Fonts\segoeui.ttf",
    r"C:\Windows\Fonts\arial.ttf",
    r"C:\Windows\Fonts\tahoma.ttf",
    "/System/Library/Fonts/Helvetica.ttc",
    "/System/Library/Fonts/Supplemental/Arial.ttf",
    "/Library/Fonts/Arial.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
    "/usr/share/fonts/truetype/freefont/FreeSans.ttf",
];

/// The overlay chrome font (Inter Medium, Latin + UI symbols only).
pub fn ui_font() -> Option<FontArc> {
    FontArc::try_from_slice(INTER).ok()
}

/// First system font that parses; Inter (Latin only) when none is found.
pub fn load_system_font() -> Option<FontArc> {
    SYSTEM_CANDIDATES
        .iter()
        .find_map(|p| std::fs::read(p).ok().and_then(|b| FontArc::try_from_vec(b).ok()))
        .or_else(ui_font)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ab_glyph::Font;

    #[test]
    fn ui_font_has_ui_glyphs() {
        let f = ui_font().expect("embedded Inter parses");
        for ch in ['A', 'g', '0', '×', '·', '…', '—', '⇧', '⌘'] {
            assert_ne!(f.glyph_id(ch).0, 0, "missing {ch:?}");
        }
    }

    #[test]
    fn ui_font_is_small() {
        assert!(INTER.len() < 40 * 1024, "subset is {} bytes", INTER.len());
    }

    #[test]
    fn system_font_always_resolves() {
        assert!(load_system_font().is_some());
    }
}
