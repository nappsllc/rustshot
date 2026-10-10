//! Frames by polling GDI: `BitBlt` of the area from the screen DC into a
//! DIB section (kept for the whole recording), with the cursor drawn by
//! `DrawIconEx`. The fallback when Desktop Duplication is unavailable (RDP,
//! some drivers, rotated outputs); slower, but works everywhere.

use crate::capture::IRect;
use crate::rec::{Frame, FrameSource};
use anyhow::{anyhow, bail, Result};
use std::mem::size_of;
use std::time::{Duration, Instant};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GdiFlush, GetDC, ReleaseDC, SelectObject,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ, SRCCOPY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DrawIconEx, GetCursorInfo, GetIconInfo, CURSORINFO, CURSOR_SHOWING, DI_NORMAL, HCURSOR, HICON, ICONINFO,
};

pub struct GdiPollSource {
    area: IRect,
    screen: HDC,
    mem: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *const u8,
    /// The last cursor seen and its hot spot.
    cursor: (HCURSOR, (i32, i32)),
    work: Duration,
    frames: u64,
}

// SAFETY: memory DCs, DIB sections and the screen DC are not tied to the
// thread that made them; the source is used by one thread at a time.
unsafe impl Send for GdiPollSource {}

impl GdiPollSource {
    pub fn open(area: IRect) -> Result<GdiPollSource> {
        let (w, h) = (area.2, area.3);
        if w == 0 || h == 0 {
            bail!("empty recording area");
        }
        unsafe {
            let screen = GetDC(None);
            if screen.is_invalid() {
                bail!("no screen DC");
            }
            let mem = CreateCompatibleDC(Some(screen));
            if mem.is_invalid() {
                ReleaseDC(None, screen);
                bail!("no memory DC");
            }
            let mut bmi = BITMAPINFO::default();
            bmi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
            bmi.bmiHeader.biWidth = w as i32;
            bmi.bmiHeader.biHeight = -(h as i32); // top-down
            bmi.bmiHeader.biPlanes = 1;
            bmi.bmiHeader.biBitCount = 32;
            bmi.bmiHeader.biCompression = BI_RGB.0;
            let mut bits = std::ptr::null_mut();
            let bmp = match CreateDIBSection(Some(mem), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
                Ok(b) if !bits.is_null() => b,
                r => {
                    if let Ok(b) = r {
                        let _ = DeleteObject(HGDIOBJ(b.0));
                    }
                    let _ = DeleteDC(mem);
                    ReleaseDC(None, screen);
                    return Err(anyhow!("create a {w}x{h} DIB section"));
                }
            };
            let old = SelectObject(mem, HGDIOBJ(bmp.0));
            Ok(GdiPollSource {
                area,
                screen,
                mem,
                bmp,
                old,
                bits: bits as *const u8,
                cursor: (HCURSOR::default(), (0, 0)),
                work: Duration::ZERO,
                frames: 0,
            })
        }
    }

    /// Mean time per frame (blit, cursor, copy).
    pub fn work_per_frame(&self) -> Duration {
        self.work / self.frames.max(1) as u32
    }

    fn draw_cursor(&mut self) {
        unsafe {
            let mut ci = CURSORINFO { cbSize: size_of::<CURSORINFO>() as u32, ..Default::default() };
            if GetCursorInfo(&mut ci).is_err() || ci.flags.0 & CURSOR_SHOWING.0 == 0 || ci.hCursor.is_invalid() {
                return;
            }
            if self.cursor.0 != ci.hCursor {
                let mut ii = ICONINFO::default();
                if GetIconInfo(HICON(ci.hCursor.0), &mut ii).is_err() {
                    return;
                }
                for b in [ii.hbmMask, ii.hbmColor] {
                    if !b.is_invalid() {
                        let _ = DeleteObject(HGDIOBJ(b.0));
                    }
                }
                self.cursor = (ci.hCursor, (ii.xHotspot as i32, ii.yHotspot as i32));
            }
            let (hx, hy) = self.cursor.1;
            let x = ci.ptScreenPos.x - hx - self.area.0;
            let y = ci.ptScreenPos.y - hy - self.area.1;
            let _ = DrawIconEx(self.mem, x, y, HICON(ci.hCursor.0), 0, 0, 0, None, DI_NORMAL);
        }
    }
}

impl FrameSource for GdiPollSource {
    fn next(&mut self, _deadline: Instant) -> Option<Frame> {
        let t0 = Instant::now();
        let (x, y, w, h) = self.area;
        unsafe {
            // CAPTUREBLT includes layered windows (the camera bubble).
            BitBlt(self.mem, 0, 0, w as i32, h as i32, Some(self.screen), x, y, SRCCOPY | CAPTUREBLT).ok()?;
        }
        self.draw_cursor();
        let n = w as usize * h as usize * 4;
        let bgra = unsafe {
            let _ = GdiFlush();
            std::slice::from_raw_parts(self.bits, n).to_vec()
        };
        self.work += t0.elapsed();
        self.frames += 1;
        Some(Frame { w, h, bgra, ts: Duration::ZERO })
    }

    fn size(&self) -> (u32, u32) {
        (self.area.2, self.area.3)
    }
}

impl Drop for GdiPollSource {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.mem, self.old);
            let _ = DeleteObject(HGDIOBJ(self.bmp.0));
            let _ = DeleteDC(self.mem);
            ReleaseDC(None, self.screen);
        }
    }
}
