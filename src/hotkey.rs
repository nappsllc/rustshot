use crate::config::Config;
use std::sync::mpsc::{self, Receiver};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotEvent {
    Capture,
    Quit,
}

/// Global hotkeys backed by Win32 `RegisterHotKey` (replaces the
/// `global-hotkey` crate). Registration and the message loop run on a
/// dedicated thread so we own the queue instead of relying on any toolkit.
pub struct Hotkeys {
    rx: Receiver<HotEvent>,
}

impl Hotkeys {
    pub fn new(cfg: &Config) -> Self {
        let (tx, rx) = mpsc::channel();
        let specs = [
            (1i32, cfg.capture_hotkey.clone(), HotEvent::Capture),
            (2i32, cfg.quit_hotkey.clone(), HotEvent::Quit),
        ];
        std::thread::spawn(move || hotkey_thread(specs, tx));
        Self { rx }
    }

    pub fn poll(&self) -> Option<HotEvent> {
        let mut out = None;
        while let Ok(ev) = self.rx.try_recv() {
            out = Some(ev);
        }
        out
    }
}

fn hotkey_thread(specs: [(i32, String, HotEvent); 2], tx: mpsc::Sender<HotEvent>) {
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
                    && tx.send(*ev).is_err()
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

/// Parse things like `Meta+Shift+X`, `Ctrl+Alt+Shift+Q`, `PrintScreen`.
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
        vk = Some(key_vk(&lower)?);
    }
    Some((mods, vk?))
}

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
        "space" => 0x20,
        "enter" | "return" => 0x0D,
        "esc" | "escape" => 0x1B,
        "tab" => 0x09,
        "backspace" => 0x08,
        "delete" => 0x2E,
        "insert" => 0x2D,
        "home" => 0x24,
        "end" => 0x23,
        "pageup" => 0x21,
        "pagedown" => 0x22,
        "up" => 0x26,
        "down" => 0x28,
        "left" => 0x25,
        "right" => 0x27,
        "printscreen" | "prtsc" | "print" => 0x2C,
        _ => return None,
    };
    Some(vk)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_custom_specs() {
        assert_eq!(parse_hotkey("Meta+Shift+X"), Some((0x0008 | 0x0004, 0x58)));
        assert_eq!(
            parse_hotkey("Ctrl+Alt+Shift+Q"),
            Some((0x0002 | 0x0001 | 0x0004, 0x51))
        );
        assert_eq!(parse_hotkey("PrintScreen"), Some((0, 0x2C)));
        assert_eq!(parse_hotkey("F5"), Some((0, 0x74)));
        assert_eq!(parse_hotkey("bogus+key"), None);
        assert_eq!(parse_hotkey(""), None);
    }
}
