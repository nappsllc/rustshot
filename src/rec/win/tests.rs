//! Live tests of the Windows recorder: they open real windows, capture the
//! real desktop and play sound through the default device, so they are
//! `#[ignore]`d and run by CI's desktop job (names start `live_rec_`).
//! `bench_rec_capture` prints timings and is run by hand.

use super::*;
use crate::rec::gif::GifEncoder;
use crate::rec::{AudioSource, Quality, RecFormat, RecSpec, Session, VideoEncoder};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, InvalidateRect, HGDIOBJ, PAINTSTRUCT};
use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

/// Colours the test window cycles through, far apart in luma.
const COLOURS: [(u8, u8, u8); 6] = [(255, 0, 0), (0, 255, 0), (0, 0, 255), (255, 255, 255), (0, 0, 0), (255, 255, 0)];

/// A top-most popup on its own thread, changing colour every `period`.
struct TestWindow {
    hwnd: isize,
    thread: Option<std::thread::JoinHandle<()>>,
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_TIMER => {
                let n = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, n + 1);
                let _ = InvalidateRect(Some(hwnd), None, false);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let dc = BeginPaint(hwnd, &mut ps);
                let (r, g, b) = COLOURS[GetWindowLongPtrW(hwnd, GWLP_USERDATA) as usize % COLOURS.len()];
                let brush = CreateSolidBrush(COLORREF(u32::from(r) | u32::from(g) << 8 | u32::from(b) << 16));
                let mut rc = RECT::default();
                let _ = GetClientRect(hwnd, &mut rc);
                FillRect(dc, &rc, brush);
                let _ = DeleteObject(HGDIOBJ(brush.0));
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

impl TestWindow {
    fn open(x: i32, y: i32, w: i32, h: i32, period_ms: u32) -> TestWindow {
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::spawn(move || unsafe {
            let inst = GetModuleHandleW(None).unwrap();
            let class = w!("rustshot_rec_test_window");
            let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: inst.into(), lpszClassName: class, ..Default::default() };
            RegisterClassW(&wc); // fails harmlessly when already registered
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class,
                w!("Rustshot recording test"),
                WS_POPUP | WS_VISIBLE,
                x,
                y,
                w,
                h,
                None,
                None,
                Some(inst.into()),
                None,
            )
            .unwrap();
            SetTimer(Some(hwnd), 1, period_ms, None);
            tx.send(hwnd.0 as isize).unwrap();
            let mut m = MSG::default();
            while GetMessageW(&mut m, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&m);
                DispatchMessageW(&m);
            }
        });
        let hwnd = rx.recv().unwrap();
        std::thread::sleep(Duration::from_millis(300)); // shown and painted
        TestWindow { hwnd, thread: Some(thread) }
    }

    fn rect(&self) -> crate::capture::IRect {
        let mut r = RECT::default();
        unsafe { GetWindowRect(HWND(self.hwnd as *mut _), &mut r) }.unwrap();
        (r.left, r.top, (r.right - r.left) as u32, (r.bottom - r.top) as u32)
    }
}

impl Drop for TestWindow {
    fn drop(&mut self) {
        unsafe {
            let _ = PostMessageW(Some(HWND(self.hwnd as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn out_dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("rustshot-rec-test-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn spec(format: RecFormat, fps: u32, area: crate::capture::IRect, out: PathBuf) -> RecSpec {
    RecSpec { format, fps, quality: Quality::Medium, area, system_audio: false, mic: None, out }
}

/// Record `secs` with `enc` built on the spec's `.part` path.
fn record(
    spec: RecSpec,
    audio: Vec<Box<dyn AudioSource>>,
    secs: f64,
    enc: impl FnOnce(&Path, u32, u32) -> Box<dyn VideoEncoder>,
) -> (PathBuf, SourceKind) {
    let (src, kind) = open_source(spec.area).unwrap();
    let (w, h) = src.size();
    let enc = enc(&spec.part_path(), w, h);
    let s = Session::start(spec, src, audio, enc).unwrap();
    std::thread::sleep(Duration::from_secs_f64(secs));
    (s.stop().unwrap(), kind)
}

fn skip_without_mf() -> bool {
    if !mf::available() {
        eprintln!("SKIP: Media Foundation is not available on this system");
        return true;
    }
    false
}

/// Distinct luma values (in steps of 16) among `ys`.
fn distinct(ys: &[u8]) -> usize {
    let mut v: Vec<u8> = ys.iter().map(|y| y / 16).collect();
    v.sort_unstable();
    v.dedup();
    v.len()
}

#[test]
#[ignore = "records the real desktop"]
fn live_rec_mp4_of_a_changing_window() {
    crate::capture::enable_dpi_awareness();
    if skip_without_mf() {
        return;
    }
    let win = TestWindow::open(200, 200, 320, 240, 100);
    let dir = out_dir();
    let spec = spec(RecFormat::Mp4, 30, win.rect(), dir.join("window.mp4"));
    let (path, kind) = record(spec, vec![], 2.0, |p, w, h| {
        let e = MfEncoder::create(p, w, h, 30, Quality::Medium, false).unwrap();
        eprintln!("encoder: {:?} hardware={}", e.encoder_name(), e.hardware());
        Box::new(e)
    });
    drop(win);
    let p = mf::probe(&path).unwrap();
    eprintln!("source {kind:?}; {p:?}");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(p.size, (320, 240));
    assert!((1.8..=2.2).contains(&p.duration.as_secs_f64()), "duration {:?}", p.duration);
    assert!((54..=66).contains(&p.frames), "{} frames", p.frames);
    assert!(!p.has_audio);
    assert!(distinct(&p.center_luma) >= 4, "the colour changes show: {:?}", p.center_luma);
}

/// Image descriptors in a GIF (walks the block structure).
fn gif_frames(b: &[u8]) -> usize {
    let mut i = 13 + if b[10] & 0x80 != 0 { 3 * (2 << (b[10] & 7)) } else { 0 };
    let skip_blocks = |i: &mut usize| {
        while b[*i] != 0 {
            *i += b[*i] as usize + 1;
        }
        *i += 1;
    };
    let mut n = 0;
    loop {
        match b[i] {
            0x3B => return n,
            0x21 => {
                i += 2;
                skip_blocks(&mut i);
            }
            0x2C => {
                let packed = b[i + 9];
                i += 10;
                if packed & 0x80 != 0 {
                    i += 3 * (2 << (packed & 7));
                }
                i += 1; // LZW minimum code size
                skip_blocks(&mut i);
                n += 1;
            }
            x => panic!("bad GIF block 0x{x:02X}"),
        }
    }
}

#[test]
#[ignore = "records the real desktop"]
fn live_rec_gif_of_a_changing_window() {
    crate::capture::enable_dpi_awareness();
    let win = TestWindow::open(240, 240, 200, 150, 100);
    let dir = out_dir();
    let spec = spec(RecFormat::Gif, 15, win.rect(), dir.join("window.gif"));
    let (path, _) = record(spec, vec![], 2.0, |p, w, h| Box::new(GifEncoder::create(p, w, h, 15).unwrap()));
    drop(win);
    let b = std::fs::read(&path).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let n = gif_frames(&b);
    eprintln!("GIF: {} bytes, {n} frames", b.len());
    assert!(n >= 15, "{n} frames");
}

#[test]
#[ignore = "records the real desktop"]
fn live_rec_dxgi_failure_falls_back_to_gdi() {
    crate::capture::enable_dpi_awareness();
    let win = TestWindow::open(260, 260, 160, 120, 100);
    dxgi::FORCE_FAIL.store(true, Ordering::Relaxed);
    let r = open_source(win.rect());
    dxgi::FORCE_FAIL.store(false, Ordering::Relaxed);
    let (mut src, kind) = r.unwrap();
    assert_eq!(kind, SourceKind::Gdi);
    assert_eq!(src.size(), (160, 120));
    let f = src.next(Instant::now() + Duration::from_millis(100)).unwrap();
    assert_eq!((f.w, f.h, f.bgra.len()), (160, 120, 160 * 120 * 4));
    // The window's colour fills the frame centre.
    let c = &f.bgra[(60 * 160 + 80) * 4..(60 * 160 + 80) * 4 + 3];
    assert!(COLOURS.iter().any(|&(r, g, b)| c == [b, g, r]), "centre {c:?}");
    // And the real path works here too (unless the machine has no duplication).
    match open_source(win.rect()).unwrap().1 {
        SourceKind::Dxgi => {}
        SourceKind::Gdi => eprintln!("note: desktop duplication is unavailable on this machine"),
    }
}

/// A mono PCM16 WAV of a sine: `pre` seconds of silence, then `secs` of tone.
fn wav(pre: f64, secs: f64, amp: f64) -> Vec<u8> {
    let rate = 48_000u32;
    let n = ((pre + secs) * f64::from(rate)) as usize;
    let lead = (pre * f64::from(rate)) as usize;
    let mut data = Vec::with_capacity(n * 2);
    for i in 0..n {
        let v = if i < lead { 0.0 } else { (i as f64 * 440.0 * std::f64::consts::TAU / f64::from(rate)).sin() * amp };
        data.extend_from_slice(&((v * 32767.0) as i16).to_le_bytes());
    }
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes()); // PCM
    b.extend_from_slice(&1u16.to_le_bytes()); // mono
    b.extend_from_slice(&rate.to_le_bytes());
    b.extend_from_slice(&(rate * 2).to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&(data.len() as u32).to_le_bytes());
    b.extend_from_slice(&data);
    b
}

/// Play a WAV image asynchronously; the image must outlive the sound.
fn play(img: &[u8]) {
    unsafe {
        let _ = PlaySoundW(PCWSTR(img.as_ptr().cast()), None, SND_MEMORY | SND_ASYNC | SND_NODEFAULT);
    }
}

fn stop_sound() {
    unsafe {
        let _ = PlaySoundW(PCWSTR::null(), None, SND_ASYNC);
    }
}

#[test]
#[ignore = "plays sound through the default device"]
fn live_rec_wasapi_loopback_tone() {
    let mut lb = match WasapiLoopback::open() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("SKIP: no render device ({e:#})");
            return;
        }
    };
    eprintln!("loopback: {} Hz, {} channels", lb.rate(), lb.channels());
    assert!(lb.rate() >= 8000 && lb.channels() >= 1);
    let img = wav(0.0, 1.0, 0.1);
    let mut got = Vec::new();
    lb.read(&mut got);
    got.clear();
    let t0 = Instant::now();
    play(&img);
    let mut heard = None;
    let end = t0 + Duration::from_millis(900);
    while Instant::now() < end {
        let from = got.len();
        lb.read(&mut got);
        if heard.is_none() && got[from..].iter().any(|s| s.abs() > 0.05) {
            heard = Some(t0.elapsed());
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    stop_sound();
    let peak = got.iter().fold(0f32, |m, &s| m.max(s.abs()));
    eprintln!("loopback: {} samples, peak {peak:.4}; first loud packet read {heard:?} after PlaySound", got.len());
    assert!(got.len() > lb.rate() as usize / 4, "samples arrive while the tone plays");
    assert!(peak > 0.005, "the tone is heard (peak {peak})");
    // The default microphone opens and delivers (nothing is kept).
    match WasapiCapture::open(None) {
        Ok(mut m) => {
            std::thread::sleep(Duration::from_millis(200));
            let mut v = Vec::new();
            m.read(&mut v);
            eprintln!("mic: {} Hz, {} channels, {} samples in 200 ms", m.rate(), m.channels(), v.len());
        }
        Err(e) => eprintln!("no default microphone ({e:#})"),
    }
    // Microphones list without failing (names only reported).
    for (id, name) in wasapi::list_mics() {
        assert!(!id.is_empty());
        eprintln!("mic: {name}");
    }
}

/// Decode the first audio stream of `path` to PCM16 and return the time
/// of the first sample louder than `threshold`.
fn audio_onset(path: &Path, threshold: f32) -> Option<Duration> {
    com_init();
    let r = mf::open_reader(path).unwrap();
    let first_audio = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;
    unsafe {
        let t = mf::new_media_type().unwrap();
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).unwrap();
        t.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM).unwrap();
        t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16).unwrap();
        r.SetCurrentMediaType(first_audio, None, &t).unwrap();
        let cur = r.GetCurrentMediaType(first_audio).unwrap();
        let rate = cur.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND).unwrap();
        let ch = cur.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS).unwrap() as usize;
        loop {
            let (mut flags, mut ts, mut sample) = (0u32, 0i64, None);
            r.ReadSample(first_audio, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut sample)).unwrap();
            if let Some(s) = sample {
                let buf = s.ConvertToContiguousBuffer().unwrap();
                let (mut p, mut len) = (std::ptr::null_mut(), 0u32);
                buf.Lock(&mut p, None, Some(&mut len)).unwrap();
                let bytes = std::slice::from_raw_parts(p, len as usize);
                let hit = bytes.as_chunks::<2>().0.iter().position(|b| f32::from(i16::from_le_bytes(*b)).abs() / 32768.0 > threshold);
                buf.Unlock().unwrap();
                if let Some(k) = hit {
                    let frame = (k / ch) as u64;
                    return Some(Duration::from_nanos(ts as u64 * 100 + frame * 1_000_000_000 / u64::from(rate)));
                }
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                return None;
            }
        }
    }
}

/// MP4 with system sound: a tone starts after 2 s of silence; the AAC
/// track must hold it close to when it was played (loopback sends no
/// packets during the silence, so this checks gap placement).
#[test]
#[ignore = "records the real desktop and plays sound"]
fn live_rec_mp4_audio_after_silence() {
    crate::capture::enable_dpi_awareness();
    if skip_without_mf() {
        return;
    }
    let lb = match WasapiLoopback::open() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("SKIP: no render device ({e:#})");
            return;
        }
    };
    stop_sound();
    let win = TestWindow::open(280, 280, 160, 120, 100);
    let dir = out_dir();
    let mut spec = spec(RecFormat::Mp4, 30, win.rect(), dir.join("audio.mp4"));
    spec.system_audio = true;
    let img = wav(0.0, 0.6, 0.2);
    let (src, _) = open_source(spec.area).unwrap();
    let (w, h) = src.size();
    let enc = MfEncoder::create(&spec.part_path(), w, h, 30, Quality::Low, true).unwrap();
    let s = Session::start(spec, src, vec![Box::new(lb)], Box::new(enc)).unwrap();
    let started = Instant::now();
    std::thread::sleep(Duration::from_secs(2));
    let played = started.elapsed();
    play(&img);
    std::thread::sleep(Duration::from_millis(1500));
    let path = s.stop().unwrap();
    stop_sound();
    drop(win);
    let p = mf::probe(&path).unwrap();
    let onset = audio_onset(&path, 0.05);
    let _ = std::fs::remove_dir_all(&dir);
    eprintln!("probe {p:?}");
    assert!(p.has_audio);
    assert!((3.3..=3.8).contains(&p.duration.as_secs_f64()), "duration {:?}", p.duration);
    let onset = onset.expect("the tone is in the MP4");
    let off = onset.as_secs_f64() - played.as_secs_f64();
    eprintln!("tone played at {played:?} (session time), heard in the MP4 at {onset:?}: offset {:+.1} ms", off * 1000.0);
    // PlaySound's own start-up and the device period come on top of the
    // capture placement; anything inside 0..250 ms is in step.
    assert!((-0.02..=0.25).contains(&off), "onset offset {off:.3} s");
}

#[test]
#[ignore = "benchmark: prints capture and conversion timings"]
fn bench_rec_capture() {
    crate::capture::enable_dpi_awareness();
    let mons = crate::capture::monitors().unwrap();
    let (x0, y0) = (mons.iter().map(|m| m.x).min().unwrap(), mons.iter().map(|m| m.y).min().unwrap());
    let x1 = mons.iter().map(|m| m.x + m.w as i32).max().unwrap();
    let y1 = mons.iter().map(|m| m.y + m.h as i32).max().unwrap();
    let prim = mons.iter().find(|m| m.primary).unwrap_or(&mons[0]).clone();
    // A small window flickering every 10 ms keeps the duplication busy.
    let _flicker = TestWindow::open(prim.x + 40, prim.y + 40, 24, 24, 10);
    let areas = [
        ("1920x1080", (prim.x, prim.y, 1920.min(prim.w), 1080.min(prim.h))),
        ("desktop", (x0, y0, (x1 - x0) as u32, (y1 - y0) as u32)),
    ];
    for (name, area) in areas {
        let n = 60;
        let (ow, oh) = nv12::fit(area.2, area.3);
        let mut out = Vec::new();
        let mut scratch = Vec::new();
        match DxgiSource::open(area) {
            Ok(mut s) => {
                let mut conv = Duration::ZERO;
                for _ in 0..n {
                    let f = s.next(Instant::now() + Duration::from_millis(50)).unwrap();
                    let t = Instant::now();
                    nv12::convert_into(&f.bgra, f.w, f.h, ow, oh, &mut out, &mut scratch);
                    conv += t.elapsed();
                }
                eprintln!(
                    "{name} {}x{}: DXGI copy+cursor {:?}/frame, NV12 ({ow}x{oh}) {:?}/frame",
                    area.2,
                    area.3,
                    s.work_per_frame(),
                    conv / n
                );
            }
            Err(e) => eprintln!("{name}: DXGI unavailable: {e:#}"),
        }
        let mut g = GdiPollSource::open(area).unwrap();
        for _ in 0..20 {
            g.next(Instant::now());
        }
        eprintln!("{name}: GDI poll {:?}/frame", g.work_per_frame());
    }
    if mf::available() {
        let dir = out_dir();
        let (w, h) = (1920.min(prim.w), 1080.min(prim.h));
        let mut e = MfEncoder::create(&dir.join("bench.mp4"), w, h, 30, Quality::Medium, false).unwrap();
        eprintln!("H.264 encoder: {:?}, hardware: {}", e.encoder_name(), e.hardware());
        let mut s = DxgiSource::open((prim.x, prim.y, w, h)).map(|s| Box::new(s) as Box<dyn crate::rec::FrameSource>)
            .unwrap_or_else(|_| Box::new(GdiPollSource::open((prim.x, prim.y, w, h)).unwrap()));
        let t = Instant::now();
        for i in 0..60u64 {
            let mut f = s.next(Instant::now() + Duration::from_millis(50)).unwrap();
            f.ts = Duration::from_millis(i * 33);
            e.push_video(&f).unwrap();
        }
        eprintln!("capture+convert+encode 60 frames at {w}x{h}: {:?}/frame", t.elapsed() / 60);
        Box::new(e).finish().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
