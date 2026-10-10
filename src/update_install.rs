//! Apply a downloaded, verified update (`update::fetch_verified`): run the
//! installer silently or swap the binary in place, then relaunch the daemon.
//! The file's SHA-256 is checked again right before anything is executed or
//! swapped in.

use crate::update::{self, InstallKind};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// What `apply` did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Applied {
    /// The new version is starting; the caller (the daemon) must exit now.
    RestartingNow,
    /// Nothing was installed; the release page was opened instead.
    OpenedPage,
}

/// Overrides `update_dir()` (tests).
pub const DIR_VAR: &str = "RUSTSHOT_UPDATE_DIR";
/// Set for a relaunched daemon: the PID of the old daemon to wait for.
pub const WAIT_PID_VAR: &str = "RUSTSHOT_WAIT_PID";
/// Longest a relaunched daemon waits for the old one.
const WAIT_MS: u32 = 10_000;
/// Subdirectory of the update dir a tarball is extracted into.
const EXTRACT_DIR: &str = "extract";
/// Created inside `EXTRACT_DIR` by us; cleanup removes only a dir that has it.
const EXTRACT_MARKER: &str = ".rustshot-extract";
/// Name of the private copy of the tarball inside `EXTRACT_DIR`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const TARBALL_COPY: &str = "update.tar.gz";
/// Hidden `daemon` flag for a daemon started by an update (the NSIS
/// installer and `relaunch` pass it): it never signals the old daemon.
pub const AFTER_UPDATE_FLAG: &str = "--after-update";
/// After the wait for the old daemon, keep trying to become the primary
/// for this long (`RETRY_TRIES` x `RETRY_MS`) before exiting quietly.
const RETRY_MS: u64 = 250;
const RETRY_TRIES: u32 = 80;

/// User-private download dir: `%LOCALAPPDATA%\rustshot\update` on Windows,
/// `$XDG_CACHE_HOME/rustshot/update` (or `~/.cache/...`) elsewhere;
/// `RUSTSHOT_UPDATE_DIR` overrides it.
pub fn update_dir() -> PathBuf {
    update_dir_from(|k| std::env::var_os(k))
}

fn update_dir_from(env: impl Fn(&str) -> Option<OsString>) -> PathBuf {
    let get = |k: &str| env(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    if let Some(d) = get(DIR_VAR) {
        return d;
    }
    #[cfg(windows)]
    let base = get("LOCALAPPDATA").filter(|p| p.is_absolute());
    #[cfg(not(windows))]
    let base = get("XDG_CACHE_HOME")
        .filter(|p| p.is_absolute())
        .or_else(|| get("HOME").map(|h| h.join(".cache")));
    match base {
        Some(b) => b.join("rustshot").join("update"),
        None => crate::config::config_dir().join("update"),
    }
}

/// Create `update_dir()` (mode 0700 on Unix) for `update::fetch_verified`.
#[allow(dead_code)] // used by the update dialog (later task)
pub fn ensure_update_dir() -> Result<PathBuf, String> {
    let d = update_dir();
    ensure_private_dir(&d)?;
    Ok(d)
}

fn ensure_private_dir(d: &Path) -> Result<(), String> {
    let err = |e: std::io::Error| format!("create {}: {e}", d.display());
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(d).map_err(err)?;
        std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700)).map_err(err)?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(d).map_err(err)?;
    Ok(())
}

/// `<path><suffix>` (e.g. `rustshot.exe.old`).
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// The file a portable update replaces: the AppImage itself when running
/// from one, otherwise this exe.
/// `$APPIMAGE` counts only when it is ours (`update::own_appimage`).
fn swap_target() -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    if let Some(a) = update::own_appimage() {
        return Some(a);
    }
    std::env::current_exe().ok()
}

/// Remove what an earlier update left behind: our files in `update_dir()`
/// and `<exe>.old` / `<exe>.new`. Called by the daemon once it owns the
/// single instance (so no download is in progress).
pub fn cleanup_previous() {
    cleanup(&update_dir(), swap_target().as_deref());
}

fn cleanup(dir: &Path, target: Option<&Path>) {
    if dir_is_private(dir)
        && let Ok(rd) = std::fs::read_dir(dir)
    {
        for e in rd.flatten() {
            let name = e.file_name();
            let Some(name) = name.to_str() else { continue };
            // Only exact names we create: the dir may be overridden.
            let Ok(meta) = std::fs::symlink_metadata(e.path()) else { continue };
            if is_our_file(name) && !meta.is_dir() {
                let _ = std::fs::remove_file(e.path());
            } else if name == EXTRACT_DIR {
                remove_extract_dir(&e.path());
            }
        }
    }
    if let Some(t) = target {
        let _ = std::fs::remove_file(sibling(t, ".old"));
        let _ = std::fs::remove_file(sibling(t, ".new"));
    }
}

/// A file name we put in the update dir: a release asset
/// (`rustshot-<version><known suffix>`), `SHA256SUMS`, or either with `.part`.
fn is_our_file(name: &str) -> bool {
    let n = name.strip_suffix(".part").unwrap_or(name);
    if n == update::SUMS_NAME {
        return true;
    }
    let Some(rest) = n.strip_prefix("rustshot-") else { return false };
    update::ASSET_SUFFIXES.iter().any(|suf| {
        rest.strip_suffix(suf).is_some_and(|v| {
            (1..=64).contains(&v.len())
                && v.starts_with(|c: char| c.is_ascii_digit())
                && v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'+')
        })
    })
}

/// Remove `dir` (recursively) only if it is a real directory (not a link)
/// holding our marker file. Returns whether it is gone.
fn remove_extract_dir(dir: &Path) -> bool {
    match std::fs::symlink_metadata(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Ok(m) if m.is_dir() && std::fs::symlink_metadata(dir.join(EXTRACT_MARKER)).is_ok_and(|m| m.is_file()) => {
            std::fs::remove_dir_all(dir).is_ok()
        }
        _ => false,
    }
}

/// The update dir may be cleaned: on Unix it must be a real directory owned
/// by us with mode 0700 (what `ensure_private_dir` makes); elsewhere a dir.
fn dir_is_private(dir: &Path) -> bool {
    let Ok(m) = std::fs::symlink_metadata(dir) else { return false };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        unsafe extern "C" {
            fn geteuid() -> u32;
        }
        m.is_dir() && m.uid() == unsafe { geteuid() } && m.mode() & 0o777 == 0o700
    }
    #[cfg(not(unix))]
    m.is_dir()
}

/// Swap `new` in for `current`: `current` → `current.old`, `new` →
/// `current`. If the second rename fails the first is undone. Renaming a
/// running exe is allowed on Windows and Unix.
pub fn swap_in_place(current: &Path, new: &Path) -> Result<(), String> {
    swap_with(current, new, &|a, b| std::fs::rename(a, b))
}

/// `swap_in_place` with the rename injected (tests make it fail).
fn swap_with(
    current: &Path,
    new: &Path,
    rename: &dyn Fn(&Path, &Path) -> std::io::Result<()>,
) -> Result<(), String> {
    let old = sibling(current, ".old");
    match std::fs::remove_file(&old) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return Err(format!("remove {}: {e}", old.display()));
        }
        _ => {}
    }
    rename(current, &old).map_err(|e| format!("rename {}: {e}", current.display()))?;
    if let Err(e) = rename(new, current) {
        return Err(match rename(&old, current) {
            Ok(()) => format!("install {}: {e}", current.display()),
            Err(e2) => format!(
                "install {}: {e}; restoring it from {} also failed: {e2}",
                current.display(),
                old.display()
            ),
        });
    }
    Ok(())
}

/// Copy `src` to a fresh `<current>.new`, check its SHA-256, make it
/// executable (Unix) and swap it in. On error `current` is unchanged and
/// `.new` is removed.
fn replace_exe(current: &Path, src: &Path, sha256: [u8; 32]) -> Result<(), String> {
    let new = sibling(current, ".new");
    let _ = std::fs::remove_file(&new);
    let r = (|| {
        copy_exclusive(src, &new)?;
        update::verify_file(&new, sha256)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755))
                .map_err(|e| format!("chmod {}: {e}", new.display()))?;
        }
        swap_in_place(current, &new)
    })();
    if r.is_err() {
        let _ = std::fs::remove_file(&new);
    }
    r
}

/// Copy into a file that must not exist yet (owner-only until swapped in).
fn copy_exclusive(src: &Path, dst: &Path) -> Result<(), String> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o700);
    let mut out = opts.open(dst).map_err(|e| format!("create {}: {e}", dst.display()))?;
    let mut inp = std::fs::File::open(src).map_err(|e| format!("open {}: {e}", src.display()))?;
    std::io::copy(&mut inp, &mut out).map_err(|e| format!("write {}: {e}", dst.display()))?;
    out.sync_all().map_err(|e| format!("write {}: {e}", dst.display()))
}

/// Start `exe daemon --after-update`, telling it to wait for `wait_pid` to
/// exit first. Our own environment is not changed.
fn relaunch(exe: &Path, wait_pid: u32) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::ffi::OsStr;
        let pid = wait_pid.to_string();
        crate::proc_win::spawn_detached(
            exe,
            &[OsStr::new("daemon"), OsStr::new(AFTER_UPDATE_FLAG)],
            &[(OsStr::new(WAIT_PID_VAR), OsStr::new(&pid))],
        )
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};
        Command::new(exe)
            .arg("daemon")
            .arg(AFTER_UPDATE_FLAG)
            .env(WAIT_PID_VAR, wait_pid.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .map(drop)
            .map_err(|e| format!("cannot start {}: {e}", exe.display()))
    }
}

/// Portable update of `exe`: swap in `file`, then relaunch with `wait_pid`.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn apply_portable(exe: &Path, file: &Path, sha256: [u8; 32], wait_pid: u32) -> Result<Applied, String> {
    replace_exe(exe, file, sha256)?;
    relaunch(exe, wait_pid).map_err(|e| format!("the update is installed but rustshot could not restart: {e}"))?;
    Ok(Applied::RestartingNow)
}

/// Run the NSIS installer silently (`/S /RELAUNCH`): it waits for this
/// daemon to exit, installs, and starts the new daemon.
#[cfg(windows)]
fn run_installer(file: &Path, sha256: [u8; 32]) -> Result<Applied, String> {
    use std::ffi::OsStr;
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::Storage::FileSystem::FILE_SHARE_READ;
    // Held open with read-only sharing from the check until the installer
    // has started, so nobody can rewrite or replace the file in between.
    let _hold = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ.0)
        .open(file)
        .map_err(|e| format!("open {}: {e}", file.display()))?;
    update::verify_file(file, sha256)?;
    crate::proc_win::spawn_detached(file, &[OsStr::new("/S"), OsStr::new("/RELAUNCH")], &[])?;
    Ok(Applied::RestartingNow)
}

/// Extract `./rustshot` from a release tarball. The tarball is first copied
/// into a fresh private `<dir of file>/extract` (with our marker), the copy
/// is verified, and only the copy is untarred, by `/usr/bin/tar` (or
/// `/bin/tar`) with a fixed `PATH` (tar runs `gzip` for `-z`).
#[cfg(target_os = "linux")]
fn extract_tarball(file: &Path, sha256: [u8; 32]) -> Result<PathBuf, String> {
    use std::process::{Command, Stdio};
    let dir = file.parent().unwrap_or(Path::new(".")).join(EXTRACT_DIR);
    if !remove_extract_dir(&dir) {
        return Err(format!("{} exists and is not ours; refusing to use it", dir.display()));
    }
    ensure_private_dir(&dir)?;
    copy_exclusive(Path::new("/dev/null"), &dir.join(EXTRACT_MARKER))?;
    let copy = dir.join(TARBALL_COPY);
    copy_exclusive(file, &copy)?;
    update::verify_file(&copy, sha256)?;
    let tar = ["/usr/bin/tar", "/bin/tar"]
        .into_iter()
        .map(Path::new)
        .find(|p| p.is_file())
        .ok_or("tar was not found in /usr/bin or /bin")?;
    let st = Command::new(tar)
        .arg("-xzf")
        .arg(&copy)
        .arg("-C")
        .arg(&dir)
        .arg("./rustshot")
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .status()
        .map_err(|e| format!("cannot run tar: {e}"))?;
    let bin = dir.join("rustshot");
    if !st.success() || !std::fs::symlink_metadata(&bin).is_ok_and(|m| m.is_file()) {
        return Err("the tarball does not contain the rustshot binary".into());
    }
    Ok(bin)
}

/// Install the downloaded `file` (whose SHA-256 must be `sha256`) for an
/// install of `kind`. The file is re-hashed right before it is run or
/// swapped in. `RestartingNow`: the caller must exit at once.
/// Kinds that cannot update in place open the release page.
#[allow(dead_code)] // used by the update dialog (later task)
pub fn apply(kind: &InstallKind, file: &Path, sha256: [u8; 32]) -> Result<Applied, String> {
    let file = &std::path::absolute(file).map_err(|e| format!("{}: {e}", file.display()))?;
    match kind {
        #[cfg(windows)]
        InstallKind::WinInstaller => run_installer(file, sha256),
        #[cfg(windows)]
        InstallKind::WinPortable => {
            let exe = std::env::current_exe().map_err(|e| format!("cannot find rustshot.exe: {e}"))?;
            apply_portable(&exe, file, sha256, std::process::id())
        }
        #[cfg(target_os = "linux")]
        InstallKind::AppImage(path) => {
            if !path.is_absolute() || !path.is_file() {
                return Err(format!("unexpected AppImage path {}", path.display()));
            }
            apply_portable(path, file, sha256, std::process::id())
        }
        #[cfg(target_os = "linux")]
        InstallKind::Tarball => {
            let exe = std::env::current_exe().map_err(|e| format!("cannot find rustshot: {e}"))?;
            let bin = extract_tarball(file, sha256)?;
            let digest = crate::sha256::file_digest(&bin).map_err(|e| e.to_string())?;
            let r = apply_portable(&exe, &bin, digest, std::process::id());
            remove_extract_dir(bin.parent().unwrap_or(&bin));
            r
        }
        // deb/rpm, stores, macOS (until tested on a Mac), unknown.
        _ => {
            let _ = (file, sha256); // nothing is run
            update::open_url(&release_page());
            Ok(Applied::OpenedPage)
        }
    }
}

fn release_page() -> String {
    format!("{}releases/latest", update::RELEASES_PREFIX)
}

/// The PID in `RUSTSHOT_WAIT_PID`, if usable: a plain positive number that
/// is not our own PID (and fits a Unix `pid_t`).
fn parse_wait_pid(s: &str, own: u32) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let pid: u32 = s.parse().ok()?;
    (pid != 0 && pid != own && pid <= i32::MAX as u32).then_some(pid)
}

/// For a relaunched daemon: wait (at most 10 s) for the old daemon named in
/// `RUSTSHOT_WAIT_PID` to exit, so the single-instance lock is free. Invalid
/// values are ignored; the variable is removed so children don't inherit it.
/// Returns whether a valid PID was given (this is an update relaunch).
/// Call on the main thread before any other thread exists.
pub fn wait_for_previous() -> bool {
    let Some(v) = std::env::var_os(WAIT_PID_VAR) else { return false };
    // SAFETY: called at daemon start, before the daemon spawns threads (and
    // always safe on Windows).
    unsafe { std::env::remove_var(WAIT_PID_VAR) };
    match v.to_str().and_then(|s| parse_wait_pid(s, std::process::id())) {
        Some(pid) => {
            wait_pid(pid, WAIT_MS);
            true
        }
        None => false,
    }
}

/// How a starting daemon proceeds.
#[derive(Debug, PartialEq, Eq)]
pub enum Start<T> {
    /// We own the instance: run with it.
    Run(T),
    /// A normal launch: take the usual acquire-or-signal path (a running
    /// daemon is asked to capture).
    AcquireOrSignal,
    /// An update relaunch whose old daemon still holds the instance after
    /// the retries: exit quietly, never signalling it.
    GiveUp,
}

/// Pure start-up decision. A normal launch (`update_relaunch` false) never
/// calls `try_acquire`. An update relaunch calls it up to `tries` times,
/// with `pause()` between attempts, and never signals.
pub fn decide_start<T>(
    update_relaunch: bool,
    mut try_acquire: impl FnMut() -> Option<T>,
    tries: u32,
    mut pause: impl FnMut(),
) -> Start<T> {
    if !update_relaunch {
        return Start::AcquireOrSignal;
    }
    for i in 0..tries {
        if i > 0 {
            pause();
        }
        if let Some(t) = try_acquire() {
            return Start::Run(t);
        }
    }
    Start::GiveUp
}

/// `decide_start` with the real retry policy (every 250 ms for 20 s after
/// the bounded wait, so about 30 s in all).
pub fn start_daemon<T>(update_relaunch: bool, try_acquire: impl FnMut() -> Option<T>) -> Start<T> {
    decide_start(update_relaunch, try_acquire, RETRY_TRIES, || {
        std::thread::sleep(std::time::Duration::from_millis(RETRY_MS))
    })
}

#[cfg(windows)]
fn wait_pid(pid: u32, ms: u32) {
    crate::proc_win::wait_pid(pid, ms);
}

/// Poll until `pid` is gone. A relaunched daemon is the old one's child:
/// there, being re-parented means the old one exited (even if not yet reaped).
#[cfg(unix)]
fn wait_pid(pid: u32, ms: u32) {
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
        fn getppid() -> i32;
    }
    let pid = pid as i32;
    let child = unsafe { getppid() } == pid;
    let end = std::time::Instant::now() + std::time::Duration::from_millis(ms as u64);
    while std::time::Instant::now() < end {
        let alive = if child { (unsafe { getppid() }) == pid } else { (unsafe { kill(pid, 0) }) == 0 };
        if !alive {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh temp dir, removed on drop.
    struct Tmp(PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Tmp {
            let d = std::env::temp_dir().join(format!("rustshot-upd-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            Tmp(d)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn read(p: &Path) -> Vec<u8> {
        std::fs::read(p).unwrap()
    }

    #[test]
    fn swap_moves_current_to_old() {
        let t = Tmp::new("swap");
        let cur = t.0.join("rustshot.exe");
        let new = t.0.join("rustshot.exe.new");
        std::fs::write(&cur, b"old").unwrap();
        std::fs::write(&new, b"new").unwrap();
        std::fs::write(t.0.join("rustshot.exe.old"), b"stale").unwrap();
        swap_in_place(&cur, &new).unwrap();
        assert_eq!(read(&cur), b"new");
        assert_eq!(read(&t.0.join("rustshot.exe.old")), b"old");
        assert!(!new.exists());
    }

    #[test]
    fn swap_rolls_back_when_second_rename_fails() {
        let t = Tmp::new("rollback");
        let cur = t.0.join("rustshot.exe");
        std::fs::write(&cur, b"old").unwrap();
        // `new` is missing, so renaming it over `current` fails.
        let err = swap_in_place(&cur, &t.0.join("missing.new")).unwrap_err();
        assert!(err.starts_with("install "), "{err}");
        assert_eq!(read(&cur), b"old");
        assert!(!t.0.join("rustshot.exe.old").exists());
    }

    #[test]
    fn swap_reports_when_rollback_also_fails() {
        let t = Tmp::new("rollback2");
        let cur = t.0.join("rustshot.exe");
        let new = t.0.join("rustshot.exe.new");
        std::fs::write(&cur, b"old").unwrap();
        std::fs::write(&new, b"new").unwrap();
        // The first rename works; the install and the rollback both fail.
        let calls = std::cell::Cell::new(0);
        let rename = |a: &Path, b: &Path| {
            calls.set(calls.get() + 1);
            if calls.get() == 1 {
                std::fs::rename(a, b)
            } else {
                Err(std::io::Error::other("injected"))
            }
        };
        let err = swap_with(&cur, &new, &rename).unwrap_err();
        assert_eq!(calls.get(), 3);
        assert!(err.starts_with("install "), "{err}");
        assert!(err.contains("also failed: injected"), "{err}");
        // The old exe is left at `.old` (named in the error), `new` untouched.
        assert!(err.contains("rustshot.exe.old"), "{err}");
        assert_eq!(read(&t.0.join("rustshot.exe.old")), b"old");
        assert_eq!(read(&new), b"new");
        assert!(!cur.exists());
    }

    #[test]
    fn swap_fails_cleanly_without_current() {
        let t = Tmp::new("nocur");
        let new = t.0.join("n");
        std::fs::write(&new, b"new").unwrap();
        assert!(swap_in_place(&t.0.join("absent.exe"), &new).is_err());
        assert_eq!(read(&new), b"new");
    }

    #[test]
    fn replace_exe_verifies_and_swaps() {
        let t = Tmp::new("replace");
        let cur = t.0.join("rustshot.exe");
        let src = t.0.join("download.bin");
        std::fs::write(&cur, b"old").unwrap();
        std::fs::write(&src, b"new build").unwrap();
        replace_exe(&cur, &src, crate::sha256::digest(b"new build")).unwrap();
        assert_eq!(read(&cur), b"new build");
        assert_eq!(read(&t.0.join("rustshot.exe.old")), b"old");
        assert!(!t.0.join("rustshot.exe.new").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&cur).unwrap().permissions().mode() & 0o777, 0o755);
        }
    }

    #[test]
    fn replace_exe_refuses_mismatch() {
        let t = Tmp::new("mismatch");
        let cur = t.0.join("rustshot.exe");
        let src = t.0.join("download.bin");
        std::fs::write(&cur, b"old").unwrap();
        std::fs::write(&src, b"tampered").unwrap();
        let err = replace_exe(&cur, &src, crate::sha256::digest(b"new build")).unwrap_err();
        assert!(err.contains("SHA-256"), "{err}");
        assert_eq!(read(&cur), b"old");
        assert!(!t.0.join("rustshot.exe.new").exists());
        assert!(!t.0.join("rustshot.exe.old").exists());
    }

    #[test]
    fn replace_exe_does_not_follow_a_stale_new() {
        // A leftover `.new` is replaced, not appended to or trusted.
        let t = Tmp::new("stalenew");
        let cur = t.0.join("rustshot.exe");
        let src = t.0.join("download.bin");
        std::fs::write(&cur, b"old").unwrap();
        std::fs::write(t.0.join("rustshot.exe.new"), b"planted").unwrap();
        std::fs::write(&src, b"good").unwrap();
        replace_exe(&cur, &src, crate::sha256::digest(b"good")).unwrap();
        assert_eq!(read(&cur), b"good");
    }

    #[cfg(windows)]
    #[test]
    fn installer_refuses_mismatch() {
        let t = Tmp::new("inst");
        let f = t.0.join("rustshot-9.9.9-setup.exe");
        std::fs::write(&f, b"not the installer").unwrap();
        let err = apply(&InstallKind::WinInstaller, &f, crate::sha256::digest(b"the installer")).unwrap_err();
        // A spawn attempt would fail with "cannot start ...", not this.
        assert!(err.contains("SHA-256"), "{err}");
        assert!(run_installer(&t.0.join("absent.exe"), [0; 32]).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn held_installer_cannot_be_rewritten_but_still_runs() {
        use std::os::windows::fs::OpenOptionsExt;
        use windows::Win32::Storage::FileSystem::FILE_SHARE_READ;
        let t = Tmp::new("hold");
        let exe = t.0.join("tool.exe");
        std::fs::copy(crate::proc_win::system_exe("cmd.exe").unwrap(), &exe).unwrap();
        let hold = std::fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ.0).open(&exe).unwrap();
        assert!(std::fs::OpenOptions::new().write(true).open(&exe).is_err());
        assert!(std::fs::rename(&exe, t.0.join("moved.exe")).is_err());
        update::verify_file(&exe, crate::sha256::file_digest(&exe).unwrap()).unwrap();
        crate::proc_win::spawn_detached(&exe, &[std::ffi::OsStr::new("/c"), std::ffi::OsStr::new("exit 0")], &[])
            .unwrap();
        drop(hold);
        std::thread::sleep(std::time::Duration::from_millis(300));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn appimage_and_tarball_refuse_mismatch() {
        let t = Tmp::new("unixkinds");
        let img = t.0.join("rustshot.AppImage");
        std::fs::write(&img, b"old").unwrap();
        let dl = t.0.join("rustshot-9.9.9-x86_64.AppImage");
        std::fs::write(&dl, b"tampered").unwrap();
        let err = apply(&InstallKind::AppImage(img.clone()), &dl, crate::sha256::digest(b"x")).unwrap_err();
        assert!(err.contains("SHA-256"), "{err}");
        assert_eq!(read(&img), b"old");
        let tgz = t.0.join("rustshot-9.9.9-linux-x86_64.tar.gz");
        std::fs::write(&tgz, b"not a tarball").unwrap();
        let err = apply(&InstallKind::Tarball, &tgz, crate::sha256::digest(b"x")).unwrap_err();
        // tar never ran: the error is the digest check, not "does not contain".
        assert!(err.contains("SHA-256"), "{err}");
    }

    #[test]
    fn our_file_names() {
        for ok in [
            "SHA256SUMS",
            "SHA256SUMS.part",
            "rustshot-0.1.2-setup.exe",
            "rustshot-0.1.2-setup.exe.part",
            "rustshot-0.1.2-windows-x86_64.exe",
            "rustshot-0.1.2-x86_64.AppImage",
            "rustshot-0.1.2-linux-x86_64.tar.gz",
            "rustshot-0.1.2-macos-universal.zip",
            "rustshot-1.0.0-rc.1+b5-setup.exe",
        ] {
            assert!(is_our_file(ok), "{ok}");
        }
        for bad in [
            "notes.txt",
            "rustshot-stale.part",
            "rustshot-.exe",
            "rustshot--setup.exe",
            "rustshot-x-setup.exe",
            "rustshot-1.0-setup.exe.bak",
            "rustshot-1.0/x-setup.exe",
            "rustshot-1.0-setup.EXE",
            "sha256sums",
            "extract",
            "rustshot-1.0-setup.exe.part.part",
        ] {
            assert!(!is_our_file(bad), "{bad}");
        }
        // Every asset name `pick_asset` can choose is recognised.
        for suf in update::ASSET_SUFFIXES {
            assert!(is_our_file(&format!("rustshot-0.1.2{suf}")), "{suf}");
        }
    }

    /// An update dir as `ensure_private_dir` makes it.
    fn private_update_dir(t: &Tmp) -> PathBuf {
        let dir = t.0.join("update");
        ensure_private_dir(&dir).unwrap();
        dir
    }

    #[test]
    fn cleanup_removes_only_our_leftovers() {
        let t = Tmp::new("cleanup");
        let dir = private_update_dir(&t);
        std::fs::create_dir_all(dir.join(EXTRACT_DIR)).unwrap();
        std::fs::write(dir.join(EXTRACT_DIR).join(EXTRACT_MARKER), b"").unwrap();
        std::fs::write(dir.join(EXTRACT_DIR).join("rustshot"), b"x").unwrap();
        std::fs::write(dir.join("SHA256SUMS"), b"x").unwrap();
        std::fs::write(dir.join("rustshot-9.9.9-setup.exe"), b"x").unwrap();
        std::fs::write(dir.join("rustshot-9.9.9-setup.exe.part"), b"x").unwrap();
        std::fs::write(dir.join("rustshot-notes.txt"), b"keep").unwrap();
        std::fs::write(dir.join("notes.txt"), b"keep").unwrap();
        // A directory with an asset-like name is not removed.
        std::fs::create_dir_all(dir.join("rustshot-1.0-setup.exe")).unwrap();
        let exe = t.0.join("rustshot.exe");
        std::fs::write(&exe, b"exe").unwrap();
        std::fs::write(t.0.join("rustshot.exe.old"), b"old").unwrap();
        std::fs::write(t.0.join("rustshot.exe.new"), b"new").unwrap();
        cleanup(&dir, Some(&exe));
        let mut left: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        left.sort();
        assert_eq!(left, ["notes.txt", "rustshot-1.0-setup.exe", "rustshot-notes.txt"]);
        assert!(exe.exists());
        assert!(!t.0.join("rustshot.exe.old").exists());
        assert!(!t.0.join("rustshot.exe.new").exists());
        cleanup(&t.0.join("absent"), None); // no dir: nothing to do
    }

    #[test]
    fn cleanup_keeps_an_extract_dir_without_our_marker() {
        let t = Tmp::new("nomarker");
        let dir = private_update_dir(&t);
        std::fs::create_dir_all(dir.join(EXTRACT_DIR).join("sub")).unwrap();
        std::fs::write(dir.join(EXTRACT_DIR).join("sub").join("data"), b"keep").unwrap();
        cleanup(&dir, None);
        assert!(dir.join(EXTRACT_DIR).join("sub").join("data").exists());
        assert!(!remove_extract_dir(&dir.join(EXTRACT_DIR)));
        assert!(remove_extract_dir(&dir.join("absent")), "a missing dir counts as removed");
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_skips_a_dir_that_is_not_private() {
        use std::os::unix::fs::PermissionsExt;
        let t = Tmp::new("notpriv");
        let dir = private_update_dir(&t);
        std::fs::write(dir.join("SHA256SUMS"), b"x").unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        cleanup(&dir, None);
        assert!(dir.join("SHA256SUMS").exists(), "0755 dir must not be cleaned");
        // A symlink to a private dir is not followed either.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let link = t.0.join("link");
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        cleanup(&link, None);
        assert!(dir.join("SHA256SUMS").exists(), "symlinked dir must not be cleaned");
        cleanup(&dir, None);
        assert!(!dir.join("SHA256SUMS").exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn extract_tarball_unpacks_a_verified_copy() {
        let t = Tmp::new("tgz");
        let stage = t.0.join("stage");
        std::fs::create_dir_all(&stage).unwrap();
        std::fs::write(stage.join("rustshot"), b"new linux build").unwrap();
        std::fs::write(stage.join("README"), b"other").unwrap();
        let dir = private_update_dir(&t);
        let tgz = dir.join("rustshot-9.9.9-linux-x86_64.tar.gz");
        let st = std::process::Command::new("/usr/bin/tar")
            .arg("-czf")
            .arg(&tgz)
            .arg("-C")
            .arg(&stage)
            .arg("./rustshot")
            .arg("./README")
            .status()
            .unwrap();
        assert!(st.success());
        let sha = crate::sha256::file_digest(&tgz).unwrap();
        let bin = extract_tarball(&tgz, sha).unwrap();
        assert_eq!(bin, dir.join(EXTRACT_DIR).join("rustshot"));
        assert_eq!(read(&bin), b"new linux build");
        assert!(!dir.join(EXTRACT_DIR).join("README").exists(), "only ./rustshot is extracted");
        assert!(dir.join(EXTRACT_DIR).join(TARBALL_COPY).exists());
        // A second run replaces our own extract dir.
        assert!(extract_tarball(&tgz, sha).is_ok());
        // A mismatching download is refused before tar runs.
        assert!(extract_tarball(&tgz, [0; 32]).unwrap_err().contains("SHA-256"));
        // An extract dir without our marker is never deleted or used.
        assert!(remove_extract_dir(&dir.join(EXTRACT_DIR)));
        std::fs::create_dir_all(dir.join(EXTRACT_DIR)).unwrap();
        std::fs::write(dir.join(EXTRACT_DIR).join("theirs"), b"x").unwrap();
        assert!(extract_tarball(&tgz, sha).unwrap_err().contains("not ours"));
        assert!(dir.join(EXTRACT_DIR).join("theirs").exists());
        cleanup(&dir, None);
        assert!(!tgz.exists());
        assert!(dir.join(EXTRACT_DIR).join("theirs").exists());
    }

    #[test]
    fn normal_launch_takes_the_signal_path_without_trying() {
        let mut tried = 0;
        let r: Start<u8> = decide_start(false, || {
            tried += 1;
            Some(1)
        }, 80, || {});
        assert_eq!(r, Start::AcquireOrSignal);
        assert_eq!(tried, 0);
    }

    #[test]
    fn update_relaunch_never_signals() {
        // (acquire results in order, tries) -> (outcome, attempts, pauses)
        let run = |results: &[Option<u8>], tries: u32| {
            let (mut i, mut pauses) = (0, 0);
            let r = decide_start(true, || {
                i += 1;
                results.get(i - 1).copied().flatten()
            }, tries, || pauses += 1);
            (r, i, pauses)
        };
        assert_eq!(run(&[Some(7)], 80), (Start::Run(7), 1, 0));
        assert_eq!(run(&[None, None, Some(7)], 80), (Start::Run(7), 3, 2));
        // Still held after every try: give up quietly, never signal.
        assert_eq!(run(&[], 80), (Start::GiveUp, 80, 79));
        assert_eq!(run(&[None, None, None, Some(7)], 3), (Start::GiveUp, 3, 2));
        assert_eq!(run(&[None, None, Some(7)], 3), (Start::Run(7), 3, 2));
        assert_eq!(run(&[Some(7)], 0), (Start::GiveUp, 0, 0));
        // The real policy: 80 x 250 ms = 20 s after the 10 s wait.
        assert_eq!(RETRY_TRIES as u64 * RETRY_MS + WAIT_MS as u64, 30_000);
    }

    #[test]
    fn update_dir_locations() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| OsString::from(v))
        };
        assert_eq!(update_dir_from(env(&[(DIR_VAR, "/x/upd"), ("HOME", "/h")])), PathBuf::from("/x/upd"));
        #[cfg(windows)]
        {
            assert_eq!(
                update_dir_from(env(&[("LOCALAPPDATA", r"C:\Users\u\AppData\Local"), ("TEMP", r"C:\T")])),
                PathBuf::from(r"C:\Users\u\AppData\Local\rustshot\update")
            );
            assert_eq!(update_dir_from(env(&[(DIR_VAR, ""), ("LOCALAPPDATA", r"C:\L")])), PathBuf::from(r"C:\L\rustshot\update"));
        }
        #[cfg(unix)]
        {
            assert_eq!(update_dir_from(env(&[("XDG_CACHE_HOME", "/c"), ("HOME", "/h")])), PathBuf::from("/c/rustshot/update"));
            assert_eq!(update_dir_from(env(&[("XDG_CACHE_HOME", "rel"), ("HOME", "/h")])), PathBuf::from("/h/.cache/rustshot/update"));
            assert_eq!(update_dir_from(env(&[("HOME", "/h")])), PathBuf::from("/h/.cache/rustshot/update"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn private_dir_is_0700() {
        use std::os::unix::fs::PermissionsExt;
        let t = Tmp::new("priv");
        let d = t.0.join("a").join("update");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o777)).unwrap();
        ensure_private_dir(&d).unwrap();
        assert_eq!(std::fs::metadata(&d).unwrap().permissions().mode() & 0o777, 0o700);
    }

    #[test]
    fn wait_pid_values() {
        assert_eq!(parse_wait_pid("1234", 99), Some(1234));
        for bad in ["", "0", "99", "-5", "+5", " 12", "12 ", "0x10", "abc", "4294967295", "2147483648", "99999999999"] {
            assert_eq!(parse_wait_pid(bad, 99), None, "{bad:?}");
        }
        // A pid that does not exist returns at once.
        let t = std::time::Instant::now();
        wait_pid(i32::MAX as u32 - 7, 10_000);
        assert!(t.elapsed() < std::time::Duration::from_secs(2));
    }

    #[test]
    fn release_page_is_allowed() {
        assert!(release_page().starts_with(update::RELEASES_PREFIX));
    }

    /// Portable update end to end (Windows): an old daemon running from a
    /// temp-dir copy is updated from a local file and replaced by the new
    /// binary. Uses its own instance name, config and update dir, so the
    /// user's daemon is never signalled. Needs a release build:
    ///
    ///     $env:RUSTSHOT_UPDATE_E2E_EXE = "target-sub\release\rustshot.exe"
    ///     cargo test live_portable_update -- --ignored --exact update_install::tests::live_portable_update
    #[cfg(windows)]
    #[test]
    #[ignore = "starts daemons from a temp copy; set RUSTSHOT_UPDATE_E2E_EXE"]
    fn live_portable_update() {
        use std::time::{Duration, Instant};
        use windows::Win32::Foundation::{CloseHandle, HWND};
        use windows::Win32::System::Threading::{
            OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
            QueryFullProcessImageNameW, TerminateProcess,
        };
        use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, GetWindowThreadProcessId};
        use windows::core::{PCWSTR, PWSTR};

        let src = std::env::var_os("RUSTSHOT_UPDATE_E2E_EXE").expect("set RUSTSHOT_UPDATE_E2E_EXE");
        let src = std::path::absolute(PathBuf::from(src)).unwrap();
        let t = Tmp::new("e2e");
        let app = t.0.join("app");
        let appdata = t.0.join("appdata");
        let upd = t.0.join("upd");
        for d in [&app, &appdata.join("rustshot"), &upd] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(
            appdata.join("rustshot").join("config.toml"),
            "check_updates = false\ncapture_hotkey = \"Ctrl+Alt+Shift+F9\"\nquit_hotkey = \"Ctrl+Alt+Shift+F10\"\n",
        )
        .unwrap();
        std::fs::write(upd.join("rustshot-0.0.1-windows-x86_64.exe.part"), b"x").unwrap();
        std::fs::write(upd.join("keep.txt"), b"x").unwrap();
        let exe = app.join("rustshot.exe");
        std::fs::copy(&src, &exe).unwrap();
        // The "new version": the same exe with a marker appended (PE overlay).
        let mut new_bytes = read(&src);
        new_bytes.extend_from_slice(b"rustshot-e2e-new-build");
        let fixture = t.0.join("rustshot-9.9.9-windows-x86_64.exe");
        std::fs::write(&fixture, &new_bytes).unwrap();
        let sha = crate::sha256::digest(&new_bytes);

        let name = format!("e2e{}", std::process::id());
        // SAFETY: always safe on Windows. Inherited by both daemons.
        unsafe {
            std::env::set_var("APPDATA", &appdata);
            std::env::set_var(crate::instance::INSTANCE_VAR, &name);
            std::env::set_var(DIR_VAR, &upd);
        }
        let class: Vec<u16> = format!("rustshot_tray_{name}").encode_utf16().chain(Some(0)).collect();
        let owner = || unsafe {
            FindWindowW(PCWSTR(class.as_ptr()), PCWSTR::null()).ok().map(|h: HWND| {
                let mut pid = 0;
                GetWindowThreadProcessId(h, Some(&mut pid));
                pid
            })
        };
        let wait = |ms: u64, f: &dyn Fn() -> bool| {
            let end = Instant::now() + Duration::from_millis(ms);
            while Instant::now() < end {
                if f() {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            f()
        };

        /// Terminates the processes this test started (only those) when
        /// dropped. Holding their handles keeps the PIDs from being reused.
        struct Kill(Vec<windows::Win32::Foundation::HANDLE>);
        impl Kill {
            fn add(&mut self, pid: u32) {
                self.0.push(unsafe { OpenProcess(PROCESS_TERMINATE, false, pid) }.unwrap());
            }
        }
        impl Drop for Kill {
            fn drop(&mut self) {
                for &h in &self.0 {
                    unsafe {
                        let _ = TerminateProcess(h, 0);
                        let _ = CloseHandle(h);
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(300));
            }
        }

        let mut old = std::process::Command::new(&exe).arg("daemon").spawn().unwrap();
        let a = old.id();
        let mut kill = Kill(Vec::new());
        kill.add(a);
        assert!(wait(5000, &|| owner() == Some(a)), "old daemon did not start");
        eprintln!("old daemon pid {a}");

        assert_eq!(apply_portable(&exe, &fixture, sha, a), Ok(Applied::RestartingNow));
        assert!(sibling(&exe, ".old").exists());
        assert_eq!(read(&exe), new_bytes);
        // The new daemon waits for the old one instead of signalling it.
        std::thread::sleep(Duration::from_millis(1500));
        assert_eq!(owner(), Some(a), "the new daemon must wait for the old one");

        old.kill().unwrap();
        old.wait().unwrap();
        assert!(wait(10_000, &|| owner().is_some_and(|p| p != a)), "new daemon did not take over");
        let b = owner().unwrap();
        kill.add(b);
        eprintln!("new daemon pid {b}");

        // It runs the swapped-in file and cleaned up after the update.
        let image = unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, b).unwrap();
            let mut buf = [0u16; 1024];
            let mut n = buf.len() as u32;
            QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut n).unwrap();
            let _ = CloseHandle(h);
            PathBuf::from(String::from_utf16_lossy(&buf[..n as usize]))
        };
        assert_eq!(image.canonicalize().unwrap(), exe.canonicalize().unwrap());
        assert_eq!(crate::sha256::file_digest(&exe).unwrap(), sha);
        assert!(wait(5000, &|| !sibling(&exe, ".old").exists()), ".old not removed");
        assert!(!upd.join("rustshot-0.0.1-windows-x86_64.exe.part").exists(), "update dir not cleaned");
        assert!(upd.join("keep.txt").exists(), "a foreign file was deleted");
        drop(kill);
        assert!(owner().is_none());
    }
}
