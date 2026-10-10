use crate::config::Config;
use crate::wind::key;
use std::sync::mpsc::{self, Receiver, Sender};

#[derive(Debug, Clone)]
pub enum HotEvent {
    Capture,
    Quit,
    /// An update is about to install: reply on the sender as soon as no
    /// capture, editor or upload is running, and start no new capture from
    /// now on (until `RestartAborted` or the exit).
    RestartWhenIdle(Sender<()>),
    /// The install after `RestartWhenIdle` failed: captures work again.
    RestartAborted,
    /// An installed update is starting: quit once no capture is open.
    Restart,
    /// The Settings window saved `config.toml`: re-read it, re-register the
    /// global hotkeys and apply `check_updates`.
    ReloadConfig,
}

/// Global hotkeys backed by Win32 `RegisterHotKey` (replaces the
/// `global-hotkey` crate). Registration and the message loop run on a
/// dedicated thread so we own the queue instead of relying on any toolkit.
pub struct Hotkeys {
    rx: Receiver<HotEvent>,
    tx: Sender<HotEvent>,
    /// The registration thread (stopped and replaced by `reload`).
    #[cfg(not(target_os = "macos"))]
    worker: Option<imp::Worker>,
    /// The startup registration's result, not read yet.
    #[cfg(not(target_os = "macos"))]
    started: Option<Receiver<Registered>>,
    /// The specs registered now (capture, quit); "" where none is (or
    /// where it is not known, so the next reload tries again).
    specs: [String; 2],
    /// Failures already told to the user (once per run each).
    #[cfg(not(target_os = "macos"))]
    told: Vec<Failure>,
}

/// Why a hotkey did not register.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(target_os = "macos", allow(dead_code))] // macOS: hotkeys apply after a restart
pub enum Cause {
    /// Not a chord Rustshot can register (or a key this keyboard lacks).
    Invalid,
    /// No X display to grab keys on (Linux).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    NoDisplay,
    /// Another app holds it.
    InUse,
    /// The registration thread did not answer in time.
    Unknown,
}

/// Per slot (capture, quit): registered (or nothing to register), or why not.
#[cfg_attr(target_os = "macos", allow(dead_code))] // macOS: hotkeys apply after a restart
pub type Registered = [Result<(), Cause>; 2];

/// The hotkeys that are not registered as saved, for the Settings window
/// (published by the daemon after every reload).
static OUTSTANDING: std::sync::Mutex<Vec<Failure>> = std::sync::Mutex::new(Vec::new());

/// The hotkeys the daemon could not register as saved (none in a process
/// without one).
pub fn outstanding() -> Vec<Failure> {
    OUTSTANDING.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

#[cfg_attr(target_os = "macos", allow(dead_code))] // macOS: hotkeys apply after a restart
fn publish(f: &[Failure]) {
    *OUTSTANDING.lock().unwrap_or_else(|e| e.into_inner()) = f.to_vec();
}

/// How long `reload` waits for a registration thread's result.
#[cfg(not(target_os = "macos"))]
const RESULT_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// Registration specs for `[capture, quit]`: capture (id 1) and quit (id 2).
fn specs_for(s: &[String; 2]) -> [(i32, String, HotEvent); 2] {
    [(1, s[0].clone(), HotEvent::Capture), (2, s[1].clone(), HotEvent::Quit)]
}

fn wanted(cfg: &Config) -> [String; 2] {
    [cfg.capture_hotkey.trim().to_string(), cfg.quit_hotkey.trim().to_string()]
}

/// A hotkey `reload` could not register.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// 0 = capture, 1 = quit.
    pub slot: usize,
    pub wanted: String,
    pub cause: Cause,
    /// The previous chord, registered again in its place.
    pub kept: Option<String>,
}

impl Failure {
    /// "Couldn't register Ctrl+Alt+F9 — it's in use by another app; kept Shift+Win+X."
    pub fn message(&self) -> String {
        let chord = chord_label(&self.wanted);
        let why = match self.cause {
            Cause::Unknown => return format!("Couldn't confirm {chord} was registered."),
            Cause::Invalid => "it's not a hotkey Rustshot can register",
            Cause::NoDisplay => "no X display is available",
            Cause::InUse => "it's in use by another app",
        };
        let mut s = format!("Couldn't register {chord} — {why}");
        if let Some(k) = &self.kept {
            s.push_str(&format!("; kept {}", chord_label(k)));
        }
        s.push('.');
        s
    }

    /// The same slot, chord and cause (what was kept does not matter).
    #[cfg_attr(target_os = "macos", allow(dead_code))] // macOS: hotkeys apply after a restart
    fn same(&self, o: &Failure) -> bool {
        self.slot == o.slot && self.cause == o.cause && same_chord(&self.wanted, &o.wanted)
    }
}

/// The failures in `now` not told yet (each slot, chord and cause once per
/// run); they are added to `told`.
#[cfg_attr(target_os = "macos", allow(dead_code))] // macOS: hotkeys apply after a restart
pub fn untold(told: &mut Vec<Failure>, now: &[Failure]) -> Vec<Failure> {
    let new: Vec<Failure> = now.iter().filter(|f| !told.iter().any(|t| t.same(f))).cloned().collect();
    told.extend(new.iter().cloned());
    new
}

/// `spec` as the Settings window shows it (Win/Super/Cmd for Meta); left
/// as written when it does not parse.
pub fn chord_label(spec: &str) -> String {
    crate::keymap::Chord::parse(spec).map_or_else(|| spec.trim().to_string(), |c| c.label())
}

/// Move from the registered `old` specs to `want`. `register` replaces
/// every registration with the specs it is given and says which worked.
/// A slot that failed for a known cause gets its old chord back (unless a
/// working slot took that chord); one whose result is unknown (no answer)
/// is left empty, with no fallback. Afterwards a slot holds its spec only
/// where the registration is confirmed, so the next reload retries the
/// others. Returns the specs registered now and the failures.
#[cfg_attr(target_os = "macos", allow(dead_code))] // macOS: hotkeys apply after a restart
pub fn plan_reload(
    old: &[String; 2],
    want: &[String; 2],
    mut register: impl FnMut(&[String; 2]) -> Registered,
) -> ([String; 2], Vec<Failure>) {
    if want == old {
        return (old.clone(), Vec::new());
    }
    let first = register(want);
    let known = |r: &Result<(), Cause>| matches!(r, Err(c) if *c != Cause::Unknown);
    let (mut tried, mut last) = (want.clone(), first);
    if first.iter().any(known) {
        let retry: [String; 2] = std::array::from_fn(|i| {
            if !known(&first[i]) {
                want[i].clone()
            } else if (0..2).any(|j| first[j].is_ok() && j != i && !want[j].is_empty() && same_chord(&want[j], &old[i])) {
                String::new() // the other slot holds that chord now
            } else {
                old[i].clone()
            }
        });
        if retry != *want {
            last = register(&retry);
            tried = retry;
        }
    }
    let now = held(&tried, last);
    let failures = (0..2)
        .filter_map(|i| {
            // A known cause from the first try, else what the last one says.
            let cause = match (first[i], last[i]) {
                (Err(c), _) if c != Cause::Unknown => c,
                (_, Err(c)) => c,
                _ => return None,
            };
            Some(Failure {
                slot: i,
                wanted: want[i].clone(),
                cause,
                kept: (!now[i].is_empty() && now[i] != want[i]).then(|| now[i].clone()),
            })
        })
        .collect();
    (now, failures)
}

/// The specs of `tried` that registered.
#[cfg_attr(target_os = "macos", allow(dead_code))] // macOS: hotkeys apply after a restart
fn held(tried: &[String; 2], ok: Registered) -> [String; 2] {
    std::array::from_fn(|i| if ok[i].is_ok() { tried[i].clone() } else { String::new() })
}

/// What a registration thread that did not answer means: every slot with
/// a chord is unknown.
#[cfg_attr(target_os = "macos", allow(dead_code))] // macOS: hotkeys apply after a restart
fn unanswered(specs: &[String; 2]) -> Registered {
    std::array::from_fn(|i| if specs[i].trim().is_empty() { Ok(()) } else { Err(Cause::Unknown) })
}

#[cfg_attr(target_os = "macos", allow(dead_code))] // macOS: hotkeys apply after a restart
fn same_chord(a: &str, b: &str) -> bool {
    match (parse_hotkey(a), parse_hotkey(b)) {
        (Some(x), Some(y)) => x == y,
        _ => a.trim().eq_ignore_ascii_case(b.trim()),
    }
}

impl Hotkeys {
    pub fn new(cfg: &Config) -> Self {
        let (tx, rx) = mpsc::channel();
        let now = wanted(cfg);
        let specs = specs_for(&now);
        #[cfg(not(target_os = "macos"))]
        let (worker, started) = {
            let (done, started) = mpsc::channel();
            (Some(imp::Worker::start(specs, tx.clone(), done)), Some(started))
        };
        #[cfg(target_os = "macos")]
        {
            let hot_tx = tx.clone();
            std::thread::spawn(move || imp::hotkey_thread(specs, hot_tx));
        }
        Self {
            rx,
            tx,
            #[cfg(not(target_os = "macos"))]
            worker,
            #[cfg(not(target_os = "macos"))]
            started,
            specs: now,
            #[cfg(not(target_os = "macos"))]
            told: Vec::new(),
        }
    }

    /// Take the startup registration's result once it is there (never
    /// waits): what did not register is left empty, so the first reload
    /// tries it again, and shows in the Settings window meanwhile.
    #[cfg(not(target_os = "macos"))]
    fn check_started(&mut self) {
        let Some(started) = &self.started else { return };
        let ok = match started.try_recv() {
            Ok(ok) => ok,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => unanswered(&self.specs),
        };
        self.started = None;
        let failed: Vec<Failure> = (0..2)
            .filter_map(|i| ok[i].err().map(|cause| Failure { slot: i, wanted: self.specs[i].clone(), cause, kept: None }))
            .collect();
        self.specs = held(&self.specs, ok);
        publish(&failed);
    }

    /// Register `cfg`'s hotkeys instead of the current ones (no-op when
    /// they are unchanged). The old registrations are released first, so
    /// a chord can move between capture and quit. A chord that cannot be
    /// registered keeps the previous one ([`plan_reload`]). Returns the
    /// failures not told to the user yet in this run ([`untold`]); every
    /// current one is published for the Settings window ([`outstanding`]).
    pub fn reload(&mut self, cfg: &Config) -> Vec<Failure> {
        let want = wanted(cfg);
        #[cfg(not(target_os = "macos"))]
        {
            if let Some(started) = self.started.take() {
                let ok = started.recv_timeout(RESULT_WAIT).unwrap_or_else(|_| unanswered(&self.specs));
                self.specs = held(&self.specs, ok);
            }
            let old = self.specs.clone();
            let (now, failures) = plan_reload(&old, &want, |s| self.restart(s));
            publish(&failures);
            self.specs = now;
            untold(&mut self.told, &failures)
        }
        #[cfg(target_os = "macos")]
        {
            if want != self.specs {
                eprintln!("rustshot: new global hotkeys take effect after a restart");
            }
            Vec::new()
        }
    }

    /// Replace the registration thread with one for `specs`; its result.
    #[cfg(not(target_os = "macos"))]
    fn restart(&mut self, specs: &[String; 2]) -> Registered {
        if let Some(w) = self.worker.take() {
            w.stop();
        }
        let (done, result) = mpsc::channel();
        self.worker = Some(imp::Worker::start(specs_for(specs), self.tx.clone(), done));
        // No answer in time: unknown (left empty, so the next reload retries).
        result.recv_timeout(RESULT_WAIT).unwrap_or_else(|_| unanswered(specs))
    }

    /// Another producer of events (single-instance listener, tray).
    pub fn sender(&self) -> Sender<HotEvent> {
        self.tx.clone()
    }

    /// The next queued event, if any.
    pub fn poll(&mut self) -> Option<HotEvent> {
        #[cfg(not(target_os = "macos"))]
        self.check_started();
        self.rx.try_recv().ok()
    }
}

#[cfg(windows)]
#[path = "hotkey_win.rs"]
mod imp;
#[cfg(target_os = "linux")]
#[path = "hotkey_linux.rs"]
mod imp;
#[cfg(target_os = "macos")]
#[path = "hotkey_macos.rs"]
mod imp;

/// Parse things like `Shift+Meta+X`, `Ctrl+Alt+Shift+Q`, `PrintScreen`.
/// Returns `(modifier flags, virtual-key code)` for `RegisterHotKey`.
pub fn parse_hotkey(spec: &str) -> Option<(u32, u32)> {
    // Win32 modifier flags: MOD_ALT=1, MOD_CONTROL=2, MOD_SHIFT=4, MOD_WIN=8.
    let mut mods = 0u32;
    let mut vk: Option<u32> = None;
    for part in spec.split('+').map(str::trim).filter(|p| !p.is_empty()) {
        let lower = part.to_ascii_lowercase();
        match lower.as_str() {
            "ctrl" | "control" => {
                mods |= 0x0002;
                continue;
            }
            "shift" => {
                mods |= 0x0004;
                continue;
            }
            "alt" => {
                mods |= 0x0001;
                continue;
            }
            "meta" | "win" | "super" | "cmd" => {
                mods |= 0x0008;
                continue;
            }
            _ => {}
        }
        if vk.is_some() {
            return None; // two non-modifier keys ("A+B")
        }
        vk = Some(key_vk(&lower)?);
    }
    Some((mods, vk?))
}

/// Key name (lowercase) → virtual-key code; the one key table shared by
/// global hotkeys and the editor keymap.
fn key_vk(lower: &str) -> Option<u32> {
    if lower.len() == 1 {
        let c = lower.chars().next().unwrap();
        if c.is_ascii_lowercase() {
            // VK_A..VK_Z = 0x41..0x5A.
            return Some(0x41 + (c as u32 - 'a' as u32));
        }
        if c.is_ascii_digit() {
            // VK_0..VK_9 = 0x30..0x39.
            return Some(0x30 + (c as u32 - '0' as u32));
        }
    }
    if let Some(rest) = lower.strip_prefix('f')
        && let Ok(n) = rest.parse::<u32>()
        && (1..=12).contains(&n)
    {
        return Some(0x70 + n - 1); // VK_F1..VK_F12
    }
    let vk = match lower {
        "space" => key::SPACE,
        "enter" | "return" => key::RETURN,
        "esc" | "escape" => key::ESCAPE,
        "tab" => key::TAB,
        "backspace" => key::BACK,
        "delete" | "del" => key::DELETE,
        "insert" => key::INSERT,
        "home" => key::HOME,
        "end" => key::END,
        "pageup" => key::PAGEUP,
        "pagedown" => key::PAGEDOWN,
        "up" => key::UP,
        "down" => key::DOWN,
        "left" => key::LEFT,
        "right" => key::RIGHT,
        "printscreen" | "prtsc" | "print" => key::PRINTSCREEN,
        _ => return None,
    };
    Some(vk)
}

/// Display name of a virtual key in the chord syntax `parse_hotkey`
/// accepts ("S", "5", "F5", "Del", "Space", ...).
pub fn key_name(vk: u32) -> Option<String> {
    if (0x41..=0x5A).contains(&vk) || (0x30..=0x39).contains(&vk) {
        return Some(char::from_u32(vk)?.to_string());
    }
    if (0x70..=0x7B).contains(&vk) {
        return Some(format!("F{}", vk - 0x70 + 1));
    }
    let name = match vk {
        key::SPACE => "Space",
        key::RETURN => "Enter",
        key::ESCAPE => "Esc",
        key::TAB => "Tab",
        key::BACK => "Backspace",
        key::DELETE => "Del",
        key::INSERT => "Insert",
        key::HOME => "Home",
        key::END => "End",
        key::PAGEUP => "PageUp",
        key::PAGEDOWN => "PageDown",
        key::UP => "Up",
        key::DOWN => "Down",
        key::LEFT => "Left",
        key::RIGHT => "Right",
        key::PRINTSCREEN => "PrintScreen",
        _ => return None,
    };
    Some(name.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_custom_specs() {
        assert_eq!(parse_hotkey("Shift+Meta+X"), Some((0x0008 | 0x0004, 0x58)));
        assert_eq!(
            parse_hotkey("Ctrl+Alt+Shift+Q"),
            Some((0x0002 | 0x0001 | 0x0004, 0x51))
        );
        assert_eq!(parse_hotkey("PrintScreen"), Some((0, 0x2C)));
        assert_eq!(parse_hotkey("F5"), Some((0, 0x74)));
        assert_eq!(parse_hotkey("bogus+key"), None);
        assert_eq!(parse_hotkey(""), None);
        assert_eq!(parse_hotkey("A+B"), None);
        assert_eq!(parse_hotkey("Shift"), None);
    }

    fn s2(a: &str, b: &str) -> [String; 2] {
        [a.to_string(), b.to_string()]
    }

    /// A fake registrar: chords in `taken` belong to another app.
    fn registrar<'a>(taken: &'a [&'a str], calls: &'a mut Vec<[String; 2]>) -> impl FnMut(&[String; 2]) -> Registered + 'a {
        move |s: &[String; 2]| {
            calls.push(s.clone());
            std::array::from_fn(|i| {
                if s[i].is_empty() || !taken.iter().any(|t| same_chord(t, &s[i])) { Ok(()) } else { Err(Cause::InUse) }
            })
        }
    }

    fn fail(slot: usize, wanted: &str, cause: Cause, kept: Option<&str>) -> Failure {
        Failure { slot, wanted: wanted.into(), cause, kept: kept.map(Into::into) }
    }

    #[test]
    fn reload_plan_unchanged_registers_nothing() {
        let mut calls = Vec::new();
        let old = s2("Shift+Meta+X", "Ctrl+Alt+Shift+Q");
        let (now, f) = plan_reload(&old, &old.clone(), registrar(&[], &mut calls));
        assert_eq!((now, f), (old, vec![]));
        assert!(calls.is_empty());
    }

    #[test]
    fn reload_plan_success_and_swap() {
        let mut calls = Vec::new();
        let old = s2("Shift+Meta+X", "Ctrl+Alt+Shift+Q");
        // Capture and quit trade places: one registration, both work.
        let want = s2("Ctrl+Alt+Shift+Q", "Shift+Meta+X");
        let (now, f) = plan_reload(&old, &want, registrar(&[], &mut calls));
        assert_eq!((now, f), (want.clone(), vec![]));
        assert_eq!(calls, [want]);
    }

    #[test]
    fn reload_plan_failure_keeps_the_old_chord() {
        let mut calls = Vec::new();
        let old = s2("Shift+Meta+X", "Ctrl+Alt+Shift+Q");
        let want = s2("Ctrl+Alt+F9", "Ctrl+Alt+Shift+Q");
        let (now, f) = plan_reload(&old, &want, registrar(&["ctrl+alt+f9"], &mut calls));
        assert_eq!(now, old, "the old capture chord is back");
        assert_eq!(calls, [want.clone(), old.clone()]);
        assert_eq!(f, [fail(0, "Ctrl+Alt+F9", Cause::InUse, Some("Shift+Meta+X"))]);
        #[cfg(windows)]
        assert_eq!(f[0].message(), "Couldn't register Ctrl+Alt+F9 — it's in use by another app; kept Shift+Win+X.");
        // Saving the same settings again retries (the specs differ).
        let mut calls = Vec::new();
        let (again, _) = plan_reload(&now, &want, registrar(&[], &mut calls));
        assert_eq!((again, calls.len()), (want, 1));
    }

    #[test]
    fn reload_plan_without_a_fallback() {
        // No previous chord, or the previous one is gone too: the slot is
        // left empty (so a later save retries) and nothing is "kept".
        let mut calls = Vec::new();
        let old = s2("", "Ctrl+Alt+Shift+Q");
        let want = s2("Ctrl+Alt+F9", "Ctrl+Alt+Shift+Q");
        let (now, f) = plan_reload(&old, &want, registrar(&["Ctrl+Alt+F9"], &mut calls));
        assert_eq!(now, s2("", "Ctrl+Alt+Shift+Q"));
        assert_eq!(f[0].kept, None);
        assert_eq!(f[0].message(), "Couldn't register Ctrl+Alt+F9 — it's in use by another app.");
        let mut calls = Vec::new();
        let old = s2("Shift+Meta+X", "Ctrl+Alt+Shift+Q");
        let (now, f) = plan_reload(&old, &want, registrar(&["Ctrl+Alt+F9", "Meta+Shift+X"], &mut calls));
        assert_eq!(now, s2("", "Ctrl+Alt+Shift+Q"));
        assert_eq!(f[0].kept, None);
        // The quit slot took the old capture chord: capture is not given
        // it back (it would knock quit out).
        let mut calls = Vec::new();
        let want = s2("Ctrl+Alt+F9", "Shift+Meta+X");
        let (now, f) = plan_reload(&old, &want, registrar(&["Ctrl+Alt+F9"], &mut calls));
        assert_eq!(now, s2("", "Shift+Meta+X"));
        assert_eq!(calls, [want.clone(), s2("", "Shift+Meta+X")]);
        assert_eq!((f.len(), f[0].kept.clone()), (1, None));
    }

    /// No answer from the registration thread: the result is unknown, the
    /// slots are left empty (the next reload retries), nothing falls back,
    /// and the user is told it is not confirmed.
    #[test]
    fn reload_plan_timeout_is_unknown() {
        let old = s2("Shift+Meta+X", "Ctrl+Alt+Shift+Q");
        let want = s2("Ctrl+Alt+F9", "");
        let mut calls = 0;
        let (now, f) = plan_reload(&old, &want, |s| {
            calls += 1;
            unanswered(s)
        });
        assert_eq!((now.clone(), calls), (s2("", ""), 1), "no fallback registration");
        assert_eq!(f, [fail(0, "Ctrl+Alt+F9", Cause::Unknown, None)]);
        assert_eq!(f[0].message(), "Couldn't confirm Ctrl+Alt+F9 was registered.");
        // The next reload (an unrelated save) tries again, and it works.
        let mut calls = Vec::new();
        let (again, f) = plan_reload(&now, &want, registrar(&[], &mut calls));
        assert_eq!((again, f, calls.len()), (want.clone(), vec![], 1));
        // One slot fails for a known cause, the retry gets no answer: both
        // are left empty; the known cause is the one reported.
        let want = s2("Ctrl+Alt+F9", "Ctrl+Alt+F10");
        let mut n = 0;
        let (now, f) = plan_reload(&old, &want, |s| {
            n += 1;
            if n == 1 { [Err(Cause::InUse), Ok(())] } else { unanswered(s) }
        });
        assert_eq!(now, s2("", ""));
        assert_eq!(f, [fail(0, "Ctrl+Alt+F9", Cause::InUse, None), fail(1, "Ctrl+Alt+F10", Cause::Unknown, None)]);
    }

    #[test]
    fn failure_causes_have_their_own_words() {
        let m = |c| fail(1, "Ctrl+Alt+F9", c, Some("Ctrl+Alt+Shift+Q")).message();
        assert_eq!(m(Cause::InUse), "Couldn't register Ctrl+Alt+F9 — it's in use by another app; kept Ctrl+Alt+Shift+Q.");
        assert_eq!(m(Cause::Invalid), "Couldn't register Ctrl+Alt+F9 — it's not a hotkey Rustshot can register; kept Ctrl+Alt+Shift+Q.");
        assert_eq!(m(Cause::NoDisplay), "Couldn't register Ctrl+Alt+F9 — no X display is available; kept Ctrl+Alt+Shift+Q.");
        assert_eq!(m(Cause::Unknown), "Couldn't confirm Ctrl+Alt+F9 was registered.");
        // An invalid chord is reported as such, left as written.
        let (_, f) = plan_reload(&s2("", ""), &s2("Hyper+X", ""), |s| {
            [if s[0].is_empty() { Ok(()) } else { Err(Cause::Invalid) }, Ok(())]
        });
        assert_eq!(f[0].message(), "Couldn't register Hyper+X — it's not a hotkey Rustshot can register.");
    }

    /// A failure is told once per run: an unrelated save that hits the
    /// same (slot, chord, cause) again stays quiet; another cause, chord
    /// or slot is new.
    #[test]
    fn failures_are_told_once() {
        let mut told = Vec::new();
        let a = fail(0, "Ctrl+Alt+F9", Cause::InUse, Some("Shift+Meta+X"));
        assert_eq!(untold(&mut told, std::slice::from_ref(&a)), std::slice::from_ref(&a));
        assert_eq!(untold(&mut told, std::slice::from_ref(&a)), []);
        // Spelled differently, kept something else: still the same failure.
        let a2 = fail(0, "ctrl+alt+f9", Cause::InUse, None);
        assert_eq!(untold(&mut told, &[a2]), []);
        let b = fail(0, "Ctrl+Alt+F9", Cause::Unknown, None);
        let c = fail(1, "Ctrl+Alt+F9", Cause::InUse, None);
        let d = fail(0, "Ctrl+Alt+F10", Cause::InUse, None);
        assert_eq!(untold(&mut told, &[a, b.clone(), c.clone(), d.clone()]), [b, c, d]);
    }

    #[test]
    fn key_names_round_trip() {
        for vk in (0x30..=0x39).chain(0x41..=0x5A).chain(0x70..=0x7B) {
            let n = key_name(vk).unwrap();
            assert_eq!(key_vk(&n.to_ascii_lowercase()), Some(vk), "{n}");
        }
        for vk in [key::SPACE, key::RETURN, key::ESCAPE, key::DELETE, key::PRINTSCREEN, key::LEFT] {
            let n = key_name(vk).unwrap();
            assert_eq!(key_vk(&n.to_ascii_lowercase()), Some(vk), "{n}");
        }
    }
}
