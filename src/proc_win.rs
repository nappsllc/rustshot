//! Win32 process helpers built on `CreateProcessW` directly (std's `Command`
//! costs ~27 KB of exe size).

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, CreateProcessW, DETACHED_PROCESS, OpenProcess, PROCESS_INFORMATION,
    PROCESS_SYNCHRONIZE, STARTUPINFOW, WaitForSingleObject,
};
use windows::core::{PCWSTR, PWSTR};

const SPACE: u16 = b' ' as u16;
const QUOTE: u16 = b'"' as u16;
const BACKSLASH: u16 = b'\\' as u16;

/// Append `arg` to a command line, quoted so `CommandLineToArgvW` (and the
/// MSVC CRT) parse it back unchanged.
pub fn push_arg(cmd: &mut Vec<u16>, arg: &OsStr) {
    if !cmd.is_empty() {
        cmd.push(SPACE);
    }
    let w: Vec<u16> = arg.encode_wide().collect();
    if !w.is_empty() && !w.iter().any(|&c| matches!(c, 0x20 | 0x09 | 0x0a | 0x0b | QUOTE)) {
        cmd.extend(w);
        return;
    }
    cmd.push(QUOTE);
    let mut backslashes = 0;
    for c in w {
        if c == BACKSLASH {
            backslashes += 1;
        } else {
            if c == QUOTE {
                // Backslashes before a quote are doubled, plus one for the quote.
                cmd.extend(std::iter::repeat_n(BACKSLASH, backslashes + 1));
            }
            backslashes = 0;
        }
        cmd.push(c);
    }
    // Trailing backslashes are doubled so the closing quote stays a quote.
    cmd.extend(std::iter::repeat_n(BACKSLASH, backslashes));
    cmd.push(QUOTE);
}

/// The full command line (NUL-terminated) for `exe` and `args`. The program
/// name is always quoted: it is parsed without escapes, and a path cannot
/// contain `"`.
pub fn command_line(exe: &Path, args: &[&OsStr]) -> Result<Vec<u16>, String> {
    let w: Vec<u16> = exe.as_os_str().encode_wide().collect();
    if w.is_empty() || w.contains(&QUOTE) || w.contains(&0) {
        return Err(format!("invalid program path {}", exe.display()));
    }
    let mut cmd = Vec::with_capacity(w.len() + 3);
    cmd.push(QUOTE);
    cmd.extend(w);
    cmd.push(QUOTE);
    for a in args {
        if a.encode_wide().any(|c| c == 0) {
            return Err("argument contains NUL".into());
        }
        push_arg(&mut cmd, a);
    }
    cmd.push(0);
    Ok(cmd)
}

/// Start `exe args...` detached from us (no console, own process group, no
/// inherited handles; the environment is inherited) and forget it. `exe`
/// must be an absolute path: it is passed as the application name, so no
/// search path (and no current directory) is involved.
pub fn spawn_detached(exe: &Path, args: &[&OsStr]) -> Result<(), String> {
    if !exe.is_absolute() {
        return Err(format!("not an absolute path: {}", exe.display()));
    }
    let mut cmd = command_line(exe, args)?;
    let app: Vec<u16> = exe.as_os_str().encode_wide().chain(Some(0)).collect();
    let si = STARTUPINFOW { cb: std::mem::size_of::<STARTUPINFOW>() as u32, ..Default::default() };
    let mut pi = PROCESS_INFORMATION::default();
    unsafe {
        CreateProcessW(
            PCWSTR(app.as_ptr()),
            Some(PWSTR(cmd.as_mut_ptr())),
            None,
            None,
            false,
            DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP,
            None,
            PCWSTR::null(),
            &si,
            &mut pi,
        )
        .map_err(|e| format!("cannot start {}: {e}", exe.display()))?;
        let _ = CloseHandle(pi.hThread);
        let _ = CloseHandle(pi.hProcess);
    }
    Ok(())
}

/// `%SystemRoot%\System32\<name>` (from `GetSystemDirectoryW`).
pub fn system_exe(name: &str) -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows::Win32::System::SystemInformation::GetSystemDirectoryW;
    let mut buf = [0u16; 260];
    let n = unsafe { GetSystemDirectoryW(Some(&mut buf)) } as usize;
    (n > 0 && n < buf.len()).then(|| PathBuf::from(std::ffi::OsString::from_wide(&buf[..n])).join(name))
}

/// Wait up to `timeout_ms` for process `pid` to exit. Returns at once when
/// the process does not exist or cannot be opened.
pub fn wait_pid(pid: u32, timeout_ms: u32) {
    unsafe {
        if let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            let _ = WaitForSingleObject(h, timeout_ms);
            let _ = CloseHandle(h);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::UI::Shell::CommandLineToArgvW;

    fn parse(cmd: &[u16]) -> Vec<OsString> {
        unsafe {
            let mut n = 0;
            let argv = CommandLineToArgvW(PCWSTR(cmd.as_ptr()), &mut n);
            assert!(!argv.is_null());
            let v = (0..n as usize).map(|i| OsString::from_wide((*argv.add(i)).as_wide())).collect();
            let _ = LocalFree(Some(HLOCAL(argv as _)));
            v
        }
    }

    #[test]
    fn quoting_round_trips_through_command_line_to_argv() {
        let args = [
            "daemon",
            "",
            "with space",
            "tab\there",
            "quote\"inside",
            "trailing\\",
            "trailing space\\",
            "\\\\server\\share\\",
            "back\\\\\"quote",
            "\"",
            "\\\"",
            "/S",
            "/RELAUNCH",
            "ünïcödé ✓",
        ];
        let exe = Path::new(r"C:\Program Files\rust shot\rustshot.exe");
        let os: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
        let cmd = command_line(exe, &os).unwrap();
        let got = parse(&cmd);
        assert_eq!(got[0], exe.as_os_str());
        let want: Vec<OsString> = args.iter().map(OsString::from).collect();
        assert_eq!(&got[1..], &want[..]);
    }

    #[test]
    fn simple_args_are_not_quoted() {
        let cmd = command_line(Path::new(r"C:\a\b.exe"), &[OsStr::new("daemon"), OsStr::new("/S")]).unwrap();
        assert_eq!(String::from_utf16(&cmd[..cmd.len() - 1]).unwrap(), r#""C:\a\b.exe" daemon /S"#);
    }

    #[test]
    fn bad_paths_are_refused() {
        assert!(command_line(Path::new("a\"b.exe"), &[]).is_err());
        assert!(command_line(Path::new(""), &[]).is_err());
        assert!(command_line(Path::new(r"C:\a.exe"), &[OsStr::new("x\0y")]).is_err());
        assert!(spawn_detached(Path::new("cmd.exe"), &[]).is_err(), "relative paths are refused");
    }

    #[test]
    fn spawns_and_waits() {
        // `cmd /c exit` starts and ends at once; wait_pid on a gone (or never
        // valid) pid returns promptly.
        let cmd = system_exe("cmd.exe").unwrap();
        assert!(cmd.is_file());
        spawn_detached(&cmd, &[OsStr::new("/c"), OsStr::new("exit 0")]).unwrap();
        let t = std::time::Instant::now();
        wait_pid(0, 10_000);
        wait_pid(u32::MAX - 2, 10_000);
        assert!(t.elapsed() < std::time::Duration::from_secs(2));
        assert!(spawn_detached(&cmd.with_file_name("rustshot-no-such.exe"), &[]).is_err());
    }
}
