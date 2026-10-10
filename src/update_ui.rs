//! The update dialog: "Rustshot X is available (you have Y)" with the
//! release notes and Update / Skip this version / Cancel, a progress bar
//! while downloading, then install and restart; also "up to date", check
//! errors (Retry / Close) and store-managed installs. Shown by the daemon's
//! daily check and by the tray's "Check for updates".
//!
//! The dialog is a [`Machine`] (pure: events in, actions out, unit-tested)
//! drawn with the `ui` kit in a `wind::run_window` window on its own
//! thread. Network and disk work runs on worker threads that report back
//! through a channel the window drains on its timer. One dialog at a time:
//! [`show`] while it is open replaces its content (unless a download or
//! install is running) and brings it to the front.
//!
//! macOS opens windows only on the main thread (the daemon's overlay loop),
//! so there the dialog is not shown: an available release opens its page.
#![cfg_attr(target_os = "macos", allow(dead_code))]

use crate::hotkey::HotEvent;
use crate::objects::FRect;
use crate::pixbuf::PixBuf;
use crate::theme::Theme;
use crate::ui::{FocusState, Input, Ui};
use crate::uifb::C4;
use crate::update::{self, Release};
use crate::update_install::{self, Applied};
use crate::wind::{self, Cursor, Driver, Ev, Hwnd, WindowSpec};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

/// This build's version.
const CURRENT: &str = env!("CARGO_PKG_VERSION");
/// Release-notes lines shown (the box scrolls when they don't fit).
const NOTES_LINES: usize = 12;
/// Window size, logical px: with release notes / message only.
const W: u32 = 440;
const H_FULL: u32 = 300;
const H_SHORT: u32 = 184;
const PAD: f32 = 20.0;
const ICON: f32 = 24.0;

/// What the dialog opens with.
#[derive(Clone, Debug, PartialEq)]
pub enum DialogState {
    Available(Release),
    UpToDate,
    /// The check failed (message); Retry checks again.
    Error(String),
    /// A store updates this install (its name).
    Managed(&'static str),
}

impl DialogState {
    /// The dialog for the result of `update::check_now`.
    pub fn from_check(r: Result<Option<Release>, String>) -> DialogState {
        match r {
            Ok(Some(rel)) => DialogState::Available(rel),
            Ok(None) => DialogState::UpToDate,
            Err(e) => DialogState::Error(e),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Btn {
    Update,
    Skip,
    Cancel,
    Ok,
    Retry,
    Close,
}

impl Btn {
    fn label(self) -> &'static str {
        match self {
            Btn::Update => "Update",
            Btn::Skip => "Skip this version",
            Btn::Cancel => "Cancel",
            Btn::Ok => "OK",
            Btn::Retry => "Retry",
            Btn::Close => "Close",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Btn::Update => "update",
            Btn::Skip => "skip",
            Btn::Cancel => "cancel",
            Btn::Ok => "ok",
            Btn::Retry => "retry",
            Btn::Close => "close",
        }
    }
}

/// What the dialog shows.
#[derive(Clone, Debug, PartialEq)]
enum Screen {
    /// Retry after a failed check: waiting for `check_now`.
    Checking,
    Available(Release),
    UpToDate,
    Managed(&'static str),
    CheckFailed(String),
    /// Downloading the asset (bytes so far, total when known).
    Downloading { rel: Release, got: u64, total: Option<u64> },
    /// Download complete; its SHA-256 is being checked.
    Verifying(Release),
    /// Cancel pressed; waiting for the download to stop and clean up.
    Cancelling(Release),
    /// The installer / swap runs; nothing can be cancelled any more.
    Installing(Release),
    /// Download, verification or install failed; Retry downloads again.
    InstallFailed { rel: Release, msg: String },
    /// Writing `skip_version` failed.
    SaveFailed(String),
}

impl From<DialogState> for Screen {
    fn from(s: DialogState) -> Screen {
        match s {
            DialogState::Available(r) => Screen::Available(r),
            DialogState::UpToDate => Screen::UpToDate,
            DialogState::Error(e) => Screen::CheckFailed(e),
            DialogState::Managed(m) => Screen::Managed(m),
        }
    }
}

/// Input to the [`Machine`]: user actions and worker results.
#[derive(Debug)]
pub enum In {
    Click(Btn),
    /// The window's close button, Alt+F4 or Esc.
    Dismiss,
    /// Enter not taken by a focused control: the primary button.
    Enter,
    Checked(Result<Option<Release>, String>),
    Progress(u64, Option<u64>),
    /// The download finished and passed its SHA-256 check (Err: it failed
    /// or was cancelled; the files are gone either way).
    Fetched(Result<(), String>),
    Installed(Result<Applied, String>),
    Skipped(Result<(), String>),
    /// [`show`] while the dialog is open.
    Show(DialogState),
}

/// What the [`Machine`] asks its driver to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Act {
    Close,
    /// Run `update::check_now` on a worker (→ `In::Checked`).
    Check,
    /// Download, verify and install on a worker (→ `Progress`, `Fetched`, `Installed`).
    Download(Release),
    /// Abort the running download.
    Cancel,
    /// Write `skip_version` (→ `In::Skipped`).
    Skip(String),
    /// The new version is starting: end the daemon now.
    QuitDaemon,
}

/// The dialog's state machine.
#[derive(Clone, Debug, PartialEq)]
pub struct Machine {
    screen: Screen,
}

impl Machine {
    pub fn new(s: DialogState) -> Machine {
        Machine { screen: s.into() }
    }

    /// A download or install is running (new content is not taken).
    fn busy(&self) -> bool {
        matches!(
            self.screen,
            Screen::Downloading { .. } | Screen::Verifying(_) | Screen::Cancelling(_) | Screen::Installing(_)
        )
    }

    /// The buttons, left to right (right-aligned), with the primary one.
    fn buttons(&self) -> (&'static [Btn], Option<Btn>) {
        use Btn::*;
        match self.screen {
            Screen::Available(_) => (&[Update, Skip, Cancel], Some(Update)),
            Screen::UpToDate | Screen::Managed(_) => (&[Ok], Some(Ok)),
            Screen::CheckFailed(_) | Screen::InstallFailed { .. } => (&[Retry, Close], Some(Retry)),
            Screen::SaveFailed(_) => (&[Close], Some(Close)),
            Screen::Checking | Screen::Downloading { .. } | Screen::Verifying(_) => (&[Cancel], None),
            Screen::Cancelling(_) | Screen::Installing(_) => (&[Cancel], None),
        }
    }

    /// The buttons that react (the others are drawn disabled).
    fn enabled(&self) -> bool {
        !matches!(self.screen, Screen::Cancelling(_) | Screen::Installing(_))
    }

    /// Advance on `ev`; returns what the driver must do, in order.
    pub fn on(&mut self, ev: In) -> Vec<Act> {
        use Screen as S;
        if let In::Show(st) = ev {
            if !self.busy() {
                self.screen = st.into();
            }
            return Vec::new();
        }
        // The button the event stands for (Enter = the primary one).
        let btn = match &ev {
            In::Click(b) => Some(*b),
            In::Enter => self.buttons().1,
            _ => None,
        };
        let busy = self.busy();
        let screen = std::mem::replace(&mut self.screen, S::Checking);
        let (next, acts) = match (screen, ev, btn) {
            (S::Checking, In::Checked(r), _) => (DialogState::from_check(r).into(), vec![]),
            (S::Available(rel), _, Some(Btn::Update)) => {
                (S::Downloading { rel: rel.clone(), got: 0, total: None }, vec![Act::Download(rel)])
            }
            (S::Available(rel), _, Some(Btn::Skip)) => {
                let v = rel.version.clone();
                (S::Available(rel), vec![Act::Skip(v)])
            }
            (s @ S::Available(_), In::Skipped(Ok(())), _) => (s, vec![Act::Close]),
            (S::Available(_), In::Skipped(Err(e)), _) => (S::SaveFailed(e), vec![]),
            (S::Downloading { rel, .. } | S::Verifying(rel), In::Progress(got, total), _) => {
                if total.is_some_and(|t| t > 0 && got >= t) {
                    (S::Verifying(rel), vec![])
                } else {
                    (S::Downloading { rel, got, total }, vec![])
                }
            }
            (S::Downloading { rel, .. } | S::Verifying(rel), In::Fetched(r), _) => match r {
                Ok(()) => (S::Installing(rel), vec![]),
                Err(msg) => (S::InstallFailed { rel, msg }, vec![]),
            },
            (S::Downloading { rel, .. } | S::Verifying(rel), In::Dismiss, _)
            | (S::Downloading { rel, .. } | S::Verifying(rel), _, Some(Btn::Cancel)) => {
                (S::Cancelling(rel), vec![Act::Cancel])
            }
            // Verified before the cancel reached the worker: it installs now.
            (S::Cancelling(rel), In::Fetched(Ok(())), _) => (S::Installing(rel), vec![]),
            (s @ S::Cancelling(_), In::Fetched(Err(_)), _) => (s, vec![Act::Close]),
            (s @ S::Installing(_), In::Installed(Ok(Applied::RestartingNow)), _) => {
                (s, vec![Act::QuitDaemon, Act::Close])
            }
            (s @ S::Installing(_), In::Installed(Ok(Applied::OpenedPage)), _) => (s, vec![Act::Close]),
            (S::Installing(rel), In::Installed(Err(msg)), _) => (S::InstallFailed { rel, msg }, vec![]),
            (S::InstallFailed { rel, .. }, _, Some(Btn::Retry)) => {
                (S::Downloading { rel: rel.clone(), got: 0, total: None }, vec![Act::Download(rel)])
            }
            (S::CheckFailed(_), _, Some(Btn::Retry)) => (S::Checking, vec![Act::Check]),
            (s, In::Dismiss, _) | (s, _, Some(Btn::Cancel | Btn::Close | Btn::Ok)) if !busy => (s, vec![Act::Close]),
            (s, _, _) => (s, vec![]),
        };
        self.screen = next;
        acts
    }
}

// --- Drawing ----------------------------------------------------------------

/// Per-window view state.
#[derive(Default)]
struct View {
    /// Release-notes scroll offset (logical px).
    scroll: f32,
    /// `notes_excerpt` of `notes_of`'s body.
    notes: String,
    notes_of: Option<String>,
}

impl View {
    fn notes(&mut self, rel: &Release) -> &str {
        if self.notes_of.as_deref() != Some(&rel.version) {
            self.notes = update::notes_excerpt(&rel.notes, NOTES_LINES);
            self.notes_of = Some(rel.version.clone());
            self.scroll = 0.0;
        }
        &self.notes
    }
}

fn mb(b: u64) -> String {
    format!("{:.1} MB", b as f64 / 1_000_000.0)
}

/// Heading icon: the app glyph, or a Lucide status icon in a theme colour.
enum Mark {
    App,
    Icon(&'static str, C4),
}

/// One frame of the dialog; returns the clicked button.
fn paint(ui: &mut Ui, m: &Machine, v: &mut View) -> Option<Btn> {
    let th = ui.theme;
    let b = ui.bounds();
    let available = |r: &Release| format!("Rustshot {} is available (you have {CURRENT})", r.version);
    let (mark, title, rel, text): (Mark, String, Option<&Release>, Option<String>) = match &m.screen {
        Screen::Checking => (Mark::App, "Checking for updates…".into(), None, None),
        Screen::Available(r) => (Mark::App, available(r), Some(r), None),
        Screen::Downloading { rel, .. }
        | Screen::Verifying(rel)
        | Screen::Cancelling(rel)
        | Screen::Installing(rel) => (Mark::App, available(rel), Some(rel), None),
        Screen::UpToDate => (
            Mark::Icon("okc", th.success),
            "Rustshot is up to date".into(),
            None,
            Some(format!("You have the latest version, {CURRENT}.")),
        ),
        Screen::Managed(store) => (
            Mark::Icon("info", th.accent_fg),
            format!("Rustshot {CURRENT}"),
            None,
            Some(format!("Updates for this install come from {store}.")),
        ),
        Screen::CheckFailed(e) => (Mark::Icon("alert", th.error), "Could not check for updates".into(), None, Some(e.clone())),
        Screen::InstallFailed { rel, msg } => (
            Mark::Icon("alert", th.error),
            format!("Rustshot {} could not be installed", rel.version),
            None,
            Some(msg.clone()),
        ),
        Screen::SaveFailed(e) => {
            (Mark::Icon("alert", th.error), "Could not save the setting".into(), None, Some(e.clone()))
        }
    };
    let inner = b.w - 2.0 * PAD;
    let btn_y = b.h - PAD - crate::ui::layout::H;
    // Title line.
    ui.area(FRect { x: PAD, y: PAD, w: inner, h: 32.0 }, |ui| {
        ui.row(|ui| {
            match mark {
                Mark::App => {
                    let r = ui.alloc(Some(ICON), crate::ui::layout::H);
                    let s = ui.px(ICON);
                    let c = th.accent;
                    crate::tray::draw_glyph(&mut ui.fb, r.x, (r.y + (r.h - s) / 2.0).round(), s, c);
                }
                Mark::Icon(name, c) => ui.icon(name, ICON, c),
            }
            ui.space(4.0);
            ui.heading(&title);
        })
    });
    let top = PAD + 32.0 + 12.0;
    // Progress line while a download / install runs.
    let progress = match &m.screen {
        Screen::Downloading { got, total, .. } => {
            let s = match total {
                Some(t) => format!("Downloading… {} of {}", mb(*got), mb(*t)),
                None if *got > 0 => format!("Downloading… {}", mb(*got)),
                None => "Downloading…".into(),
            };
            Some((s, total.map_or(0.0, |t| *got as f32 / t.max(1) as f32)))
        }
        Screen::Verifying(_) => Some(("Verifying the download…".into(), 1.0)),
        Screen::Cancelling(_) => Some(("Cancelling…".into(), 0.0)),
        Screen::Installing(_) => Some(("Installing… Rustshot will restart.".into(), 1.0)),
        _ => None,
    };
    let body_bottom = btn_y - 16.0 - if progress.is_some() { 40.0 } else { 0.0 };
    if let Some(r) = rel {
        let notes = v.notes(r).to_string();
        let notes = if notes.is_empty() { "No release notes.".to_string() } else { notes };
        let mut scroll = v.scroll;
        ui.place(FRect { x: PAD, y: top, w: inner, h: (body_bottom - top).max(40.0) }).text_view(
            "notes",
            &notes,
            &mut scroll,
        );
        v.scroll = scroll;
    }
    if let Some(t) = &text {
        ui.area(FRect { x: PAD + ICON + 4.0 + 8.0, y: top - 8.0, w: inner - ICON - 12.0, h: btn_y - top }, |ui| {
            ui.height(btn_y - 12.0 - (top - 8.0)).paragraph(t, true);
        });
    }
    if let Some((s, f)) = progress {
        ui.area(FRect { x: PAD, y: body_bottom + 10.0, w: inner, h: 40.0 }, |ui| {
            ui.lay.gap = 6.0;
            ui.note(&s);
            ui.progress(f);
        });
    }
    // Buttons, right-aligned.
    let (btns, primary) = m.buttons();
    let gap = crate::ui::layout::GAP;
    let total: f32 = btns.iter().map(|b| ui.button_width(b.label())).sum::<f32>() + gap * (btns.len() as f32 - 1.0);
    let mut clicked = None;
    ui.area(FRect { x: PAD, y: btn_y, w: inner, h: crate::ui::layout::H }, |ui| {
        ui.row(|ui| {
            ui.space(inner - total);
            ui.enabled = m.enabled();
            for &bt in btns {
                if ui.button(bt.id(), bt.label(), Some(bt) == primary) {
                    clicked = Some(bt);
                }
            }
            ui.enabled = true;
        })
    });
    clicked
}

// --- Window -----------------------------------------------------------------

/// The open dialog's inbox (`show` while open), if any.
static OPEN: Mutex<Option<Sender<In>>> = Mutex::new(None);
/// The daemon's event sender: `Quit` after an update restarts us.
static QUIT: Mutex<Option<Sender<HotEvent>>> = Mutex::new(None);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Register the daemon's event sender (an installed update quits through it).
pub fn set_quit_sender(tx: Sender<HotEvent>) {
    *lock(&QUIT) = Some(tx);
}

/// End the daemon cleanly (tray icon removed, instance released); the
/// relaunched version waits for this process to exit.
fn quit_daemon() {
    if let Some(tx) = lock(&QUIT).as_ref()
        && tx.send(HotEvent::Quit).is_ok()
    {
        return;
    }
    std::process::exit(0);
}

/// Open the dialog with `state` on its own thread, or, when it is already
/// open, hand it the new content and bring it to the front.
pub fn show(state: DialogState) {
    #[cfg(target_os = "macos")]
    {
        match state {
            DialogState::Available(r) => update::open_url(&r.url),
            DialogState::UpToDate => eprintln!("Rustshot is up to date"),
            DialogState::Error(e) => eprintln!("update check failed: {e}"),
            DialogState::Managed(s) => eprintln!("Updates for this install come from {s}."),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let mut open = lock(&OPEN);
        if let Some(tx) = open.as_ref()
            && tx.send(In::Show(state.clone())).is_ok()
        {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        *open = Some(tx.clone());
        drop(open);
        std::thread::spawn(move || run_dialog(state, tx, rx));
    }
}

fn run_dialog(state: DialogState, tx: Sender<In>, rx: Receiver<In>) {
    let h = if matches!(state, DialogState::Available(_)) { H_FULL } else { H_SHORT };
    let th = crate::theme::resolve(&crate::config::load());
    let mut d = Dialog::new(Machine::new(state), th, tx, rx);
    let spec = WindowSpec { title: "Rustshot update".into(), w: W, h, resizable: false, min: (W, h) };
    if let Err(e) = wind::run_window(spec, &mut d) {
        eprintln!("rustshot: update dialog: {e:#}");
    }
    d.cancel.store(true, Ordering::SeqCst);
    // A `show` that raced the close still gets its dialog.
    let mut open = lock(&OPEN);
    let late = d.rx.try_iter().filter_map(|e| if let In::Show(s) = e { Some(s) } else { None }).last();
    *open = None;
    drop(open);
    drop(d);
    if let Some(s) = late {
        show(s);
    }
}

/// The `wind` driver: renders on every input event into `canvas` (the
/// controls are immediate-mode), steps the machine and runs its actions.
struct Dialog {
    hwnd: Hwnd,
    m: Machine,
    view: View,
    th: Theme,
    input: Input,
    focus: FocusState,
    canvas: PixBuf,
    out: PixBuf,
    tx: Sender<In>,
    rx: Receiver<In>,
    queue: VecDeque<In>,
    /// Abort flag of the running download.
    cancel: Arc<AtomicBool>,
    /// `Act::Close` seen: the window closes at the end of the callback.
    closing: bool,
}

impl Dialog {
    fn new(m: Machine, th: Theme, tx: Sender<In>, rx: Receiver<In>) -> Dialog {
        Dialog {
            hwnd: Hwnd::default(),
            m,
            view: View::default(),
            th,
            input: Input::default(),
            focus: FocusState::default(),
            canvas: PixBuf::default(),
            out: PixBuf::default(),
            tx,
            rx,
            queue: VecDeque::new(),
            cancel: Arc::new(AtomicBool::new(false)),
            closing: false,
        }
    }

    /// Feed queued events to the machine and run its actions.
    fn process(&mut self) -> bool {
        let mut any = false;
        while let Some(ev) = self.queue.pop_front() {
            any = true;
            if matches!(ev, In::Show(_)) && !cfg!(test) {
                #[cfg(windows)]
                wind::raise(self.hwnd);
            }
            for a in self.m.on(ev) {
                self.perform(a);
            }
        }
        any
    }

    fn perform(&mut self, a: Act) {
        match a {
            Act::Close => self.closing = true,
            Act::Check => {
                let tx = self.tx.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(In::Checked(update::check_now()));
                });
            }
            Act::Download(rel) => {
                self.cancel = Arc::new(AtomicBool::new(false));
                let (tx, cancel) = (self.tx.clone(), self.cancel.clone());
                std::thread::spawn(move || download(rel, tx, cancel));
            }
            Act::Cancel => self.cancel.store(true, Ordering::SeqCst),
            Act::Skip(v) => {
                let r = crate::config::save_skip_version(&v).map_err(|e| format!("{}: {e}", crate::config::config_path().display()));
                self.queue.push_back(In::Skipped(r));
            }
            Act::QuitDaemon => quit_daemon(),
        }
    }

    /// Draw a frame from the current input; repeat while it produced
    /// events (a click changes the screen) or the kit asks for it.
    fn render(&mut self) {
        let k = wind::scale(self.hwnd);
        for _ in 0..4 {
            if self.closing || self.canvas.width() == 0 {
                return;
            }
            let (w, h) = self.canvas.dimensions();
            let bg = self.th.surface.with_alpha(255);
            for p in self.canvas.as_raw_mut().as_chunks_mut::<4>().0 {
                *p = [bg.r, bg.g, bg.b, 255];
            }
            let area = FRect { x: 0.0, y: 0.0, w: w as f32 / k, h: h as f32 / k };
            let mut clip = crate::ui::SystemClipboard;
            let fb = crate::uifb::Fb::new(self.canvas.as_raw_mut(), w as usize);
            let mut ui = Ui::new(fb, &self.th, &self.input, &mut self.focus, &mut clip, k, area);
            let clicked = paint(&mut ui, &self.m, &mut self.view);
            let out = ui.finish();
            self.input.end_frame();
            if let Some(b) = clicked {
                self.queue.push_back(In::Click(b));
            } else if out.escape {
                self.queue.push_back(In::Dismiss);
            } else if out.enter {
                self.queue.push_back(In::Enter);
            }
            if !self.process() && !out.redraw {
                return;
            }
        }
    }

    /// End of a callback: close if the machine said so.
    fn settle(&mut self) {
        if self.closing {
            wind::close(self.hwnd);
        }
    }
}

/// Worker: pick the asset for this install, download and verify it, then
/// install. No asset for this kind of install: open the release page.
fn download(rel: Release, tx: Sender<In>, cancel: Arc<AtomicBool>) {
    let kind = update::detect_install();
    let Some(asset) = update::pick_asset(&kind, &rel).cloned() else {
        update::open_url(&rel.url);
        let _ = tx.send(In::Fetched(Ok(())));
        let _ = tx.send(In::Installed(Ok(Applied::OpenedPage)));
        return;
    };
    let dir = match update_install::ensure_update_dir() {
        Ok(d) => d,
        Err(e) => {
            let _ = tx.send(In::Fetched(Err(e)));
            return;
        }
    };
    let last = std::cell::Cell::new(0u64);
    let progress = |got: u64, total: Option<u64>| {
        // About every 64 KB, and the final byte.
        if got < last.get() || got - last.get() >= 64 * 1024 || total == Some(got) {
            last.set(got);
            let _ = tx.send(In::Progress(got, total));
        }
        !cancel.load(Ordering::SeqCst)
    };
    match update::fetch_verified(&rel, &asset, &dir, &progress) {
        Err(e) => {
            let _ = tx.send(In::Fetched(Err(e)));
        }
        Ok((file, _)) if cancel.load(Ordering::SeqCst) => {
            let _ = std::fs::remove_file(&file);
            let _ = tx.send(In::Fetched(Err(update::CANCELLED.into())));
        }
        Ok((file, sha)) => {
            let _ = tx.send(In::Fetched(Ok(())));
            let r = update_install::apply(&kind, &file, sha);
            if r.is_err() {
                let _ = std::fs::remove_file(&file);
            }
            let _ = tx.send(In::Installed(r));
        }
    }
}

impl Driver for Dialog {
    fn on_create(&mut self, hwnd: Hwnd) {
        self.hwnd = hwnd;
    }

    fn on_event(&mut self, ev: Ev) -> bool {
        match ev {
            Ev::Resize(w, h) => {
                self.canvas = PixBuf::new(w.max(1), h.max(1));
                self.render();
            }
            Ev::Close => {
                self.queue.push_back(In::Dismiss);
                self.process();
                self.render();
            }
            Ev::Timer => {
                self.queue.extend(self.rx.try_iter());
                let changed = self.process();
                if changed {
                    self.render();
                }
                self.settle();
                return changed && !self.closing;
            }
            Ev::Move { .. } => {
                self.input.feed(&ev);
                self.render();
                if !self.closing {
                    wind::invalidate(self.hwnd); // hover changes
                }
            }
            _ => {
                self.input.feed(&ev);
                self.render();
            }
        }
        self.settle();
        !self.closing
    }

    fn frame(&mut self) -> Option<&mut PixBuf> {
        if self.canvas.width() == 0 || self.closing {
            return None;
        }
        if self.out.dimensions() != self.canvas.dimensions() {
            self.out = PixBuf::new(self.canvas.width(), self.canvas.height());
        }
        self.out.as_raw_mut().copy_from_slice(self.canvas.as_raw());
        Some(&mut self.out)
    }

    fn cursor(&self) -> Cursor {
        Cursor::Arrow
    }

    fn on_quit(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::preview;
    use crate::update::Asset;

    fn rel(v: &str) -> Release {
        Release {
            version: v.into(),
            url: format!("https://github.com/nappsllc/rustshot/releases/tag/v{v}"),
            notes: "## Highlights\n\n- Save straight into a dated folder (`%F`) with **Ctrl+S**\n- Shortcuts are configurable; conflicts are reported in Settings\n- In-app updates: download, verify the SHA-256 and restart\n\n## Fixes\n\n- The tray icon follows the taskbar theme\n- Text annotations use the system font renderer for every script\n- Faster overlay on multi-monitor setups\n- Smaller binary\n".into(),
            assets: vec![Asset { name: format!("rustshot-{v}-setup.exe"), url: "u".into(), size: 3_400_000 }],
        }
    }

    fn avail() -> Machine {
        Machine::new(DialogState::Available(rel("9.9.9")))
    }

    #[test]
    fn update_downloads_verifies_installs_and_restarts() {
        let mut m = avail();
        assert_eq!(m.on(In::Click(Btn::Update)), vec![Act::Download(rel("9.9.9"))]);
        assert!(matches!(m.screen, Screen::Downloading { got: 0, total: None, .. }));
        assert_eq!(m.on(In::Progress(1000, Some(4000))), vec![]);
        assert!(matches!(m.screen, Screen::Downloading { got: 1000, total: Some(4000), .. }));
        m.on(In::Progress(4000, Some(4000)));
        assert!(matches!(m.screen, Screen::Verifying(_)));
        // Busy: neither a new state nor Esc/close interrupts the install.
        assert_eq!(m.on(In::Fetched(Ok(()))), vec![]);
        assert!(matches!(m.screen, Screen::Installing(_)));
        assert_eq!(m.on(In::Dismiss), vec![]);
        assert_eq!(m.on(In::Click(Btn::Cancel)), vec![]);
        m.on(In::Show(DialogState::UpToDate));
        assert!(matches!(m.screen, Screen::Installing(_)));
        assert_eq!(m.on(In::Installed(Ok(Applied::RestartingNow))), vec![Act::QuitDaemon, Act::Close]);
    }

    #[test]
    fn enter_is_the_primary_button() {
        let mut m = avail();
        assert_eq!(m.on(In::Enter), vec![Act::Download(rel("9.9.9"))]);
        let mut m = Machine::new(DialogState::UpToDate);
        assert_eq!(m.on(In::Enter), vec![Act::Close]);
        // No primary while downloading: Enter does nothing.
        let mut m = avail();
        m.on(In::Enter);
        assert_eq!(m.on(In::Enter), vec![]);
    }

    #[test]
    fn cancel_mid_download_aborts_then_closes() {
        let mut m = avail();
        m.on(In::Click(Btn::Update));
        m.on(In::Progress(500, Some(4000)));
        assert_eq!(m.on(In::Click(Btn::Cancel)), vec![Act::Cancel]);
        assert!(matches!(m.screen, Screen::Cancelling(_)));
        // Stale progress and a second cancel change nothing.
        assert_eq!(m.on(In::Progress(600, Some(4000))), vec![]);
        assert_eq!(m.on(In::Click(Btn::Cancel)), vec![]);
        assert_eq!(m.on(In::Dismiss), vec![]);
        // The worker stops (deleting the partial file) and reports it.
        assert_eq!(m.on(In::Fetched(Err(update::CANCELLED.into()))), vec![Act::Close]);
        // The close button during a download cancels too.
        let mut m = avail();
        m.on(In::Click(Btn::Update));
        assert_eq!(m.on(In::Dismiss), vec![Act::Cancel]);
    }

    #[test]
    fn cancel_too_late_waits_for_the_install() {
        let mut m = avail();
        m.on(In::Click(Btn::Update));
        m.on(In::Progress(4000, Some(4000)));
        assert_eq!(m.on(In::Click(Btn::Cancel)), vec![Act::Cancel]);
        // Verified before the worker saw the flag: the install goes ahead.
        m.on(In::Fetched(Ok(())));
        assert!(matches!(m.screen, Screen::Installing(_)));
        assert_eq!(m.on(In::Installed(Ok(Applied::RestartingNow))), vec![Act::QuitDaemon, Act::Close]);
    }

    #[test]
    fn skip_writes_the_version_and_closes() {
        let mut m = avail();
        assert_eq!(m.on(In::Click(Btn::Skip)), vec![Act::Skip("9.9.9".into())]);
        assert_eq!(m.on(In::Skipped(Ok(()))), vec![Act::Close]);
        let mut m = avail();
        m.on(In::Click(Btn::Skip));
        assert_eq!(m.on(In::Skipped(Err("denied".into()))), vec![]);
        assert_eq!(m.screen, Screen::SaveFailed("denied".into()));
        assert_eq!(m.on(In::Click(Btn::Close)), vec![Act::Close]);
    }

    #[test]
    fn skip_action_writes_skip_version() {
        // What the driver does for Act::Skip, on a scratch config text.
        let text = crate::config::to_toml(&crate::config::Config::default());
        let out = crate::config::with_skip_version(&text, "9.9.9");
        assert_eq!(crate::config::parse_config(&out).unwrap().skip_version, "9.9.9");
    }

    #[test]
    fn retry_after_errors() {
        let mut m = Machine::new(DialogState::Error("offline".into()));
        assert_eq!(m.on(In::Click(Btn::Retry)), vec![Act::Check]);
        assert_eq!(m.screen, Screen::Checking);
        m.on(In::Checked(Err("still offline".into())));
        assert_eq!(m.screen, Screen::CheckFailed("still offline".into()));
        m.on(In::Enter);
        m.on(In::Checked(Ok(Some(rel("9.9.9")))));
        assert_eq!(m.screen, Screen::Available(rel("9.9.9")));
        // A failed download retries the download.
        m.on(In::Click(Btn::Update));
        m.on(In::Fetched(Err("SHA-256 mismatch".into())));
        assert!(matches!(&m.screen, Screen::InstallFailed { msg, .. } if msg == "SHA-256 mismatch"));
        assert_eq!(m.on(In::Click(Btn::Retry)), vec![Act::Download(rel("9.9.9"))]);
        m.on(In::Fetched(Ok(())));
        m.on(In::Installed(Err("installer failed".into())));
        assert!(matches!(&m.screen, Screen::InstallFailed { msg, .. } if msg == "installer failed"));
        assert_eq!(m.on(In::Click(Btn::Close)), vec![Act::Close]);
        // Checking can be abandoned.
        let mut m = Machine::new(DialogState::Error("x".into()));
        m.on(In::Click(Btn::Retry));
        assert_eq!(m.on(In::Click(Btn::Cancel)), vec![Act::Close]);
        m.on(In::Checked(Ok(None)));
        let mut m = Machine::new(DialogState::Error("x".into()));
        m.on(In::Click(Btn::Retry));
        m.on(In::Checked(Ok(None)));
        assert_eq!(m.screen, Screen::UpToDate);
    }

    #[test]
    fn simple_states_close() {
        for s in [DialogState::UpToDate, DialogState::Managed("Microsoft Store")] {
            assert_eq!(Machine::new(s.clone()).on(In::Click(Btn::Ok)), vec![Act::Close]);
            assert_eq!(Machine::new(s.clone()).on(In::Dismiss), vec![Act::Close]);
            assert_eq!(Machine::new(s).on(In::Click(Btn::Update)), vec![], "not on screen");
        }
        assert_eq!(avail().on(In::Click(Btn::Cancel)), vec![Act::Close]);
        assert_eq!(avail().on(In::Dismiss), vec![Act::Close]);
        // Opened page instead of installing (no asset for this install).
        let mut m = avail();
        m.on(In::Click(Btn::Update));
        m.on(In::Fetched(Ok(())));
        assert_eq!(m.on(In::Installed(Ok(Applied::OpenedPage))), vec![Act::Close]);
    }

    #[test]
    fn show_replaces_content_unless_busy() {
        let mut m = Machine::new(DialogState::UpToDate);
        m.on(In::Show(DialogState::Available(rel("9.9.9"))));
        assert_eq!(m.screen, Screen::Available(rel("9.9.9")));
        m.on(In::Click(Btn::Update));
        m.on(In::Show(DialogState::Error("x".into())));
        assert!(matches!(m.screen, Screen::Downloading { .. }));
    }

    #[test]
    fn check_results_map_to_states() {
        assert_eq!(DialogState::from_check(Ok(None)), DialogState::UpToDate);
        assert_eq!(DialogState::from_check(Err("e".into())), DialogState::Error("e".into()));
        assert_eq!(DialogState::from_check(Ok(Some(rel("1.0.0")))), DialogState::Available(rel("1.0.0")));
    }

    // --- Rendering ---

    /// Render `m` two frames (focus order, then the picture); returns the
    /// image and the button clicked by `input` on the second frame.
    fn render(m: &Machine, th: &Theme, k: f32, h: u32, input: &Input) -> (PixBuf, Option<Btn>) {
        let mut focus = FocusState::default();
        let mut v = View::default();
        preview::render(W, h, k, th, &Input::default(), &mut focus, |ui| {
            paint(ui, m, &mut v);
        });
        let mut clicked = None;
        let img = preview::render(W, h, k, th, input, &mut focus, |ui| clicked = paint(ui, m, &mut v));
        (img, clicked)
    }

    fn screens() -> Vec<(&'static str, Machine, u32)> {
        let r = rel("0.1.2");
        let at = |s: Screen| Machine { screen: s };
        vec![
            ("available", at(Screen::Available(r.clone())), H_FULL),
            ("downloading", at(Screen::Downloading { rel: r.clone(), got: 1_234_567, total: Some(3_400_000) }), H_FULL),
            ("verifying", at(Screen::Verifying(r.clone())), H_FULL),
            ("cancelling", at(Screen::Cancelling(r.clone())), H_FULL),
            ("installing", at(Screen::Installing(r.clone())), H_FULL),
            (
                "install-failed",
                at(Screen::InstallFailed {
                    rel: r.clone(),
                    msg: "rustshot-0.1.2-setup.exe failed its SHA-256 check; refusing to install".into(),
                }),
                H_FULL,
            ),
            ("uptodate", at(Screen::UpToDate), H_SHORT),
            ("managed", at(Screen::Managed("Microsoft Store")), H_SHORT),
            (
                "error",
                at(Screen::CheckFailed(
                    "WinHttpSendRequest: The server name or address could not be resolved (0x80072EE7)".into(),
                )),
                H_SHORT,
            ),
            ("checking", at(Screen::Checking), H_SHORT),
            ("save-failed", at(Screen::SaveFailed(r"C:\Users\denis\AppData\Roaming\rustshot\config.toml: Access is denied. (os error 5)".into())), H_SHORT),
        ]
    }

    #[test]
    fn preview_dialog_pngs() {
        for (tname, th) in [("dark", &crate::theme::DARK), ("light", &crate::theme::LIGHT)] {
            for (name, m, h) in screens() {
                let (img, _) = render(&m, th, 1.0, h, &Input::default());
                // Something besides the background was drawn.
                let bg = th.surface;
                assert!(img.as_raw().as_chunks::<4>().0.iter().any(|p| p[..3] != [bg.r, bg.g, bg.b]), "{name}");
                preview::save(&format!("update-{name}-{tname}.png"), &img);
            }
        }
        let (name, m, h) = screens().remove(0);
        let (img, _) = render(&m, &crate::theme::DARK, 1.5, h, &Input::default());
        preview::save(&format!("update-{name}-dark-150.png"), &img);
    }

    /// Clicking where each button is drawn reports it (layout and hit
    /// rects agree), at 100 % and 150 %.
    #[test]
    fn buttons_click_where_drawn() {
        for k in [1.0f32, 1.5] {
            let m = avail();
            // Right-aligned row: Cancel is rightmost, ending at W - PAD.
            let y = ((H_FULL as f32 - PAD - 16.0) * k) as i32;
            let x = ((W as f32 - PAD - 20.0) * k) as i32;
            let mut input = Input::default();
            for e in [Ev::Move { x, y }, Ev::Down { x, y }, Ev::Up { x, y }] {
                input.feed(&e);
            }
            let (_, clicked) = render(&m, &crate::theme::DARK, k, H_FULL, &input);
            assert_eq!(clicked, Some(Btn::Cancel), "k={k}");
        }
    }

    #[test]
    fn notes_are_the_excerpt() {
        let mut v = View::default();
        let r = rel("9.9.9");
        let n = v.notes(&r).to_string();
        assert!(n.starts_with("Highlights\n\n- Save straight"));
        assert!(n.lines().count() <= NOTES_LINES);
        v.scroll = 30.0;
        v.notes(&r);
        assert_eq!(v.scroll, 30.0, "same release keeps the scroll");
        v.notes(&rel("9.9.10"));
        assert_eq!(v.scroll, 0.0);
    }
}
