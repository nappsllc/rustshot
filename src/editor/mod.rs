use crate::anim::Tween;
use crate::capture::{self, Shot};
use crate::config::{self, Config};
use crate::export::{self, Task};
use crate::fonts;
use crate::hotkey::{HotEvent, Hotkeys};
use crate::keymap::{Action, Keymap};
use crate::objects::{FRect, Obj, Pt};
use crate::pixbuf::PixBuf;
use crate::theme::{self, Theme};
use crate::raster::Order;
use crate::uifb::C4;
use compose::PixBufBackdrop;
use crate::wind::{self, Cursor, Driver, Ev, Hwnd, Mods};
use crate::fonts::UiFont;
use crate::text::AnnotFont;
use chrome::ToastKind;
use toolbar::{Act, Toolbar};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod toolbar;
mod chrome;
mod compose;

/// Chrome styling shared with the window control kit (`crate::ui`).
pub(crate) mod style {
    pub(crate) use super::chrome::{chord_caps, fill, ring, round, surface, Ui, ALL_LAYERS};
    pub(crate) use super::toolbar::BTN;
}
#[cfg(windows)]
mod gdi;
/// Linux/macOS: no GDI backend (`Edit::gdi` is always `None`).
#[cfg(not(windows))]
mod gdi {
    pub struct GdiScreen;
}

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
    /// Visible time before the fade-out starts.
    ttl: f32,
}

impl Toast {
    fn new(text: impl Into<String>, kind: ToastKind) -> Self {
        let ttl = if kind == ToastKind::Error { 4000.0 } else { 1600.0 };
        Toast { text: text.into(), kind, at: Instant::now(), ttl }
    }

    fn ttl_ms(&self) -> f32 {
        self.ttl
    }

    fn age_ms(&self, now: Instant) -> f32 {
        now.saturating_duration_since(self.at).as_secs_f32() * 1000.0
    }

    const FADE_IN: f32 = 120.0;
    const FADE_OUT: f32 = 100.0;

    fn expired(&self, now: Instant) -> bool {
        self.age_ms(now) > self.ttl_ms() + Self::FADE_OUT
    }

    /// Fade + rise in over 120 ms, fade out over 100 ms after the TTL.
    fn opacity(&self, now: Instant) -> f32 {
        let a = self.age_ms(now);
        let fade_in = crate::anim::ease_out(a / Self::FADE_IN);
        let fade_out = (1.0 - (a - self.ttl_ms()) / Self::FADE_OUT).clamp(0.0, 1.0);
        fade_in.min(fade_out)
    }

    /// Needs the fast tick (fading in, or close enough to fading out).
    fn animating(&self, now: Instant) -> bool {
        let a = self.age_ms(now);
        a < Self::FADE_IN || a > self.ttl_ms() - 160.0
    }
}

/// Overlay animation state (spec "Motion").
struct Motion {
    dim: Tween,
    bar: Tween,
    pop: Tween,
    hint: Tween,
    hover: Tween,
}

impl Motion {
    fn new(now: Instant) -> Self {
        let mut dim = Tween::new(0.0, now);
        dim.set(1.0, 120, now);
        let mut hint = Tween::new(0.0, now);
        hint.set(1.0, 120, now);
        Motion {
            dim,
            bar: Tween::new(0.0, now),
            pop: Tween::new(0.0, now),
            hint,
            hover: Tween::new(1.0, now),
        }
    }

    fn active(&self, now: Instant) -> bool {
        [&self.dim, &self.bar, &self.pop, &self.hint, &self.hover].iter().any(|t| t.active(now))
    }
}

pub fn run(
    cfg: Config,
    kind: RunKind,
    pending: Option<Pending>,
    exit_code: Arc<AtomicI32>,
    upload_slot: UploadSlot,
    instance: Option<crate::instance::Guard>,
) -> i32 {
    let hot = match kind {
        RunKind::Daemon => Some(Hotkeys::new(&cfg)),
        RunKind::OneShot => None,
    };
    // A later launch of the daemon asks this one to capture; the guard lives until we return.
    if let (Some(g), Some(h)) = (&instance, &hot) {
        g.listen(h.sender());
        crate::tray::spawn(h.sender());
    }
    if let Some(h) = &hot {
        // The update worker asks us to go idle (`RestartWhenIdle`), then to
        // quit once the new version starts (`Restart`).
        crate::update_ui::set_quit_sender(h.sender());
        // A save in the Settings window makes us reload the config.
        crate::settings_ui::set_daemon(h.sender());
    }
    let updates = match kind {
        RunKind::Daemon => crate::update::spawn_checker(cfg.check_updates),
        RunKind::OneShot => None,
    };
    let font = AnnotFont::load();
    let ui_font = Some(fonts::ui_font());
    let mut app = App {
        cfg,
        kind,
        hot,
        pending,
        st: State::Hidden,
        exit_code,
        upload_slot,
        updates,
        update_pending: None,
        restart_pending: false,
        updating: false,
        idle_reply: None,
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
    drop(instance);
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
    /// Results from the background update checker (daemon only).
    updates: Option<Receiver<crate::update::Release>>,
    /// A newer release found; shown in the update dialog once no capture is up.
    update_pending: Option<crate::update::Release>,
    /// An installed update asked us to quit (`HotEvent::Restart`); done
    /// once no capture is open.
    restart_pending: bool,
    /// An update is installing (`HotEvent::RestartWhenIdle`): no new capture.
    updating: bool,
    /// Told (once) as soon as we are idle; the worker installs only then.
    idle_reply: Option<std::sync::mpsc::Sender<()>>,
    font: Option<AnnotFont>,
    ui_font: Option<&'static UiFont>,
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
        while let Some(ev) = self.hot.as_ref().and_then(|h| h.poll()) {
            if !self.on_hot(ev) {
                return;
            }
        }
        self.poll_upload();
        if let Some(rx) = &self.updates {
            while let Ok(r) = rx.try_recv() {
                self.update_pending = Some(r);
            }
        }
        // Never over a capture in progress: the dialog would take its focus.
        if matches!(self.st, State::Hidden)
            && self.pending.is_none()
            && let Some(r) = self.update_pending.take()
        {
            crate::update_ui::offer(crate::update_ui::DialogState::Available(r));
        }
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
                    let cleared = had && edit.notice.is_none();
                    self.st = State::Edit(Box::new(edit));
                    if cleared {
                        self.repaint();
                    }
                    break;
                }
            }
        }
        // An update waits for us to go idle before it installs.
        self.idle_gate();
        // An update is restarting us: quit as soon as no capture is open
        // (never under the user's open capture or export).
        if self.restart_due() {
            self.request_exit(0);
        }
    }

    /// Handle one daemon event; false once we are quitting.
    fn on_hot(&mut self, ev: HotEvent) -> bool {
        match ev {
            HotEvent::Quit => {
                self.request_exit(0);
                return false;
            }
            HotEvent::Restart => self.restart_pending = true,
            HotEvent::RestartWhenIdle(reply) => {
                self.updating = true;
                self.idle_reply = Some(reply);
            }
            HotEvent::RestartAborted => {
                self.updating = false;
                self.idle_reply = None;
            }
            // A file that stopped parsing is not taken as "all defaults".
            HotEvent::ReloadConfig => match crate::config::read_at(&crate::config::config_path()) {
                Ok(cfg) => self.reload_config(cfg.unwrap_or_default()),
                Err(e) => eprintln!("rustshot: config.toml not reloaded: {e}"),
            },
            // No new capture while an update installs or restarts us.
            HotEvent::Capture if self.updating || self.restart_pending => {
                eprintln!("rustshot: an update is installing; capture ignored");
            }
            HotEvent::Capture if matches!(self.st, State::Hidden) => self.pending = Some(Pending::editor()),
            HotEvent::Capture => {}
        }
        true
    }

    /// Take `cfg` (Settings saved it): the next capture uses it, the global
    /// hotkeys are re-registered and the update checker follows
    /// `check_updates`. An open capture keeps the config it started with.
    fn reload_config(&mut self, cfg: Config) {
        if let Some(h) = self.hot.as_mut() {
            let failed = h.reload(&cfg);
            if !failed.is_empty() {
                let text = failed.iter().map(|f| f.message()).collect::<Vec<_>>().join("
");
                eprintln!("rustshot: {text}");
                // In the Settings window when it is open, else a tray balloon.
                if !crate::settings_ui::notify(&text) {
                    crate::tray::notify(&text);
                }
            }
        }
        if cfg.check_updates != self.cfg.check_updates && self.kind == RunKind::Daemon {
            // A new checker generation: the old one stops before its next
            // check (and never takes the day's stamp).
            self.updates = crate::update::spawn_checker(cfg.check_updates);
        }
        self.cfg = cfg;
    }

    /// No capture, editor or export is open or about to open, and no
    /// upload is in flight.
    fn idle(&self) -> bool {
        matches!(self.st, State::Hidden)
            && self.pending.is_none()
            && self.upload_slot.lock().unwrap_or_else(|e| e.into_inner()).is_none()
    }

    /// Answer a waiting update worker once we are idle. Captures stay
    /// refused afterwards; if the worker is gone it will not install, so
    /// they are allowed again.
    fn idle_gate(&mut self) {
        if self.idle_reply.is_some() && self.idle() {
            let reply = self.idle_reply.take().expect("checked");
            if reply.send(()).is_err() {
                self.updating = false;
            }
        }
    }

    /// A pending restart may happen now: we are idle. Clears the request
    /// when it says yes.
    fn restart_due(&mut self) -> bool {
        let due = self.restart_pending && self.idle();
        if due {
            self.restart_pending = false;
        }
        due
    }

    /// Timer tick: poll, then report whether the screen needs a new frame
    /// (an animation runs, the caret blinked, a toast came or went, the
    /// state switched). Idle ticks cost no frame.
    fn tick(&mut self) -> bool {
        let sig = |a: &App| std::mem::discriminant(&a.st);
        let before = sig(self);
        self.pump();
        let changed = sig(self) != before;
        let State::Edit(edit) = &self.st else { return changed };
        let repaint = changed || stale(edit, self.notice.as_ref(), Instant::now());
        if !repaint {
            // No frame runs to re-evaluate the cadence: drop to idle.
            wind::set_fast_timer(self.hwnd, false);
        }
        repaint
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

        let th = theme::resolve(&cfg);
        let (shot, gdi) = match grab(&cfg, &th, screen) {
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
        // The capture is held once, as `base`; `shot` keeps only geometry.
        let mut shot = shot;
        let base = std::mem::take(&mut shot.image);
        let sel = initial_sel.filter(|r| !r.is_trivial());
        let accept_now = accept_on_select && sel.is_some();
        let mut edit = Edit {
            mo: Motion::new(Instant::now()),
            shot,
            base,
            composed: None,
            frame: PixBuf::default(),
            scene: None,
            painted: None,
            gdi,
            draft_buf: Default::default(),
            caret_drawn: None,
            toast_drawn: None,
            area_drawn: None,
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
            keys: Keymap::from_config(&cfg.shortcuts).0,
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
            ui_font: self.ui_font,
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
        wind::set_fast_timer(self.hwnd, false);
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
        #[cfg(windows)]
        crate::memlog("overlay");
        let img = edit.export(sel);
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

        // Arrow keys nudge / resize the selection (built in, not remappable).
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

        let Some(action) = edit.keys.resolve(vk, mods) else { return };
        match action {
            Action::Cancel => cancel_key(edit),
            Action::Accept => accept_key(edit),
            Action::SelectAll => {
                edit.sel = Some(FRect { x: 0.0, y: 0.0, w: edit.shot.size.0 as f32, h: edit.shot.size.1 as f32 });
                edit.interact = Interact::None;
            }
            // The palette popover hangs off the toolbar, which needs a selection.
            Action::TogglePalette if edit.sel.is_none() => {}
            a => {
                if let Some(act) = act_of(a) {
                    self.apply_act(edit, act);
                }
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
                edit.tasks = vec![Task::Save { path: None, ask: edit.cfg.save_dialog }];
                edit.done = true;
            }
            Act::SaveAs => {
                // Always ask, seeded with the auto-save path.
                edit.tasks = vec![Task::Save { path: None, ask: true }];
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

impl App {
    /// Ask for a repaint of what changed (GDI: the dirty rects; software:
    /// the whole window).
    fn repaint(&mut self) {
        let h = self.hwnd;
        wind::request(h, self);
    }
}

impl Driver for App {
    fn on_create(&mut self, hwnd: Hwnd) {
        self.hwnd = hwnd;
        self.pump();
    }

    fn on_event(&mut self, ev: Ev) -> bool {
        match ev {
            Ev::Move { x, y } => {
                self.mouse = (x, y);
                let dragging =
                    matches!(&self.st, State::Edit(e) if !matches!(e.interact, Interact::None));
                if dragging {
                    self.pointer_move(x, y);
                    self.repaint();
                } else if let State::Edit(e) = &mut self.st {
                    let p = Pt::new(x as f32, y as f32);
                    // Chrome follows the monitor under the pointer when
                    // nothing is selected (hint, toast placement).
                    let area_moved =
                        e.area_drawn.is_some_and(|a| a != pick_area(&e.shot.monitors, e.sel, p));
                    if e.track_hover(p) || area_moved {
                        self.repaint();
                    }
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
            Ev::Timer => return self.tick(),
            // Decorated-window events (`run_window`); the overlay never gets them.
            Ev::Close | Ev::Resize(..) | Ev::Focus(_) => return false,
        }
        self.pump();
        // Only `Ev::Timer` consults this value (returned from `tick`);
        // moves call `invalidate` themselves, only when something changed.
        true
    }

    fn frame(&mut self) -> Option<&mut PixBuf> {
        let State::Edit(edit) = &mut self.st else { return None };
        let edit: &mut Edit = edit;
        if edit.gdi.is_some() {
            return None; // painted by `paint`
        }
        let now = Instant::now();
        edit.prepare(self.notice.as_ref(), self.mouse, now);
        // Borrowed out of `edit` while composing, put back below;
        // allocated once per capture.
        let mut img = std::mem::take(&mut edit.frame);
        if img.dimensions() != edit.base.dimensions() {
            img = PixBuf::new(edit.base.width(), edit.base.height());
        }
        // The whole image as one rect: same code path as partial repaints.
        let bd = PixBufBackdrop { img: edit.composed(), dim: edit.th.dim };
        edit.compose_rect(compose::PxRect::image(edit.shot.size), &bd, img.as_raw_mut(), Order::Rgba);
        wind::set_fast_timer(self.hwnd, animating(edit, self.notice.as_ref(), now));
        edit.frame = img;
        Some(&mut edit.frame)
    }

    fn damage(&mut self) -> Option<Vec<[i32; 4]>> {
        let State::Edit(edit) = &mut self.st else { return None };
        let edit: &mut Edit = edit;
        edit.gdi.as_ref()?;
        let now = Instant::now();
        edit.prepare(self.notice.as_ref(), self.mouse, now);
        wind::set_fast_timer(self.hwnd, animating(edit, self.notice.as_ref(), now));
        let rects = compose::merge_rects(edit.dirty_rects(edit.painted.as_ref()));
        // The target state: every pixel that differs from it is invalid now.
        edit.painted = edit.scene.clone();
        if rects.len() > MAX_DAMAGE_RECTS {
            return None;
        }
        Some(rects.into_iter().map(|r| [r.x0, r.y0, r.x1, r.y1]).collect())
    }

    #[cfg(windows)]
    fn paint(&mut self, hdc: windows::Win32::Graphics::Gdi::HDC, rects: &[[i32; 4]]) -> bool {
        let State::Edit(edit) = &mut self.st else { return false };
        let edit: &mut Edit = edit;
        if edit.gdi.is_none() {
            return false;
        }
        if edit.scene.is_none() {
            // A system paint before any damage (first show).
            let now = Instant::now();
            edit.prepare(self.notice.as_ref(), self.mouse, now);
            wind::set_fast_timer(self.hwnd, animating(edit, self.notice.as_ref(), now));
            edit.painted = edit.scene.clone();
        }
        let rects: Vec<compose::PxRect> =
            rects.iter().map(|r| compose::PxRect::new(r[0], r[1], r[2], r[3])).collect();
        edit.paint_gdi(hdc, &rects);
        true
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

/// Capture for the editor with the renderer from `cfg` (resolved once per
/// capture): Windows `gdi` keeps the pixels in GDI bitmaps and leaves
/// `shot.image` empty; `software` (and Linux/macOS) reads them into memory.
fn grab(cfg: &Config, th: &Theme, screen: Option<u32>) -> anyhow::Result<(Shot, Option<gdi::GdiScreen>)> {
    #[cfg(windows)]
    if cfg.use_gdi() {
        let shot = capture::plan_edit(screen, cfg.capture_active_monitor)?;
        let dim = th.dim.with_alpha(th.dim_alpha(cfg.contrast_opacity));
        let scr = gdi::GdiScreen::capture(shot.origin, shot.size, dim)?;
        return Ok((shot, Some(scr)));
    }
    let _ = th;
    Ok((capture::grab_edit(screen, cfg.capture_active_monitor)?, None))
}

/// Esc: drop the current tool / draft / palette, or cancel the capture.
fn cancel_key(edit: &mut Edit) {
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
}

/// Enter: select everything if nothing is selected, then run the given
/// tasks (or save).
fn accept_key(edit: &mut Edit) {
    if edit.sel.is_none() {
        edit.sel = Some(FRect { x: 0.0, y: 0.0, w: edit.shot.size.0 as f32, h: edit.shot.size.1 as f32 });
        if edit.accept_on_select {
            edit.done = true;
            edit.cancelled = false;
            return;
        }
    }
    if edit.tasks.is_empty() {
        edit.tasks = vec![Task::Save { path: None, ask: edit.cfg.save_dialog }];
    }
    edit.done = true;
    edit.cancelled = false;
}

/// The toolbar act a keymap action triggers (`None`: handled in
/// `handle_key` itself).
fn act_of(a: Action) -> Option<Act> {
    Some(match a {
        Action::ToolPencil => Act::Tool(Tool::Path),
        Action::ToolLine => Act::Tool(Tool::Line),
        Action::ToolArrow => Act::Tool(Tool::Arrow),
        Action::ToolRectangle => Act::Tool(Tool::Rect),
        Action::ToolCircle => Act::Tool(Tool::Ellipse),
        Action::ToolMarker => Act::Tool(Tool::Marker),
        Action::ToolText => Act::Tool(Tool::Text),
        Action::ToolPixelate => Act::Tool(Tool::Pixelate),
        Action::ToolInvert => Act::Tool(Tool::Invert),
        Action::Copy => Act::Copy,
        Action::Save => Act::Save,
        Action::SaveAs => Act::SaveAs,
        Action::Upload => Act::Upload,
        Action::Undo => Act::Undo,
        Action::Redo => Act::Redo,
        Action::TogglePalette => Act::Palette,
        Action::SelectAll | Action::Accept | Action::Cancel => return None,
    })
}

/// The keymap action whose chord a toolbar button's tooltip shows.
fn action_of(a: Act) -> Option<Action> {
    match a {
        Act::Exit => Some(Action::Cancel),
        Act::Accept => Some(Action::Accept),
        _ => Action::ALL.into_iter().find(|x| act_of(*x) == Some(a)),
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
    mo: Motion,
    shot: Shot,
    /// The capture (moved out of `shot.image`, which stays empty).
    base: PixBuf,
    /// `base` with the committed objects baked in; `None` while there are
    /// none (identical to `base`, so no second full-size buffer). Read it
    /// through `composed()`.
    composed: Option<PixBuf>,
    /// Display buffer `frame()` composes into; reused across frames,
    /// allocated on the first frame of a capture.
    frame: PixBuf,
    /// The last prepared frame state (`prepare`), drawn by `compose_rect`.
    scene: Option<compose::Scene>,
    /// GDI renderer: the scene the window shows once the pending
    /// invalidations are painted (what `dirty_rects` diffs against).
    painted: Option<compose::Scene>,
    /// GDI renderer (Windows, `renderer = "gdi"`): the capture as GDI
    /// bitmaps; `base` then stays empty and `composed` unused.
    gdi: Option<gdi::GdiScreen>,
    /// Scratch for a pixelate draft rendered over whole cells
    /// (`compose_rect`); about one band, released when the draft ends.
    draft_buf: std::cell::RefCell<Vec<u8>>,
    /// Caret blink phase in the last frame (None: no text draft).
    caret_drawn: Option<bool>,
    /// `at` of the toast drawn in the last frame (None: no toast).
    toast_drawn: Option<Instant>,
    /// Monitor area chrome was placed in by the last frame.
    area_drawn: Option<FRect>,
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
    /// Editor shortcuts (`cfg.shortcuts` over the defaults).
    keys: Keymap,
    toolbar: Option<Toolbar>,
    palette_open: bool,
    done: bool,
    cancelled: bool,
    dirty: bool,
    notice: Option<Toast>,
    last_wheel: Instant,
    font: Option<AnnotFont>,
    th: Theme,
    ui_font: Option<&'static UiFont>,
    /// Toolbar item under the pointer and since when (tooltip delay).
    hover: Option<usize>,
    hover_at: Instant,
    pressed: Option<usize>,
    /// Selection handle under the pointer (ring grows to 3 px).
    hot_handle: Option<usize>,
}

/// More merged dirty rects than this invalidate the whole window.
const MAX_DAMAGE_RECTS: usize = 16;

const HANDLE_HIT: f32 = 10.0; // handle hit radius, logical px (20 px target)
const CLICK_PX: f32 = 2.5; // movement below this counts as a click

impl Edit {
    /// The selection crop (`sel` in image coords) to export, objects
    /// baked in.
    fn export(&self, sel: FRect) -> PixBuf {
        #[cfg(windows)]
        if self.gdi.is_some() {
            return self.export_gdi(sel);
        }
        crop_to_image(self.composed(), sel)
    }

    /// The capture with committed objects baked in.
    fn composed(&self) -> &PixBuf {
        self.composed.as_ref().unwrap_or(&self.base)
    }

    fn rebuild(&mut self) {
        #[cfg(windows)]
        if let Some(scr) = self.gdi.as_mut() {
            // The annotation layer: objects baked over the plain capture,
            // only where they are.
            scr.set_objects(&self.objects, self.font.as_ref());
            self.dirty = false;
            return;
        }
        if self.objects.is_empty() {
            // Nothing to bake (e.g. undid the last object): drop the copy.
            self.composed = None;
        } else {
            // Re-bake in place; allocate only when it was `None`.
            let base = &self.base;
            let composed = self.composed.get_or_insert_with(|| base.clone());
            composed.as_raw_mut().copy_from_slice(base.as_raw());
            for o in &self.objects {
                o.render(composed, self.font.as_ref());
            }
        }
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
            let now = Instant::now();
            self.mo.hover = Tween::new(0.0, now);
            self.mo.hover.set(1.0, 60, now);
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
fn text_box_rect(win_w: f32, td: &TextDraft, font_px: f32, font: Option<&AnnotFont>, s: f32) -> FRect {
    let pad = 4.0 * s;
    let tw = font.map(|f| f.line_width(&td.text, font_px)).unwrap_or(0.0);
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

/// Something on screen moves by itself: a tween, the pending tooltip,
/// a toast fading in or out.
fn animating(edit: &Edit, app_notice: Option<&Toast>, now: Instant) -> bool {
    let tip_pending = edit.hover.is_some() && now.saturating_duration_since(edit.hover_at) < Duration::from_millis(500);
    let toast_moving = edit.notice.as_ref().or(app_notice).is_some_and(|t| t.animating(now));
    edit.mo.active(now) || tip_pending || toast_moving
}

/// The last frame no longer matches: something animates, the caret
/// blink phase flipped, or the toast that would be drawn now differs
/// from the one drawn (set, cleared or replaced between frames).
fn stale(edit: &Edit, app_notice: Option<&Toast>, now: Instant) -> bool {
    let toast_now = edit.notice.as_ref().or(app_notice).map(|t| t.at);
    animating(edit, app_notice, now)
        || edit.text.as_ref().map(caret_on) != edit.caret_drawn
        || toast_now != edit.toast_drawn
}

/// Caret blink phase: shown 530 ms, hidden 530 ms.
fn caret_on(td: &TextDraft) -> bool {
    (td.at.elapsed().as_millis() / 530).is_multiple_of(2)
}

/// Blends toward a dim colour by alpha `a`, two RGBA pixels per `u64`
/// (SWAR, 16-bit lanes): per channel `(v * (255 - a) + c * a + 127) / 255`.
/// For integer inputs that is exactly the rounding of the former per-pixel
/// float blend (`n / 255` is never within float error of a half), so the
/// output is bit-identical. Alpha bytes pass through. Built per alpha (the
/// fade-in changes it every frame), which costs nothing.
#[derive(Clone, Copy)]
struct Dimmer {
    k: u64,
    add_rb: u64,
    add_ga: u64,
}

const LANES: u64 = 0x00FF_00FF_00FF_00FF;
const ALPHA: u64 = 0xFF00_0000_FF00_0000;

impl Dimmer {
    fn new(c: C4) -> Self {
        let a = c.a as u64;
        let lane = |lo: u8, hi: u8| {
            let pair = (lo as u64 * a + 127) | ((hi as u64 * a + 127) << 16);
            pair | (pair << 32)
        };
        // Lanes (little-endian bytes R G B A): R|B and G|A of two pixels.
        Dimmer { k: 255 - a, add_rb: lane(c.r, c.b), add_ga: lane(c.g, 0) }
    }

    /// Exact `x / 255` per 16-bit lane for lane values below 65535
    /// (ours stay below 65153).
    #[inline(always)]
    fn div255(x: u64) -> u64 {
        ((x + ((x >> 8) & LANES) + 0x0001_0001_0001_0001) >> 8) & LANES
    }

    #[inline(always)]
    fn pair(&self, v: u64) -> u64 {
        let rb = Self::div255((v & LANES) * self.k + self.add_rb);
        let ga = Self::div255(((v >> 8) & LANES) * self.k + self.add_ga);
        ((rb | (ga << 8)) & !ALPHA) | (v & ALPHA)
    }

    /// `buf` blended in place, `times` over.
    #[cfg_attr(not(windows), allow(dead_code))]
    fn apply(&self, buf: &mut [u8], times: i32) {
        let (b8, bt) = buf.as_chunks_mut::<8>();
        for o in b8 {
            let mut v = u64::from_le_bytes(*o);
            for _ in 0..times {
                v = self.pair(v);
            }
            *o = v.to_le_bytes();
        }
        if let Some(o) = bt.first_chunk_mut::<4>() {
            let mut v = u32::from_le_bytes(*o) as u64;
            for _ in 0..times {
                v = self.pair(v);
            }
            *o = (v as u32).to_le_bytes();
        }
    }

    /// `dst = src` blended `times` over (overlapping dim rects blend once
    /// per rect; only fractional selections overlap).
    fn run(&self, src: &[u8], dst: &mut [u8], times: i32) {
        let (d8, dt) = dst.as_chunks_mut::<8>();
        let (s8, st) = src.as_chunks::<8>();
        if times == 1 {
            for (o, i) in d8.iter_mut().zip(s8) {
                *o = self.pair(u64::from_le_bytes(*i)).to_le_bytes();
            }
        } else {
            for (o, i) in d8.iter_mut().zip(s8) {
                let mut v = u64::from_le_bytes(*i);
                for _ in 0..times {
                    v = self.pair(v);
                }
                *o = v.to_le_bytes();
            }
        }
        if let (Some(o), Some(i)) = (dt.first_chunk_mut::<4>(), st.first_chunk::<4>()) {
            let mut v = u32::from_le_bytes(*i) as u64;
            for _ in 0..times {
                v = self.pair(v);
            }
            *o = (v as u32).to_le_bytes();
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

/// The monitor chrome is placed on: largest overlap with `sel` (ties go to
/// the first), else the one under `pointer`, else the first.
fn pick_area(monitors: &[capture::IRect], sel: Option<FRect>, pointer: Pt) -> FRect {
    let rect = |m: &capture::IRect| FRect { x: m.0 as f32, y: m.1 as f32, w: m.2 as f32, h: m.3 as f32 };
    let first = monitors.first().map(rect).unwrap_or(FRect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 });
    if let Some(sel) = sel {
        let overlap = |r: FRect| {
            let w = (sel.x1().min(r.x1()) - sel.x.max(r.x)).max(0.0);
            let h = (sel.y1().min(r.y1()) - sel.y.max(r.y)).max(0.0);
            w * h
        };
        let mut best: Option<(f32, FRect)> = None;
        for r in monitors.iter().map(rect) {
            let o = overlap(r);
            if o > 0.0 && best.is_none_or(|(bo, _)| o > bo) {
                best = Some((o, r));
            }
        }
        if let Some((_, r)) = best {
            return r;
        }
    }
    monitors.iter().map(rect).find(|r| toolbar::hit(*r, pointer)).unwrap_or(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- developer preview (renders overlay frames to PNG; no display) ----

    /// 2400x900 desktop: a shorter, offset left monitor and a taller right one.
    fn two_monitor_shot() -> Shot {
        let mut shot = synthetic_shot();
        let (w, h) = (2400u32, 900u32);
        let mut img = PixBuf::from_pixel(w, h, [30, 34, 44, 255]);
        let src = shot.image.clone();
        for y in 0..h {
            for x in 0..w {
                let i = (y as usize * w as usize + x as usize) * 4;
                let (sx, sy) = (x % src.width(), y.min(src.height() - 1));
                let j = (sy as usize * src.width() as usize + sx as usize) * 4;
                img.as_raw_mut()[i..i + 4].copy_from_slice(&src.as_raw()[j..j + 4]);
            }
        }
        shot.size = (w, h);
        shot.image = img;
        shot.monitors = vec![(0, 200, 1200, 700), (1200, 0, 1200, 900)];
        shot
    }

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
        Shot {
            origin: (0, 0),
            size: (w, h),
            scale: 1.0,
            image: img,
            monitors: vec![(0, 0, w, h)],
        }
    }

    fn preview_app(th: Theme, sel: Option<FRect>) -> App {
        preview_app_with(th, sel, synthetic_shot())
    }

    /// An update's restart never quits under an open capture: it waits
    /// until the editor is hidden again.
    #[test]
    fn restart_waits_for_the_open_capture() {
        let mut app = preview_app(theme::DARK, None);
        assert!(!app.restart_due(), "nothing asked");
        app.restart_pending = true;
        assert!(!app.restart_due(), "capture open");
        assert!(app.restart_pending);
        app.st = State::Hidden;
        app.pending = Some(Pending::editor());
        assert!(!app.restart_due(), "capture about to open");
        app.pending = None;
        assert!(app.restart_due(), "idle: quit now");
        assert!(!app.restart_pending && !app.restart_due(), "once");
    }

    /// An upload still in flight keeps the daemon from being idle.
    fn upload_in_flight(app: &App) -> std::sync::mpsc::Sender<Result<String, String>> {
        let (tx, rx) = std::sync::mpsc::channel();
        *app.upload_slot.lock().unwrap() = Some(rx);
        tx
    }

    /// A Settings save: the next capture uses the new config (the open one
    /// keeps its own) and the update checker follows `check_updates`.
    #[test]
    fn reload_takes_the_new_config() {
        let mut app = preview_app(theme::DARK, None);
        app.kind = RunKind::Daemon;
        let (_tx, rx) = std::sync::mpsc::channel();
        app.updates = Some(rx);
        let cfg = Config { save_format: "jpg".into(), check_updates: false, ..Config::default() };
        app.reload_config(cfg.clone());
        assert_eq!(app.cfg, cfg);
        assert!(app.updates.is_none(), "checker stopped");
        let State::Edit(edit) = &app.st else { panic!("capture still open") };
        assert_eq!(edit.cfg.save_format, "png");
    }

    /// The update worker installs only once no capture, editor or upload
    /// runs; from its request on no new capture starts.
    #[test]
    fn idle_gate_waits_for_capture_and_upload() {
        let mut app = preview_app(theme::DARK, None); // a capture is open
        let (tx, rx) = std::sync::mpsc::channel();
        assert!(app.on_hot(HotEvent::RestartWhenIdle(tx)));
        app.idle_gate();
        assert!(rx.try_recv().is_err(), "capture open: no reply");
        app.st = State::Hidden;
        let up = upload_in_flight(&app);
        app.idle_gate();
        assert!(rx.try_recv().is_err(), "upload in flight: no reply");
        up.send(Err("offline".into())).unwrap();
        app.poll_upload();
        app.idle_gate();
        assert_eq!(rx.try_recv(), Ok(()), "idle: reply");
        app.idle_gate();
        assert!(rx.try_recv().is_err(), "reply once");
        // From now on until the exit a capture is refused.
        assert!(app.on_hot(HotEvent::Capture));
        assert!(app.pending.is_none(), "capture refused after the gate");
        // The install failed: captures work again.
        assert!(app.on_hot(HotEvent::RestartAborted));
        assert!(app.on_hot(HotEvent::Capture));
        assert!(app.pending.is_some());
    }

    /// A capture asked while the worker waits is refused too (it would
    /// keep the daemon busy); a reply nobody reads lifts the gate.
    #[test]
    fn idle_gate_refuses_captures_while_waiting() {
        let mut app = preview_app(theme::DARK, None);
        app.st = State::Hidden;
        let (tx, rx) = std::sync::mpsc::channel();
        app.pending = Some(Pending::editor()); // a capture about to open
        app.on_hot(HotEvent::RestartWhenIdle(tx));
        app.idle_gate();
        assert!(rx.try_recv().is_err(), "capture about to open: no reply");
        app.pending = None;
        app.on_hot(HotEvent::Capture);
        assert!(app.pending.is_none(), "refused while waiting");
        drop(rx); // the worker gave up
        app.idle_gate();
        assert!(app.idle_reply.is_none() && !app.updating);
        app.on_hot(HotEvent::Capture);
        assert!(app.pending.is_some());
    }

    /// `Restart` sets the flag; a capture is ignored while it is pending.
    #[test]
    fn restart_event_sets_the_flag_and_refuses_captures() {
        let mut app = preview_app(theme::DARK, None);
        app.st = State::Hidden;
        assert!(app.on_hot(HotEvent::Restart));
        assert!(app.restart_pending);
        assert!(app.on_hot(HotEvent::Capture));
        assert!(app.pending.is_none());
        let _up = upload_in_flight(&app);
        assert!(!app.restart_due(), "upload in flight");
    }

    fn preview_app_with(th: Theme, sel: Option<FRect>, shot: Shot) -> App {
        let theme_name = if th == theme::DARK { "dark" } else { "light" };
        let cfg = Config { theme: theme_name.into(), ..Config::default() };
        let font = AnnotFont::load();
        let ui_font = Some(fonts::ui_font());
        let mut shot = shot;
        let base = std::mem::take(&mut shot.image);
        let edit = Edit {
            mo: Motion::new(Instant::now()),
            shot,
            composed: None,
            base,
            frame: PixBuf::default(),
            scene: None,
            painted: None,
            gdi: None,
            draft_buf: Default::default(),
            caret_drawn: None,
            toast_drawn: None,
            area_drawn: None,
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
            keys: Keymap::defaults(),
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
            ui_font,
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
            updates: None,
            update_pending: None,
            restart_pending: false,
            updating: false,
            idle_reply: None,
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

    /// Press `vk` without finishing the capture (no export runs).
    fn press(app: &mut App, vk: u32, mods: Mods) -> Edit {
        let State::Edit(e) = std::mem::replace(&mut app.st, State::Hidden) else { unreachable!() };
        let mut e = *e;
        app.handle_key(&mut e, vk, false, mods);
        e
    }

    const CTRL: Mods = Mods { ctrl: true, shift: false, alt: false };
    const CTRL_SHIFT: Mods = Mods { ctrl: true, shift: true, alt: false };

    #[test]
    fn keymap_drives_handle_key() {
        let sel = FRect { x: 10.0, y: 10.0, w: 50.0, h: 40.0 };
        let mut app = preview_app(theme::DARK, Some(sel));
        let e = press(&mut app, 'S' as u32, CTRL_SHIFT);
        assert!(e.done && matches!(e.tasks[..], [Task::Save { path: None, ask: true }]), "Save As asks");
        let mut app = preview_app(theme::DARK, Some(sel));
        let e = press(&mut app, 'S' as u32, CTRL);
        assert!(e.done && matches!(e.tasks[..], [Task::Save { path: None, ask: false }]));

        let mut app = preview_app(theme::DARK, Some(sel));
        let e = press(&mut app, 'A' as u32, CTRL);
        assert_eq!(e.sel, Some(FRect { x: 0.0, y: 0.0, w: e.shot.size.0 as f32, h: e.shot.size.1 as f32 }));
        assert!(!e.done);

        let mut app = preview_app(theme::DARK, Some(sel));
        assert_eq!(press(&mut app, 'D' as u32, Mods::default()).tool, Some(Tool::Line));
        let mut app = preview_app(theme::DARK, Some(sel));
        assert_eq!(press(&mut app, 'D' as u32, Mods { shift: true, ..Mods::default() }).tool, None);
        let mut app = preview_app(theme::DARK, Some(sel));
        assert!(press(&mut app, wind::key::SPACE, Mods::default()).palette_open);

        // Remapped: Pencil on K, Cancel unbound from Esc.
        let mut app = preview_app(theme::DARK, Some(sel));
        let km = [("tool_pencil", "K"), ("cancel", "")].map(|(a, b)| (a.to_string(), b.to_string()));
        edit_of(&mut app).keys = Keymap::from_config(&km.into_iter().collect()).0;
        let mut e = press(&mut app, 'K' as u32, Mods::default());
        assert_eq!(e.tool, Some(Tool::Path));
        app.handle_key(&mut e, 'P' as u32, false, Mods::default());
        assert_eq!(e.tool, Some(Tool::Path), "P no longer bound");
        app.handle_key(&mut e, wind::key::ESCAPE, false, Mods::default());
        assert!(e.tool.is_some() && !e.done, "Esc unbound");
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

    fn one_rect(sel: FRect) -> Obj {
        Obj::Rect {
            r: FRect { x: sel.x + 40.0, y: sel.y + 40.0, w: sel.w * 0.35, h: sel.h * 0.3 },
            color: C4::rgb(240, 68, 56),
            width: 3.0,
        }
    }

    /// A fresh edit has no `composed` copy; frame and export match what a
    /// materialised copy of `base` would give.
    #[test]
    fn fresh_edit_has_no_composed_and_output_matches_base() {
        let sel = FRect { x: 100.0, y: 80.0, w: 300.0, h: 200.0 };
        let mut app = preview_app(theme::DARK, Some(sel));
        let e = edit_of(&mut app);
        assert!(e.composed.is_none());
        assert!(std::ptr::eq(e.composed(), &e.base));
        let want_crop = crop_to_image(&e.base.clone(), sel);
        assert!(crop_to_image(e.composed(), sel) == want_crop);
        let got = app.frame().expect("frame").clone();
        // Same edit with an explicit identical copy of base.
        let mut app2 = preview_app(theme::DARK, Some(sel));
        let e2 = edit_of(&mut app2);
        e2.composed = Some(e2.base.clone());
        let want = app2.frame().expect("frame").clone();
        // Animation state starts at the same instant-ish; compare the
        // captured area inside the selection, which no tween touches.
        let (x0, y0) = (sel.x as u32 + 8, sel.y as u32 + 8);
        let w = sel.w as u32 - 16;
        let h = sel.h as u32 - 16;
        assert!(got.crop(x0, y0, w, h) == want.crop(x0, y0, w, h));
        assert!(edit_of(&mut app).composed.is_none(), "frame() must not allocate it");
    }

    /// Committing an object allocates `composed`; undoing back to zero
    /// objects drops it; redo brings it back.
    #[test]
    fn composed_allocated_on_commit_and_dropped_on_undo() {
        let sel = FRect { x: 100.0, y: 80.0, w: 300.0, h: 200.0 };
        let mut app = preview_app(theme::DARK, Some(sel));
        let e = edit_of(&mut app);
        e.commit_object(one_rect(sel));
        e.rebuild();
        assert!(e.composed.is_some());
        assert!(e.composed() != &e.base, "object is baked in");
        e.undo();
        e.rebuild();
        assert!(e.composed.is_none());
        assert!(e.composed() == &e.base);
        e.redo();
        e.rebuild();
        assert!(e.composed.is_some() && e.composed() != &e.base);
    }

    /// The Windows/Linux presenters convert the frame in place: no
    /// staging thread-local remains (structural check on the source).
    #[test]
    fn presenters_have_no_staging_buffer() {
        for src in [include_str!("../wind_win.rs"), include_str!("../wind_linux.rs")] {
            let prod = src.split("#[cfg(test)]").next().unwrap();
            assert!(!prod.contains("static BGRA") && !prod.contains("static BGRX"), "staging buffer is back");
        }
    }

    #[test]
    #[ignore = "writes preview PNGs; set RUSTSHOT_PREVIEW_DIR"]
    fn render_preview_pngs() {
        let Some(dir) = std::env::var_os("RUSTSHOT_PREVIEW_DIR") else { return };
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        let save = |app: &mut App, name: &str| {
            if let State::Edit(e) = &mut app.st {
                for t in [&mut e.mo.dim, &mut e.mo.bar, &mut e.mo.pop, &mut e.mo.hint, &mut e.mo.hover] {
                    t.snap(t.target());
                }
                if let Some(n) = e.notice.as_mut() {
                    n.at = Instant::now() - Duration::from_millis(500);
                }
            }
            app.frame(); // settle: tweens that start in frame() (bar/pop/hint)
            if let State::Edit(e) = &mut app.st {
                for t in [&mut e.mo.bar, &mut e.mo.pop, &mut e.mo.hint] {
                    t.snap(t.target());
                }
            }
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

        let mut app = annotated(theme::LIGHT, big);
        let e = edit_of(&mut app);
        e.objects.push(Obj::Text {
            pos: Pt::new(300.0, 420.0),
            text: "Annotation text\nПривет, мир · مرحبا · 你好".into(),
            color: C4::rgb(240, 68, 56),
            size: 28.0,
        });
        e.dirty = true;
        save(&mut app, "light-text-object.png");

        let mut app = preview_app(theme::DARK, None);
        save(&mut app, "dark-hint.png");

        let sel = FRect { x: 100.0, y: 700.0, w: 800.0, h: 180.0 };
        let mut app = preview_app_with(theme::DARK, Some(sel), two_monitor_shot());
        edit_of(&mut app).notice = Some(Toast::new("Copied", ToastKind::Success));
        save(&mut app, "dark-two-monitors.png");
    }

    fn two_mons() -> Vec<capture::IRect> {
        vec![(0, 0, 1000, 800), (1000, 0, 1000, 800)]
    }

    #[test]
    fn pick_area_follows_selection_overlap() {
        let m = two_mons();
        let sel = FRect { x: 900.0, y: 100.0, w: 400.0, h: 100.0 };
        let a = pick_area(&m, Some(sel), Pt::new(0.0, 0.0));
        assert_eq!((a.x, a.w), (1000.0, 1000.0));
        let split = FRect { x: 800.0, y: 100.0, w: 400.0, h: 100.0 };
        let a = pick_area(&m, Some(split), Pt::new(1500.0, 10.0));
        assert_eq!(a.x, 0.0, "tie goes to the first monitor");
    }

    #[test]
    fn pick_area_falls_back_to_pointer_then_first() {
        let m = vec![(0, 0, 1000, 800), (1100, 0, 1000, 800)];
        let a = pick_area(&m, None, Pt::new(1500.0, 10.0));
        assert_eq!(a.x, 1100.0);
        let a = pick_area(&m, None, Pt::new(1050.0, 10.0));
        assert_eq!(a.x, 0.0, "pointer in a gap");
    }

    #[test]
    fn toast_fades_in_and_out() {
        let t = Toast::new("x", ToastKind::Info);
        let at = t.at;
        let ms = |n: u64| at + Duration::from_millis(n);
        assert!(t.opacity(ms(0)) < 0.01);
        assert_eq!(t.opacity(ms(120)), 1.0);
        assert!((t.opacity(ms(1650)) - 0.5).abs() < 0.02);
        assert!(!t.expired(ms(1650)));
        assert!(t.expired(ms(1701)));
        assert!(t.animating(ms(10)) && !t.animating(ms(800)) && t.animating(ms(1500)));
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

    #[test]
    fn settled_editor_is_not_stale() {
        let now = Instant::now();
        let mut app = preview_app(theme::DARK, Some(FRect { x: 10.0, y: 10.0, w: 200.0, h: 100.0 }));
        let e = edit_of(&mut app);
        for t in [&mut e.mo.dim, &mut e.mo.bar, &mut e.mo.pop, &mut e.mo.hint, &mut e.mo.hover] {
            t.snap(t.target());
        }
        assert!(!stale(e, None, now), "idle tick: no frame");
        e.hover = Some(0);
        e.hover_at = now;
        assert!(stale(e, None, now), "tooltip pending");
        e.hover_at = now - Duration::from_secs(1);
        assert!(!stale(e, None, now), "tooltip settled");
        let mut toast = Toast::new("x", ToastKind::Info);
        assert!(stale(e, Some(&toast), now), "toast fading in");
        toast.at = now - Duration::from_millis(500);
        e.toast_drawn = Some(toast.at);
        assert!(!stale(e, Some(&toast), now), "toast steady");
        e.toast_drawn = None;
        e.text = Some(TextDraft { pos: Pt::new(0.0, 0.0), text: String::new(), caret: 0, at: now });
        assert!(stale(e, None, now), "caret never drawn");
        e.caret_drawn = Some(true);
        assert!(!stale(e, None, now), "same blink phase");
        e.text.as_mut().unwrap().at = now - Duration::from_millis(600);
        assert!(stale(e, None, now), "blink phase flipped");
    }

    #[test]
    fn toast_set_or_cleared_between_frames_is_stale() {
        let now = Instant::now();
        let mut app = preview_app(theme::DARK, Some(FRect { x: 10.0, y: 10.0, w: 200.0, h: 100.0 }));
        let e = edit_of(&mut app);
        for t in [&mut e.mo.dim, &mut e.mo.bar, &mut e.mo.pop, &mut e.mo.hint, &mut e.mo.hover] {
            t.snap(t.target());
        }
        assert!(!stale(e, None, now), "settled");
        let mut toast = Toast::new("Uploaded", ToastKind::Success);
        toast.at = now - Duration::from_millis(500);
        assert!(stale(e, Some(&toast), now), "set between frames, age > 120 ms");
        e.toast_drawn = Some(toast.at);
        assert!(!stale(e, Some(&toast), now), "drawn");
        assert!(stale(e, None, now), "cleared between frames");
        e.toast_drawn = None;
        e.notice = Some(toast);
        assert!(stale(e, None, now), "edit toast set between frames");
        let mut newer = Toast::new("Failed", ToastKind::Error);
        newer.at = now - Duration::from_millis(300);
        e.toast_drawn = Some(newer.at);
        assert!(stale(e, None, now), "replaced by another toast");
    }

    /// The former per-pixel float dim, kept as the reference.
    fn dim_rect_ref(img: &mut PixBuf, x: f32, y: f32, w: f32, h: f32, c: C4) {
        if w <= 0.0 || h <= 0.0 || c.a == 0 {
            return;
        }
        let (bw, bh) = (img.width() as i32, img.height() as i32);
        let x0 = x.floor().max(0.0) as i32;
        let y0 = y.floor().max(0.0) as i32;
        let (x1, y1) = (x1_clamp(x, w, bw), y1_clamp(y, h, bh));
        let a = c.a as f32 / 255.0;
        let k = 1.0 - a;
        let data = img.as_raw_mut();
        for y in y0.max(0)..y1.min(bh) {
            for x in x0.max(0)..x1.min(bw) {
                let i = ((y * bw + x) as usize) * 4;
                for (ch, sv) in [c.r, c.g, c.b].iter().enumerate() {
                    data[i + ch] = (data[i + ch] as f32 * k + *sv as f32 * a).round() as u8;
                }
            }
        }
    }

    #[test]
    fn dimmer_matches_float_blend_exhaustively() {
        for a in 0..=255u8 {
            for cv in 0..=255u8 {
                let c = C4::new(cv, 255 - cv, cv / 2, a);
                let d = Dimmer::new(c);
                let (af, k) = (a as f32 / 255.0, 1.0 - a as f32 / 255.0);
                for v in 0..=255u8 {
                    let px = [v, v, v, 200, 255 - v, v, 255 - v, 9];
                    let mut out = [0u8; 8];
                    d.run(&px, &mut out, 1);
                    for (j, &sv) in [c.r, c.g, c.b, 0, c.r, c.g, c.b, 0].iter().enumerate() {
                        let want = if j % 4 == 3 { px[j] } else { (px[j] as f32 * k + sv as f32 * af).round() as u8 };
                        assert_eq!(out[j], want, "a {a} c {c:?} v {v} byte {j}");
                    }
                }
            }
        }
    }

    #[test]
    fn backdrop_matches_four_dim_rects() {
        use compose::{backdrop_rect, PxRect};
        let src = synthetic_shot().image.crop(100, 80, 301, 211);
        let (ww, wh) = (src.width() as f32, src.height() as f32);
        let size = src.dimensions();
        let c = C4::new(8, 10, 14, 141);
        let bd = PixBufBackdrop { img: &src, dim: c };
        let whole = PxRect::image(size);
        for sr in [
            FRect { x: 40.0, y: 30.0, w: 120.0, h: 90.0 },
            FRect { x: 40.4, y: 30.6, w: 120.3, h: 90.2 },
            FRect { x: -5.0, y: 0.0, w: 400.0, h: 0.5 },
            FRect { x: 300.5, y: 210.5, w: 3.0, h: 3.0 },
        ] {
            let rects = [
                (0.0, 0.0, ww, sr.y),
                (0.0, sr.y1(), ww, wh - sr.y1()),
                (0.0, sr.y, sr.x, sr.h),
                (sr.x1(), sr.y, ww - sr.x1(), sr.h),
            ];
            let mut want = src.clone();
            for r in rects {
                dim_rect_ref(&mut want, r.0, r.1, r.2, r.3, c);
            }
            let mut got = PixBuf::from_pixel(src.width(), src.height(), [1, 2, 3, 4]);
            backdrop_rect(Some(sr), size, c.a, whole, &bd, got.as_raw_mut(), Order::Rgba);
            assert!(got == want, "{sr:?}");
        }
        let mut got = PixBuf::new(src.width(), src.height());
        backdrop_rect(None, size, 0, whole, &bd, got.as_raw_mut(), Order::Rgba);
        assert!(got == src, "alpha 0 copies");
    }

    // ---- rect composition: tilings and dirty rects ----

    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: u64) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 % n
        }
    }

    fn snap_all(app: &mut App) {
        let e = edit_of(app);
        for t in [&mut e.mo.dim, &mut e.mo.bar, &mut e.mo.pop, &mut e.mo.hint, &mut e.mo.hover] {
            t.snap(t.target());
        }
    }

    /// Settle every tween (some start in `frame()`), then paint.
    fn settled(app: &mut App) -> PixBuf {
        snap_all(app);
        app.frame();
        snap_all(app);
        app.frame().expect("frame").clone()
    }

    /// A tween stuck at about a third of the way to `to`.
    fn mid(to: f32) -> Tween {
        let now = Instant::now();
        let mut t = Tween::new(0.0, now);
        t.set(to, 1_000_000, now - Duration::from_secs(60));
        t
    }

    /// Compose the prepared scene through random tiles (both orders) and
    /// compare with the whole-image frame `want`.
    fn assert_tiles_match(app: &mut App, want: &PixBuf, name: &str, seed: u64) {
        use compose::PxRect;
        let e = edit_of(app);
        let bd = PixBufBackdrop { img: e.composed(), dim: e.th.dim };
        let (w, h) = (e.shot.size.0 as i32, e.shot.size.1 as i32);
        let mut rng = Rng(seed | 1);
        let mut n = 0;
        let mut y = 0;
        while y < h {
            let cap = if rng.below(4) == 0 { 7 } else { 150 };
            let th = 1 + rng.below(cap) as i32;
            let mut x = 0;
            while x < w {
                let cap = if rng.below(4) == 0 { 9 } else { 260 };
                let tw = 1 + rng.below(cap) as i32;
                let r = PxRect::new(x, y, (x + tw).min(w), (y + th).min(h));
                let order = if n % 2 == 0 { Order::Bgra } else { Order::Rgba };
                n += 1;
                let mut out = vec![7u8; (r.w() * r.h() * 4) as usize];
                e.compose_rect(r, &bd, &mut out, order);
                for ty in 0..r.h() {
                    for tx in 0..r.w() {
                        let i = ((ty * r.w() + tx) * 4) as usize;
                        let mut got = [out[i], out[i + 1], out[i + 2], out[i + 3]];
                        if order == Order::Bgra {
                            got.swap(0, 2);
                        }
                        let exp = want.get_pixel((r.x0 + tx) as u32, (r.y0 + ty) as u32);
                        assert_eq!(got, exp, "{name}: pixel ({}, {}) in tile {r:?} {order:?}", r.x0 + tx, r.y0 + ty);
                    }
                }
                x += tw;
            }
            y += th;
        }
    }

    fn tiling_states() -> Vec<(&'static str, App)> {
        let big = FRect { x: 240.0, y: 140.0, w: 960.0, h: 540.0 };
        let mut v = Vec::new();
        v.push(("A hint", preview_app(theme::DARK, None)));
        v.push(("B toolbar", annotated(theme::DARK, big)));
        let mut app = annotated(theme::DARK, big);
        edit_of(&mut app).interact = Interact::NewSel { anchor: Pt::new(big.x, big.y), moved: true };
        v.push(("C dragging", app));
        let mut app = annotated(theme::DARK, big);
        edit_of(&mut app).palette_open = true;
        v.push(("D palette", app));
        let mut app = annotated(theme::LIGHT, big);
        app.frame();
        let e = edit_of(&mut app);
        let copy = e.toolbar.as_ref().unwrap().items.iter().position(|i| i.kind == toolbar::Kind::Btn(Act::Copy));
        e.hover = copy;
        e.hover_at = Instant::now() - Duration::from_secs(2);
        let mut toast = Toast::new("Size 4", ToastKind::Info);
        toast.at = Instant::now() - Duration::from_millis(500);
        e.notice = Some(toast);
        v.push(("light tooltip toast", app));
        let mut app = annotated(theme::DARK, big);
        let e = edit_of(&mut app);
        e.tool = Some(Tool::Text);
        let text = "Check this".to_string();
        e.text = Some(TextDraft { pos: Pt::new(400.0, 300.0), caret: 5, text, at: Instant::now() });
        v.push(("text", app));
        let sel = FRect { x: 100.0, y: 700.0, w: 800.0, h: 180.0 };
        let mut app = preview_app_with(theme::DARK, Some(sel), two_monitor_shot());
        let mut toast = Toast::new("Uploaded https://example.com/a-rather-long-url-that-runs-on", ToastKind::Success);
        toast.at = Instant::now() - Duration::from_millis(500);
        app.notice = Some(toast);
        v.push(("two monitors", app));
        v.push(("narrow", annotated(theme::DARK, FRect { x: 500.0, y: 300.0, w: 380.0, h: 200.0 })));
        v.push(("fractional", annotated(theme::DARK, FRect { x: 240.4, y: 140.6, w: 960.3, h: 540.2 })));
        // Drafts straddling the selection edge; pixelate needs its whole rect.
        for (name, d) in [
            ("draft pixelate", Obj::Pixelate { r: FRect { x: 180.0, y: 100.0, w: 300.5, h: 210.0 }, cell: 13.0 }),
            ("draft invert", Obj::Invert { r: FRect { x: 1100.0, y: 600.0, w: 200.0, h: 150.0 } }),
            ("draft arrow", Obj::Arrow { a: Pt::new(150.0, 120.0), b: Pt::new(700.0, 420.0), color: C4::rgb(20, 200, 90), width: 9.0 }),
            ("draft marker", Obj::Marker { a: Pt::new(200.0, 600.0), b: Pt::new(900.0, 640.0), color: C4::rgb(250, 220, 0).with_alpha(90), width: 20.0 }),
        ] {
            let mut app = annotated(theme::DARK, big);
            let e = edit_of(&mut app);
            e.interact = Interact::Drawing { start: Pt::new(0.0, 0.0) };
            e.draft = Some(d);
            v.push((name, app));
        }
        let mut app = annotated(theme::DARK, big);
        let e = edit_of(&mut app);
        e.objects.push(Obj::Pixelate { r: FRect { x: 200.0, y: 400.0, w: 333.0, h: 120.0 }, cell: 11.0 });
        e.hover = Some(3);
        e.pressed = Some(4);
        e.hot_handle = Some(2);
        v.push(("objects hover pressed", app));
        v
    }

    #[test]
    fn random_tilings_match_whole_frame() {
        for (i, (name, mut app)) in tiling_states().into_iter().enumerate() {
            let want = settled(&mut app);
            assert_tiles_match(&mut app, &want, name, 0x9E37_79B9 + i as u64);
        }
    }

    /// Mid-animation: dim, toolbar scale/fade, palette, hint, hover and
    /// toast at fractional opacities.
    #[test]
    fn random_tilings_match_mid_animation() {
        let big = FRect { x: 240.0, y: 140.0, w: 960.0, h: 540.0 };
        let mut app = annotated(theme::DARK, big);
        app.frame();
        let e = edit_of(&mut app);
        e.palette_open = true;
        e.hover = Some(1);
        (e.mo.dim, e.mo.bar, e.mo.pop, e.mo.hover) = (mid(1.0), mid(1.0), mid(1.0), mid(1.0));
        let mut toast = Toast::new("Halfway", ToastKind::Error);
        toast.at = Instant::now() - Duration::from_millis(30);
        e.notice = Some(toast);
        let want = app.frame().expect("frame").clone();
        assert_tiles_match(&mut app, &want, "mid toolbar", 77);

        let mut app = preview_app(theme::LIGHT, None);
        app.frame();
        let e = edit_of(&mut app);
        (e.mo.dim, e.mo.hint) = (mid(1.0), mid(1.0));
        let want = app.frame().expect("frame").clone();
        assert_tiles_match(&mut app, &want, "mid hint", 78);
    }

    /// Paint, apply `change`, paint again: every pixel that differs lies in
    /// `dirty_rects(before, after)`. Returns the dirty area fraction.
    fn assert_dirty_covers(app: &mut App, name: &str, change: impl FnOnce(&mut App)) -> f64 {
        let before = app.frame().expect("frame").clone();
        let prev = edit_of(app).scene.clone().expect("scene");
        change(app);
        let after = app.frame().expect("frame").clone();
        let e = edit_of(app);
        let rects = e.dirty_rects(Some(&prev));
        let (w, h) = before.dimensions();
        let (a, b) = (before.as_raw(), after.as_raw());
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let i = (y as usize * w as usize + x as usize) * 4;
                if a[i..i + 4] != b[i..i + 4] {
                    assert!(
                        rects.iter().any(|r| r.contains_px(x, y)),
                        "{name}: pixel ({x}, {y}) changed outside {rects:?}"
                    );
                }
            }
        }
        let area: i64 = rects.iter().map(|r| r.w() as i64 * r.h() as i64).sum();
        area as f64 / (w as f64 * h as f64)
    }

    /// A pixelate drag repaints only the cells along the moving edges, and
    /// still covers every changed pixel (grow, shrink, fractional edges,
    /// past the image edge, the anchor moving, cell size changing).
    #[test]
    fn dirty_rects_cover_pixelate_drag() {
        let big = FRect { x: 240.0, y: 140.0, w: 960.0, h: 540.0 };
        let mut app = annotated(theme::DARK, big);
        settled(&mut app);
        let set = |app: &mut App, r: FRect, cell: f32| {
            let e = edit_of(app);
            e.interact = Interact::Drawing { start: Pt::new(r.x, r.y) };
            e.draft = Some(Obj::Pixelate { r, cell });
        };
        let r0 = FRect { x: 180.0, y: 100.0, w: 700.0, h: 500.0 };
        assert_dirty_covers(&mut app, "start", |a| set(a, r0, 12.0));
        let f = assert_dirty_covers(&mut app, "grow", |a| set(a, FRect { w: 706.0, h: 503.0, ..r0 }, 12.0));
        assert!(f < 0.1, "grow repaints the edges only: {f}");
        let f = assert_dirty_covers(&mut app, "shrink", |a| set(a, FRect { w: 650.5, h: 470.25, ..r0 }, 12.0));
        assert!(f < 0.1, "shrink repaints the edges only: {f}");
        assert_dirty_covers(&mut app, "fractional", |a| set(a, FRect { x: 180.4, y: 100.6, w: 651.0, h: 471.0 }, 12.0));
        assert_dirty_covers(&mut app, "past the edge", |a| set(a, FRect { x: 180.0, y: 100.0, w: 1400.0, h: 900.0 }, 12.0));
        assert_dirty_covers(&mut app, "back", |a| set(a, FRect { w: 1300.0, h: 820.0, ..r0 }, 12.0));
        assert_dirty_covers(&mut app, "anchor moves", |a| set(a, FRect { x: 150.0, y: 90.0, w: 730.0, h: 510.0 }, 12.0));
        assert_dirty_covers(&mut app, "cell changes", |a| set(a, FRect { x: 150.0, y: 90.0, w: 730.0, h: 510.0 }, 13.0));
        let mut neg = annotated(theme::DARK, big);
        settled(&mut neg);
        assert_dirty_covers(&mut neg, "offscreen start", |a| set(a, FRect { x: -30.5, y: -20.0, w: 300.0, h: 200.0 }, 9.0));
        assert_dirty_covers(&mut neg, "offscreen grow", |a| set(a, FRect { x: -30.5, y: -20.0, w: 340.0, h: 230.0 }, 9.0));
    }

    #[test]
    fn dirty_rects_cover_every_changed_pixel() {
        let big = FRect { x: 240.0, y: 140.0, w: 960.0, h: 540.0 };
        let mut app = annotated(theme::DARK, big);
        settled(&mut app);
        let small = |f: f64, name: &str| assert!(f < 0.5, "{name}: dirty area {f}");

        let f = assert_dirty_covers(&mut app, "drag", |app| {
            let e = edit_of(app);
            e.interact = Interact::MoveSel { start: Pt::new(0.0, 0.0), orig: big };
            e.sel = Some(FRect { x: big.x + 13.0, y: big.y + 7.0, ..big });
        });
        small(f, "drag");
        let f = assert_dirty_covers(&mut app, "resize", |app| {
            let e = edit_of(app);
            e.sel = Some(FRect { x: big.x + 13.0, y: big.y + 7.0, w: big.w + 20.0, h: big.h - 5.0 });
        });
        small(f, "resize");
        assert_dirty_covers(&mut app, "drop", |app| {
            let e = edit_of(app);
            e.interact = Interact::None;
            e.sel = Some(big);
        });
        snap_all(&mut app);
        let f = assert_dirty_covers(&mut app, "hover", |app| {
            let e = edit_of(app);
            e.hover = Some(2);
            e.hover_at = Instant::now();
            e.mo.hover = mid(1.0);
        });
        small(f, "hover");
        let f = assert_dirty_covers(&mut app, "hover settles", snap_all);
        small(f, "hover settles");
        let f = assert_dirty_covers(&mut app, "tooltip appears", |app| {
            edit_of(app).hover_at = Instant::now() - Duration::from_secs(2);
        });
        small(f, "tooltip");
        let f = assert_dirty_covers(&mut app, "hover moves", |app| edit_of(app).hover = Some(5));
        small(f, "hover moves");
        assert_dirty_covers(&mut app, "hover leaves", |app| edit_of(app).hover = None);
        assert_dirty_covers(&mut app, "hot handle", |app| edit_of(app).hot_handle = Some(4));
        assert_dirty_covers(&mut app, "pressed", |app| edit_of(app).pressed = Some(6));
        let f = assert_dirty_covers(&mut app, "palette opens", |app| edit_of(app).palette_open = true);
        small(f, "palette");
        assert_dirty_covers(&mut app, "palette fades in", snap_all);
        assert_dirty_covers(&mut app, "palette closes", |app| edit_of(app).palette_open = false);
        let f = assert_dirty_covers(&mut app, "toast set", |app| {
            edit_of(app).notice = Some(Toast::new("Size 5", ToastKind::Info));
        });
        small(f, "toast");
        assert_dirty_covers(&mut app, "toast steady", |app| {
            edit_of(app).notice.as_mut().unwrap().at = Instant::now() - Duration::from_millis(500);
        });
        assert_dirty_covers(&mut app, "toast fade frame", |app| {
            edit_of(app).notice.as_mut().unwrap().at = Instant::now() - Duration::from_millis(1650);
        });
        assert_dirty_covers(&mut app, "toast cleared", |app| edit_of(app).notice = None);
        assert_dirty_covers(&mut app, "app toast", |app| {
            let mut t = Toast::new("Uploaded https://example.com/x", ToastKind::Success);
            t.at = Instant::now() - Duration::from_millis(500);
            app.notice = Some(t);
        });
        assert_dirty_covers(&mut app, "app toast cleared", |app| app.notice = None);
        let f = assert_dirty_covers(&mut app, "tool switch", |app| edit_of(app).tool = Some(Tool::Rect));
        small(f, "tool");
        let f = assert_dirty_covers(&mut app, "draft", |app| {
            let e = edit_of(app);
            e.interact = Interact::Drawing { start: Pt::new(300.0, 200.0) };
            e.draft = e.make_draft(Tool::Rect, Pt::new(300.0, 200.0), Pt::new(420.0, 260.0));
        });
        small(f, "draft");
        assert_dirty_covers(&mut app, "draft grows", |app| {
            let e = edit_of(app);
            e.draft = e.make_draft(Tool::Rect, Pt::new(300.0, 200.0), Pt::new(520.0, 330.0));
        });
        let f = assert_dirty_covers(&mut app, "commit", |app| end_interaction(edit_of(app)));
        small(f, "commit");
        assert_dirty_covers(&mut app, "pixelate commit", |app| {
            edit_of(app).commit_object(Obj::Pixelate { r: FRect { x: 150.0, y: 100.0, w: 260.0, h: 200.0 }, cell: 12.0 });
        });
        let f = assert_dirty_covers(&mut app, "undo", |app| edit_of(app).undo());
        small(f, "undo");
        assert_dirty_covers(&mut app, "undo again", |app| edit_of(app).undo());
        assert_dirty_covers(&mut app, "redo", |app| edit_of(app).redo());
        assert_dirty_covers(&mut app, "text draft", |app| {
            let e = edit_of(app);
            e.tool = Some(Tool::Text);
            e.text = Some(TextDraft { pos: Pt::new(500.0, 400.0), text: "Hi".into(), caret: 2, at: Instant::now() });
        });
        let f = assert_dirty_covers(&mut app, "caret blink", |app| {
            edit_of(app).text.as_mut().unwrap().at = Instant::now() - Duration::from_millis(600);
        });
        small(f, "caret");
        assert_dirty_covers(&mut app, "typing", |app| {
            let td = edit_of(app).text.as_mut().unwrap();
            td.text.push_str(" there");
            td.caret = td.text.len();
            td.at = Instant::now();
        });
        assert_dirty_covers(&mut app, "fractional sel", |app| {
            edit_of(app).sel = Some(FRect { x: 240.5, y: 140.0, w: 960.0, h: 540.0 });
        });

        // No selection yet: hint, then the first selection appears.
        let mut app = preview_app(theme::DARK, None);
        settled(&mut app);
        assert_dirty_covers(&mut app, "first selection", |app| {
            let e = edit_of(app);
            e.interact = Interact::NewSel { anchor: Pt::new(100.0, 100.0), moved: true };
            e.sel = Some(FRect { x: 100.0, y: 100.0, w: 50.0, h: 40.0 });
        });
        assert_dirty_covers(&mut app, "hint fades", snap_all);
        let f = assert_dirty_covers(&mut app, "sel grows", |app| {
            edit_of(app).sel = Some(FRect { x: 100.0, y: 100.0, w: 90.0, h: 70.0 });
        });
        small(f, "sel grows");
        assert_dirty_covers(&mut app, "dim fade", |app| edit_of(app).mo.dim = mid(1.0));
    }

    /// Every pixel the chrome and draft change over the bare backdrop lies
    /// inside `chrome_rects`.
    #[test]
    fn chrome_rects_bound_the_chrome() {
        for (name, mut app) in tiling_states() {
            let want = settled(&mut app);
            let e = edit_of(&mut app);
            let sc = e.scene.as_ref().unwrap();
            let size = sc.size;
            let mut bg = PixBuf::new(size.0, size.1);
            let bd = PixBufBackdrop { img: e.composed(), dim: e.th.dim };
            compose::backdrop_rect(sc.sel, size, sc.dim_alpha, compose::PxRect::image(size), &bd, bg.as_raw_mut(), Order::Rgba);
            let rects = e.chrome_rects();
            for y in 0..size.1 {
                for x in 0..size.0 {
                    if bg.get_pixel(x, y) != want.get_pixel(x, y) {
                        assert!(rects.iter().any(|r| r.contains_px(x as i32, y as i32)), "{name}: ({x}, {y})");
                    }
                }
            }
        }
    }

    /// Release-mode frame cost at 5120x1440 (states A-D of the perf report).
    /// `cargo test --release perf_frame_bench -- --ignored --nocapture`
    #[test]
    #[ignore = "benchmark; run in release"]
    fn perf_frame_bench() {
        let (w, h) = (5120u32, 1440u32);
        let src = synthetic_shot();
        let mut img = PixBuf::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let i = (y as usize * w as usize + x as usize) * 4;
                let j = ((y % src.size.1) as usize * src.size.0 as usize + (x % src.size.0) as usize) * 4;
                img.as_raw_mut()[i..i + 4].copy_from_slice(&src.image.as_raw()[j..j + 4]);
            }
        }
        let shot = Shot { origin: (0, 0), size: (w, h), scale: 1.0, image: img, monitors: vec![(0, 0, w, h)] };
        let sel = FRect { x: 1000.0, y: 200.0, w: 2000.0, h: 900.0 };
        let median = |mut v: Vec<f64>| {
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            v[v.len() / 2]
        };
        for (name, s, drag, pal) in [
            ("A no sel, hint", None, false, false),
            ("B sel 2000x900 + toolbar", Some(sel), false, false),
            ("C dragging NewSel", Some(sel), true, false),
            ("D palette open", Some(sel), false, true),
        ] {
            let mut app = preview_app_with(theme::DARK, s, Shot { image: shot.image.clone(), monitors: shot.monitors.clone(), ..shot });
            {
                let e = edit_of(&mut app);
                e.palette_open = pal;
                if drag {
                    e.interact = Interact::NewSel { anchor: Pt::new(sel.x, sel.y), moved: true };
                }
            }
            for _ in 0..2 {
                let _ = app.frame();
                let e = edit_of(&mut app);
                for t in [&mut e.mo.dim, &mut e.mo.bar, &mut e.mo.pop, &mut e.mo.hint, &mut e.mo.hover] {
                    t.snap(t.target());
                }
            }
            let (mut tf, mut tp) = (Vec::new(), Vec::new());
            for _ in 0..30 {
                let t0 = Instant::now();
                let fb = app.frame().expect("frame");
                let t1 = Instant::now();
                bench_present_prep(fb);
                let t2 = Instant::now();
                std::hint::black_box(&*fb);
                tf.push((t1 - t0).as_secs_f64() * 1000.0);
                tp.push((t2 - t1).as_secs_f64() * 1000.0);
            }
            let (f, p) = (median(tf), median(tp));
            println!("BENCH {name}: frame {f:.2} ms, present prep {p:.2} ms, sum {:.2} ms", f + p);
        }
    }

    /// What `wind_win::present` does before StretchDIBits.
    fn bench_present_prep(fb: &mut PixBuf) {
        #[cfg(windows)]
        crate::wind::swap_rb(fb.as_raw_mut());
        #[cfg(not(windows))]
        let _ = fb;
    }

    #[cfg(windows)]
    #[path = "../gdi_tests.rs"]
    mod gdi_path;
}
