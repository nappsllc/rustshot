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
    // Recessed field fill (unlike the raised bg_hover of secondary buttons);
    // hover only strengthens the border.
    fill(ui, r, RADIUS, th.field_bg.fade(k));
    if hovered && !focused && ui.enabled {
        ring(ui, r, RADIUS, th.text_muted.fade(0.45));
    } else if focused {
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

    /// Scroll so row `i` is in view in a table `h` logical px tall
    /// (header included), as [`Ui::table`] will draw it.
    pub fn reveal(&mut self, i: usize, h: f32) {
        let (top, bot) = (i as f32 * ITEM_H, (i + 1) as f32 * ITEM_H);
        let vh = h - ITEM_H;
        if top < self.scroll || vh <= 0.0 {
            self.scroll = top;
        } else if bot > self.scroll + vh {
            self.scroll = bot - vh;
        }
    }

    fn scroll_px(&self, rh: f32) -> f32 {
        self.scroll * rh / ITEM_H
    }
}

impl Ui<'_> {
    /// One line of body text.
    pub fn label(&mut self, s: &str) {
        let c = self.theme.text;
        self.text_line(s, text_size(), c);
    }

    /// Secondary text: muted and slightly smaller.
    pub fn note(&mut self, s: &str) {
        let c = self.theme.text_muted;
        self.text_line(s, 12.0, c);
    }

    /// A [`Ui::note`] in the error colour (conflicts, invalid input).
    pub fn error_note(&mut self, s: &str) {
        let c = self.theme.error;
        self.text_line(s, 12.0, c);
    }

    /// Section heading; below other controls it gets extra space above.
    pub fn heading(&mut self, s: &str) {
        if !self.lay.in_row() && self.cursor_y() > self.bounds().y + 0.5 {
            self.space(8.0);
        }
        let c = self.theme.text;
        self.text_line(s, 15.0, c);
    }

    fn text_line(&mut self, s: &str, size: f32, c: C4) {
        let w = tw(self, size, s).ceil();
        let h = if self.lay.in_row() { H } else { (size * 1.6).round() };
        let r = self.alloc(Some(w), h);
        let c = c.fade(fade(self));
        text(self, size, s, r.x, r.y + r.h / 2.0, c);
    }

    /// Text button styled as a link: accent text, underlined on hover.
    /// Clicked like a button (release over it, Enter/Space while focused).
    pub fn link(&mut self, id: &str, label: &str) -> bool {
        let w = tw(self, text_size(), label).ceil();
        let r = self.alloc(Some(w), H);
        let id = id_of(id);
        let focused = self.focusable(id);
        let (hov, _, mut clicked) = press_logic(self, id, r);
        clicked |= self.take_key(focused, &[key::RETURN, key::SPACE]);
        let c = self.theme.accent_fg.fade(fade(self));
        let cy = r.y + r.h / 2.0;
        text(self, text_size(), label, r.x, cy, c);
        if hov {
            let lw = self.k.round().max(1.0);
            let y = (cy + self.px(8.0)).round();
            self.fb.fill_rect(r.x as i32, y as i32, r.w as i32, lw as i32, c);
        }
        if show_focus(self, id) {
            let fr = FRect { x: r.x - self.px(4.0), y: cy - self.px(11.0), w: r.w + self.px(8.0), h: self.px(22.0) };
            focus_ring(self, fr, 4.0);
        }
        clicked && self.enabled
    }

    /// Dim everything drawn so far (a modal question is up) and draw a
    /// raised card at `r` (logical px) to lay the question out in.
    pub fn modal_card(&mut self, r: FRect) {
        let (w, h) = (self.fb.stride as i32, self.fb.height());
        let a = if self.theme.dark { 120 } else { 70 };
        self.fb.fill_rect(self.fb.ox, self.fb.oy, w, h, C4::new(0, 0, 0, a));
        let k = self.k;
        let pr = FRect { x: (r.x * k).round(), y: (r.y * k).round(), w: (r.w * k).round(), h: (r.h * k).round() };
        let cu = self.chrome();
        let layers: &[usize] = if self.theme.dark { &ALL_LAYERS } else { &[0, 1] };
        chrome::surface(&mut self.fb, &cu, pr, 12.0, layers, 1.0);
    }

    /// Width `tabs` takes for `labels` (logical px), for centring it.
    pub fn tabs_width(&self, labels: &[&str]) -> f32 {
        labels.iter().map(|s| (tw(self, text_size(), s) + 2.0 * 14.0).ceil()).sum::<f32>() + 2.0 * 4.0
    }

    /// Push button; `primary` is the accent-filled default action.
    /// Clicked by mouse release over it or Enter/Space while focused.
    pub fn button(&mut self, id: &str, label: &str, primary: bool) -> bool {
        let w = self.button_width(label);
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
            (n > 0 && contains(lr, p) && p.y >= lr.y + pad).then(|| (((p.y - lr.y - pad) / ih) as usize).min(n - 1))
        };
        if n == 0 && self.focus.open.is_some_and(|o| o.0 == id) {
            self.focus.open = None; // nothing to choose from
        }

        if self.pressed_in(r) && n > 0 {
            // A press on the box toggles (Ui::new already closed it on a press outside the list).
            let was_open = self.closed == Some(id) || self.focus.open.is_some_and(|o| o.0 == id);
            self.focus.set(Some(id), false);
            self.focus.open = if was_open { None } else { Some((id, *sel, lr)) };
        }
        if let Some((oid, hover, _)) = self.focus.open
            && oid == id
        {
            let mut hover = hover.min(n - 1);
            if self.input.moved
                && let Some(i) = item_at(self.input.mouse)
            {
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
        // Disabled: no accent at all (a faded accent still reads as "on",
        // most of all on the light theme), a flat knob without shadow.
        let on = if self.enabled { th.accent } else { th.text_muted };
        fill(self, FRect { w: kx - track.x, ..track }, 2.0, on.fade(k));
        let grab = self.focus.active == Some(id) && self.input.held;
        let kr2 = if grab || hov { kr + self.px(1.0) } else { kr };
        if self.enabled {
            self.fb.fill_circle(kx, cy + self.px(1.0), kr2 + self.px(0.5), C4::new(0, 0, 0, 50));
        }
        let knob = if self.enabled { C4::rgb(255, 255, 255) } else { mix(th.surface.with_alpha(255), th.text_muted.with_alpha(255), 0.25) };
        self.fb.fill_circle(kx, cy, kr2, knob);
        self.fb.stroke_circle(kx, cy, kr2 - self.px(0.5), self.px(1.0), on.fade(0.6 * k));
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
        // `scroll` counts ITEM_H per row; rows are drawn `rh` physical px
        // tall, so the viewport spans body.h / rh rows.
        let to_scroll = ITEM_H / rh;
        let max_scroll = ((n as f32 * rh - body.h) * to_scroll).max(0.0);

        if self.hovered(r) && self.input.wheel != 0 {
            st.scroll -= self.input.wheel as f32 / 120.0 * 3.0 * ITEM_H;
        }
        // Clamp before hit-testing so a click lands on the row it shows.
        st.scroll = st.scroll.clamp(0.0, max_scroll);
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
            let vh = body.h * to_scroll;
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
        // Rows in a conflict: red text and an alert icon after the first cell.
        let isz = self.px(14.0).round();
        let gap = self.px(6.0);
        let first_w: Vec<f32> = rows.iter().map(|r| if r.error { text_width(font, px, r.cells.first().copied().unwrap_or("")) } else { 0.0 }).collect();
        // Highlights stop short of the scroll thumb (4 px + 3 px margin).
        let right = if max_scroll > 0.0 { 4.0 + 4.0 + 3.0 } else { 4.0 };
        super::clipped(&mut self.fb, body, |fb| {
            for (i, row) in rows.iter().enumerate().skip(first).take(page + 2) {
                let y = body.y + i as f32 * rh - scroll;
                let rr = FRect { x: body.x + ui_k * 4.0, y: y + ui_k, w: body.w - ui_k * (4.0 + right), h: rh - 2.0 * ui_k };
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
                if row.error {
                    let ix = (xs[0] + pad + first_w[i] + gap).round();
                    draw_icon(fb, "triangle-alert", ix, (cy - isz / 2.0).round(), isz, th.error.fade(k));
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
        let r = self.alloc(Some(200.0), H);
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
                    let meta = self.allow_meta && crate::wind::meta_down();
                    *chord = Some(Chord { vk, ctrl: m.ctrl, shift: m.shift, alt: m.alt, meta });
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
    // The full stack reads as a grey halo on the light surface: only the
    // contact and near layers there.
    let layers: &[usize] = if ui.theme.dark { &ALL_LAYERS } else { &[0, 1] };
    chrome::surface(&mut ui.fb, &cu, r, 10.0, layers, 1.0);
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

/// Greedy word wrap of `text` to `max_w` (the unit of `measure`). Every
/// `\n` starts a new line (empty source lines are kept); words are split
/// at spaces, and a word wider than a whole line is broken between
/// characters.
pub fn wrap(text: &str, max_w: f32, measure: impl Fn(&str) -> f32) -> Vec<String> {
    let mut out = Vec::new();
    for src in text.split('\n') {
        let src = src.trim_end_matches('\r');
        let mut line = String::new();
        for word in src.split(' ').filter(|w| !w.is_empty()) {
            let joined = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
            if measure(&joined) <= max_w {
                line = joined;
                continue;
            }
            if !line.is_empty() {
                out.push(std::mem::take(&mut line));
            }
            // The word alone: break it if even that is too wide.
            for ch in word.chars() {
                line.push(ch);
                if measure(&line) > max_w && line.chars().count() > 1 {
                    line.pop();
                    out.push(std::mem::replace(&mut line, ch.to_string()));
                }
            }
        }
        out.push(line);
    }
    out
}

/// Line pitch of wrapped body text (logical px).
const LINE_H: f32 = 20.0;
/// Vertical padding inside a text view, each side (logical px).
const VIEW_PAD_Y: f32 = 8.0;

/// The tallest text-view height up to `h` whose body shows whole lines
/// only (at least `min_lines`), logical px.
pub fn text_view_fit(h: f32, min_lines: usize) -> f32 {
    let n = ((h - 2.0 * VIEW_PAD_Y) / LINE_H).floor().max(min_lines as f32);
    n * LINE_H + 2.0 * VIEW_PAD_Y
}

impl Ui<'_> {
    /// Wrapped text filling the width; `muted` for secondary text. Lines
    /// past the height given by `height(..)` (default: all of them) are
    /// not drawn.
    pub fn paragraph(&mut self, s: &str, muted: bool) {
        let w = self.bounds().w;
        let lines = wrap(s, w, |t| tw(self, text_size(), t));
        let r = self.alloc(None, lines.len() as f32 * LINE_H);
        let c = if muted { self.theme.text_muted } else { self.theme.text };
        let c = c.fade(fade(self));
        let lh = self.px(LINE_H);
        let n = ((r.h + 0.5) / lh).floor().max(0.0) as usize;
        for (i, l) in lines.iter().take(n).enumerate() {
            text(self, text_size(), l, r.x, r.y + (i as f32 + 0.5) * lh, c);
        }
    }

    /// Width of `s` as body text (logical px).
    pub fn text_width(&self, s: &str) -> f32 {
        tw(self, text_size(), s)
    }

    /// Height `paragraph` takes for `s` at width `w` (logical px).
    pub fn paragraph_height(&self, s: &str, w: f32) -> f32 {
        wrap(s, w, |t| tw(self, text_size(), t)).len() as f32 * LINE_H
    }

    /// Bordered, read-only box of wrapped text (release notes) that
    /// scrolls by wheel, and by Up/Down/PageUp/PageDown/Home/End while
    /// focused. Fills the width; height = `height(..)` or 120. `scroll` is
    /// the offset in logical px, clamped here.
    #[cfg(test)]
    pub fn text_view(&mut self, id: &str, s: &str, scroll: &mut f32) {
        let src: Vec<(&str, bool)> = s.split('\n').map(|l| (l, false)).collect();
        self.text_view_styled(id, &src, scroll);
    }

    /// [`Ui::text_view`] over source lines, each `(text, muted)`: muted
    /// lines use the secondary text colour (body text under headings).
    pub fn text_view_styled(&mut self, id: &str, src: &[(&str, bool)], scroll: &mut f32) {
        let r = self.alloc(None, 120.0);
        let id = id_of(id);
        let focused = self.focusable(id);
        let pad = 12.0;
        let inner_w = r.w / self.k - 2.0 * pad;
        let wrap_all = |ui: &Self, w: f32| -> Vec<(String, bool)> {
            (src.iter())
                .flat_map(|&(l, m)| wrap(l, w, |t| tw(ui, text_size(), t)).into_iter().map(move |l| (l, m)))
                .collect()
        };
        let mut lines = wrap_all(self, inner_w);
        let view_h = r.h / self.k - 2.0 * VIEW_PAD_Y;
        let mut max = (lines.len() as f32 * LINE_H - view_h).max(0.0);
        if max > 0.0 {
            // Leave room for the scroll thumb.
            lines = wrap_all(self, inner_w - 8.0);
            max = (lines.len() as f32 * LINE_H - view_h).max(0.0);
        }
        if self.pressed_in(r) {
            self.focus.set(Some(id), false);
        }
        if self.hovered(r) && self.input.wheel != 0 {
            *scroll -= self.input.wheel as f32 / 120.0 * 3.0 * LINE_H;
        }
        if focused && self.enabled {
            for (i, vk, m) in self.keys() {
                if m.ctrl || m.alt {
                    continue;
                }
                *scroll = match vk {
                    key::UP => *scroll - LINE_H,
                    key::DOWN => *scroll + LINE_H,
                    key::PAGEUP => *scroll - view_h,
                    key::PAGEDOWN => *scroll + view_h,
                    key::HOME => 0.0,
                    key::END => max,
                    _ => continue,
                };
                self.consume(i);
            }
        }
        *scroll = scroll.clamp(0.0, max);

        let th = self.theme;
        let k = fade(self);
        fill(self, r, RADIUS, th.bg_hover.fade(0.6 * k));
        let body = FRect { x: r.x, y: r.y + self.px(VIEW_PAD_Y), w: r.w, h: r.h - self.px(2.0 * VIEW_PAD_Y) };
        let font = &crate::fonts::UI;
        let px = self.px(text_size());
        let lh = self.px(LINE_H);
        let off = self.px(*scroll);
        let x = (r.x + self.px(pad)).round();
        let (c, cm) = (th.text.fade(k), th.text_muted.fade(k));
        let first = (off / lh).floor() as usize;
        let shown = (body.h / lh).ceil() as usize + 1;
        super::clipped(&mut self.fb, body, |fb| {
            for (i, (l, m)) in lines.iter().enumerate().skip(first).take(shown) {
                let cy = body.y + (i as f32 + 0.5) * lh - off;
                fb.draw_text(font, px, l, x, (cy - text_height(font, px) / 2.0).round(), if *m { cm } else { c });
            }
        });
        if max > 0.0 {
            let total = lines.len() as f32 * lh;
            let th_h = (body.h * body.h / total).max(self.px(20.0));
            let ty = body.y + (body.h - th_h) * (*scroll / max);
            let w = self.px(4.0);
            let tr = FRect { x: r.x1() - w - self.px(3.0), y: ty, w, h: th_h };
            fill(self, tr, 2.0, th.text_muted.fade(0.5 * k));
        }
        ring(self, r, RADIUS, th.border.fade(k));
        if show_focus(self, id) {
            focus_ring(self, r, RADIUS);
        }
    }

    /// A Lucide icon (`icon_path` name) in a `size` box, vertically centred
    /// in a row.
    pub fn icon(&mut self, name: &str, size: f32, c: C4) {
        let h = if self.lay.in_row() { H } else { size };
        let r = self.alloc(Some(size), h);
        let s = self.px(size);
        let c = c.fade(fade(self));
        draw_icon(&mut self.fb, name, r.x, (r.y + (r.h - s) / 2.0).round(), s, c);
    }

    /// Width `button` takes for `label` (logical px), for laying out a
    /// right-aligned button row.
    pub fn button_width(&self, label: &str) -> f32 {
        (tw(self, text_size(), label) + 2.0 * 16.0).max(84.0).ceil()
    }
}
