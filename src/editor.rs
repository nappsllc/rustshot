use crate::capture::{self, Shot};
use crate::config::{self, Config};
use crate::export::{self, Task};
use crate::hotkey::{HotEvent, Hotkeys};
use crate::icons::Icons;
use crate::objects::{self, FRect, Obj, Pt};
use ab_glyph::FontArc;
use eframe::egui;
use egui::epaint::ColorImage;
use egui::{
    pos2, vec2, Align2, Color32, CursorIcon, FontFamily, FontId, Id, Key, Margin, Modifiers,
    Order, PointerButton, Pos2, Rect, Rounding, Stroke, TextureHandle, TextureOptions,
    ViewportCommand,
};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tiny_skia::Pixmap;

use egui::viewport::WindowLevel;

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RunKind {
    OneShot,
    Daemon,
}

/// Everything needed to start one capture.
pub struct Pending {
    pub tasks: Vec<Task>,
    pub screen: Option<u32>,
    /// Initial selection in global (virtual-screen physical) coords.
    pub region: Option<(i32, i32, u32, u32)>,
    pub accept_on_select: bool,
    pub filename: Option<String>,
}

impl Pending {
    pub fn editor() -> Self {
        Self {
            tasks: Vec::new(),
            screen: None,
            region: None,
            accept_on_select: false,
            filename: None,
        }
    }
}

pub type UploadSlot = Arc<Mutex<Option<Receiver<Result<String, String>>>>>;

pub fn native_options() -> eframe::NativeOptions {
    eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_decorations(false)
            .with_visible(false)
            .with_taskbar(false)
            .with_resizable(false)
            .with_maximized(false)
            .with_inner_size([400.0, 300.0])
            .with_position([-16000.0, -16000.0]),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    }
}

pub fn run(
    cfg: Config,
    kind: RunKind,
    pending: Option<Pending>,
    exit_code: Arc<AtomicI32>,
    upload_slot: UploadSlot,
) -> i32 {
    let hot = match kind {
        RunKind::Daemon => Some(Hotkeys::new(&cfg)),
        RunKind::OneShot => None,
    };
    let opts = native_options();
    let creator_cfg = cfg.clone();
    let code_arc = Arc::clone(&exit_code);
    let res = eframe::run_native(
        "rustshot",
        opts,
        Box::new(move |cc| {
            let font = load_fonts(&cc.egui_ctx);
            let icons = Icons::load(&cc.egui_ctx);
            Box::new(App {
                cfg: creator_cfg,
                kind,
                hot,
                pending,
                st: State::Hidden,
                exit_code: code_arc,
                upload_slot,
                font,
                icons,
                notice: None,
            })
        }),
    );
    match res {
        Ok(()) => exit_code.load(Ordering::SeqCst),
        Err(e) => {
            eprintln!("error: ui failed: {e}");
            1
        }
    }
}

/// Load a Windows system font for both the UI and baked text annotations.
/// Deliberately avoids `FontDefinitions::default()`: those fonts are embedded
/// in the binary (~1.4 MB of TTFs) and we render everything with system fonts.
fn load_fonts(ctx: &egui::Context) -> Option<FontArc> {
    const CANDIDATES: [&str; 4] = [
        "C:\\Windows\\Fonts\\segoeui.ttf",
        "C:\\Windows\\Fonts\\arial.ttf",
        "C:\\Windows\\Fonts\\tahoma.ttf",
        "C:\\Windows\\Fonts\\verdana.ttf",
    ];
    let mut bytes = None;
    for c in CANDIDATES {
        if let Ok(b) = std::fs::read(c) {
            bytes = Some(b);
            break;
        }
    }
    let bytes = bytes?;
    let mut fonts = egui::FontDefinitions {
        font_data: Default::default(),
        families: Default::default(),
    };
    fonts
        .font_data
        .insert("ui".to_owned(), egui::FontData::from_owned(bytes.clone()));
    let mut chain = vec!["ui".to_owned()];
    if let Ok(sym) = std::fs::read("C:\\Windows\\Fonts\\seguisym.ttf") {
        fonts
            .font_data
            .insert("sym".to_owned(), egui::FontData::from_owned(sym));
        chain.push("sym".to_owned());
    }
    fonts
        .families
        .insert(FontFamily::Proportional, chain.clone());
    fonts.families.insert(FontFamily::Monospace, chain);
    ctx.set_fonts(fonts);
    FontArc::try_from_vec(bytes).ok()
}

// ---------------------------------------------------------------------------
// App state machine
// ---------------------------------------------------------------------------

enum State {
    /// Waiting for a hotkey (daemon) or about to close (one-shot).
    Hidden,
    /// Window is being moved/sized onto the capture target.
    Show(ShowStage),
    /// Interactive editor is up.
    Edit(Box<Edit>),
    /// Window is hidden; running export tasks (may show a file dialog).
    Finish(FinishJob),
}

/// Work to do after the overlay has been hidden again.
struct FinishJob {
    img: image::RgbaImage,
    sel_global: (i32, i32),
    tasks: Vec<Task>,
    cfg: Config,
}

struct ShowStage {
    shot: Shot,
    tasks: Vec<Task>,
    initial_sel: Option<FRect>,
    accept_on_select: bool,
    cfg: Config,
    frames: u32,
}

struct App {
    cfg: Config,
    kind: RunKind,
    hot: Option<Hotkeys>,
    pending: Option<Pending>,
    st: State,
    exit_code: Arc<AtomicI32>,
    upload_slot: UploadSlot,
    font: Option<FontArc>,
    icons: Icons,
    notice: Option<(String, Instant)>,
}

impl App {
    fn one_shot(&self) -> bool {
        self.kind == RunKind::OneShot
    }

    fn request_exit(&mut self, ctx: &egui::Context, code: i32) {
        self.exit_code.store(code, Ordering::SeqCst);
        ctx.send_viewport_cmd(ViewportCommand::Close);
    }

    fn begin_capture(&mut self, ctx: &egui::Context, mut pending: Pending) {
        let mut cfg = self.cfg.clone();
        if let Some(f) = pending.filename.take() {
            cfg.filename_pattern = f;
        }
        let screen = pending.screen.take();
        let region = pending.region.take();
        let tasks = std::mem::take(&mut pending.tasks);
        let accept_on_select = pending.accept_on_select;

        let shot = match capture::grab_edit(screen, cfg.capture_active_monitor) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: capture failed: {e:#}");
                if self.one_shot() {
                    self.exit_code.store(1, Ordering::SeqCst);
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
                return;
            }
        };
        let initial_sel = region.map(|(x, y, w, h)| {
            let (ix, iy) = shot.global_to_image((x, y));
            let mut r = FRect {
                x: ix as f32,
                y: iy as f32,
                w: w as f32,
                h: h as f32,
            };
            r.clamp_to(shot.size.0 as f32, shot.size.1 as f32);
            r
        });
        self.st = State::Show(ShowStage {
            shot,
            tasks,
            initial_sel,
            accept_on_select,
            cfg,
            frames: 0,
        });
        ctx.request_repaint();
    }

    fn tick_show(&mut self, ctx: &egui::Context, mut stage: ShowStage) {
        stage.frames += 1;
        let ppp = ctx.pixels_per_point();
        let (ow, oh) = (stage.shot.size.0 as f32, stage.shot.size.1 as f32);
        let (ox, oy) = (stage.shot.origin.0 as f32, stage.shot.origin.1 as f32);

        let (outer, inner) = ctx.input(|i| (i.viewport().outer_rect, i.viewport().inner_rect));
        let scale_ok = (ppp - stage.shot.scale).abs() < 0.02;
        let outer_ok = outer
            .map(|r| (r.min.x * ppp - ox).abs() < 2.0 && (r.min.y * ppp - oy).abs() < 2.0)
            .unwrap_or(false);
        let inner_ok = inner
            .map(|r| (r.width() * ppp - ow).abs() < 2.0 && (r.height() * ppp - oh).abs() < 2.0)
            .unwrap_or(false);

        if !(scale_ok && outer_ok && inner_ok) && stage.frames < 90 {
            ctx.send_viewport_cmd(ViewportCommand::WindowLevel(WindowLevel::AlwaysOnTop));
            ctx.send_viewport_cmd(ViewportCommand::Decorations(false));
            // Logical position is derived from the *current* scale; repeating
            // every frame converges even across monitors with different DPI.
            ctx.send_viewport_cmd(ViewportCommand::OuterPosition(pos2(ox / ppp, oy / ppp)));
            if scale_ok {
                ctx.send_viewport_cmd(ViewportCommand::InnerSize(vec2(ow / ppp, oh / ppp)));
            }
            ctx.request_repaint();
            self.st = State::Show(stage);
            return;
        }

        // Geometry settled: show and focus the window.
        ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(ViewportCommand::InnerSize(vec2(ow / ppp, oh / ppp)));
        ctx.send_viewport_cmd(ViewportCommand::WindowLevel(WindowLevel::AlwaysOnTop));
        ctx.send_viewport_cmd(ViewportCommand::Focus);
        ctx.request_repaint();

        let shot = stage.shot;
        let Some(base) = image_to_pixmap(&shot.image) else {
            eprintln!("error: could not allocate annotation surface");
            self.exit_code.store(1, Ordering::SeqCst);
            if self.one_shot() {
                ctx.send_viewport_cmd(ViewportCommand::Close);
            } else {
                self.hide_window(ctx);
                self.st = State::Hidden;
            }
            return;
        };
        let composed = base.clone();
        let tex = ctx.load_texture(
            "composed",
            ColorImage::from_rgba_premultiplied(
                [composed.width() as usize, composed.height() as usize],
                composed.data(),
            ),
            TextureOptions::LINEAR,
        );
        let (tw, th) = (shot.size.0 as f32, shot.size.1 as f32);
        let full = FRect {
            x: 0.0,
            y: 0.0,
            w: tw,
            h: th,
        };
        let sel = stage.initial_sel.filter(|r| !r.is_trivial());
        let accept_now = stage.accept_on_select && sel.is_some();
        let mut edit = Edit {
            shot,
            base,
            composed,
            tex,
            objects: Vec::new(),
            hist: vec![Vec::new()],
            hi: 0,
            sel,
            tool: None,
            draft: None,
            stroke_pts: Vec::new(),
            interact: Interact::None,
            color: config::parse_color(&self.cfg.draw_color)
                .map(|(r, g, b, _)| Color32::from_rgb(r, g, b))
                .unwrap_or(Color32::RED),
            sizes: Sizes::from_cfg(&stage.cfg),
            text: None,
            tasks: stage.tasks,
            accept_on_select: stage.accept_on_select,
            cfg: stage.cfg,
            toolbar_rect: None,
            palette_open: false,
            done: false,
            cancelled: false,
            dirty: false,
            notice: None,
            last_wheel: Instant::now(),
            font: self.font.clone(),
            focus_tries: 0,
            _full: full,
        };
        if accept_now {
            edit.done = true;
            edit.cancelled = false;
        }
        if edit.done {
            let cancelled = edit.cancelled;
            self.finish(ctx, &mut edit, cancelled);
        } else {
            self.st = State::Edit(Box::new(edit));
        }
    }

    fn hide_window(&self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        ctx.send_viewport_cmd(ViewportCommand::WindowLevel(WindowLevel::Normal));
    }

    /// Start the finish sequence: crop now, hide the window, export next frame
    /// (so a native save dialog is never covered by the always-on-top overlay).
    fn finish(&mut self, ctx: &egui::Context, edit: &mut Edit, cancelled: bool) {
        if cancelled {
            if self.one_shot() {
                self.exit_code.store(2, Ordering::SeqCst);
                ctx.send_viewport_cmd(ViewportCommand::Close);
            } else {
                self.hide_window(ctx);
                self.st = State::Hidden;
            }
            return;
        }
        if edit.dirty {
            edit.rebuild(ctx);
        }
        let sel = edit.sel.unwrap_or(FRect {
            x: 0.0,
            y: 0.0,
            w: edit.shot.size.0 as f32,
            h: edit.shot.size.1 as f32,
        });
        let img = crop_to_image(&edit.composed, sel);
        let g = (
            (sel.x.round() as i32) + edit.shot.origin.0,
            (sel.y.round() as i32) + edit.shot.origin.1,
        );
        self.hide_window(ctx);
        self.st = State::Finish(FinishJob {
            img,
            sel_global: g,
            tasks: std::mem::take(&mut edit.tasks),
            cfg: edit.cfg.clone(),
        });
    }

    fn run_finish(&mut self, ctx: &egui::Context, job: FinishJob) {
        let res = export::run_export(&job.img, job.sel_global, &job.tasks, &job.cfg);
        for m in &res.messages {
            println!("{m}");
        }
        if res.error {
            self.exit_code.store(1, Ordering::SeqCst);
        }
        *self.upload_slot.lock().unwrap() = res.upload;
        self.hide_window(ctx);
        match self.kind {
            RunKind::Daemon => self.st = State::Hidden,
            RunKind::OneShot => {
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        }
    }

    fn poll_upload(&mut self) {
        let slot = self.upload_slot.clone();
        let mut guard = slot.lock().unwrap();
        let Some(rx) = guard.take() else { return };
        match rx.try_recv() {
            Ok(Ok(url)) => {
                println!("uploaded: {url}");
                if self.cfg.copy_url_after_upload
                    && let Err(e) = export::copy_text_to_clipboard(&url) {
                        eprintln!("warning: could not copy URL: {e:#}");
                    }
                self.notice = Some((format!("uploaded {url}"), Instant::now()));
            }
            Ok(Err(e)) => eprintln!("error: upload failed: {e}"),
            Err(std::sync::mpsc::TryRecvError::Empty) => *guard = Some(rx),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {}
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Global hotkeys (daemon).
        if let Some(hot) = &self.hot {
            let ev = hot.poll();
            if ev == Some(HotEvent::Quit) {
                self.request_exit(ctx, 0);
                return;
            }
            if ev == Some(HotEvent::Capture) && matches!(self.st, State::Hidden) {
                self.begin_capture(ctx, Pending::editor());
                return;
            }
        }
        self.poll_upload();
        if let Some((_, at)) = &self.notice
            && at.elapsed() > Duration::from_millis(1500) {
                self.notice = None;
            }

        match std::mem::replace(&mut self.st, State::Hidden) {
            State::Hidden => {
                if let Some(pending) = self.pending.take() {
                    self.begin_capture(ctx, pending);
                }
                if matches!(self.st, State::Hidden) {
                    ctx.request_repaint_after(Duration::from_millis(150));
                }
            }
            State::Show(stage) => self.tick_show(ctx, stage),
            State::Finish(job) => self.run_finish(ctx, job),
            State::Edit(mut edit) => {
                self.tick_edit(ctx, &mut edit);
                if edit.done {
                    let cancelled = edit.cancelled;
                    self.finish(ctx, &mut edit, cancelled);
                } else {
                    self.st = State::Edit(edit);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The editor
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tool {
    Path,
    Line,
    Arrow,
    Rect,
    Ellipse,
    Marker,
    Text,
    Pixelate,
    Invert,
}

const TOOLS: [(Tool, &str); 9] = [
    (Tool::Path, "Pen"),
    (Tool::Line, "Line"),
    (Tool::Arrow, "Arrow"),
    (Tool::Rect, "Rect"),
    (Tool::Ellipse, "Oval"),
    (Tool::Marker, "Mark"),
    (Tool::Text, "Text"),
    (Tool::Pixelate, "Pix"),
    (Tool::Invert, "Inv"),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Handle {
    NW,
    N,
    NE,
    E,
    SE,
    S,
    SW,
    W,
}

enum Interact {
    None,
    NewSel {
        anchor: Pt,
        moved: bool,
    },
    MoveSel {
        start: Pt,
        orig: FRect,
    },
    Resize {
        handle: Handle,
        orig: FRect,
        aspect: f32,
    },
    Drawing {
        start: Pt,
    },
}

struct Sizes {
    line: f32,
    marker: f32,
    pixelate: f32,
    font: f32,
}

impl Sizes {
    fn from_cfg(cfg: &Config) -> Self {
        Self {
            line: cfg.draw_thickness.clamp(1.0, 50.0),
            marker: cfg.draw_marker_size.clamp(1.0, 50.0),
            pixelate: cfg.draw_pixelate_size.clamp(4.0, 100.0),
            font: cfg.draw_font_size.clamp(8.0, 96.0),
        }
    }
    fn active_mut(&mut self, tool: Option<Tool>) -> &mut f32 {
        match tool {
            Some(Tool::Marker) => &mut self.marker,
            Some(Tool::Pixelate) => &mut self.pixelate,
            Some(Tool::Text) => &mut self.font,
            _ => &mut self.line,
        }
    }
}

struct TextDraft {
    pos: Pt,
    text: String,
}

struct Edit {
    shot: Shot,
    base: Pixmap,
    composed: Pixmap,
    tex: TextureHandle,
    objects: Vec<Obj>,
    hist: Vec<Vec<Obj>>,
    hi: usize,
    sel: Option<FRect>,
    tool: Option<Tool>,
    draft: Option<Obj>,
    stroke_pts: Vec<Pt>,
    interact: Interact,
    color: Color32,
    sizes: Sizes,
    text: Option<TextDraft>,
    tasks: Vec<Task>,
    accept_on_select: bool,
    cfg: Config,
    toolbar_rect: Option<Rect>,
    palette_open: bool,
    done: bool,
    cancelled: bool,
    dirty: bool,
    notice: Option<(String, Instant)>,
    last_wheel: Instant,
    font: Option<FontArc>,
    focus_tries: u8,
    _full: FRect,
}

const HANDLE_PX: f32 = 6.0; // half-size of selection handles, in points
const CLICK_PX: f32 = 2.5; // movement below this counts as a click

impl Edit {
    fn rebuild(&mut self, ctx: &egui::Context) {
        let mut pm = self.base.clone();
        for o in &self.objects {
            o.render(&mut pm, self.font.as_ref());
        }
        let img = ColorImage::from_rgba_premultiplied(
            [pm.width() as usize, pm.height() as usize],
            pm.data(),
        );
        self.tex = ctx.load_texture("composed", img, TextureOptions::LINEAR);
        self.composed = pm;
        self.dirty = false;
    }

    fn push_history(&mut self) {
        self.hist.truncate(self.hi + 1);
        self.hist.push(self.objects.clone());
        self.hi = self.hist.len() - 1;
        let limit = self.cfg.undo_limit.max(1);
        if self.hist.len() > limit {
            let excess = self.hist.len() - limit;
            self.hist.drain(0..excess);
            self.hi -= excess;
        }
    }

    fn undo(&mut self) {
        if self.hi > 0 {
            self.hi -= 1;
            self.objects = self.hist[self.hi].clone();
            self.dirty = true;
        }
    }

    fn redo(&mut self) {
        if self.hi + 1 < self.hist.len() {
            self.hi += 1;
            self.objects = self.hist[self.hi].clone();
            self.dirty = true;
        }
    }

    fn commit_object(&mut self, obj: Obj) {
        self.objects.push(obj);
        self.push_history();
        self.dirty = true;
    }

    fn bucket(&self) -> &'static str {
        match self.tool {
            Some(Tool::Marker) => "marker",
            Some(Tool::Pixelate) => "pixelate",
            Some(Tool::Text) => "font",
            _ => "line",
        }
    }

    fn adjust_size(&mut self, dir: i32) {
        let v = self.sizes.active_mut(self.tool);
        let (lo, hi, step) = match self.tool {
            Some(Tool::Pixelate) => (4.0, 100.0, 1.0),
            Some(Tool::Text) => (8.0, 96.0, 1.0),
            _ => (1.0, 50.0, 1.0),
        };
        *v = (*v + dir as f32 * step).clamp(lo, hi);
        self.notice = Some((format!("{} px", *v as i32), Instant::now()));
    }

    fn make_draft(&self, tool: Tool, a: Pt, b: Pt) -> Option<Obj> {
        let c = self.color;
        let w = self.sizes.line;
        Some(match tool {
            Tool::Path => Obj::Path {
                pts: vec![a, b],
                color: c,
                width: w,
            },
            Tool::Line => Obj::Line { a, b, color: c, width: w },
            Tool::Arrow => Obj::Arrow { a, b, color: c, width: w },
            Tool::Rect => Obj::Rect {
                r: FRect::from_pts(a, b),
                color: c,
                width: w,
            },
            Tool::Ellipse => Obj::Ellipse {
                r: FRect::from_pts(a, b),
                color: c,
                width: w,
            },
            Tool::Marker => Obj::Marker {
                a,
                b,
                color: Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 90),
                width: self.sizes.marker,
            },
            Tool::Pixelate => Obj::Pixelate {
                r: FRect::from_pts(a, b),
                cell: self.sizes.pixelate,
            },
            Tool::Invert => Obj::Invert {
                r: FRect::from_pts(a, b),
            },
            Tool::Text => None?,
        })
    }

    fn draft_is_valid(&self) -> bool {
        match &self.draft {
            None => false,
            Some(Obj::Line { a, b, .. })
            | Some(Obj::Arrow { a, b, .. })
            | Some(Obj::Marker { a, b, .. }) => (a.x - b.x).hypot(a.y - b.y) >= 2.0,
            Some(Obj::Rect { r, .. })
            | Some(Obj::Ellipse { r, .. })
            | Some(Obj::Pixelate { r, .. })
            | Some(Obj::Invert { r }) => !r.is_trivial(),
            Some(Obj::Path { pts, .. }) => pts.len() >= 2,
            Some(Obj::Text { .. }) => true,
        }
    }

    fn snap_point(&self, start: Pt, cur: Pt, shift_axis: bool) -> Pt {
        if !shift_axis {
            return cur;
        }
        let dx = cur.x - start.x;
        let dy = cur.y - start.y;
        let len = dx.hypot(dy);
        if len < 1.0 {
            return cur;
        }
        let deg = dy.atan2(dx).to_degrees();
        let snapped = ((deg / 45.0).round() * 45.0).to_radians();
        Pt::new(start.x + len * snapped.cos(), start.y + len * snapped.sin())
    }
}

// ---------------------------------------------------------------------------
// Editor rendering + input
// ---------------------------------------------------------------------------

impl App {
    fn tick_edit(&mut self, ctx: &egui::Context, edit: &mut Edit) {
        // Windows only delivers key events to the foreground window; retry
        // for a few frames after the overlay becomes visible.
        if edit.focus_tries < 5 {
            edit.focus_tries += 1;
            capture::focus_our_window();
        }
        if edit.dirty {
            edit.rebuild(ctx);
        }
        if let Some((_, at)) = &edit.notice
            && at.elapsed() > Duration::from_millis(1500) {
                edit.notice = None;
            }

        self.handle_keys(ctx, edit);
        if edit.done {
            return;
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::none())
            .show(ctx, |ui| {
                let full = ui.max_rect();
                let s = (edit.shot.size.0 as f32 / full.width().max(1.0)).max(0.01);
                let inv = 1.0 / s;
                let origin = full.min;
                let to_img = |p: Pos2| -> Pt {
                    Pt::new((p.x - origin.x) * s, (p.y - origin.y) * s)
                };
                let to_pt = |p: Pt| -> Pos2 {
                    pos2(origin.x + p.x * inv, origin.y + p.y * inv)
                };

                let painter = ui.painter();
                painter.image(
                    edit.tex.id(),
                    full,
                    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                    Color32::WHITE,
                );

                // Selection rect in points.
                let sel_pt = edit
                    .sel
                    .map(|r| Rect::from_min_max(to_pt(Pt::new(r.x, r.y)), to_pt(Pt::new(r.x1(), r.y1()))));

                // Dim everything outside the selection.
                let dim = Color32::from_black_alpha(edit.cfg.contrast_opacity);
                match sel_pt {
                    None => {
                        painter.rect_filled(full, 0.0, dim);
                    }
                    Some(sr) => {
                        painter.rect_filled(
                            Rect::from_min_max(full.min, pos2(full.max.x, sr.min.y)),
                            0.0,
                            dim,
                        );
                        painter.rect_filled(
                            Rect::from_min_max(pos2(full.min.x, sr.max.y), full.max),
                            0.0,
                            dim,
                        );
                        painter.rect_filled(
                            Rect::from_min_max(full.min, pos2(sr.min.x, full.max.y)),
                            0.0,
                            dim,
                        );
                        painter.rect_filled(
                            Rect::from_min_max(pos2(sr.max.x, full.min.y), full.max),
                            0.0,
                            dim,
                        );
                    }
                }

                // Live draft preview.
                if let Some(d) = &edit.draft {
                    objects::paint_preview(painter, d, origin, inv);
                }

                // Selection border + handles + size label.
                if let Some(sr) = sel_pt {
                    let stroke = Stroke::new(1.4, edit.accent());
                    painter.rect_stroke(sr, 0.0, stroke);
                    for (h, pos) in handle_points(sr) {
                        let _ = h;
                        let r = Rect::from_center_size(pos, vec2(HANDLE_PX * 2.0, HANDLE_PX * 2.0));
                        painter.rect_filled(r, 2.0, edit.accent());
                        painter.rect_stroke(r, 2.0, Stroke::new(1.0, Color32::WHITE));
                    }
                    let label = format!(
                        "{}x{}",
                        (sr.width() * s).round() as i32,
                        (sr.height() * s).round() as i32
                    );
                    let font = FontId::proportional(12.0);
                    let at = pos2(sr.min.x, sr.max.y + 16.0);
                    let trect = painter.text(at, Align2::LEFT_CENTER, &label, font.clone(), Color32::TRANSPARENT);
                    painter.rect_filled(trect.expand(3.0), 2.0, Color32::from_black_alpha(190));
                    painter.text(at, Align2::LEFT_CENTER, &label, font, Color32::WHITE);
                }

                // Notice (tool size etc.), bottom center.
                if let Some((t, _)) = &edit.notice {
                    let font = FontId::proportional(14.0);
                    let at = pos2(full.center().x, full.max.y - 36.0);
                    let trect = painter.text(at, Align2::CENTER_CENTER, t, font.clone(), Color32::TRANSPARENT);
                    painter.rect_filled(trect.expand(6.0), 4.0, Color32::from_black_alpha(200));
                    painter.text(at, Align2::CENTER_CENTER, t, font, Color32::WHITE);
                }

                self.pointer_logic(ui, edit, full, s, inv, origin, &to_img, &to_pt, sel_pt);
                self.toolbar(ui, ctx, edit, full, s, inv, &to_pt, sel_pt);

                // Cursor.
                let hover = ui.input(|i| i.pointer.interact_pos());
                if edit.text.is_none()
                    && let Some(p) = hover {
                        if edit.tool.is_some() {
                            ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
                        } else if let Some(sr) = sel_pt {
                            if let Some((h, _)) = handle_points(sr).into_iter().find(|(_, hp)| {
                                hp.distance(p) <= HANDLE_PX * 1.4
                            }) {
                                ui.ctx().set_cursor_icon(handle_cursor(h));
                            } else if sr.contains(p) {
                                ui.ctx().set_cursor_icon(CursorIcon::Move);
                            } else {
                                ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
                            }
                        } else {
                            ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
                        }
                    }
            });
    }

    fn handle_keys(&mut self, ctx: &egui::Context, edit: &mut Edit) {
        let (escape, enter, arrows, mods, wheel) = ctx.input(|i| {
            (
                i.key_pressed(Key::Escape),
                i.key_pressed(Key::Enter),
                [
                    i.key_pressed(Key::ArrowLeft),
                    i.key_pressed(Key::ArrowRight),
                    i.key_pressed(Key::ArrowUp),
                    i.key_pressed(Key::ArrowDown),
                ],
                i.modifiers,
                i.raw_scroll_delta.y + i.smooth_scroll_delta.y,
            )
        });

        // Text editing mode consumes Enter/Escape.
        if edit.text.is_some() {
            if enter && !mods.shift {
                self.commit_text(edit);
                return;
            }
            if escape {
                edit.text = None;
                return;
            }
            return;
        }

        if escape {
            if matches!(edit.interact, Interact::None) && edit.draft.is_none() && !edit.palette_open && edit.tool.is_none() {
                edit.cancelled = true;
                edit.done = true;
            } else {
                edit.interact = Interact::None;
                edit.draft = None;
                edit.stroke_pts.clear();
                edit.palette_open = false;
                edit.tool = None;
            }
            return;
        }

        if mods.ctrl && !mods.alt {
            if ctx.input(|i| i.key_pressed(Key::Z)) {
                if mods.shift {
                    edit.redo();
                } else {
                    edit.undo();
                }
                return;
            }
            if ctx.input(|i| i.key_pressed(Key::Y)) {
                edit.redo();
                return;
            }
            if ctx.input(|i| i.key_pressed(Key::C)) {
                edit.tasks = vec![Task::Copy];
                edit.done = true;
                edit.cancelled = false;
                return;
            }
            if ctx.input(|i| i.key_pressed(Key::S)) {
                edit.tasks = vec![Task::Save { path: None }];
                edit.done = true;
                edit.cancelled = false;
                return;
            }
        }

        // Arrow keys nudge / resize the selection.
        if arrows.iter().any(|a| *a) && edit.sel.is_some() && !mods.ctrl && !mods.alt {
            let step = (edit.shot.scale).round().max(1.0);
            let dir = match (arrows[0], arrows[1], arrows[2], arrows[3]) {
                (true, ..) => (-1.0, 0.0),
                (_, true, ..) => (1.0, 0.0),
                (_, _, true, _) => (0.0, -1.0),
                _ => (0.0, 1.0),
            };
            let mut r = edit.sel.unwrap();
            if mods.shift {
                if dir.0 < 0.0 {
                    r.x -= step;
                    r.w += step;
                } else if dir.0 > 0.0 {
                    r.w += step;
                }
                if dir.1 < 0.0 {
                    r.y -= step;
                    r.h += step;
                } else if dir.1 > 0.0 {
                    r.h += step;
                }
                r.clamp_to(edit.shot.size.0 as f32, edit.shot.size.1 as f32);
            } else {
                r.x += dir.0 * step;
                r.y += dir.1 * step;
                r.clamp_to(edit.shot.size.0 as f32, edit.shot.size.1 as f32);
            }
            edit.sel = Some(r);
            return;
        }

        if enter {
            if edit.sel.is_none() {
                edit.sel = Some(FRect {
                    x: 0.0,
                    y: 0.0,
                    w: edit.shot.size.0 as f32,
                    h: edit.shot.size.1 as f32,
                });
                if edit.accept_on_select {
                    edit.done = true;
                    edit.cancelled = false;
                    return;
                }
            }
            if edit.tasks.is_empty() {
                edit.tasks = vec![Task::Save { path: None }];
            }
            edit.done = true;
            edit.cancelled = false;
            return;
        }

        // Tool shortcuts (plain letters, Flameshot-style).
        if !mods.ctrl && !mods.alt && !mods.shift {
            let pressed = |k: Key| ctx.input(|i| i.key_pressed(k));
            let tool = if pressed(Key::P) {
                Some(Tool::Path)
            } else if pressed(Key::D) || pressed(Key::L) {
                Some(Tool::Line)
            } else if pressed(Key::A) {
                Some(Tool::Arrow)
            } else if pressed(Key::R) {
                Some(Tool::Rect)
            } else if pressed(Key::C) {
                Some(Tool::Ellipse)
            } else if pressed(Key::M) {
                Some(Tool::Marker)
            } else if pressed(Key::T) {
                Some(Tool::Text)
            } else if pressed(Key::B) {
                Some(Tool::Pixelate)
            } else if pressed(Key::I) {
                Some(Tool::Invert)
            } else {
                None
            };
            if let Some(t) = tool {
                edit.tool = if edit.tool == Some(t) { None } else { Some(t) };
                edit.draft = None;
                edit.stroke_pts.clear();
                return;
            }
        }

        // Wheel adjusts the active tool size.
        if wheel.abs() > 0.5 && edit.last_wheel.elapsed() > Duration::from_millis(160) {
            edit.last_wheel = Instant::now();
            let dir = if wheel > 0.0 { 1 } else { -1 };
            edit.adjust_size(dir);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn pointer_logic(
        &mut self,
        ui: &mut egui::Ui,
        edit: &mut Edit,
        full: Rect,
        s: f32,
        inv: f32,
        origin: Pos2,
        to_img: &dyn Fn(Pos2) -> Pt,
        _to_pt: &dyn Fn(Pt) -> Pos2,
        sel_pt: Option<Rect>,
    ) {
        let _ = (full, inv, origin);
        let pressed = ui.input(|i| i.pointer.button_pressed(PointerButton::Primary));
        let released = ui.input(|i| i.pointer.button_released(PointerButton::Primary));
        let down = ui.input(|i| i.pointer.button_down(PointerButton::Primary));
        let pos = ui.input(|i| i.pointer.interact_pos());
        let mods = ui.input(|i| i.modifiers);
        let over_toolbar = edit
            .toolbar_rect
            .map(|r| pos.map(|p| r.contains(p)).unwrap_or(false))
            .unwrap_or(false);

        let Some(pos) = pos else {
            if released {
                self.end_interaction(edit, to_img(pos2(0.0, 0.0)), false, sel_pt, s);
            }
            return;
        };
        let img_pos = to_img(pos);

        // --- text draft takes priority -------------------------------------
        let mut outside_click: Option<(Pt, String)> = None;
        if let Some(td) = &mut edit.text {
            let font_pt = edit.sizes.font * inv;
            let rect = Rect::from_min_size(
                to_pt_clamped(td.pos, origin, inv, full),
                vec2(320.0, font_pt * 1.5),
            );
            if pressed && !rect.contains(pos) && !over_toolbar {
                outside_click = Some((td.pos, td.text.clone()));
            } else {
                let resp = ui.put(
                    rect,
                    egui::TextEdit::singleline(&mut td.text)
                        .font(FontId::proportional(font_pt))
                        .desired_width(316.0),
                );
                let _ = resp;
                return;
            }
        }
        if let Some((pos_img, text)) = outside_click {
            edit.text = None;
            self.commit_text_str(edit, pos_img, text);
            return;
        }

        // --- press ----------------------------------------------------------
        if pressed && !matches!(edit.interact, Interact::None) {
            return; // already dragging
        }
        if pressed && !over_toolbar {
            if let Some(tool) = edit.tool {
                if tool == Tool::Text {
                    edit.text = Some(TextDraft {
                        pos: img_pos,
                        text: String::new(),
                    });
                    edit.interact = Interact::None;
                    return;
                }
                edit.stroke_pts = vec![img_pos];
                edit.draft = edit.make_draft(tool, img_pos, img_pos);
                edit.interact = Interact::Drawing { start: img_pos };
                return;
            }
            if let Some(sr) = sel_pt {
                if let Some((h, _)) = handle_points(sr).into_iter().find(|(_, hp)| hp.distance(pos) <= HANDLE_PX * 1.4) {
                    let orig = edit.sel.unwrap();
                    let aspect = if orig.h > 0.0 { orig.w / orig.h } else { 1.0 };
                    edit.interact = Interact::Resize { handle: h, orig, aspect };
                    return;
                }
                if sr.contains(pos) {
                    edit.interact = Interact::MoveSel {
                        start: img_pos,
                        orig: edit.sel.unwrap(),
                    };
                    return;
                }
            }
            edit.interact = Interact::NewSel {
                anchor: img_pos,
                moved: false,
            };
            return;
        }

        // --- drag -----------------------------------------------------------
        if down {
            match &mut edit.interact {
                Interact::None => {}
                Interact::NewSel { anchor, moved } => {
                    let dist = (img_pos.x - anchor.x).hypot(img_pos.y - anchor.y) * inv;
                    if dist >= CLICK_PX {
                        *moved = true;
                    }
                    if *moved {
                        let p = if mods.shift {
                            constrain_square(*anchor, img_pos)
                        } else {
                            img_pos
                        };
                        let mut r = FRect::from_pts(*anchor, p);
                        r.clamp_to(edit.shot.size.0 as f32, edit.shot.size.1 as f32);
                        edit.sel = Some(r);
                    }
                }
                Interact::MoveSel { start, orig } => {
                    let dx = img_pos.x - start.x;
                    let dy = img_pos.y - start.y;
                    let mut r = FRect {
                        x: orig.x + dx,
                        y: orig.y + dy,
                        w: orig.w,
                        h: orig.h,
                    };
                    r.clamp_to(edit.shot.size.0 as f32, edit.shot.size.1 as f32);
                    edit.sel = Some(r);
                }
                Interact::Resize { handle, orig, aspect } => {
                    let r = resize_rect(
                        *handle,
                        *orig,
                        img_pos,
                        *aspect,
                        &mods,
                        (edit.shot.size.0 as f32, edit.shot.size.1 as f32),
                    );
                    edit.sel = Some(r);
                }
                Interact::Drawing { start } => {
                    let start = *start;
                    let tool = edit.tool.unwrap_or(Tool::Line);
                    // Photoshop-style constraints while Shift is held:
                    // lines/arrows snap to 45-degree steps, rectangles and
                    // ellipses become squares and circles.
                    let cur = match tool {
                        Tool::Line | Tool::Arrow => {
                            if mods.shift || mods.ctrl {
                                edit.snap_point(start, img_pos, true)
                            } else {
                                img_pos
                            }
                        }
                        Tool::Rect
                        | Tool::Ellipse
                        | Tool::Pixelate
                        | Tool::Invert
                            if mods.shift =>
                        {
                            constrain_square(start, img_pos)
                        }
                        _ => img_pos,
                    };
                    if matches!(tool, Tool::Path | Tool::Marker) {
                        let last = edit.stroke_pts.last().copied();
                        let need = match last {
                            Some(l) => (cur.x - l.x).hypot(cur.y - l.y) >= 2.0,
                            None => true,
                        };
                        if need {
                            edit.stroke_pts.push(cur);
                        }
                        let pts = edit.stroke_pts.clone();
                        let color = edit.color;
                        let width = if tool == Tool::Marker {
                            edit.sizes.marker
                        } else {
                            edit.sizes.line
                        };
                        edit.draft = Some(if tool == Tool::Marker {
                            Obj::Marker {
                                a: pts.first().copied().unwrap_or(cur),
                                b: cur,
                                color: Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 90),
                                width,
                            }
                        } else {
                            Obj::Path { pts, color, width }
                        });
                    } else {
                        edit.draft = edit.make_draft(tool, start, cur);
                    }
                }
            }
        }

        // --- release --------------------------------------------------------
        if released {
            self.end_interaction(edit, img_pos, mods.ctrl, sel_pt, s);
        }
    }

    fn end_interaction(
        &mut self,
        edit: &mut Edit,
        _img_pos: Pt,
        _ctrl: bool,
        _sel_pt: Option<Rect>,
        _s: f32,
    ) {
        match std::mem::replace(&mut edit.interact, Interact::None) {
            Interact::Drawing { .. } => {
                if edit.draft_is_valid() {
                    let d = edit.draft.take().expect("draft exists");
                    edit.commit_object(d);
                }
                edit.draft = None;
                edit.stroke_pts.clear();
            }
            Interact::NewSel { moved, .. } => {
                if !moved {
                    // A plain click selects everything (Flameshot-ish).
                    edit.sel = Some(FRect {
                        x: 0.0,
                        y: 0.0,
                        w: edit.shot.size.0 as f32,
                        h: edit.shot.size.1 as f32,
                    });
                }
                if edit.accept_on_select {
                    edit.done = true;
                    edit.cancelled = false;
                }
            }
            Interact::MoveSel { .. } | Interact::Resize { .. } | Interact::None => {}
        }
    }

    fn commit_text(&mut self, edit: &mut Edit) {
        let td = edit.text.take().expect("text draft");
        self.commit_text_str(edit, td.pos, td.text);
    }

    fn commit_text_str(&mut self, edit: &mut Edit, pos: Pt, text: String) {
        let text = text.trim_end_matches('\n').to_string();
        if text.trim().is_empty() {
            return;
        }
        edit.commit_object(Obj::Text {
            pos,
            text,
            color: edit.color,
            size: edit.sizes.font,
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn toolbar(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        edit: &mut Edit,
        full: Rect,
        s: f32,
        inv: f32,
        to_pt: &dyn Fn(Pt) -> Pos2,
        sel_pt: Option<Rect>,
    ) {
        let _ = (s, inv, to_pt);
        let Some(sr) = sel_pt else {
            edit.toolbar_rect = None;
            edit.palette_open = false;
            return;
        };
        let gap = 8.0;
        let est = edit.toolbar_rect.map(|r| r.size()).unwrap_or(vec2(760.0, 62.0));
        let mut x = sr.center().x - est.x / 2.0;
        let mut y = sr.max.y + gap;
        if y + est.y > full.max.y - 4.0 {
            y = sr.min.y - gap - est.y;
        }
        x = x.clamp(full.min.x + 4.0, (full.max.x - 4.0 - est.x).max(full.min.x + 4.0));
        y = y.clamp(full.min.y + 4.0, (full.max.y - 4.0 - est.y).max(full.min.y + 4.0));
        let start = pos2(x, y);

        let accent = edit.accent();
        let task_mode = !edit.tasks.is_empty();
        let mut act: Option<Act> = None;
        let palette_open = edit.palette_open;
        let color = edit.color;
        let active_tool = edit.tool;
        let line_size = edit.sizes.line;
        let marker_size = edit.sizes.marker;
        let pixel_size = edit.sizes.pixelate;
        let font_size = edit.sizes.font;
        let cfg_colors = edit.cfg.user_colors.clone();
        let bucket = edit.bucket();

        let mut size = vec2(0.0, 0.0);
        egui::Area::new(Id::new("rustshot_toolbar"))
            .fixed_pos(start)
            .order(Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::none()
                    .fill(Color32::from_black_alpha(175))
                    .rounding(Rounding::same(6.0))
                    .inner_margin(Margin::same(5.0))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing = vec2(3.0, 3.0);
                        ui.horizontal_wrapped(|ui| {
                            for (tool, label) in TOOLS {
                                let on = active_tool == Some(tool);
                                let fill = if on { accent } else { Color32::from_gray(45) };
                                let b = icon_button(ui, &self.icons, tool_icon(tool), label, fill);
                                if b.clicked() {
                                    act = Some(Act::Tool(tool));
                                }
                            }
                            ui.separator();
                            if icon_button(ui, &self.icons, "undo-variant", "Undo", Color32::from_gray(45))
                                .clicked()
                            {
                                act = Some(Act::Undo);
                            }
                            if icon_button(ui, &self.icons, "redo-variant", "Redo", Color32::from_gray(45))
                                .clicked()
                            {
                                act = Some(Act::Redo);
                            }
                            ui.separator();
                            if icon_button(ui, &self.icons, "minus", "Smaller", Color32::from_gray(45))
                                .clicked()
                            {
                                act = Some(Act::Size(-1));
                            }
                            ui.label(format!("{} {bucket}", size_display(bucket, line_size, marker_size, pixel_size, font_size)));
                            if icon_button(ui, &self.icons, "plus", "Bigger", Color32::from_gray(45))
                                .clicked()
                            {
                                act = Some(Act::Size(1));
                            }
                            ui.separator();
                            let swatch = ui.add(
                                egui::Button::new("")
                                    .fill(color)
                                    .min_size(vec2(26.0, 24.0)),
                            );
                            if swatch.clicked() {
                                act = Some(Act::Palette);
                            }
                            ui.separator();
                            if task_mode {
                                if icon_button(ui, &self.icons, "accept", "OK", Color32::from_gray(45))
                                    .clicked()
                                {
                                    act = Some(Act::Accept);
                                }
                            } else {
                                if icon_button(ui, &self.icons, "content-copy", "Copy", Color32::from_gray(45))
                                    .clicked()
                                {
                                    act = Some(Act::Copy);
                                }
                                if icon_button(ui, &self.icons, "content-save", "Save", Color32::from_gray(45))
                                    .clicked()
                                {
                                    act = Some(Act::Save);
                                }
                                if icon_button(ui, &self.icons, "cloud-upload", "Upload", Color32::from_gray(45))
                                    .clicked()
                                {
                                    act = Some(Act::Upload);
                                }
                            }
                            if icon_button(ui, &self.icons, "close", "Exit", Color32::from_gray(45))
                                .clicked()
                            {
                                act = Some(Act::Exit);
                            }
                        });
                        if palette_open {
                            ui.horizontal_wrapped(|ui| {
                                for c in &cfg_colors {
                                    let Some((r, g, b, a)) = config::parse_color(c) else { continue };
                                    let col = Color32::from_rgba_unmultiplied(r, g, b, a);
                                    let sw = ui.add(egui::Button::new("").fill(col).min_size(vec2(20.0, 20.0)));
                                    if sw.clicked() {
                                        act = Some(Act::Color(col));
                                    }
                                }
                            });
                        }
                    });
                size = ui.min_rect().size();
            });

        if size.x > 0.0 {
            edit.toolbar_rect = Some(Rect::from_min_size(start, size));
        }

        match act {
            None => {}
            Some(Act::Tool(t)) => {
                edit.tool = if edit.tool == Some(t) { None } else { Some(t) };
                edit.draft = None;
                edit.stroke_pts.clear();
            }
            Some(Act::Undo) => edit.undo(),
            Some(Act::Redo) => edit.redo(),
            Some(Act::Size(d)) => edit.adjust_size(d),
            Some(Act::Color(c)) => {
                edit.color = c;
                edit.palette_open = false;
            }
            Some(Act::Palette) => edit.palette_open = !edit.palette_open,
            Some(Act::Copy) => {
                edit.tasks = vec![Task::Copy];
                edit.done = true;
            }
            Some(Act::Save) => {
                edit.tasks = vec![Task::Save { path: None }];
                edit.done = true;
            }
            Some(Act::Upload) => {
                edit.tasks = vec![Task::Upload];
                edit.done = true;
            }
            Some(Act::Exit) => {
                edit.cancelled = true;
                edit.done = true;
            }
            Some(Act::Accept) => {
                edit.done = true;
            }
        }
        let _ = ui;
    }
}

enum Act {
    Tool(Tool),
    Undo,
    Redo,
    Size(i32),
    Color(Color32),
    Palette,
    Copy,
    Save,
    Upload,
    Exit,
    Accept,
}

impl Edit {
    fn accent(&self) -> Color32 {
        config::parse_color(&self.cfg.ui_color)
            .map(|(r, g, b, _)| Color32::from_rgb(r, g, b))
            .unwrap_or(Color32::from_rgb(0x74, 0x00, 0x96))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn size_display(bucket: &str, line: f32, marker: f32, pixel: f32, font: f32) -> String {
    let v = match bucket {
        "marker" => marker,
        "pixelate" => pixel,
        "font" => font,
        _ => line,
    };
    format!("{}", v as i32)
}

fn handle_points(sr: Rect) -> [(Handle, Pos2); 8] {
    let l = sr.min.x;
    let r = sr.max.x;
    let t = sr.min.y;
    let b = sr.max.y;
    let cx = sr.center().x;
    let cy = sr.center().y;
    [
        (Handle::NW, pos2(l, t)),
        (Handle::N, pos2(cx, t)),
        (Handle::NE, pos2(r, t)),
        (Handle::E, pos2(r, cy)),
        (Handle::SE, pos2(r, b)),
        (Handle::S, pos2(cx, b)),
        (Handle::SW, pos2(l, b)),
        (Handle::W, pos2(l, cy)),
    ]
}

fn tool_icon(tool: Tool) -> &'static str {
    match tool {
        Tool::Path => "pencil",
        Tool::Line => "line",
        Tool::Arrow => "arrow-bottom-left",
        Tool::Rect => "square-outline",
        Tool::Ellipse => "circle-outline",
        Tool::Marker => "marker",
        Tool::Text => "text",
        Tool::Pixelate => "pixelate",
        Tool::Invert => "invert",
    }
}

fn icon_button(
    ui: &mut egui::Ui,
    icons: &Icons,
    name: &str,
    label: &str,
    fill: Color32,
) -> egui::Response {
    match icons.get(name) {
        Some(t) => ui
            .add(
                egui::Button::image((t.id(), vec2(20.0, 20.0)))
                    .fill(fill)
                    .min_size(vec2(26.0, 26.0)),
            )
            .on_hover_text(label),
        None => ui
            .add(egui::Button::new(label).fill(fill).min_size(vec2(0.0, 24.0))),
    }
}

fn handle_cursor(h: Handle) -> CursorIcon {
    match h {
        Handle::NW | Handle::SE => CursorIcon::ResizeNwSe,
        Handle::NE | Handle::SW => CursorIcon::ResizeNeSw,
        Handle::N | Handle::S => CursorIcon::ResizeVertical,
        Handle::E | Handle::W => CursorIcon::ResizeHorizontal,
    }
}

/// Photoshop-style Shift constraint: force the drag extent into a square,
/// keeping the larger dragged dimension and the drag quadrant.
fn constrain_square(start: Pt, cur: Pt) -> Pt {
    let dx = cur.x - start.x;
    let dy = cur.y - start.y;
    let side = dx.abs().max(dy.abs());
    Pt::new(
        start.x + side.copysign(dx),
        start.y + side.copysign(dy),
    )
}

/// Compute the selection rect while resizing with a handle.
fn resize_rect(
    h: Handle,
    orig: FRect,
    cur: Pt,
    aspect: f32,
    mods: &Modifiers,
    img: (f32, f32),
) -> FRect {
    let cur = Pt {
        x: cur.x.round(),
        y: cur.y.round(),
    };
    let (mut x0, mut y0, mut x1, mut y1) = (orig.x, orig.y, orig.x1(), orig.y1());
    let left = matches!(h, Handle::W | Handle::NW | Handle::SW);
    let right = matches!(h, Handle::E | Handle::NE | Handle::SE);
    let top = matches!(h, Handle::N | Handle::NW | Handle::NE);
    let bot = matches!(h, Handle::S | Handle::SW | Handle::SE);

    if left {
        x0 = cur.x;
    }
    if right {
        x1 = cur.x;
    }
    if top {
        y0 = cur.y;
    }
    if bot {
        y1 = cur.y;
    }
    if mods.shift {
        if left {
            x1 = orig.x1() + (orig.x - x0);
        }
        if right {
            x0 = orig.x - (x1 - orig.x1());
        }
        if top {
            y1 = orig.y1() + (orig.y - y0);
        }
        if bot {
            y0 = orig.y - (y1 - orig.y1());
        }
    }
    if mods.ctrl && aspect > 0.0 {
        let w = (x1 - x0).abs();
        let hh = (y1 - y0).abs();
        if w / aspect <= hh {
            let nh = w / aspect;
            let cy = (y0 + y1) / 2.0;
            y0 = cy - nh / 2.0;
            y1 = cy + nh / 2.0;
        } else {
            let nw = hh * aspect;
            let cx = (x0 + x1) / 2.0;
            x0 = cx - nw / 2.0;
            x1 = cx + nw / 2.0;
        }
    }
    let mut r = FRect {
        x: x0.min(x1),
        y: y0.min(y1),
        w: (x1 - x0).abs(),
        h: (y1 - y0).abs(),
    };
    r.clamp_to(img.0, img.1);
    r
}

fn to_pt_clamped(p: Pt, origin: Pos2, inv: f32, full: Rect) -> Pos2 {
    let mut x = origin.x + p.x * inv;
    let y = origin.y + p.y * inv;
    x = x.min(full.max.x - 330.0).max(full.min.x);
    pos2(x, y)
}

/// Copy an unpremultiplied RGBA image into a tiny-skia pixmap.
fn image_to_pixmap(img: &image::RgbaImage) -> Option<Pixmap> {
    let mut pm = Pixmap::new(img.width(), img.height())?;
    let src = img.as_raw();
    let dst = pm.data_mut();
    for (s, d) in src.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
        let a = s[3] as u16;
        d[0] = ((s[0] as u16 * a) / 255) as u8;
        d[1] = ((s[1] as u16 * a) / 255) as u8;
        d[2] = ((s[2] as u16 * a) / 255) as u8;
        d[3] = s[3];
    }
    Some(pm)
}

/// Crop a rect from a premultiplied pixmap, returning unpremultiplied RGBA.
fn crop_to_image(pm: &Pixmap, r: FRect) -> image::RgbaImage {
    let pw = pm.width() as i32;
    let ph = pm.height() as i32;
    let x0 = r.x.floor().max(0.0) as i32;
    let y0 = r.y.floor().max(0.0) as i32;
    let x1 = r.x1().ceil().min(pw as f32) as i32;
    let y1 = r.y1().ceil().min(ph as f32) as i32;
    let w = (x1 - x0).max(1) as u32;
    let h = (y1 - y0).max(1) as u32;
    let data = pm.data();
    let mut out = image::RgbaImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let sx = (x0.max(0) as u32 + x).min(pm.width() - 1);
            let sy = (y0.max(0) as u32 + y).min(pm.height() - 1);
            let si = ((sy * pm.width() + sx) as usize) * 4;
            let a = data[si + 3] as u32;
            let un = |v: u8| -> u8 {
                if a == 0 {
                    0
                } else if a == 255 {
                    v
                } else {
                    (((v as u32 * 255) + a / 2) / a).min(255) as u8
                }
            };
            out.put_pixel(
                x,
                y,
                image::Rgba([un(data[si]), un(data[si + 1]), un(data[si + 2]), data[si + 3]]),
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn square_constraint_keeps_quadrant() {
        let s = Pt::new(10.0, 10.0);
        let r = constrain_square(s, Pt::new(130.0, 50.0));
        assert_eq!((r.x, r.y), (130.0, 130.0));
        let r = constrain_square(s, Pt::new(-40.0, 5.0));
        assert_eq!((r.x, r.y), (-40.0, -40.0));
    }

    #[test]
    fn resize_from_se_corner() {
        let orig = FRect {
            x: 10.0,
            y: 10.0,
            w: 100.0,
            h: 50.0,
        };
        let mods = Modifiers::default();
        let r = resize_rect(Handle::SE, orig, Pt::new(210.0, 160.0), 2.0, &mods, (1000.0, 1000.0));
        assert_eq!(r.x, 10.0);
        assert_eq!(r.y, 10.0);
        assert_eq!(r.w, 200.0);
        assert_eq!(r.h, 150.0);
    }

    #[test]
    fn resize_aspect_lock() {
        let orig = FRect {
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 100.0,
        };
        let mods = Modifiers {
            ctrl: true,
            ..Default::default()
        };
        let r = resize_rect(Handle::SE, orig, Pt::new(200.0, 150.0), 2.0, &mods, (1000.0, 1000.0));
        // width 200, height forced to 100 (aspect 2), centered on y.
        assert_eq!(r.w, 200.0);
        assert!((r.h - 100.0).abs() < 0.01, "{}", r.h);
    }

    #[test]
    fn pixmap_roundtrip_preserves_opaque() {
        let img = image::RgbaImage::from_pixel(4, 4, image::Rgba([10, 20, 30, 255]));
        let pm = image_to_pixmap(&img).unwrap();
        let back = crop_to_image(
            &pm,
            FRect {
                x: 0.0,
                y: 0.0,
                w: 4.0,
                h: 4.0,
            },
        );
        assert_eq!(back.get_pixel(0, 0).0, [10, 20, 30, 255]);
    }

    #[test]
    fn crop_clamps_to_bounds() {
        let pm = Pixmap::new(10, 10).unwrap();
        let img = crop_to_image(
            &pm,
            FRect {
                x: -5.0,
                y: -5.0,
                w: 100.0,
                h: 100.0,
            },
        );
        assert_eq!(img.dimensions(), (10, 10));
    }
}
