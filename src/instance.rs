//! Single-instance guard for the daemon. The first daemon becomes `Primary`
//! and listens for "capture" requests; later launches just signal it.

use crate::hotkey::HotEvent;
use std::sync::mpsc::Sender;

pub enum Instance {
    Primary(Guard),
    Signalled,
    /// Single-instance protection is unavailable: run as the daemon without a guard.
    /// The payload keeps any held lock alive for the daemon's lifetime.
    Solo(#[allow(dead_code)] imp::Keep),
}

pub fn acquire_or_signal() -> Instance {
    imp::acquire_or_signal()
}

/// Become the primary without ever signalling: `None` while another daemon
/// holds the instance (used after an update, where the holder is the old
/// daemon and must not be asked to capture).
pub fn try_acquire() -> Option<Instance> {
    imp::try_acquire()
}

/// Env var naming a separate daemon instance (own mutex/window class or
/// socket), so tests can run a daemon beside the user's.
pub const INSTANCE_VAR: &str = "RUSTSHOT_INSTANCE";

/// A usable instance name: 1-32 of `[A-Za-z0-9_-]`.
pub fn valid_instance_name(s: &str) -> bool {
    (1..=32).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// `RUSTSHOT_INSTANCE`, if set to a valid name (read once).
pub fn instance_name() -> Option<&'static str> {
    static NAME: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    NAME.get_or_init(|| std::env::var(INSTANCE_VAR).ok().filter(|s| valid_instance_name(s))).as_deref()
}

/// The hidden daemon window's class: `rustshot_tray` (`rustshot_tray_<name>`
/// for a named instance). The installer looks for the default one.
#[cfg(windows)]
pub fn tray_class() -> windows::core::PCWSTR {
    static CLASS: std::sync::OnceLock<Vec<u16>> = std::sync::OnceLock::new();
    let w = CLASS.get_or_init(|| {
        let s = match instance_name() {
            Some(n) => format!("rustshot_tray_{n}"),
            None => "rustshot_tray".into(),
        };
        s.encode_utf16().chain(Some(0)).collect()
    });
    windows::core::PCWSTR(w.as_ptr())
}

/// Held for the daemon's lifetime.
pub struct Guard(imp::Inner);

impl Guard {
    /// Start delivering `HotEvent::Capture` to `tx` whenever another launch signals us.
    pub fn listen(&self, tx: Sender<HotEvent>) {
        self.0.listen(tx);
    }
}

#[cfg(windows)]
mod imp {
    use super::{HotEvent, Instance};
    use std::sync::Mutex;
    use std::sync::mpsc::{self, Sender};
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND, LPARAM, LRESULT, WPARAM,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Threading::{CreateMutexW, GetCurrentThreadId};
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::core::{PCWSTR, w};

    /// Posted to the tray window: "capture now".
    pub const WM_CAPTURE: u32 = WM_APP + 7;

    static TX: Mutex<Option<Sender<HotEvent>>> = Mutex::new(None);

    /// Nothing to keep alive on Windows.
    pub struct Keep;

    pub struct Inner {
        mutex: HANDLE,
        thread_id: u32,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Inner {
        pub fn listen(&self, tx: Sender<HotEvent>) {
            *TX.lock().unwrap() = Some(tx);
        }
    }

    impl Drop for Inner {
        fn drop(&mut self) {
            unsafe {
                if self.thread_id != 0 {
                    let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
                }
                // Wait for the window thread so the tray icon is removed before we exit.
                if let Some(t) = self.thread.take() {
                    let _ = t.join();
                }
                if !self.mutex.is_invalid() {
                    let _ = CloseHandle(self.mutex);
                }
            }
            *TX.lock().unwrap() = None;
            crate::tray_win::clear();
        }
    }

    unsafe extern "system" fn wndproc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        if let Some(r) = crate::tray_win::handle(h, m, w, l) {
            return r;
        }
        if m == WM_CAPTURE {
            if let Some(tx) = TX.lock().unwrap().as_ref() {
                let _ = tx.send(HotEvent::Capture);
            }
            return LRESULT(0);
        }
        unsafe { DefWindowProcW(h, m, w, l) }
    }

    /// Hidden top-level window on its own thread; returns the thread id once created.
    fn spawn_window() -> Option<(u32, std::thread::JoinHandle<()>)> {
        let (tx, rx) = mpsc::channel::<Option<u32>>();
        let handle = std::thread::spawn(move || unsafe {
            let hinst = GetModuleHandleW(PCWSTR::null()).unwrap_or_default();
            let mut wc: WNDCLASSEXW = std::mem::zeroed();
            wc.cbSize = std::mem::size_of::<WNDCLASSEXW>() as u32;
            wc.lpfnWndProc = Some(wndproc);
            wc.hInstance = hinst.into();
            wc.lpszClassName = super::tray_class();
            let _ = RegisterClassExW(&wc);
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW,
                super::tray_class(),
                w!("rustshot"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(hinst.into()),
                None,
            );
            let Ok(hwnd) = hwnd else {
                let _ = tx.send(None);
                return;
            };
            let _ = tx.send(Some(GetCurrentThreadId()));
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            crate::tray_win::remove(hwnd);
            let _ = DestroyWindow(hwnd);
        });
        let id = rx.recv().ok().flatten()?;
        Some((id, handle))
    }

    pub fn acquire_or_signal() -> Instance {
        acquire(true).expect("signal mode always decides")
    }

    pub fn try_acquire() -> Option<Instance> {
        acquire(false)
    }

    /// `signal`: when the mutex is held, signal its owner (`Signalled`);
    /// otherwise return `None` without signalling.
    fn acquire(signal: bool) -> Option<Instance> {
        unsafe {
            let name: Vec<u16> = match super::instance_name() {
                Some(n) => format!("Local\\rustshot-daemon-{n}"),
                None => "Local\\rustshot-daemon".into(),
            }
            .encode_utf16()
            .chain(Some(0))
            .collect();
            let mutex = match CreateMutexW(None, true, PCWSTR(name.as_ptr())) {
                Ok(h) => h,
                // Cannot tell; behave as the sole instance without a guard.
                Err(_) => return Some(Instance::Solo(Keep)),
            };
            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = CloseHandle(mutex);
                if !signal {
                    return None;
                }
                // The primary may still be creating its window; retry briefly.
                for _ in 0..20 {
                    if let Ok(hwnd) = FindWindowW(super::tray_class(), PCWSTR::null()) {
                        let _ = PostMessageW(Some(hwnd), WM_CAPTURE, WPARAM(0), LPARAM(0));
                        return Some(Instance::Signalled);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                eprintln!("error: another rustshot is starting but not responding");
                std::process::exit(1);
            }
            let Some((thread_id, thread)) = spawn_window() else {
                eprintln!(
                    "warning: could not create the tray window; running without tray or single-instance signalling"
                );
                let _ = CloseHandle(mutex);
                return Some(Instance::Solo(Keep));
            };
            Some(Instance::Primary(super::Guard(Inner { mutex, thread_id, thread: Some(thread) })))
        }
    }
}

#[cfg(unix)]
mod imp {
    use super::{HotEvent, Instance};
    use std::fs::{File, OpenOptions};
    use std::io::{BufRead, BufReader, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::sync::mpsc::Sender;
    use std::time::Duration;

    /// Pure path logic: Linux prefers `$XDG_RUNTIME_DIR/rustshot.sock`, otherwise
    /// `/tmp/rustshot-<uid>.sock`; macOS uses `$TMPDIR/rustshot-<uid>.sock`.
    pub fn socket_path(xdg_runtime: Option<&str>, tmpdir: Option<&str>, uid: u32, macos: bool) -> PathBuf {
        let non_empty = |s: Option<&str>| s.filter(|s| !s.is_empty()).map(PathBuf::from);
        if !macos && let Some(dir) = non_empty(xdg_runtime) {
            return dir.join("rustshot.sock");
        }
        let base = if macos { non_empty(tmpdir) } else { None }.unwrap_or_else(|| PathBuf::from("/tmp"));
        base.join(format!("rustshot-{uid}.sock"))
    }

    fn default_path() -> PathBuf {
        unsafe extern "C" {
            fn getuid() -> u32;
        }
        let uid = unsafe { getuid() };
        let p = socket_path(
            std::env::var("XDG_RUNTIME_DIR").ok().as_deref(),
            std::env::var("TMPDIR").ok().as_deref(),
            uid,
            cfg!(target_os = "macos"),
        );
        match super::instance_name() {
            Some(n) => named(&p, n),
            None => p,
        }
    }

    /// `<dir>/<stem>-<name>.sock` for a named instance.
    pub fn named(sock: &Path, name: &str) -> PathBuf {
        let stem = sock.file_stem().unwrap_or_default().to_string_lossy();
        sock.with_file_name(format!("{stem}-{name}.sock"))
    }

    /// Lock file kept open (and locked) by a Solo daemon.
    pub struct Keep(#[allow(dead_code)] Option<File>);

    pub struct Inner {
        listener: UnixListener,
        path: PathBuf,
        /// Holds the exclusive flock for the daemon's lifetime.
        _lock: File,
    }

    impl Inner {
        pub fn listen(&self, tx: Sender<HotEvent>) {
            let Ok(listener) = self.listener.try_clone() else { return };
            std::thread::spawn(move || {
                for conn in listener.incoming() {
                    let conn = match conn {
                        Ok(c) => c,
                        Err(_) => {
                            std::thread::sleep(Duration::from_millis(100));
                            continue;
                        }
                    };
                    // A silent client must not block later signals.
                    let _ = conn.set_read_timeout(Some(Duration::from_secs(1)));
                    let mut line = String::new();
                    if BufReader::new(conn).read_line(&mut line).is_ok()
                        && line.trim() == "capture"
                        && tx.send(HotEvent::Capture).is_err()
                    {
                        break;
                    }
                }
            });
        }
    }

    impl Drop for Inner {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    unsafe extern "C" {
        fn flock(fd: i32, op: i32) -> i32;
    }
    const LOCK_EX_NB: i32 = 2 | 4;

    /// Sibling lock file: `<socket path>.lock`.
    pub fn lock_path(sock: &Path) -> PathBuf {
        let mut s = sock.as_os_str().to_owned();
        s.push(".lock");
        PathBuf::from(s)
    }

    enum Lock {
        Held(File),
        Busy,
        Unavailable(std::io::Error),
    }

    fn try_lock(path: &Path) -> Lock {
        let file = match OpenOptions::new().create(true).truncate(false).write(true).mode(0o600).open(path) {
            Ok(f) => f,
            Err(e) => return Lock::Unavailable(e),
        };
        if unsafe { flock(file.as_raw_fd(), LOCK_EX_NB) } == 0 {
            return Lock::Held(file);
        }
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::WouldBlock {
            Lock::Busy
        } else {
            Lock::Unavailable(err)
        }
    }

    fn signal(path: &Path) -> bool {
        UnixStream::connect(path).and_then(|mut s| s.write_all(b"capture\n")).is_ok()
    }

    fn solo(err: impl std::fmt::Display, lock: Option<File>) -> Option<Instance> {
        eprintln!("warning: single-instance socket unavailable ({err}); running without it");
        Some(Instance::Solo(Keep(lock)))
    }

    /// `None` = another primary exists but could not be signalled.
    pub fn acquire_at(path: &Path) -> Option<Instance> {
        acquire_mode(path, true)
    }

    /// `may_signal` false: `None` whenever another primary holds the lock, and
    /// it is never signalled.
    fn acquire_mode(path: &Path, may_signal: bool) -> Option<Instance> {
        match try_lock(&lock_path(path)) {
            Lock::Held(lock) => {
                // We are the primary: any socket file is stale.
                let _ = std::fs::remove_file(path);
                match UnixListener::bind(path) {
                    Ok(listener) => {
                        Some(Instance::Primary(super::Guard(Inner { listener, path: path.to_path_buf(), _lock: lock })))
                    }
                    Err(e) => solo(e, Some(lock)),
                }
            }
            Lock::Busy if !may_signal => None,
            Lock::Busy => {
                // The primary may still be binding; retry briefly.
                for _ in 0..20 {
                    if signal(path) {
                        return Some(Instance::Signalled);
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                None
            }
            Lock::Unavailable(e) => solo(e, None),
        }
    }

    pub fn try_acquire() -> Option<Instance> {
        acquire_mode(&default_path(), false)
    }

    /// `try_acquire` on an explicit socket path (tests).
    #[cfg(test)]
    pub fn try_acquire_at(path: &Path) -> Option<Instance> {
        acquire_mode(path, false)
    }

    pub fn acquire_or_signal() -> Instance {
        acquire_at(&default_path()).unwrap_or_else(|| {
            eprintln!("error: another rustshot is running but not responding");
            std::process::exit(1);
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::sync::mpsc;
        use std::time::Duration;

        #[test]
        fn path_logic() {
            assert_eq!(
                socket_path(Some("/run/user/1000"), None, 1000, false),
                PathBuf::from("/run/user/1000/rustshot.sock")
            );
            assert_eq!(socket_path(None, None, 1000, false), PathBuf::from("/tmp/rustshot-1000.sock"));
            assert_eq!(socket_path(Some(""), None, 7, false), PathBuf::from("/tmp/rustshot-7.sock"));
            assert_eq!(
                socket_path(Some("/run/user/1"), Some("/var/T/"), 501, true),
                PathBuf::from("/var/T/rustshot-501.sock")
            );
            assert_eq!(socket_path(None, None, 501, true), PathBuf::from("/tmp/rustshot-501.sock"));
        }

        #[test]
        fn signal_round_trip() {
            let dir = std::env::temp_dir().join(format!("rustshot-inst-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("t.sock");
            let _ = std::fs::remove_file(&path);
            let Some(Instance::Primary(g)) = acquire_at(&path) else { panic!("first must be primary") };
            let (tx, rx) = mpsc::channel();
            g.listen(tx);
            assert!(matches!(acquire_at(&path), Some(Instance::Signalled)));
            assert_eq!(rx.recv_timeout(Duration::from_secs(2)), Ok(HotEvent::Capture));
            drop(g);
            assert!(!path.exists());
            // A stale socket file is replaced.
            drop(UnixListener::bind(&path).unwrap());
            assert!(matches!(acquire_at(&path), Some(Instance::Primary(_))));
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn try_acquire_never_signals_a_held_instance() {
            let dir = std::env::temp_dir().join(format!("rustshot-tryacq-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("t.sock");
            let _ = std::fs::remove_file(&path);
            let Some(Instance::Primary(g)) = try_acquire_at(&path) else { panic!("free: must be primary") };
            let (tx, rx) = mpsc::channel();
            g.listen(tx);
            assert!(try_acquire_at(&path).is_none(), "held: must not become primary or signal");
            assert!(rx.recv_timeout(Duration::from_millis(300)).is_err(), "the holder was signalled");
            drop(g);
            assert!(matches!(try_acquire_at(&path), Some(Instance::Primary(_))));
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn named_instance_socket() {
            assert_eq!(named(Path::new("/run/u/rustshot.sock"), "t1"), PathBuf::from("/run/u/rustshot-t1.sock"));
            assert_eq!(named(Path::new("/tmp/rustshot-7.sock"), "x"), PathBuf::from("/tmp/rustshot-7-x.sock"));
        }

        #[test]
        fn lock_path_is_sibling() {
            assert_eq!(lock_path(Path::new("/run/u/rustshot.sock")), PathBuf::from("/run/u/rustshot.sock.lock"));
        }

        #[test]
        fn held_lock_means_signal_not_second_primary() {
            let dir = std::env::temp_dir().join(format!("rustshot-flock-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("t.sock");
            let Some(Instance::Primary(g)) = acquire_at(&path) else { panic!("first must be primary") };
            let (tx, rx) = mpsc::channel();
            g.listen(tx);
            // The lock is held, so a second acquire must take the signal path.
            assert!(matches!(try_lock(&lock_path(&path)), Lock::Busy));
            assert!(matches!(acquire_at(&path), Some(Instance::Signalled)));
            assert_eq!(rx.recv_timeout(Duration::from_secs(2)), Ok(HotEvent::Capture));
            drop(g);
            // Released with the guard: a new primary can start.
            assert!(matches!(acquire_at(&path), Some(Instance::Primary(_))));
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn bind_failure_is_solo() {
            let dir = std::env::temp_dir().join(format!("rustshot-solo-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            // Path longer than sun_path: lock file opens fine, bind fails.
            let long = dir.join("x".repeat(200)).join("s.sock");
            std::fs::create_dir_all(long.parent().unwrap()).unwrap();
            assert!(matches!(acquire_at(&long), Some(Instance::Solo(_))));
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

#[cfg(test)]
mod name_tests {
    use super::valid_instance_name;

    #[test]
    fn instance_names() {
        assert!(valid_instance_name("e2e-123_x"));
        for bad in ["", "a b", "a\\b", "a/b", "ü", &"x".repeat(33)] {
            assert!(!valid_instance_name(bad), "{bad:?}");
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    #[ignore = "uses the real per-user daemon mutex; fails if a daemon is running"]
    fn second_acquire_is_signalled() {
        let Instance::Primary(g) = acquire_or_signal() else { panic!("first must be primary") };
        let (tx, rx) = std::sync::mpsc::channel();
        g.listen(tx);
        assert!(matches!(acquire_or_signal(), Instance::Signalled));
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(2)), Ok(HotEvent::Capture));
    }
}
