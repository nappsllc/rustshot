//! X11 export backend: CLIPBOARD selection serving (the `SetClipboardData`
//! equivalent), a zenity/kdialog save dialog, and the curl Imgur upload.

use super::*;

use core::ffi::{c_char, c_int, c_long, c_uint, c_ulong, c_void};
use std::sync::Mutex;

// Same Xlib symbols as wind/hotkey declare, with this module's own selection
// event layout (ABI-identical) — see wind_linux.rs for the rationale.
#[allow(clashing_extern_declarations)]
#[link(name = "X11")]
unsafe extern "C" {
    fn XOpenDisplay(name: *const c_char) -> *mut c_void;
    fn XCloseDisplay(dpy: *mut c_void) -> c_int;
    fn XDefaultRootWindow(dpy: *mut c_void) -> c_ulong;
    fn XCreateSimpleWindow(
        dpy: *mut c_void,
        parent: c_ulong,
        x: c_int,
        y: c_int,
        width: c_uint,
        height: c_uint,
        border_width: c_uint,
        border: c_ulong,
        background: c_ulong,
    ) -> c_ulong;
    fn XDestroyWindow(dpy: *mut c_void, w: c_ulong) -> c_int;
    fn XInternAtom(dpy: *mut c_void, name: *const c_char, only_if_exists: c_int) -> c_ulong;
    fn XSetSelectionOwner(dpy: *mut c_void, selection: c_ulong, owner: c_ulong, time: c_ulong);
    fn XGetSelectionOwner(dpy: *mut c_void, selection: c_ulong) -> c_ulong;
    fn XChangeProperty(
        dpy: *mut c_void,
        w: c_ulong,
        property: c_ulong,
        type_: c_ulong,
        format: c_int,
        mode: c_int,
        data: *const u8,
        nelements: c_int,
    ) -> c_int;
    fn XNextEvent(dpy: *mut c_void, event: *mut XEvent) -> c_int;
    fn XSendEvent(
        dpy: *mut c_void,
        w: c_ulong,
        propagate: c_int,
        event_mask: c_long,
        event: *mut XEvent,
    ) -> c_int;
    fn XFlush(dpy: *mut c_void) -> c_int;
}

#[repr(C)]
#[derive(Clone, Copy)]
struct XSelectionRequestEvent {
    type_: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: *mut c_void,
    owner: c_ulong,
    requestor: c_ulong,
    selection: c_ulong,
    target: c_ulong,
    property: c_ulong,
    time: c_ulong,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct XSelectionNotifyEvent {
    type_: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: *mut c_void,
    requestor: c_ulong,
    selection: c_ulong,
    target: c_ulong,
    property: c_ulong,
    time: c_ulong,
}

#[repr(C)]
#[derive(Clone, Copy)]
union XEvent {
    type_: c_int,
    request: XSelectionRequestEvent,
    notify: XSelectionNotifyEvent,
    pad: [c_ulong; 24],
}

const SELECTION_CLEAR: c_int = 29;
const SELECTION_REQUEST: c_int = 30;
const SELECTION_NOTIFY: c_int = 31;
/// Xatom.h: `#define XA_ATOM 4`.
const XA_ATOM: c_ulong = 4;
const PROP_MODE_REPLACE: c_int = 0;
const CURRENT_TIME: c_ulong = 0;

/// What we are offering on the CLIPBOARD for the lifetime of one worker.
enum Clip {
    Png(Vec<u8>),
    Text(String),
}

/// Atoms the ICCCM clipboard handshake needs (interned once per worker).
#[derive(Clone, Copy)]
struct Atoms {
    clipboard: c_ulong,
    targets: c_ulong,
    png: c_ulong,
    utf8: c_ulong,
    string: c_ulong,
}

fn intern_atoms(dpy: *mut c_void) -> Atoms {
    unsafe {
        Atoms {
            clipboard: XInternAtom(dpy, c"CLIPBOARD".as_ptr(), 0),
            targets: XInternAtom(dpy, c"TARGETS".as_ptr(), 0),
            png: XInternAtom(dpy, c"image/png".as_ptr(), 0),
            utf8: XInternAtom(dpy, c"UTF8_STRING".as_ptr(), 0),
            string: XInternAtom(dpy, c"STRING".as_ptr(), 0),
        }
    }
}

/// Live clipboard workers; at most one is alive (a new copy takes ownership
/// and the old worker exits on SelectionClear). Finished ones are reaped.
static WORKERS: Mutex<Vec<std::thread::JoinHandle<()>>> = Mutex::new(Vec::new());

fn remember_worker(handle: std::thread::JoinHandle<()>) {
    let mut list = WORKERS.lock().unwrap_or_else(|e| e.into_inner());
    list.retain(|w| !w.is_finished());
    list.push(handle);
}

/// Claim ownership of CLIPBOARD and serve conversion requests until another
/// client takes it over. Returns once ownership is established (like
/// `SetClipboardData` returning).
fn serve_clipboard(clip: Clip) -> Result<()> {
    // Probe on the caller's thread so a dead display is reported here, not
    // as a silently lost worker.
    unsafe {
        let probe = XOpenDisplay(core::ptr::null());
        if probe.is_null() {
            return Err(anyhow!("cannot open X display"));
        }
        XCloseDisplay(probe);
    }
    let (done_tx, done_rx) = mpsc::channel();
    let handle = std::thread::spawn(move || clipboard_worker(clip, done_tx));
    let result = done_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or_else(|_| Err(anyhow!("clipboard worker did not start")));
    if result.is_ok() {
        remember_worker(handle);
    }
    result
}

fn clipboard_worker(clip: Clip, done: mpsc::Sender<Result<()>>) {
    unsafe {
        let dpy = XOpenDisplay(core::ptr::null());
        if dpy.is_null() {
            let _ = done.send(Err(anyhow!("cannot open X display")));
            return;
        }
        let atoms = intern_atoms(dpy);
        let win = XCreateSimpleWindow(dpy, XDefaultRootWindow(dpy), 0, 0, 1, 1, 0, 0, 0);
        XSetSelectionOwner(dpy, atoms.clipboard, win, CURRENT_TIME);
        XFlush(dpy);
        if XGetSelectionOwner(dpy, atoms.clipboard) != win {
            XDestroyWindow(dpy, win);
            XCloseDisplay(dpy);
            let _ = done.send(Err(anyhow!("clipboard ownership was lost")));
            return;
        }
        let _ = done.send(Ok(()));
        loop {
            let mut ev: XEvent = core::mem::zeroed();
            XNextEvent(dpy, &mut ev);
            match ev.type_ {
                SELECTION_REQUEST => serve_request(dpy, atoms, &clip, ev.request),
                // Another client took CLIPBOARD; our data can't be pasted
                // anymore (there is no clipboard daemon to hand it to).
                SELECTION_CLEAR => break,
                _ => {}
            }
        }
        XDestroyWindow(dpy, win);
        XCloseDisplay(dpy);
    }
}

/// Answer one `XConvertSelection` request from another client.
fn serve_request(dpy: *mut c_void, a: Atoms, clip: &Clip, req: XSelectionRequestEvent) {
    unsafe {
        // Obsolete requestors may leave property unset: fall back to target.
        let mut prop = req.property;
        if prop == 0 {
            prop = req.target;
        }
        let converted = if req.target == a.targets {
            let list: Vec<c_ulong> = match clip {
                Clip::Png(_) => vec![a.targets, a.png],
                Clip::Text(_) => vec![a.targets, a.utf8, a.string],
            };
            // Format 32 = an array of C longs (8 bytes on LP64).
            XChangeProperty(
                dpy,
                req.requestor,
                prop,
                XA_ATOM,
                32,
                PROP_MODE_REPLACE,
                list.as_ptr().cast(),
                list.len() as c_int,
            );
            true
        } else if let Clip::Png(bytes) = clip
            && req.target == a.png
        {
            XChangeProperty(
                dpy,
                req.requestor,
                prop,
                a.png,
                8,
                PROP_MODE_REPLACE,
                bytes.as_ptr(),
                bytes.len() as c_int,
            );
            true
        } else if let Clip::Text(s) = clip {
            if req.target == a.utf8 {
                XChangeProperty(
                    dpy,
                    req.requestor,
                    prop,
                    a.utf8,
                    8,
                    PROP_MODE_REPLACE,
                    s.as_bytes().as_ptr(),
                    s.len() as c_int,
                );
                true
            } else if req.target == a.string {
                // STRING is Latin-1: replace anything outside it.
                let latin1: Vec<u8> = s
                    .chars()
                    .map(|c| if (c as u32) < 0x100 { c as u8 } else { b'?' })
                    .collect();
                XChangeProperty(
                    dpy,
                    req.requestor,
                    prop,
                    a.string,
                    8,
                    PROP_MODE_REPLACE,
                    latin1.as_ptr(),
                    latin1.len() as c_int,
                );
                true
            } else {
                false
            }
        } else {
            false
        };
        let mut out: XSelectionNotifyEvent = core::mem::zeroed();
        out.type_ = SELECTION_NOTIFY;
        out.display = dpy;
        out.requestor = req.requestor;
        out.selection = req.selection;
        out.target = req.target;
        out.property = if converted { prop } else { 0 };
        out.time = req.time;
        let mut ev: XEvent = core::mem::zeroed();
        ev.notify = out;
        XSendEvent(dpy, req.requestor, 0, 0, &mut ev);
        XFlush(dpy);
    }
}

pub fn copy_to_clipboard(img: &PixBuf) -> Result<()> {
    serve_clipboard(Clip::Png(img.to_png()?))
}

pub fn copy_text_to_clipboard(text: &str) -> Result<()> {
    serve_clipboard(Clip::Text(text.to_string()))
}

/// Ask the desktop for a save path: zenity (GTK) first, then kdialog (KDE).
/// A missing binary falls through; cancel/error returns None like the Win32
/// dialog does.
pub fn save_dialog(dir: &Path, suggested: &str) -> Option<PathBuf> {
    let default = dir.join(suggested);
    let zenity = std::process::Command::new("zenity")
        .args(["--file-selection", "--save", "--confirm-overwrite"])
        .arg(format!("--filename={}", default.display()))
        .output();
    if let Ok(out) = zenity {
        return finish_dialog(out);
    }
    let kdialog = std::process::Command::new("kdialog")
        .arg("--getsavefilename")
        .arg(&default)
        .output();
    if let Ok(out) = kdialog {
        return finish_dialog(out);
    }
    None
}

fn finish_dialog(out: std::process::Output) -> Option<PathBuf> {
    if !out.status.success() {
        return None; // cancelled
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let mut p = PathBuf::from(text);
    if p.extension().is_none() {
        p.set_extension("png");
    }
    Some(p)
}

/// Imgur upload via `curl` (the OS TLS stack, mirroring export_win's WinHTTP
/// approach instead of adding a Rust HTTP/TLS crate).
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

/// HTTPS GET via `curl`; a non-2xx status fails (`-f`).
pub fn http_get(host: &str, path: &str, headers: &[(&str, &str)]) -> Result<String, String> {
    use std::process::Command;

    let mut cmd = Command::new("curl");
    cmd.args(["-fsS", "--max-time", "15"]);
    for (k, v) in headers {
        cmd.arg("-H").arg(format!("{k}: {v}"));
    }
    cmd.arg(format!("https://{host}{path}"));
    let out = cmd.output().map_err(|e| format!("spawn curl: {e}"))?;
    if !out.status.success() {
        return Err(format!("curl: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_extract() {
        let body = r#"{"data":{"link":"https://i.imgur.com/abc.png","id":"abc"}}"#;
        assert_eq!(
            extract_json_string(body, "link").as_deref(),
            Some("https://i.imgur.com/abc.png")
        );
    }
}
