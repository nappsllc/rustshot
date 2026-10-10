//! Win32 global hotkeys: `RegisterHotKey` on a dedicated message thread.

use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

/// A running registration thread.
pub struct Worker {
    join: std::thread::JoinHandle<()>,
    /// Its thread id once its message queue exists (0 before).
    tid: Arc<AtomicU32>,
}

impl Worker {
    /// Register `specs` on a new thread; `done` gets which registered.
    pub fn start(specs: [(i32, String, HotEvent); 2], tx: mpsc::Sender<HotEvent>, done: mpsc::Sender<Registered>) -> Worker {
        let tid = Arc::new(AtomicU32::new(0));
        let t = tid.clone();
        let join = std::thread::spawn(move || hotkey_thread(specs, tx, &t, done));
        Worker { join, tid }
    }

    /// End the thread (its hotkeys are unregistered) and wait for it.
    pub fn stop(self) {
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_QUIT};
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !self.join.is_finished() {
            let tid = self.tid.load(Ordering::SeqCst);
            if tid != 0 && unsafe { PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0)) }.is_ok() {
                break;
            }
            if std::time::Instant::now() >= deadline {
                eprintln!("warning: the hotkey thread did not stop");
                return; // leave it; the new registrations may then fail
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let _ = self.join.join();
    }
}

fn hotkey_thread(specs: [(i32, String, HotEvent); 2], tx: mpsc::Sender<HotEvent>, tid: &AtomicU32, done: mpsc::Sender<Registered>) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetMessageW, PeekMessageW, MSG, PM_NOREMOVE, WM_HOTKEY,
    };

    unsafe {
        // Ensure this thread has a message queue before RegisterHotKey.
        let mut bootstrap = MSG::default();
        let _ = PeekMessageW(&mut bootstrap, None, 0, 0, PM_NOREMOVE);
        tid.store(windows::Win32::System::Threading::GetCurrentThreadId(), Ordering::SeqCst);

        let mut registered = Vec::new();
        let mut result: Registered = [Ok(()); 2];
        for (i, (id, spec, _)) in specs.iter().enumerate() {
            if spec.trim().is_empty() {
                continue; // no hotkey for this action
            }
            match parse_hotkey(spec) {
                Some((mods, vk)) => {
                    let flags = HOT_KEY_MODIFIERS(mods) | MOD_NOREPEAT;
                    match RegisterHotKey(None, *id, flags, vk) {
                        Ok(()) => registered.push(*id),
                        Err(e) => {
                            // ERROR_HOTKEY_ALREADY_REGISTERED, in practice.
                            eprintln!("warning: could not register hotkey {spec:?}: {e}");
                            result[i] = Err(Cause::InUse);
                        }
                    }
                }
                None => {
                    eprintln!("warning: invalid hotkey {spec:?}");
                    result[i] = Err(Cause::Invalid);
                }
            }
        }
        let _ = done.send(result);

        loop {
            let mut msg = MSG::default();
            let got = GetMessageW(&mut msg, None, 0, 0);
            if !got.as_bool() {
                break; // 0 = WM_QUIT, negative = error
            }
            if msg.message == WM_HOTKEY {
                let which = msg.wParam.0 as i32;
                if let Some((_, _, ev)) = specs.iter().find(|(id, _, _)| *id == which)
                    && tx.send(ev.clone()).is_err()
                {
                    break; // receiver dropped
                }
            }
        }
        for id in registered {
            let _ = UnregisterHotKey(None, id);
        }
    }
}
