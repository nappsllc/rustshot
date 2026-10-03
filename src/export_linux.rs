//! Linux export stubs; real clipboard/dialog/upload comes later.

use super::*;

pub fn copy_to_clipboard(img: &PixBuf) -> Result<()> {
    let _ = img;
    Err(anyhow!("clipboard not implemented"))
}

pub fn copy_text_to_clipboard(text: &str) -> Result<()> {
    let _ = text;
    Err(anyhow!("clipboard not implemented"))
}

pub fn save_dialog(dir: &Path, suggested: &str) -> Option<PathBuf> {
    let _ = (dir, suggested);
    None
}

pub fn do_upload(png: &[u8], client_id: &str) -> Result<String, String> {
    let _ = (png, client_id);
    Err("upload not supported yet".into())
}
