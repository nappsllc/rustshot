//! Hand-written animated GIF encoder (GIF89a, loops forever).
//!
//! Each frame is reduced to the bounding box of the pixels that changed
//! since the previous frame; inside it, unchanged pixels use a transparent
//! index (frames are not disposed, so the previous frame shows through).
//! The box is quantised to at most 255 colours by median cut over a 5-6-5
//! histogram (no dither) and compressed with variable-width LZW. A frame's
//! delay is patched in once the next frame's timestamp is known
//! (centiseconds, rounded on the running total so errors do not add up);
//! frames identical to the previous one only extend its delay.

use super::{Frame, VideoEncoder};
use anyhow::{bail, Context, Result};
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::Duration;

/// Shortest delay written (many viewers treat 0-1 cs as 10 cs).
const MIN_DELAY_CS: u64 = 2;
/// Colours per frame; one more index is kept for transparency.
const MAX_COLORS: usize = 255;

pub struct GifEncoder<W: Write + Seek> {
    w: W,
    width: u32,
    height: u32,
    fps: u32,
    /// The previous frame's pixels (BGRA), to find what changed.
    prev: Option<Vec<u8>>,
    /// Time of the first written frame.
    first_ts: Option<Duration>,
    /// Time of the latest frame pushed (written or not).
    last_ts: Duration,
    /// Centiseconds of delay written so far.
    written_cs: u64,
    /// Stream offset of the last written frame's delay field.
    delay_at: Option<u64>,
    quant: Quantizer,
    indices: Vec<u8>,
}

impl GifEncoder<BufWriter<std::fs::File>> {
    /// Create (or truncate) `path` and write the GIF header.
    pub fn create(path: &Path, w: u32, h: u32, fps: u32) -> Result<Self> {
        let f = std::fs::File::create(path).with_context(|| format!("create {}", path.display()))?;
        GifEncoder::new(BufWriter::with_capacity(1 << 16, f), w, h, fps)
    }
}

impl<W: Write + Seek> GifEncoder<W> {
    /// Write the header and loop block to `w`; frames must be `w`×`h`.
    pub fn new(mut w: W, width: u32, height: u32, fps: u32) -> Result<Self> {
        if width == 0 || height == 0 || width > 0xFFFF || height > 0xFFFF {
            bail!("GIF size {width}x{height} is out of range (1-65535)");
        }
        w.write_all(b"GIF89a")?;
        w.write_all(&(width as u16).to_le_bytes())?;
        w.write_all(&(height as u16).to_le_bytes())?;
        // No global colour table; 8-bit colour resolution; background 0; no aspect.
        w.write_all(&[0x70, 0, 0])?;
        // NETSCAPE2.0 application extension: loop forever.
        w.write_all(b"\x21\xFF\x0BNETSCAPE2.0\x03\x01\x00\x00\x00")?;
        Ok(GifEncoder {
            w,
            width,
            height,
            fps: fps.max(1),
            prev: None,
            first_ts: None,
            last_ts: Duration::ZERO,
            written_cs: 0,
            delay_at: None,
            quant: Quantizer::new(),
            indices: Vec::new(),
        })
    }

    /// The nominal frame time in centiseconds.
    fn frame_cs(&self) -> u64 {
        ((100 + self.fps as u64 / 2) / self.fps as u64).max(MIN_DELAY_CS)
    }

    /// Fill in the last frame's delay so it lasts until `end_cs` (since
    /// the first frame).
    fn patch_delay(&mut self, end_cs: u64) -> Result<()> {
        let Some(at) = self.delay_at.take() else { return Ok(()) };
        let d = end_cs.saturating_sub(self.written_cs).clamp(MIN_DELAY_CS, 0xFFFF);
        self.written_cs += d;
        let here = self.w.stream_position()?;
        self.w.seek(SeekFrom::Start(at))?;
        self.w.write_all(&(d as u16).to_le_bytes())?;
        self.w.seek(SeekFrom::Start(here))?;
        Ok(())
    }

    fn since_first_cs(&self, ts: Duration) -> u64 {
        let t = ts.saturating_sub(self.first_ts.unwrap_or(ts));
        ((t.as_nanos() + 5_000_000) / 10_000_000) as u64
    }

    /// Last delay and the trailer. The last frame lasts until the last
    /// pushed frame's time plus one frame.
    fn end(&mut self) -> Result<()> {
        let end = self.since_first_cs(self.last_ts) + self.frame_cs();
        self.patch_delay(end)?;
        self.w.write_all(&[0x3B])?;
        self.w.flush()?;
        Ok(())
    }

    fn write_frame(&mut self, f: &Frame) -> Result<()> {
        let (w, h) = (self.width as usize, self.height as usize);
        let px = f.bgra.as_chunks::<4>().0;
        let changed = changed_box(px, self.prev.as_deref().map(|p| p.as_chunks::<4>().0), w, h);
        let Some((x0, y0, x1, y1)) = changed else {
            return Ok(()); // identical: the previous frame lasts longer
        };
        let (bw, bh) = (x1 - x0, y1 - y0);
        if self.first_ts.is_none() {
            self.first_ts = Some(f.ts);
        }
        let now_cs = self.since_first_cs(f.ts);
        self.patch_delay(now_cs)?;
        let prev = self.prev.as_deref().map(|p| p.as_chunks::<4>().0);

        // Histogram of the changed pixels in the box.
        let same = |i: usize| prev.is_some_and(|p| rgb(p[i]) == rgb(px[i]));
        // Pixel indices of the box, row by row.
        let in_box = || (y0..y1).flat_map(|y| y * w + x0..y * w + x1);
        self.quant.clear();
        for i in in_box().filter(|&i| !same(i)) {
            self.quant.add(px[i]);
        }
        let palette = self.quant.build(MAX_COLORS);
        let transparent = prev.is_some();
        let tidx = palette.len() as u8; // < 256: at most 255 colours
        let used = palette.len() + usize::from(transparent);
        let bits = (usize::BITS - (used - 1).max(1).leading_zeros()).max(1);
        self.indices.clear();
        self.indices.reserve(bw * bh);
        let quant = &self.quant;
        self.indices.extend(in_box().map(|i| if same(i) { tidx } else { quant.index(px[i]) }));

        let out = &mut self.w;
        // Graphic control extension: do not dispose, delay patched later.
        let packed = (1 << 2) | u8::from(transparent);
        out.write_all(&[0x21, 0xF9, 0x04, packed])?;
        self.delay_at = Some(out.stream_position()?);
        out.write_all(&[0, 0, if transparent { tidx } else { 0 }, 0])?;
        // Image descriptor with a local colour table.
        out.write_all(&[0x2C])?;
        for v in [x0, y0, bw, bh] {
            out.write_all(&(v as u16).to_le_bytes())?;
        }
        out.write_all(&[0x80 | (bits as u8 - 1)])?;
        let mut table = vec![0u8; 3 << bits];
        for (i, c) in palette.iter().enumerate() {
            table[i * 3..i * 3 + 3].copy_from_slice(c);
        }
        out.write_all(&table)?;
        let min = (bits as u8).max(2);
        out.write_all(&[min])?;
        for block in lzw_encode(&self.indices, min).chunks(255) {
            out.write_all(&[block.len() as u8])?;
            out.write_all(block)?;
        }
        out.write_all(&[0])?;
        Ok(())
    }
}

impl<W: Write + Seek + Send> VideoEncoder for GifEncoder<W> {
    fn push_video(&mut self, f: &Frame) -> Result<()> {
        if (f.w, f.h) != (self.width, self.height) || f.bgra.len() != f.w as usize * f.h as usize * 4 {
            bail!("frame is {}x{} ({} bytes); the GIF is {}x{}", f.w, f.h, f.bgra.len(), self.width, self.height);
        }
        self.write_frame(f)?;
        self.last_ts = self.last_ts.max(f.ts);
        match &mut self.prev {
            Some(p) => p.copy_from_slice(&f.bgra),
            None => self.prev = Some(f.bgra.clone()),
        }
        Ok(())
    }

    fn push_audio(&mut self, _pcm: &[f32], _ts: Duration) -> Result<()> {
        Ok(()) // GIF has no sound
    }

    fn finish(mut self: Box<Self>) -> Result<()> {
        self.end()
    }

    fn audio(&self) -> bool {
        false
    }
}

fn rgb(p: [u8; 4]) -> u32 {
    u32::from_le_bytes(p) & 0x00FF_FFFF
}

/// Bounding box `(x0, y0, x1, y1)` (end exclusive) of the pixels of `px`
/// that differ from `prev` (the whole frame without one); `None` when
/// nothing changed.
fn changed_box(px: &[[u8; 4]], prev: Option<&[[u8; 4]]>, w: usize, h: usize) -> Option<(usize, usize, usize, usize)> {
    let Some(prev) = prev else { return Some((0, 0, w, h)) };
    let row_diff = |y: usize| {
        let (a, b) = (&px[y * w..(y + 1) * w], &prev[y * w..(y + 1) * w]);
        let first = (0..w).find(|&x| rgb(a[x]) != rgb(b[x]))?;
        let last = (0..w).rev().find(|&x| rgb(a[x]) != rgb(b[x]))?;
        Some((first, last + 1))
    };
    let mut bbox: Option<(usize, usize, usize, usize)> = None;
    for y in 0..h {
        if let Some((a, b)) = row_diff(y) {
            bbox = Some(match bbox {
                None => (a, y, b, y + 1),
                Some((x0, y0, x1, _)) => (x0.min(a), y0, x1.max(b), y + 1),
            });
        }
    }
    bbox
}

/// Median-cut quantiser over a 5-6-5 histogram; buffers are kept between
/// frames.
struct Quantizer {
    count: Vec<u32>,
    sum: Vec<[u64; 3]>,
    touched: Vec<u16>,
    map: Vec<u8>,
}

/// Histogram bin of a BGRA pixel: r5 g6 b5.
fn bin(p: [u8; 4]) -> usize {
    ((p[2] as usize >> 3) << 11) | ((p[1] as usize >> 2) << 5) | (p[0] as usize >> 3)
}

/// A bin's channel value on the 0-255 scale (axis 0 = R, 1 = G, 2 = B).
fn bin_axis(b: u16, axis: usize) -> u32 {
    let b = b as u32;
    match axis {
        0 => (b >> 11) << 3,
        1 => ((b >> 5) & 0x3F) << 2,
        _ => (b & 0x1F) << 3,
    }
}

impl Quantizer {
    fn new() -> Quantizer {
        Quantizer { count: vec![0; 1 << 16], sum: vec![[0; 3]; 1 << 16], touched: Vec::new(), map: vec![0; 1 << 16] }
    }

    fn clear(&mut self) {
        for &b in &self.touched {
            self.count[b as usize] = 0;
            self.sum[b as usize] = [0; 3];
        }
        self.touched.clear();
    }

    fn add(&mut self, p: [u8; 4]) {
        let b = bin(p);
        if self.count[b] == 0 {
            self.touched.push(b as u16);
        }
        self.count[b] += 1;
        let s = &mut self.sum[b];
        s[0] += p[2] as u64;
        s[1] += p[1] as u64;
        s[2] += p[0] as u64;
    }

    /// The palette (RGB, at most `max` colours) for the histogram; fills
    /// the bin → index map used by [`Quantizer::index`].
    fn build(&mut self, max: usize) -> Vec<[u8; 3]> {
        struct Cut {
            start: usize,
            end: usize,
            count: u64,
            axis: usize,
            range: u32,
        }
        let count = &self.count;
        let measure = |bins: &[u16], start: usize, end: usize| {
            let mut lo = [u32::MAX; 3];
            let mut hi = [0u32; 3];
            let mut n = 0u64;
            for &b in &bins[start..end] {
                n += count[b as usize] as u64;
                for a in 0..3 {
                    let v = bin_axis(b, a);
                    lo[a] = lo[a].min(v);
                    hi[a] = hi[a].max(v);
                }
            }
            let axis = (0..3).max_by_key(|&a| (hi[a] - lo[a], 2 - a)).unwrap_or(0);
            Cut { start, end, count: n, axis, range: hi[axis] - lo[axis] }
        };
        let mut bins = std::mem::take(&mut self.touched);
        let mut cuts = vec![measure(&bins, 0, bins.len())];
        while cuts.len() < max {
            let Some(k) = (0..cuts.len())
                .filter(|&k| cuts[k].end - cuts[k].start > 1)
                .max_by_key(|&k| cuts[k].range as u64 * cuts[k].count)
            else {
                break;
            };
            let Cut { start, end, count: n, axis, .. } = cuts[k];
            bins[start..end].sort_unstable_by_key(|&b| bin_axis(b, axis));
            // Split at the median pixel, leaving both halves non-empty.
            let mut acc = 0u64;
            let mut mid = start + 1;
            for (i, &b) in bins[start..end - 1].iter().enumerate() {
                acc += count[b as usize] as u64;
                mid = start + i + 1;
                if acc * 2 >= n {
                    break;
                }
            }
            cuts[k] = measure(&bins, start, mid);
            cuts.push(measure(&bins, mid, end));
        }
        let mut palette = Vec::with_capacity(cuts.len());
        for (i, c) in cuts.iter().enumerate() {
            let mut s = [0u64; 3];
            for &b in &bins[c.start..c.end] {
                self.map[b as usize] = i as u8;
                for (acc, v) in s.iter_mut().zip(self.sum[b as usize]) {
                    *acc += v;
                }
            }
            let n = c.count.max(1);
            palette.push(s.map(|v| ((v + n / 2) / n) as u8));
        }
        self.touched = bins;
        palette
    }

    fn index(&self, p: [u8; 4]) -> u8 {
        self.map[bin(p)]
    }
}

/// LSB-first bit packer.
struct Bits {
    out: Vec<u8>,
    acc: u32,
    n: u32,
}

impl Bits {
    fn put(&mut self, code: u32, width: u32) {
        self.acc |= code << self.n;
        self.n += width;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    fn done(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push(self.acc as u8);
        }
        self.out
    }
}

/// GIF LZW: codes start at `min + 1` bits and grow to 12; a clear code
/// starts over when the table is full; ends with the end code.
pub(crate) fn lzw_encode(data: &[u8], min: u8) -> Vec<u8> {
    const SLOTS: usize = 1 << 13;
    const EMPTY: u32 = u32::MAX;
    let min = u32::from(min.clamp(2, 8));
    let clear = 1u32 << min;
    let eoi = clear + 1;
    let mut keys = vec![EMPTY; SLOTS];
    let mut vals = vec![0u16; SLOTS];
    let slot = |key: u32| (key.wrapping_mul(2_654_435_761) >> (32 - 13)) as usize;
    let mut bits = Bits { out: Vec::with_capacity(data.len() / 2 + 16), acc: 0, n: 0 };
    let mut width = min + 1;
    let mut hi = eoi; // the last code assigned
    bits.put(clear, width);
    let Some((&first, rest)) = data.split_first() else {
        bits.put(eoi, width);
        return bits.done();
    };
    // Assign the next code; true when the table filled and was cleared.
    let bump = |bits: &mut Bits, width: &mut u32, hi: &mut u32, keys: &mut [u32]| {
        *hi += 1;
        if *hi == 1 << *width {
            *width += 1;
        }
        if *hi == 4095 {
            bits.put(clear, *width);
            *width = min + 1;
            *hi = eoi;
            keys.fill(EMPTY);
            return true;
        }
        false
    };
    let mut cur = u32::from(first);
    for &b in rest {
        let key = (cur << 8) | u32::from(b);
        let mut s = slot(key);
        loop {
            match keys[s] {
                EMPTY => break,
                k if k == key => break,
                _ => s = (s + 1) & (SLOTS - 1),
            }
        }
        if keys[s] == key {
            cur = u32::from(vals[s]);
            continue;
        }
        bits.put(cur, width);
        if !bump(&mut bits, &mut width, &mut hi, &mut keys) {
            keys[s] = key;
            vals[s] = hi as u16;
        }
        cur = u32::from(b);
    }
    bits.put(cur, width);
    bump(&mut bits, &mut width, &mut hi, &mut keys);
    bits.put(eoi, width);
    bits.done()
}

#[cfg(test)]
mod tests;
