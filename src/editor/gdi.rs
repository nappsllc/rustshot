//! Windows GDI overlay backend (`renderer = "gdi"`). The capture lives in
//! two device-dependent bitmaps owned by the kernel, not in rustshot's
//! memory: `plain` (the screen as captured) and `dimmed` (the capture with
//! the theme dim applied, built band by band). Committed objects live in
//! an annotation layer ([`AnnotLayer`]) covering only their bounds. A
//! paint blits the bitmaps (and the layer) straight to the window and
//! renders only the chrome and the draft through the software compositor,
//! in band-sized buffers whose background is read back from the bitmaps
//! or the layer ([`GdiBackdrop`]).
//!
//! Bitmaps stay selected into their own memory DCs for the whole capture;
//! pixels are read by blitting into a band-sized DIB section (never
//! `GetDIBits` on a selected bitmap). Everything is released on drop.

use super::compose::{self, Backdrop, PxRect, Source};
use super::{Dimmer, Edit};
use crate::objects::Obj;
use crate::raster::{Order, Surf};
use crate::uifb::C4;
use ab_glyph::FontArc;
use anyhow::{anyhow, Result};
use std::cell::RefCell;
use windows::Win32::Graphics::Gdi::{
    AlphaBlend, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateDIBSection, DeleteDC,
    DeleteObject, GdiFlush, GetDC, ReleaseDC, SelectObject, SetDIBitsToDevice, AC_SRC_OVER,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
    SRCCOPY,
};

/// Rows per band (dimmed build, read-back, compose).
const BAND: i32 = 64;

fn bmi(w: i32, h: i32) -> BITMAPINFO {
    let mut b: BITMAPINFO = unsafe { std::mem::zeroed() };
    b.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    b.bmiHeader.biWidth = w;
    b.bmiHeader.biHeight = -h; // top-down
    b.bmiHeader.biPlanes = 1;
    b.bmiHeader.biBitCount = 32;
    b.bmiHeader.biCompression = BI_RGB.0;
    b
}

/// A bitmap selected into its own memory DC; freed on drop.
struct MemBmp {
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
}

impl MemBmp {
    unsafe fn select(dc: HDC, bmp: HBITMAP) -> Self {
        let old = unsafe { SelectObject(dc, HGDIOBJ(bmp.0)) };
        MemBmp { dc, bmp, old }
    }

    /// Device-dependent bitmap compatible with the screen.
    fn ddb(w: i32, h: i32) -> Result<Self> {
        unsafe {
            let screen = GetDC(None);
            let dc = CreateCompatibleDC(Some(screen));
            let bmp = CreateCompatibleBitmap(screen, w, h);
            let _ = ReleaseDC(None, screen);
            if dc.is_invalid() || bmp.is_invalid() {
                if !bmp.is_invalid() {
                    let _ = DeleteObject(HGDIOBJ(bmp.0));
                }
                if !dc.is_invalid() {
                    let _ = DeleteDC(dc);
                }
                return Err(anyhow!("could not create a {w}x{h} bitmap"));
            }
            Ok(Self::select(dc, bmp))
        }
    }
}

impl Drop for MemBmp {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(HGDIOBJ(self.bmp.0));
            let _ = DeleteDC(self.dc);
        }
    }
}

/// A top-down 32-bpp DIB section in its own DC (CPU-visible pixels).
struct Dib {
    m: MemBmp,
    bits: *mut u8,
    w: i32,
    h: i32,
}

impl Dib {
    fn new(w: i32, h: i32) -> Result<Self> {
        unsafe {
            let dc = CreateCompatibleDC(None);
            if dc.is_invalid() {
                return Err(anyhow!("CreateCompatibleDC failed"));
            }
            let mut bits: *mut core::ffi::c_void = core::ptr::null_mut();
            match CreateDIBSection(Some(dc), &bmi(w, h), DIB_RGB_COLORS, &mut bits, None, 0) {
                Ok(bmp) if !bits.is_null() => Ok(Dib { m: MemBmp::select(dc, bmp), bits: bits as *mut u8, w, h }),
                r => {
                    // A bitmap without bits: free it too.
                    if let Ok(bmp) = r
                        && !bmp.is_invalid()
                    {
                        let _ = DeleteObject(HGDIOBJ(bmp.0));
                    }
                    let _ = DeleteDC(dc);
                    Err(anyhow!("CreateDIBSection {w}x{h} failed"))
                }
            }
        }
    }

    /// Row `y`, `n` pixels from the left (after `GdiFlush`).
    fn row(&mut self, y: i32, n: usize) -> &mut [u8] {
        debug_assert!(y < self.h && n <= self.w as usize);
        unsafe { std::slice::from_raw_parts_mut(self.bits.add(y as usize * self.w as usize * 4), n * 4) }
    }
}

/// The capture as GDI bitmaps, for one overlay session.
pub struct GdiScreen {
    size: (u32, u32),
    plain: MemBmp,
    /// `plain` dimmed by `dim` once; `None` when the dim alpha is 0.
    dimmed: Option<MemBmp>,
    /// Theme dim colour with the full (settled) dim alpha.
    dim: C4,
    /// Image width x `BAND` rows: read-back and band staging.
    scratch: RefCell<Dib>,
    /// 1x1 dim colour, the AlphaBlend source during the dim fade.
    swatch: Dib,
    /// Compose buffer reused across paints (one band of a rect).
    buf: RefCell<Vec<u8>>,
    /// Rect lists `paint_gdi` reuses across paints.
    lists: RefCell<PaintLists>,
    /// The committed objects baked over `plain`; `None` while there are
    /// none.
    layer: Option<AnnotLayer>,
}

#[derive(Default)]
struct PaintLists {
    over: Vec<PxRect>,
    ov: Vec<PxRect>,
    bg: Vec<PxRect>,
    tmp: Vec<PxRect>,
    runs: Vec<(PxRect, Source)>,
    slow: Vec<PxRect>,
}

/// The annotation layer: `plain` with every committed object rendered on
/// top, over the union of the objects' bounds (AA margins included, so it
/// holds every pixel they touch). Updated when the objects change; the
/// backdrop of every paint and of the export inside `rect`.
pub struct AnnotLayer {
    rect: PxRect,
    /// BGRA, opaque, `rect.w()` x `rect.h()`, top-down.
    px: Vec<u8>,
    /// The objects baked in, in order (appending one renders just it).
    baked: Vec<Obj>,
}

/// `c` with R and B exchanged: a [`Dimmer`] for BGRA bytes.
fn bgra(c: C4) -> C4 {
    C4 { r: c.b, g: c.g, b: c.r, a: c.a }
}

/// `d = s` with every alpha byte set to 255 (DDB read-backs leave it 0).
fn copy_opaque(d: &mut [u8], s: &[u8]) {
    let (d8, dt) = d.as_chunks_mut::<8>();
    let (s8, st) = s.as_chunks::<8>();
    for (o, i) in d8.iter_mut().zip(s8) {
        *o = (u64::from_le_bytes(*i) | 0xFF00_0000_FF00_0000).to_le_bytes();
    }
    if let (Some(o), Some(i)) = (dt.first_chunk_mut::<4>(), st.first_chunk::<4>()) {
        *o = (u32::from_le_bytes(*i) | 0xFF00_0000).to_le_bytes();
    }
}

impl GdiScreen {
    /// Capture the virtual-screen rect at `origin`/`size` (`BitBlt` from
    /// the screen DC) and build the dimmed copy for `dim` (theme dim colour
    /// with the settled alpha).
    pub fn capture(origin: (i32, i32), size: (u32, u32), dim: C4) -> Result<Self> {
        let s = Self::empty(size, dim)?;
        unsafe {
            let screen = GetDC(None);
            let r = BitBlt(s.plain.dc, 0, 0, size.0 as i32, size.1 as i32, Some(screen), origin.0, origin.1, SRCCOPY);
            let _ = ReleaseDC(None, screen);
            r.map_err(|e| anyhow!("screen blit failed: {e}"))?;
        }
        s.finish()
    }

    /// Same as `capture`, from an in-memory RGBA image (tests, benches).
    #[cfg(test)]
    pub fn from_pixels(img: &crate::pixbuf::PixBuf, dim: C4) -> Result<Self> {
        let size = img.dimensions();
        let s = Self::empty(size, dim)?;
        {
            let mut sc = s.scratch.borrow_mut();
            let w = size.0 as usize;
            for y0 in (0..size.1 as i32).step_by(BAND as usize) {
                let bh = BAND.min(size.1 as i32 - y0);
                unsafe { let _ = GdiFlush(); }
                for i in 0..bh {
                    let src = &img.as_raw()[((y0 + i) as usize * w) * 4..][..w * 4];
                    let d = sc.row(i, w);
                    d.copy_from_slice(src);
                    crate::wind::swap_rb(d);
                }
                unsafe {
                    let _ = BitBlt(s.plain.dc, 0, y0, w as i32, bh, Some(sc.m.dc), 0, 0, SRCCOPY);
                }
            }
        }
        s.finish()
    }

    fn empty(size: (u32, u32), dim: C4) -> Result<Self> {
        let (w, h) = (size.0 as i32, size.1 as i32);
        if w <= 0 || h <= 0 {
            return Err(anyhow!("empty capture region"));
        }
        let plain = MemBmp::ddb(w, h)?;
        let scratch = Dib::new(w, BAND.min(h))?;
        let mut swatch = Dib::new(1, 1)?;
        unsafe { let _ = GdiFlush(); }
        swatch.row(0, 1).copy_from_slice(&[dim.b, dim.g, dim.r, 255]);
        Ok(GdiScreen {
            size,
            plain,
            dimmed: None,
            dim,
            scratch: RefCell::new(scratch),
            swatch,
            buf: RefCell::new(Vec::new()),
            lists: RefCell::new(PaintLists::default()),
            layer: None,
        })
    }

    /// Build `dimmed` from `plain`, one band at a time.
    fn finish(mut self) -> Result<Self> {
        if self.dim.a == 0 {
            return Ok(self);
        }
        let (w, h) = (self.size.0 as i32, self.size.1 as i32);
        let dimmed = MemBmp::ddb(w, h)?;
        let dm = Dimmer::new(bgra(self.dim));
        {
            let mut sc = self.scratch.borrow_mut();
            for y0 in (0..h).step_by(BAND as usize) {
                let bh = BAND.min(h - y0);
                unsafe {
                    BitBlt(sc.m.dc, 0, 0, w, bh, Some(self.plain.dc), 0, y0, SRCCOPY)
                        .map_err(|e| anyhow!("blit failed: {e}"))?;
                    let _ = GdiFlush();
                }
                for i in 0..bh {
                    dm.apply(sc.row(i, w as usize), 1);
                }
                unsafe {
                    BitBlt(dimmed.dc, 0, y0, w, bh, Some(sc.m.dc), 0, 0, SRCCOPY)
                        .map_err(|e| anyhow!("blit failed: {e}"))?;
                }
            }
        }
        self.dimmed = Some(dimmed);
        Ok(self)
    }

    /// Copy rect `r` of the bitmap in `src` into `out` (BGRA; row `i` at
    /// byte `i * stride * 4`). DDB read-backs carry no alpha: `opaque`
    /// sets it to 255 (export, layer); painting skips that pass (nothing
    /// a paint draws reads the destination alpha, and the window ignores
    /// it).
    fn read(&self, src: HDC, r: PxRect, out: &mut [u8], stride: usize, opaque: bool) {
        let mut sc = self.scratch.borrow_mut();
        let w = r.w() as usize;
        let mut y = r.y0;
        while y < r.y1 {
            let bh = BAND.min(r.y1 - y).min(sc.h);
            unsafe {
                let _ = BitBlt(sc.m.dc, 0, 0, w as i32, bh, Some(src), r.x0, y, SRCCOPY);
                let _ = GdiFlush();
            }
            for i in 0..bh {
                let d = &mut out[((y - r.y0 + i) as usize * stride) * 4..][..w * 4];
                if opaque {
                    copy_opaque(d, sc.row(i, w));
                } else {
                    d.copy_from_slice(sc.row(i, w));
                }
            }
            y += bh;
        }
    }

    /// The bitmap a backdrop source reads from, and the dim passes left
    /// to apply on top (`None`: none).
    fn source(&self, src: Source) -> (HDC, Option<(u8, u8)>) {
        match (src, &self.dimmed) {
            (Source::Plain, _) => (self.plain.dc, None),
            (Source::Dimmed { alpha, times }, Some(d)) if alpha == self.dim.a => {
                (d.dc, (times > 1).then_some((alpha, times - 1)))
            }
            (Source::Dimmed { alpha, times }, _) => (self.plain.dc, Some((alpha, times))),
        }
    }

    /// Blend rows of `out` (BGRA) toward the dim colour, `times` over.
    fn dim_rows(&self, out: &mut [u8], r: PxRect, stride: usize, alpha: u8, times: u8) {
        let dm = Dimmer::new(bgra(self.dim.with_alpha(alpha)));
        let n = r.w() as usize * 4;
        for i in 0..r.h() as usize {
            dm.apply(&mut out[i * stride * 4..][..n], times as i32);
        }
    }

    /// Bake `objects` (in order) over the plain capture into the layer:
    /// appended objects are rendered into the existing layer (grown when
    /// needed, keeping its pixels); anything else rebuilds it. No objects:
    /// no layer.
    pub fn set_objects(&mut self, objects: &[Obj], font: Option<&FontArc>) {
        let img = PxRect::image(self.size);
        let rect = objects
            .iter()
            .filter_map(|o| o.bounds(font))
            .map(|b| PxRect::outer(b).intersect(&img))
            .fold(PxRect::default(), |u, b| u.union(&b));
        if rect.is_empty() {
            self.layer = None;
            return;
        }
        let keep = self.layer.take().filter(|l| rect.contains(&l.rect) && objects.starts_with(&l.baked));
        let (w, h) = (rect.w() as usize, rect.h() as usize);
        let (mut px, from) = match keep {
            Some(l) if l.rect == rect => (l.px, l.baked.len()),
            keep => {
                let mut px = vec![0u8; w * h * 4];
                let from = match &keep {
                    // Grown: the old layer where it was, plain around it.
                    Some(l) => {
                        for p in rect.minus_iter(&l.rect) {
                            self.read(self.plain.dc, p, &mut px[at(rect, p.x0, p.y0)..], w, true);
                        }
                        let n = l.rect.w() as usize * 4;
                        for (i, y) in (l.rect.y0..l.rect.y1).enumerate() {
                            px[at(rect, l.rect.x0, y)..][..n].copy_from_slice(&l.px[i * n..][..n]);
                        }
                        l.baked.len()
                    }
                    None => {
                        self.read(self.plain.dc, rect, &mut px, w, true);
                        0
                    }
                };
                (px, from)
            }
        };
        {
            let mut sf = Surf::with_origin(&mut px, w as u32, h as u32, rect.x0, rect.y0, Order::Bgra);
            for o in &objects[from..] {
                o.render_into(&mut sf, font);
            }
        }
        self.layer = Some(AnnotLayer { rect, px, baked: objects.to_vec() });
    }

    /// The layer rect (tests).
    #[cfg(test)]
    pub fn layer_rect(&self) -> Option<PxRect> {
        self.layer.as_ref().map(|l| l.rect)
    }

    /// Fill `r` from the bitmaps (ignoring the layer).
    fn fill_bitmap(&self, r: PxRect, src: Source, out: &mut [u8], stride: usize, opaque: bool) {
        let (dc, extra) = self.source(src);
        self.read(dc, r, out, stride, opaque);
        if let Some((alpha, times)) = extra {
            self.dim_rows(out, r, stride, alpha, times);
        }
    }

    /// Paint backdrop run `q` straight to `hdc`; false when it needs the
    /// compose path (a double dim from a fractional selection).
    fn blit(&self, hdc: HDC, q: PxRect, src: Source) -> bool {
        let (x, y, w, h) = (q.x0, q.y0, q.w(), q.h());
        let (dc, extra) = self.source(src);
        unsafe {
            match extra {
                None => {
                    let _ = BitBlt(hdc, x, y, w, h, Some(dc), x, y, SRCCOPY);
                    true
                }
                // Dim fade-in: the plain capture, then the dim colour at
                // this frame's alpha (a 1x1 source stretched).
                Some((alpha, 1)) if dc == self.plain.dc => {
                    let _ = BitBlt(hdc, x, y, w, h, Some(dc), x, y, SRCCOPY);
                    let bf = BLENDFUNCTION {
                        BlendOp: AC_SRC_OVER as u8,
                        BlendFlags: 0,
                        SourceConstantAlpha: alpha,
                        AlphaFormat: 0,
                    };
                    let _ = AlphaBlend(hdc, x, y, w, h, self.swatch.m.dc, 0, 0, 1, 1, bf);
                    true
                }
                Some(_) => false,
            }
        }
    }
}

/// Byte offset of image pixel (`x`, `y`) in a buffer covering `rect`.
fn at(rect: PxRect, x: i32, y: i32) -> usize {
    ((y - rect.y0) as usize * rect.w() as usize + (x - rect.x0) as usize) * 4
}

/// Write `buf` (BGRA, `r.w()` x `r.h()`, top-down) at `r` in `hdc`.
fn put(hdc: HDC, r: PxRect, buf: &[u8]) {
    put_from(hdc, r, buf, r.w(), 0);
}

/// Write `r.h()` rows of `buf` (BGRA, `stride` pixels per row, top-down;
/// row 0 lands at `r.y0`), from column `x_src`, at `r` in `hdc`.
fn put_from(hdc: HDC, r: PxRect, buf: &[u8], stride: i32, x_src: i32) {
    assert!(buf.len() >= r.h() as usize * stride as usize * 4 && x_src >= 0 && x_src + r.w() <= stride);
    let b = bmi(stride, r.h());
    unsafe {
        SetDIBitsToDevice(
            hdc,
            r.x0,
            r.y0,
            r.w() as u32,
            r.h() as u32,
            x_src,
            0,
            0,
            r.h() as u32,
            buf.as_ptr() as *const _,
            &b,
            DIB_RGB_COLORS,
        );
    }
}

/// The GDI bitmaps plus the annotation layer, as a compose [`Backdrop`].
pub struct GdiBackdrop<'a> {
    pub scr: &'a GdiScreen,
    /// Alpha 255 everywhere (the export); painting leaves the alpha of
    /// pixels read from the bitmaps undefined.
    pub opaque: bool,
}

impl Backdrop for GdiBackdrop<'_> {
    fn fill(&self, r: PxRect, src: Source, out: &mut [u8], stride: usize, order: Order) {
        if r.is_empty() {
            return;
        }
        let scr = self.scr;
        let off = |p: PxRect| ((p.y0 - r.y0) as usize * stride + (p.x0 - r.x0) as usize) * 4;
        match scr.layer.as_ref().map(|l| (l, r.intersect(&l.rect))).filter(|(_, i)| !i.is_empty()) {
            None => scr.fill_bitmap(r, src, out, stride, self.opaque),
            Some((l, i)) => {
                for p in r.minus_iter(&i) {
                    scr.fill_bitmap(p, src, &mut out[off(p)..], stride, self.opaque);
                }
                // The layer, dimmed as the bitmaps would be.
                let n = i.w() as usize * 4;
                let d = &mut out[off(i)..];
                for (k, y) in (i.y0..i.y1).enumerate() {
                    d[k * stride * 4..][..n].copy_from_slice(&l.px[at(l.rect, i.x0, y)..][..n]);
                }
                if let Source::Dimmed { alpha, times } = src {
                    scr.dim_rows(d, i, stride, alpha, times);
                }
            }
        }
        if order == Order::Rgba {
            let n = r.w() as usize * 4;
            for i in 0..r.h() as usize {
                crate::wind::swap_rb(&mut out[i * stride * 4..][..n]);
            }
        }
    }
}

impl Edit {
    /// Paint `rects` (window == image coordinates) of the prepared scene
    /// into `hdc`: the bitmaps (or the annotation layer) blitted where only
    /// the capture shows, the chrome composed in band-sized buffers on top.
    /// Each pixel is written once (no flicker under DWM). Committed objects
    /// are never rendered here: they are in the layer.
    pub(super) fn paint_gdi(&self, hdc: HDC, rects: &[PxRect]) {
        let (Some(sc), Some(scr)) = (self.scene.as_ref(), self.gdi.as_ref()) else { return };
        let img = PxRect::image(sc.size);
        let bd = GdiBackdrop { scr, opaque: false };
        let mut lists = scr.lists.borrow_mut();
        let PaintLists { over, ov, bg, tmp, runs, slow } = &mut *lists;
        over.clear();
        sc.chrome_rects_into(over);
        for clip in rects.iter().map(|r| r.intersect(&img)).filter(|r| !r.is_empty()) {
            ov.clear();
            ov.extend(over.iter().map(|o| o.intersect(&clip)));
            *ov = compose::merge_rects(std::mem::take(ov));
            bg.clear();
            bg.push(clip);
            for o in ov.iter() {
                tmp.clear();
                tmp.extend(bg.iter().flat_map(|p| p.minus_iter(o)));
                std::mem::swap(bg, tmp);
            }
            slow.clear();
            for p in bg.iter() {
                runs.clear();
                compose::backdrop_runs_into(sc.sel, sc.size, sc.dim_alpha, *p, runs);
                for &(q, src) in runs.iter() {
                    let li = scr.layer.as_ref().map(|l| (l, q.intersect(&l.rect))).filter(|(_, i)| !i.is_empty());
                    let Some((l, i)) = li else {
                        if !scr.blit(hdc, q, src) {
                            slow.push(q);
                        }
                        continue;
                    };
                    for part in q.minus_iter(&i) {
                        if !scr.blit(hdc, part, src) {
                            slow.push(part);
                        }
                    }
                    if src == Source::Plain {
                        let rows = &l.px[at(l.rect, l.rect.x0, i.y0)..];
                        put_from(hdc, i, rows, l.rect.w(), i.x0 - l.rect.x0);
                    } else {
                        // Objects outside the selection: dimmed in compose.
                        slow.push(i);
                    }
                }
            }
            for o in ov.iter().chain(slow.iter()) {
                self.compose_put(hdc, *o, &bd);
            }
        }
    }

    /// `compose_rect` of `r` into the reused buffer, band by band, then
    /// out to `hdc`.
    fn compose_put(&self, hdc: HDC, r: PxRect, bd: &GdiBackdrop) {
        let Some(scr) = self.gdi.as_ref() else { return };
        let mut buf = scr.buf.borrow_mut();
        // Bands end on pixelate cell edges, so a pixelate draft renders
        // each cell once per paint (not once per band it straddles).
        let grid = self.scene.as_ref().and_then(|sc| sc.draft_grid());
        let h = self.shot.size.1 as i32;
        let mut y = r.y0;
        while y < r.y1 {
            let end = grid.map_or(y + BAND, |g| g.row_edge(y + BAND, h));
            let band = PxRect::new(r.x0, y, r.x1, end.min(r.y1));
            let n = band.w() as usize * band.h() as usize * 4;
            if buf.len() < n {
                buf.resize(n, 0);
            }
            self.compose_rect(band, bd, &mut buf[..n], Order::Bgra);
            put(hdc, band, &buf[..n]);
            y = band.y1;
        }
    }

    /// The export crop (`crop_to_image` of the composed capture): the
    /// annotation layer where it overlaps the selection, `plain` elsewhere,
    /// in one selection-sized buffer; unpremultiplied RGBA.
    pub(super) fn export_gdi(&self, r: crate::objects::FRect) -> crate::pixbuf::PixBuf {
        let scr = self.gdi.as_ref().expect("gdi export without bitmaps");
        let (pw, ph) = (self.shot.size.0 as i32, self.shot.size.1 as i32);
        // Same geometry as `crop_to_image`: floor/ceil, at least 1x1,
        // source indices clamped to the image.
        let x0 = r.x.floor().max(0.0) as i32;
        let y0 = r.y.floor().max(0.0) as i32;
        let w = (super::x1_clamp(r.x, r.w, pw) - x0).max(1);
        let h = (super::y1_clamp(r.y, r.h, ph) - y0).max(1);
        let cx0 = x0.min(pw - 1);
        let cy0 = y0.min(ph - 1);
        let src = PxRect::new(cx0, cy0, (x0 + w).min(pw).max(cx0 + 1), (y0 + h).min(ph).max(cy0 + 1));
        let bd = GdiBackdrop { scr, opaque: true };
        let sw = src.w() as usize;
        let mut px = vec![0u8; sw * src.h() as usize * 4];
        let mut y = src.y0;
        while y < src.y1 {
            let band = PxRect::new(src.x0, y, src.x1, (y + BAND).min(src.y1));
            bd.fill(band, Source::Plain, &mut px[(y - src.y0) as usize * sw * 4..], sw, Order::Rgba);
            y = band.y1;
        }
        if src.w() == w && src.h() == h && src.x0 == x0 && src.y0 == y0 {
            return crate::pixbuf::PixBuf::from_raw(w as u32, h as u32, px).expect("crop size");
        }
        let mut out = crate::pixbuf::PixBuf::new(w as u32, h as u32);
        let d = out.as_raw_mut();
        for yy in 0..h {
            for xx in 0..w {
                let sx = ((x0 + xx).min(pw - 1) - src.x0) as usize;
                let sy = ((y0 + yy).min(ph - 1) - src.y0) as usize;
                let di = (yy as usize * w as usize + xx as usize) * 4;
                d[di..di + 4].copy_from_slice(&px[(sy * sw + sx) * 4..][..4]);
            }
        }
        out
    }
}
