use crate::config::Config;
use crate::wind::key;
use std::sync::mpsc::{self, Receiver, Sender};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotEvent {
    Capture,
    Quit,
    /// An installed update is starting: quit once no capture is open.
    Restart,
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
        assert_eq!(parse_hotkey("Meta+Shift+X"), Some((0x0008 | 0x0004, 0x58)));
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
