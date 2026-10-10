use super::*;
use std::io::Cursor;

/// A decoded frame: where it went, its colour table and the composited
/// picture after it (RGB).
struct DFrame {
    delay: u16,
    rect: (usize, usize, usize, usize),
    palette: Vec<[u8; 3]>,
    transparent: Option<u8>,
    disposal: u8,
    indices: Vec<u8>,
    canvas: Vec<[u8; 3]>,
}

struct Decoded {
    w: usize,
    h: usize,
    loops: Option<u16>,
    frames: Vec<DFrame>,
}

/// GIF LZW decoder; also returns how many clear codes it read.
fn lzw_decode(data: &[u8], min: u8) -> (Vec<u8>, usize) {
    let min = u32::from(min);
    let clear = 1usize << min;
    let eoi = clear + 1;
    let fresh = || -> Vec<Vec<u8>> { (0..clear).map(|i| vec![i as u8]).chain([vec![], vec![]]).collect() };
    let mut table = fresh();
    let mut width = min + 1;
    let (mut acc, mut n, mut at) = (0u32, 0u32, 0usize);
    let mut out = Vec::new();
    let mut prev: Option<usize> = None;
    let mut clears = 0;
    loop {
        while n < width {
            let b = *data.get(at).expect("ran out of data before the end code");
            acc |= u32::from(b) << n;
            n += 8;
            at += 1;
        }
        let code = (acc & ((1 << width) - 1)) as usize;
        acc >>= width;
        n -= width;
        if code == clear {
            table = fresh();
            width = min + 1;
            prev = None;
            clears += 1;
            continue;
        }
        if code == eoi {
            break;
        }
        let entry = if code < table.len() {
            table[code].clone()
        } else {
            let p = prev.expect("code before any entry");
            assert_eq!(code, table.len(), "code {code} out of order");
            let mut e = table[p].clone();
            e.push(table[p][0]);
            e
        };
        out.extend_from_slice(&entry);
        if let Some(p) = prev
            && table.len() < 4096
        {
            let mut e = table[p].clone();
            e.push(entry[0]);
            table.push(e);
        }
        if table.len() == 1 << width && width < 12 {
            width += 1;
        }
        prev = Some(code);
    }
    (out, clears)
}

fn u16_at(b: &[u8], i: usize) -> usize {
    u16::from_le_bytes([b[i], b[i + 1]]) as usize
}

fn decode(b: &[u8]) -> Decoded {
    assert_eq!(&b[..6], b"GIF89a");
    let (w, h) = (u16_at(b, 6), u16_at(b, 8));
    let mut i = 13;
    assert_eq!(b[10] & 0x80, 0, "no global colour table");
    let mut canvas = vec![[0u8; 3]; w * h];
    let mut frames = Vec::new();
    let mut loops = None;
    let (mut delay, mut transparent, mut disposal) = (0u16, None, 0u8);
    let blocks = |i: &mut usize| {
        let mut data = Vec::new();
        loop {
            let len = b[*i] as usize;
            *i += 1;
            if len == 0 {
                return data;
            }
            data.extend_from_slice(&b[*i..*i + len]);
            *i += len;
        }
    };
    loop {
        match b[i] {
            0x3B => break,
            0x21 => {
                let label = b[i + 1];
                i += 2;
                let data = blocks(&mut i);
                match label {
                    0xF9 => {
                        assert_eq!(data.len(), 4);
                        disposal = (data[0] >> 2) & 7;
                        delay = u16::from_le_bytes([data[1], data[2]]);
                        transparent = (data[0] & 1 != 0).then_some(data[3]);
                    }
                    0xFF if data.starts_with(b"NETSCAPE2.0") => {
                        assert_eq!(&data[11..], &[1, 0, 0]);
                        loops = Some(u16::from_le_bytes([data[12], data[13]]));
                    }
                    _ => {}
                }
            }
            0x2C => {
                let (x, y, fw, fh) = (u16_at(b, i + 1), u16_at(b, i + 3), u16_at(b, i + 5), u16_at(b, i + 7));
                let packed = b[i + 9];
                i += 10;
                assert!(packed & 0x80 != 0, "local colour table");
                assert_eq!(packed & 0x40, 0, "not interlaced");
                let size = 2usize << (packed & 7);
                let palette: Vec<[u8; 3]> = b[i..i + size * 3].as_chunks::<3>().0.to_vec();
                i += size * 3;
                let min = b[i];
                i += 1;
                let (indices, _) = lzw_decode(&blocks(&mut i), min);
                assert_eq!(indices.len(), fw * fh, "pixel count");
                assert!(x + fw <= w && y + fh <= h, "frame inside the canvas");
                for (k, &v) in indices.iter().enumerate() {
                    if Some(v) == transparent {
                        continue;
                    }
                    assert!((v as usize) < size, "index in the table");
                    canvas[(y + k / fw) * w + x + k % fw] = palette[v as usize];
                }
                frames.push(DFrame {
                    delay,
                    rect: (x, y, fw, fh),
                    palette,
                    transparent,
                    disposal,
                    indices,
                    canvas: canvas.clone(),
                });
            }
            other => panic!("unexpected block 0x{other:02X} at {i}"),
        }
    }
    assert_eq!(i, b.len() - 1, "trailer is last");
    Decoded { w, h, loops, frames }
}

fn frame(w: u32, h: u32, ms: u64, color: impl Fn(usize, usize) -> [u8; 3]) -> Frame {
    let mut bgra = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h as usize {
        for x in 0..w as usize {
            let [r, g, b] = color(x, y);
            bgra.extend_from_slice(&[b, g, r, 255]);
        }
    }
    Frame { w, h, bgra, ts: Duration::from_millis(ms) }
}

fn rgb_of(f: &Frame) -> Vec<[u8; 3]> {
    f.bgra.as_chunks::<4>().0.iter().map(|p| [p[2], p[1], p[0]]).collect()
}

/// Encode `frames` (w×h at `fps`) to memory and decode the result.
fn round_trip(w: u32, h: u32, fps: u32, frames: &[Frame]) -> (Vec<u8>, Decoded) {
    let mut enc = GifEncoder::new(Cursor::new(Vec::new()), w, h, fps).unwrap();
    for f in frames {
        enc.push_video(f).unwrap();
    }
    enc.end().unwrap();
    let bytes = enc.w.into_inner();
    let d = decode(&bytes);
    (bytes, d)
}

/// A tiny xorshift generator for test data.
fn noise(seed: u32, n: usize) -> Vec<u8> {
    let mut x = seed.max(1);
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x >> 24) as u8
        })
        .collect()
}

#[test]
fn lzw_round_trip() {
    for min in 2..=8u8 {
        let mask = ((1u16 << min) - 1) as u8;
        let random: Vec<u8> = noise(min as u32, 30_000).iter().map(|v| v & mask).collect();
        let mut runs = vec![0u8; 5000];
        runs.extend(std::iter::repeat_n(mask, 7000));
        let ramp: Vec<u8> = (0..20_000u32).map(|i| (i / 7) as u8 & mask).collect();
        for data in [vec![], vec![1 & mask], vec![mask, mask], random.clone(), runs, ramp] {
            let enc = lzw_encode(&data, min);
            let (back, _) = lzw_decode(&enc, min);
            assert_eq!(back, data, "min {min}, {} bytes", data.len());
        }
        // Random data fills the table: the encoder starts over with clear codes.
        let (_, clears) = lzw_decode(&lzw_encode(&random, min), min);
        assert!(clears >= 2, "min {min}: {clears} clear codes");
    }
    // Lengths around the 4095-code boundary.
    for n in 4080..4110 {
        let data = noise(n as u32, n * 3);
        assert_eq!(lzw_decode(&lzw_encode(&data, 8), 8).0, data, "{n}");
    }
}

#[test]
fn many_colours_fit_one_palette() {
    // 64×64 gradient: 4096 distinct colours.
    let f = frame(64, 64, 0, |x, y| [(x * 4) as u8, (y * 4) as u8, ((x + y) * 2) as u8]);
    let (_, d) = round_trip(64, 64, 15, std::slice::from_ref(&f));
    assert_eq!((d.w, d.h, d.frames.len()), (64, 64, 1));
    let fr = &d.frames[0];
    assert!(fr.palette.len() <= 256);
    let mut used: Vec<u8> = fr.indices.clone();
    used.sort_unstable();
    used.dedup();
    assert!(used.len() <= 255 && used.len() > 128, "{} colours used", used.len());
    assert_eq!(fr.transparent, None, "nothing to show through on the first frame");
    let worst = rgb_of(&f)
        .iter()
        .zip(&fr.canvas)
        .map(|(a, b)| (0..3).map(|c| a[c].abs_diff(b[c])).max().unwrap())
        .max()
        .unwrap();
    assert!(worst <= 24, "quantisation error {worst}");
}

#[test]
fn unchanged_pixels_are_transparent_in_the_changed_box() {
    let bg = [20, 40, 60];
    let a = frame(16, 10, 0, |_, _| bg);
    let b = frame(16, 10, 100, |x, y| match (x, y) {
        (2, 2) => [255, 0, 0],
        (8, 5) => [0, 0, 255],
        _ => bg,
    });
    let (_, d) = round_trip(16, 10, 10, &[a.clone(), b.clone()]);
    assert_eq!(d.frames.len(), 2);
    assert_eq!(d.frames[0].rect, (0, 0, 16, 10));
    assert_eq!(d.frames[0].canvas, rgb_of(&a), "few colours are exact");
    let f = &d.frames[1];
    assert_eq!(f.rect, (2, 2, 7, 4), "only the changed box");
    assert_eq!(f.disposal, 1, "kept for the next frame to show through");
    let t = f.transparent.expect("transparent index");
    assert_eq!(f.indices.iter().filter(|&&v| v != t).count(), 2, "only the changed pixels are opaque");
    assert_ne!(f.indices[0], t);
    assert_ne!(f.indices[6 + 3 * 7], t);
    assert_eq!(f.canvas, rgb_of(&b), "composited picture is the second frame");
}

#[test]
fn delays_follow_timestamps_without_drift() {
    // 15 fps: 66.7 ms apart → 7, 6, 7 cs (rounded on the running total);
    // the last frame lasts one frame (7 cs).
    let fr: Vec<Frame> = (0..4).map(|i| frame(4, 4, i * 200 / 3, move |_, _| [i as u8 * 50, 0, 0])).collect();
    let (_, d) = round_trip(4, 4, 15, &fr);
    let delays: Vec<u16> = d.frames.iter().map(|f| f.delay).collect();
    assert_eq!(delays, [7, 6, 7, 7]);
    // An identical frame extends the previous one; a too-close frame gets
    // the minimum and the next delay makes up for it.
    let red = |ms| frame(4, 4, ms, |_, _| [255, 0, 0]);
    let blue = |ms| frame(4, 4, ms, |_, _| [0, 0, 255]);
    let (_, d) = round_trip(4, 4, 10, &[red(0), red(100), blue(250), red(260), blue(400), blue(500)]);
    let delays: Vec<u16> = d.frames.iter().map(|f| f.delay).collect();
    // red 0→250 = 25; blue 250→260 = 1 → 2; red until 400: 40-27 = 13;
    // blue from 400, last pushed at 500 (unchanged), + 10 cs = 60 - 40 = 20.
    assert_eq!(delays, [25, 2, 13, 20]);
    assert_eq!(delays.iter().map(|&d| d as u32).sum::<u32>(), 60, "total matches the clock");
}

#[test]
fn loops_forever() {
    let (bytes, d) = round_trip(2, 2, 15, &[frame(2, 2, 0, |_, _| [1, 2, 3])]);
    assert_eq!(d.loops, Some(0));
    let at = bytes.windows(11).position(|w| w == b"NETSCAPE2.0").expect("loop block");
    assert_eq!(&bytes[at - 3..at], &[0x21, 0xFF, 0x0B]);
    assert_eq!(&bytes[at + 11..at + 16], &[3, 1, 0, 0, 0]);
}

#[test]
fn rejects_bad_sizes_and_writes_files() {
    assert!(GifEncoder::new(Cursor::new(Vec::new()), 0, 5, 15).is_err());
    assert!(GifEncoder::new(Cursor::new(Vec::new()), 70_000, 5, 15).is_err());
    let mut enc = GifEncoder::new(Cursor::new(Vec::new()), 4, 4, 15).unwrap();
    assert!(enc.push_video(&frame(5, 4, 0, |_, _| [0; 3])).is_err());
    let mut short = frame(4, 4, 0, |_, _| [0; 3]);
    short.bgra.pop();
    assert!(enc.push_video(&short).is_err());
    assert!(!enc.audio());

    let dir = std::env::temp_dir().join(format!("rustshot-gif-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("t.gif");
    let mut enc: Box<dyn VideoEncoder> = Box::new(GifEncoder::create(&p, 8, 8, 10).unwrap());
    for i in 0..20u64 {
        enc.push_video(&frame(8, 8, i * 100, move |x, _| if x as u64 == i % 8 { [255; 3] } else { [0; 3] })).unwrap();
    }
    enc.push_audio(&[0.0; 4], Duration::ZERO).unwrap();
    enc.finish().unwrap();
    let d = decode(&std::fs::read(&p).unwrap());
    assert_eq!(d.frames.len(), 20);
    assert!(d.frames.iter().all(|f| f.delay == 10));
    let _ = std::fs::remove_dir_all(&dir);
}
