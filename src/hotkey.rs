use crate::config::Config;
use crate::wind::key;
use std::sync::mpsc::{self, Receiver, Sender};

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
    tx: Sender<HotEvent>,
}

impl Hotkeys {
    pub fn new(cfg: &Config) -> Self {
        let (tx, rx) = mpsc::channel();
        let hot_tx = tx.clone();
        let specs = [
            (1i32, cfg.capture_hotkey.clone(), HotEvent::Capture),
            (2i32, cfg.quit_hotkey.clone(), HotEvent::Quit),
        ];
        std::thread::spawn(move || imp::hotkey_thread(specs, hot_tx));
        Self { rx, tx }
    }

    /// Another producer of events (single-instance listener, tray).
    pub fn sender(&self) -> Sender<HotEvent> {
        self.tx.clone()
    }

    pub fn poll(&self) -> Option<HotEvent> {
        let mut out = None;
        while let Ok(ev) = self.rx.try_recv() {
            out = Some(ev);
        }
        out
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
        "space" => key::SPACE,
        "enter" | "return" => key::RETURN,
        "esc" | "escape" => key::ESCAPE,
        "tab" => key::TAB,
        "backspace" => key::BACK,
        "delete" => key::DELETE,
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
