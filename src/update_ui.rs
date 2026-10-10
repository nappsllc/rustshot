//! The update dialog: "Rustshot X is available (you have Y)" with the
//! release notes and Update / Skip this version / Cancel, a progress bar
//! while downloading, then install and restart; also "up to date", check
//! errors (Retry / Close) and store-managed installs. Opened by the
//! daemon's daily check ([`offer`]) and by the tray's "Check for updates"
//! ([`check`], [`show`]).
//!
//! The dialog is a [`Machine`] (pure: events in, actions out, unit-tested)
//! drawn with the `ui` kit in a `wind::run_window` window on its own
//! thread. Network and disk work runs on worker threads that report back
//! through a channel the window drains on its timer. One dialog at a time:
//! a request while it is open replaces its content (unless a download or
//! install is running) and brings it to the front.
//!
//! A dialog the user did not ask for (the daily check) has no default
//! button: Enter does nothing there, so typing into another window when it
//! pops up never starts an update. After any content change Enter is also
//! ignored for [`ENTER_GRACE`].
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
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// This build's version.
const CURRENT: &str = env!("CARGO_PKG_VERSION");
/// Release-notes lines shown (the box scrolls when they don't fit).
const NOTES_LINES: usize = 12;
/// Window size, logical px (every state: the window never resizes).
const W: u32 = 440;
const H: u32 = 300;
const PAD: f32 = 20.0;
const ICON: f32 = 24.0;
/// Title row height.
const TITLE_H: f32 = 32.0;
/// Indent of text under the title (past the icon).
const TEXT_X: f32 = ICON + 4.0 + crate::ui::layout::GAP;
/// Enter is ignored this long after the window opens or its content changes.
pub const ENTER_GRACE: Duration = Duration::from_millis(500);

/// What the dialog shows when opened.
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

/// A request to open (or re-target) the dialog.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    /// The user asked for this (tray).
    Show(DialogState),
    /// Unasked (daily check): no default button.
    Offer(DialogState),
    /// The user asked to check now: "Checking…", then the result.
    Check,
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
    /// Waiting for `check_now` (tray check, Retry).
    Checking,
    Available(Release),
    UpToDate,
    Managed(&'static str),
    CheckFailed(String),
    /// Downloading the asset (bytes so far, total when known).
    Downloading { rel: Release, got: u64, total: Option<u64> },
    /// Download complete; its SHA-256 is being checked.
    Verifying(Release),
    /// Cancel pressed; the download stops and cleans up (Close hides the
    /// window meanwhile).
    Cancelling(Release),
    /// Committed to installing; nothing can be cancelled any more.
    /// `waiting`: the daemon finishes the user's open capture (or upload)
    /// first; then the installer / swap runs.
    Installing { rel: Release, waiting: bool },
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
    /// Enter not taken by a focused control: the default button, if any.
    Enter,
    Checked(Result<Option<Release>, String>),
    Progress(u64, Option<u64>),
    /// The download finished, passed its SHA-256 check and the install
    /// starts (Err: it failed or was cancelled; the files are gone either way).
    Fetched(Result<(), String>),
    /// The install waits for the daemon to go idle (true), or runs (false).
    Waiting(bool),
    Installed(Result<Applied, String>),
    Skipped(Result<(), String>),
    /// A request while the dialog is open.
    Open(Request),
}

/// What the [`Machine`] asks its driver to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Act {
    Close,
    /// Run `update::check_now` on a worker (→ `In::Checked`).
    Check,
    /// Download, verify and install on a worker (→ `Progress`, `Fetched`,
    /// `Waiting`, `Installed`; the worker has the daemon go idle before it
    /// installs, and ends it when the new version is starting).
    Download(Release),
    /// Abort the running download.
    Cancel,
    /// Write `skip_version` (→ `In::Skipped`).
    Skip(String),
}

/// The dialog's state machine.
#[derive(Clone, Debug, PartialEq)]
pub struct Machine {
    screen: Screen,
    /// The content was not asked for (daily check): Enter does nothing.
    unsolicited: bool,
}

impl Machine {
    /// A dialog the user opened with `s`.
    pub fn new(s: DialogState) -> Machine {
        Machine { screen: s.into(), unsolicited: false }
    }

    /// The dialog a request opens, and what to do first.
    pub fn start(req: Request) -> (Machine, Vec<Act>) {
        match req {
            Request::Show(s) => (Machine::new(s), vec![]),
            Request::Offer(s) => (Machine { screen: s.into(), unsolicited: true }, vec![]),
            Request::Check => (Machine { screen: Screen::Checking, unsolicited: false }, vec![Act::Check]),
        }
    }

    /// A download or install is running (new content is not taken).
    fn busy(&self) -> bool {
        matches!(
            self.screen,
            Screen::Downloading { .. } | Screen::Verifying(_) | Screen::Cancelling(_) | Screen::Installing { .. }
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
            Screen::Cancelling(_) => (&[Close], None),
            Screen::Installing { .. } => (&[Cancel], None),
        }
    }

    /// The buttons that react (the others are drawn disabled).
    fn enabled(&self) -> bool {
        !matches!(self.screen, Screen::Installing { .. })
    }

    /// The button Enter presses: none in an unsolicited dialog.
    fn default_button(&self) -> Option<Btn> {
        if self.unsolicited { None } else { self.buttons().1 }
    }

    /// Advance on `ev`; returns what the driver must do, in order.
    pub fn on(&mut self, ev: In) -> Vec<Act> {
        use Screen as S;
        if let In::Open(req) = ev {
            if self.busy() {
                return Vec::new();
            }
            return match req {
                Request::Check if self.screen == S::Checking => Vec::new(),
                req => {
                    let acts;
                    (*self, acts) = Machine::start(req);
                    acts
                }
            };
        }
        // The user clicked (or activated a button from the keyboard): the
        // dialog is theirs now, Enter works again.
        if matches!(ev, In::Click(_)) {
            self.unsolicited = false;
        }
        // The button the event stands for.
        let btn = match &ev {
            In::Click(b) => Some(*b),
            In::Enter => self.default_button(),
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
                Ok(()) => (S::Installing { rel, waiting: false }, vec![]),
                Err(msg) => (S::InstallFailed { rel, msg }, vec![]),
            },
            (S::Downloading { rel, .. } | S::Verifying(rel), In::Dismiss, _)
            | (S::Downloading { rel, .. } | S::Verifying(rel), _, Some(Btn::Cancel)) => {
                (S::Cancelling(rel), vec![Act::Cancel])
            }
            // Committed to installing before the cancel reached the worker.
            (S::Cancelling(rel), In::Fetched(Ok(())), _) => (S::Installing { rel, waiting: false }, vec![]),
            (s @ S::Cancelling(_), In::Fetched(Err(_)), _) => (s, vec![Act::Close]),
            // Don't wait for a stalled download: the worker cleans up alone.
            (s @ S::Cancelling(_), In::Dismiss, _) | (s @ S::Cancelling(_), _, Some(Btn::Close)) => {
                (s, vec![Act::Close])
            }
            (S::Installing { rel, .. }, In::Waiting(waiting), _) => (S::Installing { rel, waiting }, vec![]),
            (s @ S::Installing { .. }, In::Installed(Ok(_)), _) => (s, vec![Act::Close]),
            (S::Installing { rel, .. }, In::Installed(Err(msg)), _) => (S::InstallFailed { rel, msg }, vec![]),
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
    /// `notes_lines` of `notes_of`'s body: (text, heading).
    notes: Vec<(String, bool)>,
    notes_of: Option<String>,
}

impl View {
    fn notes(&mut self, rel: &Release) -> &[(String, bool)] {
        if self.notes_of.as_deref() != Some(&rel.version) {
            self.notes = update::notes_lines(&rel.notes, NOTES_LINES);
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

/// What a screen shows: heading, release notes, message, progress line.
struct Content<'a> {
    mark: Mark,
    title: String,
    notes: Option<&'a Release>,
    text: Option<String>,
    /// Label and bar fraction while a download / install runs.
    progress: Option<(String, f32)>,
}

fn content<'a>(m: &'a Machine, th: &Theme) -> Content<'a> {
    let available = |r: &Release| format!("Rustshot {} is available (you have {CURRENT})", r.version);
    let msg = |mark, title: String, text: &str| Content {
        mark,
        title,
        notes: None,
        text: Some(text.to_string()),
        progress: None,
    };
    let busy = |rel, label: String, f: f32| Content {
        mark: Mark::App,
        title: available(rel),
        notes: Some(rel),
        text: None,
        progress: Some((label, f)),
    };
    match &m.screen {
        Screen::Checking => Content {
            mark: Mark::App,
            title: "Checking for updates…".into(),
            notes: None,
            text: Some(format!("You have {CURRENT}.")),
            progress: None,
        },
        Screen::Available(r) => Content { mark: Mark::App, title: available(r), notes: Some(r), text: None, progress: None },
        Screen::Downloading { rel, got, total } => {
            let s = match total {
                Some(t) => format!("Downloading… {} of {}", mb(*got), mb(*t)),
                None if *got > 0 => format!("Downloading… {}", mb(*got)),
                None => "Downloading…".into(),
            };
            busy(rel, s, total.map_or(0.0, |t| *got as f32 / t.max(1) as f32))
        }
        Screen::Verifying(rel) => busy(rel, "Verifying the download…".into(), 1.0),
        Screen::Cancelling(rel) => busy(rel, "Cancelling…".into(), 0.0),
        Screen::Installing { rel, waiting: true } => busy(rel, "Waiting for the capture to close…".into(), 1.0),
        Screen::Installing { rel, waiting: false } => busy(rel, "Installing… Rustshot will restart.".into(), 1.0),
        Screen::UpToDate => msg(
            Mark::Icon("okc", th.success),
            "Rustshot is up to date".into(),
            &format!("You have the latest version, {CURRENT}."),
        ),
        Screen::Managed(store) => msg(
            Mark::Icon("info", th.accent_fg),
            format!("Rustshot {CURRENT}"),
            &format!("Updates for this install come from {store}."),
        ),
        Screen::CheckFailed(e) => msg(Mark::Icon("alert", th.error), "Could not check for updates".into(), e),
        Screen::InstallFailed { rel, msg: e } => Content {
            mark: Mark::Icon("alert", th.error),
            title: format!("Rustshot {} could not be installed", rel.version),
            notes: Some(rel),
            text: Some(e.clone()),
            progress: None,
        },
        Screen::SaveFailed(e) => msg(Mark::Icon("alert", th.error), "Could not save the setting".into(), e),
    }
}

/// Where each part of a screen goes (logical px).
#[derive(Clone, Copy, Debug)]
struct Lay {
    title: FRect,
    text: Option<FRect>,
    notes: Option<FRect>,
    progress: Option<FRect>,
    buttons: FRect,
}

/// Lay out `c` in the window: title on top, buttons at the bottom; release
/// notes fill the space between (a failure message sits above them, the
/// progress line below them). Screens without notes centre their title
/// and message vertically above the buttons.
fn layout(ui: &Ui, c: &Content) -> Lay {
    let b = ui.bounds();
    let inner = b.w - 2.0 * PAD;
    let btn_y = b.h - PAD - crate::ui::layout::H;
    let buttons = FRect { x: PAD, y: btn_y, w: inner, h: crate::ui::layout::H };
    let bottom = btn_y - 16.0;
    let text_w = inner - TEXT_X;
    let text_h = |t: &str, max: f32| ui.paragraph_height(t, text_w).min(max.max(0.0));
    let progress = c.progress.as_ref().map(|_| FRect { x: PAD, y: bottom - 32.0, w: inner, h: 32.0 });
    let body_bottom = progress.map_or(bottom, |p| p.y - 12.0);
    let mut lay = Lay { title: buttons, text: None, notes: None, progress, buttons };
    if c.notes.is_some() {
        lay.title = FRect { x: PAD, y: PAD, w: inner, h: TITLE_H };
        let mut top = PAD + TITLE_H + 12.0;
        if let Some(t) = &c.text {
            // A failure: up to three lines of message, then the notes.
            let r = FRect { x: PAD + TEXT_X, y: PAD + TITLE_H + 2.0, w: text_w, h: text_h(t, 60.0) };
            top = r.y1() + 12.0;
            lay.text = Some(r);
        }
        // Whole lines only: no sliver of a cut-off last row.
        let h = crate::ui::controls::text_view_fit(body_bottom - top, 2);
        lay.notes = Some(FRect { x: PAD, y: top, w: inner, h });
    } else {
        let room = body_bottom - PAD;
        let th = c.text.as_ref().map_or(0.0, |t| text_h(t, room - TITLE_H - 2.0));
        let block = TITLE_H + if c.text.is_some() { 2.0 + th } else { 0.0 };
        let y = PAD + ((room - block) / 2.0).max(0.0).floor();
        lay.title = FRect { x: PAD, y, w: inner, h: TITLE_H };
        lay.text = c.text.as_ref().map(|_| FRect { x: PAD + TEXT_X, y: y + TITLE_H + 2.0, w: text_w, h: th });
    }
    lay
}

/// One frame of the dialog; returns the clicked button.
fn paint(ui: &mut Ui, m: &Machine, v: &mut View) -> Option<Btn> {
    let th = ui.theme;
    let c = content(m, th);
    let lay = layout(ui, &c);
    // Title line.
    ui.area(lay.title, |ui| {
        ui.row(|ui| {
            match c.mark {
                Mark::App => {
                    let r = ui.alloc(Some(ICON), crate::ui::layout::H);
                    let s = ui.px(ICON);
                    let col = th.accent;
                    crate::tray::draw_glyph(&mut ui.fb, r.x, (r.y + (r.h - s) / 2.0).round(), s, col);
                }
                Mark::Icon(name, col) => ui.icon(name, ICON, col),
            }
            ui.space(4.0);
            ui.heading(&c.title);
        })
    });
    if let (Some(t), Some(r)) = (&c.text, lay.text) {
        ui.area(r, |ui| ui.height(r.h).paragraph(t, true));
    }
    if let (Some(rel), Some(r)) = (c.notes, lay.notes) {
        v.notes(rel); // refresh for this release (resets the scroll)
        let mut scroll = v.scroll;
        let notes = &v.notes;
        let src: Vec<(&str, bool)> = if notes.is_empty() {
            vec![("No release notes.", true)]
        } else {
            // Headings in the primary text colour, the rest secondary.
            notes.iter().map(|(l, heading)| (l.as_str(), !heading)).collect()
        };
        ui.place(r).text_view_styled("notes", &src, &mut scroll);
        v.scroll = scroll;
    }
    if let (Some((s, f)), Some(r)) = (&c.progress, lay.progress) {
        ui.area(r, |ui| {
            ui.lay.gap = 6.0;
            ui.note(s);
            ui.progress(*f);
        });
    }
    // Buttons, right-aligned.
    let (btns, primary) = m.buttons();
    let gap = crate::ui::layout::GAP;
    let total: f32 = btns.iter().map(|b| ui.button_width(b.label())).sum::<f32>() + gap * (btns.len() as f32 - 1.0);
    let mut clicked = None;
    ui.area(lay.buttons, |ui| {
        ui.row(|ui| {
            ui.space(lay.buttons.w - total);
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

/// The open dialog's inbox, if any.
static OPEN: Mutex<Option<Sender<In>>> = Mutex::new(None);
/// The daemon's event sender: the download worker asks it to go idle
/// (`RestartWhenIdle`) before installing, then sends `Restart` (the new
/// version is starting) or `RestartAborted` (the install failed).
static QUIT: Mutex<Option<Sender<HotEvent>>> = Mutex::new(None);
/// Held by the download worker for its whole run: a new download waits
/// until a cancelled one has cleaned up its files.
static WORKER: Mutex<()> = Mutex::new(());

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Register the daemon's event sender (an update installs and restarts
/// through it).
pub fn set_quit_sender(tx: Sender<HotEvent>) {
    *lock(&QUIT) = Some(tx);
}

/// How long the worker waits for the daemon's idle reply before the dialog
/// says it is waiting for the capture.
const IDLE_QUICK: Duration = Duration::from_millis(250);

/// Ask the daemon to finish what the user has open (capture, editor,
/// export, upload) and start nothing new, and wait until it has. The
/// dialog shows the wait when it is not immediate. False: the daemon is
/// gone (or quit meanwhile), so nothing may be installed.
fn wait_idle(daemon: Option<&Sender<HotEvent>>, tx: &Sender<In>) -> bool {
    let Some(d) = daemon else { return false };
    let (reply, idle) = std::sync::mpsc::channel();
    if d.send(HotEvent::RestartWhenIdle(reply)).is_err() {
        return false;
    }
    match idle.recv_timeout(IDLE_QUICK) {
        Ok(()) => return true,
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return false,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
    }
    let _ = tx.send(In::Waiting(true));
    let ok = idle.recv().is_ok();
    if ok {
        let _ = tx.send(In::Waiting(false));
    }
    ok
}

/// Open the dialog with `state` (the user asked), or hand it to the open one.
pub fn show(state: DialogState) {
    request(Request::Show(state));
}

/// Open the dialog with `state` unasked (the daily check): no default button.
pub fn offer(state: DialogState) {
    request(Request::Offer(state));
}

/// Tray "Check for updates": the dialog opens at once ("Checking…") and
/// shows the result; while a check runs, another one just raises it.
pub fn check() {
    request(Request::Check);
}

fn request(req: Request) {
    #[cfg(target_os = "macos")]
    {
        let out = |state| match state {
            DialogState::Available(r) => update::open_url(&r.url),
            DialogState::UpToDate => eprintln!("Rustshot is up to date"),
            DialogState::Error(e) => eprintln!("update check failed: {e}"),
            DialogState::Managed(s) => eprintln!("Updates for this install come from {s}."),
        };
        match req {
            // Unasked (the daily check): never open a browser tab.
            Request::Offer(DialogState::Available(r)) => {
                eprintln!("Rustshot {} is available — run `rustshot update`", r.version)
            }
            Request::Show(s) | Request::Offer(s) => out(s),
            Request::Check => {
                std::thread::spawn(move || out(DialogState::from_check(update::check_now())));
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let mut open = lock(&OPEN);
        if let Some(tx) = open.as_ref()
            && tx.send(In::Open(req.clone())).is_ok()
        {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        *open = Some(tx.clone());
        drop(open);
        std::thread::spawn(move || run_dialog(req, tx, rx));
    }
}

fn run_dialog(req: Request, tx: Sender<In>, rx: Receiver<In>) {
    let th = crate::theme::resolve(&crate::config::load());
    let (m, acts) = Machine::start(req);
    let mut d = Dialog::new(m, th, tx, rx);
    for a in acts {
        d.perform(a);
    }
    let spec = WindowSpec { title: "Rustshot update".into(), w: W, h: H, resizable: false, min: (W, H) };
    if let Err(e) = wind::run_window(spec, &mut d) {
        eprintln!("rustshot: update dialog: {e:#}");
    }
    d.cancel();
    // A request that raced the close still gets its dialog.
    let mut open = lock(&OPEN);
    let late = d.rx.try_iter().filter_map(|e| if let In::Open(r) = e { Some(r) } else { None }).last();
    *open = None;
    drop(open);
    drop(d);
    if let Some(r) = late {
        request(r);
    }
}

/// Download worker phases (shared with the dialog).
const RUNNING: u8 = 0;
const CANCELLED: u8 = 1;
/// Verified and committed to installing: a cancel comes too late.
const COMMITTED: u8 = 2;

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
    /// Phase of the running download (`RUNNING`, `CANCELLED`, `COMMITTED`).
    phase: Arc<AtomicU8>,
    /// `Act::Close` seen: the window closes at the end of the callback.
    closing: bool,
    /// The file "Skip this version" writes.
    config: PathBuf,
    /// When the window opened or its content last changed (Enter grace).
    changed_at: Instant,
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
            phase: Arc::new(AtomicU8::new(CANCELLED)),
            closing: false,
            config: crate::config::config_path(),
            changed_at: Instant::now(),
        }
    }

    /// Stop the running download unless it already committed to installing.
    fn cancel(&self) {
        let _ = self.phase.compare_exchange(RUNNING, CANCELLED, Ordering::SeqCst, Ordering::SeqCst);
    }

    /// Enter may press the default button (not right after a change).
    fn enter_ready(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.changed_at) >= ENTER_GRACE
    }

    /// Feed queued events to the machine and run its actions.
    fn process(&mut self) -> bool {
        let mut any = false;
        while let Some(ev) = self.queue.pop_front() {
            any = true;
            let open = matches!(ev, In::Open(_));
            if open && !cfg!(test) {
                #[cfg(windows)]
                wind::raise(self.hwnd);
            }
            let before = (std::mem::discriminant(&self.m.screen), self.m.busy());
            let acts = self.m.on(ev);
            if (open && !before.1) || std::mem::discriminant(&self.m.screen) != before.0 {
                self.changed_at = Instant::now();
            }
            for a in acts {
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
                self.phase = Arc::new(AtomicU8::new(RUNNING));
                let (tx, phase) = (self.tx.clone(), self.phase.clone());
                std::thread::spawn(move || download(rel, tx, phase));
            }
            Act::Cancel => self.cancel(),
            Act::Skip(v) => {
                let r = crate::config::save_skip_version_at(&self.config, &v)
                    .map_err(|e| format!("{}: {e}", self.config.display()));
                self.queue.push_back(In::Skipped(r));
            }
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
            } else if out.enter && self.enter_ready(Instant::now()) {
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

/// `update_install::apply` for this install (verified file, its SHA-256).
type ApplyFn = Box<dyn Fn(&std::path::Path, [u8; 32]) -> Result<Applied, String> + Send>;

/// The daemon and the installer, as the worker sees them (stubbed in tests).
struct Installer {
    /// The daemon's event sender (`None`: no daemon, nothing installs).
    daemon: Option<Sender<HotEvent>>,
    apply: ApplyFn,
}

/// Worker: pick the asset for this install, download and verify it, then
/// install. No asset for this kind of install: open the release page. The
/// dialog may close meanwhile (a cancel it no longer waits for): then
/// nobody reads the reports, and the cleanup still happens here.
fn download(rel: Release, tx: Sender<In>, phase: Arc<AtomicU8>) {
    let _one = lock(&WORKER);
    let commit = || phase.compare_exchange(RUNNING, COMMITTED, Ordering::SeqCst, Ordering::SeqCst).is_ok();
    let kind = update::detect_install();
    let Some(asset) = update::pick_asset(&kind, &rel).cloned() else {
        if commit() {
            update::open_url(&rel.url);
            let _ = tx.send(In::Fetched(Ok(())));
            let _ = tx.send(In::Installed(Ok(Applied::OpenedPage)));
        } else {
            let _ = tx.send(In::Fetched(Err(update::CANCELLED.into())));
        }
        return;
    };
    let dir = match update_install::ensure_update_dir() {
        Ok(d) => d,
        Err(e) => {
            let _ = tx.send(In::Fetched(Err(format!("Download failed: {e}"))));
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
        phase.load(Ordering::SeqCst) != CANCELLED
    };
    let fetched = update::fetch_verified(&rel, &asset, &dir, &progress);
    let inst = Installer {
        daemon: lock(&QUIT).clone(),
        apply: Box::new(move |file, sha| update_install::apply(&kind, file, sha)),
    };
    install(fetched, &phase, &tx, &inst);
}

/// After the download: commit (unless cancelled), have the daemon go idle,
/// then install and let the daemon quit for the new version. Every way
/// out that does not install deletes the file.
fn install(fetched: Result<(PathBuf, [u8; 32]), String>, phase: &AtomicU8, tx: &Sender<In>, inst: &Installer) {
    let commit = || phase.compare_exchange(RUNNING, COMMITTED, Ordering::SeqCst, Ordering::SeqCst).is_ok();
    match fetched {
        Err(e) if e == update::CANCELLED => {
            let _ = tx.send(In::Fetched(Err(e)));
        }
        Err(e) => {
            let _ = tx.send(In::Fetched(Err(format!("Download failed: {e}"))));
        }
        Ok((file, _)) if !commit() => {
            let _ = std::fs::remove_file(&file);
            let _ = tx.send(In::Fetched(Err(update::CANCELLED.into())));
        }
        Ok((file, sha)) => {
            let _ = tx.send(In::Fetched(Ok(())));
            // The installer starts the new version, which gives up if we
            // have not exited within ~30 s: so no capture may be open from
            // here on, or the user could end up with no Rustshot running.
            let daemon = inst.daemon.as_ref();
            if !wait_idle(daemon, tx) {
                let _ = std::fs::remove_file(&file);
                let msg = "Install failed: Rustshot is not running in the background any more";
                let _ = tx.send(In::Installed(Err(msg.into())));
                return;
            }
            let r = (inst.apply)(&file, sha);
            if r.is_err() {
                let _ = std::fs::remove_file(&file);
            }
            if let Some(d) = daemon {
                // Even when the dialog is gone: the new version waits for us.
                // Nothing to do if the daemon is already gone.
                let ev = if r == Ok(Applied::RestartingNow) { HotEvent::Restart } else { HotEvent::RestartAborted };
                let _ = d.send(ev);
            }
            let _ = tx.send(In::Installed(r.map_err(|e| format!("Install failed: {e}"))));
        }
    }
}

impl Driver for Dialog {
    fn on_create(&mut self, hwnd: Hwnd) {
        self.hwnd = hwnd;
        self.changed_at = Instant::now();
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
                if matches!(ev, Ev::Down { .. }) {
                    // A click into the window: the user is using it.
                    self.m.unsolicited = false;
                }
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
        self.cancel();
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
            notes: "## Highlights\n\n- Save straight into a dated folder (`%F`) with **Ctrl+S**\n- Shortcuts are configurable; conflicts are reported in Settings\n- In-app updates: download, verify the SHA-256 and restart\n\n\n\n## Fixes\n\n- The tray icon follows the taskbar theme\n- Text annotations use the system font renderer for every script\n- Faster overlay on multi-monitor setups\n- Smaller binary\n".into(),
            assets: vec![Asset { name: format!("rustshot-{v}-setup.exe"), url: "u".into(), size: 3_400_000 }],
        }
    }

    fn avail() -> Machine {
        Machine::new(DialogState::Available(rel("9.9.9")))
    }

    /// Smoke (needs a display; CI runs it under `xvfb-run` on Linux): the
    /// real update dialog opens with release notes, draws and closes after
    /// about a second. `cargo test window_smoke -- --ignored --test-threads=1`
    #[cfg(not(target_os = "macos"))] // macOS windows need the main thread
    #[test]
    #[ignore = "opens a window"]
    fn update_dialog_window_smoke() {
        if !crate::wind::has_display() {
            eprintln!("no display; skipped");
            return;
        }
        let _guard = crate::wind::test_window_lock();
        let (tx, rx) = std::sync::mpsc::channel();
        let (m, acts) = Machine::start(Request::Show(DialogState::Available(rel("9.9.9"))));
        assert!(acts.is_empty());
        let mut dlg = Dialog::new(m, crate::theme::resolve(&crate::config::Config::default()), tx, rx);
        let mut d = crate::wind::AutoClose::new(&mut dlg, 1000);
        let spec = WindowSpec { title: "Rustshot update".into(), w: W, h: H, resizable: false, min: (W, H) };
        wind::run_window(spec, &mut d).expect("update dialog");
        assert!(d.frames > 0, "drew a frame");
        dlg.cancel();
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
        assert!(matches!(m.screen, Screen::Installing { waiting: false, .. }));
        assert_eq!(m.on(In::Dismiss), vec![]);
        assert_eq!(m.on(In::Click(Btn::Cancel)), vec![]);
        m.on(In::Open(Request::Show(DialogState::UpToDate)));
        m.on(In::Open(Request::Check));
        assert!(matches!(m.screen, Screen::Installing { .. }));
        // The daemon finishes the user's capture first; still not cancellable.
        m.on(In::Waiting(true));
        assert!(matches!(m.screen, Screen::Installing { waiting: true, .. }));
        assert_eq!(m.on(In::Dismiss), vec![]);
        assert!(!m.enabled());
        m.on(In::Waiting(false));
        assert!(matches!(m.screen, Screen::Installing { waiting: false, .. }));
        // The worker has already asked the daemon to quit; the dialog closes.
        assert_eq!(m.on(In::Installed(Ok(Applied::RestartingNow))), vec![Act::Close]);
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
    fn unsolicited_dialog_has_no_default_button() {
        let (mut m, acts) = Machine::start(Request::Offer(DialogState::Available(rel("9.9.9"))));
        assert_eq!(acts, vec![]);
        assert_eq!(m.on(In::Enter), vec![], "Enter never updates");
        assert_eq!(m.screen, Screen::Available(rel("9.9.9")));
        // Clicks (and Tab + Enter on a focused button, which is a click) work.
        assert_eq!(m.clone().on(In::Click(Btn::Update)), vec![Act::Download(rel("9.9.9"))]);
        assert_eq!(m.clone().on(In::Dismiss), vec![Act::Close]);
        // Offered onto a dialog the user opened: unsolicited from then on.
        let mut m = Machine::new(DialogState::UpToDate);
        m.on(In::Open(Request::Offer(DialogState::Available(rel("9.9.9")))));
        assert_eq!(m.on(In::Enter), vec![]);
        // Shown by the user again: Enter is the primary button again.
        m.on(In::Open(Request::Show(DialogState::Available(rel("9.9.9")))));
        assert_eq!(m.on(In::Enter), vec![Act::Download(rel("9.9.9"))]);
        // The first click makes an offered dialog the user's: Enter works.
        let (mut m, _) = Machine::start(Request::Offer(DialogState::Error("offline".into())));
        assert_eq!(m.on(In::Enter), vec![]);
        assert_eq!(m.on(In::Click(Btn::Retry)), vec![Act::Check]);
        m.on(In::Checked(Ok(Some(rel("9.9.9")))));
        assert_eq!(m.on(In::Enter), vec![Act::Download(rel("9.9.9"))]);
        // So does a click anywhere in the window.
        let (tx, rx) = std::sync::mpsc::channel();
        let (m, _) = Machine::start(Request::Offer(DialogState::UpToDate));
        let mut d = Dialog::new(m, crate::theme::DARK, tx, rx);
        d.on_event(Ev::Down { x: 5, y: 5 });
        assert!(!d.m.unsolicited);
    }

    #[test]
    fn tray_check_opens_checking_and_repeats_only_raise() {
        let (mut m, acts) = Machine::start(Request::Check);
        assert_eq!((m.screen.clone(), acts), (Screen::Checking, vec![Act::Check]));
        // A second click while checking: no second check.
        assert_eq!(m.on(In::Open(Request::Check)), vec![]);
        m.on(In::Checked(Ok(Some(rel("9.9.9")))));
        assert_eq!(m.screen, Screen::Available(rel("9.9.9")));
        assert_eq!(m.on(In::Enter), vec![Act::Download(rel("9.9.9"))], "the user asked: Enter updates");
        // A click on a finished result checks again.
        let mut m = Machine::new(DialogState::UpToDate);
        assert_eq!(m.on(In::Open(Request::Check)), vec![Act::Check]);
        assert_eq!(m.screen, Screen::Checking);
    }

    #[test]
    fn cancel_mid_download_aborts_then_closes() {
        let mut m = avail();
        m.on(In::Click(Btn::Update));
        m.on(In::Progress(500, Some(4000)));
        assert_eq!(m.on(In::Click(Btn::Cancel)), vec![Act::Cancel]);
        assert!(matches!(m.screen, Screen::Cancelling(_)));
        // Stale progress changes nothing.
        assert_eq!(m.on(In::Progress(600, Some(4000))), vec![]);
        assert!(matches!(m.screen, Screen::Cancelling(_)));
        // The worker stops (deleting the partial file) and reports it.
        assert_eq!(m.clone().on(In::Fetched(Err(update::CANCELLED.into()))), vec![Act::Close]);
        // Close / Esc while cancelling: the window goes at once (the worker
        // finishes its cleanup alone).
        assert_eq!(m.clone().on(In::Dismiss), vec![Act::Close]);
        assert_eq!(m.clone().on(In::Click(Btn::Close)), vec![Act::Close]);
        assert!(m.enabled(), "Close is clickable while cancelling");
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
        // Committed before the worker saw the flag: the install goes ahead.
        m.on(In::Fetched(Ok(())));
        assert!(matches!(m.screen, Screen::Installing { .. }));
        assert_eq!(m.on(In::Installed(Ok(Applied::RestartingNow))), vec![Act::Close]);
    }

    /// A verified file in a scratch dir, as `fetch_verified` returns it.
    fn fetched_file(tag: &str) -> (PathBuf, PathBuf) {
        let dir = scratch(tag);
        let file = dir.join("rustshot-9.9.9-setup.exe");
        std::fs::write(&file, b"new").unwrap();
        (dir, file)
    }

    fn installer(daemon: Option<Sender<HotEvent>>, applied: Arc<std::sync::atomic::AtomicBool>) -> Installer {
        Installer {
            daemon,
            apply: Box::new(move |_, _| {
                applied.store(true, Ordering::SeqCst);
                Ok(Applied::RestartingNow)
            }),
        }
    }

    /// Cancelled after verification: the file is deleted and CANCELLED
    /// reported; nothing is asked of the daemon or installed.
    #[test]
    fn worker_cancelled_after_verify_deletes_the_file() {
        let (dir, file) = fetched_file("wcancel");
        let (tx, rx) = std::sync::mpsc::channel();
        let (dtx, drx) = std::sync::mpsc::channel();
        let applied = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let phase = AtomicU8::new(CANCELLED);
        install(Ok((file.clone(), [0; 32])), &phase, &tx, &installer(Some(dtx), applied.clone()));
        assert!(!file.exists());
        assert!(matches!(rx.try_recv(), Ok(In::Fetched(Err(e))) if e == update::CANCELLED));
        assert!(drx.try_recv().is_err() && !applied.load(Ordering::SeqCst));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `apply` (which starts the new version) runs only after the daemon
    /// replied that no capture is open; then the daemon is told to quit.
    #[test]
    fn worker_installs_only_after_the_daemon_is_idle() {
        let (dir, file) = fetched_file("widle");
        let (tx, rx) = std::sync::mpsc::channel();
        let (dtx, drx) = std::sync::mpsc::channel();
        let applied = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let inst = installer(Some(dtx), applied.clone());
        let f = file.clone();
        let worker = std::thread::spawn(move || {
            install(Ok((f, [0; 32])), &AtomicU8::new(RUNNING), &tx, &inst);
        });
        let Ok(HotEvent::RestartWhenIdle(reply)) = drx.recv_timeout(Duration::from_secs(5)) else {
            panic!("the worker asks the daemon to go idle first");
        };
        assert!(matches!(rx.recv_timeout(Duration::from_secs(5)), Ok(In::Fetched(Ok(())))));
        // A capture is open: the dialog says so, nothing is installed.
        assert!(matches!(rx.recv_timeout(Duration::from_secs(5)), Ok(In::Waiting(true))));
        std::thread::sleep(Duration::from_millis(100));
        assert!(!applied.load(Ordering::SeqCst), "apply before the idle reply");
        reply.send(()).unwrap();
        worker.join().unwrap();
        assert!(applied.load(Ordering::SeqCst));
        assert!(matches!(drx.try_recv(), Ok(HotEvent::Restart)));
        let rest: Vec<In> = rx.try_iter().collect();
        assert!(matches!(&rest[..], [In::Waiting(false), In::Installed(Ok(Applied::RestartingNow))]), "{rest:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The daemon went away (quit while waiting, or never registered):
    /// nothing is installed and the file is deleted.
    #[test]
    fn worker_aborts_without_a_daemon() {
        for gone_while_waiting in [false, true] {
            let (dir, file) = fetched_file(&format!("wgone{gone_while_waiting}"));
            let (tx, rx) = std::sync::mpsc::channel();
            let applied = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let daemon = if gone_while_waiting {
                let (dtx, drx) = std::sync::mpsc::channel::<HotEvent>();
                // The daemon quits: its receiver and the pending reply go.
                std::thread::spawn(move || drop(drx.recv()));
                Some(dtx)
            } else {
                None
            };
            install(Ok((file.clone(), [0; 32])), &AtomicU8::new(RUNNING), &tx, &installer(daemon, applied.clone()));
            assert!(!applied.load(Ordering::SeqCst));
            assert!(!file.exists());
            let last = rx.try_iter().last();
            assert!(matches!(last, Some(In::Installed(Err(_)))), "{last:?}");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// A failed install lifts the daemon's capture gate.
    #[test]
    fn worker_failed_install_releases_the_daemon() {
        let (dir, file) = fetched_file("wfail");
        let (tx, rx) = std::sync::mpsc::channel();
        let (dtx, drx) = std::sync::mpsc::channel();
        let inst = Installer { daemon: Some(dtx), apply: Box::new(|_, _| Err("denied".into())) };
        let daemon = std::thread::spawn(move || {
            let Ok(HotEvent::RestartWhenIdle(reply)) = drx.recv() else { panic!() };
            reply.send(()).unwrap();
            drx.recv().unwrap()
        });
        install(Ok((file.clone(), [0; 32])), &AtomicU8::new(RUNNING), &tx, &inst);
        assert!(matches!(daemon.join().unwrap(), HotEvent::RestartAborted));
        assert!(!file.exists());
        assert!(matches!(rx.try_iter().last(), Some(In::Installed(Err(e))) if e == "Install failed: denied"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn worker_phase_decides_cancel_or_install() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut d = Dialog::new(avail(), crate::theme::DARK, tx, rx);
        d.phase = Arc::new(AtomicU8::new(RUNNING));
        d.perform(Act::Cancel);
        assert_eq!(d.phase.load(Ordering::SeqCst), CANCELLED);
        // Committed first: a later cancel (or the window closing) can't undo it.
        d.phase.store(COMMITTED, Ordering::SeqCst);
        d.perform(Act::Cancel);
        d.on_quit();
        assert_eq!(d.phase.load(Ordering::SeqCst), COMMITTED);
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

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rustshot-dlg-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The driver runs `Act::Skip` against its config file and closes.
    #[test]
    fn driver_skip_writes_the_config_and_closes() {
        let dir = scratch("skip");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut d = Dialog::new(avail(), crate::theme::DARK, tx, rx);
        d.config = dir.join("config.toml");
        std::fs::write(&d.config, "# keep me\ntheme = \"light\"\n").unwrap();
        d.queue.push_back(In::Click(Btn::Skip));
        assert!(d.process());
        assert!(d.closing);
        let text = std::fs::read_to_string(&d.config).unwrap();
        assert_eq!(text, "# keep me\ntheme = \"light\"\nskip_version = \"9.9.9\"\n");
        // Unwritable (a directory): the error is shown, the window stays.
        let (tx, rx) = std::sync::mpsc::channel();
        let mut d = Dialog::new(avail(), crate::theme::DARK, tx, rx);
        d.config = dir.clone();
        d.queue.push_back(In::Click(Btn::Skip));
        d.process();
        assert!(!d.closing);
        assert!(matches!(&d.m.screen, Screen::SaveFailed(e) if e.starts_with(&dir.display().to_string())));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn enter_waits_out_the_grace_after_changes() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut d = Dialog::new(Machine::new(DialogState::UpToDate), crate::theme::DARK, tx, rx);
        let t0 = d.changed_at;
        assert!(!d.enter_ready(t0 + Duration::from_millis(100)));
        assert!(d.enter_ready(t0 + ENTER_GRACE));
        // New content restarts the grace.
        std::thread::sleep(Duration::from_millis(5));
        d.queue.push_back(In::Open(Request::Show(DialogState::Available(rel("9.9.9")))));
        d.process();
        assert!(d.changed_at > t0);
        assert!(!d.enter_ready(t0 + ENTER_GRACE));
        // So does a screen change (Retry → Checking → result).
        let t1 = d.changed_at;
        std::thread::sleep(Duration::from_millis(5));
        d.m = Machine::start(Request::Check).0;
        d.queue.push_back(In::Checked(Ok(None)));
        d.process();
        assert!(d.changed_at > t1);
        // Progress within a screen does not.
        d.m = avail();
        d.m.on(In::Click(Btn::Update));
        let t2 = d.changed_at;
        d.queue.push_back(In::Progress(10, Some(100)));
        d.process();
        assert_eq!(d.changed_at, t2);
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
    fn open_replaces_content_unless_busy() {
        let mut m = Machine::new(DialogState::UpToDate);
        m.on(In::Open(Request::Show(DialogState::Available(rel("9.9.9")))));
        assert_eq!(m.screen, Screen::Available(rel("9.9.9")));
        m.on(In::Click(Btn::Update));
        m.on(In::Open(Request::Show(DialogState::Error("x".into()))));
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
    fn render(m: &Machine, th: &Theme, k: f32, input: &Input) -> (PixBuf, Option<Btn>) {
        let mut focus = FocusState::default();
        let mut v = View::default();
        preview::render(W, H, k, th, &Input::default(), &mut focus, |ui| {
            paint(ui, m, &mut v);
        });
        let mut clicked = None;
        let img = preview::render(W, H, k, th, input, &mut focus, |ui| clicked = paint(ui, m, &mut v));
        (img, clicked)
    }

    fn screens() -> Vec<(&'static str, Machine)> {
        let r = rel("0.1.2");
        let at = |s: Screen| Machine { screen: s, unsolicited: false };
        vec![
            ("available", at(Screen::Available(r.clone()))),
            ("downloading", at(Screen::Downloading { rel: r.clone(), got: 1_234_567, total: Some(3_400_000) })),
            ("verifying", at(Screen::Verifying(r.clone()))),
            ("cancelling", at(Screen::Cancelling(r.clone()))),
            ("installing", at(Screen::Installing { rel: r.clone(), waiting: false })),
            ("waiting", at(Screen::Installing { rel: r.clone(), waiting: true })),
            (
                "install-failed",
                at(Screen::InstallFailed {
                    rel: r.clone(),
                    msg: "Download failed: rustshot-0.1.2-setup.exe failed its SHA-256 check; refusing to install".into(),
                }),
            ),
            ("uptodate", at(Screen::UpToDate)),
            ("managed", at(Screen::Managed("Microsoft Store"))),
            (
                "error",
                at(Screen::CheckFailed(
                    "WinHttpSendRequest: The server name or address could not be resolved (0x80072EE7)".into(),
                )),
            ),
            ("checking", at(Screen::Checking)),
            ("save-failed", at(Screen::SaveFailed(r"C:\Users\denis\AppData\Roaming\rustshot\config.toml: Access is denied. (os error 5)".into()))),
        ]
    }

    #[test]
    fn preview_dialog_pngs() {
        for (tname, th) in [("dark", &crate::theme::DARK), ("light", &crate::theme::LIGHT)] {
            for (name, m) in screens() {
                let (img, _) = render(&m, th, 1.0, &Input::default());
                // Something besides the background was drawn.
                let bg = th.surface;
                assert!(img.as_raw().as_chunks::<4>().0.iter().any(|p| p[..3] != [bg.r, bg.g, bg.b]), "{name}");
                preview::save(&format!("update-{name}-{tname}.png"), &img);
            }
        }
        for (name, m) in screens().into_iter().filter(|(n, _)| ["available", "install-failed", "error"].contains(n)) {
            let (img, _) = render(&m, &crate::theme::DARK, 1.5, &Input::default());
            preview::save(&format!("update-{name}-dark-150.png"), &img);
        }
    }

    fn overlap(a: FRect, b: FRect) -> bool {
        a.x < b.x1() && b.x < a.x1() && a.y < b.y1() && b.y < a.y1()
    }

    /// No part of any screen overlaps another or leaves the window, with
    /// short and very long messages, at 100 % and 150 %.
    #[test]
    fn layout_parts_never_overlap() {
        let long = "word ".repeat(80);
        let mut cases = screens();
        let r = rel("0.1.2");
        let at = |s: Screen| Machine { screen: s, unsolicited: false };
        cases.push(("long-error", at(Screen::CheckFailed(long.clone()))));
        cases.push(("long-install-failed", at(Screen::InstallFailed { rel: r.clone(), msg: long.clone() })));
        cases.push(("short-install-failed", at(Screen::InstallFailed { rel: r, msg: "x".into() })));
        for k in [1.0f32, 1.5] {
            for (name, m) in &cases {
                let mut lay = None;
                let mut has_text = false;
                let mut focus = FocusState::default();
                preview::render(W, H, k, &crate::theme::DARK, &Input::default(), &mut focus, |ui| {
                    let c = content(m, ui.theme);
                    has_text = c.text.is_some();
                    lay = Some(layout(ui, &c));
                });
                let lay = lay.unwrap();
                let parts: Vec<(&str, FRect)> = [("title", Some(lay.title)), ("text", lay.text), ("notes", lay.notes)]
                    .into_iter()
                    .chain([("progress", lay.progress), ("buttons", Some(lay.buttons))])
                    .filter_map(|(n, r)| r.map(|r| (n, r)))
                    .collect();
                let win = FRect { x: 0.0, y: 0.0, w: W as f32, h: H as f32 };
                for (i, &(a, ra)) in parts.iter().enumerate() {
                    assert!(ra.x >= PAD - 0.01 && ra.x1() <= win.x1() - PAD + 0.01, "{name} {a} {ra:?}");
                    assert!(ra.y >= PAD - 0.01 && ra.y1() <= win.y1() - PAD + 0.01, "{name} {a} {ra:?}");
                    for &(b, rb) in &parts[i + 1..] {
                        assert!(!overlap(ra, rb), "{name} k={k}: {a} {ra:?} overlaps {b} {rb:?}");
                    }
                }
                assert_eq!(lay.text.is_some(), has_text, "{name}");
                if let Some(t) = lay.text {
                    assert!(t.h >= 20.0, "{name}: at least one line of message");
                }
                if let Some(n) = lay.notes {
                    assert!(n.h >= 40.0, "{name}: notes box keeps some height");
                    // Whole 20 px lines inside 8 px padding: no partial row.
                    let rows = (n.h - 16.0) / 20.0;
                    assert!((rows - rows.round()).abs() < 1e-3, "{name}: {rows} rows");
                }
                if name.contains("install-failed") {
                    assert!(lay.notes.is_some(), "{name}: the notes stay visible");
                }
            }
        }
    }

    /// Clicking where each button is drawn reports it (layout and hit
    /// rects agree), at 100 % and 150 %.
    #[test]
    fn buttons_click_where_drawn() {
        for k in [1.0f32, 1.5] {
            let m = avail();
            // Right-aligned row: Cancel is rightmost, ending at W - PAD.
            let y = ((H as f32 - PAD - 16.0) * k) as i32;
            let x = ((W as f32 - PAD - 20.0) * k) as i32;
            let mut input = Input::default();
            for e in [Ev::Move { x, y }, Ev::Down { x, y }, Ev::Up { x, y }] {
                input.feed(&e);
            }
            let (_, clicked) = render(&m, &crate::theme::DARK, k, &input);
            assert_eq!(clicked, Some(Btn::Cancel), "k={k}");
        }
    }

    #[test]
    fn notes_are_the_excerpt() {
        let mut v = View::default();
        let r = rel("9.9.9");
        let n = v.notes(&r).to_vec();
        assert_eq!(n[0], ("Highlights".to_string(), true));
        assert!(n[1].0.starts_with("- Save straight") && !n[1].1);
        assert!(n.len() <= NOTES_LINES);
        // The run of blank lines before "Fixes" is one blank line.
        let fixes = n.iter().position(|l| l.0 == "Fixes").unwrap();
        assert!(n[fixes].1 && n[fixes - 1].0.is_empty() && !n[fixes - 2].0.is_empty());
        v.scroll = 30.0;
        v.notes(&r);
        assert_eq!(v.scroll, 30.0, "same release keeps the scroll");
        v.notes(&rel("9.9.10"));
        assert_eq!(v.scroll, 0.0);
    }
}
