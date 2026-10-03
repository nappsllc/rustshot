use crate::config::Config;
use anyhow::{anyhow, Context, Result};
use chrono::Local;
use image::RgbaImage;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

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

/// Format a Flameshot-style filename pattern (`%F`, `%H`, `%M`, ...).
pub fn format_filename(pattern: &str) -> String {
    let now = Local::now();
    let mut out = String::with_capacity(pattern.len() + 8);
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let Some(tok) = chars.next() else { break };
        let fmt = match tok {
            'F' => "%Y-%m-%d",
            'T' => "%H:%M:%S",
            'R' => "%H:%M",
            'H' => "%H",
            'M' => "%M",
            'S' => "%S",
            'Y' => "%Y",
            'y' => "%y",
            'm' => "%m",
            'd' => "%d",
            'j' => "%j",
            'p' => "%p",
            'I' => "%I",
            's' => "%s",
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
        out.push_str(&now.format(fmt).to_string());
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

fn save_image(img: &RgbaImage, path: &Path, jpeg_quality: u8) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).context("create save directory")?;
        }
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "jpg" | "jpeg" => {
            let file = std::fs::File::create(path).context("create file")?;
            let mut writer = std::io::BufWriter::new(file);
            let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(
                &mut writer,
                jpeg_quality.clamp(1, 100),
            );
            // JPEG has no alpha; flatten onto white.
            let rgb: image::RgbImage = image::ImageBuffer::from_fn(img.width(), img.height(), |x, y| {
                let p = img.get_pixel(x, y).0;
                let a = p[3] as u32;
                let blend = |c: u8| ((c as u32 * a + 255 * (255 - a)) / 255) as u8;
                image::Rgb([blend(p[0]), blend(p[1]), blend(p[2])])
            });
            enc.encode_image(&rgb).context("encode jpeg")?;
        }
        _ => {
            img.save(path).context("write png")?;
        }
    }
    Ok(())
}

pub fn copy_to_clipboard(img: &RgbaImage) -> Result<()> {
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

pub fn png_bytes(img: &RgbaImage) -> Result<Vec<u8>> {
    let mut buf = std::io::Cursor::new(Vec::new());
    let enc = image::codecs::png::PngEncoder::new(&mut buf);
    image::ImageEncoder::write_image(
        enc,
        img.as_raw(),
        img.width(),
        img.height(),
        image::ExtendedColorType::Rgba8,
    )
    .context("encode png")?;
    Ok(buf.into_inner())
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
    img: &RgbaImage,
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
            let picked = rfd::FileDialog::new()
                .set_directory(&dir)
                .set_file_name(&suggested)
                .add_filter("PNG image", &["png"])
                .add_filter("JPEG image", &["jpg", "jpeg"])
                .save_file();
            picked.ok_or_else(|| anyhow!("save dialog cancelled"))
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

fn do_upload(png: &[u8], client_id: &str) -> Result<String, String> {
    let resp = ureq::post("https://api.imgur.com/3/image")
        .set("Authorization", &format!("Client-ID {client_id}"))
        .query("title", "rustshot")
        .query("description", "rustshot capture")
        .send_bytes(png)
        .map_err(|e| e.to_string())?;
    let body = resp.into_string().map_err(|e| e.to_string())?;
    extract_json_string(&body, "link").ok_or_else(|| format!("no link in response: {body}"))
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
}
