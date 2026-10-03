//! macOS export backend: NSPasteboard clipboard, the native `NSSavePanel`
//! save dialog, and the curl Imgur upload (same wire format as WinHTTP).

use super::*;

use crate::wind::{msg0, msg1, msg2, ns_string, objc_cls, objc_sel};
use core::ffi::{c_char, c_void, CStr};

// The AppKit/Foundation classes below are reached through objc_msgSend only;
// item-less extern blocks still emit their -framework flags so the classes
// exist even before wind's pump has created NSApplication (direct CLI
// captures copy to the clipboard without ever opening the overlay).
#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {}

#[link(name = "Foundation", kind = "framework")]
unsafe extern "C" {}

/// Per-call autorelease pool: export can run outside wind's per-pass pool
/// (`wait_upload` copies the URL after `run` has returned).
struct Pool(*mut c_void);

impl Pool {
    fn new() -> Self {
        unsafe {
            let cls = objc_cls(c"NSAutoreleasePool");
            if cls.is_null() {
                return Pool(core::ptr::null_mut());
            }
            Pool(msg0(cls, objc_sel(c"init")))
        }
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _: () = msg0(self.0, objc_sel(c"drain"));
            }
        }
    }
}

/// Open the system clipboard and clear it; shared by the two copy fns.
fn open_pasteboard() -> Option<*mut c_void> {
    unsafe {
        let cls = objc_cls(c"NSPasteboard");
        if cls.is_null() {
            return None;
        }
        let pb: *mut c_void = msg0(cls, objc_sel(c"generalPasteboard"));
        if pb.is_null() {
            return None;
        }
        let _: i64 = msg0(pb, objc_sel(c"clearContents")); // NSInteger
        Some(pb)
    }
}

pub fn copy_to_clipboard(img: &PixBuf) -> Result<()> {
    let png = img.to_png()?;
    let _pool = Pool::new();
    let pb = open_pasteboard()
        .ok_or_else(|| anyhow!("failed to reach the system pasteboard"))?;
    unsafe {
        // "public.png" is the UTI every mac app reads PNG data from (the
        // CF_PNG counterpart of export_win).
        let data: *mut c_void = msg2(
            objc_cls(c"NSData"),
            objc_sel(c"dataWithBytes:length:"),
            png.as_ptr(),
            png.len(),
        );
        if data.is_null() {
            return Err(anyhow!("clipboard data allocation failed"));
        }
        let ok: i8 = msg2(pb, objc_sel(c"setData:forType:"), data, ns_string("public.png"));
        if ok == 0 {
            return Err(anyhow!("pasteboard rejected PNG data"));
        }
    }
    Ok(())
}

pub fn copy_text_to_clipboard(text: &str) -> Result<()> {
    let _pool = Pool::new();
    let pb = open_pasteboard()
        .ok_or_else(|| anyhow!("failed to reach the system pasteboard"))?;
    unsafe {
        let ok: i8 = msg2(
            pb,
            objc_sel(c"setString:forType:"),
            ns_string(text),
            ns_string("public.utf8-plain-text"),
        );
        if ok == 0 {
            return Err(anyhow!("pasteboard rejected text"));
        }
    }
    Ok(())
}

/// Native save dialog via `NSSavePanel` (the GetSaveFileNameW equivalent);
/// cancel/error returns None like the Win32 dialog does.
pub fn save_dialog(dir: &Path, suggested: &str) -> Option<PathBuf> {
    let _pool = Pool::new();
    unsafe {
        // Idempotent singleton creation — the same call wind's pump makes,
        // so a panel outside the pump still finds a live NSApplication.
        let app_cls = objc_cls(c"NSApplication");
        if app_cls.is_null() {
            return None;
        }
        let _: *mut c_void = msg0(app_cls, objc_sel(c"sharedApplication"));
        let panel_cls = objc_cls(c"NSSavePanel");
        if panel_cls.is_null() {
            return None;
        }
        let panel: *mut c_void = msg0(panel_cls, objc_sel(c"savePanel"));
        if panel.is_null() {
            return None;
        }
        let url: *mut c_void = msg1(
            objc_cls(c"NSURL"),
            objc_sel(c"fileURLWithPath:"),
            ns_string(&dir.to_string_lossy()),
        );
        if !url.is_null() {
            let _: () = msg1(panel, objc_sel(c"setDirectoryURL:"), url);
        }
        let _: () = msg1(
            panel,
            objc_sel(c"setNameFieldStringValue:"),
            ns_string(suggested),
        );
        let _: () = msg1(panel, objc_sel(c"setCanCreateDirectories:"), 1i64);
        let code: i64 = msg0(panel, objc_sel(c"runModal"));
        if code != 1 {
            return None; // NSModalResponseOK is 1; anything else = cancelled
        }
        let url: *mut c_void = msg0(panel, objc_sel(c"URL"));
        if url.is_null() {
            return None;
        }
        let path: *mut c_void = msg0(url, objc_sel(c"path"));
        if path.is_null() {
            return None;
        }
        let utf8: *const c_char = msg0(path, objc_sel(c"UTF8String"));
        if utf8.is_null() {
            return None;
        }
        let mut p = PathBuf::from(CStr::from_ptr(utf8).to_string_lossy().into_owned());
        if p.extension().is_none() {
            p.set_extension("png");
        }
        Some(p)
    }
}

/// Imgur upload via `curl` (the OS TLS stack, mirroring export_win's WinHTTP
/// approach instead of adding a Rust HTTP/TLS crate; the image travels on
/// stdin, so there are no temp files to clean up).
pub fn do_upload(png: &[u8], client_id: &str) -> Result<String, String> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut child = Command::new("curl")
        .args(["-sS", "--max-time", "60"])
        .args(["-X", "POST"])
        .arg("-H")
        .arg(format!("Authorization: Client-ID {client_id}"))
        .args(["-H", "Content-Type: application/octet-stream"])
        .args(["--data-binary", "@-"])
        .arg("https://api.imgur.com/3/image?title=rustshot&description=rustshot%20capture")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn curl: {e}"))?;
    {
        let mut stdin = child.stdin.take().ok_or_else(|| "curl stdin".to_string())?;
        stdin.write_all(png).map_err(|e| format!("write curl stdin: {e}"))?;
    } // dropping stdin closes the pipe so curl sees EOF
    let out = child.wait_with_output().map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "curl: {}{}",
            String::from_utf8_lossy(&out.stderr),
            String::from_utf8_lossy(&out.stdout)
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    extract_json_string(&text, "link").ok_or_else(|| format!("no link in response: {text}"))
}

/// Minimal `"key": "value"` extraction; a verbatim copy of export_win's
/// helper (shared sources are read-only for this change set).
fn extract_json_string(body: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let i = body.find(&needle)? + needle.len();
    let rest = &body[i..];
    let q1 = rest.find('"')?;
    let q2 = rest[q1 + 1..].find('"')? + q1 + 1;
    Some(rest[q1 + 1..q2].to_string())
}
