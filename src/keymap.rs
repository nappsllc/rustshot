//! Remappable editor shortcuts: the actions, their default chords, and the
//! `[shortcuts]` config table (`action = "Ctrl+Shift+S"`, several chords
//! separated by commas, `""` unbinds). Chords use the global-hotkey syntax
//! (`hotkey::parse_hotkey`). Arrow nudges/resizes stay built in.

use crate::hotkey;
use crate::wind::Mods;
use std::collections::BTreeMap;

/// Editor actions in table order (the first wins when two share a chord).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Action {
    ToolPencil,
    ToolLine,
    ToolArrow,
    ToolRectangle,
    ToolCircle,
    ToolMarker,
    ToolText,
    ToolPixelate,
    ToolInvert,
    Copy,
    Save,
    SaveAs,
    Upload,
    Undo,
    Redo,
    SelectAll,
    TogglePalette,
    Accept,
    Cancel,
}

impl Action {
    pub const ALL: [Action; 19] = [
        Action::ToolPencil,
        Action::ToolLine,
        Action::ToolArrow,
        Action::ToolRectangle,
        Action::ToolCircle,
        Action::ToolMarker,
        Action::ToolText,
        Action::ToolPixelate,
        Action::ToolInvert,
        Action::Copy,
        Action::Save,
        Action::SaveAs,
        Action::Upload,
        Action::Undo,
        Action::Redo,
        Action::SelectAll,
        Action::TogglePalette,
        Action::Accept,
        Action::Cancel,
    ];

    /// Config key in the `[shortcuts]` table.
    pub fn id(self) -> &'static str {
        self.info().0
    }

    /// Description for the Settings shortcut table.
    pub fn label(self) -> &'static str {
        self.info().1
    }

    fn defaults(self) -> &'static [&'static str] {
        self.info().2
    }

    fn info(self) -> (&'static str, &'static str, &'static [&'static str]) {
        match self {
            Action::ToolPencil => ("tool_pencil", "Pencil", &["P"]),
            Action::ToolLine => ("tool_line", "Line", &["D"]),
            Action::ToolArrow => ("tool_arrow", "Arrow", &["A"]),
            Action::ToolRectangle => ("tool_rectangle", "Rectangle", &["R"]),
            Action::ToolCircle => ("tool_circle", "Circle", &["C"]),
            Action::ToolMarker => ("tool_marker", "Marker", &["M"]),
            Action::ToolText => ("tool_text", "Text", &["T"]),
            Action::ToolPixelate => ("tool_pixelate", "Pixelate", &["B"]),
            Action::ToolInvert => ("tool_invert", "Invert", &["I"]),
            Action::Copy => ("copy", "Copy", &["Ctrl+C"]),
            Action::Save => ("save", "Save", &["Ctrl+S"]),
            Action::SaveAs => ("save_as", "Save As", &["Ctrl+Shift+S"]),
            Action::Upload => ("upload", "Upload", &["Ctrl+U"]),
            Action::Undo => ("undo", "Undo", &["Ctrl+Z"]),
            Action::Redo => ("redo", "Redo", &["Ctrl+Shift+Z", "Ctrl+Y"]),
            Action::SelectAll => ("select_all", "Select whole screen", &["Ctrl+A"]),
            Action::TogglePalette => ("toggle_palette", "Toggle palette", &["Space"]),
            Action::Accept => ("accept", "Accept (default action)", &["Enter"]),
            Action::Cancel => ("cancel", "Cancel", &["Esc"]),
        }
    }

    pub fn from_id(id: &str) -> Option<Action> {
        Action::ALL.into_iter().find(|a| a.id() == id)
    }

    fn index(self) -> usize {
        self as usize
    }
}

/// What the Meta modifier is called here: the Windows key, Super on
/// Linux, ⌘ (Cmd) on macOS.
#[cfg(windows)]
pub const META_NAME: &str = "Win";
#[cfg(target_os = "macos")]
pub const META_NAME: &str = "Cmd";
#[cfg(not(any(windows, target_os = "macos")))]
pub const META_NAME: &str = "Super";

/// One key plus modifiers. `vk` uses the Win32 numbering of `wind::key`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, PartialOrd, Ord)]
pub struct Chord {
    pub vk: u32,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub meta: bool,
}

impl Chord {
    /// `"Ctrl+Shift+S"`, `"Del"`, `"F5"`, `"Meta+X"` (case-insensitive).
    pub fn parse(s: &str) -> Option<Chord> {
        // Win32 modifier flags from parse_hotkey: ALT=1, CONTROL=2, SHIFT=4, WIN=8.
        let (m, vk) = hotkey::parse_hotkey(s)?;
        hotkey::key_name(vk)?; // only keys we can also display
        Some(Chord { vk, ctrl: m & 2 != 0, shift: m & 4 != 0, alt: m & 1 != 0, meta: m & 8 != 0 })
    }

    /// Canonical text form, e.g. `"Ctrl+Shift+S"`.
    pub fn display(&self) -> String {
        let mut out = String::new();
        for (on, name) in [(self.ctrl, "Ctrl"), (self.alt, "Alt"), (self.shift, "Shift"), (self.meta, "Meta")] {
            if on {
                out.push_str(name);
                out.push('+');
            }
        }
        out.push_str(&hotkey::key_name(self.vk).unwrap_or_else(|| format!("0x{:X}", self.vk)));
        out
    }

    /// [`display`](Chord::display) with this OS's name for Meta
    /// ([`META_NAME`]), for messages: `"Shift+Win+X"`.
    pub fn label(&self) -> String {
        let s = self.display();
        if self.meta { s.replacen("Meta", META_NAME, 1) } else { s }
    }

    /// The key name alone ("S", "Space"), for key caps.
    pub fn key(&self) -> String {
        hotkey::key_name(self.vk).unwrap_or_default()
    }

    fn matches(&self, vk: u32, mods: Mods) -> bool {
        // `Mods` carries no meta state (macOS folds ⌘ into ctrl), so a
        // Meta chord never fires inside the editor.
        self.vk == vk && self.ctrl == mods.ctrl && self.shift == mods.shift && self.alt == mods.alt && !self.meta
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Keymap {
    chords: Vec<Vec<Chord>>,
}

fn parse_list(v: &str) -> Vec<Option<Chord>> {
    v.split(',').map(str::trim).filter(|p| !p.is_empty()).map(Chord::parse).collect()
}

impl Keymap {
    pub fn defaults() -> Keymap {
        let chords = Action::ALL
            .iter()
            .map(|a| a.defaults().iter().map(|s| Chord::parse(s).expect("default chord")).collect())
            .collect();
        Keymap { chords }
    }

    /// Defaults with the `[shortcuts]` overrides applied, plus warnings for
    /// unknown actions and unparsable chords (those entries are ignored).
    pub fn from_config(m: &BTreeMap<String, String>) -> (Keymap, Vec<String>) {
        let mut km = Keymap::defaults();
        let mut warn = Vec::new();
        for (k, v) in m {
            let Some(a) = Action::from_id(k.trim()) else {
                warn.push(format!("[shortcuts] unknown action '{k}' (ignored)"));
                continue;
            };
            let parsed = parse_list(v);
            let good: Vec<Chord> = parsed.iter().flatten().copied().collect();
            if good.len() < parsed.len() {
                warn.push(format!("[shortcuts] {k}: invalid key in {v:?}"));
                if good.is_empty() {
                    continue; // keep the default
                }
            }
            if good.iter().any(|c| c.meta) {
                warn.push(format!("[shortcuts] {k}: Meta chords do not reach the editor"));
            }
            km.chords[a.index()] = good;
        }
        (km, warn)
    }

    /// The action bound to `vk` with exactly these modifiers.
    pub fn resolve(&self, vk: u32, mods: Mods) -> Option<Action> {
        Action::ALL.into_iter().find(|a| self.chords(*a).iter().any(|c| c.matches(vk, mods)))
    }

    pub fn chords(&self, a: Action) -> &[Chord] {
        &self.chords[a.index()]
    }

    /// Chords bound to more than one action (actions in table order).
    pub fn conflicts(&self) -> Vec<(Chord, Vec<Action>)> {
        let mut by: BTreeMap<Chord, Vec<Action>> = BTreeMap::new();
        for a in Action::ALL {
            for c in self.chords(a) {
                let v = by.entry(*c).or_default();
                if !v.contains(&a) {
                    v.push(a);
                }
            }
        }
        by.into_iter().filter(|(_, v)| v.len() > 1).collect()
    }

    /// Bind `a` to exactly `chords` (empty = unbound).
    pub fn set(&mut self, a: Action, chords: Vec<Chord>) {
        self.chords[a.index()] = chords;
    }

    /// The `[shortcuts]` entries that differ from the defaults.
    pub fn to_config(&self) -> BTreeMap<String, String> {
        let def = Keymap::defaults();
        Action::ALL
            .into_iter()
            .filter(|a| self.chords(*a) != def.chords(*a))
            .map(|a| {
                let v: Vec<String> = self.chords(a).iter().map(Chord::display).collect();
                (a.id().to_string(), v.join(", "))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wind::key;

    fn m(ctrl: bool, shift: bool, alt: bool) -> Mods {
        Mods { ctrl, shift, alt }
    }
    const NONE: Mods = Mods { ctrl: false, shift: false, alt: false };

    fn shown(km: &Keymap, a: Action) -> Vec<String> {
        km.chords(a).iter().map(Chord::display).collect()
    }

    #[test]
    fn defaults_match_spec_table() {
        let km = Keymap::defaults();
        let want: [(Action, &[&str]); 19] = [
            (Action::ToolPencil, &["P"]),
            (Action::ToolLine, &["D"]),
            (Action::ToolArrow, &["A"]),
            (Action::ToolRectangle, &["R"]),
            (Action::ToolCircle, &["C"]),
            (Action::ToolMarker, &["M"]),
            (Action::ToolText, &["T"]),
            (Action::ToolPixelate, &["B"]),
            (Action::ToolInvert, &["I"]),
            (Action::Copy, &["Ctrl+C"]),
            (Action::Save, &["Ctrl+S"]),
            (Action::SaveAs, &["Ctrl+Shift+S"]),
            (Action::Upload, &["Ctrl+U"]),
            (Action::Undo, &["Ctrl+Z"]),
            (Action::Redo, &["Ctrl+Shift+Z", "Ctrl+Y"]),
            (Action::SelectAll, &["Ctrl+A"]),
            (Action::TogglePalette, &["Space"]),
            (Action::Accept, &["Enter"]),
            (Action::Cancel, &["Esc"]),
        ];
        for (a, keys) in want {
            assert_eq!(shown(&km, a), keys, "{a:?}");
        }
        assert!(km.conflicts().is_empty());
        assert!(km.to_config().is_empty());
    }

    #[test]
    fn ids_are_unique_and_round_trip() {
        for a in Action::ALL {
            assert_eq!(Action::from_id(a.id()), Some(a));
            assert!(!a.label().is_empty());
        }
        assert_eq!(Action::SaveAs.id(), "save_as");
        assert_eq!(Action::ToolPencil.id(), "tool_pencil");
    }

    #[test]
    fn parse_display_round_trip() {
        for s in ["Ctrl+Shift+S", "Meta+X", "Del", "Space", "Q", "F5", "F12", "Ctrl+Alt+Shift+Meta+7", "Esc", "Enter"] {
            let c = Chord::parse(s).unwrap_or_else(|| panic!("{s}"));
            assert_eq!(c.display(), s);
            assert_eq!(Chord::parse(&c.display()), Some(c));
        }
        assert_eq!(Chord::parse("shift+ctrl+s").unwrap().display(), "Ctrl+Shift+S");
        assert_eq!(Chord::parse("Win+delete").unwrap().display(), "Meta+Del");
        assert_eq!(Chord::parse("Meta+Shift+X").unwrap().label(), format!("Shift+{META_NAME}+X"));
        assert_eq!(Chord::parse("Ctrl+S").unwrap().label(), "Ctrl+S");
        // The default capture hotkey is written in the display order.
        let def = crate::config::Config::default().capture_hotkey;
        assert_eq!(Chord::parse(&def).unwrap().display(), def);
        let c = Chord::parse("Meta+X").unwrap();
        assert!(c.meta && !c.ctrl && c.vk == 'X' as u32);
        assert_eq!(Chord::parse("Del").unwrap().vk, key::DELETE);
        for bad in ["", "Ctrl", "Ctrl+", "Foo", "A+B", "F13"] {
            assert_eq!(Chord::parse(bad), None, "{bad}");
        }
    }

    fn cfg(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn override_replaces_default() {
        let (km, w) = Keymap::from_config(&cfg(&[("tool_pencil", "Shift+K"), ("redo", "Ctrl+R, F4")]));
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(shown(&km, Action::ToolPencil), ["Shift+K"]);
        assert_eq!(shown(&km, Action::Redo), ["Ctrl+R", "F4"]);
        assert_eq!(km.resolve('P' as u32, NONE), None);
        assert_eq!(km.resolve('K' as u32, m(false, true, false)), Some(Action::ToolPencil));
        let out = km.to_config();
        assert_eq!(out.len(), 2);
        assert_eq!(out["redo"], "Ctrl+R, F4");
        assert_eq!(Keymap::from_config(&out).0, km);
    }

    #[test]
    fn empty_string_unbinds() {
        let (km, w) = Keymap::from_config(&cfg(&[("save", "")]));
        assert!(w.is_empty());
        assert!(km.chords(Action::Save).is_empty());
        assert_eq!(km.resolve('S' as u32, m(true, false, false)), None);
        assert_eq!(km.to_config()["save"], "");
    }

    #[test]
    fn unknown_action_and_bad_chord_warn() {
        let (km, w) = Keymap::from_config(&cfg(&[("tool_counter", "N"), ("copy", "Ctrl+Nope")]));
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(w.iter().any(|s| s.contains("tool_counter")));
        assert_eq!(shown(&km, Action::Copy), ["Ctrl+C"], "bad value keeps the default");
    }

    #[test]
    fn conflicts_are_reported_and_first_in_table_wins() {
        let (km, _) = Keymap::from_config(&cfg(&[("cancel", "P"), ("undo", "Ctrl+Y")]));
        let c = km.conflicts();
        assert_eq!(c.len(), 2, "{c:?}");
        let p = Chord::parse("P").unwrap();
        assert!(c.contains(&(p, vec![Action::ToolPencil, Action::Cancel])));
        let y = Chord::parse("Ctrl+Y").unwrap();
        assert!(c.contains(&(y, vec![Action::Undo, Action::Redo])));
        assert_eq!(km.resolve('P' as u32, NONE), Some(Action::ToolPencil));
        assert_eq!(km.resolve('Y' as u32, m(true, false, false)), Some(Action::Undo));
    }

    #[test]
    fn resolve_matches_modifiers_exactly() {
        let km = Keymap::defaults();
        assert_eq!(km.resolve('P' as u32, NONE), Some(Action::ToolPencil));
        assert_eq!(km.resolve('P' as u32, m(false, true, false)), None);
        assert_eq!(km.resolve('P' as u32, m(true, false, false)), None);
        assert_eq!(km.resolve('P' as u32, m(false, false, true)), None);
        assert_eq!(km.resolve('S' as u32, m(true, false, false)), Some(Action::Save));
        assert_eq!(km.resolve('S' as u32, m(true, true, false)), Some(Action::SaveAs));
        assert_eq!(km.resolve('S' as u32, m(true, true, true)), None);
        assert_eq!(km.resolve('Z' as u32, m(true, false, false)), Some(Action::Undo));
        assert_eq!(km.resolve('Z' as u32, m(true, true, false)), Some(Action::Redo));
        assert_eq!(km.resolve('A' as u32, m(true, false, false)), Some(Action::SelectAll));
        assert_eq!(km.resolve('A' as u32, NONE), Some(Action::ToolArrow));
        assert_eq!(km.resolve(key::SPACE, NONE), Some(Action::TogglePalette));
        assert_eq!(km.resolve(key::RETURN, NONE), Some(Action::Accept));
        assert_eq!(km.resolve(key::ESCAPE, NONE), Some(Action::Cancel));
        assert_eq!(km.resolve(key::DELETE, NONE), None);
        // Meta chords never fire (no meta state in `Mods`).
        let (km, _) = Keymap::from_config(&cfg(&[("copy", "Meta+C")]));
        assert_eq!(km.resolve('C' as u32, m(true, false, false)), None);
    }
}
