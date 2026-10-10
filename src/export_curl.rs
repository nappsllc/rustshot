//! Release-asset download via `curl`, shared by the Linux and macOS
//! backends (both already shell out to curl for HTTP).

use crate::update::{CANCELLED, is_allowed_download_hop, is_safe_download_url};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Download a release asset to `dest`. Only an `update::is_safe_download_url`
/// start is accepted; curl follows up to 5 https-only redirects and the final
/// URL must pass `update::is_allowed_download_hop`. Progress is the growing
/// size of `dest`, polled while curl runs (total unknown); `progress`
/// returning false kills curl (error `update::CANCELLED`). A partial file is
/// deleted on any failure.
#[allow(dead_code)] // reached through update::fetch_verified (dialog: later task)
pub fn download_to(
    url: &str,
    dest: &Path,
    progress: &dyn Fn(u64, Option<u64>) -> bool,
) -> Result<(), String> {
    if !is_safe_download_url(url) {
        return Err("refusing to download from an unexpected URL".into());
    }
    let r = run_curl(url, dest, progress);
    if r.is_err() {
        let _ = std::fs::remove_file(dest);
    }
    r
}

fn run_curl(
    url: &str,
    dest: &Path,
    progress: &dyn Fn(u64, Option<u64>) -> bool,
) -> Result<(), String> {
    // A stale file must not be reused (curl would truncate it in place).
    let _ = std::fs::remove_file(dest);
    let mut child = Command::new("curl")
        .arg("-q") // first argument: ignore ~/.curlrc
        .args(["-fsSL", "--proto", "=https", "--proto-redir", "=https", "--max-redirs", "5"])
        .args(["--connect-timeout", "15", "-w", "%{url_effective}", "-o"])
        .arg(dest)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn curl: {e}"))?;
    let size = || std::fs::metadata(dest).map_or(0, |m| m.len());
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("curl: {e}"));
            }
        }
        if !progress(size(), None) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(CANCELLED.into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    // stdout/stderr are tiny (-s, -w), so reading after exit cannot deadlock.
    let out = child.wait_with_output().map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        return Err(format!("curl: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let effective = String::from_utf8_lossy(&out.stdout);
    if !is_allowed_download_hop(effective.trim()) {
        return Err("refusing a download redirected to an unexpected host".into());
    }
    let got = size();
    if !progress(got, Some(got)) {
        return Err(CANCELLED.into());
    }
    Ok(())
}
