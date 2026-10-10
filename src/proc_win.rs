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

/// A `CreateProcessW` environment block (`CREATE_UNICODE_ENVIRONMENT`):
/// `base` with `extra` added (replacing names that match case-insensitively),
/// as `NAME=VALUE\0` entries sorted by name (case-insensitive, as Windows
/// requires) and ended by an extra NUL.
pub fn env_block<I>(base: I, extra: &[(&OsStr, &OsStr)]) -> Result<Vec<u16>, String>
where
    I: IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
{
    let wide = |s: &OsStr| s.encode_wide().collect::<Vec<u16>>();
    let upper = |k: &[u16]| k.iter().map(|&c| if (0x61..=0x7a).contains(&c) { c - 0x20 } else { c }).collect::<Vec<u16>>();
    let mut vars: Vec<(Vec<u16>, Vec<u16>)> = Vec::new();
    for (k, v) in extra {
        let (k, v) = (wide(k), wide(v));
        // A leading '=' is allowed (the hidden per-drive "=C:" entries).
        if k.is_empty() || k[1..].contains(&(b'=' as u16)) || k.contains(&0) || v.contains(&0) {
            return Err("invalid environment variable".into());
        }
        vars.retain(|(n, _)| upper(n) != upper(&k));
        vars.push((k, v));
    }
    for (k, v) in base {
        let (k, v) = (wide(&k), wide(&v));
        if k.is_empty() || k.contains(&0) || v.contains(&0) || vars.iter().any(|(n, _)| upper(n) == upper(&k)) {
            continue;
        }
        vars.push((k, v));
    }
    vars.sort_by_cached_key(|(k, _)| upper(k));
    let mut block = Vec::new();
    for (k, v) in vars {
        block.extend(k);
        block.push(b'=' as u16);
        block.extend(v);
        block.push(0);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    Ok(block)
}

/// Start `exe args...` detached from us (no console, own process group, no
/// inherited handles) and forget it. The child gets our environment plus
/// `extra_env` (passed as an explicit block; our own environment is not
/// changed). `exe` must be an absolute path: it is passed as the application
/// name, so no search path (and no current directory) is involved.
pub fn spawn_detached(exe: &Path, args: &[&OsStr], extra_env: &[(&OsStr, &OsStr)]) -> Result<(), String> {
    use windows::Win32::System::Threading::CREATE_UNICODE_ENVIRONMENT;
    if !exe.is_absolute() {
        return Err(format!("not an absolute path: {}", exe.display()));
    }
    let mut cmd = command_line(exe, args)?;
    let app: Vec<u16> = exe.as_os_str().encode_wide().chain(Some(0)).collect();
    let env = if extra_env.is_empty() { None } else { Some(env_block(std::env::vars_os(), extra_env)?) };
    let si = STARTUPINFOW { cb: std::mem::size_of::<STARTUPINFOW>() as u32, ..Default::default() };
    let mut pi = PROCESS_INFORMATION::default();
    unsafe {
        CreateProcessW(
            PCWSTR(app.as_ptr()),
            Some(PWSTR(cmd.as_mut_ptr())),
            None,
            None,
            false,
            DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_UNICODE_ENVIRONMENT,
            env.as_ref().map(|b| b.as_ptr() as *const std::ffi::c_void),
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
        assert!(spawn_detached(Path::new("cmd.exe"), &[], &[]).is_err(), "relative paths are refused");
    }

    #[test]
    fn spawns_and_waits() {
        // `cmd /c exit` starts and ends at once; wait_pid on a gone (or never
        // valid) pid returns promptly.
        let cmd = system_exe("cmd.exe").unwrap();
        assert!(cmd.is_file());
        spawn_detached(&cmd, &[OsStr::new("/c"), OsStr::new("exit 0")], &[]).unwrap();
        spawn_detached(&cmd, &[OsStr::new("/c"), OsStr::new("exit 0")], &[(OsStr::new("RUSTSHOT_X"), OsStr::new("1"))])
            .unwrap();
        let t = std::time::Instant::now();
        wait_pid(0, 10_000);
        wait_pid(u32::MAX - 2, 10_000);
        assert!(t.elapsed() < std::time::Duration::from_secs(2));
        assert!(spawn_detached(&cmd.with_file_name("rustshot-no-such.exe"), &[], &[]).is_err());
    }

    #[test]
    fn env_block_is_sorted_double_nul_terminated_and_overrides() {
        let os = |s: &str| OsString::from(s);
        let base = vec![(os("Path"), os(r"C:\x")), (os("b"), os("2")), (os("rustshot_wait_pid"), os("old")), (os("A"), os("1"))];
        let block = env_block(base, &[(OsStr::new("RUSTSHOT_WAIT_PID"), OsStr::new("42"))]).unwrap();
        assert_eq!(&block[block.len() - 2..], &[0, 0]);
        let text = String::from_utf16(&block[..block.len() - 2]).unwrap();
        let entries: Vec<&str> = text.split('\0').collect();
        assert_eq!(entries, ["A=1", "b=2", r"Path=C:\x", "RUSTSHOT_WAIT_PID=42"]);
        // An empty environment is still a valid (double-NUL) block.
        assert_eq!(env_block(Vec::new(), &[]).unwrap(), [0, 0]);
        for (k, v) in [("", "v"), ("A=B", "v"), ("A\0", "v"), ("A", "v\0")] {
            assert!(env_block(Vec::new(), &[(OsStr::new(k), OsStr::new(v))]).is_err(), "{k:?}={v:?}");
        }
    }
}
