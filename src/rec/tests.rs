use super::*;
use std::sync::atomic::AtomicUsize;

fn f(ms: u64) -> Frame {
    Frame { w: 1, h: 1, bgra: vec![0, 0, 0, 255], ts: Duration::from_millis(ms) }
}

#[test]
fn clock_excludes_paused_time() {
    let t0 = Instant::now();
    let s = |ms: u64| t0 + Duration::from_millis(ms);
    let mut c = Clock::start_at(t0);
    assert_eq!(c.now_at(s(1000)), Duration::from_millis(1000));
    c.pause_at(s(1000));
    assert!(c.is_paused());
    assert_eq!(c.now_at(s(2500)), Duration::from_millis(1000), "frozen while paused");
    c.pause_at(s(2000)); // a second pause changes nothing
    c.resume_at(s(3000));
    assert!(!c.is_paused());
    assert_eq!(c.now_at(s(3000)), Duration::from_millis(1000));
    assert_eq!(c.now_at(s(4000)), Duration::from_millis(2000));
    c.resume_at(s(5000)); // resume without pause changes nothing
    assert_eq!(c.now_at(s(5000)), Duration::from_millis(3000));
    c.pause_at(s(6000));
    c.resume_at(s(6500));
    assert_eq!(c.now_at(s(7000)), Duration::from_millis(4500));
    // Never negative, even for an instant before the start.
    assert_eq!(Clock::start_at(s(10)).now_at(t0), Duration::ZERO);
}

#[test]
fn queue_drops_oldest_when_full() {
    let q = FrameQueue::new();
    for i in 0..10 {
        assert!(q.push(f(i)));
        assert!(q.len() <= FrameQueue::CAP);
    }
    assert_eq!((q.len(), q.dropped()), (4, 6));
    for want in 6..10 {
        assert_eq!(q.pop(Duration::ZERO), Pop::Frame(f(want)));
    }
    assert_eq!(q.pop(Duration::from_millis(5)), Pop::Empty);
    assert!(q.push(f(42)));
    q.close();
    assert!(!q.push(f(43)), "closed queues refuse frames");
    assert_eq!(q.pop(Duration::ZERO), Pop::Frame(f(42)), "queued frames survive close");
    let t = Instant::now();
    assert_eq!(q.pop(Duration::from_secs(5)), Pop::Closed);
    assert!(t.elapsed() < Duration::from_secs(1), "closed is reported at once");
}

#[test]
fn queue_with_a_slow_consumer_stays_bounded() {
    let q = Arc::new(FrameQueue::new());
    let consumer = {
        let q = q.clone();
        std::thread::spawn(move || {
            let mut got = Vec::new();
            loop {
                match q.pop(Duration::from_millis(50)) {
                    Pop::Frame(fr) => got.push(fr.ts),
                    Pop::Empty => {}
                    Pop::Closed => return got,
                }
                std::thread::sleep(Duration::from_millis(3));
            }
        })
    };
    for i in 0..200 {
        q.push(f(i));
        assert!(q.len() <= FrameQueue::CAP);
        if i % 20 == 0 {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    q.close();
    let got = consumer.join().unwrap();
    assert_eq!(got.len() as u64 + q.dropped(), 200, "every frame is either encoded or counted as dropped");
    assert!(q.dropped() > 0, "the consumer was slower");
    assert!(got.windows(2).all(|w| w[0] < w[1]), "order kept");
    assert_eq!(got.last(), Some(&Duration::from_millis(199)), "the newest frame survives");
}

/// Hands out 4×4 frames, numbered in the blue channel.
struct FakeSource {
    made: Arc<AtomicUsize>,
}

impl FrameSource for FakeSource {
    fn next(&mut self, _deadline: Instant) -> Option<Frame> {
        let n = self.made.fetch_add(1, Ordering::SeqCst);
        Some(Frame { w: 4, h: 4, bgra: vec![n as u8; 64], ts: Duration::MAX })
    }
    fn size(&self) -> (u32, u32) {
        (4, 4)
    }
}

#[derive(Default)]
struct Log {
    frames: Vec<Duration>,
    audio: Vec<(Duration, usize)>,
}

/// Writes one byte per frame to the `.part` file; can fail on demand.
struct FakeEncoder {
    file: std::fs::File,
    log: Arc<Mutex<Log>>,
    audio: bool,
    fail_push_at: Option<usize>,
    fail_finish: bool,
}

impl VideoEncoder for FakeEncoder {
    fn push_video(&mut self, f: &Frame) -> Result<()> {
        use std::io::Write;
        let mut log = self.log.lock().unwrap();
        if Some(log.frames.len()) == self.fail_push_at {
            anyhow::bail!("disk full");
        }
        log.frames.push(f.ts);
        self.file.write_all(&[1])?;
        Ok(())
    }
    fn push_audio(&mut self, pcm: &[f32], ts: Duration) -> Result<()> {
        self.log.lock().unwrap().audio.push((ts, pcm.len()));
        Ok(())
    }
    fn finish(self: Box<Self>) -> Result<()> {
        if self.fail_finish {
            anyhow::bail!("cannot finalise");
        }
        Ok(())
    }
    fn audio(&self) -> bool {
        self.audio
    }
}

/// A steady 48 kHz mono tone, delivered as wall time passes.
struct FakeMic {
    started: Instant,
    sent: usize,
}

impl AudioSource for FakeMic {
    fn rate(&self) -> u32 {
        48_000
    }
    fn channels(&self) -> u16 {
        1
    }
    fn read(&mut self, out: &mut Vec<f32>) -> usize {
        let due = (self.started.elapsed().as_secs_f64() * 48_000.0) as usize;
        let n = due.saturating_sub(self.sent);
        out.extend(std::iter::repeat_n(0.1, n));
        self.sent = due;
        n
    }
}

struct Run {
    dir: PathBuf,
    spec: RecSpec,
    made: Arc<AtomicUsize>,
    log: Arc<Mutex<Log>>,
}

fn setup(tag: &str, format: RecFormat, fps: u32) -> Run {
    let dir = std::env::temp_dir().join(format!("rustshot-rec-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let spec = RecSpec {
        format,
        fps,
        quality: Quality::Medium,
        area: (0, 0, 4, 4),
        system_audio: false,
        mic: None,
        out: dir.join("sub").join(format!("clip.{}", format.ext())),
    };
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    Run { dir, spec, made: Arc::default(), log: Arc::default() }
}

impl Run {
    fn start(&self, audio: Vec<Box<dyn AudioSource>>, enc_audio: bool, fail_push_at: Option<usize>, fail_finish: bool) -> Session {
        let file = std::fs::File::create(self.spec.part_path()).unwrap();
        let enc = FakeEncoder { file, log: self.log.clone(), audio: enc_audio, fail_push_at, fail_finish };
        let src = FakeSource { made: self.made.clone() };
        Session::start(self.spec.clone(), Box::new(src), audio, Box::new(enc))
    }

    fn files(&self) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(self.dir.join("sub"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn session_records_paced_frames_and_renames_the_part_file() {
    let run = setup("ok", RecFormat::Mp4, 50);
    let s = run.start(Vec::new(), false, None, false);
    assert_eq!(run.files(), ["clip.mp4.part"]);
    std::thread::sleep(Duration::from_millis(300));
    let st = s.stats();
    assert!(!st.paused && !st.failed);
    assert!(st.elapsed >= Duration::from_millis(300));
    let t = Instant::now();
    let out = s.stop().unwrap();
    assert!(t.elapsed() < Duration::from_millis(500), "stops promptly");
    assert_eq!(out, run.spec.out);
    assert_eq!(run.files(), ["clip.mp4"]);
    let log = run.log.lock().unwrap();
    let made = run.made.load(Ordering::SeqCst);
    // 50 fps for ~0.3 s: about 15 frames (loose bounds for a busy machine).
    assert!((5..=25).contains(&made), "{made} frames captured");
    assert_eq!(log.frames.len(), made, "a fast encoder drops nothing");
    assert_eq!(std::fs::metadata(&out).unwrap().len(), made as u64);
    // Stamped from the session clock, increasing, starting near zero.
    assert!(log.frames[0] < Duration::from_millis(100), "{:?}", log.frames[0]);
    assert!(log.frames.windows(2).all(|w| w[0] < w[1]));
}

#[test]
fn session_pause_cuts_time_and_frames() {
    let run = setup("pause", RecFormat::Gif, 50);
    let s = run.start(Vec::new(), false, None, false);
    std::thread::sleep(Duration::from_millis(150));
    s.pause();
    std::thread::sleep(Duration::from_millis(50)); // let an in-flight frame land
    let (made, at) = (run.made.load(Ordering::SeqCst), s.stats().elapsed);
    assert!(s.stats().paused);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(run.made.load(Ordering::SeqCst), made, "no frames while paused");
    assert_eq!(s.stats().elapsed, at, "clock frozen");
    s.resume();
    std::thread::sleep(Duration::from_millis(150));
    let total = s.stats().elapsed;
    assert!(total < Duration::from_millis(450), "paused time is not recorded: {total:?}");
    s.stop().unwrap();
    let log = run.log.lock().unwrap();
    assert!(log.frames.len() > made, "frames after resume");
    // No gap of the pause length between consecutive frame stamps.
    let gap = log.frames.windows(2).map(|w| w[1] - w[0]).max().unwrap();
    assert!(gap < Duration::from_millis(200), "largest gap {gap:?}");
}

#[test]
fn session_mixes_audio_continuously() {
    let run = setup("audio", RecFormat::Mp4, 30);
    let mic: Box<dyn AudioSource> = Box::new(FakeMic { started: Instant::now(), sent: 0 });
    let s = run.start(vec![mic], true, None, false);
    std::thread::sleep(Duration::from_millis(400));
    s.stop().unwrap();
    let log = run.log.lock().unwrap();
    let total: usize = log.audio.iter().map(|a| a.1).sum();
    // Pushes are back to back: each starts where the last ended.
    let mut next = Duration::ZERO;
    for &(ts, n) in &log.audio {
        assert!(ts.abs_diff(next) < Duration::from_micros(50), "{ts:?} vs {next:?}");
        next = ts + Duration::from_secs_f64(n as f64 / 2.0 / 48_000.0);
    }
    let secs = total as f64 / 2.0 / 48_000.0;
    assert!((0.38..0.8).contains(&secs), "{secs} s of audio");
    // An encoder without audio (GIF) never gets any.
    let run = setup("noaudio", RecFormat::Gif, 15);
    let mic: Box<dyn AudioSource> = Box::new(FakeMic { started: Instant::now(), sent: 0 });
    let s = run.start(vec![mic], false, None, false);
    std::thread::sleep(Duration::from_millis(100));
    s.stop().unwrap();
    assert!(run.log.lock().unwrap().audio.is_empty());
}

#[test]
fn session_failure_to_finish_removes_the_part_file() {
    let run = setup("fail", RecFormat::Gif, 30);
    let s = run.start(Vec::new(), false, None, true);
    std::thread::sleep(Duration::from_millis(80));
    let e = s.stop().unwrap_err();
    assert!(format!("{e:#}").contains("cannot finalise"), "{e:#}");
    assert!(run.files().is_empty(), "{:?}", run.files());
}

#[test]
fn session_push_error_stops_early_but_keeps_a_finished_file() {
    let run = setup("push", RecFormat::Mp4, 50);
    let s = run.start(Vec::new(), false, Some(3), false);
    std::thread::sleep(Duration::from_millis(250));
    assert!(s.stats().failed);
    let made = run.made.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(100));
    assert!(run.made.load(Ordering::SeqCst) <= made + 1, "capture stopped after the failure");
    let e = format!("{:#}", s.stop().unwrap_err());
    assert!(e.contains("disk full") && e.contains("clip.mp4"), "{e}");
    assert_eq!(run.files(), ["clip.mp4"]);
    assert_eq!(run.log.lock().unwrap().frames.len(), 3);
}

#[test]
fn session_output_name_taken_gets_a_numbered_sibling() {
    let run = setup("taken", RecFormat::Gif, 30);
    std::fs::write(&run.spec.out, b"old").unwrap();
    let s = run.start(Vec::new(), false, None, false);
    std::thread::sleep(Duration::from_millis(50));
    let out = s.stop().unwrap();
    assert_ne!(out, run.spec.out);
    assert_eq!(std::fs::read(&run.spec.out).unwrap(), b"old", "never overwritten");
    assert_eq!(run.files().len(), 2);
    // Dropping a session stops it and keeps the file.
    let run = setup("drop", RecFormat::Gif, 30);
    drop(run.start(Vec::new(), false, None, false));
    assert_eq!(run.files(), ["clip.gif"]);
}

#[test]
fn spec_from_config() {
    let cfg = crate::config::Config { save_path: "rec-out".into(), ..Default::default() };
    let now = (2026, 10, 10, 9, 5, 0);
    let s = RecSpec::from_config(&cfg, (1, 2, 30, 40), now);
    assert_eq!((s.format, s.fps, s.quality), (RecFormat::Mp4, 30, Quality::Medium));
    assert!(s.system_audio && s.mic.is_none());
    assert_eq!(s.out, crate::export::auto_save_path_ext(&cfg, now, "mp4"));
    assert_eq!(s.out.extension().unwrap(), "mp4");
    assert_eq!(s.part_path().file_name().unwrap().to_string_lossy(), "2026-10-10_09-05.mp4.part");
    let gif = crate::config::Config { rec_format: "gif".into(), rec_mic: true, ..cfg.clone() };
    let s = RecSpec::from_config(&gif, (0, 0, 8, 8), now);
    assert_eq!((s.format, s.fps), (RecFormat::Gif, 15));
    assert!(!s.system_audio && s.mic.is_none(), "GIF has no sound");
    assert_eq!(s.out.extension().unwrap(), "gif");
    let mic = crate::config::Config { rec_mic: true, rec_mic_device: "usb".into(), ..cfg };
    assert_eq!(RecSpec::from_config(&mic, (0, 0, 8, 8), now).mic.as_deref(), Some("usb"));
    // Capture rates are capped per format.
    assert_eq!(RecSpec { fps: 60, ..s.clone() }.capture_fps(), 15);
    assert_eq!(RecSpec { fps: 0, ..s.clone() }.capture_fps(), 1);
    assert_eq!(RecSpec { fps: 60, format: RecFormat::Mp4, ..s }.capture_fps(), 60);
    assert_eq!(Quality::from_config("HIGH"), Quality::High);
    assert_eq!(RecFormat::from_config(" Gif "), RecFormat::Gif);
}
