//! BGRA → NV12 (BT.709, limited range), with a box-filter downscale for
//! areas larger than the MP4 limit.
//!
//! NV12 is a full-size Y plane followed by an interleaved U/V plane at half
//! resolution in both directions. Chroma is taken from the average of each
//! 2×2 block. Fixed point, 16 fractional bits.

/// The largest frame recorded to MP4; larger areas are scaled to fit.
pub const MAX_W: u32 = 4096;
pub const MAX_H: u32 = 2304;

/// The encoded size for a `w`×`h` area: scaled down (aspect kept) to fit
/// [`MAX_W`]×[`MAX_H`], then rounded down to even sizes (at least 2×2).
pub fn fit(w: u32, h: u32) -> (u32, u32) {
    let (mut ow, mut oh) = (w.max(1), h.max(1));
    if ow > MAX_W || oh > MAX_H {
        let s = (f64::from(MAX_W) / f64::from(ow)).min(f64::from(MAX_H) / f64::from(oh));
        ow = ((f64::from(ow) * s).round() as u32).clamp(1, MAX_W);
        oh = ((f64::from(oh) * s).round() as u32).clamp(1, MAX_H);
    }
    ((ow & !1).max(2), (oh & !1).max(2))
}

/// Bytes of an NV12 picture of `w`×`h` (both even).
pub fn size(w: u32, h: u32) -> usize {
    w as usize * h as usize * 3 / 2
}

/// Convert `bgra` (`w`×`h`, top-down) to NV12 of `out_w`×`out_h` (even).
/// The same size, or one less in a direction (odd areas), crops; anything
/// else is resampled with a box filter.
pub fn convert(bgra: &[u8], w: u32, h: u32, out_w: u32, out_h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut scratch = Vec::new();
    convert_into(bgra, w, h, out_w, out_h, &mut out, &mut scratch);
    out
}

/// [`convert`] into `out`, reusing `scratch` for the resampled picture.
pub fn convert_into(
    bgra: &[u8],
    w: u32,
    h: u32,
    out_w: u32,
    out_h: u32,
    out: &mut Vec<u8>,
    scratch: &mut Vec<u8>,
) {
    assert!(out_w >= 2 && out_h >= 2 && out_w.is_multiple_of(2) && out_h.is_multiple_of(2), "NV12 size must be even");
    assert!(bgra.len() >= w as usize * h as usize * 4, "short BGRA buffer");
    out.resize(size(out_w, out_h), 0);
    let crop = out_w <= w && out_h <= h && w - out_w <= 1 && h - out_h <= 1;
    if crop {
        to_nv12(bgra, w as usize * 4, out_w as usize, out_h as usize, out);
    } else {
        box_scale(bgra, w as usize, h as usize, out_w as usize, out_h as usize, scratch);
        to_nv12(scratch, out_w as usize * 4, out_w as usize, out_h as usize, out);
    }
}

// BT.709 limited range, ×65536: Y = 16 + 219·(.2126 R + .7152 G + .0722 B)/255,
// U/V = 128 + 224·(B−Y', R−Y' scaled by 1/1.8556, 1/1.5748)/255.
const YR: i32 = 11966;
const YG: i32 = 40254;
const YB: i32 = 4064;
const UR: i32 = -6597;
const UG: i32 = -22187;
const UB: i32 = 28784;
const VR: i32 = 28784;
const VG: i32 = -26147;
const VB: i32 = -2637;

#[inline(always)]
fn luma(p: &[u8]) -> u8 {
    let (b, g, r) = (i32::from(p[0]), i32::from(p[1]), i32::from(p[2]));
    (((YR * r + YG * g + YB * b + 32768) >> 16) + 16) as u8
}

/// The conversion proper: `src` rows are `stride` bytes; `w`×`h` even.
fn to_nv12(src: &[u8], stride: usize, w: usize, h: usize, out: &mut [u8]) {
    let (ys, uvs) = out.split_at_mut(w * h);
    for ((ypair, uvrow), y) in ys.chunks_mut(w * 2).zip(uvs.chunks_mut(w)).zip((0..h).step_by(2)) {
        let (y0, y1) = ypair.split_at_mut(w);
        let r0 = &src[y * stride..y * stride + w * 4];
        let r1 = &src[(y + 1) * stride..(y + 1) * stride + w * 4];
        let it = r0.as_chunks::<8>().0.iter().zip(r1.as_chunks::<8>().0);
        let outs = y0.as_chunks_mut::<2>().0.iter_mut().zip(y1.as_chunks_mut::<2>().0).zip(uvrow.as_chunks_mut::<2>().0);
        for ((a, b), ((ya, yb), uv)) in it.zip(outs) {
            ya[0] = luma(&a[0..4]);
            ya[1] = luma(&a[4..8]);
            yb[0] = luma(&b[0..4]);
            yb[1] = luma(&b[4..8]);
            let sb = i32::from(a[0]) + i32::from(a[4]) + i32::from(b[0]) + i32::from(b[4]);
            let sg = i32::from(a[1]) + i32::from(a[5]) + i32::from(b[1]) + i32::from(b[5]);
            let sr = i32::from(a[2]) + i32::from(a[6]) + i32::from(b[2]) + i32::from(b[6]);
            // Sums of four: one more shift by 2, rounding at 2^17.
            uv[0] = (((UR * sr + UG * sg + UB * sb + 131072) >> 18) + 128) as u8;
            uv[1] = (((VR * sr + VG * sg + VB * sb + 131072) >> 18) + 128) as u8;
        }
    }
}

/// Source span `[a, b)` of output index `i` of `o` over `n` inputs (never empty).
fn span(i: usize, o: usize, n: usize) -> (usize, usize) {
    let a = i * n / o;
    let b = ((i + 1) * n / o).max(a + 1).min(n);
    (a.min(n - 1), b)
}

/// Box-filter `src` (`w`×`h` BGRA) to `ow`×`oh` BGRA in `dst` (each output
/// pixel is the mean of the source pixels it covers; alpha opaque).
fn box_scale(src: &[u8], w: usize, h: usize, ow: usize, oh: usize, dst: &mut Vec<u8>) {
    dst.resize(ow * oh * 4, 0);
    let xs: Vec<(usize, usize)> = (0..ow).map(|x| span(x, ow, w)).collect();
    // Shrinking: the spans partition the source columns, so each source
    // pixel adds to exactly one output column (one table lookup per pixel).
    let col: Option<Vec<u32>> = (ow <= w).then(|| {
        let mut c = vec![0u32; w];
        for (ox, &(x0, x1)) in xs.iter().enumerate() {
            c[x0..x1].fill(ox as u32 * 3);
        }
        c
    });
    // Reciprocals for the box sizes (a division per channel would cost
    // more than the rest of the filter): ⌈2³²/n⌉ gives the exact rounded
    // mean for every n below 4096.
    let max_x = xs.iter().map(|&(a, b)| b - a).max().unwrap_or(1);
    let max_y = (0..oh).map(|y| span(y, oh, h)).map(|(a, b)| b - a).max().unwrap_or(1);
    let inv: Vec<u64> = (0..=max_x * max_y).map(|n| (1u64 << 32).div_ceil(n.max(1) as u64)).collect();
    let mut acc = vec![0u32; ow * 3];
    for (oy, drow) in dst.chunks_mut(ow * 4).enumerate() {
        let (y0, y1) = span(oy, oh, h);
        acc.fill(0);
        for sy in y0..y1 {
            let row = src[sy * w * 4..(sy + 1) * w * 4].as_chunks::<4>().0;
            match &col {
                Some(col) => {
                    for (p, &o) in row.iter().zip(col) {
                        let a = &mut acc[o as usize..o as usize + 3];
                        a[0] += u32::from(p[0]);
                        a[1] += u32::from(p[1]);
                        a[2] += u32::from(p[2]);
                    }
                }
                None => {
                    for (a, &(x0, x1)) in acc.as_chunks_mut::<3>().0.iter_mut().zip(&xs) {
                        for p in &row[x0..x1] {
                            a[0] += u32::from(p[0]);
                            a[1] += u32::from(p[1]);
                            a[2] += u32::from(p[2]);
                        }
                    }
                }
            }
        }
        let rows = y1 - y0;
        for ((d, a), &(x0, x1)) in drow.as_chunks_mut::<4>().0.iter_mut().zip(acc.as_chunks::<3>().0).zip(&xs) {
            let n = (x1 - x0) * rows;
            let (half, inv) = (n as u64 / 2, inv[n]);
            for k in 0..3 {
                d[k] = (((u64::from(a[k]) + half) * inv) >> 32) as u8;
            }
            d[3] = 255;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
        [rgb[2], rgb[1], rgb[0], 255].repeat((w * h) as usize)
    }

    fn yuv_of(rgb: [u8; 3]) -> (u8, u8, u8) {
        let nv = convert(&solid(4, 4, rgb), 4, 4, 4, 4);
        assert_eq!(nv.len(), 24);
        assert!(nv[..16].iter().all(|&y| y == nv[0]), "uniform Y");
        let uv = &nv[16..];
        assert!(uv.as_chunks::<2>().0.iter().all(|c| c == &[uv[0], uv[1]]), "uniform UV");
        (nv[0], uv[0], uv[1])
    }

    #[test]
    fn known_colours_bt709_limited_range() {
        assert_eq!(yuv_of([0, 0, 0]), (16, 128, 128));
        assert_eq!(yuv_of([255, 255, 255]), (235, 128, 128));
        assert_eq!(yuv_of([255, 0, 0]), (63, 102, 240));
        assert_eq!(yuv_of([0, 255, 0]), (173, 42, 26));
        assert_eq!(yuv_of([0, 0, 255]), (32, 240, 118));
        assert_eq!(yuv_of([128, 128, 128]), (126, 128, 128));
    }

    #[test]
    fn chroma_is_the_mean_of_each_block_and_luma_per_pixel() {
        // Left column red, right column blue, in a 2×2 block.
        let mut px = Vec::new();
        for _ in 0..2 {
            px.extend_from_slice(&[0, 0, 255, 255, 255, 0, 0, 255]);
        }
        let nv = convert(&px, 2, 2, 2, 2);
        assert_eq!(&nv[..4], &[63, 32, 63, 32]);
        // Mean of red and blue = (127.5, 0, 127.5).
        let (u, v) = (nv[4], nv[5]);
        assert!((170..=172).contains(&u), "{u}");
        assert!((178..=180).contains(&v), "{v}");
    }

    #[test]
    fn fit_keeps_small_areas_and_scales_big_ones() {
        assert_eq!(fit(1920, 1080), (1920, 1080));
        assert_eq!(fit(1001, 601), (1000, 600));
        assert_eq!(fit(1, 1), (2, 2));
        assert_eq!(fit(4096, 2304), (4096, 2304));
        assert_eq!(fit(8192, 4608), (4096, 2304));
        assert_eq!(fit(7680, 2160), (4096, 1152));
        assert_eq!(fit(3000, 5000), (1382, 2304));
        let (w, h) = fit(5121, 1441);
        assert!(w <= MAX_W && h <= MAX_H && w % 2 == 0 && h % 2 == 0);
        let (a, b) = (5121.0 / 1441.0, f64::from(w) / f64::from(h));
        assert!((a - b).abs() / a < 0.01, "aspect {a} vs {b}");
    }

    #[test]
    fn odd_sizes_crop_the_last_row_and_column() {
        // 3×3: the 2×2 top-left is white, the rest black.
        let mut px = solid(3, 3, [0, 0, 0]);
        for (x, y) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            px[(y * 3 + x) * 4..(y * 3 + x) * 4 + 3].fill(255);
        }
        let nv = convert(&px, 3, 3, 2, 2);
        assert_eq!(nv, vec![235, 235, 235, 235, 128, 128]);
    }

    #[test]
    fn downscale_averages_boxes() {
        // 8×4: columns alternate white and black → every 2×2 box is mid grey.
        let mut px = Vec::new();
        for _ in 0..4 {
            for x in 0..8 {
                let v = if x % 2 == 0 { 255 } else { 0 };
                px.extend_from_slice(&[v, v, v, 255]);
            }
        }
        let nv = convert(&px, 8, 4, 4, 2);
        assert_eq!(nv.len(), size(4, 2));
        // Grey 127.5 → 128 → Y 126.
        assert!(nv[..8].iter().all(|&y| y == 126), "{:?}", &nv[..8]);
        // Left half white, right half black, 8×8 → 4×4 keeps the halves.
        let mut px = Vec::new();
        for _ in 0..8 {
            for x in 0..8 {
                let v = if x < 4 { 255 } else { 0 };
                px.extend_from_slice(&[v, v, v, 255]);
            }
        }
        let nv = convert(&px, 8, 8, 4, 4);
        for row in nv[..16].chunks(4) {
            assert_eq!(row, &[235, 235, 16, 16]);
        }
        // Growing (a 1×1 area becomes 2×2) repeats the pixel.
        let nv = convert(&solid(1, 1, [255, 255, 255]), 1, 1, 2, 2);
        assert_eq!(nv, vec![235, 235, 235, 235, 128, 128]);
    }

    #[test]
    fn downscale_of_a_big_area_has_the_fitted_size() {
        let (w, h) = (4200, 1000);
        let px = solid(w, h, [255, 0, 0]);
        let (ow, oh) = fit(w, h);
        assert!(ow <= MAX_W && oh <= MAX_H);
        let nv = convert(&px, w, h, ow, oh);
        assert_eq!(nv.len(), size(ow, oh));
        assert!(nv[..(ow * oh) as usize].iter().all(|&y| y == 63));
        assert!(nv[(ow * oh) as usize..].as_chunks::<2>().0.iter().all(|c| c == &[102, 240]));
    }
}

#[cfg(test)]
mod bench {
    use super::*;
    use std::time::Instant;

    #[test]
    #[ignore = "benchmark: prints conversion timings"]
    fn bench_nv12() {
        for (w, h) in [(1920u32, 1080u32), (5120, 1440), (7680, 4320)] {
            let px: Vec<u8> = (0..w * h * 4).map(|i| (i * 7 % 251) as u8).collect();
            let (ow, oh) = fit(w, h);
            let (mut out, mut scratch) = (Vec::new(), Vec::new());
            convert_into(&px, w, h, ow, oh, &mut out, &mut scratch);
            let n = 10;
            let t = Instant::now();
            for _ in 0..n {
                convert_into(&px, w, h, ow, oh, &mut out, &mut scratch);
            }
            let all = t.elapsed() / n;
            let t = Instant::now();
            if (ow, oh) != (w, h) {
                for _ in 0..n {
                    box_scale(&px, w as usize, h as usize, ow as usize, oh as usize, &mut scratch);
                }
            }
            eprintln!("{w}x{h} -> {ow}x{oh}: convert {all:?}, of which box {:?}", t.elapsed() / n);
        }
    }
}
