//! Single-instance guard for the daemon. The first daemon becomes `Primary`
//! and listens for "capture" requests; later launches just signal it.

use crate::hotkey::HotEvent;
use std::sync::mpsc::Sender;

pub enum Instance {
    Primary(Guard),
    Signalled,
}

pub fn acquire_or_signal() -> Instance {
    imp::acquire_or_signal()
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

    pub struct Inner {
        mutex: HANDLE,
        thread_id: u32,
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
                if !self.mutex.is_invalid() {
                    let _ = CloseHandle(self.mutex);
                }
            }
            *TX.lock().unwrap() = None;
        }
    }

    unsafe extern "system" fn wndproc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        if m == WM_CAPTURE {
            if let Some(tx) = TX.lock().unwrap().as_ref() {
                let _ = tx.send(HotEvent::Capture);
            }
            return LRESULT(0);
        }
        unsafe { DefWindowProcW(h, m, w, l) }
    }

    /// Message-only window on its own thread; returns the thread id once created.
    fn spawn_window() -> Option<u32> {
        let (tx, rx) = mpsc::channel::<Option<u32>>();
        std::thread::spawn(move || unsafe {
            let hinst = GetModuleHandleW(PCWSTR::null()).unwrap_or_default();
            let mut wc: WNDCLASSEXW = std::mem::zeroed();
            wc.cbSize = std::mem::size_of::<WNDCLASSEXW>() as u32;
            wc.lpfnWndProc = Some(wndproc);
            wc.hInstance = hinst.into();
            wc.lpszClassName = w!("rustshot_tray");
            let _ = RegisterClassExW(&wc);
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("rustshot_tray"),
                w!("rustshot"),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
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
            let _ = DestroyWindow(hwnd);
        });
        rx.recv().ok().flatten()
    }

    pub fn acquire_or_signal() -> Instance {
        unsafe {
            let mutex = match CreateMutexW(None, true, w!("Local\\rustshot-daemon")) {
                Ok(h) => h,
                // Cannot tell; behave as the sole instance without a guard.
                Err(_) => {
                    return Instance::Primary(super::Guard(Inner { mutex: HANDLE::default(), thread_id: 0 }));
                }
            };
            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = CloseHandle(mutex);
                // The primary may still be creating its window; retry briefly.
                for _ in 0..20 {
                    if let Ok(hwnd) =
                        FindWindowExW(Some(HWND_MESSAGE), None, w!("rustshot_tray"), PCWSTR::null())
                    {
                        let _ = PostMessageW(Some(hwnd), WM_CAPTURE, WPARAM(0), LPARAM(0));
                        return Instance::Signalled;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                return Instance::Signalled;
            }
            let thread_id = spawn_window().unwrap_or(0);
            Instance::Primary(super::Guard(Inner { mutex, thread_id }))
        }
    }
}

#[cfg(unix)]
mod imp {
    use super::{HotEvent, Instance};
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::sync::mpsc::Sender;

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
        socket_path(
            std::env::var("XDG_RUNTIME_DIR").ok().as_deref(),
            std::env::var("TMPDIR").ok().as_deref(),
            uid,
            cfg!(target_os = "macos"),
        )
    }

    pub struct Inner {
        listener: UnixListener,
        path: PathBuf,
    }

    impl Inner {
        pub fn listen(&self, tx: Sender<HotEvent>) {
            let Ok(listener) = self.listener.try_clone() else { return };
            std::thread::spawn(move || {
                for conn in listener.incoming() {
                    let Ok(conn) = conn else { continue };
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

    fn signal(path: &Path) -> bool {
        UnixStream::connect(path).and_then(|mut s| s.write_all(b"capture\n")).is_ok()
    }

    pub fn acquire_at(path: &Path) -> Instance {
        for _ in 0..2 {
            if signal(path) {
                return Instance::Signalled;
            }
            // Nobody answered: any socket file is stale.
            let _ = std::fs::remove_file(path);
            if let Ok(listener) = UnixListener::bind(path) {
                return Instance::Primary(super::Guard(Inner { listener, path: path.to_path_buf() }));
            }
        }
        // Another instance won the bind race and is not answering yet.
        Instance::Signalled
    }

    pub fn acquire_or_signal() -> Instance {
        acquire_at(&default_path())
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
            let Instance::Primary(g) = acquire_at(&path) else { panic!("first must be primary") };
            let (tx, rx) = mpsc::channel();
            g.listen(tx);
            assert!(matches!(acquire_at(&path), Instance::Signalled));
            assert_eq!(rx.recv_timeout(Duration::from_secs(2)), Ok(HotEvent::Capture));
            drop(g);
            assert!(!path.exists());
            // A stale socket file is replaced.
            drop(UnixListener::bind(&path).unwrap());
            assert!(matches!(acquire_at(&path), Instance::Primary(_)));
            let _ = std::fs::remove_dir_all(&dir);
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
