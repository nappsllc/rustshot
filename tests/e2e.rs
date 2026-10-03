//! End-to-end interaction tests: launch the real binary and drive it with
//! posted Win32 messages (keys, mouse, chars). Ignored by default like the
//! other live tests; run with:
//!
//!     cargo test --test e2e -- --ignored
#![cfg(windows)]

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowThreadProcessId, IsWindowVisible, PostMessageW, WM_CHAR, WM_KEYDOWN,
    WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
};

const BIN: &str = env!("CARGO_BIN_EXE_rustshot");

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

fn launch_editor() -> (Child, HWND) {
    let child = Command::new(BIN)
        .args(["gui", "--region", "800x600+20+20", "--clip"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn rustshot");
    let hwnd = wait_window(&child, 8000).expect("editor window");
    (child, hwnd)
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
    let (mut child, hwnd) = launch_editor();
    key(hwnd, 0x1B); // VK_ESCAPE
    assert_eq!(wait_exit(&mut child, 5000), Some(2), "esc should cancel");
}

#[test]
#[ignore = "live display access"]
fn rect_draw_then_enter_exports_zero() {
    let (mut child, hwnd) = launch_editor();
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
    let (mut child, hwnd) = launch_editor();
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
    let mut child = Command::new(BIN)
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
