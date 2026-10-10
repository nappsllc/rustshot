//! Frames from DXGI Desktop Duplication.
//!
//! One `IDXGIOutputDuplication` per output that intersects the area, each
//! created on a D3D11 device of the adapter that owns the output. A new
//! desktop image is cropped on the GPU (`CopySubresourceRegion` into a
//! staging texture of just the intersection), mapped and copied into a
//! canvas of the whole area; the mouse pointer is composited from the
//! duplication's pointer shape onto each frame handed out. When nothing
//! changed, the last canvas is handed out again. Lost access (desktop
//! switch, mode change, UAC prompt) re-creates the duplication.
//!
//! Physical pixels throughout: output desktop coordinates are physical in
//! a per-monitor-DPI-aware process. Rotated outputs are not supported
//! here (the caller falls back to GDI).

use crate::capture::IRect;
use crate::rec::{Frame, FrameSource};
use anyhow::{bail, Context, Result};
use std::time::{Duration, Instant};
use windows::core::Interface;
use windows::Win32::Foundation::{HMODULE, RECT};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_MODE_ROTATION, DXGI_MODE_ROTATION_IDENTITY, DXGI_MODE_ROTATION_UNSPECIFIED, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::*;

/// Tests force [`DxgiSource::open`] to fail to exercise the GDI fallback.
#[cfg(test)]
pub(crate) static FORCE_FAIL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// How long to wait before trying to re-create a lost duplication again.
const RETRY: Duration = Duration::from_millis(250);

struct Output {
    adapter: IDXGIAdapter1,
    /// GDI device name, to find the output again after access is lost.
    name: [u16; 32],
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    dup: Option<IDXGIOutputDuplication>,
    /// Output rect on the virtual screen.
    desk: RECT,
    /// The part of the area on this output, virtual-screen coordinates.
    isect: IRect,
    staging: Option<ID3D11Texture2D>,
    retry_at: Instant,
}

#[derive(Default)]
struct Pointer {
    info: DXGI_OUTDUPL_POINTER_SHAPE_INFO,
    shape: Vec<u8>,
    /// Top-left of the shape on the virtual screen.
    pos: (i32, i32),
    visible: bool,
    /// Output that reported the pointer last.
    owner: usize,
    updated: i64,
}

pub struct DxgiSource {
    area: IRect,
    outs: Vec<Output>,
    /// The latest desktop image of the area (no pointer), BGRA.
    canvas: Vec<u8>,
    pointer: Pointer,
    /// Time spent copying and compositing (not waiting), for measurements.
    work: Duration,
    frames: u64,
}

// SAFETY: D3D11 devices are free-threaded; the immediate contexts and
// duplications are only used by whichever single thread owns the source.
unsafe impl Send for DxgiSource {}

fn intersect(area: IRect, r: &RECT) -> Option<IRect> {
    let x0 = area.0.max(r.left);
    let y0 = area.1.max(r.top);
    let x1 = (area.0 + area.2 as i32).min(r.right);
    let y1 = (area.1 + area.3 as i32).min(r.bottom);
    (x1 > x0 && y1 > y0).then(|| (x0, y0, (x1 - x0) as u32, (y1 - y0) as u32))
}

fn upright(r: DXGI_MODE_ROTATION) -> bool {
    r == DXGI_MODE_ROTATION_IDENTITY || r == DXGI_MODE_ROTATION_UNSPECIFIED
}

fn staging(device: &ID3D11Device, w: u32, h: u32) -> Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
    };
    let mut tex = None;
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut tex)) }.context("create a staging texture")?;
    tex.context("no staging texture")
}

fn create_device(adapter: &IDXGIAdapter1) -> Result<(ID3D11Device, ID3D11DeviceContext)> {
    let (mut dev, mut ctx) = (None, None);
    unsafe {
        D3D11CreateDevice(
            adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut dev),
            None,
            Some(&mut ctx),
        )
    }
    .context("create a D3D11 device")?;
    Ok((dev.context("no device")?, ctx.context("no context")?))
}

impl Output {
    /// (Re-)duplicate the output named `self.name` on its adapter.
    fn duplicate(&mut self, area: IRect) -> Result<()> {
        self.dup = None;
        let mut i = 0;
        let output = loop {
            let o = unsafe { self.adapter.EnumOutputs(i) }.context("the output is gone")?;
            let d = unsafe { o.GetDesc() }?;
            if d.DeviceName == self.name {
                break (o, d);
            }
            i += 1;
        };
        let (output, desc) = output;
        if !upright(desc.Rotation) {
            bail!("rotated output");
        }
        if unsafe { self.device.GetDeviceRemovedReason() }.is_err() {
            (self.device, self.ctx) = create_device(&self.adapter)?;
            self.staging = None;
        }
        let dup = unsafe { output.cast::<IDXGIOutput1>()?.DuplicateOutput(&self.device) }.context("duplicate the output")?;
        if !upright(unsafe { dup.GetDesc() }.Rotation) {
            bail!("rotated output");
        }
        let isect = intersect(area, &desc.DesktopCoordinates).unwrap_or((area.0, area.1, 0, 0));
        if isect != self.isect || desc.DesktopCoordinates != self.desk {
            self.staging = None;
        }
        self.desk = desc.DesktopCoordinates;
        self.isect = isect;
        if self.staging.is_none() && isect.2 > 0 && isect.3 > 0 {
            self.staging = Some(staging(&self.device, isect.2, isect.3)?);
        }
        self.dup = Some(dup);
        Ok(())
    }
}

impl DxgiSource {
    /// Duplicate every output intersecting `area` (physical virtual-screen
    /// pixels). Fails when any of them cannot be duplicated (or is rotated).
    pub fn open(area: IRect) -> Result<DxgiSource> {
        #[cfg(test)]
        if FORCE_FAIL.load(std::sync::atomic::Ordering::Relaxed) {
            bail!("desktop duplication disabled by the test");
        }
        if area.2 == 0 || area.3 == 0 {
            bail!("empty recording area");
        }
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.context("create a DXGI factory")?;
        let mut outs = Vec::new();
        let mut ai = 0;
        while let Ok(adapter) = unsafe { factory.EnumAdapters1(ai) } {
            ai += 1;
            let mut device: Option<(ID3D11Device, ID3D11DeviceContext)> = None;
            let mut oi = 0;
            while let Ok(output) = unsafe { adapter.EnumOutputs(oi) } {
                oi += 1;
                let desc = unsafe { output.GetDesc() }?;
                if !desc.AttachedToDesktop.as_bool() || intersect(area, &desc.DesktopCoordinates).is_none() {
                    continue;
                }
                let (dev, ctx) = match &device {
                    Some(d) => d.clone(),
                    None => {
                        let d = create_device(&adapter)?;
                        device = Some(d.clone());
                        d
                    }
                };
                let mut o = Output {
                    adapter: adapter.clone(),
                    name: desc.DeviceName,
                    device: dev,
                    ctx,
                    dup: None,
                    desk: RECT::default(),
                    isect: (0, 0, 0, 0),
                    staging: None,
                    retry_at: Instant::now(),
                };
                o.duplicate(area)?;
                outs.push(o);
            }
        }
        if outs.is_empty() {
            bail!("no display output shows the recorded area");
        }
        let mut s = DxgiSource {
            area,
            outs,
            canvas: vec![0; area.2 as usize * area.3 as usize * 4],
            pointer: Pointer::default(),
            work: Duration::ZERO,
            frames: 0,
        };
        // The first frame of a new duplication is the whole desktop.
        for i in 0..s.outs.len() {
            s.poll(i, 200);
        }
        Ok(s)
    }

    /// Mean time per frame spent copying and compositing (not waiting).
    pub fn work_per_frame(&self) -> Duration {
        self.work / self.frames.max(1) as u32
    }

    /// Wait up to `timeout_ms` for output `i` to present; take what it has.
    fn poll(&mut self, i: usize, timeout_ms: u32) {
        let area = self.area;
        let o = &mut self.outs[i];
        let Some(dup) = o.dup.clone() else {
            if Instant::now() >= o.retry_at && o.duplicate(area).is_err() {
                o.retry_at = Instant::now() + RETRY;
            }
            return;
        };
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut res = None;
        match unsafe { dup.AcquireNextFrame(timeout_ms, &mut info, &mut res) } {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return,
            Err(e) => {
                // Access lost (desktop switch, mode change) or worse: start over.
                let lost = e.code() == DXGI_ERROR_ACCESS_LOST;
                if !lost || o.duplicate(area).is_err() {
                    o.dup = None;
                    o.retry_at = Instant::now() + RETRY;
                }
                return;
            }
        }
        let t0 = Instant::now();
        let mut copied = false;
        if info.LastPresentTime != 0
            && let (Some(res), Some(stg)) = (res, &o.staging)
            && let Ok(tex) = res.cast::<ID3D11Texture2D>()
        {
            let (ix, iy, iw, ih) = o.isect;
            let bx = D3D11_BOX {
                left: (ix - o.desk.left) as u32,
                top: (iy - o.desk.top) as u32,
                front: 0,
                right: (ix - o.desk.left) as u32 + iw,
                bottom: (iy - o.desk.top) as u32 + ih,
                back: 1,
            };
            unsafe { o.ctx.CopySubresourceRegion(stg, 0, 0, 0, 0, &tex, 0, Some(&bx)) };
            copied = true;
        }
        let p = &mut self.pointer;
        if info.LastMouseUpdateTime != 0 {
            let visible = info.PointerPosition.Visible.as_bool();
            // Several outputs may report; the newest visible one wins, and
            // only the owner can hide it.
            if (visible && info.LastMouseUpdateTime >= p.updated) || (!visible && p.owner == i) {
                p.visible = visible;
                p.owner = i;
                p.updated = info.LastMouseUpdateTime;
                p.pos = (o.desk.left + info.PointerPosition.Position.x, o.desk.top + info.PointerPosition.Position.y);
            }
        }
        if info.PointerShapeBufferSize > 0 {
            p.shape.resize(info.PointerShapeBufferSize as usize, 0);
            let mut need = 0;
            let mut si = DXGI_OUTDUPL_POINTER_SHAPE_INFO::default();
            let ok = unsafe {
                dup.GetFramePointerShape(p.shape.len() as u32, p.shape.as_mut_ptr().cast(), &mut need, &mut si)
            };
            if ok.is_ok() {
                p.info = si;
            } else {
                p.shape.clear();
            }
        }
        let _ = unsafe { dup.ReleaseFrame() };
        if copied && let Some(stg) = &o.staging {
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            if unsafe { o.ctx.Map(stg, 0, D3D11_MAP_READ, 0, Some(&mut m)) }.is_ok() {
                let (ix, iy, iw, ih) = o.isect;
                let stride = area.2 as usize * 4;
                let row = iw as usize * 4;
                let x = (ix - area.0) as usize * 4;
                for r in 0..ih as usize {
                    let src = unsafe { std::slice::from_raw_parts((m.pData as *const u8).add(r * m.RowPitch as usize), row) };
                    let at = (iy - area.1) as usize * stride + r * stride + x;
                    self.canvas[at..at + row].copy_from_slice(src);
                }
                unsafe { o.ctx.Unmap(stg, 0) };
            }
        }
        self.work += t0.elapsed();
    }
}

impl FrameSource for DxgiSource {
    fn next(&mut self, deadline: Instant) -> Option<Frame> {
        let n = self.outs.len();
        for i in 0..n {
            let left = deadline.saturating_duration_since(Instant::now());
            // Split the wait between the outputs still to look at.
            let ms = (left.as_millis() as u32) / (n - i) as u32;
            self.poll(i, ms);
        }
        let t0 = Instant::now();
        let mut bgra = self.canvas.clone();
        draw_pointer(&mut bgra, self.area, &self.pointer);
        self.work += t0.elapsed();
        self.frames += 1;
        Some(Frame { w: self.area.2, h: self.area.3, bgra, ts: Duration::ZERO })
    }

    fn size(&self) -> (u32, u32) {
        (self.area.2, self.area.3)
    }
}

/// Composite the pointer shape onto `bgra` (the area's pixels).
fn draw_pointer(bgra: &mut [u8], area: IRect, p: &Pointer) {
    if !p.visible || p.shape.is_empty() {
        return;
    }
    let info = &p.info;
    let mono = info.Type == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME.0 as u32;
    let (sw, sh) = (info.Width as i32, if mono { info.Height as i32 / 2 } else { info.Height as i32 });
    let pitch = info.Pitch as usize;
    let (aw, ah) = (area.2 as i32, area.3 as i32);
    let (ox, oy) = (p.pos.0 - area.0, p.pos.1 - area.1);
    for sy in 0..sh {
        let y = oy + sy;
        if y < 0 || y >= ah {
            continue;
        }
        for sx in 0..sw {
            let x = ox + sx;
            if x < 0 || x >= aw {
                continue;
            }
            let at = (y as usize * aw as usize + x as usize) * 4;
            let Some(d) = bgra.get_mut(at..at + 4) else { continue };
            let (sx, sy) = (sx as usize, sy as usize);
            if mono {
                let bit = 0x80u8 >> (sx % 8);
                let and = p.shape.get(sy * pitch + sx / 8).is_some_and(|b| b & bit != 0);
                let xor = p.shape.get((sy + sh as usize) * pitch + sx / 8).is_some_and(|b| b & bit != 0);
                for c in &mut d[..3] {
                    *c = if and { *c } else { 0 } ^ if xor { 0xFF } else { 0 };
                }
            } else {
                let Some(s) = p.shape.get(sy * pitch + sx * 4..sy * pitch + sx * 4 + 4) else { continue };
                if info.Type == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR.0 as u32 {
                    // Alpha 0: replace; 0xFF: XOR with the screen.
                    if s[3] == 0 {
                        d[..3].copy_from_slice(&s[..3]);
                    } else {
                        for k in 0..3 {
                            d[k] ^= s[k];
                        }
                    }
                } else {
                    let a = u32::from(s[3]);
                    for k in 0..3 {
                        d[k] = ((u32::from(s[k]) * a + u32::from(d[k]) * (255 - a) + 127) / 255) as u8;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ptr(kind: DXGI_OUTDUPL_POINTER_SHAPE_TYPE, w: u32, h: u32, pitch: u32, shape: Vec<u8>) -> Pointer {
        Pointer {
            info: DXGI_OUTDUPL_POINTER_SHAPE_INFO { Type: kind.0 as u32, Width: w, Height: h, Pitch: pitch, ..Default::default() },
            shape,
            pos: (11, 21),
            visible: true,
            owner: 0,
            updated: 1,
        }
    }

    #[test]
    fn colour_pointer_is_alpha_blended_and_clipped() {
        // 2×1 shape: opaque red, half-transparent white; area at (10, 20) 2×2.
        let p = ptr(DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR, 2, 1, 8, vec![0, 0, 255, 255, 255, 255, 255, 128]);
        let mut px = vec![0u8; 2 * 2 * 4];
        draw_pointer(&mut px, (10, 20, 2, 2), &p);
        // Pointer at (1, 1) of the area: red there, the white pixel is clipped.
        assert_eq!(&px[12..15], &[0, 0, 255]);
        assert!(px[..12].iter().all(|&b| b == 0));
    }

    #[test]
    fn masked_colour_pointer_replaces_or_xors() {
        let p = ptr(DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR, 2, 1, 8, vec![1, 2, 3, 0, 0xFF, 0x0F, 0, 0xFF]);
        let mut px = vec![0x55u8; 3 * 2 * 4];
        draw_pointer(&mut px, (10, 20, 3, 2), &p);
        let at = (3 + 1) * 4;
        assert_eq!(&px[at..at + 3], &[1, 2, 3]);
        assert_eq!(&px[at + 4..at + 7], &[0x55 ^ 0xFF, 0x55 ^ 0x0F, 0x55]);
    }

    #[test]
    fn monochrome_pointer_uses_and_then_xor() {
        // 8×1 shape (height 2: AND row, XOR row), pitch 1.
        // Pixel 0: AND 0 XOR 0 → black; 1: AND 0 XOR 1 → white;
        // 2: AND 1 XOR 0 → screen; 3: AND 1 XOR 1 → inverted.
        let p = ptr(DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME, 8, 2, 1, vec![0b0011_1111, 0b0101_0000]);
        let mut px = vec![0x30u8; 6 * 2 * 4];
        draw_pointer(&mut px, (10, 20, 6, 2), &p);
        let at = |x: usize| (6 + 1 + x) * 4;
        assert_eq!(&px[at(0)..at(0) + 3], &[0, 0, 0]);
        assert_eq!(&px[at(1)..at(1) + 3], &[0xFF, 0xFF, 0xFF]);
        assert_eq!(&px[at(2)..at(2) + 3], &[0x30, 0x30, 0x30]);
        assert_eq!(&px[at(3)..at(3) + 3], &[0xCF, 0xCF, 0xCF]);
    }

    #[test]
    fn hidden_pointer_draws_nothing() {
        let mut p = ptr(DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR, 1, 1, 4, vec![9, 9, 9, 255]);
        p.visible = false;
        let mut px = vec![0u8; 4 * 4 * 4];
        draw_pointer(&mut px, (10, 20, 4, 4), &p);
        assert!(px.iter().all(|&b| b == 0));
    }

    #[test]
    fn intersection_with_outputs() {
        let r = RECT { left: 0, top: 0, right: 1920, bottom: 1080 };
        assert_eq!(intersect((1900, 1000, 100, 100), &r), Some((1900, 1000, 20, 80)));
        assert_eq!(intersect((1920, 0, 10, 10), &r), None);
        assert_eq!(intersect((-10, -10, 20, 20), &r), Some((0, 0, 10, 10)));
    }
}
