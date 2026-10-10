//! The controls: label, button, toggle, dropdown, slider, tabs, table,
//! progress bar and shortcut capture (the text field is in text_field.rs).
//! Styling comes from the toolbar: 8 px radii, `bg_hover`/`bg_pressed`
//! states, the accent tint of an active tool, popover surfaces with the
//! shadow stack, and an accent focus ring for keyboard focus.

use super::layout::H;
use super::{contains, id_of, Popup, Ui};
use crate::editor::style::{self as chrome, chord_caps, ALL_LAYERS};
use crate::hotkey;
use crate::icon_path::draw_icon;
use crate::keymap::Chord;
use crate::objects::FRect;
use crate::uifb::{text_height, text_width, C4};
use crate::wind::key;

/// Body text size of every control (logical px; the hint line's size).
pub(super) fn text_size() -> f32 {
    13.0
}

const RADIUS: f32 = 8.0;
const DISABLED: f32 = 0.4;
const ITEM_H: f32 = 28.0;

/// Text width at `size` logical px, in logical px.
fn tw(ui: &Ui, size: f32, s: &str) -> f32 {
    text_width(&crate::fonts::UI, ui.px(size), s) / ui.k
}

/// Draw `s` at `size` vertically centred on `cy` (physical px).
fn text(ui: &mut Ui, size: f32, s: &str, x: f32, cy: f32, c: C4) -> f32 {
    let font = &crate::fonts::UI;
    let px = ui.px(size);
    ui.fb.draw_text(font, px, s, x.round(), (cy - text_height(font, px) / 2.0).round(), c)
}

fn fill(ui: &mut Ui, r: FRect, radius: f32, c: C4) {
    let rad = ui.px(radius);
    chrome::fill(&mut ui.fb, chrome::round(r), rad, c);
}

fn ring(ui: &mut Ui, r: FRect, radius: f32, c: C4) {
    let cu = ui.chrome();
    let rad = ui.px(radius);
    chrome::ring(&mut ui.fb, &cu, chrome::round(r), rad, c);
}

fn grow(r: FRect, d: f32) -> FRect {
    FRect { x: r.x - d, y: r.y - d, w: r.w + 2.0 * d, h: r.h + 2.0 * d }
}

/// 2 px accent ring 2 px outside `r` (keyboard focus).
fn focus_ring(ui: &mut Ui, r: FRect, radius: f32) {
    let (d, lw) = (ui.px(2.0), ui.px(2.0).round());
    let o = grow(r, d + lw / 2.0);
    let c = ui.theme.accent.fade(0.7);
    ui.fb.stroke_rounded(o, ui.px(radius) + d + lw / 2.0, lw, c);
}

fn show_focus(ui: &Ui, id: u64) -> bool {
    ui.enabled && ui.focus.focused == Some(id) && ui.focus.visible
}

fn fade(ui: &Ui) -> f32 {
    if ui.enabled { 1.0 } else { DISABLED }
}

/// Text field / dropdown / shortcut box: subtle fill, 1 px border, accent
/// border while focused.
pub(super) fn field_frame(ui: &mut Ui, r: FRect, focused: bool, hovered: bool) {
    let th = ui.theme;
    let k = fade(ui);
    let bg = if hovered && !focused { th.bg_pressed } else { th.bg_hover };
    fill(ui, r, RADIUS, bg.fade(k));
    if focused {
        ring(ui, r, RADIUS, th.accent.fade(k));
        let o = grow(r, ui.px(1.0));
        let lw = ui.px(2.0).round();
        ui.fb.stroke_rounded(grow(o, lw / 2.0), ui.px(RADIUS) + ui.px(1.0) + lw / 2.0, lw, th.accent_ring);
    } else {
        ring(ui, r, RADIUS, th.border.fade(k));
    }
}

/// Mouse half of a push control: press captures, release over it clicks.
/// Returns (hovered, pressed-look, clicked).
fn press_logic(ui: &mut Ui, id: u64, r: FRect) -> (bool, bool, bool) {
    let hov = ui.hovered(r);
    if ui.pressed_in(r) {
        ui.focus.active = Some(id);
        ui.focus.set(Some(id), false);
    }
    let mine = ui.focus.active == Some(id);
    let clicked = mine && ui.input.released.is_some_and(|p| contains(r, p));
    (hov, mine && ui.input.held && hov, clicked)
}

/// A table column: title and share of the width (the last takes the rest).
#[derive(Clone, Copy, Debug)]
pub struct Col<'a> {
    pub title: &'a str,
    pub frac: f32,
}

/// A table row: one string per column; `error` paints it in the error colour.
#[derive(Clone, Copy, Debug)]
pub struct Row<'a> {
    pub cells: &'a [&'a str],
    pub error: bool,
}

/// Table selection and scroll offset (logical px).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TableState {
    pub selected: Option<usize>,
    pub scroll: f32,
    /// Last frame's body rect and row height (physical px), for `row_rect`.
    pub body: Option<(FRect, f32)>,
}

impl TableState {
    /// Where row `i` was drawn last frame (physical px), when visible.
    pub fn row_rect(&self, i: usize) -> Option<FRect> {
        let (b, rh) = self.body?;
        let y = b.y + i as f32 * rh - self.scroll_px(rh);
        (y + rh > b.y && y < b.y1()).then_some(FRect { x: b.x, y, w: b.w, h: rh })
    }

    fn scroll_px(&self, rh: f32) -> f32 {
        self.scroll * rh / ITEM_H
    }
}

impl Ui<'_> {
    /// One line of body text.
    pub fn label(&mut self, s: &str) {
        self.text_line(s, text_size(), false);
    }

    /// Secondary text: muted and slightly smaller.
    pub fn note(&mut self, s: &str) {
        self.text_line(s, 12.0, true);
    }

    /// Section heading.
    pub fn heading(&mut self, s: &str) {
        self.text_line(s, 15.0, false);
    }

    fn text_line(&mut self, s: &str, size: f32, muted: bool) {
        let w = tw(self, size, s).ceil();
        let h = if self.lay.in_row() { H } else { (size * 1.6).round() };
        let r = self.alloc(Some(w), h);
        let c = if muted { self.theme.text_muted } else { self.theme.text };
        let c = c.fade(fade(self));
        text(self, size, s, r.x, r.y + r.h / 2.0, c);
    }

    /// Push button; `primary` is the accent-filled default action.
    /// Clicked by mouse release over it or Enter/Space while focused.
    pub fn button(&mut self, id: &str, label: &str, primary: bool) -> bool {
        let w = (tw(self, text_size(), label) + 2.0 * 16.0).max(84.0).ceil();
        let r = self.alloc(Some(w), H);
        let id = id_of(id);
        let focused = self.focusable(id);
        let (hov, pressed, mut clicked) = press_logic(self, id, r);
        clicked |= self.take_key(focused, &[key::RETURN, key::SPACE]);
        let th = self.theme;
        let k = fade(self);
        let (bg, fg) = if primary {
            let bg = if pressed {
                mix(th.accent, C4::rgb(0, 0, 0), 0.2)
            } else if hov {
                mix(th.accent, C4::rgb(255, 255, 255), 0.12)
            } else {
                th.accent
            };
            (bg, if th.dark { C4::rgb(0x10, 0x11, 0x16) } else { C4::rgb(255, 255, 255) })
        } else {
            let bg = if pressed { th.bg_pressed } else { th.bg_hover };
            (bg, th.text)
        };
        fill(self, r, RADIUS, bg.fade(k));
        if !primary {
            if hov && !pressed {
                fill(self, r, RADIUS, th.bg_hover.fade(k));
            }
            ring(self, r, RADIUS, th.border.fade(k));
        }
        if show_focus(self, id) {
            focus_ring(self, r, RADIUS);
        }
        let x = r.x + (r.w - self.px(tw(self, text_size(), label))) / 2.0;
        text(self, text_size(), label, x, r.y + r.h / 2.0, fg.fade(k));
        clicked && self.enabled
    }

    /// On/off switch. Returns whether it flipped.
    pub fn toggle(&mut self, id: &str, on: &mut bool) -> bool {
        let r = self.alloc(Some(36.0), H);
        let id = id_of(id);
        let focused = self.focusable(id);
        let (hov, _, mut clicked) = press_logic(self, id, r);
        clicked |= self.take_key(focused, &[key::RETURN, key::SPACE]);
        if clicked && self.enabled {
            *on = !*on;
        }
        let th = self.theme;
        let k = fade(self);
        let (tw_, th_) = (self.px(36.0).round(), self.px(20.0).round());
        let t = FRect { x: r.x, y: (r.y + (r.h - th_) / 2.0).round(), w: tw_, h: th_ };
        let track = if *on {
            if hov { mix(th.accent, C4::rgb(255, 255, 255), 0.12) } else { th.accent }
        } else if hov {
            th.text_muted.fade(0.6)
        } else {
            th.text_muted.fade(0.45)
        };
        fill(self, t, 10.0, track.fade(k));
        let kr = self.px(8.0);
        let cx = if *on { t.x1() - self.px(10.0) } else { t.x + self.px(10.0) };
        let cy = t.y + t.h / 2.0;
        self.fb.fill_circle(cx, cy + self.px(0.5), kr + self.px(0.5), C4::new(0, 0, 0, 40).fade(k));
        self.fb.fill_circle(cx, cy, kr, C4::rgb(255, 255, 255).fade(k));
        if show_focus(self, id) {
            focus_ring(self, t, 10.0);
        }
        clicked && self.enabled
    }

    /// Choice from `items`; the list opens below the box (above when it
    /// would leave the window). Returns whether `sel` changed.
    pub fn dropdown(&mut self, id: &str, items: &[&str], sel: &mut usize) -> bool {
        let wmax = items.iter().map(|s| tw(self, text_size(), s)).fold(0.0, f32::max);
        let r = self.alloc(Some((wmax + 12.0 + 16.0 + 10.0 + 12.0).max(120.0).ceil()), H);
        let id = id_of(id);
        let focused = self.focusable(id);
        let n = items.len();
        *sel = (*sel).min(n.saturating_sub(1));
        let old = *sel;
        let hov = self.hovered(r);

        // The list rect for this frame.
        let ih = self.px(ITEM_H).round();
        let pad = self.px(4.0).round();
        let lh = ih * n as f32 + 2.0 * pad;
        let below = r.y1() + self.px(4.0);
        let ly = if below + lh > self.fb.height() as f32 && r.y - self.px(4.0) - lh >= 0.0 {
            r.y - self.px(4.0) - lh
        } else {
            below
        };
        let lr = FRect { x: r.x, y: ly.round(), w: r.w, h: lh };
        let item_at = |p: crate::objects::Pt| {
            (contains(lr, p) && p.y >= lr.y + pad).then(|| (((p.y - lr.y - pad) / ih) as usize).min(n - 1))
        };

        if self.pressed_in(r) && n > 0 {
            // A press on the box toggles (Ui::new already closed it on a press outside the list).
            let was_open = self.closed == Some(id) || self.focus.open.is_some_and(|o| o.0 == id);
            self.focus.set(Some(id), false);
            self.focus.open = if was_open { None } else { Some((id, *sel, lr)) };
        }
        if let Some((oid, hover, _)) = self.focus.open
            && oid == id
        {
            let mut hover = hover;
            if let Some(i) = item_at(self.input.mouse) {
                hover = i;
            }
            let mut close = false;
            if let Some(p) = self.input.pressed
                && let Some(i) = item_at(p)
            {
                *sel = i;
                close = true;
            }
            for (i, vk, m) in self.keys() {
                if m.ctrl {
                    continue;
                }
                match vk {
                    key::UP => hover = hover.saturating_sub(1),
                    key::DOWN => hover = (hover + 1).min(n - 1),
                    key::HOME => hover = 0,
                    key::END => hover = n - 1,
                    key::RETURN | key::SPACE => {
                        *sel = hover;
                        close = true;
                    }
                    key::ESCAPE => close = true,
                    _ => continue,
                }
                self.consume(i);
            }
            if close {
                self.focus.open = None;
            } else {
                self.focus.open = Some((id, hover, lr));
                self.popup = Some(Popup { r: lr, items: items.iter().map(|s| s.to_string()).collect(), sel: *sel, hover });
            }
        } else if focused && self.enabled && n > 0 {
            for (i, vk, m) in self.keys() {
                match vk {
                    key::DOWN if m.alt => self.focus.open = Some((id, *sel, lr)),
                    key::RETURN | key::SPACE if !m.ctrl => self.focus.open = Some((id, *sel, lr)),
                    key::UP if !m.alt => *sel = sel.saturating_sub(1),
                    key::DOWN => *sel = (*sel + 1).min(n - 1),
                    key::HOME => *sel = 0,
                    key::END => *sel = n - 1,
                    _ => continue,
                }
                self.consume(i);
            }
            if self.focus.open.is_some() {
                self.redraw = true;
            }
        }

        let open = self.focus.open.is_some_and(|o| o.0 == id);
        field_frame(self, r, open, hov);
        if show_focus(self, id) && !open {
            focus_ring(self, r, RADIUS);
        }
        let k = fade(self);
        let (tc, ic) = (self.theme.text.fade(k), self.theme.icon.fade(k));
        let cy = r.y + r.h / 2.0;
        if let Some(s) = items.get(*sel) {
            text(self, text_size(), s, r.x + self.px(12.0), cy, tc);
        }
        let isz = self.px(16.0);
        let ix = r.x1() - self.px(10.0) - isz;
        draw_icon(&mut self.fb, "chevron", ix, cy - isz / 2.0, isz, ic);
        *sel != old
    }

    /// Integer slider over `min..=max` with the value printed at its right.
    /// Drag, click, arrows (±1), PageUp/PageDown (±10), Home/End.
    pub fn slider(&mut self, id: &str, v: &mut u8, min: u8, max: u8) -> bool {
        let r = self.alloc(None, H);
        let id = id_of(id);
        let focused = self.focusable(id);
        let old = *v;
        let (lo, hi) = (min.min(max), max.max(min));
        *v = (*v).clamp(lo, hi);
        let vw = self.px(tw(self, text_size(), &hi.to_string()) + 12.0).round();
        let kr = self.px(8.0);
        let (x0, x1) = (r.x + kr, r.x1() - vw - kr);
        let at = |x: f32| {
            let t = ((x - x0) / (x1 - x0).max(1.0)).clamp(0.0, 1.0);
            (lo as f32 + t * (hi - lo) as f32).round() as u8
        };
        let hov = self.hovered(r);
        if self.pressed_in(r) {
            self.focus.set(Some(id), false);
            self.focus.active = Some(id);
        }
        if self.focus.active == Some(id) {
            match self.input.released {
                Some(p) => *v = at(p.x),
                None if self.input.held => *v = at(self.input.mouse.x),
                None => {}
            }
        }
        if focused && self.enabled {
            for (i, vk, m) in self.keys() {
                if m.ctrl || m.alt {
                    continue;
                }
                let n = *v as i32;
                let to = match vk {
                    key::LEFT | key::DOWN => n - 1,
                    key::RIGHT | key::UP => n + 1,
                    key::PAGEDOWN => n - 10,
                    key::PAGEUP => n + 10,
                    key::HOME => lo as i32,
                    key::END => hi as i32,
                    _ => continue,
                };
                *v = to.clamp(lo as i32, hi as i32) as u8;
                self.consume(i);
            }
        }

        let th = self.theme;
        let k = fade(self);
        let cy = (r.y + r.h / 2.0).round();
        let t = (*v - lo) as f32 / (hi - lo).max(1) as f32;
        let kx = x0 + t * (x1 - x0);
        let lh = self.px(4.0).round();
        let track = FRect { x: x0 - kr / 2.0, y: cy - lh / 2.0, w: x1 - x0 + kr, h: lh };
        fill(self, track, 2.0, th.text_muted.fade(0.35 * k));
        fill(self, FRect { w: kx - track.x, ..track }, 2.0, th.accent.fade(k));
        let grab = self.focus.active == Some(id) && self.input.held;
        let kr2 = if grab || hov { kr + self.px(1.0) } else { kr };
        self.fb.fill_circle(kx, cy + self.px(1.0), kr2 + self.px(0.5), C4::new(0, 0, 0, 50).fade(k));
        self.fb.fill_circle(kx, cy, kr2, C4::rgb(255, 255, 255).fade(k));
        self.fb.stroke_circle(kx, cy, kr2 - self.px(0.5), self.px(1.0), th.accent.fade(0.6 * k));
        if show_focus(self, id) {
            let lw = self.px(2.0).round();
            self.fb.stroke_circle(kx, cy, kr2 + self.px(2.0) + lw / 2.0, lw, th.accent.fade(0.7));
        }
        let s = v.to_string();
        let sx = r.x1() - self.px(tw(self, text_size(), &s));
        text(self, text_size(), &s, sx, cy, th.text.fade(k));
        *v != old
    }

    /// Segmented tab bar; the current tab has the active-tool tint.
    /// Left/Right switch while focused. Returns whether `sel` changed.
    pub fn tabs(&mut self, id: &str, labels: &[&str], sel: &mut usize) -> bool {
        let pad = 4.0;
        let ws: Vec<f32> = labels.iter().map(|s| (tw(self, text_size(), s) + 2.0 * 14.0).ceil()).collect();
        let total = ws.iter().sum::<f32>() + 2.0 * pad;
        let r = self.alloc(Some(total), H + 2.0 * pad);
        let id = id_of(id);
        let focused = self.focusable(id);
        let old = *sel;
        let n = labels.len();
        let mut segs = Vec::with_capacity(n);
        let mut x = r.x + self.px(pad);
        for w in &ws {
            let w = self.px(*w).round();
            segs.push(FRect { x, y: r.y + self.px(pad), w, h: r.h - 2.0 * self.px(pad) });
            x += w;
        }
        if let Some(p) = self.input.pressed
            && self.pressed_in(r)
        {
            self.focus.set(Some(id), false);
            if let Some(i) = segs.iter().position(|s| contains(*s, p)) {
                *sel = i;
            }
        }
        if focused && self.enabled && n > 0 {
            for (i, vk, m) in self.keys() {
                if m.ctrl || m.alt {
                    continue;
                }
                match vk {
                    key::LEFT => *sel = sel.saturating_sub(1),
                    key::RIGHT => *sel = (*sel + 1).min(n - 1),
                    key::HOME => *sel = 0,
                    key::END => *sel = n - 1,
                    _ => continue,
                }
                self.consume(i);
            }
        }
        let th = self.theme;
        let k = fade(self);
        fill(self, r, RADIUS + pad, th.bg_hover.fade(k));
        for (i, s) in segs.iter().enumerate() {
            let active = i == *sel;
            let hov = self.hovered(*s);
            if active {
                fill(self, *s, RADIUS, if hov { th.accent_bg_hover } else { th.accent_bg }.fade(k));
                ring(self, *s, RADIUS, th.accent_ring.fade(k));
            } else if hov {
                fill(self, *s, RADIUS, th.bg_hover.fade(k));
            }
            let c = if active { th.accent_fg } else if hov { th.text } else { th.text_muted };
            let x = s.x + (s.w - self.px(tw(self, text_size(), labels[i]))) / 2.0;
            text(self, text_size(), labels[i], x, s.y + s.h / 2.0, c.fade(k));
        }
        if show_focus(self, id) {
            focus_ring(self, r, RADIUS + pad);
        }
        *sel != old
    }

    /// Scrolling table with a header row and single selection (click,
    /// Up/Down/Home/End/PageUp/PageDown while focused, wheel to scroll).
    /// Fills the width; height = `height(..)` or 8 rows. Returns whether
    /// the selection changed.
    pub fn table(&mut self, id: &str, cols: &[Col], rows: &[Row], st: &mut TableState) -> bool {
        let r = self.alloc(None, ITEM_H * 9.0);
        let id = id_of(id);
        let focused = self.focusable(id);
        let old = st.selected;
        let rh = self.px(ITEM_H).round();
        let head = FRect { h: rh, ..r };
        let body = FRect { x: r.x, y: r.y + rh, w: r.w, h: r.h - rh };
        let n = rows.len();
        let page = ((body.h / rh).floor() as usize).max(1);
        let max_scroll = (n as f32 * ITEM_H - body.h / self.k).max(0.0);

        if self.hovered(r) && self.input.wheel != 0 {
            st.scroll -= self.input.wheel as f32 / 120.0 * 3.0 * ITEM_H;
        }
        if let Some(p) = self.input.pressed
            && self.pressed_in(r)
        {
            self.focus.set(Some(id), false);
            if contains(body, p) {
                let i = ((p.y - body.y + st.scroll_px(rh)) / rh) as usize;
                if i < n {
                    st.selected = Some(i);
                }
            }
        }
        let mut moved = false;
        if focused && self.enabled && n > 0 {
            for (i, vk, m) in self.keys() {
                if m.ctrl || m.alt {
                    continue;
                }
                let cur = st.selected.map(|s| s as i64);
                let to = match (vk, cur) {
                    (key::UP, Some(c)) => c - 1,
                    (key::DOWN, Some(c)) => c + 1,
                    (key::UP | key::DOWN | key::HOME, None) | (key::HOME, _) => 0,
                    (key::END, _) => n as i64 - 1,
                    (key::PAGEUP, c) => c.unwrap_or(0) - page as i64,
                    (key::PAGEDOWN, c) => c.unwrap_or(0) + page as i64,
                    _ => continue,
                };
                st.selected = Some(to.clamp(0, n as i64 - 1) as usize);
                moved = true;
                self.consume(i);
            }
        }
        if moved && let Some(s) = st.selected {
            // Scroll the selection into view.
            let (top, bot) = (s as f32 * ITEM_H, (s + 1) as f32 * ITEM_H);
            let vh = body.h / self.k;
            if top < st.scroll {
                st.scroll = top;
            } else if bot > st.scroll + vh {
                st.scroll = bot - vh;
            }
        }
        st.scroll = st.scroll.clamp(0.0, max_scroll);
        st.selected = st.selected.filter(|&s| s < n);
        st.body = Some((body, rh));

        let th = self.theme;
        let k = fade(self);
        fill(self, r, RADIUS, th.bg_hover.fade(0.6 * k));
        // Column x positions.
        let pad = self.px(12.0);
        let mut xs = Vec::with_capacity(cols.len());
        let mut x = r.x;
        for (i, c) in cols.iter().enumerate() {
            xs.push(x);
            x += if i + 1 == cols.len() { 0.0 } else { (r.w * c.frac).round() };
        }
        let hy = head.y + head.h / 2.0;
        for (c, x) in cols.iter().zip(&xs) {
            text(self, 12.0, c.title, x + pad, hy, th.text_muted.fade(k));
        }
        self.fb.fill_rect(r.x as i32, body.y as i32 - 1, r.w as i32, self.k.round().max(1.0) as i32, th.separator.fade(k));
        let scroll = st.scroll_px(rh);
        let sel = st.selected;
        let hover_row = (self.hovered(body)).then(|| ((self.input.mouse.y - body.y + scroll) / rh) as usize);
        let font = &crate::fonts::UI;
        let px = self.px(text_size());
        let first = (scroll / rh) as usize;
        let ui_k = self.k;
        super::clipped(&mut self.fb, body, |fb| {
            for (i, row) in rows.iter().enumerate().skip(first).take(page + 2) {
                let y = body.y + i as f32 * rh - scroll;
                let rr = FRect { x: body.x + ui_k * 4.0, y: y + ui_k, w: body.w - ui_k * 8.0, h: rh - 2.0 * ui_k };
                if sel == Some(i) {
                    chrome::fill(fb, chrome::round(rr), ui_k * 6.0, th.accent_bg.fade(k));
                } else if hover_row == Some(i) {
                    chrome::fill(fb, chrome::round(rr), ui_k * 6.0, th.bg_hover.fade(k));
                }
                let c = if row.error { th.error } else { th.text };
                let cy = y + rh / 2.0;
                for (j, cell) in row.cells.iter().enumerate() {
                    let Some(&cx) = xs.get(j) else { break };
                    let ty = (cy - text_height(font, px) / 2.0).round();
                    fb.draw_text(font, px, cell, (cx + pad).round(), ty, c.fade(k));
                }
            }
        });
        if max_scroll > 0.0 {
            // Scroll thumb.
            let total = n as f32 * rh;
            let th_h = (body.h * body.h / total).max(self.px(20.0));
            let ty = body.y + (body.h - th_h) * (st.scroll / max_scroll);
            let w = self.px(4.0);
            let tr = FRect { x: r.x1() - w - self.px(3.0), y: ty + self.px(2.0), w, h: th_h - self.px(4.0) };
            fill(self, tr, 2.0, th.text_muted.fade(0.5 * k));
        }
        ring(self, r, RADIUS, th.border.fade(k));
        if show_focus(self, id) {
            focus_ring(self, r, RADIUS);
        }
        st.selected != old
    }

    /// Determinate progress bar (`fraction` 0..=1), full width.
    pub fn progress(&mut self, fraction: f32) {
        let r = self.alloc(None, 6.0);
        let th = self.theme;
        fill(self, r, 3.0, th.text_muted.fade(0.3));
        let f = fraction.clamp(0.0, 1.0);
        if f > 0.0 {
            let w = (r.w * f).max(r.h).round();
            fill(self, FRect { w, ..r }, 3.0, th.accent);
        }
    }

    /// Shortcut box: click (or Enter/Space) to record, then press a chord.
    /// Backspace clears, Tab moves on, a click elsewhere stops recording;
    /// modifier keys alone wait for the key. Esc is recorded like any key.
    /// Returns whether `chord` changed.
    pub fn key_capture(&mut self, id: &str, chord: &mut Option<Chord>) -> bool {
        let r = self.alloc(Some(140.0), H);
        let id = id_of(id);
        let focused = self.focusable(id);
        let old = *chord;
        let hov = self.hovered(r);
        if self.pressed_in(r) {
            self.focus.set(Some(id), false);
            self.focus.recording = Some(id);
        } else if self.input.pressed.is_some() && self.focus.recording == Some(id) {
            self.focus.recording = None;
        }
        let mut recording = self.focus.recording == Some(id);
        if focused && self.enabled {
            for (i, vk, m) in self.keys() {
                if !recording {
                    if matches!(vk, key::RETURN | key::SPACE) && !m.ctrl && !m.alt {
                        recording = true;
                        self.consume(i);
                    }
                    continue;
                }
                self.consume(i);
                if is_modifier(vk) {
                    continue;
                }
                if vk == key::BACK && !m.ctrl && !m.alt && !m.shift {
                    *chord = None;
                    recording = false;
                } else if hotkey::key_name(vk).is_some() {
                    *chord = Some(Chord { vk, ctrl: m.ctrl, shift: m.shift, alt: m.alt, meta: false });
                    recording = false;
                }
            }
            self.focus.recording = recording.then_some(id);
        }

        field_frame(self, r, recording, hov);
        if show_focus(self, id) && !recording {
            focus_ring(self, r, RADIUS);
        }
        let th = self.theme;
        let k = fade(self);
        let cy = r.y + r.h / 2.0;
        let x0 = r.x + self.px(10.0);
        match (recording, chord.as_ref()) {
            (true, _) => {
                text(self, text_size(), "Press a shortcut…", x0, cy, th.accent_fg.fade(k));
            }
            (false, None) => {
                text(self, text_size(), "None", x0, cy, th.text_muted.fade(k));
            }
            (false, Some(c)) => {
                let mut x = x0;
                for cap in chord_caps(c) {
                    let w = self.px(tw(self, 12.0, &cap) + 12.0).max(self.px(22.0)).round();
                    let h = self.px(22.0).round();
                    let cr = FRect { x, y: (cy - h / 2.0).round(), w, h };
                    fill(self, cr, 5.0, th.bg_pressed.fade(k));
                    ring(self, cr, 5.0, th.border.fade(k));
                    let tx = x + (w - self.px(tw(self, 12.0, &cap))) / 2.0;
                    text(self, 12.0, &cap, tx, cy, th.text.fade(k));
                    x += w + self.px(4.0);
                }
            }
        }
        *chord != old
    }
}

fn is_modifier(vk: u32) -> bool {
    matches!(vk, 0x10..=0x12 | 0x5B | 0x5C | 0xA0..=0xA5 | 0x14)
}

/// Linear mix of two opaque colours (`t` of `b`).
fn mix(a: C4, b: C4, t: f32) -> C4 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    C4::new(l(a.r, b.r), l(a.g, b.g), l(a.b, b.b), a.a)
}

/// The open dropdown list (drawn last, over everything): a popover
/// surface with the selected item checked and the highlighted one tinted.
pub(super) fn draw_popup(ui: &mut Ui, items: &[String], r: FRect, sel: usize, hover: usize) {
    let cu = ui.chrome();
    chrome::surface(&mut ui.fb, &cu, r, 10.0, &ALL_LAYERS, 1.0);
    let th = ui.theme;
    let ih = ui.px(ITEM_H).round();
    let pad = ui.px(4.0).round();
    for (i, s) in items.iter().enumerate() {
        let ir = FRect { x: r.x + pad, y: r.y + pad + i as f32 * ih, w: r.w - 2.0 * pad, h: ih };
        if i == hover {
            fill(ui, ir, 6.0, th.bg_hover);
        }
        let cy = ir.y + ih / 2.0;
        let c = if i == sel { th.accent_fg } else { th.text };
        text(ui, text_size(), s, ir.x + ui.px(8.0), cy, c);
        if i == sel {
            let isz = ui.px(16.0);
            let ix = ir.x1() - ui.px(8.0) - isz;
            draw_icon(&mut ui.fb, "check", ix, cy - isz / 2.0, isz, th.accent_fg);
        }
    }
}
