//! "Start at login" for the tray daemon, per OS (see the plan's autostart locations).
#![cfg_attr(not(windows), allow(dead_code))] // used by the macOS/Linux trays (later tasks)

use std::path::Path;

pub const LABEL: &str = "io.github.nappsllc.rustshot";

/// Windows Run-key value data: `"<exe>" daemon`.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn run_command(exe: &Path) -> String {
    format!("\"{}\" daemon", exe.display())
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// macOS LaunchAgent plist: `ProgramArguments` = [exe, "daemon"], `RunAtLoad`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn launch_agent_plist(exe: &Path) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n\
<dict>\n\
\t<key>Label</key>\n\
\t<string>{LABEL}</string>\n\
\t<key>ProgramArguments</key>\n\
\t<array>\n\
\t\t<string>{}</string>\n\
\t\t<string>daemon</string>\n\
\t</array>\n\
\t<key>RunAtLoad</key>\n\
\t<true/>\n\
</dict>\n\
</plist>\n",
        xml_escape(&exe.display().to_string())
    )
}

/// Linux XDG autostart entry (`Exec=<exe> daemon`, quoted per the Desktop Entry spec).
#[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code))]
pub fn autostart_desktop(exe: &Path) -> String {
    // `%` starts a field code in Exec, so it is always doubled.
    let p = exe.display().to_string().replace('%', "%%");
    let exec = if p.chars().any(|c| c.is_whitespace() || "\"'\\><~|&;$*?#()`".contains(c)) {
        // Quoted argument: backslash-escape `"`, `` ` ``, `$`, `\`; the value is also a
        // string, so every backslash is doubled again (`\` -> 4, `"` -> `\\"`).
        let mut q = String::from("\"");
        for c in p.chars() {
            match c {
                '\\' => q.push_str("\\\\\\\\"),
                '"' | '`' | '$' => {
                    q.push_str("\\\\");
                    q.push(c);
                }
                _ => q.push(c),
            }
        }
        q.push('"');
        q
    } else {
        p
    };
    format!(
        "[Desktop Entry]\nType=Application\nName=rustshot\nComment=Screenshot and annotation tool\n\
Exec={exec} daemon\nIcon={LABEL}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n"
    )
}

/// Autostart file location (macOS plist / Linux .desktop) from the relevant env values.
#[cfg_attr(windows, allow(dead_code))]
pub fn autostart_file(home: Option<&str>, xdg_config: Option<&str>, macos: bool) -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    let ne = |s: Option<&str>| s.filter(|s| !s.is_empty()).map(PathBuf::from);
    if macos {
        Some(ne(home)?.join("Library/LaunchAgents").join(format!("{LABEL}.plist")))
    } else {
        let base = ne(xdg_config).or_else(|| ne(home).map(|h| h.join(".config")))?;
        Some(base.join("autostart").join(format!("{LABEL}.desktop")))
    }
}

pub fn is_enabled() -> bool {
    imp::is_enabled()
}

pub fn set(on: bool) -> Result<(), String> {
    imp::set(on)
}

#[cfg(windows)]
mod imp {
    use std::path::Path;
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows::Win32::System::Registry::*;
    use windows::core::{HSTRING, PCWSTR, w};

    const VALUE: &str = "rustshot";
    const SUBKEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");

    pub fn is_enabled() -> bool {
        is_enabled_named(VALUE)
    }

    pub fn set(on: bool) -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        set_named(VALUE, on, &exe)
    }

    pub fn is_enabled_named(name: &str) -> bool {
        unsafe {
            RegGetValueW(HKEY_CURRENT_USER, SUBKEY, &HSTRING::from(name), RRF_RT_REG_SZ, None, None, None)
                == ERROR_SUCCESS
        }
    }

    pub fn set_named(name: &str, on: bool, exe: &Path) -> Result<(), String> {
        unsafe {
            if on {
                let data: Vec<u16> = super::run_command(exe).encode_utf16().chain(Some(0)).collect();
                let r = RegSetKeyValueW(
                    HKEY_CURRENT_USER,
                    SUBKEY,
                    &HSTRING::from(name),
                    REG_SZ.0,
                    Some(data.as_ptr() as *const _),
                    (data.len() * 2) as u32,
                );
                if r != ERROR_SUCCESS {
                    return Err(format!("registry write failed ({})", r.0));
                }
            } else {
                let r = RegDeleteKeyValueW(HKEY_CURRENT_USER, SUBKEY, &HSTRING::from(name));
                if r != ERROR_SUCCESS && r != ERROR_FILE_NOT_FOUND {
                    return Err(format!("registry delete failed ({})", r.0));
                }
            }
            Ok(())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn toggle_temp_value() {
            let name = format!("rustshot-test-{}", std::process::id());
            let exe = Path::new("C:\\Program Files\\rustshot\\rustshot.exe");
            assert!(!is_enabled_named(&name));
            set_named(&name, true, exe).unwrap();
            assert!(is_enabled_named(&name));
            set_named(&name, false, exe).unwrap();
            assert!(!is_enabled_named(&name));
            // Deleting a missing value is fine.
            set_named(&name, false, exe).unwrap();
        }
    }
}

#[cfg(unix)]
mod imp {
    pub fn is_enabled() -> bool {
        path().is_some_and(|p| p.exists())
    }

    fn path() -> Option<std::path::PathBuf> {
        super::autostart_file(
            std::env::var("HOME").ok().as_deref(),
            std::env::var("XDG_CONFIG_HOME").ok().as_deref(),
            cfg!(target_os = "macos"),
        )
    }

    pub fn set(on: bool) -> Result<(), String> {
        let p = path().ok_or("cannot locate the autostart directory")?;
        if on {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let text = if cfg!(target_os = "macos") {
                super::launch_agent_plist(&exe)
            } else {
                super::autostart_desktop(&exe)
            };
            if let Some(dir) = p.parent() {
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
            std::fs::write(&p, text).map_err(|e| e.to_string())
        } else {
            match std::fs::remove_file(&p) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
                _ => Ok(()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn run_command_quotes_exe() {
        assert_eq!(run_command(Path::new("C:\\a b\\rustshot.exe")), "\"C:\\a b\\rustshot.exe\" daemon");
    }

    #[test]
    fn plist_contents() {
        let p = launch_agent_plist(Path::new("/Applications/rust & shot.app/Contents/MacOS/rustshot"));
        assert!(p.contains("<string>io.github.nappsllc.rustshot</string>"));
        assert!(p.contains("<string>/Applications/rust &amp; shot.app/Contents/MacOS/rustshot</string>"));
        assert!(p.contains("<string>daemon</string>"));
        assert!(p.contains("<key>RunAtLoad</key>\n\t<true/>"));
    }

    #[test]
    fn desktop_contents() {
        let d = autostart_desktop(Path::new("/usr/bin/rustshot"));
        assert!(d.contains("Exec=/usr/bin/rustshot daemon\n"));
        assert!(d.starts_with("[Desktop Entry]\n"));
        let q = autostart_desktop(Path::new("/opt/my apps/rustshot"));
        assert!(q.contains("Exec=\"/opt/my apps/rustshot\" daemon\n"));
        let t = autostart_desktop(Path::new("/o p/100%/a\\b"));
        assert!(t.contains("Exec=\"/o p/100%%/a\\\\\\\\b\" daemon\n"), "{t}");
    }

    #[test]
    fn file_locations() {
        assert_eq!(
            autostart_file(Some("/Users/a"), None, true),
            Some(PathBuf::from("/Users/a/Library/LaunchAgents/io.github.nappsllc.rustshot.plist"))
        );
        assert_eq!(
            autostart_file(Some("/home/a"), Some("/cfg"), false),
            Some(PathBuf::from("/cfg/autostart/io.github.nappsllc.rustshot.desktop"))
        );
        assert_eq!(
            autostart_file(Some("/home/a"), Some(""), false),
            Some(PathBuf::from("/home/a/.config/autostart/io.github.nappsllc.rustshot.desktop"))
        );
        assert_eq!(autostart_file(None, None, false), None);
    }
}
