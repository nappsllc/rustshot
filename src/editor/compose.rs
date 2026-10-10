//! Rect-based composition of the overlay. `Edit::prepare` resolves one
//! frame's state (layout, tweens, toast, caret) into a [`Scene`];
//! `Edit::compose_rect` renders any rect of that scene, byte-exact with a
//! whole-image compose, from a [`Backdrop`] (the capture, plain or dimmed)
//! plus the draft object and the chrome drawn through offset surfaces.
//! `dirty_rects` diffs two scenes into the rects whose pixels may differ.

use super::chrome::{self, BarState, ToastKind, Ui};
use super::toolbar::{self, Act, Toolbar};
use super::{caret_on, palette_colors, pick_area, text_box_rect, Dimmer, Edit, Interact, Toast};
use crate::anim;
use crate::objects::{CellGrid, FRect, Obj, Pt};
use crate::pixbuf::PixBuf;
use crate::raster::{Order, Surf};
use crate::theme::Theme;
use crate::uifb::{Fb, C4};
use std::sync::Arc;
use std::time::Instant;

/// Pixel rect in image coordinates, end-exclusive.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct PxRect {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl PxRect {
    pub const fn new(x0: i32, y0: i32, x1: i32, y1: i32) -> Self {
        PxRect { x0, y0, x1, y1 }
    }

    /// The whole `w` x `h` image.
    pub fn image(size: (u32, u32)) -> Self {
        PxRect::new(0, 0, size.0 as i32, size.1 as i32)
    }

    /// Smallest pixel rect containing `r` (floor / ceil).
    pub fn outer(r: FRect) -> Self {
        PxRect::new(r.x.floor() as i32, r.y.floor() as i32, r.x1().ceil() as i32, r.y1().ceil() as i32)
    }

    pub fn w(&self) -> i32 {
        (self.x1 - self.x0).max(0)
    }

    pub fn h(&self) -> i32 {
        (self.y1 - self.y0).max(0)
    }

    pub fn is_empty(&self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }

    pub fn intersect(&self, o: &PxRect) -> PxRect {
        PxRect::new(self.x0.max(o.x0), self.y0.max(o.y0), self.x1.min(o.x1), self.y1.min(o.y1))
    }

    pub fn intersects(&self, o: &PxRect) -> bool {
        !self.intersect(o).is_empty()
    }

    pub fn contains(&self, o: &PxRect) -> bool {
        o.is_empty() || (self.x0 <= o.x0 && self.y0 <= o.y0 && o.x1 <= self.x1 && o.y1 <= self.y1)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn contains_px(&self, x: i32, y: i32) -> bool {
        x >= self.x0 && x < self.x1 && y >= self.y0 && y < self.y1
    }

    /// `self` minus `o` as up to four disjoint rects.
    pub fn minus(&self, o: &PxRect) -> Vec<PxRect> {
        self.minus_iter(o).collect()
    }

    /// [`PxRect::minus`] without allocating.
    pub fn minus_iter(&self, o: &PxRect) -> impl Iterator<Item = PxRect> + use<> {
        let i = self.intersect(o);
        let parts = if i.is_empty() {
            [*self, PxRect::default(), PxRect::default(), PxRect::default()]
        } else {
            [
                PxRect::new(self.x0, self.y0, self.x1, i.y0),
                PxRect::new(self.x0, i.y1, self.x1, self.y1),
                PxRect::new(self.x0, i.y0, i.x0, i.y1),
                PxRect::new(i.x1, i.y0, self.x1, i.y1),
            ]
        };
        parts.into_iter().filter(|r| !r.is_empty())
    }

    /// Smallest rect containing both (an empty one contributes nothing).
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn union(&self, o: &PxRect) -> PxRect {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        PxRect::new(self.x0.min(o.x0), self.y0.min(o.y0), self.x1.max(o.x1), self.y1.max(o.y1))
    }
}

/// What a backdrop rect shows: the capture as is, or blended toward the
/// theme dim colour by `alpha`, `times` over (two only where the dim
/// rects of a fractional selection overlap).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    Plain,
    Dimmed { alpha: u8, times: u8 },
}

/// The pixels under the chrome: the capture (with committed objects).
pub trait Backdrop {
    /// Write rect `r` (inside the image) into `out`: row `i` starts at byte
    /// `i * stride * 4`, `r.w()` pixels in `order`.
    fn fill(&self, r: PxRect, src: Source, out: &mut [u8], stride: usize, order: Order);
}

/// In-memory backdrop (Linux/macOS, tests): `composed` plus the dim colour.
pub struct PixBufBackdrop<'a> {
    pub img: &'a PixBuf,
    /// Theme dim colour; its alpha is ignored (`Source` carries it).
    pub dim: C4,
}

impl Backdrop for PixBufBackdrop<'_> {
    fn fill(&self, r: PxRect, src: Source, out: &mut [u8], stride: usize, order: Order) {
        let iw = self.img.width() as usize;
        let w = r.w() as usize * 4;
        let raw = self.img.as_raw();
        let dimmer = match src {
            Source::Plain => None,
            Source::Dimmed { alpha, times } => Some((Dimmer::new(self.dim.with_alpha(alpha)), times as i32)),
        };
        for (i, y) in (r.y0..r.y1).enumerate() {
            let s = &raw[(y as usize * iw + r.x0 as usize) * 4..][..w];
            let d = &mut out[i * stride * 4..][..w];
            match &dimmer {
                None => d.copy_from_slice(s),
                Some((dm, times)) => dm.run(s, d, *times),
            }
            if order == Order::Bgra {
                for px in d.as_chunks_mut::<4>().0 {
                    px.swap(0, 2);
                }
            }
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct SelUi {
    pub sr: FRect,
    pub hot: Option<usize>,
}

#[derive(Clone, PartialEq, Debug)]
pub struct LabelUi {
    pub sr: FRect,
    pub show_pos: bool,
    pub area: FRect,
    pub avoid: Option<FRect>,
}

#[derive(Clone, PartialEq, Debug)]
pub struct TextUi {
    pub boxr: FRect,
    pub pos: Pt,
    pub text: String,
    pub px: f32,
    pub color: C4,
    /// Byte index of the caret while the blink phase shows it.
    pub caret: Option<usize>,
}

#[derive(Clone, PartialEq, Debug)]
pub struct ToastUi {
    pub text: String,
    pub kind: ToastKind,
    pub area: FRect,
    pub k: f32,
}

#[derive(Clone, PartialEq)]
pub struct BarUi {
    /// The toolbar as drawn (scaled during the fade-in).
    pub tb: Toolbar,
    pub k: f32,
    pub hover: Option<usize>,
    pub hover_k: f32,
    pub pressed: Option<usize>,
    pub tool: Option<super::Tool>,
    pub color: C4,
    pub value: String,
    pub unit: &'static str,
    pub pop_k: f32,
}

#[derive(Clone, PartialEq, Debug)]
pub struct TipUi {
    pub anchor: FRect,
    pub act: Act,
    pub label: &'static str,
    /// Key caps of the first chord bound to `act`.
    pub keys: Vec<String>,
    pub area: FRect,
    pub k: f32,
}

/// Chrome elements in paint order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum El {
    Draft,
    Sel,
    Label,
    Text,
    Toast,
    Bar,
    Tip,
    Hint,
}

const ELS: [El; 8] = [El::Draft, El::Sel, El::Label, El::Text, El::Toast, El::Bar, El::Tip, El::Hint];

/// Everything one frame paints, resolved at one instant. Two scenes
/// compare element by element (`dirty_rects`); a clone is the "painted
/// state" a presenter keeps.
#[derive(Clone, PartialEq)]
pub struct Scene {
    pub size: (u32, u32),
    pub s: f32,
    pub th: Theme,
    /// Dim alpha this frame (0 while the fade-in starts).
    pub dim_alpha: u8,
    /// The selection as the backdrop sees it (unrounded).
    pub sel: Option<FRect>,
    pub objects: Arc<Vec<Obj>>,
    pub draft: Option<Obj>,
    pub sel_ui: Option<SelUi>,
    pub label: Option<LabelUi>,
    pub text: Option<TextUi>,
    pub toast: Option<ToastUi>,
    pub bar: Option<BarUi>,
    pub tip: Option<TipUi>,
    pub hint: Option<(FRect, f32)>,
    /// Per element (index = `El as usize`): conservative pixel bounds.
    bounds: [Vec<PxRect>; 8],
}

impl Scene {
    fn rects(&self, e: El) -> &[PxRect] {
        &self.bounds[e as usize]
    }

    fn same(&self, o: &Scene, e: El) -> bool {
        match e {
            El::Draft => self.draft == o.draft,
            El::Sel => self.sel_ui == o.sel_ui,
            El::Label => self.label == o.label,
            El::Text => self.text == o.text,
            El::Toast => self.toast == o.toast,
            El::Bar => self.bar == o.bar,
            El::Tip => self.tip == o.tip,
            El::Hint => self.hint == o.hint,
        }
    }

    /// Every chrome (and draft) bound, clipped to the image.
    pub fn chrome_rects(&self) -> Vec<PxRect> {
        let mut v = Vec::new();
        self.chrome_rects_into(&mut v);
        v
    }

    /// [`Scene::chrome_rects`] appended to `out`.
    pub fn chrome_rects_into(&self, out: &mut Vec<PxRect>) {
        let img = PxRect::image(self.size);
        out.extend(self.bounds.iter().flatten().map(|r| r.intersect(&img)).filter(|r| !r.is_empty()));
    }

    /// The pixelate draft's cell grid, if the draft is a pixelate.
    pub fn draft_grid(&self) -> Option<CellGrid> {
        self.draft.as_ref().and_then(Obj::pixel_grid)
    }
}

/// Dim spans around the selection, snapped outward exactly like the
/// former whole-image pass (fractional edges overlap and dim twice).
/// Up to four; the count is returned with them.
fn dim_spans(sel: Option<FRect>, size: (u32, u32)) -> ([PxRect; 4], usize) {
    let (ww, wh) = (size.0 as f32, size.1 as f32);
    let (bw, bh) = (size.0 as i32, size.1 as i32);
    let none = (0.0, 0.0, 0.0, 0.0);
    let rects: [(f32, f32, f32, f32); 4] = match sel {
        None => [(0.0, 0.0, ww, wh), none, none, none],
        Some(sr) => [
            (0.0, 0.0, ww, sr.y),
            (0.0, sr.y1(), ww, wh - sr.y1()),
            (0.0, sr.y, sr.x, sr.h),
            (sr.x1(), sr.y, ww - sr.x1(), sr.h),
        ],
    };
    let mut out = [PxRect::default(); 4];
    let mut n = 0;
    for (x, y, w, h) in rects {
        if !(w > 0.0 && h > 0.0) {
            continue;
        }
        let x0 = x.floor().max(0.0) as i32;
        let y0 = y.floor().max(0.0) as i32;
        let x1 = ((x + w).ceil().min(bw as f32) as i32).min(bw);
        let y1 = ((y + h).ceil().min(bh as f32) as i32).min(bh);
        let r = PxRect::new(x0, y0, x1, y1);
        if !r.is_empty() {
            out[n] = r;
            n += 1;
        }
    }
    (out, n)
}

/// Backdrop of `r` into `out` (stride `r.w()`): plain inside the
/// selection, dimmed outside. Split into bands of rows crossed by the same
/// spans, so a backdrop gets a handful of rect fills, not one per row.
pub(super) fn compose_bg(sc: &Scene, r: PxRect, bd: &dyn Backdrop, out: &mut [u8], order: Order) {
    backdrop_rect(sc.sel, sc.size, sc.dim_alpha, r, bd, out, order);
}

/// `compose_bg` for a selection, image size and dim alpha.
pub(super) fn backdrop_rect(
    sel: Option<FRect>,
    size: (u32, u32),
    alpha: u8,
    r: PxRect,
    bd: &dyn Backdrop,
    out: &mut [u8],
    order: Order,
) {
    let stride = r.w() as usize;
    let mut runs = Vec::new();
    backdrop_runs_into(sel, size, alpha, r, &mut runs);
    for (q, src) in runs {
        let off = ((q.y0 - r.y0) as usize * stride + (q.x0 - r.x0) as usize) * 4;
        bd.fill(q, src, &mut out[off..], stride, order);
    }
}

/// Rect `r` split into disjoint runs that each show one [`Source`]: bands
/// of rows crossed by the same dim spans, cut where the dim depth changes.
/// A handful of runs per rect (the GDI overlay blits each one), appended
/// to `runs` (no other allocation).
pub(super) fn backdrop_runs_into(
    sel: Option<FRect>,
    size: (u32, u32),
    alpha: u8,
    r: PxRect,
    runs: &mut Vec<(PxRect, Source)>,
) {
    if r.is_empty() {
        return;
    }
    let (all, n) = if alpha == 0 { ([PxRect::default(); 4], 0) } else { dim_spans(sel, size) };
    let mut spans = [PxRect::default(); 4];
    let mut ns = 0;
    for s in all[..n].iter().filter(|s| s.intersects(&r)) {
        spans[ns] = *s;
        ns += 1;
    }
    let spans = &spans[..ns];
    if spans.is_empty() {
        runs.push((r, Source::Plain));
        return;
    }
    let mut ys = [0i32; 10];
    ys[0] = r.y0;
    ys[1] = r.y1;
    let mut ny = 2;
    for s in spans {
        for y in [s.y0, s.y1] {
            if y > r.y0 && y < r.y1 {
                ys[ny] = y;
                ny += 1;
            }
        }
    }
    let ys = &mut ys[..ny];
    ys.sort_unstable();
    let mut nu = 0;
    for i in 0..ys.len() {
        if i == 0 || ys[i] != ys[nu - 1] {
            ys[nu] = ys[i];
            nu += 1;
        }
    }
    for band in ys[..nu].windows(2) {
        let (ya, yb) = (band[0], band[1]);
        let mut edges = [(0i32, 0i32); 9];
        let mut ne = 0;
        for s in spans.iter().filter(|s| s.y0 <= ya && ya < s.y1) {
            edges[ne] = (s.x0.clamp(r.x0, r.x1), 1);
            edges[ne + 1] = (s.x1.clamp(r.x0, r.x1), -1);
            ne += 2;
        }
        edges[ne] = (r.x1, 0);
        ne += 1;
        let edges = &mut edges[..ne];
        edges.sort_unstable();
        let (mut x, mut depth) = (r.x0, 0i32);
        for &(ex, de) in edges.iter() {
            if ex > x {
                let src = if depth == 0 {
                    Source::Plain
                } else {
                    Source::Dimmed { alpha, times: depth as u8 }
                };
                runs.push((PxRect::new(x, ya, ex, yb), src));
                x = ex;
            }
            depth += de;
        }
    }
}

/// Coalesce rects: two merge when their bounding box is no larger than
/// the two areas summed (so merging never paints more). Order-insensitive
/// enough for invalidation; the result covers every input rect.
pub fn merge_rects(mut v: Vec<PxRect>) -> Vec<PxRect> {
    v.retain(|r| !r.is_empty());
    let area = |r: &PxRect| r.w() as i64 * r.h() as i64;
    let mut changed = true;
    while changed {
        changed = false;
        let mut i = 0;
        while i < v.len() {
            let mut j = i + 1;
            while j < v.len() {
                let (a, b) = (v[i], v[j]);
                let u = PxRect::new(a.x0.min(b.x0), a.y0.min(b.y0), a.x1.max(b.x1), a.y1.max(b.y1));
                if b.contains(&a) || a.contains(&b) || area(&u) <= area(&a) + area(&b) {
                    v[i] = u;
                    v.swap_remove(j);
                    changed = true;
                } else {
                    j += 1;
                }
            }
            i += 1;
        }
    }
    v
}

fn px_bounds(r: FRect) -> PxRect {
    PxRect::outer(r)
}

/// `r` grown to whole cells of `g` where it cuts the pixelated area.
fn cell_align(r: PxRect, g: &CellGrid, size: (u32, u32)) -> PxRect {
    let (x0, y0, x1, y1) = g.align((r.x0, r.y0, r.x1, r.y1), size.0 as i32, size.1 as i32);
    PxRect::new(x0, y0, x1, y1)
}

impl Edit {
    /// Resolve this frame: toolbar layout, tweens, area, toast, caret,
    /// draft; the result is `self.scene`. Also records what is drawn for
    /// `stale()` and rebuilds `composed` when objects changed.
    pub(super) fn prepare(&mut self, app_notice: Option<&Toast>, mouse: (i32, i32), now: Instant) {
        if self.dirty {
            self.rebuild();
        }
        let (ww, _) = (self.shot.size.0 as f32, self.shot.size.1 as f32);
        let area = pick_area(&self.shot.monitors, self.sel, Pt::new(mouse.0 as f32, mouse.1 as f32));
        self.area_drawn = Some(area);
        let s = self.ui_scale();
        let colors = palette_colors(&self.cfg);
        self.toolbar = self.sel.map(|sel| {
            toolbar::layout(&toolbar::Input {
                sel,
                area,
                s,
                busy: !self.tasks.is_empty(),
                can_undo: self.hi > 0,
                can_redo: self.hi + 1 < self.hist.len(),
                palette: self.palette_open.then_some(colors.as_slice()),
            })
        });

        let interacting = !matches!(self.interact, Interact::None);
        if self.toolbar.is_some() {
            self.mo.bar.set(1.0, 120, now);
        } else {
            self.mo.bar.snap(0.0);
        }
        if self.palette_open {
            self.mo.pop.set(1.0, 100, now);
        } else {
            self.mo.pop.snap(0.0);
        }
        let hint_on = self.sel.is_none() && !interacting;
        self.mo.hint.set(if hint_on { 1.0 } else { 0.0 }, if hint_on { 120 } else { 100 }, now);

        let dim_a = self.th.dim_alpha(self.cfg.contrast_opacity) as f32 * self.mo.dim.value(now);
        let objects = match &self.scene {
            Some(sc) if *sc.objects == self.objects => sc.objects.clone(),
            _ => Arc::new(self.objects.clone()),
        };

        let sel_ui = self.sel.map(|sr| SelUi { sr, hot: self.hot_handle });
        let label = self.sel.map(|sr| {
            let avoid = self
                .toolbar
                .as_ref()
                .filter(|t| t.above && t.bar.y1() <= sr.y)
                .map(|t| t.pop.map_or(t.bar, |p| toolbar::union(t.bar, p)));
            LabelUi { sr, show_pos: !interacting, area, avoid }
        });
        let text = self.text.as_ref().map(|td| TextUi {
            boxr: text_box_rect(ww, td, self.sizes.font, self.font.as_ref(), s),
            pos: td.pos,
            text: td.text.clone(),
            px: self.sizes.font,
            color: self.color,
            caret: caret_on(td).then_some(td.caret),
        });
        let toast = self.notice.as_ref().or(app_notice).map(|t| ToastUi {
            text: t.text.clone(),
            kind: t.kind,
            area,
            k: t.opacity(now),
        });
        let mut tip = None;
        let bar = self.toolbar.as_ref().map(|tb| {
            let kb = self.mo.bar.value(now);
            let (value, unit) = self.size_label();
            let shown_ms = now.saturating_duration_since(self.hover_at).as_secs_f32() * 1000.0 - 400.0;
            if !interacting
                && shown_ms >= 0.0
                && let Some(it) = self.hover.and_then(|i| tb.items.get(i))
                && let toolbar::Kind::Btn(act) = it.kind
            {
                let (label, keys) = chrome::act_tip(act, &self.keys);
                tip = Some(TipUi { anchor: it.r, act, label, keys, area, k: anim::ease_out(shown_ms / 80.0) });
            }
            BarUi {
                tb: tb.scaled(0.96 + 0.04 * kb),
                k: kb,
                hover: self.hover,
                hover_k: self.mo.hover.value(now),
                pressed: self.pressed,
                tool: self.tool,
                color: self.color,
                value,
                unit,
                pop_k: self.mo.pop.value(now),
            }
        });
        let kh = self.mo.hint.value(now);
        let hint = (kh > 0.01).then_some((area, kh));

        let mut sc = Scene {
            size: self.shot.size,
            s,
            th: self.th,
            dim_alpha: dim_a.round() as u8,
            sel: self.sel,
            objects,
            draft: self.draft.clone(),
            sel_ui,
            label,
            text,
            toast,
            bar,
            tip,
            hint,
            bounds: Default::default(),
        };
        sc.bounds = ELS.map(|e| self.el_bounds(&sc, e));
        if !matches!(sc.draft, Some(Obj::Pixelate { .. })) && self.draft_buf.borrow().capacity() > 0 {
            // The drag ended: release the pixelate scratch.
            *self.draft_buf.borrow_mut() = Vec::new();
        }
        self.caret_drawn = self.text.as_ref().map(caret_on);
        self.toast_drawn = self.notice.as_ref().or(app_notice).map(|t| t.at);
        self.scene = Some(sc);
    }

    fn ui<'a>(&'a self, sc: &'a Scene) -> Ui<'a> {
        Ui { th: &sc.th, s: sc.s, font: self.ui_font }
    }

    fn el_bounds(&self, sc: &Scene, e: El) -> Vec<PxRect> {
        let ui = self.ui(sc);
        let one = |r: FRect| vec![px_bounds(r)];
        match e {
            El::Draft => sc.draft.as_ref().and_then(|d| d.bounds(self.font.as_ref())).map_or(Vec::new(), one),
            El::Sel => sc
                .sel_ui
                .as_ref()
                .map_or(Vec::new(), |u| chrome::selection_bounds(&ui, u.sr).into_iter().map(px_bounds).collect()),
            El::Label => sc
                .label
                .as_ref()
                .map_or(Vec::new(), |l| one(chrome::size_label_bounds(&ui, l.sr, l.show_pos, l.area, l.avoid))),
            El::Text => sc.text.as_ref().map_or(Vec::new(), |t| {
                let lw = sc.s.round().max(1.0);
                let m = lw / 2.0 + 2.0;
                let b = FRect { x: t.boxr.x - m, y: t.boxr.y - m, w: t.boxr.w + 2.0 * m, h: t.boxr.h + 2.0 * m };
                let tw = self.font.as_ref().map_or(0.0, |f| f.line_width(&t.text, t.px));
                let g = t.px + 2.0;
                let txt = FRect { x: t.pos.x - g, y: t.pos.y - g, w: tw + 2.0 * g + 2.0 * sc.s, h: t.px * 1.15 + 2.0 * g };
                vec![px_bounds(b), px_bounds(txt)]
            }),
            El::Toast => sc.toast.as_ref().map_or(Vec::new(), |t| one(chrome::toast_bounds(&ui, &t.text, t.area, t.k))),
            El::Bar => sc.bar.as_ref().map_or(Vec::new(), |b| one(chrome::toolbar_bounds(&ui, &b.tb))),
            El::Tip => sc.tip.as_ref().map_or(Vec::new(), |t| {
                one(chrome::tooltip_bounds(&ui, t.anchor, t.label, &t.keys, t.area))
            }),
            El::Hint => sc.hint.map_or(Vec::new(), |(area, _)| one(chrome::hint_bounds(&ui, area))),
        }
    }

    /// Conservative bounds of every chrome element and the draft object
    /// in the prepared scene.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn chrome_rects(&self) -> Vec<PxRect> {
        self.scene.as_ref().map_or(Vec::new(), Scene::chrome_rects)
    }

    /// Render rect `r` of the prepared scene into `out` (`r.w()` x `r.h()`
    /// pixels in `order`): exactly the pixels a whole-image compose has in
    /// `r`. `r` is clipped to the image; `out` must hold the clipped rect.
    pub fn compose_rect(&self, r: PxRect, bd: &dyn Backdrop, out: &mut [u8], order: Order) {
        let Some(sc) = &self.scene else { return };
        let img = PxRect::image(sc.size);
        let r = r.intersect(&img);
        if r.is_empty() {
            return;
        }
        let (w, h) = (r.w() as usize, r.h() as usize);
        let out = &mut out[..w * h * 4];
        let hits = |e: El| sc.rects(e).iter().any(|b| b.intersects(&r));
        // Pixelate averages whole cells: where `r` cuts through cells,
        // render over `r` grown to whole cells (a reused scratch buffer,
        // about the size of `r`), then copy `r` out.
        let grown = match &sc.draft {
            Some(d) if hits(El::Draft) => d.pixel_grid().map(|g| cell_align(r, &g, sc.size)).filter(|e| *e != r),
            _ => None,
        };
        if let (Some(e), Some(d)) = (grown, &sc.draft) {
            let (ew, eh) = (e.w() as usize, e.h() as usize);
            let mut tmp = self.draft_buf.borrow_mut();
            // Every byte is written by `compose_bg`: no clearing.
            tmp.resize(ew * eh * 4, 0);
            let tmp = &mut tmp[..ew * eh * 4];
            compose_bg(sc, e, bd, tmp, order);
            d.render_into(&mut Surf::with_origin(tmp, ew as u32, eh as u32, e.x0, e.y0, order), self.font.as_ref());
            let n = w * 4;
            for y in r.y0..r.y1 {
                let si = ((y - e.y0) as usize * ew + (r.x0 - e.x0) as usize) * 4;
                out[(y - r.y0) as usize * n..][..n].copy_from_slice(&tmp[si..si + n]);
            }
        } else {
            compose_bg(sc, r, bd, out, order);
            if let Some(d) = &sc.draft
                && hits(El::Draft)
            {
                d.render_into(&mut Surf::with_origin(out, w as u32, h as u32, r.x0, r.y0, order), self.font.as_ref());
            }
        }

        let mut f = Fb::with_origin(out, w, r.x0, r.y0, order);
        let ui = self.ui(sc);
        if let Some(u) = &sc.sel_ui
            && hits(El::Sel)
        {
            chrome::selection(&mut f, &ui, u.sr, u.hot, 1.0);
        }
        if let Some(l) = &sc.label
            && hits(El::Label)
        {
            chrome::size_label(&mut f, &ui, l.sr, l.show_pos, l.area, l.avoid, 1.0);
        }
        if let Some(t) = &sc.text
            && hits(El::Text)
        {
            let s = sc.s;
            f.stroke_dashed_rect(t.boxr, 4.0 * s, 3.0 * s, s.round().max(1.0), sc.th.accent);
            if let Some(font) = self.font.as_ref() {
                // Drawn exactly as the committed text object renders.
                font.render(&mut f.surf(), &t.text, t.px, t.pos, t.color);
                if let Some(caret) = t.caret {
                    let before = t.text.get(..caret).unwrap_or("");
                    let cx = t.pos.x + font.line_width(before, t.px);
                    f.fill_rect(
                        cx.round() as i32,
                        t.pos.y.round() as i32,
                        (2.0 * s).round() as i32,
                        (t.px * 1.15).round() as i32,
                        t.color,
                    );
                }
            }
        }
        if let Some(t) = &sc.toast
            && hits(El::Toast)
        {
            chrome::toast(&mut f, &ui, &t.text, t.kind, t.area, t.k);
        }
        if let Some(b) = &sc.bar
            && hits(El::Bar)
        {
            let st = BarState {
                hover: b.hover,
                hover_k: b.hover_k,
                pressed: b.pressed,
                tool: b.tool,
                color: b.color,
                value: &b.value,
                unit: b.unit,
                pop_k: b.pop_k,
            };
            chrome::toolbar(&mut f, &ui, &b.tb, &st, b.k);
        }
        if let Some(t) = &sc.tip
            && hits(El::Tip)
        {
            chrome::tooltip(&mut f, &ui, t.anchor, t.label, &t.keys, t.area, t.k);
        }
        if let Some((area, kh)) = sc.hint
            && hits(El::Hint)
        {
            chrome::hint(&mut f, &ui, area, kh);
        }
    }

    /// Rects whose pixels may differ between the painted scene `prev` and
    /// the prepared one (see [`dirty_rects`]).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn dirty_rects(&self, prev: Option<&Scene>) -> Vec<PxRect> {
        match &self.scene {
            Some(now) => dirty_rects(prev, now, self.font.as_ref()),
            None => Vec::new(),
        }
    }
}

/// Pixels two pixelate drafts render identically: the whole cells inside
/// both, when the grid origin and cell size, the selection and the
/// objects (so the backdrop under them) are unchanged. A drag that moves
/// one corner repaints only the cells along the moving edges.
fn stable_draft(prev: &Scene, now: &Scene) -> Option<PxRect> {
    let (a, b) = (prev.draft_grid()?, now.draft_grid()?);
    if prev.sel != now.sel || !Arc::ptr_eq(&prev.objects, &now.objects) {
        return None;
    }
    let (x0, y0, x1, y1) = a.stable_with(&b, now.size.0 as i32, now.size.1 as i32)?;
    Some(PxRect::new(x0, y0, x1, y1)).filter(|r| !r.is_empty())
}

/// Rects (clipped to the image, possibly overlapping) covering every pixel
/// that may differ between scenes `prev` and `now`: old and new bounds of
/// each changed element, the symmetric difference of the selections, and
/// the bounds of objects past the first changed one. The whole image when
/// there is no previous scene, or size, scale, theme or dim alpha changed,
/// or a selection edge is fractional.
pub fn dirty_rects(prev: Option<&Scene>, now: &Scene, font: Option<&crate::text::AnnotFont>) -> Vec<PxRect> {
    let img = PxRect::image(now.size);
    let whole = vec![img];
    let Some(prev) = prev else { return whole };
    if prev.size != now.size || prev.s != now.s || prev.th != now.th || prev.dim_alpha != now.dim_alpha {
        return whole;
    }
    let mut out = Vec::new();
    if prev.sel != now.sel && now.dim_alpha != 0 {
        let integral = |r: &Option<FRect>| {
            r.is_none_or(|r| [r.x, r.y, r.w, r.h].iter().all(|v| v.fract() == 0.0))
        };
        if !integral(&prev.sel) || !integral(&now.sel) {
            return whole;
        }
        let inside = |r: &Option<FRect>| r.map_or(PxRect::default(), PxRect::outer).intersect(&img);
        let (a, b) = (inside(&prev.sel), inside(&now.sel));
        out.extend(a.minus(&b));
        out.extend(b.minus(&a));
    }
    for e in ELS {
        if prev.same(now, e) {
            continue;
        }
        if e == El::Draft
            && let Some(keep) = stable_draft(prev, now)
        {
            out.extend(prev.rects(e).iter().chain(now.rects(e)).flat_map(|r| r.minus_iter(&keep)));
            continue;
        }
        out.extend_from_slice(prev.rects(e));
        out.extend_from_slice(now.rects(e));
    }
    if !Arc::ptr_eq(&prev.objects, &now.objects) {
        let (p, n) = (&*prev.objects, &*now.objects);
        let common = p.iter().zip(n.iter()).take_while(|(a, b)| a == b).count();
        for o in p[common..].iter().chain(&n[common..]) {
            if let Some(b) = o.bounds(font) {
                out.push(px_bounds(b));
            }
        }
    }
    out.into_iter().map(|r| r.intersect(&img)).filter(|r| !r.is_empty()).collect()
}
