//! Win32 global hotkeys: `RegisterHotKey` on a dedicated message thread.

use super::*;

pub fn hotkey_thread(specs: [(i32, String, HotEvent); 2], tx: mpsc::Sender<HotEvent>) {
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

        let mut registered = Vec::new();
        for (id, spec, _) in &specs {
            match parse_hotkey(spec) {
                Some((mods, vk)) => {
                    let flags = HOT_KEY_MODIFIERS(mods) | MOD_NOREPEAT;
                    match RegisterHotKey(None, *id, flags, vk) {
                        Ok(()) => registered.push(*id),
                        Err(e) => {
                            eprintln!("warning: could not register hotkey {spec:?}: {e}")
                        }
                    }
                }
                None => eprintln!("warning: invalid hotkey {spec:?}"),
            }
        }

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
