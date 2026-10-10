//! Overlay chrome drawing: surfaces, toolbar and palette, selection and
//! handles, size label, toast, tooltip and the empty-state hint.

use super::Tool;
use super::toolbar::{label_rect, Act, Item, Kind, Toolbar};
use crate::icon_path::draw_icon;
use crate::keymap::{Chord, Keymap};
use crate::objects::FRect;
use crate::theme::{rgba, Theme, SHADOW};
use crate::uifb::{text_height, text_width, Fb, C4};
use crate::fonts::UiFont;

/// Tokens, DPI scale and UI font: what every chrome call needs.
pub struct Ui<'a> {
    pub th: &'a Theme,
    pub s: f32,
    pub font: Option<&'a UiFont>,
}

impl Ui<'_> {
    fn px(&self, v: f32) -> f32 {
        v * self.s
    }

    /// A crisp "1 px" line at this scale.
    fn line(&self) -> f32 {
        self.s.round().max(1.0)
    }
}

pub const ALL_LAYERS: [usize; 4] = [0, 1, 2, 3];
pub const SMALL_LAYERS: [usize; 2] = [0, 2];

fn round(r: FRect) -> FRect {
    FRect { x: r.x.round(), y: r.y.round(), w: r.w.round(), h: r.h.round() }
}

fn fill(f: &mut Fb, r: FRect, radius: f32, c: C4) {
    f.fill_rounded(r.x as i32, r.y as i32, r.w as i32, r.h as i32, radius, c);
}

/// Inner 1 px ring along the edge of `r`.
fn ring(f: &mut Fb, ui: &Ui, r: FRect, radius: f32, c: C4) {
    let lw = ui.line();
    let inset = FRect { x: r.x + lw / 2.0, y: r.y + lw / 2.0, w: r.w - lw, h: r.h - lw };
    f.stroke_rounded(inset, radius - lw / 2.0, lw, c);
}

#[allow(clippy::too_many_arguments)]
fn panel(f: &mut Fb, ui: &Ui, r: FRect, radius: f32, layers: &[usize], bg: C4, border: C4, k: f32) {
    let r = round(r);
    let rad = ui.px(radius);
    for &i in layers.iter().rev() {
        let (dy, sp) = (ui.px(SHADOW[i].0), ui.px(SHADOW[i].1));
        let sr = FRect { x: r.x - sp, y: r.y + dy - sp, w: r.w + 2.0 * sp, h: r.h + 2.0 * sp };
        fill(f, round(sr), rad + sp, ui.th.shadow[i].fade(k));
    }
    fill(f, r, rad, bg.fade(k));
    ring(f, ui, r, rad, border.fade(k));
}

/// Shadow layers (SHADOW indices) + surface fill + 1 px inner border.
pub fn surface(f: &mut Fb, ui: &Ui, r: FRect, radius: f32, layers: &[usize], k: f32) {
    panel(f, ui, r, radius, layers, ui.th.surface, ui.th.border, k);
}

/// Draw `s` vertically centred on `cy`; returns the advance width.
fn text(f: &mut Fb, ui: &Ui, size: f32, s: &str, x: f32, cy: f32, c: C4) -> f32 {
    let Some(font) = ui.font else { return 0.0 };
    let px = ui.px(size);
    f.draw_text(font, px, s, x, cy - text_height(font, px) / 2.0, c)
}

fn tw(ui: &Ui, size: f32, s: &str) -> f32 {
    ui.font.map(|f| text_width(f, ui.px(size), s)).unwrap_or(0.0)
}

pub fn act_icon(a: Act) -> &'static str {
    match a {
        Act::Tool(t) => match t {
            Tool::Path => "pencil",
            Tool::Line => "line",
            Tool::Arrow => "arrow",
            Tool::Rect => "rect",
            Tool::Ellipse => "circle",
            Tool::Marker => "marker",
            Tool::Text => "text",
            Tool::Pixelate => "pixel",
            Tool::Invert => "invert",
        },
        Act::Undo => "undo",
        Act::Redo => "redo",
        Act::Size(d) if d < 0 => "minus",
        Act::Size(_) => "plus",
        Act::Copy => "copy",
        Act::Save | Act::SaveAs => "save",
        Act::Upload => "cloud",
        Act::Exit => "x",
        Act::Accept => "check",
        Act::Palette | Act::Color(_) => "",
    }
}

pub struct BarState<'a> {
    pub hover: Option<usize>,
    /// 0..1 fade of the hovered button's background.
    pub hover_k: f32,
    pub pressed: Option<usize>,
    pub tool: Option<Tool>,
    pub color: C4,
    pub value: &'a str,
    pub unit: &'a str,
    /// 0..1 opacity of the palette popover.
    pub pop_k: f32,
}

pub fn toolbar(f: &mut Fb, ui: &Ui, tb: &Toolbar, st: &BarState, k: f32) {
    surface(f, ui, tb.bar, 12.0, &ALL_LAYERS, k);
    if let Some(pop) = tb.pop {
        surface(f, ui, pop, 12.0, &ALL_LAYERS, k * st.pop_k);
    }
    for (i, it) in tb.items.iter().enumerate() {
        let hovered = st.hover == Some(i) && !it.disabled;
        let pressed = st.pressed == Some(i) && !it.disabled;
        match it.kind {
            Kind::Btn(act) => button(f, ui, it, act, (hovered, pressed), st, k),
            Kind::Value => value(f, ui, it.r, st, k),
            Kind::SepV | Kind::SepH => {
                let r = round(it.r);
                f.fill_rect(r.x as i32, r.y as i32, r.w.max(1.0) as i32, r.h.max(1.0) as i32, ui.th.separator.fade(k));
            }
            Kind::Dot(c) => dot(f, ui, it.r, c, (c == st.color, hovered), k * st.pop_k),
        }
    }
}

fn button(f: &mut Fb, ui: &Ui, it: &Item, act: Act, (hovered, pressed): (bool, bool), st: &BarState, k: f32) {
    let th = ui.th;
    let r = round(it.r);
    let active = matches!(act, Act::Tool(t) if st.tool == Some(t));
    let danger = act == Act::Exit;
    let k = if it.disabled { k * 0.34 } else { k };
    let rad = ui.px(8.0);
    let bg = if active {
        Some(if hovered { th.accent_bg_hover } else { th.accent_bg })
    } else if pressed {
        Some(th.bg_pressed)
    } else if hovered {
        Some(if danger { th.danger_bg } else { th.bg_hover }.fade(st.hover_k))
    } else {
        None
    };
    if let Some(bg) = bg {
        fill(f, r, rad, bg.fade(k));
    }
    if active {
        ring(f, ui, r, rad, th.accent_ring.fade(k));
    }
    let fg = if active {
        th.accent_fg
    } else if danger && hovered {
        th.danger_fg
    } else if hovered || pressed {
        th.icon_hover
    } else {
        th.icon
    }
    .fade(k);
    let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
    match act {
        Act::Palette => swatch(f, ui, cx, cy, st.color, k),
        Act::Accept => {
            let (icon, gap) = (ui.px(20.0), ui.px(6.0));
            let x0 = cx - (icon + gap + tw(ui, 12.0, "OK")) / 2.0;
            draw_icon(f, "check", x0, cy - icon / 2.0, icon, fg);
            text(f, ui, 12.0, "OK", x0 + icon + gap, cy, fg);
        }
        _ => {
            let icon = ui.px(if pressed { 19.2 } else { 20.0 });
            draw_icon(f, act_icon(act), cx - icon / 2.0, cy - icon / 2.0, icon, fg);
        }
    }
}

fn value(f: &mut Fb, ui: &Ui, r: FRect, st: &BarState, k: f32) {
    let gap = ui.px(4.0);
    let (vw, uw) = (tw(ui, 12.0, st.value), tw(ui, 11.0, st.unit));
    let x = r.x + (r.w - vw - gap - uw) / 2.0;
    let cy = r.y + r.h / 2.0;
    text(f, ui, 12.0, st.value, x, cy, ui.th.text.fade(k));
    text(f, ui, 11.0, st.unit, x + vw + gap, cy, ui.th.text_muted.fade(k));
}

fn swatch(f: &mut Fb, ui: &Ui, cx: f32, cy: f32, c: C4, k: f32) {
    let rad = ui.px(9.0);
    let lw = ui.line();
    f.fill_circle(cx, cy, rad, c.fade(k));
    f.stroke_circle(cx, cy, rad - lw / 2.0, lw, ui.th.rim.fade(k));
}

fn dot(f: &mut Fb, ui: &Ui, r: FRect, c: C4, (selected, hovered): (bool, bool), k: f32) {
    let r = round(r);
    if selected || hovered {
        fill(f, r, ui.px(7.0), ui.th.bg_hover.fade(k));
    }
    if selected {
        ring(f, ui, r, ui.px(7.0), ui.th.accent_ring.fade(k));
    }
    swatch(f, ui, r.x + r.w / 2.0, r.y + r.h / 2.0, c, k);
}

/// Accent border with dark (or light) outer/inner lines, plus 8 handles:
/// white 6 ⌀ dot, 2 px accent ring (3 when hot), 1 px halo.
pub fn selection(f: &mut Fb, ui: &Ui, sr: FRect, hot: Option<usize>, k: f32) {
    let sr = round(sr);
    let th = ui.th;
    let lw = ui.line();
    let grow = |d: f32| FRect { x: sr.x - d, y: sr.y - d, w: sr.w + 2.0 * d, h: sr.h + 2.0 * d };
    for (d, c) in [(1.5, th.sel_outer), (0.5, th.accent), (-0.5, th.sel_inner)] {
        let r = grow(lw * d);
        f.stroke_rect(r.x, r.y, r.w, r.h, lw, c.fade(k));
    }
    for (n, (_, p)) in super::handle_points(sr).iter().enumerate() {
        let ring_w = if hot == Some(n) { 3.0 } else { 2.0 };
        f.fill_circle(p.x, p.y, ui.px(3.0 + ring_w + 1.0), th.handle_halo.fade(k));
        f.fill_circle(p.x, p.y, ui.px(3.0 + ring_w), th.accent.fade(k));
        f.fill_circle(p.x, p.y, ui.px(3.0), C4::rgb(255, 255, 255).fade(k));
    }
}

fn label_texts(sr: FRect) -> (String, String) {
    let size = format!("{} × {}", sr.w.round() as i32, sr.h.round() as i32);
    let pos = format!("{}, {}", sr.x.round() as i32, sr.y.round() as i32);
    (size, pos)
}

/// Where `size_label` puts its panel.
fn size_label_rect(ui: &Ui, sr: FRect, show_pos: bool, area: FRect, avoid: Option<FRect>) -> FRect {
    let (size, pos) = label_texts(sr);
    let pad = ui.px(8.0);
    let mut w = 2.0 * pad + tw(ui, 12.0, &size);
    if show_pos {
        w += pad + tw(ui, 11.0, &pos);
    }
    label_rect(sr, w, ui.s, area, avoid)
}

/// "W × H" (and "x, y" when not dragging) above the selection.
pub fn size_label(f: &mut Fb, ui: &Ui, sr: FRect, show_pos: bool, area: FRect, avoid: Option<FRect>, k: f32) {
    let (size, pos) = label_texts(sr);
    let pad = ui.px(8.0);
    let r = size_label_rect(ui, sr, show_pos, area, avoid);
    surface(f, ui, r, 6.0, &SMALL_LAYERS, k);
    let cy = r.y + r.h / 2.0;
    let x = r.x + pad + text(f, ui, 12.0, &size, r.x + pad, cy, ui.th.text.fade(k));
    if show_pos {
        text(f, ui, 11.0, &pos, x + pad, cy, ui.th.text_muted.fade(k));
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToastKind {
    Info,
    Success,
    Error,
}

/// The toast panel at opacity `k` (it rises 4 px while fading in).
fn toast_rect(ui: &Ui, msg: &str, area: FRect, k: f32) -> FRect {
    let (h, isz, gap, lpad) = (ui.px(36.0), ui.px(16.0), ui.px(8.0), ui.px(10.0));
    let w = (lpad + isz + gap + tw(ui, 12.0, msg) + ui.px(14.0)).min(area.w - ui.px(24.0));
    let rise = ui.px(4.0) * (1.0 - k);
    FRect { x: area.x + (area.w - w) / 2.0, y: area.y1() - ui.px(28.0) - h + rise, w, h }
}

/// Bottom-centre notice; `k` drives both fade and the 4 px rise.
pub fn toast(f: &mut Fb, ui: &Ui, msg: &str, kind: ToastKind, area: FRect, k: f32) {
    let th = ui.th;
    let (icon, color) = match kind {
        ToastKind::Info => ("info", th.accent),
        ToastKind::Success => ("okc", th.success),
        ToastKind::Error => ("alert", th.error),
    };
    let (h, isz, gap, lpad) = (ui.px(36.0), ui.px(16.0), ui.px(8.0), ui.px(10.0));
    let r = toast_rect(ui, msg, area, k);
    surface(f, ui, r, 12.0, &ALL_LAYERS, k);
    let cy = r.y + h / 2.0;
    draw_icon(f, icon, r.x + lpad, cy - isz / 2.0, isz, color.fade(k));
    text(f, ui, 12.0, msg, r.x + lpad + isz + gap, cy, th.text.fade(k));
}

fn key_cap(f: &mut Fb, ui: &Ui, x: f32, cy: f32, w: f32, s: &str, k: f32) {
    let h = ui.px(18.0);
    let r = round(FRect { x, y: cy - h / 2.0, w, h });
    fill(f, r, ui.px(4.0), ui.th.key_bg.fade(k));
    text(f, ui, 11.0, s, x + (w - tw(ui, 11.0, s)) / 2.0, cy, ui.th.key_text.fade(k));
}

fn key_w(ui: &Ui, s: &str) -> f32 {
    (tw(ui, 11.0, s) + ui.px(10.0)).max(ui.px(18.0))
}

#[cfg(target_os = "macos")]
const MOD: &str = "⌘";
#[cfg(not(target_os = "macos"))]
const MOD: &str = "Ctrl";

#[cfg(target_os = "macos")]
const ALT: &str = "⌥";
#[cfg(not(target_os = "macos"))]
const ALT: &str = "Alt";
#[cfg(target_os = "macos")]
const META: &str = "⌘";
#[cfg(not(target_os = "macos"))]
const META: &str = "Win";

/// Key caps for a chord: modifiers, then the key.
fn chord_caps(c: &Chord) -> Vec<String> {
    let mut out: Vec<String> = [(c.ctrl, MOD), (c.alt, ALT), (c.shift, "⇧"), (c.meta, META)]
        .iter()
        .filter(|(on, _)| *on)
        .map(|(_, s)| s.to_string())
        .collect();
    out.push(c.key());
    out
}

/// Tooltip label and key caps (the first chord bound in `km`).
pub fn act_tip(a: Act, km: &Keymap) -> (&'static str, Vec<String>) {
    let label = match a {
        Act::Tool(Tool::Path) => "Pencil",
        Act::Tool(Tool::Line) => "Line",
        Act::Tool(Tool::Arrow) => "Arrow",
        Act::Tool(Tool::Rect) => "Rectangle",
        Act::Tool(Tool::Ellipse) => "Ellipse",
        Act::Tool(Tool::Marker) => "Marker",
        Act::Tool(Tool::Text) => "Text",
        Act::Tool(Tool::Pixelate) => "Pixelate",
        Act::Tool(Tool::Invert) => "Invert",
        Act::Undo => "Undo",
        Act::Redo => "Redo",
        Act::Size(d) if d < 0 => "Smaller",
        Act::Size(_) => "Larger",
        Act::Palette => "Color",
        Act::Color(_) => "Use color",
        Act::Copy => "Copy",
        Act::Save => "Save",
        Act::SaveAs => "Save as",
        Act::Upload => "Upload",
        Act::Exit => "Close",
        Act::Accept => "Accept",
    };
    let keys = match a {
        Act::Size(_) => vec!["Wheel".to_string()],
        _ => super::action_of(a).and_then(|x| km.chords(x).first()).map(chord_caps).unwrap_or_default(),
    };
    (label, keys)
}

/// Dark label + key caps, 8 above `anchor` (below if no room).
pub fn tooltip(f: &mut Fb, ui: &Ui, anchor: FRect, label: &str, keys: &[String], area: FRect, k: f32) {
    let th = ui.th;
    let (h, gap, kgap) = (ui.px(26.0), ui.px(8.0), ui.px(4.0));
    let r = tooltip_rect(ui, anchor, label, keys, area);
    panel(f, ui, r, 7.0, &SMALL_LAYERS, th.tooltip_bg, rgba(0xFFFFFF, 80), k);
    let cy = r.y + h / 2.0;
    let mut cx = r.x + ui.px(9.0);
    cx += text(f, ui, 12.0, label, cx, cy, th.tooltip_text.fade(k));
    if !keys.is_empty() {
        cx += gap;
    }
    for s in keys {
        let kw = key_w(ui, s);
        key_cap(f, ui, cx, cy, kw, s, k);
        cx += kw + kgap;
    }
}

/// Where `tooltip` puts its panel.
fn tooltip_rect(ui: &Ui, anchor: FRect, label: &str, keys: &[String], area: FRect) -> FRect {
    let (h, gap, kgap) = (ui.px(26.0), ui.px(8.0), ui.px(4.0));
    let keys_w: f32 = keys.iter().map(|s| key_w(ui, s)).sum::<f32>() + kgap * keys.len().saturating_sub(1) as f32;
    let mut w = ui.px(9.0) + tw(ui, 12.0, label) + ui.px(if keys.is_empty() { 9.0 } else { 6.0 });
    if !keys.is_empty() {
        w += gap + keys_w;
    }
    let x = (anchor.x + anchor.w / 2.0 - w / 2.0).clamp(area.x, (area.x1() - w).max(area.x));
    let mut y = anchor.y - ui.px(8.0) - h;
    if y < area.y {
        y = anchor.y1() + ui.px(8.0);
    }
    FRect { x, y, w, h }
}

enum HintPart {
    T(&'static str),
    K(&'static str),
}

const HINT: [HintPart; 7] = [
    HintPart::T("Drag to select"),
    HintPart::T("·"),
    HintPart::K("Enter"),
    HintPart::T("to save"),
    HintPart::T("·"),
    HintPart::K("Esc"),
    HintPart::T("to cancel"),
];

fn hint_part_w(ui: &Ui, p: &HintPart) -> f32 {
    match p {
        HintPart::T(s) => tw(ui, 13.0, s),
        HintPart::K(s) => key_w(ui, s),
    }
}

/// Total width of the hint line (texts, key caps and 6 px gaps).
fn hint_width(ui: &Ui) -> f32 {
    HINT.iter().map(|p| hint_part_w(ui, p)).sum::<f32>() + ui.px(6.0) * (HINT.len() - 1) as f32
}

/// "Drag to select · Enter to save · Esc to cancel", centred in the active monitor.
pub fn hint(f: &mut Fb, ui: &Ui, area: FRect, k: f32) {
    let sp = ui.px(6.0);
    let total = hint_width(ui);
    let (mut x, cy) = (area.x + (area.w - total) / 2.0, area.y + area.h / 2.0);
    let c = C4::rgb(255, 255, 255).fade(0.7 * k);
    for p in &HINT {
        let w = hint_part_w(ui, p);
        match p {
            HintPart::T(s) => {
                text(f, ui, 13.0, s, x, cy, c);
            }
            HintPart::K(s) => key_cap(f, ui, x, cy, w, s, k),
        }
        x += w + sp;
    }
}

// ---- conservative bounds (shadows, AA, glyph overhang) for rect composition ----

/// Margin around anything anti-aliased.
const AA: f32 = 2.0;

fn grow4(r: FRect, l: f32, t: f32, rt: f32, b: f32) -> FRect {
    FRect { x: r.x - l, y: r.y - t, w: r.w + l + rt, h: r.h + t + b }
}

fn union(a: FRect, b: FRect) -> FRect {
    super::toolbar::union(a, b)
}

/// A panel at `r` with up to all four shadow layers: the widest spreads
/// 9 px sideways and reaches 12 + 9 px below.
fn surface_bounds(ui: &Ui, r: FRect) -> FRect {
    let (sp, dy) = (ui.px(SHADOW[3].1), ui.px(SHADOW[3].0));
    grow4(r, sp + 1.0 + AA, 1.0 + AA, sp + 1.0 + AA, dy + sp + 1.0 + AA)
}

/// Border strips (three 1 px lines) and the eight handles.
pub fn selection_bounds(ui: &Ui, sr: FRect) -> Vec<FRect> {
    let sr = round(sr);
    let b = 2.0 * ui.line() + AA;
    let mut v = vec![
        FRect { x: sr.x - b, y: sr.y - b, w: sr.w + 2.0 * b, h: 2.0 * b },
        FRect { x: sr.x - b, y: sr.y1() - b, w: sr.w + 2.0 * b, h: 2.0 * b },
        FRect { x: sr.x - b, y: sr.y - b, w: 2.0 * b, h: sr.h + 2.0 * b },
        FRect { x: sr.x1() - b, y: sr.y - b, w: 2.0 * b, h: sr.h + 2.0 * b },
    ];
    let hr = ui.px(3.0 + 3.0 + 1.0) + 1.0 + AA;
    for (_, p) in super::handle_points(sr) {
        v.push(FRect { x: p.x - hr, y: p.y - hr, w: 2.0 * hr, h: 2.0 * hr });
    }
    v
}

pub fn size_label_bounds(ui: &Ui, sr: FRect, show_pos: bool, area: FRect, avoid: Option<FRect>) -> FRect {
    surface_bounds(ui, size_label_rect(ui, sr, show_pos, area, avoid))
}

/// The panel plus the message, which may run past a panel clamped to the area.
pub fn toast_bounds(ui: &Ui, msg: &str, area: FRect, k: f32) -> FRect {
    let r = toast_rect(ui, msg, area, k);
    let x = r.x + ui.px(10.0 + 16.0 + 8.0);
    let t = FRect { x, y: r.y, w: tw(ui, 12.0, msg) + ui.px(12.0), h: r.h };
    union(surface_bounds(ui, r), grow4(t, AA, AA, AA, AA))
}

pub fn toolbar_bounds(ui: &Ui, tb: &Toolbar) -> FRect {
    let b = surface_bounds(ui, tb.bar);
    tb.pop.map_or(b, |p| union(b, surface_bounds(ui, p)))
}

pub fn tooltip_bounds(ui: &Ui, anchor: FRect, label: &str, keys: &[String], area: FRect) -> FRect {
    surface_bounds(ui, tooltip_rect(ui, anchor, label, keys, area))
}

pub fn hint_bounds(ui: &Ui, area: FRect) -> FRect {
    let total = hint_width(ui);
    let (x, cy) = (area.x + (area.w - total) / 2.0, area.y + area.h / 2.0);
    let m = ui.px(13.0);
    FRect { x: x - m - AA, y: cy - ui.px(14.0) - AA, w: total + 2.0 * (m + AA), h: ui.px(28.0) + 2.0 * AA }
}

#[cfg(test)]
mod tests {
    use super::super::toolbar::{layout, Input};
    use super::*;
    use crate::theme::DARK;

    fn buf(w: usize, h: usize, v: u8) -> Vec<u8> {
        let mut d = vec![v; w * h * 4];
        for p in d.as_chunks_mut::<4>().0 {
            p[3] = 255;
        }
        d
    }

    fn px(d: &[u8], w: usize, x: usize, y: usize) -> [u8; 4] {
        let i = (y * w + x) * 4;
        [d[i], d[i + 1], d[i + 2], d[i + 3]]
    }

    fn ui() -> Ui<'static> {
        Ui { th: &DARK, s: 1.0, font: None }
    }

    fn max_r(d: &[u8], w: usize, r: FRect) -> u8 {
        let mut m = 0;
        for y in r.y as usize..r.y1() as usize {
            for x in r.x as usize..r.x1() as usize {
                m = m.max(px(d, w, x, y)[0]);
            }
        }
        m
    }

    fn bar(tool: Option<Tool>, can_undo: bool) -> (Vec<u8>, Toolbar) {
        let tb = layout(&Input {
            sel: FRect { x: 20.0, y: 20.0, w: 760.0, h: 100.0 },
            area: FRect { x: 0.0, y: 0.0, w: 800.0, h: 200.0 },
            s: 1.0,
            busy: false,
            can_undo,
            can_redo: true,
            palette: None,
        });
        let mut d = buf(800, 200, 0);
        let st = BarState {
            hover: None,
            hover_k: 1.0,
            pressed: None,
            tool,
            color: C4::rgb(240, 68, 56),
            value: "3",
            unit: "line",
            pop_k: 1.0,
        };
        toolbar(&mut Fb::new(&mut d, 800), &ui(), &tb, &st, 1.0);
        (d, tb)
    }

    fn btn_rect(tb: &Toolbar, a: Act) -> FRect {
        tb.items.iter().find(|i| i.kind == Kind::Btn(a)).unwrap().r
    }

    #[test]
    fn surface_fill_border_and_shadow() {
        let mut d = buf(60, 50, 0);
        let r = FRect { x: 10.0, y: 10.0, w: 40.0, h: 20.0 };
        surface(&mut Fb::new(&mut d, 60), &ui(), r, 6.0, &ALL_LAYERS, 1.0);
        let c = px(&d, 60, 30, 20);
        let exp = |v: u32| (v * 245 / 255) as i32;
        assert!((c[0] as i32 - exp(0x1B)).abs() <= 1 && (c[2] as i32 - exp(0x20)).abs() <= 1, "{c:?}");
        assert!(px(&d, 60, 10, 20)[0] > c[0] + 10, "1px inner border is lighter");

        let mut w = buf(60, 50, 255);
        surface(&mut Fb::new(&mut w, 60), &ui(), r, 6.0, &ALL_LAYERS, 1.0);
        let sh = px(&w, 60, 30, 35)[0];
        assert!(sh < 250 && sh > 180, "soft shadow below: {sh}");
    }

    #[test]
    fn active_tool_gets_accent_tint() {
        let (d, tb) = bar(Some(Tool::Arrow), true);
        let a = btn_rect(&tb, Act::Tool(Tool::Arrow));
        let p = btn_rect(&tb, Act::Tool(Tool::Path));
        let active = px(&d, 800, a.x as usize + 3, a.y as usize + 16);
        let idle = px(&d, 800, p.x as usize + 3, p.y as usize + 16);
        assert!(active[2] > idle[2] + 20 && active[2] > active[0], "{active:?} vs {idle:?}");
    }

    #[test]
    fn disabled_icon_is_dimmer() {
        let (d, tb) = bar(None, false);
        let undo = max_r(&d, 800, btn_rect(&tb, Act::Undo));
        let redo = max_r(&d, 800, btn_rect(&tb, Act::Redo));
        assert!(undo + 40 < redo, "undo {undo} vs redo {redo}");
    }

    #[test]
    fn every_button_icon_exists() {
        let (_, tb) = bar(None, true);
        for it in &tb.items {
            if let Kind::Btn(a) = it.kind {
                let n = act_icon(a);
                if !n.is_empty() {
                    assert!(crate::icon_path::polylines(n).is_some(), "{n}");
                }
            }
        }
        assert!(crate::icon_path::polylines(act_icon(Act::Accept)).is_some());
    }

    #[test]
    fn selection_draws_accent_line_and_white_handles() {
        let mut d = buf(100, 80, 0);
        let sr = FRect { x: 20.0, y: 20.0, w: 60.0, h: 40.0 };
        selection(&mut Fb::new(&mut d, 100), &ui(), sr, None, 1.0);
        let line = px(&d, 100, 19, 30);
        assert!((line[0] as i32 - 0x8B).abs() <= 2 && line[2] > 240, "accent line: {line:?}");
        assert_eq!(px(&d, 100, 20, 20)[0], 255, "handle centre is white");
    }

    #[test]
    fn tool_tooltips_match_real_shortcuts() {
        let km = Keymap::defaults();
        for t in super::super::toolbar::TOOL_ORDER {
            let (_, keys) = act_tip(Act::Tool(t), &km);
            let vk = keys[0].as_bytes()[0] as u32;
            let a = km.resolve(vk, Default::default()).and_then(super::super::act_of);
            assert_eq!(a, Some(Act::Tool(t)), "{t:?}");
        }
        assert_eq!(act_tip(Act::Redo, &km).1, [MOD, "⇧", "Z"]);
        assert_eq!(act_tip(Act::Save, &km).1, [MOD, "S"]);
        assert_eq!(act_tip(Act::Exit, &km).1, ["Esc"]);
        let (km, _) = Keymap::from_config(&[("save".to_string(), "Alt+F2".to_string())].into_iter().collect());
        assert_eq!(act_tip(Act::Save, &km).1, [ALT, "F2"]);
        let (km, _) = Keymap::from_config(&[("upload".to_string(), String::new())].into_iter().collect());
        assert!(act_tip(Act::Upload, &km).1.is_empty());
    }
}
