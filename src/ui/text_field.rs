//! Single-line text field: caret, selection, clipboard, horizontal scroll.
//! No IME (out of scope) and no caret blink (frames are input-driven).

use super::controls::{field_frame, text_size};
use super::layout::H;
use super::{id_of, Clipboard, KeyIn, Ui};
use crate::objects::FRect;
use crate::uifb::text_width;
use crate::wind::{key, Mods};

/// Text plus caret/selection (byte offsets on char boundaries) and scroll.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextState {
    pub text: String,
    caret: usize,
    anchor: usize,
    /// Horizontal scroll, physical px.
    scroll: f32,
}

impl TextState {
    /// `text` with the caret at the end.
    pub fn new(text: &str) -> TextState {
        TextState { text: text.to_string(), caret: text.len(), anchor: text.len(), scroll: 0.0 }
    }

    /// Replace the text (caret to the end, selection dropped).
    pub fn set(&mut self, text: &str) {
        *self = TextState::new(text);
    }

    pub fn caret(&self) -> usize {
        self.caret
    }

    /// Selected byte range (empty when nothing is selected).
    pub fn selection(&self) -> std::ops::Range<usize> {
        self.caret.min(self.anchor)..self.caret.max(self.anchor)
    }

    pub fn selected(&self) -> &str {
        &self.text[self.selection()]
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.caret = self.text.len();
    }

    /// Select `range` with the caret at its end (clamped to char boundaries).
    pub fn select(&mut self, range: std::ops::Range<usize>) {
        self.anchor = self.floor(range.start);
        self.caret = self.floor(range.end);
    }

    /// Replace the selection with `s` (typing, paste, token insertion).
    pub fn insert(&mut self, s: &str) {
        let r = self.selection();
        self.text.replace_range(r.clone(), s);
        self.caret = r.start + s.len();
        self.anchor = self.caret;
    }

    fn floor(&self, mut i: usize) -> usize {
        i = i.min(self.text.len());
        while !self.text.is_char_boundary(i) {
            i -= 1;
        }
        i
    }

    fn prev(&self, i: usize) -> usize {
        self.text[..i].char_indices().next_back().map_or(0, |(j, _)| j)
    }

    fn next(&self, i: usize) -> usize {
        self.text[i..].chars().next().map_or(i, |c| i + c.len_utf8())
    }

    /// Start of the word before `i` (skips spaces first).
    fn word_prev(&self, i: usize) -> usize {
        let t = self.text[..i].trim_end();
        t.rfind(char::is_whitespace).map_or(0, |j| j + t[j..].chars().next().map_or(1, char::len_utf8))
    }

    /// End of the word after `i` (skips spaces first).
    fn word_next(&self, i: usize) -> usize {
        let rest = &self.text[i..];
        let skip = rest.len() - rest.trim_start().len();
        let w = rest[skip..].find(char::is_whitespace).unwrap_or(rest.len() - skip);
        i + skip + w
    }

    fn move_to(&mut self, i: usize, extend: bool) {
        self.caret = i;
        if !extend {
            self.anchor = i;
        }
    }

    /// One editing key. Returns (text changed, key handled).
    pub fn key(&mut self, vk: u32, m: Mods, clip: &mut dyn Clipboard) -> (bool, bool) {
        let sel = !self.selection().is_empty();
        let c = self.caret;
        match vk {
            key::LEFT if sel && !m.shift => {
                let s = self.selection().start;
                self.move_to(s, false)
            }
            key::RIGHT if sel && !m.shift => {
                let e = self.selection().end;
                self.move_to(e, false)
            }
            key::LEFT => self.move_to(if m.ctrl { self.word_prev(c) } else { self.prev(c) }, m.shift),
            key::RIGHT => self.move_to(if m.ctrl { self.word_next(c) } else { self.next(c) }, m.shift),
            key::HOME | key::UP => self.move_to(0, m.shift),
            key::END | key::DOWN => self.move_to(self.text.len(), m.shift),
            key::BACK | key::DELETE => {
                if !sel {
                    let back = vk == key::BACK;
                    let to = match (back, m.ctrl) {
                        (true, true) => self.word_prev(c),
                        (true, false) => self.prev(c),
                        (false, true) => self.word_next(c),
                        (false, false) => self.next(c),
                    };
                    if to == c {
                        return (false, true);
                    }
                    self.anchor = to;
                }
                self.insert("");
                return (true, true);
            }
            0x41 if m.ctrl => self.select_all(),             // A
            0x43 | key::INSERT if m.ctrl => copy(self, clip), // C, Ctrl+Insert
            0x58 if m.ctrl => {
                // X
                if !sel {
                    return (false, true);
                }
                copy(self, clip);
                self.insert("");
                return (true, true);
            }
            0x56 if m.ctrl => {
                // V
                let Some(t) = clip.get() else { return (false, true) };
                // Single line: line breaks become spaces, other controls go.
                let t: String = t
                    .trim_end_matches(['\r', '\n'])
                    .chars()
                    .map(|c| if c == '\n' || c == '\t' { ' ' } else { c })
                    .filter(|c| !c.is_control())
                    .collect();
                self.insert(&t);
                return (!t.is_empty() || sel, true);
            }
            _ => return (false, false),
        }
        (false, true)
    }
}

fn copy(st: &TextState, clip: &mut dyn Clipboard) {
    if !st.selected().is_empty() {
        clip.set(st.selected());
    }
}

/// Byte offset in `s` nearest to `x` px from the text origin.
fn hit(s: &str, px: f32, x: f32) -> usize {
    let font = &crate::fonts::UI;
    let mut best = (0, x.abs());
    for (i, c) in s.char_indices() {
        let end = i + c.len_utf8();
        let d = (text_width(font, px, &s[..end]) - x).abs();
        if d < best.1 {
            best = (end, d);
        }
    }
    best.0
}

impl Ui<'_> {
    /// An editable single-line field (fills the row unless `width` is set).
    /// Returns whether the text changed this frame.
    pub fn text_field(&mut self, id: &str, st: &mut TextState) -> bool {
        let r = self.alloc(None, H);
        let id = id_of(id);
        self.focusable(id);
        let px = self.px(text_size());
        let pad = self.px(10.0);
        let inner = FRect { x: r.x + pad, y: r.y, w: (r.w - 2.0 * pad).max(1.0), h: r.h };
        let font = &crate::fonts::UI;
        let mut changed = false;

        // Mouse: press places the caret (Shift extends), drag selects.
        let ox = inner.x - st.scroll;
        if self.pressed_in(r) {
            let shift = self.input.mods.shift;
            self.focus.set(Some(id), false);
            self.focus.active = Some(id);
            let i = hit(&st.text, px, self.input.pressed.unwrap().x - ox);
            st.move_to(i, shift);
        } else if self.focus.active == Some(id) && self.input.held {
            let i = hit(&st.text, px, self.input.mouse.x - ox);
            st.move_to(i, true);
        }

        if self.focus.focused == Some(id) && self.enabled {
            let input = self.input;
            for (i, ev) in input.keys.iter().enumerate() {
                if self.used & super::bit(i) != 0 {
                    continue;
                }
                match *ev {
                    KeyIn::Text(c) => {
                        st.insert(c.encode_utf8(&mut [0; 4]));
                        changed = true;
                    }
                    KeyIn::Key { vk, mods } => {
                        let (ch, used) = st.key(vk, mods, &mut *self.clip);
                        changed |= ch;
                        if used {
                            self.used |= super::bit(i);
                        }
                    }
                }
            }
        }

        // Keep the caret in view while editing; show the start otherwise.
        let cx = text_width(font, px, &st.text[..st.caret]);
        if self.focus.focused != Some(id) {
            st.scroll = 0.0;
        } else if cx - st.scroll > inner.w {
            st.scroll = cx - inner.w;
        } else if cx < st.scroll {
            st.scroll = cx;
        }
        let full = text_width(font, px, &st.text);
        st.scroll = st.scroll.min((full - inner.w + 1.0).max(0.0)).max(0.0);

        let focused = self.focus.focused == Some(id) && self.enabled;
        field_frame(self, r, focused, self.hovered(r));
        let th = self.theme;
        let fade = if self.enabled { 1.0 } else { 0.4 };
        let (sel, caret, scroll) = (st.selection(), st.caret, st.scroll);
        let text = st.text.as_str();
        let k = self.k;
        super::clipped(&mut self.fb, inner, |fb| {
            let ox = inner.x - scroll;
            let cy = inner.y + inner.h / 2.0;
            let lh = (16.0 * k).round();
            if focused && !sel.is_empty() {
                let x0 = ox + text_width(font, px, &text[..sel.start]);
                let x1 = ox + text_width(font, px, &text[..sel.end]);
                let y = (cy - lh / 2.0).round();
                fb.fill_rect(x0.round() as i32, y as i32, (x1 - x0).round() as i32, lh as i32, th.accent_ring);
            }
            let ty = cy - crate::uifb::text_height(font, px) / 2.0;
            fb.draw_text(font, px, text, ox, ty, th.text.fade(fade));
            if focused {
                let x = (ox + text_width(font, px, &text[..caret])).round().min(inner.x1() - k.round().max(1.0));
                let y = (cy - lh / 2.0).round();
                fb.fill_rect(x as i32, y as i32, k.round().max(1.0) as i32, lh as i32, th.text);
            }
        });
        changed
    }
}
