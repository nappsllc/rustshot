//! Minimal RGBA8 image buffer with PNG codec, replacing the `image` crate.

use anyhow::{anyhow, Context, Result};
use std::path::Path;

#[derive(Clone, Debug, PartialEq)]
pub struct PixBuf {
    w: u32,
    h: u32,
    d: Vec<u8>,
}

impl PixBuf {
    pub fn new(w: u32, h: u32) -> Self {
        PixBuf {
            w,
            h,
            d: vec![0; (w as usize) * (h as usize) * 4],
        }
    }

    #[cfg(test)]
    pub fn from_pixel(w: u32, h: u32, px: [u8; 4]) -> Self {
        let mut p = Self::new(w, h);
        for c in p.d.chunks_exact_mut(4) {
            c.copy_from_slice(&px);
        }
        p
    }

    pub fn from_raw(w: u32, h: u32, d: Vec<u8>) -> Option<Self> {
        if w == 0 || h == 0 || d.len() != (w as usize) * (h as usize) * 4 {
            return None;
        }
        Some(PixBuf { w, h, d })
    }

    pub fn width(&self) -> u32 {
        self.w
    }

    pub fn height(&self) -> u32 {
        self.h
    }

    pub fn dimensions(&self) -> (u32, u32) {
        (self.w, self.h)
    }

    pub fn as_raw(&self) -> &Vec<u8> {
        &self.d
    }

    pub fn as_raw_mut(&mut self) -> &mut Vec<u8> {
        &mut self.d
    }

    #[cfg(test)]
    pub fn get_pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = (y as usize * self.w as usize + x as usize) * 4;
        [self.d[i], self.d[i + 1], self.d[i + 2], self.d[i + 3]]
    }

    pub fn put_pixel(&mut self, x: u32, y: u32, px: [u8; 4]) {
        let i = y as usize * self.w as usize + x as usize;
        self.d[i * 4..i * 4 + 4].copy_from_slice(&px);
    }

    /// Copy a rect out of the buffer (bounds are the caller's contract).
    pub fn crop(&self, x: u32, y: u32, w: u32, h: u32) -> PixBuf {
        debug_assert!(x + w <= self.w && y + h <= self.h);
        let mut out = PixBuf::new(w, h);
        let stride = w as usize * 4;
        for row in 0..h as usize {
            let s = ((y as usize + row) * self.w as usize + x as usize) * 4;
            let d = row * stride;
            out.d[d..d + stride].copy_from_slice(&self.d[s..s + stride]);
        }
        out
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        std::fs::write(path, self.to_png()?).context("write png")
    }

    pub fn to_png(&self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, self.w, self.h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut wr = enc.write_header().context("png header")?;
            wr.write_image_data(&self.d).context("png data")?;
        }
        Ok(out)
    }

    pub fn from_png(bytes: &[u8]) -> Result<PixBuf> {
        let dec = png::Decoder::new(std::io::Cursor::new(bytes));
        let mut reader = dec.read_info().context("png info")?;
        let mut buf = vec![0u8; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buf).context("png frame")?;
        buf.truncate(info.buffer_size());
        let (w, h) = (info.width, info.height);
        let rgba = match info.color_type {
            png::ColorType::Rgba => buf,
            png::ColorType::Rgb => {
                let mut o = Vec::with_capacity((w as usize * h as usize) * 4);
                for c in buf.chunks_exact(3) {
                    o.extend_from_slice(c);
                    o.push(255);
                }
                o
            }
            other => return Err(anyhow!("unsupported png color type {other:?}")),
        };
        PixBuf::from_raw(w, h, rgba).ok_or_else(|| anyhow!("bad png dimensions"))
    }
}
