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

pub fn do_upload(png: &[u8], client_id: &str) -> Result<String, String> {
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
