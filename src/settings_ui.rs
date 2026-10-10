//! The Settings window: **General** (theme, renderer, start at login,
//! update check, global hotkeys), **Saving** (folder, daily subfolders,
//! format and JPEG quality, the Save As toggle and a file-name pattern
//! editor with a live preview) and **Shortcuts** (the editor keymap), with
//! **OK** / **Cancel** / **Apply**. Opened from the tray ("Settings…") and
//! by `rustshot settings` ([`show`], [`run_here`]).
//!
//! The form ([`Form`]) is a plain copy of the settings it edits; saving
//! writes only the values the user changed onto the file as it is on disk
//! then (so a `skip_version` written meanwhile survives) and rewrites the
//! whole `config.toml` ([`config::save_at`]). A file with comments or
//! unknown keys would lose them: the first save asks first ([`REWRITE`]).
//! A file that does not parse is never replaced by the defaults behind the
//! user's back: the window shows the error ([`broken_text`]) and offers to
//! open the file or, after a second question ([`RESET`]), to reset it
//! (the old file is kept as `config.toml.bak`).
//! After a save the daemon reloads the config (re-registers its hotkeys,
//! applies `check_updates`); every later capture uses it.
//!
//! Drawn with the `ui` kit in a `wind::run_window` window on its own
//! thread, one window at a time: another request brings it to the front.
//! A hotkey the daemon could not register after a save is reported in the
//! open window ([`notify`]). macOS opens windows only on the main thread,
//! so there the config file opens in the default editor instead.
#![cfg_attr(target_os = "macos", allow(dead_code))]

use crate::config::{self, Config};
use crate::export;
use crate::hotkey::HotEvent;
use crate::keymap::{Action, Chord, Keymap};
use crate::objects::FRect;
use crate::pixbuf::PixBuf;
use crate::theme::Theme;
use crate::ui::layout::{GAP, H as ROW};
use crate::ui::{Col, FocusState, Input, Row, TableState, TextState, Ui};
use crate::wind::{self, Cursor, Driver, Ev, Hwnd, WindowSpec};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender};

/// Window size, logical px.
pub const W: u32 = 640;
pub const H: u32 = 540;
const PAD: f32 = 20.0;
/// Width of the label column of a form row.
const LABEL_W: f32 = 168.0;
/// Top of the tab bar and of the tab body.
const TABS_Y: f32 = 16.0;
const BODY_Y: f32 = 72.0;

pub const TABS: [&str; 3] = ["General", "Saving", "Shortcuts"];
/// (label, config value)
const THEMES: [(&str, &str); 3] = [("Auto", "auto"), ("Dark", "dark"), ("Light", "light")];
const RENDERERS: [(&str, &str); 2] = [("GDI", "gdi"), ("Software", "software")];
const FORMATS: [(&str, &str); 3] = [("PNG", "png"), ("JPEG", "jpg"), ("BMP", "bmp")];
const DEFAULT_FILENAME: &str = "%F_%H-%M";

/// The file-name editor's token buttons: (label, inserted text), shown in
/// two columns (top to bottom, then the next column).
pub const TOKENS: [(&str, &str); 15] = [
    ("Century (00-99)", "%C"),
    ("Day (001-366)", "%j"),
    ("Day (01-31)", "%d"),
    ("Day of Month (1-31)", "%e"),
    ("Full Date (%Y-%m-%d)", "%F"),
    ("Full Date (%d-%m-%Y)", "%d-%m-%Y"),
    ("Hour (00-23)", "%H"),
    ("Hour (01-12)", "%I"),
    ("Minute (00-59)", "%M"),
    ("Month (01-12)", "%m"),
    ("Second (00-59)", "%S"),
    ("Week (01-53)", "%V"),
    ("Week Day (1-7)", "%u"),
    ("Year (00-99)", "%y"),
    ("Year (2000)", "%Y"),
];
/// Token buttons per column (3 columns of 5).
const TOKEN_ROWS: usize = 5;
/// Token button height and the gap between them (logical px).
const TOKEN_H: f32 = 28.0;
const TOKEN_GAP: f32 = 6.0;

/// Asked once before the first save over a file rustshot would not write
/// that way (comments, unknown keys).
pub const REWRITE: &str = "Saving will rewrite config.toml and remove comments.";

/// The question before "Reset to defaults…" replaces a broken file.
pub const RESET: &str = "Replace config.toml with default settings? Your current values will be lost.";

/// The error state's text for a `config.toml` that does not parse (`e`
/// as [`config::read_at`] words it: "line 3: expected key = value").
pub fn broken_text(e: &str) -> String {
    format!("config.toml has an error ({e}). Fix it or reset it.")
}

/// Where "Reset to defaults…" keeps the old file: `config.toml.bak`.
pub fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_else(|| "config.toml".into());
    name.push(".bak");
    path.with_file_name(name)
}

fn labels<const N: usize>(list: &[(&'static str, &'static str); N]) -> [&'static str; N] {
    list.map(|(l, _)| l)
}

fn theme_index(v: &str) -> usize {
    THEMES.iter().position(|(_, t)| v.trim().eq_ignore_ascii_case(t)).unwrap_or(0)
}

fn format_index(cfg: &Config) -> usize {
    match export::Format::from_config(cfg) {
        export::Format::Png => 0,
        export::Format::Jpeg => 1,
        export::Format::Bmp => 2,
    }
}

/// The folder field's text: `save_path`, or the folder an empty one means.
fn folder_text(cfg: &Config) -> String {
    if cfg.save_path.trim().is_empty() {
        export::default_save_dir(cfg).display().to_string()
    } else {
        cfg.save_path.clone()
    }
}

fn chord_text(c: Option<Chord>) -> String {
    c.map(|c| c.display()).unwrap_or_default()
}

/// The values the window edits.
#[derive(Clone, Debug)]
pub struct Form {
    pub theme: usize,
    pub renderer: usize,
    /// Start at login; `None` hides it (store installs).
    pub autostart: Option<bool>,
    pub check_updates: bool,
    pub capture: Option<Chord>,
    pub quit: Option<Chord>,
    pub folder: TextState,
    pub subfolder: bool,
    pub subfolder_pattern: TextState,
    pub format: usize,
    pub quality: u8,
    pub ask: bool,
    pub filename: TextState,
    pub keys: Keymap,
}

impl Form {
    pub fn new(cfg: &Config, autostart: Option<bool>) -> Form {
        Form {
            theme: theme_index(&cfg.theme),
            renderer: !cfg.use_gdi() as usize,
            autostart,
            check_updates: cfg.check_updates,
            capture: Chord::parse(&cfg.capture_hotkey),
            quit: Chord::parse(&cfg.quit_hotkey),
            folder: TextState::new(&folder_text(cfg)),
            subfolder: cfg.save_subfolder,
            subfolder_pattern: TextState::new(&cfg.subfolder_pattern),
            format: format_index(cfg),
            quality: cfg.jpeg_quality.clamp(1, 100),
            ask: cfg.save_dialog,
            filename: TextState::new(&cfg.filename_pattern),
            keys: Keymap::from_config(&cfg.shortcuts).0,
        }
    }

    /// `onto` with the values the user changed from `loaded` (the config
    /// the form was made from). A value left as it was keeps `onto`'s, so
    /// spellings the form cannot show (an unknown theme, a hotkey it
    /// cannot parse) and edits made to the file meanwhile survive.
    pub fn apply(&self, loaded: &Config, onto: &Config) -> Config {
        let was = Form::new(loaded, None);
        let mut c = onto.clone();
        if self.theme != was.theme {
            c.theme = THEMES[self.theme].1.into();
        }
        if self.renderer != was.renderer {
            c.renderer = RENDERERS[self.renderer].1.into();
        }
        if self.check_updates != was.check_updates {
            c.check_updates = self.check_updates;
        }
        if self.capture != was.capture {
            c.capture_hotkey = chord_text(self.capture);
        }
        if self.quit != was.quit {
            c.quit_hotkey = chord_text(self.quit);
        }
        if self.folder.text.trim() != was.folder.text.trim() {
            c.save_path = self.folder.text.trim().to_string();
        }
        if self.subfolder != was.subfolder {
            c.save_subfolder = self.subfolder;
        }
        if self.subfolder_pattern.text != was.subfolder_pattern.text {
            c.subfolder_pattern = self.subfolder_pattern.text.clone();
        }
        if self.format != was.format {
            c.save_format = FORMATS[self.format].1.into();
        }
        if self.quality != was.quality {
            c.jpeg_quality = self.quality;
        }
        if self.ask != was.ask {
            c.save_dialog = self.ask;
        }
        if self.filename.text != was.filename.text {
            c.filename_pattern = self.filename.text.clone();
        }
        let changed: Vec<Action> = Action::ALL.into_iter().filter(|a| self.keys.chords(*a) != was.keys.chords(*a)).collect();
        if !changed.is_empty() {
            let mut km = Keymap::from_config(&onto.shortcuts).0;
            for a in changed {
                km.set(a, self.keys.chords(a).to_vec());
            }
            let mut sc = km.to_config();
            // Entries for actions this build does not know stay as they are.
            for (k, v) in &onto.shortcuts {
                if Action::from_id(k.trim()).is_none() {
                    sc.insert(k.clone(), v.clone());
                }
            }
            c.shortcuts = sc;
        }
        c
    }

    /// Insert a token at the caret of `target` (replacing its selection).
    pub fn insert(&mut self, target: Pattern, token: &str) {
        match target {
            Pattern::File => self.filename.insert(token),
            Pattern::Sub => self.subfolder_pattern.insert(token),
        }
    }

    /// Where Ctrl+S would save now, with the form's values over `base`.
    pub fn preview(&self, base: &Config, now: export::Tm) -> PathBuf {
        export::auto_save_path(&self.apply(base, base), now)
    }

    /// The shortcut table: (description, keys, in a conflict).
    pub fn shortcut_rows(&self) -> Vec<(&'static str, String, bool)> {
        let bad: Vec<Action> = self.keys.conflicts().into_iter().flat_map(|(_, v)| v).collect();
        (Action::ALL.into_iter())
            .map(|a| {
                let keys: Vec<String> = self.keys.chords(a).iter().map(Chord::display).collect();
                let keys = if keys.is_empty() { "None".to_string() } else { keys.join(", ") };
                (a.label(), keys, bad.contains(&a))
            })
            .collect()
    }

    /// The first conflict as a sentence, plus how many more there are.
    pub fn conflict_note(&self) -> Option<String> {
        let all = self.keys.conflicts();
        let (c, acts) = all.first()?;
        let names: Vec<&str> = acts.iter().map(|a| a.label()).collect();
        let list = match names.split_last() {
            Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
            _ => names.join(""),
        };
        let mut s = format!("{} is bound to {list}; {} wins.", c.display(), names[0]);
        match all.len() - 1 {
            0 => {}
            1 => s.push_str(" 1 more conflict."),
            n => s.push_str(&format!(" {n} more conflicts.")),
        }
        Some(s)
    }

    /// A problem with the global hotkeys, if any.
    pub fn hotkey_note(&self) -> Option<&'static str> {
        let bare = |c: &Option<Chord>| {
            c.is_some_and(|c| {
                let modded = c.ctrl || c.alt || c.shift || c.meta;
                let special = (0x70..=0x7B).contains(&c.vk) || c.vk == wind::key::PRINTSCREEN; // F1-F12
                !(modded || special)
            })
        };
        if self.capture.is_some() && self.capture == self.quit {
            Some("Capture and Quit use the same keys.")
        } else if bare(&self.capture) || bare(&self.quit) {
            Some(BARE_NOTE)
        } else {
            None
        }
    }

    /// A word about the folder that does not stop the save: a relative
    /// path, or a drive (root folder) this computer does not have.
    pub fn folder_note(&self) -> Option<String> {
        folder_note(self.folder.text.trim())
    }
}

#[cfg(windows)]
const BARE_NOTE: &str = "A hotkey without Ctrl, Alt, Shift or Win takes that key from every app.";
#[cfg(target_os = "macos")]
const BARE_NOTE: &str = "A hotkey without Ctrl, Alt, Shift or Cmd takes that key from every app.";
#[cfg(not(any(windows, target_os = "macos")))]
const BARE_NOTE: &str = "A hotkey without Ctrl, Alt, Shift or Super takes that key from every app.";

/// See [`Form::folder_note`].
fn folder_note(t: &str) -> Option<String> {
    use std::path::Component;
    if t.is_empty() {
        return None;
    }
    let p = Path::new(t);
    if !p.has_root() || p.is_relative() {
        return Some("A relative folder is saved under the folder Rustshot was started in.".into());
    }
    // The drive (Windows) or the top-level folder (elsewhere: /media, /mnt).
    let mut root = PathBuf::new();
    for c in p.components() {
        if let Component::Prefix(x) = c
            && matches!(x.kind(), std::path::Prefix::UNC(..) | std::path::Prefix::VerbatimUNC(..))
        {
            return None; // a network share: not probed (it can take seconds)
        }
        root.push(c);
        if cfg!(windows) && matches!(c, Component::RootDir) || matches!(c, Component::Normal(_)) {
            break;
        }
    }
    (!root.exists()).then(|| {
        let name = root.display().to_string();
        if cfg!(windows) { format!("Drive {} is not on this computer.", name.trim_end_matches('\\')) } else { format!("{name} does not exist.") }
    })
}

/// The pattern field token buttons insert into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pattern {
    File,
    Sub,
}

/// A question over the form.
#[derive(Clone, Debug, PartialEq)]
enum Modal {
    /// [`REWRITE`]; `close`: OK (not Apply) asked to save.
    Rewrite { close: bool },
    /// A save failed, or a message for the user (a hotkey the daemon
    /// could not register).
    Error(String),
    /// `config.toml` does not parse (the error): Open config file, Reset
    /// to defaults…, Close; nothing else until it is fixed.
    Broken(String),
    /// [`RESET`] over [`Modal::Broken`] (its error, to go back to).
    Reset(String),
}

/// What the user did this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Click {
    Ok,
    Cancel,
    Apply,
    /// The rewrite question's Save.
    Save,
    /// The rewrite question's Cancel, or the error's OK.
    Back,
    Browse,
    OpenConfig,
    Restore,
    Clear,
    ResetAll,
    /// The broken file's "Reset to defaults…".
    ResetDefaults,
    /// The reset question's Replace.
    Replace,
    /// The broken file's Close.
    Close,
    /// Esc not taken by a control.
    Escape,
    /// Enter not taken by a control.
    Enter,
}

/// What the window must do.
#[derive(Clone, Debug, PartialEq)]
pub enum Act {
    Close,
    /// The config was written: the daemon reloads it.
    Saved(Box<Config>),
    /// Show the folder picker.
    Browse,
    OpenConfig,
}

/// The window's state: the form, what it was loaded from, the tab and the
/// open question.
pub struct Settings {
    /// The config as last loaded or saved.
    loaded: Config,
    /// Start at login as it is now (`None`: hidden).
    autostart: Option<bool>,
    pub form: Form,
    pub tab: usize,
    table: TableState,
    modal: Option<Modal>,
    /// The rewrite question was answered (or the file is ours now).
    confirmed: bool,
    /// Token buttons insert here.
    pattern: Pattern,
    /// The config file.
    path: PathBuf,
    /// Show the renderer choice (Windows).
    renderer: bool,
    /// The form was not made from the file (it did not parse at open):
    /// rebuilt from it once it does.
    stale: bool,
    /// Messages waiting for the open question to close.
    pending: Vec<String>,
    /// The first conflicting shortcut row last revealed.
    conflict_row: Option<usize>,
    /// The folder text and its note (probing a drive every frame is slow).
    folder_note: (String, Option<String>),
    set_autostart: fn(bool) -> Result<(), String>,
}

impl Settings {
    pub fn new(cfg: Config, autostart: Option<bool>, path: PathBuf) -> Settings {
        Settings {
            form: Form::new(&cfg, autostart),
            loaded: cfg,
            autostart,
            tab: 0,
            table: TableState::default(),
            modal: None,
            confirmed: false,
            pattern: Pattern::File,
            path,
            renderer: cfg!(windows),
            stale: false,
            pending: Vec::new(),
            conflict_row: None,
            folder_note: (String::new(), None),
            set_autostart: crate::autostart::set,
        }
    }

    /// The window for the config file at `path` as it is now. A file that
    /// does not parse opens in the error state ([`Modal::Broken`]).
    pub fn open(path: PathBuf, autostart: Option<bool>) -> Settings {
        match config::read_at(&path) {
            Ok(cfg) => Settings::new(cfg.unwrap_or_default(), autostart, path),
            Err(e) => {
                let mut s = Settings::new(Config::default(), autostart, path);
                s.stale = true;
                s.modal = Some(Modal::Broken(e));
                s
            }
        }
    }

    /// Showing the broken-file error (or its reset question).
    pub fn broken(&self) -> bool {
        matches!(self.modal, Some(Modal::Broken(_) | Modal::Reset(_)))
    }

    /// While the broken-file error shows: read the file again; once it
    /// parses the window works again (a form made before it broke is
    /// kept, one that never saw it is rebuilt). Returns whether anything
    /// changed.
    pub fn recheck(&mut self) -> bool {
        let Some(Modal::Broken(old)) = &self.modal else { return false };
        match config::read_at(&self.path) {
            Ok(cfg) => {
                if self.stale {
                    let cfg = cfg.unwrap_or_default();
                    self.form = Form::new(&cfg, self.form.autostart);
                    self.loaded = cfg;
                    self.stale = false;
                }
                self.modal = None;
                self.show_pending();
                true
            }
            Err(e) if e != *old => {
                self.modal = Some(Modal::Broken(e));
                true
            }
            Err(_) => false,
        }
    }

    /// Show `msg` (e.g. a hotkey the daemon could not register) once no
    /// other question is open.
    pub fn notify(&mut self, msg: String) {
        match &mut self.modal {
            None => self.modal = Some(Modal::Error(msg)),
            Some(Modal::Error(e)) => {
                e.push('\n');
                e.push_str(&msg);
            }
            Some(_) => self.pending.push(msg),
        }
    }

    fn show_pending(&mut self) {
        if self.modal.is_none() && !self.pending.is_empty() {
            self.modal = Some(Modal::Error(std::mem::take(&mut self.pending).join("\n")));
        }
    }

    /// The file on disk now, if it parses: `Ok(None)` when there is none.
    /// When it does not, the window switches to the error state.
    fn disk(&mut self) -> Result<Option<Config>, ()> {
        config::read_at(&self.path).map_err(|e| {
            self.modal = Some(Modal::Broken(e));
        })
    }

    /// Something differs from what is saved.
    pub fn dirty(&self) -> bool {
        let now = self.form.apply(&self.loaded, &self.loaded);
        config::to_toml(&now) != config::to_toml(&self.loaded) || self.form.autostart != self.autostart
    }

    /// The file has content a rewrite would drop and nobody said yes yet.
    fn needs_confirm(&self) -> bool {
        !self.confirmed && std::fs::read_to_string(&self.path).is_ok_and(|t| config::rewrite_loses(&t))
    }

    pub fn on(&mut self, c: Click) -> Vec<Act> {
        let c = match (c, &self.modal) {
            (Click::Escape, Some(Modal::Broken(_))) => Click::Close,
            (Click::Escape, Some(_)) => Click::Back,
            (Click::Escape, None) => Click::Cancel,
            (Click::Enter, Some(Modal::Rewrite { .. })) => Click::Save,
            (Click::Enter, Some(Modal::Error(_))) => Click::Back,
            // Nothing happens to a broken file by a stray Enter.
            (Click::Enter, Some(Modal::Broken(_) | Modal::Reset(_))) => return vec![],
            (Click::Enter, None) => Click::Ok,
            // While the file is broken only its own buttons work.
            (Click::OpenConfig | Click::ResetDefaults | Click::Close | Click::Cancel, Some(Modal::Broken(_))) => c,
            (Click::Replace | Click::Back, Some(Modal::Reset(_))) => c,
            (Click::Cancel, Some(Modal::Reset(_))) => Click::Close, // the window's close button
            (_, Some(Modal::Broken(_) | Modal::Reset(_))) => return vec![],
            (c, _) => c,
        };
        match c {
            Click::Ok if !self.dirty() => vec![Act::Close],
            Click::Ok => self.save(true),
            Click::Apply if self.dirty() => self.save(false),
            Click::Apply => vec![],
            Click::Cancel => vec![Act::Close],
            Click::Save => {
                let close = matches!(self.modal, Some(Modal::Rewrite { close: true }));
                self.confirmed = true;
                self.modal = None;
                self.commit(close)
            }
            Click::Back => {
                self.modal = match self.modal.take() {
                    Some(Modal::Reset(e)) => Some(Modal::Broken(e)),
                    _ => None,
                };
                self.show_pending();
                vec![]
            }
            Click::ResetDefaults => {
                if let Some(Modal::Broken(e)) = &self.modal {
                    self.modal = Some(Modal::Reset(e.clone()));
                }
                vec![]
            }
            Click::Replace => self.reset(),
            Click::Close => vec![Act::Close],
            Click::Browse => vec![Act::Browse],
            Click::OpenConfig => vec![Act::OpenConfig],
            Click::Restore => {
                self.form.filename.set(DEFAULT_FILENAME);
                vec![]
            }
            Click::Clear => {
                self.form.filename.set("");
                vec![]
            }
            Click::ResetAll => {
                self.form.keys = Keymap::defaults();
                vec![]
            }
            Click::Escape | Click::Enter => vec![],
        }
    }

    fn save(&mut self, close: bool) -> Vec<Act> {
        if self.disk().is_err() {
            return vec![]; // never merged onto the defaults
        }
        if self.needs_confirm() {
            self.modal = Some(Modal::Rewrite { close });
            return vec![];
        }
        self.commit(close)
    }

    /// Write the changed values onto the file as it is now.
    fn commit(&mut self, close: bool) -> Vec<Act> {
        // The file as it is now (it may have changed, or broken, since).
        let Ok(onto) = self.disk() else { return vec![] };
        let onto = onto.unwrap_or_else(|| self.loaded.clone());
        let cfg = self.form.apply(&self.loaded, &onto);
        if let Err(e) = config::save_at(&self.path, &cfg) {
            self.modal = Some(Modal::Error(format!("Could not save {}: {e}", self.path.display())));
            return vec![];
        }
        self.confirmed = true; // the file is ours now
        self.loaded = cfg.clone();
        // The form shows what was saved (merged with the file, trimmed).
        self.form = Form::new(&cfg, self.form.autostart);
        let mut acts = vec![Act::Saved(Box::new(cfg))];
        if let (Some(on), true) = (self.form.autostart, self.form.autostart != self.autostart) {
            match (self.set_autostart)(on) {
                Ok(()) => self.autostart = Some(on),
                Err(e) => {
                    self.modal = Some(Modal::Error(format!("Could not change Start at login: {e}")));
                    return acts;
                }
            }
        }
        if close {
            acts.push(Act::Close);
        }
        acts
    }

    /// The reset question's Replace: keep the broken file as
    /// `config.toml.bak`, then write the defaults.
    fn reset(&mut self) -> Vec<Act> {
        let Some(Modal::Reset(_)) = &self.modal else { return vec![] };
        let bak = backup_path(&self.path);
        if self.path.exists()
            && let Err(e) = std::fs::copy(&self.path, &bak)
        {
            self.modal = Some(Modal::Error(format!("Could not back up config.toml to {}: {e}", bak.display())));
            return vec![];
        }
        let cfg = Config::default();
        if let Err(e) = config::save_at(&self.path, &cfg) {
            self.modal = Some(Modal::Error(format!("Could not save {}: {e}", self.path.display())));
            return vec![];
        }
        self.modal = None;
        self.confirmed = true;
        self.stale = false;
        self.form = Form::new(&cfg, self.form.autostart);
        self.loaded = cfg.clone();
        self.show_pending();
        vec![Act::Saved(Box::new(cfg))]
    }
}

// --- Drawing ----------------------------------------------------------------

/// A form row: label column, then the controls `f` draws, which start at
/// `LABEL_W + GAP` whatever the tab's row gap.
fn field(ui: &mut Ui, label: &str, f: impl FnOnce(&mut Ui)) {
    ui.row(|ui| {
        let w = LABEL_W + GAP - ui.lay.gap;
        ui.width(w).label(label);
        f(ui);
    });
}

/// One frame of the window; returns what was clicked.
fn paint(ui: &mut Ui, s: &mut Settings) -> Option<Click> {
    let b = ui.bounds();
    let mut click = None;
    let modal = s.modal.clone();
    ui.enabled = modal.is_none();
    let inner = b.w - 2.0 * PAD;

    let tw = ui.tabs_width(&TABS);
    ui.area(FRect { x: PAD, y: TABS_Y, w: inner, h: 40.0 }, |ui| {
        ui.row(|ui| {
            ui.space(((inner - tw) / 2.0).floor());
            ui.tabs("tabs", &TABS, &mut s.tab);
        })
    });
    let btn_y = b.h - PAD - ROW;
    // A form that never saw the file (it did not parse) is not shown.
    if !s.stale {
        let body = FRect { x: PAD, y: BODY_Y, w: inner, h: btn_y - 16.0 - BODY_Y };
        match s.tab {
            0 => general(ui, s, body),
            1 => saving(ui, s, body, &mut click),
            _ => shortcuts(ui, s, body, &mut click),
        }

        // Bottom bar: the config-file link (General), then OK / Cancel / Apply.
        let bar = FRect { x: PAD, y: btn_y, w: inner, h: ROW };
        if s.tab == 0 {
            ui.area(bar, |ui| {
                ui.row(|ui| {
                    if ui.link("open_config", "Open config file") {
                        click = Some(Click::OpenConfig);
                    }
                })
            });
        }
        let btns = [("ok", "OK", Click::Ok), ("cancel", "Cancel", Click::Cancel), ("apply", "Apply", Click::Apply)];
        let total: f32 = btns.iter().map(|b| ui.button_width(b.1)).sum::<f32>() + GAP * (btns.len() as f32 - 1.0);
        let dirty = s.dirty();
        ui.area(bar, |ui| {
            ui.row(|ui| {
                ui.space(inner - total);
                let on = ui.enabled;
                for (id, label, c) in btns {
                    ui.enabled = on && (c != Click::Apply || dirty);
                    if ui.button(id, label, c == Click::Ok) {
                        click = Some(c);
                    }
                }
                ui.enabled = on;
            })
        });
    }

    if let Some(m) = modal {
        ui.enabled = true;
        if let Some(c) = question(ui, &m) {
            click = Some(c);
        }
    }
    click
}

/// The question, error or broken-file state, centred over the dimmed form.
fn question(ui: &mut Ui, m: &Modal) -> Option<Click> {
    let b = ui.bounds();
    type Btns = &'static [(&'static str, &'static str, Click)];
    let (icon, text, btns): (&str, String, Btns) = match m {
        Modal::Rewrite { .. } => {
            ("info", REWRITE.into(), &[("q_save", "Save", Click::Save), ("q_cancel", "Cancel", Click::Back)])
        }
        Modal::Error(e) => ("alert", e.clone(), &[("q_ok", "OK", Click::Back)]),
        Modal::Broken(e) => (
            "alert",
            broken_text(e),
            &[
                ("q_open", "Open config file", Click::OpenConfig),
                ("q_reset", "Reset to defaults…", Click::ResetDefaults),
                ("q_close", "Close", Click::Close),
            ],
        ),
        Modal::Reset(_) => ("alert", RESET.into(), &[("q_replace", "Replace", Click::Replace), ("q_cancel", "Cancel", Click::Back)]),
    };
    let (cp, icon_w) = (24.0f32, 24.0 + 12.0);
    let total: f32 = btns.iter().map(|b| ui.button_width(b.1)).sum::<f32>() + GAP * (btns.len() as f32 - 1.0);
    let cw = (total + 2.0 * cp).max(420.0);
    let text_w = cw - 2.0 * cp - icon_w;
    let th = ui.paragraph_height(&text, text_w).clamp(24.0, 160.0);
    let ch = cp + th + 24.0 + ROW + cp;
    let card = FRect { x: ((b.w - cw) / 2.0).round(), y: ((b.h - ch) / 2.0).round(), w: cw, h: ch };
    ui.modal_card(card);
    let col = if matches!(m, Modal::Rewrite { .. }) { ui.theme.accent_fg } else { ui.theme.error };
    ui.area(FRect { x: card.x + cp, y: card.y + cp, w: 24.0, h: 24.0 }, |ui| ui.icon(icon, 24.0, col));
    let tr = FRect { x: card.x + cp + icon_w, y: card.y + cp + if th <= 24.0 { 2.0 } else { 0.0 }, w: text_w, h: th };
    ui.area(tr, |ui| ui.height(th).paragraph(&text, false));
    let bar = FRect { x: card.x + cp, y: card.y1() - cp - ROW, w: cw - 2.0 * cp, h: ROW };
    let mut click = None;
    ui.area(bar, |ui| {
        ui.row(|ui| {
            ui.space(bar.w - total);
            for (i, &(id, label, c)) in btns.iter().enumerate() {
                if ui.button(id, label, i == 0) {
                    click = Some(c);
                }
            }
        })
    });
    click
}

fn general(ui: &mut Ui, s: &mut Settings, body: FRect) {
    let show_renderer = s.renderer;
    let f = &mut s.form;
    let note = f.hotkey_note();
    ui.area(body, |ui| {
        // Spread out over the tab (less so when the hotkey note needs a row).
        let roomy = note.is_none();
        ui.lay.gap = if roomy { 12.0 } else { 8.0 };
        ui.heading("Appearance");
        field(ui, "Theme", |ui| {
            ui.dropdown("theme", &labels(&THEMES), &mut f.theme);
        });
        if show_renderer {
            field(ui, "Overlay renderer", |ui| {
                ui.dropdown("renderer", &labels(&RENDERERS), &mut f.renderer);
                ui.note(if f.renderer == 0 { "Uses the least memory" } else { "Composes every frame in memory" });
            });
        }
        if roomy {
            ui.space(6.0);
        }
        ui.heading("Startup and updates");
        if let Some(on) = f.autostart.as_mut() {
            field(ui, "Start at login", |ui| {
                ui.toggle("autostart", on);
            });
        }
        field(ui, "Check for updates", |ui| {
            ui.toggle("check_updates", &mut f.check_updates);
            ui.note("Once a day, from GitHub");
        });
        if roomy {
            ui.space(6.0);
        }
        ui.heading("Global hotkeys");
        ui.allow_meta = true;
        field(ui, "Capture", |ui| {
            ui.key_capture("capture_hotkey", &mut f.capture);
        });
        field(ui, "Quit Rustshot", |ui| {
            ui.key_capture("quit_hotkey", &mut f.quit);
        });
        if let Some(n) = note {
            ui.row(|ui| {
                ui.space(LABEL_W + GAP);
                ui.error_note(n);
            });
        }
        ui.allow_meta = false;
    });
}

fn saving(ui: &mut Ui, s: &mut Settings, body: FRect, click: &mut Option<Click>) {
    let base = s.loaded.clone();
    if s.folder_note.0 != s.form.folder.text {
        s.folder_note = (s.form.folder.text.clone(), s.form.folder_note());
    }
    let folder_note = s.folder_note.1.clone();
    let f = &mut s.form;
    let mut target = s.pattern;
    let mut token = None;
    ui.area(body, |ui| {
        ui.lay.gap = 6.0;
        field(ui, "Folder", |ui| {
            let bw = ui.button_width("Browse…");
            let fw = body.w - (LABEL_W + GAP) - bw - ui.lay.gap;
            ui.width(fw).text_field("folder", &mut f.folder);
            if ui.button("browse", "Browse…", false) {
                *click = Some(Click::Browse);
            }
        });
        if let Some(n) = &folder_note {
            ui.row(|ui| {
                ui.space(LABEL_W + GAP);
                ui.height(18.0).note(n);
            });
        }
        field(ui, "Daily subfolders", |ui| {
            ui.toggle("subfolder", &mut f.subfolder);
            ui.space(4.0);
            let on = ui.enabled;
            ui.enabled = on && f.subfolder;
            ui.width(180.0).text_field("subfolder_pattern", &mut f.subfolder_pattern);
            ui.enabled = on;
        });
        field(ui, "Format", |ui| {
            ui.dropdown("format", &labels(&FORMATS), &mut f.format);
            ui.space(12.0);
            let on = ui.enabled;
            ui.enabled = on && f.format == 1;
            ui.label("Quality");
            ui.slider("quality", &mut f.quality, 1, 100);
            ui.enabled = on;
        });
        field(ui, "Ask where to save", |ui| {
            ui.toggle("ask", &mut f.ask);
            ui.note("Ctrl+S always shows the Save As dialog");
        });
        ui.heading("File name");
        let gy = ui.cursor_y();
        // Three balanced columns of compact buttons (top to bottom).
        let col_w = token_col_w(ui);
        for (i, (label, _)) in TOKENS.iter().enumerate() {
            let (c, r) = ((i / TOKEN_ROWS) as f32, (i % TOKEN_ROWS) as f32);
            let rect = FRect { x: body.x + c * (col_w + TOKEN_GAP), y: gy + r * (TOKEN_H + TOKEN_GAP), w: col_w, h: TOKEN_H };
            if ui.place(rect).button(&format!("token{i}"), label, false) {
                token = Some(i);
            }
        }
        let cols = TOKENS.len().div_ceil(TOKEN_ROWS) as f32;
        let rx = body.x + cols * col_w + (cols - 1.0) * TOKEN_GAP + 16.0;
        let right = FRect { x: rx, y: gy, w: body.x1() - rx, h: body.y1() - gy };
        ui.area(right, |ui| {
            ui.lay.gap = 6.0;
            ui.text_field("filename", &mut f.filename);
            ui.note("Saves as");
            let path = f.preview(&base, export::local_ymdhms()).display().to_string();
            let w = ui.bounds().w;
            let lines = path_lines(ui, &path, w);
            ui.paragraph(&lines, true);
            ui.row(|ui| {
                let bw = ((right.w - GAP) / 2.0).floor();
                if ui.width(bw).button("restore", "Restore", false) {
                    *click = Some(Click::Restore);
                }
                if ui.width(bw).button("clear", "Clear", false) {
                    *click = Some(Click::Clear);
                }
            });
        });
        if ui.focus.is_focused("subfolder_pattern") {
            target = Pattern::Sub;
        } else if ui.focus.is_focused("filename") {
            target = Pattern::File;
        }
    });
    if let Some(i) = token {
        if target == Pattern::Sub && !f.subfolder {
            target = Pattern::File;
        }
        f.insert(target, TOKENS[i].1);
        ui.focus.focus(if target == Pattern::Sub { "subfolder_pattern" } else { "filename" });
    }
    s.pattern = target;
}

/// Width of a token button column: the widest label plus a little room.
fn token_col_w(ui: &Ui) -> f32 {
    TOKENS.iter().map(|(l, _)| ui.text_width(l)).fold(0.0, f32::max).ceil() + 16.0
}

/// `path` broken into lines of at most `w` (logical px) after its
/// separators (a component wider than a line is left to `paragraph`).
fn path_lines(ui: &Ui, path: &str, w: f32) -> String {
    let mut lines = vec![String::new()];
    for piece in path.split_inclusive(['\\', '/']) {
        let cur = lines.last_mut().expect("one line");
        if !cur.is_empty() && ui.text_width(&format!("{cur}{piece}")) > w {
            lines.push(piece.to_string());
        } else {
            cur.push_str(piece);
        }
    }
    lines.join("\n")
}

fn shortcuts(ui: &mut Ui, s: &mut Settings, body: FRect, click: &mut Option<Click>) {
    let rows = s.form.shortcut_rows();
    let note = s.form.conflict_note();
    let cells: Vec<[&str; 2]> = rows.iter().map(|(l, k, _)| [*l, k.as_str()]).collect();
    let trows: Vec<Row> = rows.iter().zip(&cells).map(|((_, _, bad), c)| Row { cells: c, error: *bad }).collect();
    let cols = [Col { title: "Description", frac: 0.62 }, Col { title: "Key", frac: 0.38 }];
    let note_h = if note.is_some() { 20.0 + GAP } else { 0.0 };
    let table_h = body.h - ROW - GAP - note_h;
    // A new conflict: scroll its first row into view.
    let first_bad = rows.iter().position(|r| r.2);
    if first_bad != s.conflict_row {
        if let Some(i) = first_bad {
            s.table.reveal(i, table_h);
        }
        s.conflict_row = first_bad;
    }
    let mut record = false;
    ui.area(body, |ui| {
        ui.height(table_h).table("shortcuts", &cols, &trows, &mut s.table);
        // A press on a row: record that action's new keys right away.
        let row = s.table.selected.and_then(|i| s.table.row_rect(i));
        record = ui.enabled && (ui.input.pressed.zip(row)).is_some_and(|(p, r)| crate::ui::contains(r, p));
        let ry = ui.cursor_y();
        ui.row(|ui| match s.table.selected {
            Some(i) => {
                let a = Action::ALL[i];
                ui.width(LABEL_W + GAP - ui.lay.gap).label(a.label());
                let mut c = s.form.keys.chords(a).first().copied();
                if ui.key_capture("rebind", &mut c) {
                    s.form.keys.set(a, c.into_iter().collect());
                }
            }
            None => ui.note("Select an action, then press its new keys (Backspace clears)."),
        });
        let bw = ui.button_width("Reset all");
        if ui.place(FRect { x: body.x1() - bw, y: ry, w: bw, h: ROW }).button("reset_all", "Reset all", false) {
            *click = Some(Click::ResetAll);
        }
        if let Some(n) = &note {
            ui.error_note(n);
        }
    });
    if record {
        ui.focus.record("rebind");
    }
}

// --- Window -----------------------------------------------------------------

/// What the open window is told from other threads.
enum Msg {
    /// Come to the front (another request to open it).
    Raise,
    /// Show this to the user ([`notify`]).
    Note(String),
}

/// The open window's inbox, if any.
static OPEN: Mutex<Option<Sender<Msg>>> = Mutex::new(None);
/// The daemon's event sender: told to reload after a save.
static DAEMON: Mutex<Option<Sender<HotEvent>>> = Mutex::new(None);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Register the daemon's event sender (saves make it reload the config).
pub fn set_daemon(tx: Sender<HotEvent>) {
    *lock(&DAEMON) = Some(tx);
}

/// Show `text` in the open Settings window (over the form, with OK);
/// false when no window is open (the caller tells the user some other way).
pub fn notify(text: &str) -> bool {
    lock(&OPEN).as_ref().is_some_and(|tx| tx.send(Msg::Note(text.to_string())).is_ok())
}

/// Open the window on its own thread, or bring the open one to the front.
pub fn show() {
    #[cfg(target_os = "macos")]
    std::thread::spawn(crate::actions::open_config); // never block the caller
    #[cfg(not(target_os = "macos"))]
    if let Some(rx) = claim() {
        std::thread::spawn(move || run(rx));
    }
}

/// `rustshot settings` with no daemon running: the window on this thread
/// (returns when it closes).
pub fn run_here() {
    #[cfg(target_os = "macos")]
    crate::actions::open_config();
    #[cfg(not(target_os = "macos"))]
    if let Some(rx) = claim() {
        run(rx);
    }
}

/// Take the single-window slot; `None` (and the open window raised) when
/// a window is already open.
fn claim() -> Option<Receiver<Msg>> {
    let mut open = lock(&OPEN);
    if let Some(tx) = open.as_ref()
        && tx.send(Msg::Raise).is_ok()
    {
        return None;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    *open = Some(tx);
    Some(rx)
}

fn run(rx: Receiver<Msg>) {
    let autostart = crate::update::managed_install().is_none().then(crate::autostart::is_enabled);
    let s = Settings::open(config::config_path(), autostart);
    let th = crate::theme::resolve(&s.loaded);
    let mut w = Window::new(s, th, rx);
    let spec = WindowSpec { title: "Rustshot Settings".into(), w: W, h: H, resizable: false, min: (W, H) };
    if let Err(e) = wind::run_window(spec, &mut w) {
        eprintln!("rustshot: settings window: {e:#}");
    }
    let mut open = lock(&OPEN);
    let mut late = false;
    let mut notes = Vec::new();
    for m in w.rx.try_iter() {
        match m {
            Msg::Raise => late = true,
            Msg::Note(t) => notes.push(t),
        }
    }
    *open = None;
    drop(open);
    // Told while closing (OK saves, then closes): the tray says it instead.
    for t in notes {
        crate::tray::notify(&t);
    }
    if late {
        show(); // a request raced the close
    }
}

/// The `wind` driver: renders on every input event (the controls are
/// immediate-mode) and runs the [`Settings`] actions.
struct Window {
    hwnd: Hwnd,
    s: Settings,
    th: Theme,
    input: Input,
    focus: FocusState,
    canvas: PixBuf,
    out: PixBuf,
    /// Requests to come to the front, and messages.
    rx: Receiver<Msg>,
    /// When the broken file was last read again.
    rechecked: std::time::Instant,
    /// The folder picker's answer (it runs on its own thread).
    picked: (Sender<Option<PathBuf>>, Receiver<Option<PathBuf>>),
    picking: bool,
    closing: bool,
}

impl Window {
    fn new(s: Settings, th: Theme, rx: Receiver<Msg>) -> Window {
        Window {
            hwnd: Hwnd::default(),
            s,
            th,
            input: Input::default(),
            focus: FocusState::default(),
            canvas: PixBuf::default(),
            out: PixBuf::default(),
            rx,
            rechecked: std::time::Instant::now(),
            picked: std::sync::mpsc::channel(),
            picking: false,
            closing: false,
        }
    }

    fn perform(&mut self, a: Act) {
        match a {
            Act::Close => self.closing = true,
            Act::Saved(cfg) => {
                self.th = crate::theme::resolve(&cfg);
                if let Some(d) = lock(&DAEMON).as_ref() {
                    let _ = d.send(HotEvent::ReloadConfig);
                }
            }
            Act::Browse if !self.picking => {
                self.picking = true;
                let start = PathBuf::from(self.s.form.folder.text.trim());
                let tx = self.picked.0.clone();
                let owner = OwnerHandle::of(self.hwnd);
                std::thread::spawn(move || {
                    let _ = tx.send(crate::ui::folder_dialog::pick_folder(owner.get(), &start));
                });
            }
            Act::Browse => {}
            Act::OpenConfig => {
                std::thread::spawn(crate::actions::open_config);
            }
        }
    }

    /// Draw a frame from the current input; repeat while it produced a
    /// click (the content changes) or the kit asks for it.
    fn render(&mut self) {
        let k = wind::scale(self.hwnd);
        for _ in 0..4 {
            if self.closing || self.canvas.width() == 0 {
                return;
            }
            let click = frame(&mut self.canvas, &self.th, &self.input, &mut self.focus, &mut self.s, k);
            self.input.end_frame();
            let (click, redraw) = click;
            let Some(c) = click else {
                if redraw {
                    continue;
                }
                return;
            };
            for a in self.s.on(c) {
                self.perform(a);
            }
        }
    }

    fn settle(&mut self) {
        if self.closing {
            wind::close(self.hwnd);
        }
    }
}

/// Paint one frame of `s` into `canvas` (window background first); returns
/// the click (Esc/Enter included) and whether the kit wants another frame.
fn frame(canvas: &mut PixBuf, th: &Theme, input: &Input, focus: &mut FocusState, s: &mut Settings, k: f32) -> (Option<Click>, bool) {
    let (w, h) = canvas.dimensions();
    let bg = th.surface.with_alpha(255);
    for p in canvas.as_raw_mut().as_chunks_mut::<4>().0 {
        *p = [bg.r, bg.g, bg.b, 255];
    }
    let area = FRect { x: 0.0, y: 0.0, w: w as f32 / k, h: h as f32 / k };
    let mut clip = crate::ui::SystemClipboard;
    let fb = crate::uifb::Fb::new(canvas.as_raw_mut(), w as usize);
    let mut ui = Ui::new(fb, th, input, focus, &mut clip, k, area);
    let click = paint(&mut ui, s);
    let out = ui.finish();
    let click = click.or(if out.escape {
        Some(Click::Escape)
    } else if out.enter {
        Some(Click::Enter)
    } else {
        None
    });
    (click, out.redraw)
}

/// A window handle that may cross to the folder-picker thread.
#[derive(Clone, Copy)]
struct OwnerHandle(usize);

impl OwnerHandle {
    fn of(h: Hwnd) -> OwnerHandle {
        #[cfg(not(target_os = "macos"))]
        return OwnerHandle(h.0 as usize);
        #[cfg(target_os = "macos")]
        return OwnerHandle(h.0);
    }

    fn get(self) -> Option<Hwnd> {
        #[cfg(windows)]
        let h = windows::Win32::Foundation::HWND(self.0 as *mut core::ffi::c_void);
        #[cfg(not(windows))]
        let h = Hwnd(self.0 as _);
        (self.0 != 0).then_some(h)
    }
}

impl Driver for Window {
    fn on_create(&mut self, hwnd: Hwnd) {
        self.hwnd = hwnd;
    }

    fn on_event(&mut self, ev: Ev) -> bool {
        match ev {
            Ev::Resize(w, h) => {
                self.canvas = PixBuf::new(w.max(1), h.max(1));
                self.render();
            }
            Ev::Close => {
                for a in self.s.on(Click::Cancel) {
                    self.perform(a);
                }
            }
            Ev::Timer => {
                let mut changed = false;
                let mut raise = false;
                while let Ok(m) = self.rx.try_recv() {
                    match m {
                        Msg::Raise => raise = true,
                        Msg::Note(t) => {
                            self.s.notify(t);
                            changed = true;
                        }
                    }
                }
                if raise {
                    #[cfg(windows)]
                    wind::raise(self.hwnd);
                }
                // The broken file fixed in an editor: the window works again.
                if self.s.broken() && self.rechecked.elapsed() >= std::time::Duration::from_millis(500) {
                    self.rechecked = std::time::Instant::now();
                    if self.s.recheck() {
                        self.th = crate::theme::resolve(&self.s.loaded);
                        changed = true;
                    }
                }
                while let Ok(p) = self.picked.1.try_recv() {
                    self.picking = false;
                    if let Some(p) = p {
                        self.s.form.folder.set(&p.display().to_string());
                        changed = true;
                    }
                }
                if changed {
                    self.render();
                }
                self.settle();
                return changed && !self.closing;
            }
            Ev::Move { .. } => {
                self.input.feed(&ev);
                self.render();
                if !self.closing {
                    wind::invalidate(self.hwnd); // hover changes
                }
            }
            _ => {
                self.input.feed(&ev);
                self.render();
            }
        }
        self.settle();
        !self.closing
    }

    fn frame(&mut self) -> Option<&mut PixBuf> {
        if self.canvas.width() == 0 || self.closing {
            return None;
        }
        if self.out.dimensions() != self.canvas.dimensions() {
            self.out = PixBuf::new(self.canvas.width(), self.canvas.height());
        }
        self.out.as_raw_mut().copy_from_slice(self.canvas.as_raw());
        Some(&mut self.out)
    }

    fn cursor(&self) -> Cursor {
        Cursor::Arrow
    }
}

#[cfg(test)]
mod tests;
