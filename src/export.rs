use crate::config::Config;
use anyhow::{anyhow, Context, Result};
use crate::pixbuf::PixBuf;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use windows::Win32::System::SystemInformation::GetLocalTime;

#[derive(Clone, Debug)]
pub enum Task {
    /// Save to a specific file or directory; `None` = ask with a dialog.
    Save { path: Option<PathBuf> },
    Copy,
    /// Print `WxH+X+Y` of the selection to stdout.
    Geometry,
    /// Print raw PNG bytes to stdout.
    Raw,
    Upload,
}

/// Day of year (1-366) for a civil date.
fn day_of_year(year: i32, month: u16, day: u16) -> u32 {
    const CUM: [u32; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let m = month.clamp(1, 12) as usize - 1;
    CUM[m] + day as u32 + if leap && month > 2 { 1 } else { 0 }
}

/// Format a Flameshot-style filename pattern (`%F`, `%H`, `%M`, ...).
pub fn format_filename(pattern: &str) -> String {
    let t = unsafe { GetLocalTime() };
    let (y, mo, d) = (t.wYear as i64, t.wMonth as i64, t.wDay as i64);
    let (h, mi, s) = (t.wHour as i64, t.wMinute as i64, t.wSecond as i64);
    let epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut out = String::with_capacity(pattern.len() + 8);
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let Some(tok) = chars.next() else { break };
        let piece = match tok {
            'F' => format!("{y:04}-{mo:02}-{d:02}"),
            'T' => format!("{h:02}:{mi:02}:{s:02}"),
            'R' => format!("{h:02}:{mi:02}"),
            'H' => format!("{h:02}"),
            'M' => format!("{mi:02}"),
            'S' => format!("{s:02}"),
            'Y' => format!("{y:04}"),
            'y' => format!("{:02}", y.rem_euclid(100)),
            'm' => format!("{mo:02}"),
            'd' => format!("{d:02}"),
            'j' => format!("{:03}", day_of_year(t.wYear as i32, t.wMonth, t.wDay)),
            'p' => (if h < 12 { "AM" } else { "PM" }).to_string(),
            'I' => {
                let h12 = h % 12;
                format!("{:02}", if h12 == 0 { 12 } else { h12 })
            }
            's' => format!("{epoch}"),
            '%' => {
                out.push('%');
                continue;
            }
            other => {
                // Unknown token: keep it literal.
                out.push('%');
                out.push(other);
                continue;
            }
        };
        out.push_str(&piece);
    }
    out.replace(':', "-").replace('/', "\u{2044}")
}

pub fn default_save_dir(cfg: &Config) -> PathBuf {
    if !cfg.save_path.trim().is_empty() {
        return PathBuf::from(&cfg.save_path);
    }
    if let Ok(pics) = std::env::var("USERPROFILE") {
        let p = PathBuf::from(pics).join("Pictures");
        if p.exists() {
            return p;
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Append `_1`, `_2`, ... before the extension until the path is free.
pub fn unique_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "capture".into());
    let ext = path.extension().map(|e| e.to_string_lossy().to_string());
    for i in 1..10_000 {
        let name = match &ext {
            Some(e) => format!("{stem}_{i}.{e}"),
            None => format!("{stem}_{i}"),
        };
        let candidate = path.with_file_name(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    path.to_path_buf()
}

fn save_image(img: &PixBuf, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).context("create save directory")?;
        }
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if ext == "jpg" || ext == "jpeg" {
        return Err(anyhow!(
            "JPEG output is not supported (PNG only); use a .png path: {}",
            path.display()
        ));
    }
    img.save(path).context("write png")?;
    Ok(())
}

pub fn copy_to_clipboard(img: &PixBuf) -> Result<()> {
    let mut cb = arboard::Clipboard::new().context("open clipboard")?;
    let data = arboard::ImageData {
        width: img.width() as usize,
        height: img.height() as usize,
        bytes: std::borrow::Cow::Owned(img.as_raw().clone()),
    };
    cb.set_image(data).context("set clipboard image")?;
    // Keep the clipboard contents after we exit.
    std::mem::forget(cb);
    Ok(())
}

pub fn copy_text_to_clipboard(text: &str) -> Result<()> {
    let mut cb = arboard::Clipboard::new().context("open clipboard")?;
    cb.set_text(text.to_string()).context("set clipboard text")?;
    std::mem::forget(cb);
    Ok(())
}

pub fn png_bytes(img: &PixBuf) -> Result<Vec<u8>> {
    img.to_png()
}

pub struct ExportResult {
    pub messages: Vec<String>,
    pub error: bool,
    /// Handle for a spawned upload; the caller must wait for it.
    pub upload: Option<Receiver<Result<String, String>>>,
}

/// Run the synchronous export tasks against the final (cropped) image.
/// `sel_global` is the selection rect in virtual-screen physical coords.
pub fn run_export(
    img: &PixBuf,
    sel_global: (i32, i32),
    tasks: &[Task],
    cfg: &Config,
) -> ExportResult {
    let mut messages = Vec::new();
    let mut error = false;
    let mut upload = None;

    for task in tasks {
        match task {
            Task::Geometry => {
                println!("{}x{}+{}+{}", img.width(), img.height(), sel_global.0, sel_global.1);
            }
            Task::Raw => match png_bytes(img) {
                Ok(bytes) => {
                    use std::io::Write;
                    let out = std::io::stdout();
                    let mut lock = out.lock();
                    if let Err(e) = lock.write_all(&bytes).and_then(|_| lock.flush()) {
                        messages.push(format!("error: write png to stdout: {e}"));
                        error = true;
                    }
                }
                Err(e) => {
                    messages.push(format!("error: encode png: {e:#}"));
                    error = true;
                }
            },
            Task::Copy => match copy_to_clipboard(img) {
                Ok(()) => messages.push("copied to clipboard".to_string()),
                Err(e) => {
                    messages.push(format!("error: clipboard: {e:#}"));
                    error = true;
                }
            },
            Task::Save { path } => match resolve_save_path(path, cfg) {
                Ok(p) => {
                    let target = unique_path(&p);
                    match save_image(img, &target) {
                        Ok(()) => messages.push(format!("saved: {}", target.display())),
                        Err(e) => {
                            messages.push(format!("error: save: {e:#}"));
                            error = true;
                        }
                    }
                }
                Err(e) => {
                    messages.push(format!("error: save: {e:#}"));
                    error = true;
                }
            },
            Task::Upload => {
                let bytes = match png_bytes(img) {
                    Ok(b) => b,
                    Err(e) => {
                        messages.push(format!("error: encode for upload: {e:#}"));
                        error = true;
                        continue;
                    }
                };
                upload = Some(spawn_upload(bytes, cfg.upload_client_id.clone()));
            }
        }
    }
    ExportResult {
        messages,
        error,
        upload,
    }
}

/// Figure out where `Save` should write, possibly asking the user.
fn resolve_save_path(path: &Option<PathBuf>, cfg: &Config) -> Result<PathBuf> {
    match path {
        Some(p) => {
            if p.is_dir() {
                let ext = "png";
                let name = format!("{}.{}", format_filename(&cfg.filename_pattern), ext);
                Ok(p.join(name))
            } else if p.extension().is_some() {
                Ok(p.clone())
            } else {
                // Treat as a directory even if it does not exist yet.
                let name = format!("{}.png", format_filename(&cfg.filename_pattern));
                Ok(p.join(name))
            }
        }
        None => {
            let dir = default_save_dir(cfg);
            let suggested = format!("{}.png", format_filename(&cfg.filename_pattern));
            save_dialog(&dir, &suggested).ok_or_else(|| anyhow!("save dialog cancelled"))
        }
    }
}

/// Native save dialog via the classic GetSaveFileNameW (replaces rfd).
fn save_dialog(dir: &Path, suggested: &str) -> Option<PathBuf> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::UI::Controls::Dialogs::{
        GetSaveFileNameW, OFN_NOCHANGEDIR, OFN_OVERWRITEPROMPT, OFN_PATHMUSTEXIST, OPENFILENAMEW,
    };
    use windows::core::{PCWSTR, PWSTR};

    let mut file = vec![0u16; 1024];
    let name: Vec<u16> = suggested.encode_utf16().collect();
    let n = name.len().min(file.len());
    file[..n].copy_from_slice(&name[..n]);
    let dir_w: Vec<u16> = dir
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let filters: Vec<u16> = "PNG image\0*.png\0\0".encode_utf16().collect();
    unsafe {
        let mut ofn: OPENFILENAMEW = std::mem::zeroed();
        ofn.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
        ofn.lpstrFilter = PCWSTR(filters.as_ptr());
        ofn.nFilterIndex = 1;
        ofn.lpstrFile = PWSTR(file.as_mut_ptr());
        ofn.nMaxFile = file.len() as u32;
        ofn.lpstrInitialDir = PCWSTR(dir_w.as_ptr());
        ofn.Flags = OFN_OVERWRITEPROMPT | OFN_NOCHANGEDIR | OFN_PATHMUSTEXIST;
        if !GetSaveFileNameW(&mut ofn).as_bool() {
            return None;
        }
    }
    let len = file.iter().position(|&c| c == 0).unwrap_or(file.len());
    let mut p = PathBuf::from(String::from_utf16_lossy(&file[..len]));
    if p.extension().is_none() {
        p.set_extension("png");
    }
    Some(p)
}

/// Blocking wait used after the UI has exited (one-shot processes).
pub fn wait_upload(rx: Receiver<Result<String, String>>, copy_url: bool) -> Option<String> {
    match rx.recv_timeout(Duration::from_secs(120)) {
        Ok(Ok(url)) => {
            println!("uploaded: {url}");
            if copy_url
                && let Err(e) = copy_text_to_clipboard(&url) {
                    eprintln!("warning: could not copy URL: {e:#}");
                }
            Some(url)
        }
        Ok(Err(e)) => {
            eprintln!("error: upload failed: {e}");
            None
        }
        Err(_) => {
            eprintln!("error: upload timed out");
            None
        }
    }
}

fn spawn_upload(png: Vec<u8>, client_id: String) -> Receiver<Result<String, String>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let r = do_upload(&png, &client_id);
        let _ = tx.send(r);
    });
    rx
}

fn do_upload(png: &[u8], client_id: &str) -> Result<String, String> {
    upload_winhttp(png, client_id)
}

/// Imgur upload over WinHTTP: the OS TLS stack, no Rust HTTP/TLS dependency.
fn upload_winhttp(png: &[u8], client_id: &str) -> Result<String, String> {
    use std::ptr;
    use windows::Win32::Networking::WinHttp::{
        WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest,
        WinHttpQueryDataAvailable, WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest,
        WinHttpSetTimeouts, WINHTTP_ACCESS_TYPE_DEFAULT_PROXY, WINHTTP_FLAG_SECURE,
    };
    use windows::core::{PCWSTR, w};

    struct Handle(*mut std::ffi::c_void);
    impl Drop for Handle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    let _ = WinHttpCloseHandle(self.0);
                }
            }
        }
    }

    /// Null-terminated UTF-16, for PCWSTR parameters.
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    let err = |step: &str| format!("{step}: {}", std::io::Error::last_os_error());

    unsafe {
        let agent = wide("rustshot/0.1");
        let session = Handle(WinHttpOpen(
            PCWSTR(agent.as_ptr()),
            WINHTTP_ACCESS_TYPE_DEFAULT_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        ));
        if session.0.is_null() {
            return Err(err("WinHttpOpen"));
        }
        let _ = WinHttpSetTimeouts(session.0, 10_000, 10_000, 30_000, 30_000);

        let host = wide("api.imgur.com");
        let conn = Handle(WinHttpConnect(session.0, PCWSTR(host.as_ptr()), 443, 0));
        if conn.0.is_null() {
            return Err(err("WinHttpConnect"));
        }

        let object = wide("/3/image?title=rustshot&description=rustshot%20capture");
        let request = Handle(WinHttpOpenRequest(
            conn.0,
            PCWSTR(w!("POST").as_ptr()),
            PCWSTR(object.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            ptr::null(),
            WINHTTP_FLAG_SECURE,
        ));
        if request.0.is_null() {
            return Err(err("WinHttpOpenRequest"));
        }

        // Counted (not NUL-terminated): windows-rs passes slice.len() as the
        // header block length.
        let headers: Vec<u16> = format!(
            "Authorization: Client-ID {client_id}\r\nContent-Type: application/octet-stream"
        )
        .encode_utf16()
        .collect();
        WinHttpSendRequest(
            request.0,
            Some(&headers),
            Some(png.as_ptr() as *const _),
            png.len() as u32,
            png.len() as u32,
            0,
        )
        .map_err(|e| format!("WinHttpSendRequest: {e}"))?;
        WinHttpReceiveResponse(request.0, ptr::null_mut())
            .map_err(|e| format!("WinHttpReceiveResponse: {e}"))?;

        let mut body: Vec<u8> = Vec::new();
        loop {
            let mut avail = 0u32;
            WinHttpQueryDataAvailable(request.0, &mut avail)
                .map_err(|e| format!("WinHttpQueryDataAvailable: {e}"))?;
            if avail == 0 {
                break;
            }
            let mut buf = vec![0u8; avail as usize];
            let mut read = 0u32;
            WinHttpReadData(request.0, buf.as_mut_ptr() as _, avail, &mut read)
                .map_err(|e| format!("WinHttpReadData: {e}"))?;
            buf.truncate(read as usize);
            if buf.is_empty() {
                break;
            }
            body.extend_from_slice(&buf);
        }

        let text = String::from_utf8_lossy(&body);
        extract_json_string(&text, "link").ok_or_else(|| format!("no link in response: {text}"))
    }
}

/// Minimal `"key": "value"` extraction; avoids pulling in a JSON crate.
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
    fn formats_tokens() {
        let s = format_filename("%F_%H-%M");
        assert!(s.contains('_'), "{s}");
        assert!(!s.contains('%'), "{s}");
        assert_eq!(format_filename("shot"), "shot");
        assert_eq!(format_filename("a%Qb"), "a%Qb");
    }

    #[test]
    fn unique_paths() {
        let dir = std::env::temp_dir().join("rustshot_test_unique");
        let _ = std::fs::create_dir_all(&dir);
        let f = dir.join("x.png");
        let _ = std::fs::remove_file(&f);
        assert_eq!(unique_path(&f), f);
        std::fs::write(&f, b"hi").unwrap();
        let u = unique_path(&f);
        assert_ne!(u, f);
        let _ = std::fs::remove_file(&f);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn json_extract() {
        let body = r#"{"data":{"link":"https://i.imgur.com/abc.png","id":"abc"}}"#;
        assert_eq!(
            extract_json_string(body, "link").as_deref(),
            Some("https://i.imgur.com/abc.png")
        );
    }

    #[test]
    #[ignore = "live network upload to imgur"]
    fn live_imgur_upload() {
        let img = PixBuf::from_pixel(1, 1, [1, 2, 3, 255]);
        let png = png_bytes(&img).unwrap();
        match do_upload(&png, "313baf0c7b4d3ff") {
            Ok(link) => assert!(link.starts_with("https://i.imgur.com/"), "{link}"),
            // Transport verified either way; 429 means imgur rate-limits the
            // shared anonymous client-id, not that the request failed.
            Err(e) if e.contains("\"code\":\"429\"") => {}
            Err(e) => panic!("upload failed: {e}"),
        }
    }
}
