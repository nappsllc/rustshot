//! Immediate-mode control kit for the decorated windows (update dialog,
//! Settings): drawn with `uifb` + the baked Inter font + theme tokens, in
//! the toolbar/popover visual language of `editor/chrome.rs`.
//!
//! One frame: feed the window's events into an [`Input`], build a [`Ui`]
//! over the frame buffer, call the controls (each lays itself out through
//! the [`layout`] cursor, draws, and returns whether its value changed or
//! it was clicked), then [`Ui::finish`] draws popups and reports keys no
//! control consumed (Esc, Enter). Clear the input with [`Input::end_frame`].
//! Control state that outlives a frame lives with the caller (`&mut bool`,
//! [`TextState`], [`TableState`]) or in [`FocusState`] (focus, mouse
//! capture, open dropdown, shortcut recording).
//!
//! Coordinates: layout values are logical px; the frame buffer and mouse
//! positions are physical px; `Ui::k` is physical px per logical px.


pub mod controls;
pub mod folder_dialog;
pub mod layout;
mod text_field;

#[cfg_attr(not(test), allow(unused_imports))]
pub use controls::{Col, Row, TableState};
#[cfg_attr(not(test), allow(unused_imports))]
pub use text_field::TextState;

use crate::editor::style as chrome;
use crate::fonts::UI;
use crate::objects::{FRect, Pt};
use crate::theme::Theme;
use crate::uifb::Fb;
use crate::wind::{key, Ev, Mods};
use layout::Layout;

/// One keyboard event of the frame, in arrival order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum KeyIn {
    /// A key press (auto-repeat included); releases are not kept.
    Key { vk: u32, mods: Mods },
    /// A typed character (control codes filtered, surrogates combined).
    Text(char),
}

/// Mouse and keyboard input gathered since the last frame.
#[derive(Clone, Debug)]
pub struct Input {
    /// Pointer position, physical px.
    pub mouse: Pt,
    /// Left button held now.
    pub held: bool,
    /// Where the left button went down this frame.
    pub pressed: Option<Pt>,
    /// Where the left button went up this frame.
    pub released: Option<Pt>,
    /// Wheel delta this frame (120 per notch; positive = up).
    pub wheel: i32,
    pub keys: Vec<KeyIn>,
    /// Modifiers as of the latest key event (Shift+click extends a selection).
    pub mods: Mods,
    /// The pointer position changed this frame (hover follows the mouse
    /// only then, so a keyboard-moved highlight is not snapped back).
    pub moved: bool,
    /// High surrogate waiting for its low half.
    hi: Option<u16>,
}

impl Default for Input {
    fn default() -> Input {
        Input {
            mouse: Pt::new(-1.0, -1.0),
            held: false,
            pressed: None,
            released: None,
            wheel: 0,
            keys: Vec::new(),
            mods: Mods::default(),
            moved: false,
            hi: None,
        }
    }
}

impl Input {
    /// Fold one window event in; returns whether it is UI input.
    pub fn feed(&mut self, ev: &Ev) -> bool {
        match *ev {
            Ev::Move { x, y } => self.point(x, y),
            Ev::Down { x, y } => {
                self.point(x, y);
                self.pressed = Some(self.mouse);
                self.held = true;
            }
            Ev::Up { x, y } => {
                self.point(x, y);
                self.released = Some(self.mouse);
                self.held = false;
            }
            Ev::Wheel { delta, x, y } => {
                self.point(x, y);
                self.wheel += delta;
            }
            Ev::Key { vk, up, mods, .. } => {
                self.mods = mods;
                if !up {
                    self.keys.push(KeyIn::Key { vk, mods });
                }
            }
            Ev::Char(u) => self.char_unit(u),
            Ev::Focus(false) => {
                if self.held {
                    self.held = false;
                    self.released = Some(self.mouse);
                }
            }
            _ => return false,
        }
        true
    }

    fn point(&mut self, x: i32, y: i32) {
        let p = Pt::new(x as f32, y as f32);
        self.moved |= p != self.mouse;
        self.mouse = p;
    }

    fn char_unit(&mut self, u: u16) {
        let units: Vec<u16> = match (self.hi.take(), u) {
            (_, 0xD800..=0xDBFF) => {
                self.hi = Some(u); // an unpaired earlier high is dropped
                return;
            }
            (Some(h), 0xDC00..=0xDFFF) => vec![h, u],
            (None, 0xDC00..=0xDFFF) => return,
            (_, u) => vec![u],
        };
        for c in char::decode_utf16(units).flatten() {
            if !c.is_control() {
                self.keys.push(KeyIn::Text(c));
            }
        }
    }

    /// Clear the per-frame parts (presses, wheel, keys) after a frame.
    pub fn end_frame(&mut self) {
        self.pressed = None;
        self.released = None;
        self.wheel = 0;
        self.moved = false;
        self.keys.clear();
    }
}

/// A control id: the FNV-1a hash of its name.
pub fn id_of(name: &str) -> u64 {
    name.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
}

/// Keyboard focus and other cross-frame interaction state.
#[derive(Clone, Debug, Default)]
pub struct FocusState {
    focused: Option<u64>,
    /// Show the focus ring (focus moved by keyboard, not by a click).
    visible: bool,
    /// Control holding the mouse since the press (button, slider drag, text selection).
    active: Option<u64>,
    /// Dropdown whose list is open, its highlighted item and list rect (physical px).
    open: Option<(u64, usize, FRect)>,
    /// key_capture waiting for a chord.
    recording: Option<u64>,
    /// Focusable ids in draw order: this frame / the previous one.
    order: Vec<u64>,
    prev: Vec<u64>,
    /// Tab (false) / Shift+Tab (true) seen before any focus order was known.
    pending_tab: Option<bool>,
}

impl FocusState {
    #[allow(dead_code)] // Settings window (Task 8)
    pub fn is_focused(&self, id: &str) -> bool {
        self.focused == Some(id_of(id))
    }

    /// Focus control `id` (with the focus ring, as if tabbed to).
    #[allow(dead_code)] // Settings window (Task 8)
    pub fn focus(&mut self, id: &str) {
        self.set(Some(id_of(id)), true);
    }

    #[allow(dead_code)] // Settings window (Task 8)
    pub fn clear(&mut self) {
        self.set(None, false);
    }

    /// Whether any dropdown list is open or a shortcut is being recorded
    /// (a dialog should then not treat Esc/Enter as its own).
    #[allow(dead_code)] // Settings window (Task 8)
    pub fn busy(&self) -> bool {
        self.open.is_some() || self.recording.is_some()
    }

    fn set(&mut self, id: Option<u64>, visible: bool) {
        if self.focused != id {
            self.open = None;
            self.recording = None;
        }
        self.focused = id;
        self.visible = visible && id.is_some();
    }

    /// Move along `order` (wrapping); from nothing, to the first/last.
    fn step(&mut self, order: &[u64], back: bool) {
        if order.is_empty() {
            return;
        }
        let n = order.len();
        let i = match self.focused.and_then(|f| order.iter().position(|&o| o == f)) {
            Some(i) if back => (i + n - 1) % n,
            Some(i) => (i + 1) % n,
            None if back => n - 1,
            None => 0,
        };
        self.set(Some(order[i]), true);
    }
}

/// Text get/set; tests inject a fake.
pub trait Clipboard {
    #[allow(dead_code)] // Settings window (Task 8)
    fn get(&mut self) -> Option<String>;
    #[allow(dead_code)] // Settings window (Task 8)
    fn set(&mut self, text: &str);
}

/// The OS clipboard (CF_UNICODETEXT / NSPasteboard / X11 CLIPBOARD).
pub struct SystemClipboard;

impl Clipboard for SystemClipboard {
    fn get(&mut self) -> Option<String> {
        #[cfg(not(target_os = "linux"))]
        return crate::export::clipboard_text();
        #[cfg(target_os = "linux")]
        return linux_clipboard_text();
    }

    fn set(&mut self, text: &str) {
        if let Err(e) = crate::export::copy_text_to_clipboard(text) {
            eprintln!("rustshot: clipboard: {e:#}");
        }
    }
}

/// Reading CLIPBOARD needs a selection-conversion round trip; the desktop
/// tools do it (wl-paste on Wayland, then xclip, then xsel). None when
/// none is installed or the clipboard holds no text.
#[cfg(target_os = "linux")]
fn linux_clipboard_text() -> Option<String> {
    let tools: [(&str, &[&str]); 3] = [
        ("wl-paste", &["--no-newline", "--type", "text/plain"]),
        ("xclip", &["-selection", "clipboard", "-o"]),
        ("xsel", &["--clipboard", "--output"]),
    ];
    for (bin, args) in tools {
        if bin == "wl-paste" && std::env::var_os("WAYLAND_DISPLAY").is_none() {
            continue;
        }
        let mut cmd = std::process::Command::new(bin);
        cmd.args(args);
        // A hung selection owner must not freeze the UI thread for long.
        if let Some(out) = output_within(cmd, std::time::Duration::from_secs(1)) {
            return String::from_utf8(out).ok();
        }
    }
    None
}

/// Run `cmd` (stdin closed, stderr dropped) and return its stdout when it
/// exits successfully within `limit`; otherwise kill it and return None.
#[cfg(any(target_os = "linux", test))]
fn output_within(mut cmd: std::process::Command, limit: std::time::Duration) -> Option<Vec<u8>> {
    use std::io::Read;
    use std::process::Stdio;
    use std::time::Instant;
    let start = Instant::now();
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let mut pipe = child.stdout.take()?;
    // Read on a helper thread so a full pipe cannot stall the deadline.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    let kill = |child: &mut std::process::Child| {
        let _ = child.kill();
        let _ = child.wait();
    };
    let Ok(buf) = rx.recv_timeout(limit) else {
        kill(&mut child);
        return None;
    };
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success().then_some(buf),
            Ok(None) if start.elapsed() < limit => std::thread::sleep(std::time::Duration::from_millis(5)),
            _ => {
                kill(&mut child);
                return None;
            }
        }
    }
}

/// What the frame left for the caller.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Out {
    /// Esc pressed and not consumed (no open list, no recording): close.
    pub escape: bool,
    /// Enter pressed and not consumed by a focused control: default action.
    pub enter: bool,
    /// Focus changed after drawing: render another frame.
    pub redraw: bool,
}

/// Deferred top layer (the open dropdown list).
struct Popup {
    r: FRect,
    items: Vec<String>,
    sel: usize,
    hover: usize,
}

/// One frame of controls over a frame buffer.
pub struct Ui<'a> {
    pub fb: Fb<'a>,
    pub theme: &'a Theme,
    pub input: &'a Input,
    pub focus: &'a mut FocusState,
    /// Physical px per logical px.
    pub k: f32,
    #[allow(dead_code)] // Settings window (Task 8)
    pub clip: &'a mut dyn Clipboard,
    /// Controls drawn while false are dimmed, inert and skipped by Tab.
    pub enabled: bool,
    pub(crate) lay: Layout,
    /// `used[i]`: `input.keys[i]` was consumed.
    used: Vec<bool>,
    /// Ids registered this frame, disabled ones included (duplicate check).
    seen: std::collections::HashSet<u64>,
    popup: Option<Popup>,
    /// The open list's rect at frame start: presses and hover there belong
    /// to its dropdown even after it closes mid-frame.
    block: Option<FRect>,
    /// Dropdown whose list this frame's press closed (a press on its own box).
    closed: Option<u64>,
    redraw: bool,
}

impl<'a> Ui<'a> {
    /// Start a frame laid out in `area` (logical px). Tab / Shift+Tab move
    /// the focus along the previous frame's order here.
    pub fn new(
        fb: Fb<'a>,
        theme: &'a Theme,
        input: &'a Input,
        focus: &'a mut FocusState,
        clip: &'a mut dyn Clipboard,
        k: f32,
        area: FRect,
    ) -> Ui<'a> {
        let mut ui = Ui {
            fb,
            theme,
            input,
            focus,
            k,
            clip,
            enabled: true,
            lay: Layout::new(area),
            used: vec![false; input.keys.len()],
            seen: Default::default(),
            popup: None,
            block: None,
            closed: None,
            redraw: false,
        };
        ui.focus.prev = std::mem::take(&mut ui.focus.order);
        for (i, ev) in input.keys.iter().enumerate() {
            if let KeyIn::Key { vk: key::TAB, mods } = *ev
                && !mods.ctrl
                && !mods.alt
            {
                ui.used[i] = true;
                let prev = std::mem::take(&mut ui.focus.prev);
                if prev.is_empty() {
                    ui.focus.pending_tab = Some(mods.shift);
                } else {
                    ui.focus.step(&prev, mods.shift);
                }
                ui.focus.prev = prev;
            }
        }
        // A press outside the open list closes it (the press still lands).
        if let (Some(p), Some((id, _, r))) = (input.pressed, ui.focus.open)
            && !contains(r, p)
        {
            ui.focus.open = None;
            ui.closed = Some(id);
        }
        ui.block = ui.focus.open.map(|o| o.2);
        if input.pressed.is_some() {
            ui.focus.visible = false;
        }
        ui
    }

    /// Draw the popups and report unconsumed Esc/Enter.
    pub fn finish(mut self) -> Out {
        if self.input.released.is_some() {
            self.focus.active = None;
        }
        if let Some(back) = self.focus.pending_tab.take() {
            let order = self.focus.order.clone();
            self.focus.step(&order, back);
            self.redraw = true;
        }
        if let Some(f) = self.focus.focused
            && !self.focus.order.contains(&f)
        {
            self.focus.set(None, false); // its control is gone (other tab, hidden)
        }
        if let Some(p) = self.popup.take() {
            controls::draw_popup(&mut self, &p.items, p.r, p.sel, p.hover);
        }
        let mut out = Out { redraw: self.redraw, ..Out::default() };
        for i in 0..self.input.keys.len() {
            if !self.used[i]
                && let KeyIn::Key { vk, .. } = self.input.keys[i]
            {
                out.escape |= vk == key::ESCAPE;
                out.enter |= vk == key::RETURN;
            }
        }
        out
    }

    /// Logical → physical px.
    pub fn px(&self, v: f32) -> f32 {
        v * self.k
    }

    /// The chrome drawing context (shared surfaces, rings, key caps).
    fn chrome(&self) -> chrome::Ui<'a> {
        chrome::Ui { th: self.theme, s: self.k, font: Some(&UI) }
    }

    /// Register `id` as focusable (in draw order); returns whether it has focus.
    fn focusable(&mut self, id: u64) -> bool {
        let fresh = self.seen.insert(id);
        debug_assert!(fresh, "two controls share the id {id:#x} this frame");
        if self.enabled {
            self.focus.order.push(id);
        }
        self.focus.focused == Some(id)
    }

    /// Pointer over `r` (and not over a popup that is not `owner`'s).
    fn hovered(&self, r: FRect) -> bool {
        self.enabled && contains(r, self.input.mouse) && !self.blocked(self.input.mouse)
    }

    /// The press of this frame landed in `r`.
    fn pressed_in(&self, r: FRect) -> bool {
        self.enabled && self.input.pressed.is_some_and(|p| contains(r, p) && !self.blocked(p))
    }

    /// Inside the open dropdown list (which belongs to its dropdown only).
    fn blocked(&self, p: Pt) -> bool {
        self.block.is_some_and(|r| contains(r, p))
    }

    /// Unconsumed key presses of this frame: (index, vk, mods).
    fn keys(&self) -> Vec<(usize, u32, Mods)> {
        (self.input.keys.iter().enumerate())
            .filter_map(|(i, e)| match *e {
                KeyIn::Key { vk, mods } if !self.used[i] => Some((i, vk, mods)),
                _ => None,
            })
            .collect()
    }

    fn consume(&mut self, i: usize) {
        self.used[i] = true;
    }

    #[allow(dead_code)] // Settings window (Task 8)
    fn is_used(&self, i: usize) -> bool {
        self.used[i]
    }

    /// Consume and report a press of `vk` (no Ctrl/Alt) while `id` is focused.
    fn take_key(&mut self, focused: bool, vks: &[u32]) -> bool {
        if !focused || !self.enabled {
            return false;
        }
        let mut hit = false;
        for (i, vk, m) in self.keys() {
            if vks.contains(&vk) && !m.ctrl && !m.alt {
                self.consume(i);
                hit = true;
            }
        }
        hit
    }
}

pub fn contains(r: FRect, p: Pt) -> bool {
    p.x >= r.x && p.y >= r.y && p.x < r.x1() && p.y < r.y1()
}

/// Run `draw` with output clipped to `r` (physical px): the region is
/// copied out, drawn into as its own buffer, and copied back.
pub fn clipped(fb: &mut Fb, r: FRect, draw: impl FnOnce(&mut Fb)) {
    let (bx0, by0) = (fb.ox, fb.oy);
    let (bx1, by1) = (fb.ox + fb.stride as i32, fb.oy + fb.height());
    let x0 = (r.x.floor() as i32).max(bx0);
    let y0 = (r.y.floor() as i32).max(by0);
    let x1 = (r.x1().ceil() as i32).min(bx1);
    let y1 = (r.y1().ceil() as i32).min(by1);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
    let row = |y: usize| ((y0 - by0) as usize + y) * fb.stride * 4 + (x0 - bx0) as usize * 4;
    let mut tmp = vec![0u8; w * h * 4];
    for y in 0..h {
        tmp[y * w * 4..(y + 1) * w * 4].copy_from_slice(&fb.d[row(y)..row(y) + w * 4]);
    }
    draw(&mut Fb::with_origin(&mut tmp, w, x0, y0, fb.order));
    for y in 0..h {
        let at = row(y);
        fb.d[at..at + w * 4].copy_from_slice(&tmp[y * w * 4..(y + 1) * w * 4]);
    }
}

#[cfg(test)]
pub mod preview {
    //! Render a frame of controls into a PixBuf; save PNGs for review.
    use super::*;
    use crate::pixbuf::PixBuf;

    /// Fake clipboard for tests.
    #[derive(Default)]
    pub struct FakeClip(pub Option<String>);

    impl Clipboard for FakeClip {
        fn get(&mut self) -> Option<String> {
            self.0.clone()
        }
        fn set(&mut self, text: &str) {
            self.0 = Some(text.to_string());
        }
    }

    /// `w`×`h` logical px at scale `k`, window background, one frame of `f`.
    pub fn render(w: u32, h: u32, k: f32, th: &Theme, input: &Input, focus: &mut FocusState, f: impl FnOnce(&mut Ui)) -> PixBuf {
        let (pw, ph) = ((w as f32 * k).round() as u32, (h as f32 * k).round() as u32);
        let bg = th.surface.with_alpha(255);
        let mut img = PixBuf::from_pixel(pw, ph, [bg.r, bg.g, bg.b, 255]);
        let mut clip = FakeClip::default();
        let area = FRect { x: 0.0, y: 0.0, w: w as f32, h: h as f32 };
        let fb = Fb::new(img.as_raw_mut(), pw as usize);
        let mut ui = Ui::new(fb, th, input, focus, &mut clip, k, area);
        f(&mut ui);
        ui.finish();
        img
    }

    /// Write `img` as `name` under `RUSTSHOT_PREVIEW_DIR` (no-op when unset).
    pub fn save(name: &str, img: &PixBuf) {
        let Some(dir) = std::env::var_os("RUSTSHOT_PREVIEW_DIR") else { return };
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(name), img.to_png().unwrap()).unwrap();
    }
}

#[cfg(test)]
mod tests;
