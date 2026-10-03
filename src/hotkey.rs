use crate::config::Config;
use global_hotkey::hotkey::{Code, Modifiers};
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotEvent {
    Capture,
    Quit,
}

pub struct Hotkeys {
    mgr: Option<GlobalHotKeyManager>,
    capture: Option<HotKey>,
    quit: Option<HotKey>,
}

impl Hotkeys {
    pub fn new(cfg: &Config) -> Self {
        let mgr = match GlobalHotKeyManager::new() {
            Ok(m) => Some(m),
            Err(e) => {
                eprintln!("warning: global hotkeys unavailable: {e}");
                None
            }
        };
        let mut s = Self {
            mgr,
            capture: None,
            quit: None,
        };
        s.capture = s.register(&cfg.capture_hotkey, "capture");
        s.quit = s.register(&cfg.quit_hotkey, "quit");
        s
    }

    fn register(&self, spec: &str, what: &str) -> Option<HotKey> {
        let hk = parse_hotkey(spec)?;
        let mgr = self.mgr.as_ref()?;
        match mgr.register(hk) {
            Ok(()) => Some(hk),
            Err(e) => {
                eprintln!("warning: could not register {what} hotkey {spec:?}: {e}");
                None
            }
        }
    }

    pub fn poll(&self) -> Option<HotEvent> {
        let rx = GlobalHotKeyEvent::receiver();
        let mut out = None;
        while let Ok(ev) = rx.try_recv() {
            if self.capture.map(|h| h.id()) == Some(ev.id) {
                out = Some(HotEvent::Capture);
            } else if self.quit.map(|h| h.id()) == Some(ev.id) {
                out = Some(HotEvent::Quit);
            }
        }
        out
    }
}

/// Parse things like `Meta+Shift+X`, `Ctrl+Alt+Shift+Q`, `PrintScreen`.
/// Tries the crate parser first, then falls back to a small custom parser.
pub fn parse_hotkey(spec: &str) -> Option<HotKey> {
    if let Ok(hk) = spec.parse::<HotKey>() {
        return Some(hk);
    }
    let mut mods = Modifiers::empty();
    let mut code: Option<Code> = None;
    for part in spec.split('+').map(str::trim).filter(|p| !p.is_empty()) {
        let lower = part.to_ascii_lowercase();
        match lower.as_str() {
            "ctrl" | "control" => {
                mods |= Modifiers::CONTROL;
                continue;
            }
            "shift" => {
                mods |= Modifiers::SHIFT;
                continue;
            }
            "alt" => {
                mods |= Modifiers::ALT;
                continue;
            }
            "meta" | "win" | "super" | "cmd" => {
                mods |= Modifiers::META;
                continue;
            }
            _ => {}
        }
        code = Some(key_code(&lower)?);
    }
    let code = code?;
    Some(HotKey::new(Some(mods), code))
}

fn key_code(lower: &str) -> Option<Code> {
    if lower.len() == 1 {
        let c = lower.chars().next().unwrap();
        if c.is_ascii_lowercase() {
            // Code::KeyA..KeyZ are contiguous.
            let idx = (c as u8 - b'a') as usize;
            const LETTERS: [Code; 26] = [
                Code::KeyA, Code::KeyB, Code::KeyC, Code::KeyD, Code::KeyE, Code::KeyF, Code::KeyG,
                Code::KeyH, Code::KeyI, Code::KeyJ, Code::KeyK, Code::KeyL, Code::KeyM, Code::KeyN,
                Code::KeyO, Code::KeyP, Code::KeyQ, Code::KeyR, Code::KeyS, Code::KeyT, Code::KeyU,
                Code::KeyV, Code::KeyW, Code::KeyX, Code::KeyY, Code::KeyZ,
            ];
            return Some(LETTERS[idx]);
        }
        if c.is_ascii_digit() {
            let idx = (c as u8 - b'0') as usize;
            const DIGITS: [Code; 10] = [
                Code::Digit0, Code::Digit1, Code::Digit2, Code::Digit3, Code::Digit4, Code::Digit5,
                Code::Digit6, Code::Digit7, Code::Digit8, Code::Digit9,
            ];
            return Some(DIGITS[idx]);
        }
    }
    let code = match lower {
        "space" => Code::Space,
        "enter" | "return" => Code::Enter,
        "esc" | "escape" => Code::Escape,
        "tab" => Code::Tab,
        "backspace" => Code::Backspace,
        "delete" => Code::Delete,
        "insert" => Code::Insert,
        "home" => Code::Home,
        "end" => Code::End,
        "pageup" => Code::PageUp,
        "pagedown" => Code::PageDown,
        "up" => Code::ArrowUp,
        "down" => Code::ArrowDown,
        "left" => Code::ArrowLeft,
        "right" => Code::ArrowRight,
        "printscreen" | "prtsc" | "print" => Code::PrintScreen,
        "f1" => Code::F1,
        "f2" => Code::F2,
        "f3" => Code::F3,
        "f4" => Code::F4,
        "f5" => Code::F5,
        "f6" => Code::F6,
        "f7" => Code::F7,
        "f8" => Code::F8,
        "f9" => Code::F9,
        "f10" => Code::F10,
        "f11" => Code::F11,
        "f12" => Code::F12,
        _ => return None,
    };
    Some(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_custom_specs() {
        assert!(parse_hotkey("Meta+Shift+X").is_some());
        assert!(parse_hotkey("Ctrl+Alt+Shift+Q").is_some());
        assert!(parse_hotkey("PrintScreen").is_some());
        assert!(parse_hotkey("F5").is_some());
        assert!(parse_hotkey("bogus+key").is_none());
    }
}
