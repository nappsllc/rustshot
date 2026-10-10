//! End-to-end interaction tests: launch the real binary and drive it with
//! posted Win32 messages (keys, mouse, chars). Ignored by default like the
//! other live tests; run with:
//!
//!     cargo test --test e2e -- --ignored
//!
//! The child gets its own config dir (APPDATA) so the user's config is
//! never read or written; `RUSTSHOT_E2E_RENDERER=gdi|software` picks the
//! overlay renderer (default gdi).
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowThreadProcessId, IsWindowVisible, PostMessageW, WM_CHAR, WM_KEYDOWN,
    WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
};

const BIN_BUILT: &str = env!("CARGO_BIN_EXE_rustshot");

/// The binary under test (`RUSTSHOT_E2E_BIN` overrides, e.g. an older build).
fn bin() -> String {
    std::env::var("RUSTSHOT_E2E_BIN").unwrap_or_else(|_| BIN_BUILT.into())
}

#[repr(C)]
struct Slot {
    pid: u32,
    hwnd: HWND,
}

unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
    unsafe {
        let slot = &mut *(lparam.0 as *mut Slot);
        let mut p = 0u32;
        let _ = GetWindowThreadProcessId(hwnd, Some(&mut p));
        if p == slot.pid && IsWindowVisible(hwnd).as_bool() {
            slot.hwnd = hwnd;
            BOOL(0)
        } else {
            BOOL(1)
        }
    }
}

/// Find the visible top-level window of process `pid`.
fn find_window(pid: u32) -> Option<HWND> {
    let mut slot = Slot {
        pid,
        hwnd: HWND::default(),
    };
    unsafe {
        let _ = EnumWindows(Some(enum_cb), LPARAM(&mut slot as *mut Slot as isize));
    }
    (!slot.hwnd.0.is_null()).then_some(slot.hwnd)
}

fn wait_window(child: &Child, ms: u64) -> Option<HWND> {
    let deadline = Instant::now() + Duration::from_millis(ms);
    loop {
        if let Some(h) = find_window(child.id()) {
            return Some(h);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn wait_exit(child: &mut Child, ms: u64) -> Option<i32> {
    let deadline = Instant::now() + Duration::from_millis(ms);
    loop {
        if let Ok(Some(st)) = child.try_wait() {
            return st.code();
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A temp APPDATA root, removed on drop.
struct TempRoot(PathBuf);

impl std::ops::Deref for TempRoot {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const ROOT_PREFIX: &str = "rustshot-e2e-";

/// Remove `rustshot-e2e-<pid>-*` dirs earlier runs left in %TEMP% (only
/// that prefix, and not this process' own).
fn sweep_old_roots() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let own = format!("{ROOT_PREFIX}{}-", std::process::id());
        let Ok(rd) = std::fs::read_dir(std::env::temp_dir()) else { return };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with(ROOT_PREFIX) && !name.starts_with(&own) && e.path().is_dir() {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    });
}

/// A fresh APPDATA root whose `rustshot/config.toml` selects `renderer`.
fn config_root(tag: &str, renderer: &str) -> TempRoot {
    sweep_old_roots();
    let root = std::env::temp_dir().join(format!("{ROOT_PREFIX}{}-{tag}", std::process::id()));
    let dir = root.join("rustshot");
    std::fs::create_dir_all(&dir).expect("config dir");
    std::fs::write(dir.join("config.toml"), format!("renderer = \"{renderer}\"\ncheck_updates = false\n"))
        .expect("config");
    TempRoot(root)
}

fn renderer() -> String {
    std::env::var("RUSTSHOT_E2E_RENDERER").unwrap_or_else(|_| "gdi".into())
}

fn rustshot(appdata: &Path) -> Command {
    let mut c = Command::new(bin());
    c.env("APPDATA", appdata);
    c
}

/// The editor on an 800x600 region; keep the root until the child exits.
fn launch_editor(tag: &str) -> (Child, HWND, TempRoot) {
    let root = config_root(tag, &renderer());
    let child = rustshot(&root)
        .args(["gui", "--region", "800x600+20+20", "--clip"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn rustshot");
    let hwnd = wait_window(&child, 8000).expect("editor window");
    (child, hwnd, root)
}

fn post(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) {
    let _ = unsafe { PostMessageW(Some(hwnd), msg, w, l) };
}

fn pack(x: i32, y: i32) -> LPARAM {
    LPARAM((((y as u32) << 16) | (x as u32 & 0xFFFF)) as isize)
}

fn key_down(hwnd: HWND, vk: u32) {
    post(hwnd, WM_KEYDOWN, WPARAM(vk as usize), LPARAM(0));
}
fn key_up(hwnd: HWND, vk: u32) {
    post(hwnd, WM_KEYUP, WPARAM(vk as usize), LPARAM(0));
}
fn key(hwnd: HWND, vk: u32) {
    key_down(hwnd, vk);
    key_up(hwnd, vk);
}
fn mouse_down(hwnd: HWND, x: i32, y: i32) {
    post(hwnd, WM_LBUTTONDOWN, WPARAM(1), pack(x, y));
}
fn mouse_up(hwnd: HWND, x: i32, y: i32) {
    post(hwnd, WM_LBUTTONUP, WPARAM(0), pack(x, y));
}
fn mouse_move(hwnd: HWND, x: i32, y: i32) {
    post(hwnd, WM_MOUSEMOVE, WPARAM(1), pack(x, y));
}
fn char_msg(hwnd: HWND, c: char) {
    post(hwnd, WM_CHAR, WPARAM(c as usize), LPARAM(0));
}

#[test]
#[ignore = "live display access"]
fn escape_cancels_with_code_2() {
    let (mut child, hwnd, _root) = launch_editor("escape");
    key(hwnd, 0x1B); // VK_ESCAPE
    assert_eq!(wait_exit(&mut child, 5000), Some(2), "esc should cancel");
}

#[test]
#[ignore = "live display access"]
fn rect_draw_then_enter_exports_zero() {
    let (mut child, hwnd, _root) = launch_editor("rect");
    key(hwnd, 'R' as u32);
    mouse_move(hwnd, 400, 300);
    mouse_down(hwnd, 400, 300);
    mouse_move(hwnd, 500, 400);
    mouse_move(hwnd, 600, 380);
    mouse_up(hwnd, 600, 380);
    key(hwnd, 0x0D); // VK_RETURN accepts
    assert_eq!(
        wait_exit(&mut child, 5000),
        Some(0),
        "draw + accept should succeed"
    );
}

#[test]
#[ignore = "live display access"]
fn text_tool_types_and_accepts() {
    let (mut child, hwnd, _root) = launch_editor("text");
    key(hwnd, 'T' as u32);
    mouse_down(hwnd, 300, 300);
    mouse_up(hwnd, 300, 300);
    for c in "Hello".chars() {
        char_msg(hwnd, c);
    }
    key(hwnd, 0x0D); // commit text draft
    key(hwnd, 0x0D); // accept
    assert_eq!(
        wait_exit(&mut child, 5000),
        Some(0),
        "text + accept should succeed"
    );
}

#[test]
#[ignore = "live display access"]
fn daemon_stays_alive() {
    if daemon_running() {
        // A second daemon would signal it (open a capture) and exit.
        println!("skipped: another rustshot daemon is running in this session");
        return;
    }
    let root = config_root("daemon", &renderer());
    let mut child = rustshot(&root)
        .arg("daemon")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn daemon");
    std::thread::sleep(Duration::from_secs(2));
    match child.try_wait().expect("try_wait") {
        None => {
            let _ = child.kill();
            let _ = child.wait();
        }
        Some(st) => panic!("daemon exited early: {st:?}"),
    }
}

/// Whether a rustshot daemon holds the single-instance mutex (opened,
/// never created, so nothing is signalled).
fn daemon_running() -> bool {
    use windows::core::w;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};
    match unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, w!("Local\\rustshot-daemon")) } {
        Ok(h) => {
            let _ = unsafe { CloseHandle(h) };
            true
        }
        Err(_) => false,
    }
}

/// Peak private bytes of a one-shot full-desktop capture saved as PNG,
/// per renderer (and with one rectangle drawn), from `RUSTSHOT_MEMLOG`.
///
///     cargo test --release --test e2e memory_one_shot -- --ignored --nocapture
#[test]
#[ignore = "live display access; measurement"]
fn memory_one_shot() {
    // (label, renderer, tool key + drag from/to)
    type Draw = Option<(char, (i32, i32), (i32, i32))>;
    let cases: [(&str, &str, Draw); 5] = [
        ("software", "software", None),
        ("software + 2000x900 pixelate", "software", Some(('B', (400, 300), (2400, 1200)))),
        ("gdi", "gdi", None),
        ("gdi + rect", "gdi", Some(('R', (400, 300), (900, 600)))),
        ("gdi + 2000x900 pixelate", "gdi", Some(('B', (400, 300), (2400, 1200)))),
    ];
    for (i, (label, renderer, draw)) in cases.into_iter().enumerate() {
        let root = config_root(&format!("mem-{i}"), renderer);
        let out = root.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let log = root.join("mem.log");
        let _ = std::fs::remove_file(&log);
        let mut child = rustshot(&root)
            .env("RUSTSHOT_MEMLOG", &log)
            .args(["gui", "--region", "all", "--path"])
            .arg(&out)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn rustshot");
        let hwnd = wait_window(&child, 8000).expect("editor window");
        std::thread::sleep(Duration::from_millis(500));
        if let Some((tool, (x0, y0), (x1, y1))) = draw {
            key(hwnd, tool as u32);
            mouse_move(hwnd, x0, y0);
            mouse_down(hwnd, x0, y0);
            mouse_move(hwnd, (x0 + x1) / 2, (y0 + y1) / 2);
            mouse_move(hwnd, x1, y1);
            mouse_up(hwnd, x1, y1);
            std::thread::sleep(Duration::from_millis(500));
        }
        key(hwnd, 0x0D);
        assert_eq!(wait_exit(&mut child, 15000), Some(0), "{label}: save should succeed");
        let saved = std::fs::read_dir(&out).unwrap().count();
        assert_eq!(saved, 1, "{label}: one PNG saved");
        let text = std::fs::read_to_string(&log).expect("memlog written");
        let line = |phase: &str| text.lines().find(|l| l.contains(&format!("phase={phase} "))).unwrap_or("").to_string();
        let field = |line: &str, k: &str| -> u64 {
            line.split_whitespace()
                .find_map(|kv| kv.strip_prefix(k).and_then(|v| v.strip_prefix('=')))
                .and_then(|v| v.parse().ok())
                .unwrap_or(0)
        };
        let mb = |b: u64| b as f64 / (1024.0 * 1024.0);
        let (ov, ex) = (line("overlay"), line("exit"));
        println!(
            "MEM {label}: peak private {:.1} MiB before export, {:.1} MiB overall (export crop + PNG included); peak working set {:.1} MiB",
            mb(field(&ov, "peak_private")),
            mb(field(&ex, "peak_private")),
            mb(field(&ex, "peak_working_set"))
        );
    }
}

/// Screen pixels of `w` x `h` at (`x`, `y`), BGRA (what DWM shows).
fn screen_pixels(x: i32, y: i32, w: i32, h: i32) -> Vec<u8> {
    use windows::Win32::Graphics::Gdi::*;
    unsafe {
        let screen = GetDC(None);
        let dc = CreateCompatibleDC(Some(screen));
        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = w;
        bmi.bmiHeader.biHeight = -h;
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB.0;
        let mut bits: *mut core::ffi::c_void = core::ptr::null_mut();
        let bmp = CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0).expect("dib");
        let old = SelectObject(dc, HGDIOBJ(bmp.0));
        let _ = BitBlt(dc, 0, 0, w, h, Some(screen), x, y, SRCCOPY);
        let _ = GdiFlush();
        let v = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize).to_vec();
        SelectObject(dc, old);
        let _ = DeleteObject(HGDIOBJ(bmp.0));
        let _ = DeleteDC(dc);
        ReleaseDC(None, screen);
        v
    }
}

/// The overlay as shown on screen is the same with either renderer:
/// open it, read the screen back, compare. Run against a release build
/// (`RUSTSHOT_E2E_BIN`): a debug build composes a 5K frame in ~100 ms and
/// sometimes shows a stale fade-in frame (also before the GDI renderer).
/// Writes both captures as PNGs to `RUSTSHOT_E2E_OUT` when set.
#[test]
#[ignore = "live display access"]
fn renderers_show_the_same_overlay() {
    use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let (x, y, w, h) = (0, 0, 1400, 900);
    let mut shots = Vec::new();
    for renderer in ["software", "gdi"] {
        let root = config_root(&format!("same-{renderer}"), renderer);
        let mut child = rustshot(&root)
            .args(["gui", "--region", "800x600+200+150", "--clip"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn rustshot");
        let hwnd = wait_window(&child, 8000).expect("editor window");
        std::thread::sleep(Duration::from_millis(1500));
        mouse_move(hwnd, 600, 450);
        std::thread::sleep(Duration::from_millis(1000));
        shots.push(screen_pixels(x, y, w, h));
        key(hwnd, 0x1B);
        assert_eq!(wait_exit(&mut child, 5000), Some(2));
        std::thread::sleep(Duration::from_millis(300));
    }
    if let Some(dir) = std::env::var_os("RUSTSHOT_E2E_OUT") {
        for (name, px) in ["software", "gdi"].iter().zip(&shots) {
            let mut rgba = px.clone();
            for p in rgba.chunks_exact_mut(4) {
                p.swap(0, 2);
                p[3] = 255;
            }
            let f = std::fs::File::create(Path::new(&dir).join(format!("overlay-{name}.png"))).unwrap();
            let mut enc = png::Encoder::new(std::io::BufWriter::new(f), w as u32, h as u32);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header().unwrap().write_image_data(&rgba).unwrap();
        }
    }
    // Each run captures the live desktop anew, so changing windows behind
    // the overlay (a terminal printing) differ; the chrome must not. The
    // toolbar sits below the selection: 800x600 at (200, 150).
    let (a, b) = (&shots[0], &shots[1]);
    let mut n = 0;
    let (mut bar_max, mut bar_at) = (0u8, (0, 0));
    for i in 0..(w * h) as usize {
        let d = (0..3).map(|c| a[i * 4 + c].abs_diff(b[i * 4 + c])).max().unwrap();
        if d > 0 {
            n += 1;
        }
        let (px, py) = ((i % w as usize) as i32, (i / w as usize) as i32);
        if (290..990).contains(&px) && (762..798).contains(&py) && d > bar_max {
            bar_max = d;
            bar_at = (px, py);
        }
    }
    println!("differing pixels: {n} of {}, toolbar max diff {bar_max} at {bar_at:?}", w * h);
    // The toolbar surface is 96% opaque: a changed window behind it shows
    // through by up to ~10 levels.
    assert!(bar_max <= 12, "toolbar differs by {bar_max}");
    assert!((n as f64) < 0.1 * (w * h) as f64, "overlays differ in {n} pixels");
}
