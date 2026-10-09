use crate::capture::{self, Shot};
use crate::config::{self, Config};
use crate::export::{self, Task};
use crate::fonts;
use crate::hotkey::{HotEvent, Hotkeys};
use crate::objects::{FRect, Obj, Pt};
use crate::pixbuf::PixBuf;
use crate::theme::{self, Theme};
use crate::uifb::{text_width, C4, Fb};
use crate::wind::{self, Cursor, Driver, Ev, Hwnd, Mods};
use ab_glyph::FontArc;
use chrome::{ToastKind, Ui};
use toolbar::{Act, Toolbar};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod toolbar;
mod chrome;

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

/// A transient bottom-centre notice.
struct Toast {
    text: String,
    kind: ToastKind,
    at: Instant,
}

impl Toast {
    fn new(text: impl Into<String>, kind: ToastKind) -> Self {
        Toast { text: text.into(), kind, at: Instant::now() }
    }

    fn ttl_ms(&self) -> f32 {
        if self.kind == ToastKind::Error { 4000.0 } else { 1600.0 }
    }

    fn age_ms(&self, now: Instant) -> f32 {
        now.saturating_duration_since(self.at).as_secs_f32() * 1000.0
    }

    fn expired(&self, now: Instant) -> bool {
        self.age_ms(now) > self.ttl_ms()
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
    let font = fonts::load_system_font();
    let ui_font = fonts::ui_font().or_else(|| font.clone());
    let mut app = App {
        cfg,
        kind,
        hot,
        pending,
        st: State::Hidden,
        exit_code,
        upload_slot,
        font,
        ui_font,
        notice: None,
        hwnd: Hwnd::default(),
        mouse: (0, 0),
        focus_tries: 0,
    };
    if wind::run(&mut app) != 0 {
        return 1;
    }
    app.exit_code.load(Ordering::SeqCst)
}

// ---------------------------------------------------------------------------
// App state machine
// ---------------------------------------------------------------------------

enum State {
    /// Waiting for a hotkey (daemon) or about to close (one-shot).
    Hidden,
    /// Interactive editor is up.
    Edit(Box<Edit>),
    /// Window is hidden; running export tasks (may show a file dialog).
    Finish(Box<FinishJob>),
}

/// Work to do after the overlay has been hidden again.
struct FinishJob {
    img: PixBuf,
    sel_global: (i32, i32),
    tasks: Vec<Task>,
    cfg: Config,
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
    ui_font: Option<FontArc>,
    notice: Option<Toast>,
    hwnd: Hwnd,
    /// Last known mouse position (client == image coordinates).
    mouse: (i32, i32),
    focus_tries: u8,
}

impl App {
    fn one_shot(&self) -> bool {
        self.kind == RunKind::OneShot
    }

    fn request_exit(&mut self, code: i32) {
        self.exit_code.store(code, Ordering::SeqCst);
        wind::close(self.hwnd);
    }

    /// Run hotkey/upload polling and advance the state machine until stable.
    fn pump(&mut self) {
        let ev = self.hot.as_ref().and_then(|h| h.poll());
        match ev {
            Some(HotEvent::Quit) => {
                self.request_exit(0);
                return;
            }
            Some(HotEvent::Capture) if matches!(self.st, State::Hidden) => {
                self.pending = Some(Pending::editor());
            }
            _ => {}
        }
        self.poll_upload();
        if self.notice.as_ref().is_some_and(|t| t.expired(Instant::now())) {
            self.notice = None;
        }

        for _ in 0..16 {
            match std::mem::replace(&mut self.st, State::Hidden) {
                State::Hidden => {
                    if let Some(pending) = self.pending.take() {
                        self.begin_capture(pending);
                        continue;
                    }
                    break;
                }
                State::Finish(job) => self.run_finish(*job),
                State::Edit(edit) => {
                    let mut edit = *edit;
                    // Windows only delivers keys to the foreground window;
                    // retry focus for a few events after the overlay shows.
                    if self.focus_tries < 5 {
                        self.focus_tries += 1;
                        capture::focus_our_window();
                    }
                    let had = edit.notice.is_some();
                    if edit.notice.as_ref().is_some_and(|t| t.expired(Instant::now())) {
                        edit.notice = None;
                    }
                    if had && edit.notice.is_none() {
                        wind::invalidate(self.hwnd);
                    }
                    self.st = State::Edit(Box::new(edit));
                    break;
                }
            }
        }
    }

    /// Take the edit state out of `self`, hand it to `f`, then finish or
    /// put it back.
    fn edit_tx<F: FnOnce(&mut Self, &mut Edit)>(&mut self, f: F) {
        if !matches!(self.st, State::Edit(_)) {
            return;
        }
        let State::Edit(edit) = std::mem::replace(&mut self.st, State::Hidden) else {
            unreachable!("guarded by matches! above");
        };
        let mut edit = *edit;
        f(self, &mut edit);
        if edit.done {
            let cancelled = edit.cancelled;
            self.finish(&mut edit, cancelled);
        } else {
            self.st = State::Edit(Box::new(edit));
        }
    }

    fn begin_capture(&mut self, mut pending: Pending) {
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
                    wind::close(self.hwnd);
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
        let base = shot.image.clone();
        let composed = base.clone();
        let sel = initial_sel.filter(|r| !r.is_trivial());
        let accept_now = accept_on_select && sel.is_some();
        let th = theme::resolve(&cfg);
        let mut edit = Edit {
            shot,
            base,
            composed,
            objects: Vec::new(),
            hist: vec![Vec::new()],
            hi: 0,
            sel,
            tool: None,
            draft: None,
            stroke_pts: Vec::new(),
            interact: Interact::None,
            color: config::parse_color(&self.cfg.draw_color)
                .map(|(r, g, b, _)| C4::rgb(r, g, b))
                .unwrap_or(C4::rgb(255, 0, 0)),
            sizes: Sizes::from_cfg(&cfg),
            text: None,
            tasks,
            accept_on_select,
            cfg,
            toolbar: None,
            palette_open: false,
            done: false,
            cancelled: false,
            dirty: false,
            notice: None,
            last_wheel: Instant::now(),
            font: self.font.clone(),
            th,
            ui_font: self.ui_font.clone(),
            hover: None,
            hover_at: Instant::now(),
            pressed: None,
            hot_handle: None,
        };
        if accept_now {
            edit.done = true;
            edit.cancelled = false;
        }
        // Place the overlay at the exact physical rect of the shot (the
        // window is DPI-aware and borderless, so outer == inner).
        wind::show_at(
            self.hwnd,
            edit.shot.origin.0,
            edit.shot.origin.1,
            edit.shot.size.0 as i32,
            edit.shot.size.1 as i32,
        );
        self.focus_tries = 0;
        if edit.done {
            let cancelled = edit.cancelled;
            self.finish(&mut edit, cancelled);
        } else {
            self.st = State::Edit(Box::new(edit));
        }
    }

    /// Start the finish sequence: crop now, hide the window, export next
    /// (so a native save dialog is never covered by the overlay).
    fn finish(&mut self, edit: &mut Edit, cancelled: bool) {
        if cancelled {
            if self.one_shot() {
                self.exit_code.store(2, Ordering::SeqCst);
                wind::close(self.hwnd);
            } else {
                wind::hide(self.hwnd);
                self.st = State::Hidden;
            }
            return;
        }
        if edit.dirty {
            edit.rebuild();
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
        wind::hide(self.hwnd);
        self.st = State::Finish(Box::new(FinishJob {
            img,
            sel_global: g,
            tasks: std::mem::take(&mut edit.tasks),
            cfg: edit.cfg.clone(),
        }));
    }

    fn run_finish(&mut self, job: FinishJob) {
        let res = export::run_export(&job.img, job.sel_global, &job.tasks, &job.cfg);
        for m in &res.messages {
            println!("{m}");
        }
        if res.error {
            self.exit_code.store(1, Ordering::SeqCst);
        }
        *self.upload_slot.lock().unwrap() = res.upload;
        wind::hide(self.hwnd);
        match self.kind {
            RunKind::Daemon => self.st = State::Hidden,
            RunKind::OneShot => wind::close(self.hwnd),
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
                self.notice = Some(Toast::new(format!("Uploaded {url}"), ToastKind::Success));
            }
            Ok(Err(e)) => {
                eprintln!("error: upload failed: {e}");
                self.notice = Some(Toast::new(format!("Upload failed: {e}"), ToastKind::Error));
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => *guard = Some(rx),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {}
        }
    }

    // --- input ------------------------------------------------------------

    fn key(&mut self, vk: u32, repeat: bool, mods: Mods) {
        self.edit_tx(|this, edit| this.handle_key(edit, vk, repeat, mods));
    }

    fn handle_key(&mut self, edit: &mut Edit, vk: u32, repeat: bool, mods: Mods) {
        let left = vk == wind::key::LEFT;
        let right = vk == wind::key::RIGHT;
        let up = vk == wind::key::UP;
        let down = vk == wind::key::DOWN;
        let arrows = [left, right, up, down];
        let is_arrow = arrows.iter().any(|a| *a);
        let escape = vk == wind::key::ESCAPE;
        let enter = vk == wind::key::RETURN;
        let editing = vk == wind::key::BACK || vk == wind::key::DELETE;
        // Auto-repeat: selection nudges and text editing keys repeat,
        // everything else is edge-triggered.
        if repeat && !is_arrow && !editing {
            return;
        }

        // Text editing mode consumes keys.
        if edit.text.is_some() {
            if escape {
                edit.text = None;
                return;
            }
            if enter && !mods.shift {
                self.commit_text(edit);
                return;
            }
            let td = edit.text.as_mut().expect("text draft");
            if left {
                td.caret = prev_boundary(&td.text, td.caret);
            } else if right {
                td.caret = next_boundary(&td.text, td.caret);
            } else if vk == wind::key::HOME {
                td.caret = 0;
            } else if vk == wind::key::END {
                td.caret = td.text.len();
            } else if vk == wind::key::BACK && td.caret > 0 {
                let p = prev_boundary(&td.text, td.caret);
                td.text.replace_range(p..td.caret, "");
                td.caret = p;
            } else if vk == wind::key::DELETE {
                let n = next_boundary(&td.text, td.caret);
                td.text.replace_range(td.caret..n, "");
            }
            td.at = Instant::now();
            return;
        }

        if escape {
            if matches!(edit.interact, Interact::None)
                && edit.draft.is_none()
                && !edit.palette_open
                && edit.tool.is_none()
            {
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
            if vk == 'Z' as u32 {
                if mods.shift {
                    edit.redo();
                } else {
                    edit.undo();
                }
                return;
            }
            if vk == 'Y' as u32 {
                edit.redo();
                return;
            }
            if vk == 'C' as u32 {
                self.apply_act(edit, Act::Copy);
                return;
            }
            if vk == 'S' as u32 {
                self.apply_act(edit, Act::Save);
                return;
            }
            if vk == 'U' as u32 {
                self.apply_act(edit, Act::Upload);
                return;
            }
        }

        // Arrow keys nudge / resize the selection.
        if is_arrow && edit.sel.is_some() && !mods.ctrl && !mods.alt {
            let step = edit.shot.scale.round().max(1.0);
            let dir = if left {
                (-1.0, 0.0)
            } else if right {
                (1.0, 0.0)
            } else if up {
                (0.0, -1.0)
            } else {
                (0.0, 1.0)
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
            let tool = tool_for_key(vk);
            if let Some(t) = tool {
                edit.tool = if edit.tool == Some(t) { None } else { Some(t) };
                edit.draft = None;
                edit.stroke_pts.clear();
            }
        }
    }

    fn char_input(&mut self, c: u16) {
        self.edit_tx(|_this, edit| {
            let Some(td) = edit.text.as_mut() else { return };
            let Some(ch) = char::from_u32(c as u32) else { return };
            if ch.is_control() {
                return;
            }
            let mut idx = td.caret.min(td.text.len());
            while idx > 0 && !td.text.is_char_boundary(idx) {
                idx -= 1;
            }
            td.text.insert(idx, ch);
            td.caret = idx + ch.len_utf8();
            td.at = Instant::now();
        });
    }

    fn wheel(&mut self, delta: i32) {
        self.edit_tx(|_this, edit| {
            if edit.text.is_some() {
                return;
            }
            if delta != 0 && edit.last_wheel.elapsed() > Duration::from_millis(160) {
                edit.last_wheel = Instant::now();
                let dir = if delta > 0 { 1 } else { -1 };
                edit.adjust_size(dir);
            }
        });
    }

    fn pointer_down(&mut self, x: i32, y: i32) {
        self.edit_tx(|this, edit| {
            let p = Pt::new(x as f32, y as f32);
            // The toolbar (and open palette) sits on top: it consumes the press.
            let on_bar = edit
                .toolbar
                .as_ref()
                .filter(|tb| tb.contains(p))
                .map(|tb| (tb.act_at(p), tb.index_at(p)));
            if let Some((act, idx)) = on_bar {
                edit.pressed = idx;
                if edit.text.is_some() {
                    this.commit_text(edit);
                }
                if let Some(act) = act {
                    this.apply_act(edit, act);
                }
                return;
            }
            // Text draft: a click outside commits it.
            if edit.text.is_some() {
                let td = edit.text.as_ref().expect("text draft");
                let r = text_box_rect(
                    edit.shot.size.0 as f32,
                    td,
                    edit.sizes.font,
                    edit.font.as_ref(),
                    edit.ui_scale(),
                );
                if !hit(r, p) {
                    let (pos, text) = (td.pos, td.text.clone());
                    edit.text = None;
                    this.commit_text_str(edit, pos, text);
                }
                return;
            }
            if !matches!(edit.interact, Interact::None) {
                return; // already dragging
            }
            if let Some(tool) = edit.tool {
                if tool == Tool::Text {
                    edit.text = Some(TextDraft {
                        pos: p,
                        text: String::new(),
                        caret: 0,
                        at: Instant::now(),
                    });
                    return;
                }
                edit.stroke_pts = vec![p];
                edit.draft = edit.make_draft(tool, p, p);
                edit.interact = Interact::Drawing { start: p };
                return;
            }
            if let Some(sr) = edit.sel {
                for (h, hp) in handle_points(sr) {
                    if (hp.x - p.x).hypot(hp.y - p.y) <= HANDLE_HIT * edit.ui_scale() {
                        let aspect = if sr.h > 0.0 { sr.w / sr.h } else { 1.0 };
                        edit.interact = Interact::Resize {
                            handle: h,
                            orig: sr,
                            aspect,
                        };
                        return;
                    }
                }
                if hit(sr, p) {
                    edit.interact = Interact::MoveSel {
                        start: p,
                        orig: sr,
                    };
                    return;
                }
            }
            edit.interact = Interact::NewSel {
                anchor: p,
                moved: false,
            };
        });
    }

    fn pointer_move(&mut self, x: i32, y: i32) {
        self.edit_tx(|_this, edit| {
            if matches!(edit.interact, Interact::None) {
                return;
            }
            let p = Pt::new(x as f32, y as f32);
            let mods = Mods::current();
            let img = (edit.shot.size.0 as f32, edit.shot.size.1 as f32);
            match &mut edit.interact {
                Interact::None => {}
                Interact::NewSel { anchor, moved } => {
                    let anchor = *anchor;
                    let dist = (p.x - anchor.x).hypot(p.y - anchor.y);
                    if dist >= CLICK_PX {
                        *moved = true;
                    }
                    if *moved {
                        let cur = if mods.shift {
                            constrain_square(anchor, p)
                        } else {
                            p
                        };
                        let mut r = FRect::from_pts(anchor, cur);
                        r.clamp_to(img.0, img.1);
                        edit.sel = Some(r);
                    }
                }
                Interact::MoveSel { start, orig } => {
                    let (start, orig) = (*start, *orig);
                    let mut r = FRect {
                        x: orig.x + p.x - start.x,
                        y: orig.y + p.y - start.y,
                        w: orig.w,
                        h: orig.h,
                    };
                    r.clamp_to(img.0, img.1);
                    edit.sel = Some(r);
                }
                Interact::Resize {
                    handle,
                    orig,
                    aspect,
                } => {
                    let (h, o, a) = (*handle, *orig, *aspect);
                    edit.sel = Some(resize_rect(h, o, p, a, &mods, img));
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
                                edit.snap_point(start, p, true)
                            } else {
                                p
                            }
                        }
                        Tool::Rect
                        | Tool::Ellipse
                        | Tool::Pixelate
                        | Tool::Invert
                            if mods.shift =>
                        {
                            constrain_square(start, p)
                        }
                        _ => p,
                    };
                    if matches!(tool, Tool::Path | Tool::Marker) {
                        let last = edit.stroke_pts.last().copied();
                        let need = last
                            .map(|l| (cur.x - l.x).hypot(cur.y - l.y) >= 2.0)
                            .unwrap_or(true);
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
                                color: color.with_alpha(90),
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
        });
    }

    fn pointer_up(&mut self) {
        self.edit_tx(|_this, edit| {
            edit.pressed = None;
            end_interaction(edit);
        });
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

    fn apply_act(&mut self, edit: &mut Edit, act: Act) {
        match act {
            Act::Tool(t) => {
                edit.tool = if edit.tool == Some(t) { None } else { Some(t) };
                edit.draft = None;
                edit.stroke_pts.clear();
            }
            Act::Undo => edit.undo(),
            Act::Redo => edit.redo(),
            Act::Size(d) => edit.adjust_size(d),
            Act::Color(c) => {
                edit.color = c;
                edit.palette_open = false;
            }
            Act::Palette => edit.palette_open = !edit.palette_open,
            Act::Copy => {
                edit.tasks = vec![Task::Copy];
                edit.done = true;
            }
            Act::Save => {
                edit.tasks = vec![Task::Save { path: None }];
                edit.done = true;
            }
            Act::Upload => {
                edit.tasks = vec![Task::Upload];
                edit.done = true;
            }
            Act::Exit => {
                edit.cancelled = true;
                edit.done = true;
            }
            Act::Accept => edit.done = true,
        }
    }
}

impl Driver for App {
    fn on_create(&mut self, hwnd: Hwnd) {
        self.hwnd = hwnd;
        self.pump();
    }

    fn on_event(&mut self, ev: Ev) {
        match ev {
            Ev::Move { x, y } => {
                self.mouse = (x, y);
                let dragging =
                    matches!(&self.st, State::Edit(e) if !matches!(e.interact, Interact::None));
                if dragging {
                    self.pointer_move(x, y);
                    wind::invalidate(self.hwnd);
                } else if let State::Edit(e) = &mut self.st
                    && e.track_hover(Pt::new(x as f32, y as f32))
                {
                    wind::invalidate(self.hwnd);
                }
            }
            Ev::Down { x, y } => {
                self.mouse = (x, y);
                self.pointer_down(x, y);
            }
            Ev::Up { x, y } => {
                self.mouse = (x, y);
                self.pointer_up();
            }
            Ev::Wheel { delta, x, y } => {
                self.mouse = (x, y);
                self.wheel(delta);
            }
            Ev::Key { vk, up, repeat, mods } => {
                if !up {
                    self.key(vk, repeat, mods);
                }
            }
            Ev::Char(c) => self.char_input(c),
            Ev::Timer => {}
        }
        self.pump();
    }

    fn frame(&mut self) -> Option<PixBuf> {
        let State::Edit(edit) = &mut self.st else { return None };
        if edit.dirty {
            edit.rebuild();
        }
        let (ww, wh) = (edit.shot.size.0 as f32, edit.shot.size.1 as f32);
        let s = edit.ui_scale();
        let colors = palette_colors(&edit.cfg);
        edit.toolbar = edit.sel.map(|sel| {
            toolbar::layout(&toolbar::Input {
                sel,
                screen: (ww, wh),
                s,
                busy: !edit.tasks.is_empty(),
                can_undo: edit.hi > 0,
                can_redo: edit.hi + 1 < edit.hist.len(),
                palette: edit.palette_open.then_some(colors.as_slice()),
            })
        });

        // Compose: base + objects, themed dim outside the selection, draft.
        let mut img = edit.composed.clone();
        let dim = edit.th.dim.with_alpha(edit.th.dim_alpha(edit.cfg.contrast_opacity));
        match edit.sel {
            None => dim_rect(&mut img, 0.0, 0.0, ww, wh, dim),
            Some(sr) => {
                dim_rect(&mut img, 0.0, 0.0, ww, sr.y, dim);
                dim_rect(&mut img, 0.0, sr.y1(), ww, wh - sr.y1(), dim);
                dim_rect(&mut img, 0.0, sr.y, sr.x, sr.h, dim);
                dim_rect(&mut img, sr.x1(), sr.y, ww - sr.x1(), sr.h, dim);
            }
        }
        if let Some(d) = &edit.draft {
            d.render(&mut img, edit.font.as_ref());
        }

        // Chrome, drawn into the display buffer.
        let stride = img.width() as usize;
        let mut f = Fb::new(img.as_raw_mut(), stride);
        let ui = Ui { th: &edit.th, s, font: edit.ui_font.as_ref() };
        let interacting = !matches!(edit.interact, Interact::None);

        if let Some(sr) = edit.sel {
            chrome::selection(&mut f, &ui, sr, edit.hot_handle, 1.0);
            chrome::size_label(&mut f, &ui, sr, !interacting, ww, 1.0);
        }
        if let Some(td) = &edit.text {
            let r = text_box_rect(ww, td, edit.sizes.font, edit.font.as_ref(), s);
            f.stroke_dashed_rect(r, 4.0 * s, 3.0 * s, s.round().max(1.0), edit.th.accent);
            if let Some(font) = edit.font.as_ref() {
                let px = edit.sizes.font;
                f.draw_text(font, px, &td.text, td.pos.x, td.pos.y, edit.color);
                if (td.at.elapsed().as_millis() / 530).is_multiple_of(2) {
                    let before = td.text.get(..td.caret).unwrap_or("");
                    let cx = td.pos.x + text_width(font, px, before);
                    f.fill_rect(
                        cx.round() as i32,
                        td.pos.y.round() as i32,
                        (2.0 * s).round() as i32,
                        (px * 1.15).round() as i32,
                        edit.color,
                    );
                }
            }
        }
        if let Some(t) = edit.notice.as_ref().or(self.notice.as_ref()) {
            chrome::toast(&mut f, &ui, &t.text, t.kind, (ww, wh), 1.0);
        }
        if let Some(tb) = &edit.toolbar {
            let (value, unit) = edit.size_label();
            let st = chrome::BarState {
                hover: edit.hover,
                hover_k: 1.0,
                pressed: edit.pressed,
                tool: edit.tool,
                color: edit.color,
                value: &value,
                unit,
                pop_k: 1.0,
            };
            chrome::toolbar(&mut f, &ui, tb, &st, 1.0);
            if !interacting
                && edit.hover_at.elapsed() >= Duration::from_millis(400)
                && let Some(it) = edit.hover.and_then(|i| tb.items.get(i))
                && let toolbar::Kind::Btn(act) = it.kind
            {
                let (label, keys) = chrome::act_tip(act);
                chrome::tooltip(&mut f, &ui, it.r, label, keys, ww, 1.0);
            }
        }
        if edit.sel.is_none() && !interacting {
            chrome::hint(&mut f, &ui, (ww, wh), 1.0);
        }
        Some(img)
    }

    fn cursor(&self) -> Cursor {
        let State::Edit(edit) = &self.st else { return Cursor::Arrow };
        if edit.text.is_some() {
            return Cursor::IBeam;
        }
        if edit.tool.is_some() {
            return Cursor::Cross;
        }
        let p = Pt::new(self.mouse.0 as f32, self.mouse.1 as f32);
        if let Some(sr) = edit.sel {
            if let Some((h, _)) = handle_points(sr)
                .into_iter()
                .find(|(_, q)| (q.x - p.x).hypot(q.y - p.y) <= HANDLE_HIT * edit.ui_scale())
            {
                return handle_cursor(h);
            }
            if hit(sr, p) {
                return Cursor::Move;
            }
        }
        Cursor::Cross
    }
}

// ---------------------------------------------------------------------------
// The editor
// ---------------------------------------------------------------------------

/// Plain-letter tool shortcuts (Flameshot-style).
fn tool_for_key(vk: u32) -> Option<Tool> {
    match char::from_u32(vk)? {
        'P' => Some(Tool::Path),
        'D' | 'L' => Some(Tool::Line),
        'A' => Some(Tool::Arrow),
        'R' => Some(Tool::Rect),
        'C' => Some(Tool::Ellipse),
        'M' => Some(Tool::Marker),
        'T' => Some(Tool::Text),
        'B' => Some(Tool::Pixelate),
        'I' => Some(Tool::Invert),
        _ => None,
    }
}

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
    shape: f32,
}

impl Sizes {
    fn from_cfg(cfg: &Config) -> Self {
        Self {
            line: cfg.draw_thickness.clamp(1.0, 50.0),
            marker: cfg.draw_marker_size.clamp(1.0, 50.0),
            pixelate: cfg.draw_pixelate_size.clamp(4.0, 100.0),
            font: cfg.draw_font_size.clamp(8.0, 96.0),
            shape: cfg.draw_thickness.clamp(1.0, 50.0),
        }
    }
    fn active_mut(&mut self, tool: Option<Tool>) -> &mut f32 {
        match tool {
            Some(Tool::Rect | Tool::Ellipse) => &mut self.shape,
            Some(Tool::Marker) => &mut self.marker,
            Some(Tool::Pixelate | Tool::Invert) => &mut self.pixelate,
            Some(Tool::Text) => &mut self.font,
            _ => &mut self.line,
        }
    }
}

struct TextDraft {
    pos: Pt,
    text: String,
    /// Byte index of the caret inside `text`.
    caret: usize,
    /// Last edit, for the caret blink phase.
    at: Instant,
}

struct Edit {
    shot: Shot,
    base: PixBuf,
    composed: PixBuf,
    objects: Vec<Obj>,
    hist: Vec<Vec<Obj>>,
    hi: usize,
    sel: Option<FRect>,
    tool: Option<Tool>,
    draft: Option<Obj>,
    stroke_pts: Vec<Pt>,
    interact: Interact,
    color: C4,
    sizes: Sizes,
    text: Option<TextDraft>,
    tasks: Vec<Task>,
    accept_on_select: bool,
    cfg: Config,
    toolbar: Option<Toolbar>,
    palette_open: bool,
    done: bool,
    cancelled: bool,
    dirty: bool,
    notice: Option<Toast>,
    last_wheel: Instant,
    font: Option<FontArc>,
    th: Theme,
    ui_font: Option<FontArc>,
    /// Toolbar item under the pointer and since when (tooltip delay).
    hover: Option<usize>,
    hover_at: Instant,
    pressed: Option<usize>,
    /// Selection handle under the pointer (ring grows to 3 px).
    hot_handle: Option<usize>,
}

const HANDLE_HIT: f32 = 10.0; // handle hit radius, logical px (20 px target)
const CLICK_PX: f32 = 2.5; // movement below this counts as a click

impl Edit {
    fn rebuild(&mut self) {
        let mut pm = self.base.clone();
        for o in &self.objects {
            o.render(&mut pm, self.font.as_ref());
        }
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

    fn adjust_size(&mut self, dir: i32) {
        let v = self.sizes.active_mut(self.tool);
        let (lo, hi, step) = match self.tool {
            Some(Tool::Pixelate | Tool::Invert) => (4.0, 100.0, 1.0),
            Some(Tool::Text) => (8.0, 96.0, 1.0),
            _ => (1.0, 50.0, 1.0),
        };
        *v = (*v + dir as f32 * step).clamp(lo, hi);
        self.notice = Some(Toast::new(format!("Size {}", *v as i32), ToastKind::Info));
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
                width: self.sizes.shape,
            },
            Tool::Ellipse => Obj::Ellipse {
                r: FRect::from_pts(a, b),
                color: c,
                width: self.sizes.shape,
            },
            Tool::Marker => Obj::Marker {
                a,
                b,
                color: c.with_alpha(90),
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

    /// Toolbar readout: current tool group's size and its name.
    fn size_label(&self) -> (String, &'static str) {
        let (v, unit) = match self.tool {
            Some(Tool::Rect | Tool::Ellipse) => (self.sizes.shape, "shape"),
            Some(Tool::Marker) => (self.sizes.marker, "mark"),
            Some(Tool::Text) => (self.sizes.font, "text"),
            Some(Tool::Pixelate | Tool::Invert) => (self.sizes.pixelate, "block"),
            _ => (self.sizes.line, "line"),
        };
        ((v as i32).to_string(), unit)
    }

    fn ui_scale(&self) -> f32 {
        self.shot.scale.clamp(1.0, 4.0)
    }

    /// Track the hovered toolbar item / handle; true when either changed.
    fn track_hover(&mut self, p: Pt) -> bool {
        let hover = self.toolbar.as_ref().and_then(|tb| tb.index_at(p));
        let reach = HANDLE_HIT * self.ui_scale();
        let hot = self.sel.and_then(|sr| {
            handle_points(sr).iter().position(|(_, q)| (q.x - p.x).hypot(q.y - p.y) <= reach)
        });
        let changed = hover != self.hover || hot != self.hot_handle;
        if hover != self.hover {
            self.hover = hover;
            self.hover_at = Instant::now();
        }
        self.hot_handle = hot;
        changed
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn hit(r: FRect, p: Pt) -> bool {
    p.x >= r.x && p.x < r.x1() && p.y >= r.y && p.y < r.y1()
}

fn end_interaction(edit: &mut Edit) {
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

fn handle_points(sr: FRect) -> [(Handle, Pt); 8] {
    let (l, r) = (sr.x, sr.x1());
    let (t, b) = (sr.y, sr.y1());
    let (cx, cy) = (sr.x + sr.w / 2.0, sr.y + sr.h / 2.0);
    [
        (Handle::NW, Pt::new(l, t)),
        (Handle::N, Pt::new(cx, t)),
        (Handle::NE, Pt::new(r, t)),
        (Handle::E, Pt::new(r, cy)),
        (Handle::SE, Pt::new(r, b)),
        (Handle::S, Pt::new(cx, b)),
        (Handle::SW, Pt::new(l, b)),
        (Handle::W, Pt::new(l, cy)),
    ]
}

fn handle_cursor(h: Handle) -> Cursor {
    match h {
        Handle::NW | Handle::SE => Cursor::SizeNWSE,
        Handle::NE | Handle::SW => Cursor::SizeNESW,
        Handle::N | Handle::S => Cursor::SizeNS,
        Handle::E | Handle::W => Cursor::SizeWE,
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
    mods: &Mods,
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

/// Dashed box around the text draft: 4 px padding, at least 24×28; the text
/// itself is drawn at `td.pos`, exactly where the committed object renders.
fn text_box_rect(win_w: f32, td: &TextDraft, font_px: f32, font: Option<&FontArc>, s: f32) -> FRect {
    let pad = 4.0 * s;
    let tw = font.map(|f| text_width(f, font_px, &td.text)).unwrap_or(0.0);
    let w = (tw + 2.0 * pad + 2.0 * s).max(24.0 * s);
    let h = (font_px * 1.15 + 2.0 * pad).max(28.0 * s);
    let x = (td.pos.x - pad).min((win_w - w).max(0.0));
    FRect { x, y: td.pos.y - pad, w, h }
}

fn palette_colors(cfg: &Config) -> Vec<C4> {
    cfg.user_colors
        .iter()
        .filter_map(|c| config::parse_color(c).map(|(r, g, b, a)| C4::new(r, g, b, a)))
        .collect()
}

fn prev_boundary(s: &str, i: usize) -> usize {
    s[..i]
        .char_indices()
        .next_back()
        .map(|(k, _)| k)
        .unwrap_or(0)
}

fn next_boundary(s: &str, i: usize) -> usize {
    s[i..].char_indices().nth(1).map(|(k, _)| i + k).unwrap_or(s.len())
}

/// Blend a rect of the (opaque) image toward `c` by `c.a`.
fn dim_rect(img: &mut PixBuf, x: f32, y: f32, w: f32, h: f32, c: C4) {
    if w <= 0.0 || h <= 0.0 || c.a == 0 {
        return;
    }
    let (bw, bh) = (img.width() as i32, img.height() as i32);
    let x0 = x.floor().max(0.0) as i32;
    let y0 = y.floor().max(0.0) as i32;
    let x1 = x1_clamp(x, w, bw);
    let y1 = y1_clamp(y, h, bh);
    let a = c.a as f32 / 255.0;
    let k = 1.0 - a;
    let src = [c.r, c.g, c.b];
    let data = img.as_raw_mut();
    for y in y0.max(0)..y1.min(bh) {
        for x in x0.max(0)..x1.min(bw) {
            let i = ((y * bw + x) as usize) * 4;
            for (ch, sv) in src.iter().enumerate() {
                data[i + ch] = (data[i + ch] as f32 * k + *sv as f32 * a).round() as u8;
            }
        }
    }
}

fn x1_clamp(x: f32, w: f32, bw: i32) -> i32 {
    (x + w).ceil().min(bw as f32) as i32
}

fn y1_clamp(y: f32, h: f32, bh: i32) -> i32 {
    (y + h).ceil().min(bh as f32) as i32
}

/// Crop a rect from an unpremultiplied RGBA image (clamped to bounds).
fn crop_to_image(img: &PixBuf, r: FRect) -> PixBuf {
    let pw = img.width() as i32;
    let ph = img.height() as i32;
    let x0 = r.x.floor().max(0.0) as i32;
    let y0 = r.y.floor().max(0.0) as i32;
    let x1 = x1_clamp(r.x, r.w, pw);
    let y1 = y1_clamp(r.y, r.h, ph);
    let w = (x1 - x0).max(1) as u32;
    let h = (y1 - y0).max(1) as u32;
    let data = img.as_raw();
    let mut out = PixBuf::new(w, h);
    let dst = out.as_raw_mut();
    for y in 0..h {
        for x in 0..w {
            let sx = (x0.max(0) as u32 + x).min(img.width() - 1);
            let sy = (y0.max(0) as u32 + y).min(img.height() - 1);
            let si = ((sy * img.width() + sx) as usize) * 4;
            let di = ((y * w + x) as usize) * 4;
            dst[di..di + 4].copy_from_slice(&data[si..si + 4]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- developer preview (renders overlay frames to PNG; no display) ----

    fn synthetic_shot() -> Shot {
        let (w, h) = (1440u32, 900u32);
        let mut img = PixBuf::from_pixel(w, h, [0, 0, 0, 255]);
        let mut put = |x0: u32, y0: u32, x1: u32, y1: u32, c: [u8; 3]| {
            for y in y0..y1.min(h) {
                for x in x0..x1.min(w) {
                    let i = (y as usize * w as usize + x as usize) * 4;
                    img.as_raw_mut()[i..i + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
                }
            }
        };
        for y in 0..h {
            let t = y as f32 / h as f32;
            put(0, y, w, y + 1, [(20.0 + 20.0 * t) as u8, (40.0 + 40.0 * t) as u8, (90.0 + 60.0 * t) as u8]);
        }
        put(120, 90, 1320, 810, [245, 246, 248]); // window
        put(120, 90, 1320, 130, [225, 228, 233]); // title bar
        put(120, 130, 340, 810, [236, 239, 243]); // sidebar
        for i in 0..8 {
            put(140, 160 + i * 40, 320, 182 + i * 40, [200, 206, 216]);
        }
        for i in 0..6 {
            put(370, 160 + i * 60, 1290 - i * 90, 172 + i * 60, [180, 188, 200]);
        }
        put(370, 540, 700, 760, [92, 140, 230]);
        put(740, 540, 1060, 760, [240, 170, 70]);
        put(1100, 540, 1290, 760, [110, 190, 140]);
        Shot { origin: (0, 0), size: (w, h), scale: 1.0, image: img }
    }

    fn preview_app(th: Theme, sel: Option<FRect>) -> App {
        let theme_name = if th == theme::DARK { "dark" } else { "light" };
        let cfg = Config { theme: theme_name.into(), ..Config::default() };
        let font = fonts::load_system_font();
        let ui_font = fonts::ui_font().or_else(|| font.clone());
        let shot = synthetic_shot();
        let base = shot.image.clone();
        let edit = Edit {
            shot,
            composed: base.clone(),
            base,
            objects: Vec::new(),
            hist: vec![Vec::new()],
            hi: 0,
            sel,
            tool: None,
            draft: None,
            stroke_pts: Vec::new(),
            interact: Interact::None,
            color: C4::rgb(240, 68, 56),
            sizes: Sizes::from_cfg(&cfg),
            text: None,
            tasks: Vec::new(),
            accept_on_select: false,
            cfg: cfg.clone(),
            toolbar: None,
            palette_open: false,
            done: false,
            cancelled: false,
            dirty: false,
            notice: None,
            last_wheel: Instant::now(),
            font: font.clone(),
            th,
            ui_font: ui_font.clone(),
            hover: None,
            hover_at: Instant::now(),
            pressed: None,
            hot_handle: None,
        };
        App {
            cfg,
            kind: RunKind::OneShot,
            hot: None,
            pending: None,
            st: State::Edit(Box::new(edit)),
            exit_code: Arc::new(AtomicI32::new(0)),
            upload_slot: Arc::new(Mutex::new(None)),
            font,
            ui_font,
            notice: None,
            hwnd: Hwnd::default(),
            mouse: (0, 0),
            focus_tries: 0,
        }
    }

    fn edit_of(app: &mut App) -> &mut Edit {
        match &mut app.st {
            State::Edit(e) => e,
            _ => unreachable!(),
        }
    }

    /// Selection with the Arrow tool, a Rect and an Arrow object inside it.
    fn annotated(th: Theme, sel: FRect) -> App {
        let mut app = preview_app(th, Some(sel));
        let e = edit_of(&mut app);
        e.tool = Some(Tool::Arrow);
        let red = C4::rgb(240, 68, 56);
        e.objects.push(Obj::Rect {
            r: FRect { x: sel.x + 40.0, y: sel.y + 40.0, w: sel.w * 0.35, h: sel.h * 0.3 },
            color: red,
            width: 3.0,
        });
        e.objects.push(Obj::Arrow {
            a: Pt::new(sel.x + sel.w * 0.8, sel.y + sel.h * 0.8),
            b: Pt::new(sel.x + sel.w * 0.5, sel.y + sel.h * 0.5),
            color: red,
            width: 4.0,
        });
        e.dirty = true;
        app
    }

    #[test]
    #[ignore = "writes preview PNGs; set RUSTSHOT_PREVIEW_DIR"]
    fn render_preview_pngs() {
        let Some(dir) = std::env::var_os("RUSTSHOT_PREVIEW_DIR") else { return };
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        let save = |app: &mut App, name: &str| {
            let png = app.frame().expect("frame").to_png().unwrap();
            std::fs::write(dir.join(name), png).unwrap();
        };
        let big = FRect { x: 240.0, y: 140.0, w: 960.0, h: 540.0 };

        let mut app = annotated(theme::DARK, big);
        app.frame();
        let e = edit_of(&mut app);
        let copy = e
            .toolbar
            .as_ref()
            .and_then(|t| t.items.iter().position(|i| i.kind == toolbar::Kind::Btn(Act::Copy)))
            .expect("copy button");
        e.hover = Some(copy);
        e.hover_at = Instant::now() - Duration::from_secs(2);
        e.notice = Some(Toast::new("Size 4", ToastKind::Info));
        save(&mut app, "dark-toolbar.png");

        let e = edit_of(&mut app);
        e.palette_open = true;
        e.hover = None;
        save(&mut app, "dark-palette.png");

        let mut app = annotated(theme::DARK, FRect { x: 500.0, y: 300.0, w: 380.0, h: 200.0 });
        save(&mut app, "dark-narrow.png");

        let mut app = annotated(theme::LIGHT, big);
        let e = edit_of(&mut app);
        e.hover = Some(copy);
        e.hover_at = Instant::now() - Duration::from_secs(2);
        e.notice = Some(Toast::new("Size 4", ToastKind::Info));
        save(&mut app, "light-toolbar.png");

        let mut app = annotated(theme::DARK, big);
        let e = edit_of(&mut app);
        e.tool = Some(Tool::Text);
        let text = "Check this".to_string();
        e.text = Some(TextDraft { pos: Pt::new(400.0, 300.0), caret: text.len(), text, at: Instant::now() });
        save(&mut app, "dark-text.png");

        let mut app = preview_app(theme::DARK, None);
        save(&mut app, "dark-hint.png");
    }

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
        let mods = Mods::default();
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
        let mods = Mods {
            ctrl: true,
            ..Default::default()
        };
        let r = resize_rect(Handle::SE, orig, Pt::new(200.0, 150.0), 2.0, &mods, (1000.0, 1000.0));
        // width 200, height forced to 100 (aspect 2), centered on y.
        assert_eq!(r.w, 200.0);
        assert!((r.h - 100.0).abs() < 0.01, "{}", r.h);
    }

    #[test]
    fn crop_roundtrip_preserves_opaque() {
        let img = PixBuf::from_pixel(4, 4, [10, 20, 30, 255]);
        let back = crop_to_image(
            &img,
            FRect {
                x: 0.0,
                y: 0.0,
                w: 4.0,
                h: 4.0,
            },
        );
        assert_eq!(back.get_pixel(0, 0), [10, 20, 30, 255]);
    }

    #[test]
    fn crop_clamps_to_bounds() {
        let pm = PixBuf::new(10, 10);
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

    #[test]
    fn caret_boundaries_stay_on_chars() {
        let s = "héllo";
        assert_eq!(next_boundary(s, 0), 1);
        assert_eq!(next_boundary(s, 1), 3);
        assert_eq!(prev_boundary(s, 3), 1);
        assert_eq!(prev_boundary(s, 0), 0);
        assert_eq!(next_boundary(s, s.len()), s.len());
    }

    #[test]
    fn text_box_pads_around_the_draft() {
        let td = TextDraft { pos: Pt::new(100.0, 50.0), text: String::new(), caret: 0, at: Instant::now() };
        let r = text_box_rect(1000.0, &td, 16.0, None, 1.0);
        assert_eq!((r.x, r.y), (96.0, 46.0));
        assert_eq!((r.w, r.h), (24.0, 28.0));
        let r = text_box_rect(110.0, &td, 16.0, None, 1.0);
        assert_eq!(r.x, 86.0, "kept on screen");
    }
}
