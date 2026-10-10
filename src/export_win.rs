//! Win32 export backend: clipboard (CF_DIB/CF_PNG/CF_UNICODETEXT), native
//! save dialog, local time, and the WinHTTP Imgur upload.

use super::*;

use std::mem::size_of;
use windows::Win32::System::SystemInformation::GetLocalTime;

/// Local (year, month, day, hour, min, sec) wall-clock time.
pub fn local_ymdhms() -> (i32, u32, u32, u32, u32, u32) {
    let t = unsafe { GetLocalTime() };
    (
        t.wYear as i32,
        t.wMonth as u32,
        t.wDay as u32,
        t.wHour as u32,
        t.wMinute as u32,
        t.wSecond as u32,
    )
}

/// Run `f` with the system clipboard open (retries if another process holds it).
fn with_clipboard<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    use windows::Win32::System::DataExchange::{CloseClipboard, OpenClipboard};
    let mut last = String::new();
    for attempt in 0..6 {
        match unsafe { OpenClipboard(None) } {
            Ok(()) => {
                let r = f();
                let _ = unsafe { CloseClipboard() };
                return r;
            }
            Err(e) => {
                last = e.to_string();
                if attempt < 5 {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
    }
    Err(anyhow!("open clipboard: {last}"))
}

/// Place one buffer on the open clipboard as `format` (the OS takes ownership).
fn set_clip_data(format: u32, bytes: &[u8]) -> Result<()> {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::DataExchange::SetClipboardData;
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    unsafe {
        let hglb = GlobalAlloc(GMEM_MOVEABLE, bytes.len()).context("GlobalAlloc")?;
        let ptr = GlobalLock(hglb);
        if ptr.is_null() {
            return Err(anyhow!("GlobalLock failed"));
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len());
        let _ = GlobalUnlock(hglb);
        SetClipboardData(format, Some(HANDLE(hglb.0))).context("SetClipboardData")?;
    }
    Ok(())
}

/// BGRA (bottom-up BGRX) DIB payload for the `CF_DIB` clipboard format.
fn rgba_to_dib(img: &PixBuf) -> Vec<u8> {
    use windows::Win32::Graphics::Gdi::{BITMAPINFOHEADER, BI_RGB};
    let (w, h) = img.dimensions();
    let stride = w as usize * 4;
    let mut hdr: BITMAPINFOHEADER = unsafe { std::mem::zeroed() };
    hdr.biSize = size_of::<BITMAPINFOHEADER>() as u32;
    hdr.biWidth = w as i32;
    hdr.biHeight = h as i32; // positive = bottom-up
    hdr.biPlanes = 1;
    hdr.biBitCount = 32;
    hdr.biCompression = BI_RGB.0;
    let mut out = Vec::with_capacity(size_of::<BITMAPINFOHEADER>() + stride * h as usize);
    out.extend_from_slice(unsafe {
        std::slice::from_raw_parts(
            (&raw const hdr) as *const u8,
            size_of::<BITMAPINFOHEADER>(),
        )
    });
    let raw = img.as_raw();
    for y in (0..h).rev() {
        let row = &raw[y as usize * stride..(y as usize + 1) * stride];
        for px in row.as_chunks::<4>().0 {
            out.extend_from_slice(&[px[2], px[1], px[0], 0]);
        }
    }
    out
}

pub fn copy_to_clipboard(img: &PixBuf) -> Result<()> {
    use windows::Win32::System::DataExchange::{EmptyClipboard, RegisterClipboardFormatW};
    use windows::Win32::System::Ole::CF_DIB;
    use windows::core::w;
    let png = img.to_png()?;
    let dib = rgba_to_dib(img);
    with_clipboard(|| unsafe {
        EmptyClipboard().context("empty clipboard")?;
        // "PNG" is the community-standard registered format (CF_PNG).
        let fmt_png = RegisterClipboardFormatW(w!("PNG"));
        if fmt_png != 0 {
            set_clip_data(fmt_png, &png)?;
        }
        set_clip_data(CF_DIB.0 as u32, &dib)?;
        Ok(())
    })
}

pub fn copy_text_to_clipboard(text: &str) -> Result<()> {
    use windows::Win32::System::DataExchange::EmptyClipboard;
    use windows::Win32::System::Ole::CF_UNICODETEXT;
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes: Vec<u8> = wide.iter().flat_map(|u| u.to_le_bytes()).collect();
    with_clipboard(|| unsafe {
        EmptyClipboard().context("empty clipboard")?;
        set_clip_data(CF_UNICODETEXT.0 as u32, &bytes)
    })
}

/// The clipboard's text (CF_UNICODETEXT), if any.
#[allow(dead_code)] // Settings window (Task 8)
pub fn clipboard_text() -> Option<String> {
    use windows::Win32::Foundation::HGLOBAL;
    use windows::Win32::System::DataExchange::{GetClipboardData, IsClipboardFormatAvailable};
    use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    use windows::Win32::System::Ole::CF_UNICODETEXT;
    let fmt = CF_UNICODETEXT.0 as u32;
    unsafe { IsClipboardFormatAvailable(fmt) }.ok()?;
    with_clipboard(|| unsafe {
        let h = GetClipboardData(fmt).context("GetClipboardData")?;
        let hg = HGLOBAL(h.0);
        let ptr = GlobalLock(hg) as *const u16;
        if ptr.is_null() {
            return Err(anyhow!("GlobalLock failed"));
        }
        let max = GlobalSize(hg) / 2;
        let units = std::slice::from_raw_parts(ptr, max);
        let len = units.iter().position(|&u| u == 0).unwrap_or(max);
        let s = String::from_utf16_lossy(&units[..len]);
        let _ = GlobalUnlock(hg);
        Ok(s)
    })
    .ok()
}

/// Native save dialog via the classic GetSaveFileNameW (replaces rfd).
pub fn save_dialog(dir: &Path, suggested: &str) -> Option<PathBuf> {
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
    // Preselect the filter (and default extension) of the suggested name.
    let ext = Path::new(suggested)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_else(|| "png".into());
    let filter_index = match ext.as_str() {
        "jpg" | "jpeg" => 2,
        "bmp" => 3,
        _ => 1,
    };
    let def_ext: Vec<u16> = ext.encode_utf16().chain(std::iter::once(0)).collect();
    let filters: Vec<u16> = "PNG image\0*.png\0JPEG image\0*.jpg;*.jpeg\0BMP image\0*.bmp\0\0"
        .encode_utf16()
        .collect();
    unsafe {
        let mut ofn: OPENFILENAMEW = std::mem::zeroed();
        ofn.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
        ofn.lpstrFilter = PCWSTR(filters.as_ptr());
        ofn.nFilterIndex = filter_index;
        ofn.lpstrDefExt = PCWSTR(def_ext.as_ptr());
        ofn.lpstrFile = PWSTR(file.as_mut_ptr());
        ofn.nMaxFile = file.len() as u32;
        ofn.lpstrInitialDir = PCWSTR(dir_w.as_ptr());
        ofn.Flags = OFN_OVERWRITEPROMPT | OFN_NOCHANGEDIR | OFN_PATHMUSTEXIST;
        if !GetSaveFileNameW(&mut ofn).as_bool() {
            return None;
        }
    }
    let len = file.iter().position(|&c| c == 0).unwrap_or(file.len());
    // A missing extension is filled in by the caller (export.rs).
    Some(PathBuf::from(String::from_utf16_lossy(&file[..len])))
}

pub fn do_upload(png: &[u8], client_id: &str) -> Result<String, String> {
    upload_winhttp(png, client_id)
}

/// Imgur upload over WinHTTP: the OS TLS stack, no Rust HTTP/TLS dependency.
fn upload_winhttp(png: &[u8], client_id: &str) -> Result<String, String> {
    let headers = format!(
        "Authorization: Client-ID {client_id}\r\nContent-Type: application/octet-stream"
    );
    let (_status, text) = winhttp(
        "POST",
        "api.imgur.com",
        "/3/image?title=rustshot&description=rustshot%20capture",
        &headers,
        png,
    )?;
    extract_json_string(&text, "link").ok_or_else(|| format!("no link in response: {text}"))
}

/// HTTPS GET; a non-2xx status is an `Err("HTTP <code>")`.
pub fn http_get(host: &str, path: &str, headers: &[(&str, &str)]) -> Result<String, String> {
    let headers = headers
        .iter()
        .map(|(k, v)| format!("{k}: {v}"))
        .collect::<Vec<_>>()
        .join("\r\n");
    let (status, text) = winhttp("GET", host, path, &headers, &[])?;
    if (200..300).contains(&status) {
        Ok(text)
    } else {
        Err(format!("HTTP {status}"))
    }
}

/// Owned WinHTTP handle.
struct Handle(*mut std::ffi::c_void);
impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = windows::Win32::Networking::WinHttp::WinHttpCloseHandle(self.0);
            }
        }
    }
}

/// A sent request whose response headers have arrived. Fields drop in
/// declaration order: request, connection, session.
struct Response {
    req: Handle,
    _conn: Handle,
    _session: Handle,
    status: u32,
}

/// Null-terminated UTF-16, for PCWSTR parameters.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Send one HTTPS request over WinHTTP and wait for the response headers.
/// With `follow_redirects` false, a 3xx is returned as is (see `download_to`).
fn winhttp_send(
    method: &str,
    host: &str,
    path: &str,
    headers: &str,
    body: &[u8],
    follow_redirects: bool,
) -> Result<Response, String> {
    use std::ptr;
    use windows::Win32::Networking::WinHttp::{
        WINHTTP_ACCESS_TYPE_DEFAULT_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_OPTION_REDIRECT_POLICY,
        WINHTTP_OPTION_REDIRECT_POLICY_NEVER, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
        WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders,
        WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetOption, WinHttpSetTimeouts,
    };
    use windows::core::PCWSTR;

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

        let host = wide(host);
        let conn = Handle(WinHttpConnect(session.0, PCWSTR(host.as_ptr()), 443, 0));
        if conn.0.is_null() {
            return Err(err("WinHttpConnect"));
        }

        let method = wide(method);
        let object = wide(path);
        let request = Handle(WinHttpOpenRequest(
            conn.0,
            PCWSTR(method.as_ptr()),
            PCWSTR(object.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            ptr::null(),
            WINHTTP_FLAG_SECURE,
        ));
        if request.0.is_null() {
            return Err(err("WinHttpOpenRequest"));
        }
        if !follow_redirects {
            let never = WINHTTP_OPTION_REDIRECT_POLICY_NEVER.to_ne_bytes();
            WinHttpSetOption(Some(request.0), WINHTTP_OPTION_REDIRECT_POLICY, Some(&never))
                .map_err(|e| format!("WinHttpSetOption: {e}"))?;
        }

        // Counted (not NUL-terminated): windows-rs passes slice.len() as the
        // header block length.
        let headers: Vec<u16> = headers.encode_utf16().collect();
        let headers = if headers.is_empty() { None } else { Some(&headers[..]) };
        let (data, len) = if body.is_empty() {
            (None, 0)
        } else {
            (Some(body.as_ptr() as *const _), body.len() as u32)
        };
        WinHttpSendRequest(request.0, headers, data, len, len, 0)
            .map_err(|e| format!("WinHttpSendRequest: {e}"))?;
        WinHttpReceiveResponse(request.0, ptr::null_mut())
            .map_err(|e| format!("WinHttpReceiveResponse: {e}"))?;

        let mut status = 0u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut _),
            &mut size,
            ptr::null_mut(),
        )
        .map_err(|e| format!("WinHttpQueryHeaders: {e}"))?;

        Ok(Response { req: request, _conn: conn, _session: session, status })
    }
}

/// Read the next chunk of the response body; 0 at the end.
fn read_some(resp: &Response, buf: &mut [u8]) -> Result<usize, String> {
    let mut read = 0u32;
    let len = buf.len().min(u32::MAX as usize) as u32;
    unsafe {
        windows::Win32::Networking::WinHttp::WinHttpReadData(
            resp.req.0,
            buf.as_mut_ptr() as _,
            len,
            &mut read,
        )
    }
    .map_err(|e| format!("WinHttpReadData: {e}"))?;
    Ok(read as usize)
}

/// `Location` of a redirect response.
fn location(resp: &Response) -> Option<String> {
    use windows::Win32::Networking::WinHttp::{WINHTTP_QUERY_LOCATION, WinHttpQueryHeaders};
    use windows::core::PCWSTR;
    let mut buf = vec![0u16; 4096];
    let mut size = (buf.len() * 2) as u32;
    unsafe {
        WinHttpQueryHeaders(
            resp.req.0,
            WINHTTP_QUERY_LOCATION,
            PCWSTR::null(),
            Some(buf.as_mut_ptr() as *mut _),
            &mut size,
            std::ptr::null_mut(),
        )
        .ok()?;
    }
    buf.truncate(size as usize / 2);
    String::from_utf16(&buf).ok()
}

/// `Content-Length`, when the server sent one.
fn content_length(resp: &Response) -> Option<u64> {
    use windows::Win32::Networking::WinHttp::{
        WINHTTP_QUERY_CONTENT_LENGTH, WINHTTP_QUERY_FLAG_NUMBER64, WinHttpQueryHeaders,
    };
    use windows::core::PCWSTR;
    let mut len = 0u64;
    let mut size = std::mem::size_of::<u64>() as u32;
    unsafe {
        WinHttpQueryHeaders(
            resp.req.0,
            WINHTTP_QUERY_CONTENT_LENGTH | WINHTTP_QUERY_FLAG_NUMBER64,
            PCWSTR::null(),
            Some(&mut len as *mut u64 as *mut _),
            &mut size,
            std::ptr::null_mut(),
        )
        .ok()?;
    }
    Some(len)
}

/// One HTTPS request over WinHTTP; returns (status code, response body).
fn winhttp(
    method: &str,
    host: &str,
    path: &str,
    headers: &str,
    body: &[u8],
) -> Result<(u32, String), String> {
    let resp = winhttp_send(method, host, path, headers, body, true)?;
    let mut out: Vec<u8> = Vec::new();
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let n = read_some(&resp, &mut buf)?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
    }
    Ok((resp.status, String::from_utf8_lossy(&out).into_owned()))
}

const MAX_REDIRECTS: usize = 5;

/// Download a release asset to `dest`. Starts only at a URL that passes
/// `update::is_safe_download_url`; redirects are followed by hand and each
/// hop must pass `update::is_allowed_download_hop`. `progress(got, total)`
/// returning false cancels (error `update::CANCELLED`). A partial file is
/// deleted on any failure.
#[allow(dead_code)] // reached through update::fetch_verified (dialog: later task)
pub fn download_to(
    url: &str,
    dest: &Path,
    progress: &dyn Fn(u64, Option<u64>) -> bool,
) -> Result<(), String> {
    use crate::update::{is_allowed_download_hop, is_safe_download_url, split_https_url};
    if !is_safe_download_url(url) {
        return Err("refusing to download from an unexpected URL".into());
    }
    let mut current = url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        let (host, path) = split_https_url(&current).ok_or("unexpected download URL")?;
        let resp = winhttp_send("GET", host, path, "", &[], false)?;
        match resp.status {
            301 | 302 | 303 | 307 | 308 => {
                let next = location(&resp).ok_or("redirect without a Location")?;
                if !is_allowed_download_hop(&next) {
                    return Err("refusing a redirect to an unexpected host".into());
                }
                current = next;
            }
            200 => {
                let r = write_body(&resp, dest, progress);
                if r.is_err() {
                    let _ = std::fs::remove_file(dest);
                }
                return r;
            }
            s => return Err(format!("HTTP {s}")),
        }
    }
    Err("too many redirects".into())
}

fn write_body(
    resp: &Response,
    dest: &Path,
    progress: &dyn Fn(u64, Option<u64>) -> bool,
) -> Result<(), String> {
    use std::io::Write;
    let total = content_length(resp);
    let wr = |e: std::io::Error| format!("write {}: {e}", dest.display());
    let mut file =
        std::fs::OpenOptions::new().write(true).create_new(true).open(dest).map_err(wr)?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut got = 0u64;
    if !progress(0, total) {
        return Err(crate::update::CANCELLED.into());
    }
    loop {
        let n = read_some(resp, &mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(wr)?;
        got += n as u64;
        if total.is_some_and(|t| got > t) {
            return Err("download is larger than announced".into());
        }
        if !progress(got, total) {
            return Err(crate::update::CANCELLED.into());
        }
    }
    if total.is_some_and(|t| got != t) {
        return Err("download ended early".into());
    }
    file.sync_all().map_err(wr)
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
    fn json_extract() {
        let body = r#"{"data":{"link":"https://i.imgur.com/abc.png","id":"abc"}}"#;
        assert_eq!(
            extract_json_string(body, "link").as_deref(),
            Some("https://i.imgur.com/abc.png")
        );
    }

    #[test]
    #[ignore = "live clipboard access"]
    fn live_clipboard_roundtrip() {
        use windows::Win32::System::DataExchange::{
            CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
            RegisterClipboardFormatW,
        };
        use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
        use windows::Win32::System::Ole::{CF_DIB, CF_UNICODETEXT};
        use windows::core::w;

        let img = PixBuf::from_pixel(2, 2, [7, 8, 9, 255]);
        copy_to_clipboard(&img).unwrap();
        unsafe {
            assert!(IsClipboardFormatAvailable(CF_DIB.0 as u32).is_ok());
            assert!(IsClipboardFormatAvailable(RegisterClipboardFormatW(w!("PNG"))).is_ok());
        }

        copy_text_to_clipboard("hello rustshot").unwrap();
        unsafe {
            OpenClipboard(None).unwrap();
            let h = GetClipboardData(CF_UNICODETEXT.0 as u32).unwrap();
            let p = GlobalLock(windows::Win32::Foundation::HGLOBAL(h.0)) as *const u16;
            assert!(!p.is_null());
            let mut s = Vec::new();
            while *p.add(s.len()) != 0 {
                s.push(*p.add(s.len()));
            }
            let _ = GlobalUnlock(windows::Win32::Foundation::HGLOBAL(h.0));
            CloseClipboard().unwrap();
            assert_eq!(String::from_utf16(&s).unwrap(), "hello rustshot");
        }
    }
}
