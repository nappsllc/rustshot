use crate::config::Config;
use crate::pixbuf::PixBuf;
use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
    let (ty, tmo, td, th, tmi, ts) = local_ymdhms();
    let (y, mo, d) = (ty as i64, tmo as i64, td as i64);
    let (h, mi, s) = (th as i64, tmi as i64, ts as i64);
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
            'j' => format!("{:03}", day_of_year(ty, tmo as u16, td as u16)),
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

/// Local (year, month, day, hour, min, sec) wall-clock time.
#[cfg(unix)]
fn local_ymdhms() -> (i32, u32, u32, u32, u32, u32) {
    #[repr(C)]
    #[derive(Default)]
    struct Tm {
        sec: i32,
        min: i32,
        hour: i32,
        mday: i32,
        mon: i32,
        year: i32,
        wday: i32,
        yday: i32,
        isdst: i32,
        gmtoff: i64,
        zone: usize,
    }
    unsafe extern "C" {
        fn time(t: *mut i64) -> i64;
        fn localtime_r(t: *const i64, tm: *mut Tm) -> *mut Tm;
    }
    unsafe {
        let now = time(core::ptr::null_mut());
        let mut tm = Tm::default();
        if localtime_r(&now, &mut tm).is_null() {
            return (1970, 1, 1, 0, 0, 0);
        }
        (
            tm.year + 1900,
            tm.mon as u32 + 1,
            tm.mday as u32,
            tm.hour as u32,
            tm.min as u32,
            tm.sec as u32,
        )
    }
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

use imp::save_dialog;

#[cfg(windows)]
#[path = "export_win.rs"]
mod imp;
#[cfg(target_os = "linux")]
#[path = "export_linux.rs"]
mod imp;
#[cfg(target_os = "macos")]
#[path = "export_macos.rs"]
mod imp;

pub use imp::*;

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
