use crate::capture::{self, Shot};
use crate::config::{self, Config};
use crate::export::{self, Task};
use crate::hotkey::{HotEvent, Hotkeys};
use crate::icons::Icons;
use crate::objects::{FRect, Obj, Pt};
use crate::pixbuf::PixBuf;
use crate::uifb::{text_height, text_width, C4, Fb};
use crate::wind::{self, Cursor, Driver, Ev, Hwnd, Mods};
use ab_glyph::FontArc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[allow(dead_code)] // wired in Task 8
mod toolbar;

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
    let font = load_font();
    let icons = Icons::load();
    let mut app = App {
        cfg,
        kind,
        hot,
        pending,
        st: State::Hidden,
        exit_code,
        upload_slot,
        font,
        icons,
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

/// Load a Windows system font. Deliberately avoids embedding TTFs (~1.4 MB):
/// we render UI and baked text annotations with system fonts.
fn load_font() -> Option<FontArc> {
    const CANDIDATES: [&str; 4] = [
        "C:\\Windows\\Fonts\\segoeui.ttf",
        "C:\\Windows\\Fonts\\arial.ttf",
        "C:\\Windows\\Fonts\\tahoma.ttf",
        "C:\\Windows\\Fonts\\verdana.ttf",
    ];
    for c in CANDIDATES {
        if let Ok(b) = std::fs::read(c)
            && let Ok(f) = FontArc::try_from_vec(b) {
                return Some(f);
            }
    }
    None
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
    icons: Icons,
    notice: Option<(String, Instant)>,
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
        if let Some((_, at)) = &self.notice
            && at.elapsed() > Duration::from_millis(1500) {
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
                    if let Some((_, at)) = &edit.notice
                        && at.elapsed() > Duration::from_millis(1500) {
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
                self.notice = Some((format!("uploaded {url}"), Instant::now()));
            }
            Ok(Err(e)) => eprintln!("error: upload failed: {e}"),
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
            let tool = match vk {
                v if v == 'P' as u32 => Some(Tool::Path),
                v if v == 'D' as u32 || v == 'L' as u32 => Some(Tool::Line),
                v if v == 'A' as u32 => Some(Tool::Arrow),
                v if v == 'R' as u32 => Some(Tool::Rect),
                v if v == 'C' as u32 => Some(Tool::Ellipse),
                v if v == 'M' as u32 => Some(Tool::Marker),
                v if v == 'T' as u32 => Some(Tool::Text),
                v if v == 'B' as u32 => Some(Tool::Pixelate),
                v if v == 'I' as u32 => Some(Tool::Invert),
                _ => None,
            };
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
            // The toolbar sits on top: it consumes the press.
            let tb = edit
                .toolbar
                .as_ref()
                .map(|tb| (hit(tb.rect, p), tb.hit(p)));
            if let Some((true, act)) = tb {
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
                let r = text_box_rect(edit.shot.size.0 as f32, td, edit.sizes.font);
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
                    if (hp.x - p.x).hypot(hp.y - p.y) <= HANDLE_PX * 1.4 {
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
        self.edit_tx(|_this, edit| end_interaction(edit));
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
        layout(edit);
        let (ww, wh) = (edit.shot.size.0 as f32, edit.shot.size.1 as f32);

        // Compose: base + objects, then dim outside the selection, then
        // the live draft (painted over the dim, like the old overlay).
        let mut img = edit.composed.clone();
        let dim = edit.cfg.contrast_opacity;
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
        let accent = edit.accent();
        let font = edit.font.clone();

        if let Some(sr) = edit.sel {
            f.stroke_rect(sr.x, sr.y, sr.w, sr.h, 1.4, accent);
            for (_h, hp) in handle_points(sr) {
                let hx = (hp.x - HANDLE_PX).round() as i32;
                let hy = (hp.y - HANDLE_PX).round() as i32;
                f.fill_rounded(hx, hy, 12, 12, 2.0, accent);
                f.stroke_rect(
                    hx as f32,
                    hy as f32,
                    HANDLE_PX * 2.0,
                    HANDLE_PX * 2.0,
                    1.0,
                    C4::new(255, 255, 255, 220),
                );
            }
            if let Some(font) = font.as_ref() {
                let label = format!(
                    "{}x{}",
                    sr.w.round() as i32,
                    sr.h.round() as i32
                );
                let tw = text_width(font, 12.0, &label);
                let th = text_height(font, 12.0);
                let lx = sr.x;
                let ly = sr.y1() + 16.0;
                f.fill_rounded(
                    (lx - 3.0) as i32,
                    (ly - th / 2.0 - 3.0) as i32,
                    (tw + 6.0) as i32,
                    (th + 6.0) as i32,
                    2.0,
                    C4::black_alpha(190),
                );
                f.draw_text(
                    font,
                    12.0,
                    &label,
                    lx,
                    ly - th / 2.0,
                    C4::rgb(255, 255, 255),
                );
            }
        }

        // Notice (tool size etc.), bottom center.
        let notice = edit
            .notice
            .as_ref()
            .map(|(t, _)| t.as_str())
            .or_else(|| self.notice.as_ref().map(|(t, _)| t.as_str()));
        if let (Some(font), Some(t)) = (font.as_ref(), notice) {
            let tw = text_width(font, 14.0, t);
            let th = text_height(font, 14.0);
            let cx = ww / 2.0;
            let cy = wh - 36.0;
            f.fill_rounded(
                (cx - tw / 2.0 - 6.0) as i32,
                (cy - th / 2.0 - 6.0) as i32,
                (tw + 12.0) as i32,
                (th + 12.0) as i32,
                4.0,
                C4::black_alpha(200),
            );
            f.draw_text_centered(
                font,
                14.0,
                t,
                cx,
                cy,
                C4::rgb(255, 255, 255),
            );
        }

        // Text draft box.
        if let Some(td) = &edit.text
            && let Some(font) = font.as_ref()
        {
            let r = text_box_rect(ww, td, edit.sizes.font);
            f.fill_rounded(
                r.x as i32,
                r.y as i32,
                r.w as i32,
                r.h as i32,
                3.0,
                C4::new(255, 255, 255, 235),
            );
            f.stroke_rect(r.x, r.y, r.w, r.h, 1.0, accent);
            let px = edit.sizes.font;
            let tx = r.x + 6.0;
            let ty = r.y + (r.h - text_height(font, px)) / 2.0;
            let shown = td.text.get(..td.caret).unwrap_or("");
            f.draw_text(font, px, shown, tx + 1.0, ty + 1.0, C4::black_alpha(140));
            f.draw_text(font, px, shown, tx, ty, edit.color);
            let caret_x = tx + text_width(font, px, shown);
            f.fill_rect(
                caret_x as i32,
                (ty + 1.0) as i32,
                1,
                text_height(font, px) as i32,
                C4::rgb(20, 20, 20),
            );
        }

        // Toolbar, on top of everything.
        if let Some(tb) = &edit.toolbar {
            f.fill_rounded(
                tb.rect.x as i32,
                tb.rect.y as i32,
                tb.rect.w as i32,
                tb.rect.h as i32,
                6.0,
                C4::black_alpha(175),
            );
            for (tile, r) in &tb.items {
                let (cx0, cy0, cw, ch) = (r.x as i32, r.y as i32, r.w as i32, r.h as i32);
                match tile {
                    Tile::Btn { icon, label, fill, .. } => {
                        f.fill_rounded(cx0, cy0, cw, ch, 4.0, *fill);
                        if !icon.is_empty()
                            && let Some(ic) = self.icons.get(icon)
                        {
                            f.blit(
                                cx0 + (cw - ICON_PX as i32) / 2,
                                cy0 + (ch - ICON_PX as i32) / 2,
                                ic,
                            );
                        } else if !label.is_empty()
                            && let Some(font) = font.as_ref()
                        {
                            let tw = text_width(font, 11.0, label);
                            f.draw_text(
                                font,
                                11.0,
                                label,
                                r.x + (r.w - tw) / 2.0,
                                r.y + (r.h - 11.0) / 2.0,
                                C4::rgb(255, 255, 255),
                            );
                        }
                    }
                    Tile::Sep => {
                        f.fill_rect(cx0 + cw / 2, cy0 + 3, 1, (ch - 6).max(1), C4::rgb(80, 80, 80));
                    }
                    Tile::Text(s) => {
                        if let Some(font) = font.as_ref() {
                            f.draw_text_centered(
                                font,
                                12.0,
                                s,
                                r.x + r.w / 2.0,
                                r.y + r.h / 2.0,
                                C4::rgb(230, 230, 230),
                            );
                        }
                    }
                    Tile::Swatch(_, c) => {
                        f.fill_rounded(cx0, cy0, cw, ch, 3.0, *c);
                        f.stroke_rect(
                            r.x + 0.5,
                            r.y + 0.5,
                            r.w - 1.0,
                            r.h - 1.0,
                            1.0,
                            C4::new(255, 255, 255, 90),
                        );
                    }
                }
            }
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
                .find(|(_, q)| (q.x - p.x).hypot(q.y - p.y) <= HANDLE_PX * 1.4)
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
    /// Byte index of the caret inside `text`.
    caret: usize,
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
    notice: Option<(String, Instant)>,
    last_wheel: Instant,
    font: Option<FontArc>,
}

const HANDLE_PX: f32 = 6.0; // half-size of selection handles, in points
const CLICK_PX: f32 = 2.5; // movement below this counts as a click
const BTN: f32 = 26.0; // toolbar button size
const ICON_PX: f32 = 20.0; // toolbar icon size
const PAD: f32 = 5.0; // toolbar frame padding
const GAP: f32 = 3.0; // toolbar item gap

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

    fn accent(&self) -> C4 {
        config::parse_color(&self.cfg.ui_color)
            .map(|(r, g, b, _)| C4::rgb(r, g, b))
            .unwrap_or(C4::rgb(0x74, 0x00, 0x96))
    }
}

// ---------------------------------------------------------------------------
// Toolbar layout + actions
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Act {
    Tool(Tool),
    Undo,
    Redo,
    Size(i32),
    Color(C4),
    Palette,
    Copy,
    Save,
    Upload,
    Exit,
    Accept,
}

enum Tile {
    Btn {
        act: Act,
        icon: &'static str,
        label: &'static str,
        fill: C4,
    },
    Sep,
    Text(String),
    Swatch(Act, C4),
}

impl Tile {
    fn size(&self, font: Option<&FontArc>) -> (f32, f32) {
        match self {
            Tile::Btn { .. } => (BTN, BTN),
            Tile::Sep => (7.0, BTN),
            Tile::Text(s) => {
                let w = font
                    .map(|f| text_width(f, 12.0, s))
                    .unwrap_or(s.len() as f32 * 7.0);
                (w + 8.0, BTN)
            }
            Tile::Swatch(..) => (20.0, 20.0),
        }
    }
}

struct Toolbar {
    rect: FRect,
    items: Vec<(Tile, FRect)>,
}

impl Toolbar {
    fn hit(&self, p: Pt) -> Option<Act> {
        for (tile, r) in &self.items {
            if hit(*r, p) {
                return match tile {
                    Tile::Btn { act, .. } => Some(*act),
                    Tile::Swatch(act, _) => Some(*act),
                    Tile::Sep | Tile::Text(_) => None,
                };
            }
        }
        None
    }
}

/// Pack `tiles` into rows at the current cursor, wrapping at `maxw`.
#[allow(clippy::too_many_arguments)]
fn pack_rows(
    tiles: Vec<Tile>,
    font: Option<&FontArc>,
    maxw: f32,
    x: &mut f32,
    y: &mut f32,
    rowh: &mut f32,
    maxx: &mut f32,
    placed: &mut bool,
    items: &mut Vec<(Tile, FRect)>,
) {
    for tile in tiles {
        let (w, h) = tile.size(font);
        if *placed && *x + w > maxw {
            *x = 0.0;
            *y += *rowh + GAP;
            *rowh = 0.0;
        }
        items.push((
            tile,
            FRect {
                x: *x,
                y: *y,
                w,
                h,
            },
        ));
        *x += w + GAP;
        *rowh = (*rowh).max(h);
        *maxx = (*maxx).max(*x - GAP);
        *placed = true;
    }
}

/// (Re)build the toolbar hit-test/layout for the current edit state.
fn layout(edit: &mut Edit) {
    let Some(sr) = edit.sel else {
        edit.toolbar = None;
        return;
    };
    let (ww, wh) = (edit.shot.size.0 as f32, edit.shot.size.1 as f32);
    let font = edit.font.clone();
    let accent = edit.accent();
    let gray = C4::rgb(45, 45, 45);

    let mut tiles: Vec<Tile> = Vec::new();
    for (tool, label) in TOOLS {
        tiles.push(Tile::Btn {
            act: Act::Tool(tool),
            icon: tool_icon(tool),
            label,
            fill: if edit.tool == Some(tool) { accent } else { gray },
        });
    }
    tiles.push(Tile::Sep);
    tiles.push(Tile::Btn {
        act: Act::Undo,
        icon: "undo-variant",
        label: "Undo",
        fill: gray,
    });
    tiles.push(Tile::Btn {
        act: Act::Redo,
        icon: "redo-variant",
        label: "Redo",
        fill: gray,
    });
    tiles.push(Tile::Sep);
    tiles.push(Tile::Btn {
        act: Act::Size(-1),
        icon: "minus",
        label: "Smaller",
        fill: gray,
    });
    let bucket = edit.bucket();
    let (l, m, p, fo) = (
        edit.sizes.line,
        edit.sizes.marker,
        edit.sizes.pixelate,
        edit.sizes.font,
    );
    tiles.push(Tile::Text(format!(
        "{} {}",
        size_display(bucket, l, m, p, fo),
        bucket
    )));
    tiles.push(Tile::Btn {
        act: Act::Size(1),
        icon: "plus",
        label: "Bigger",
        fill: gray,
    });
    tiles.push(Tile::Sep);
    tiles.push(Tile::Btn {
        act: Act::Palette,
        icon: "",
        label: "",
        fill: edit.color,
    });
    tiles.push(Tile::Sep);
    if edit.tasks.is_empty() {
        tiles.push(Tile::Btn {
            act: Act::Copy,
            icon: "content-copy",
            label: "Copy",
            fill: gray,
        });
        tiles.push(Tile::Btn {
            act: Act::Save,
            icon: "content-save",
            label: "Save",
            fill: gray,
        });
        tiles.push(Tile::Btn {
            act: Act::Upload,
            icon: "cloud-upload",
            label: "Upload",
            fill: gray,
        });
    } else {
        tiles.push(Tile::Btn {
            act: Act::Accept,
            icon: "accept",
            label: "OK",
            fill: gray,
        });
    }
    tiles.push(Tile::Btn {
        act: Act::Exit,
        icon: "close",
        label: "Exit",
        fill: gray,
    });

    let maxw = (ww - 8.0 - 2.0 * PAD).max(40.0);
    let mut items: Vec<(Tile, FRect)> = Vec::new();
    let (mut x, mut y, mut rowh, mut maxx, mut placed) = (0.0, 0.0, 0.0, 0.0, false);
    pack_rows(
        tiles,
        font.as_ref(),
        maxw,
        &mut x,
        &mut y,
        &mut rowh,
        &mut maxx,
        &mut placed,
        &mut items,
    );
    if edit.palette_open {
        let colors: Vec<C4> = edit
            .cfg
            .user_colors
            .iter()
            .filter_map(|c| config::parse_color(c).map(|(r, g, b, a)| C4::new(r, g, b, a)))
            .collect();
        x = 0.0;
        y += rowh + GAP;
        rowh = 0.0;
        placed = false;
        let swatches: Vec<Tile> = colors
            .into_iter()
            .map(|c| Tile::Swatch(Act::Color(c), c))
            .collect();
        pack_rows(
            swatches,
            font.as_ref(),
            maxw,
            &mut x,
            &mut y,
            &mut rowh,
            &mut maxx,
            &mut placed,
            &mut items,
        );
    }

    let total_w = maxx + 2.0 * PAD;
    let total_h = y + rowh + 2.0 * PAD;
    // Center under the selection; flip above when it would overflow.
    let mut tx = sr.x + sr.w / 2.0 - total_w / 2.0;
    let mut ty = sr.y1() + 8.0;
    if ty + total_h > wh - 4.0 {
        ty = sr.y - 8.0 - total_h;
    }
    tx = tx.clamp(4.0, (ww - 4.0 - total_w).max(4.0));
    ty = ty.clamp(4.0, (wh - 4.0 - total_h).max(4.0));
    for (_, r) in &mut items {
        r.x += tx + PAD;
        r.y += ty + PAD;
    }
    edit.toolbar = Some(Toolbar {
        rect: FRect {
            x: tx,
            y: ty,
            w: total_w,
            h: total_h,
        },
        items,
    });
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

fn size_display(bucket: &str, line: f32, marker: f32, pixel: f32, font: f32) -> String {
    let v = match bucket {
        "marker" => marker,
        "pixelate" => pixel,
        "font" => font,
        _ => line,
    };
    format!("{}", v as i32)
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

fn text_box_rect(win_w: f32, td: &TextDraft, font_px: f32) -> FRect {
    let w = 320.0;
    let h = (font_px * 1.5).max(font_px + 6.0);
    let x = td.pos.x.min((win_w - 330.0).max(0.0));
    FRect {
        x,
        y: td.pos.y,
        w,
        h,
    }
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

/// Dim a rect of the image with translucent black (opaque target).
fn dim_rect(img: &mut PixBuf, x: f32, y: f32, w: f32, h: f32, alpha: u8) {
    if w <= 0.0 || h <= 0.0 || alpha == 0 {
        return;
    }
    let (bw, bh) = (img.width() as i32, img.height() as i32);
    let x0 = x.floor().max(0.0) as i32;
    let y0 = y.floor().max(0.0) as i32;
    let x1 = x1_clamp(x, w, bw);
    let y1 = y1_clamp(y, h, bh);
    let k = 1.0 - alpha as f32 / 255.0;
    let data = img.as_raw_mut();
    for y in y0.max(0)..y1.min(bh) {
        for x in x0.max(0)..x1.min(bw) {
            let i = ((y * bw + x) as usize) * 4;
            for ch in 0..3 {
                data[i + ch] = (data[i + ch] as f32 * k).round() as u8;
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
}
