use crate::config::Config;
use crate::pixbuf::PixBuf;
use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug)]
pub enum Task {
    /// Save to a specific file or directory. `None` = `auto_save_path`;
    /// with `ask`, a Save As dialog seeded with that folder and name.
    Save { path: Option<PathBuf>, ask: bool },
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

/// ISO weekday (1 = Monday .. 7 = Sunday) of a civil date.
fn weekday(year: i32, month: u32, day: u32) -> u32 {
    // Sakamoto's method: 0 = Sunday.
    const T: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let m = month.clamp(1, 12) as usize;
    let y = if m < 3 { year - 1 } else { year };
    let w = (y + y.div_euclid(4) - y.div_euclid(100) + y.div_euclid(400) + T[m - 1] + day as i32).rem_euclid(7);
    if w == 0 { 7 } else { w as u32 }
}

/// ISO 8601 week number (1-53) of a civil date.
fn iso_week(year: i32, month: u32, day: u32) -> u32 {
    // Years with 53 weeks start on a Thursday (or a Wednesday in leap years).
    let weeks = |y: i32| -> i32 {
        let p = |y: i32| (y + y.div_euclid(4) - y.div_euclid(100) + y.div_euclid(400)).rem_euclid(7);
        if p(y) == 4 || p(y - 1) == 3 { 53 } else { 52 }
    };
    let doy = day_of_year(year, month as u16, day as u16) as i32;
    let w = (doy - weekday(year, month, day) as i32 + 10) / 7;
    if w < 1 {
        weeks(year - 1) as u32
    } else if w > weeks(year) {
        1
    } else {
        w as u32
    }
}

/// Local wall-clock time: (year, month, day, hour, minute, second).
pub type Tm = (i32, u32, u32, u32, u32, u32);

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Image formats `Save` can write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Png,
    Jpeg,
    Bmp,
}

impl Format {
    /// Format for a file extension (case-insensitive, leading dot allowed).
    pub fn from_ext(ext: &str) -> Option<Format> {
        match ext.trim_start_matches('.').to_ascii_lowercase().as_str() {
            "png" => Some(Format::Png),
            "jpg" | "jpeg" => Some(Format::Jpeg),
            "bmp" => Some(Format::Bmp),
            _ => None,
        }
    }

    pub fn ext(self) -> &'static str {
        match self {
            Format::Png => "png",
            Format::Jpeg => "jpg",
            Format::Bmp => "bmp",
        }
    }

    /// The configured `save_format` (PNG if unrecognised).
    pub fn from_config(cfg: &Config) -> Format {
        Format::from_ext(cfg.save_format.trim()).unwrap_or(Format::Png)
    }
}

/// Encode `img` as `fmt`; `quality` (1-100) only affects JPEG.
pub fn encode(img: &PixBuf, fmt: Format, quality: u8) -> Result<Vec<u8>> {
    match fmt {
        Format::Png => img.to_png(),
        Format::Jpeg => jpeg_bytes(img, quality),
        Format::Bmp => bmp_bytes(img),
    }
}

/// Baseline JPEG; alpha dropped (capture pixels are opaque). 4:4:4 at
/// quality >= 90 keeps text sharp, 4:2:0 below that.
fn jpeg_bytes(img: &PixBuf, quality: u8) -> Result<Vec<u8>> {
    use jpeg_encoder::{ColorType, Encoder, SamplingFactor};
    let (w, h) = img.dimensions();
    let (Ok(w16), Ok(h16)) = (u16::try_from(w), u16::try_from(h)) else {
        return Err(anyhow!("image too large for JPEG ({w}x{h}; at most 65535 per side)"));
    };
    let q = quality.clamp(1, 100);
    let mut out = Vec::new();
    let mut enc = Encoder::new(&mut out, q);
    enc.set_sampling_factor(if q >= 90 {
        SamplingFactor::R_4_4_4
    } else {
        SamplingFactor::R_4_2_0
    });
    enc.encode(img.as_raw(), w16, h16, ColorType::Rgba)
        .map_err(|e| anyhow!("encode jpeg: {e}"))?;
    Ok(out)
}

/// 32-bit BI_BITFIELDS BMP, rows stored top-down (negative height).
fn bmp_bytes(img: &PixBuf) -> Result<Vec<u8>> {
    const HEADER: u32 = 14 + 40 + 12; // file header + BITMAPINFOHEADER + masks
    let (w, h) = img.dimensions();
    let size = (w as u64 * h as u64 * 4 + HEADER as u64)
        .try_into()
        .ok()
        .filter(|_| w <= i32::MAX as u32 && h <= i32::MAX as u32)
        .ok_or_else(|| anyhow!("image too large for BMP ({w}x{h})"))?;
    let mut o: Vec<u8> = Vec::with_capacity(size as usize);
    let u32le = |o: &mut Vec<u8>, v: u32| o.extend_from_slice(&v.to_le_bytes());
    o.extend_from_slice(b"BM");
    u32le(&mut o, size);
    u32le(&mut o, 0); // reserved
    u32le(&mut o, HEADER); // pixel data offset
    u32le(&mut o, 40); // BITMAPINFOHEADER size
    o.extend_from_slice(&(w as i32).to_le_bytes());
    o.extend_from_slice(&(-(h as i32)).to_le_bytes()); // negative = top-down
    o.extend_from_slice(&1u16.to_le_bytes()); // planes
    o.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
    u32le(&mut o, 3); // BI_BITFIELDS
    u32le(&mut o, size - HEADER); // image size
    u32le(&mut o, 2835); // 72 DPI in pixels per metre
    u32le(&mut o, 2835);
    u32le(&mut o, 0); // colours used
    u32le(&mut o, 0); // important colours
    for mask in [0x00FF_0000, 0x0000_FF00, 0x0000_00FF] {
        u32le(&mut o, mask); // R, G, B
    }
    for px in img.as_raw().as_chunks::<4>().0 {
        o.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
    }
    Ok(o)
}

/// Make one path component safe on every OS: `<>:"/\|?*` and control
/// characters become `-`, `.`/`..` components are dropped, trailing dots
/// and spaces are trimmed. May return an empty string.
pub fn sanitize_component(s: &str) -> String {
    let parts: Vec<String> = s
        .split(['/', '\\'])
        .map(|p| {
            let p: String = p
                .chars()
                .map(|c| if c.is_control() || "<>:\"|?*".contains(c) { '-' } else { c })
                .collect();
            p.trim_end_matches(['.', ' ']).to_string()
        })
        .filter(|p| !p.is_empty())
        .collect();
    parts.join("-")
}

/// Expanded, sanitised file stem (never empty).
fn file_stem(pattern: &str, now: Tm, epoch: u64) -> String {
    let s = sanitize_component(&expand_pattern(pattern, now, epoch));
    if s.is_empty() { "capture".into() } else { s }
}

/// Where Ctrl+S saves without asking:
/// `save_dir / [subfolder_pattern] / filename_pattern.ext`. The subfolder
/// pattern may nest with `/`; each level is sanitised and `..` dropped.
pub fn auto_save_path(cfg: &Config, now: Tm) -> PathBuf {
    auto_save_path_ext(cfg, now, Format::from_config(cfg).ext())
}

/// [`auto_save_path`] with an explicit extension (`"mp4"`, `"gif"`) in
/// place of the screenshot format's: recordings land beside captures.
pub fn auto_save_path_ext(cfg: &Config, now: Tm, ext: &str) -> PathBuf {
    let epoch = unix_now();
    let mut dir = default_save_dir(cfg);
    if cfg.save_subfolder {
        for part in expand_pattern(&cfg.subfolder_pattern, now, epoch).split(['/', '\\']) {
            let part = sanitize_component(part);
            if !part.is_empty() {
                dir.push(part);
            }
        }
    }
    dir.join(format!("{}.{ext}", file_stem(&cfg.filename_pattern, now, epoch)))
}

/// Format a Flameshot-style filename pattern (`%F`, `%H`, `%M`, ...) for
/// the current local time, sanitised for use as a file name.
pub fn format_filename(pattern: &str) -> String {
    file_stem(pattern, local_ymdhms(), unix_now())
}

/// Expand pattern tokens for `now` (`%s` = `epoch`); no sanitising.
fn expand_pattern(pattern: &str, now: Tm, epoch: u64) -> String {
    let (ty, tmo, td, th, tmi, ts) = now;
    let (y, mo, d) = (ty as i64, tmo as i64, td as i64);
    let (h, mi, s) = (th as i64, tmi as i64, ts as i64);
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
            'C' => format!("{:02}", y.div_euclid(100)),
            'e' => format!("{d}"),
            'V' => format!("{:02}", iso_week(ty, tmo, td)),
            'u' => format!("{}", weekday(ty, tmo, td)),
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
    out
}

/// Local (year, month, day, hour, min, sec) wall-clock time.
#[cfg(unix)]
pub fn local_ymdhms() -> Tm {
    #[repr(C)]
    #[derive(Default)]
    struct CTm {
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
        fn localtime_r(t: *const i64, tm: *mut CTm) -> *mut CTm;
    }
    unsafe {
        let now = time(core::ptr::null_mut());
        let mut tm = CTm::default();
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

/// `save_path`, or `<Pictures>/rustshot` when it is empty (falling back to
/// `<home>/rustshot`, then the current directory). Not created here.
pub fn default_save_dir(cfg: &Config) -> PathBuf {
    if !cfg.save_path.trim().is_empty() {
        return PathBuf::from(&cfg.save_path);
    }
    #[cfg(windows)]
    let home = std::env::var_os("USERPROFILE");
    #[cfg(unix)]
    let home = std::env::var_os("HOME");
    if let Some(home) = home.filter(|h| !h.is_empty()) {
        let home = PathBuf::from(home);
        let pics = home.join("Pictures");
        if pics.is_dir() {
            return pics.join("rustshot");
        }
        if home.is_dir() {
            return home.join("rustshot");
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

/// Write `img` in the format named by the path's extension.
fn save_image(img: &PixBuf, path: &Path, jpeg_quality: u8) -> Result<()> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_default();
    let fmt = Format::from_ext(&ext).ok_or_else(|| {
        anyhow!(
            "unsupported image format {ext:?} (use .png, .jpg or .bmp): {}",
            path.display()
        )
    })?;
    let bytes = encode(img, fmt, jpeg_quality)?;
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).context("create save directory")?;
    }
    std::fs::write(path, bytes).context("write image")?;
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
            Task::Save { path, ask } => match resolve_save_path(path, *ask, cfg) {
                Ok(p) => {
                    let target = unique_path(&p);
                    match save_image(img, &target, cfg.jpeg_quality) {
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
fn resolve_save_path(path: &Option<PathBuf>, ask: bool, cfg: &Config) -> Result<PathBuf> {
    let ext = Format::from_config(cfg).ext();
    match path {
        // A file path with an extension is used as-is; anything else is a
        // directory (even if it does not exist yet).
        Some(p) if !p.is_dir() && p.extension().is_some() => Ok(p.clone()),
        Some(p) => Ok(p.join(format!("{}.{ext}", format_filename(&cfg.filename_pattern)))),
        None if !ask => Ok(auto_save_path(cfg, local_ymdhms())),
        None => {
            let auto = auto_save_path(cfg, local_ymdhms());
            let dir = auto.parent().unwrap_or(Path::new("."));
            let name = auto
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            // Seed the dialog with the dated folder; undo its creation if
            // the user cancels or picks another folder.
            let created = create_dirs(dir);
            let chosen = save_dialog(dir, &name);
            if chosen.as_deref().and_then(Path::parent) != Some(dir) {
                for d in &created {
                    let _ = std::fs::remove_dir(d); // only succeeds if empty
                }
            }
            let mut p = chosen.ok_or_else(|| anyhow!("save dialog cancelled"))?;
            if p.extension().is_none() {
                p.set_extension(ext);
            }
            Ok(p)
        }
    }
}

/// Create `dir` and its missing parents; returns the ones created,
/// deepest first.
fn create_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut missing = Vec::new();
    let mut cur = Some(dir);
    while let Some(d) = cur {
        if d.as_os_str().is_empty() || d.exists() {
            break;
        }
        missing.push(d.to_path_buf());
        cur = d.parent();
    }
    if std::fs::create_dir_all(dir).is_err() {
        return Vec::new();
    }
    missing
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

#[cfg(not(windows))]
#[path = "export_curl.rs"]
mod curl;
#[cfg(not(windows))]
pub use curl::download_to;

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

    const NOW: Tm = (2026, 3, 7, 9, 5, 2);

    #[test]
    fn settings_tokens() {
        // 2026-03-07 is a Saturday in ISO week 10.
        let e = |p: &str, t: Tm| expand_pattern(p, t, 0);
        assert_eq!(e("%C|%e|%V|%u", NOW), "20|7|10|6");
        assert_eq!(e("%e", (2026, 3, 17, 0, 0, 0)), "17");
        assert_eq!(e("%C", (1999, 1, 1, 0, 0, 0)), "19");
        // Week-year boundaries (ISO 8601).
        for (t, w, u) in [
            ((2021, 1, 1), 53, 5),  // Friday, week 53 of 2020
            ((2021, 1, 4), 1, 1),   // Monday
            ((2024, 12, 30), 1, 1), // Monday, week 1 of 2025
            ((2026, 12, 31), 53, 4),
            ((2027, 1, 3), 53, 7),  // Sunday, still week 53 of 2026
            ((2023, 1, 1), 52, 7),
            ((2020, 12, 31), 53, 4),
        ] {
            assert_eq!(iso_week(t.0, t.1, t.2), w, "{t:?}");
            assert_eq!(weekday(t.0, t.1, t.2), u, "{t:?}");
        }
        assert_eq!(e("%V-%u", (2026, 10, 10, 0, 0, 0)), "41-6");
    }

    #[test]
    fn auto_save_paths() {
        let base = std::env::temp_dir().join("rustshot_auto");
        let cfg = Config {
            save_path: base.to_string_lossy().into(),
            ..Config::default()
        };
        assert_eq!(
            auto_save_path(&cfg, NOW),
            base.join("2026-03-07").join("2026-03-07_09-05.png")
        );
        let flat = Config { save_subfolder: false, save_format: "jpg".into(), ..cfg.clone() };
        assert_eq!(auto_save_path(&flat, NOW), base.join("2026-03-07_09-05.jpg"));
        let custom = Config {
            subfolder_pattern: "%Y/%m/../x:y".into(),
            filename_pattern: "shot %T".into(),
            save_format: "bmp".into(),
            ..cfg.clone()
        };
        assert_eq!(
            auto_save_path(&custom, NOW),
            base.join("2026").join("03").join("x-y").join("shot 09-05-02.bmp")
        );
        // Recordings: same folder and stem, their own extension.
        assert_eq!(
            auto_save_path_ext(&cfg, NOW, "mp4"),
            base.join("2026-03-07").join("2026-03-07_09-05.mp4")
        );
        assert_eq!(auto_save_path_ext(&flat, NOW, "gif"), base.join("2026-03-07_09-05.gif"));
        let empty = Config { filename_pattern: "".into(), subfolder_pattern: "..".into(), ..cfg };
        assert_eq!(auto_save_path(&empty, NOW), base.join("capture.png"));
    }

    #[test]
    fn default_dir_is_pictures_rustshot() {
        let d = default_save_dir(&Config::default());
        assert_eq!(d.file_name().unwrap(), "rustshot", "{}", d.display());
        let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).unwrap();
        let pics = PathBuf::from(home).join("Pictures");
        if pics.is_dir() {
            assert_eq!(d, pics.join("rustshot"));
        }
        let set = Config { save_path: "X:/shots".into(), ..Config::default() };
        assert_eq!(default_save_dir(&set), PathBuf::from("X:/shots"));
    }

    #[test]
    fn sanitizes_components() {
        assert_eq!(sanitize_component("a<b>c:d\"e|f?g*h"), "a-b-c-d-e-f-g-h");
        assert_eq!(sanitize_component("x\ty\u{1}z"), "x-y-z");
        assert_eq!(sanitize_component("a/b\\c"), "a-b-c");
        assert_eq!(sanitize_component("a/../b"), "a-b");
        assert_eq!(sanitize_component("../.."), "");
        assert_eq!(sanitize_component("."), "");
        assert_eq!(sanitize_component("name. . "), "name");
        assert_eq!(sanitize_component("v1.2 final"), "v1.2 final");
    }

    #[test]
    fn formats_from_ext() {
        assert_eq!(Format::from_ext("png"), Some(Format::Png));
        assert_eq!(Format::from_ext("PNG"), Some(Format::Png));
        assert_eq!(Format::from_ext("jpg"), Some(Format::Jpeg));
        assert_eq!(Format::from_ext("jpeg"), Some(Format::Jpeg));
        assert_eq!(Format::from_ext(".bmp"), Some(Format::Bmp));
        assert_eq!(Format::from_ext("gif"), None);
        assert_eq!(Format::from_ext(""), None);
        assert_eq!(Format::Jpeg.ext(), "jpg");
        let cfg = Config { save_format: "jpeg".into(), ..Config::default() };
        assert_eq!(Format::from_config(&cfg), Format::Jpeg);
        assert_eq!(Format::from_config(&Config::default()), Format::Png);
    }

    #[test]
    fn bmp_layout() {
        let mut img = PixBuf::new(2, 2);
        img.as_raw_mut().copy_from_slice(&[
            1, 2, 3, 255, 4, 5, 6, 255, //
            7, 8, 9, 255, 10, 11, 12, 255,
        ]);
        let b = encode(&img, Format::Bmp, 90).unwrap();
        let u32_at = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
        let i32_at = |i: usize| i32::from_le_bytes(b[i..i + 4].try_into().unwrap());
        assert_eq!(&b[0..2], b"BM");
        assert_eq!(u32_at(2), 66 + 16);
        assert_eq!(b.len(), 66 + 16);
        assert_eq!(u32_at(10), 66, "pixel offset");
        assert_eq!(u32_at(14), 40, "info header size");
        assert_eq!(i32_at(18), 2);
        assert_eq!(i32_at(22), -2, "negative height = top-down");
        assert_eq!(u16::from_le_bytes([b[26], b[27]]), 1);
        assert_eq!(u16::from_le_bytes([b[28], b[29]]), 32);
        assert_eq!(u32_at(30), 3, "BI_BITFIELDS");
        assert_eq!(u32_at(34), 16);
        assert_eq!(
            [u32_at(54), u32_at(58), u32_at(62)],
            [0x00FF_0000, 0x0000_FF00, 0x0000_00FF]
        );
        assert_eq!(
            &b[66..],
            &[3, 2, 1, 255, 6, 5, 4, 255, 9, 8, 7, 255, 12, 11, 10, 255]
        );
    }

    /// Width/height from the first SOF0..SOF2 marker.
    fn jpeg_dims(b: &[u8]) -> (u16, u16) {
        let mut i = 2;
        while i + 9 < b.len() {
            assert_eq!(b[i], 0xFF, "marker at {i}");
            let m = b[i + 1];
            let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
            if (0xC0..=0xC2).contains(&m) {
                let h = u16::from_be_bytes([b[i + 5], b[i + 6]]);
                let w = u16::from_be_bytes([b[i + 7], b[i + 8]]);
                return (w, h);
            }
            i += 2 + len;
        }
        panic!("no SOF marker");
    }

    #[test]
    fn jpeg_output() {
        // Noisy gradient so quality matters.
        let (w, h) = (37u32, 21u32);
        let mut img = PixBuf::new(w, h);
        for (i, px) in img.as_raw_mut().as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let v = (i as u32).wrapping_mul(2_654_435_761) >> 24;
            *px = [v as u8, (i * 7) as u8, (i % w as usize * 6) as u8, 255];
        }
        let hi = encode(&img, Format::Jpeg, 95).unwrap();
        let lo = encode(&img, Format::Jpeg, 30).unwrap();
        for b in [&hi, &lo] {
            assert_eq!(&b[..2], &[0xFF, 0xD8]);
            assert_eq!(&b[b.len() - 2..], &[0xFF, 0xD9]);
            assert_eq!(jpeg_dims(b), (w as u16, h as u16));
        }
        assert!(lo.len() < hi.len(), "q30 {} >= q95 {}", lo.len(), hi.len());
        assert!(encode(&PixBuf::new(70_000, 1), Format::Jpeg, 90).is_err());
    }

    #[test]
    fn saves_by_extension() {
        let dir = std::env::temp_dir().join("rustshot_test_formats");
        let _ = std::fs::remove_dir_all(&dir);
        let img = PixBuf::from_pixel(3, 2, [10, 20, 30, 255]);
        for (name, magic) in [("a.png", &[0x89, b'P'][..]), ("b.JPG", &[0xFF, 0xD8]), ("c.bmp", b"BM")] {
            let p = dir.join("sub").join(name);
            save_image(&img, &p, 80).unwrap();
            assert!(std::fs::read(&p).unwrap().starts_with(magic), "{name}");
        }
        assert!(save_image(&img, &dir.join("d.gif"), 80).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn explicit_paths() {
        let cfg = Config { save_format: "bmp".into(), ..Config::default() };
        let f = PathBuf::from("out/x.jpg");
        assert_eq!(resolve_save_path(&Some(f.clone()), false, &cfg).unwrap(), f);
        let d = resolve_save_path(&Some(PathBuf::from("no_such_dir_rs")), false, &cfg).unwrap();
        assert_eq!(d.parent().unwrap(), Path::new("no_such_dir_rs"));
        assert_eq!(d.extension().unwrap(), "bmp");
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
