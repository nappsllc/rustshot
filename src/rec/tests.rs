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

#[test]
fn clock_counts_pauses() {
    let t0 = Instant::now();
    let s = |ms: u64| t0 + Duration::from_millis(ms);
    let mut c = Clock::start_at(t0);
    assert_eq!((c.pauses(), c.last_pause()), (0, Duration::ZERO));
    c.pause_at(s(300));
    c.pause_at(s(400)); // already paused: not a new pause
    assert_eq!((c.pauses(), c.last_pause()), (1, Duration::from_millis(300)));
    c.resume_at(s(1000));
    // A reader that saw epoch 0 and looks only now (resumed) still sees the pause.
    assert!(!c.is_paused());
    assert_eq!((c.pauses(), c.last_pause()), (1, Duration::from_millis(300)));
    c.pause_at(s(1500));
    c.resume_at(s(1600));
    assert_eq!((c.pauses(), c.last_pause()), (2, Duration::from_millis(800)));
}

#[test]
fn clock_maps_capture_instants() {
    let t0 = Instant::now() + Duration::from_secs(10);
    let s = |ms: u64| t0 + Duration::from_millis(ms);
    let ms = |v: i128| Some(v * 1_000_000);
    let mut c = Clock::start_at(t0);
    assert_eq!(c.time_of(s(250)), ms(250));
    assert_eq!(c.time_of(t0 - Duration::from_millis(30)), ms(-30), "before the start: negative");
    c.pause_at(s(1000));
    assert_eq!(c.time_of(s(999)), ms(999));
    assert_eq!(c.time_of(s(1000)), None, "captured while paused");
    assert_eq!(c.time_of(s(5000)), None);
    c.resume_at(s(1500));
    assert_eq!(c.time_of(s(990)), ms(990), "before the pause: as it was");
    assert_eq!(c.time_of(s(1200)), None, "during the pause");
    assert_eq!(c.time_of(s(1500)), ms(1000));
    assert_eq!(c.time_of(s(2000)), ms(1500));
    // Same as `now_at` for instants after the latest resume.
    assert_eq!(c.time_of(s(2000)), Some(c.now_at(s(2000)).as_nanos() as i128));
    c.pause_at(s(3000));
    c.resume_at(s(3100));
    assert_eq!(c.time_of(s(2900)), ms(2400));
    assert_eq!(c.time_of(s(3050)), None);
    assert_eq!(c.time_of(s(3200)), ms(2600));
}

/// The session's time: `base` plus what the test set.
#[derive(Clone)]
struct TestTime {
    base: Instant,
    ns: Arc<AtomicU64>,
}

impl TestTime {
    fn new() -> TestTime {
        TestTime { base: Instant::now(), ns: Arc::default() }
    }

    fn set_ms(&self, ms: u64) {
        self.ns.store(ms * 1_000_000, Ordering::SeqCst);
    }

    fn time_fn(&self) -> TimeFn {
        let t = self.clone();
        Arc::new(move || t.base + Duration::from_nanos(t.ns.load(Ordering::SeqCst)))
    }
}

/// Waits (up to 5 s) for `cond`.
fn wait_until(what: &str, cond: impl Fn() -> bool) {
    let end = Instant::now() + Duration::from_secs(5);
    while !cond() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Hands out one 4×4 frame (numbered in its bytes) per step the test sends.
struct StepSource {
    steps: mpsc::Receiver<()>,
    made: Arc<AtomicUsize>,
}

impl FrameSource for StepSource {
    fn next(&mut self, deadline: Instant) -> Option<Frame> {
        self.steps.recv_timeout(deadline.saturating_duration_since(Instant::now())).ok()?;
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
    pcm: Vec<f32>,
}

impl Log {
    /// Stereo audio frames received.
    fn audio_frames(&self) -> usize {
        self.pcm.len() / 2
    }
}

/// Holds `push_video` until the test lets it go: `entered` is signalled,
/// then it waits on `release` (dropping the sender releases for good).
struct Gate {
    entered: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
}

#[derive(Default)]
struct Opts {
    enc_audio: bool,
    fail_push_at: Option<usize>,
    fail_finish: bool,
    gate: Option<Gate>,
}

/// Writes one byte per frame to the `.part` file; can fail on demand.
struct FakeEncoder {
    file: std::fs::File,
    log: Arc<Mutex<Log>>,
    opts: Opts,
}

impl VideoEncoder for FakeEncoder {
    fn push_video(&mut self, f: &Frame) -> Result<()> {
        use std::io::Write;
        if let Some(g) = &self.opts.gate {
            let _ = g.entered.send(());
            let _ = g.release.recv();
        }
        let mut log = self.log.lock().unwrap();
        if Some(log.frames.len()) == self.opts.fail_push_at {
            anyhow::bail!("disk full");
        }
        log.frames.push(f.ts);
        self.file.write_all(&[1])?;
        Ok(())
    }
    fn push_audio(&mut self, pcm: &[f32], ts: Duration) -> Result<()> {
        let mut log = self.log.lock().unwrap();
        log.audio.push((ts, pcm.len()));
        log.pcm.extend_from_slice(pcm);
        Ok(())
    }
    fn finish(self: Box<Self>) -> Result<()> {
        if self.opts.fail_finish {
            anyhow::bail!("cannot finalise");
        }
        Ok(())
    }
    fn audio(&self) -> bool {
        self.opts.enc_audio
    }
}

/// 48 kHz mono samples queued by the test.
struct FeedMic(Arc<Mutex<Vec<f32>>>);

impl AudioSource for FeedMic {
    fn rate(&self) -> u32 {
        48_000
    }
    fn channels(&self) -> u16 {
        1
    }
    fn read(&mut self, out: &mut Vec<f32>) -> usize {
        let mut q = self.0.lock().unwrap();
        let n = q.len();
        out.append(&mut q);
        n
    }
}

struct Run {
    dir: PathBuf,
    spec: RecSpec,
    made: Arc<AtomicUsize>,
    log: Arc<Mutex<Log>>,
    time: TestTime,
    steps: Option<mpsc::Sender<()>>,
}

fn setup(tag: &str, format: RecFormat, fps: u32) -> Run {
    let dir = std::env::temp_dir().join(format!("rustshot-rec-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    let spec = RecSpec {
        format,
        fps,
        quality: Quality::Medium,
        area: (0, 0, 4, 4),
        system_audio: false,
        mic: None,
        out: dir.join("sub").join(format!("clip.{}", format.ext())),
    };
    Run { dir, spec, made: Arc::default(), log: Arc::default(), time: TestTime::new(), steps: None }
}

impl Run {
    fn start(&mut self, audio: Vec<Box<dyn AudioSource>>, opts: Opts) -> Session {
        let file = std::fs::File::create(self.spec.part_path()).unwrap();
        let enc = FakeEncoder { file, log: self.log.clone(), opts };
        let (tx, steps) = mpsc::channel();
        self.steps = Some(tx);
        let src = StepSource { steps, made: self.made.clone() };
        Session::start_with(self.spec.clone(), Box::new(src), audio, Box::new(enc), self.time.time_fn()).unwrap()
    }

    /// Let the source produce one frame at `ms` (without waiting for it).
    fn step(&self, ms: u64) {
        self.time.set_ms(ms);
        self.steps.as_ref().unwrap().send(()).unwrap();
    }

    /// One frame at `ms`, waited for until the encoder has it.
    fn shoot(&self, ms: u64) {
        let n = self.log.lock().unwrap().frames.len();
        self.step(ms);
        wait_until("the frame", || self.log.lock().unwrap().frames.len() > n);
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

fn ms(v: &[u64]) -> Vec<Duration> {
    v.iter().map(|&m| Duration::from_millis(m)).collect()
}

/// Audio pushes are back to back: each starts where the last ended.
fn assert_contiguous(log: &Log) {
    let mut next = Duration::ZERO;
    for &(ts, n) in &log.audio {
        assert!(ts.abs_diff(next) < Duration::from_micros(50), "{ts:?} vs {next:?}");
        next = ts + Duration::from_secs_f64(n as f64 / 2.0 / 48_000.0);
    }
}

#[test]
fn session_records_stepped_frames_and_renames_the_part_file() {
    let mut run = setup("ok", RecFormat::Mp4, 50);
    let s = run.start(Vec::new(), Opts::default());
    assert_eq!(run.files(), ["clip.mp4.part"]);
    for i in 0..10 {
        run.shoot(i * 20);
    }
    let st = s.stats();
    assert_eq!((st.frames, st.dropped, st.elapsed, st.paused, st.failed), (10, 0, Duration::from_millis(180), false, false));
    run.time.set_ms(200);
    let t = Instant::now();
    let out = s.stop().unwrap();
    assert!(t.elapsed() < Duration::from_secs(1), "stops promptly");
    assert_eq!(out, run.spec.out);
    assert_eq!(run.files(), ["clip.mp4"]);
    assert_eq!(run.made.load(Ordering::SeqCst), 10);
    assert_eq!(run.log.lock().unwrap().frames, ms(&[0, 20, 40, 60, 80, 100, 120, 140, 160, 180]));
    assert_eq!(std::fs::metadata(&out).unwrap().len(), 10);
}

#[test]
fn session_pause_cuts_time_and_frames() {
    let mut run = setup("pause", RecFormat::Gif, 50);
    let s = run.start(Vec::new(), Opts::default());
    for t in [0, 20, 40] {
        run.shoot(t);
    }
    run.time.set_ms(50);
    s.pause();
    assert!(s.stats().paused);
    assert_eq!(s.stats().elapsed, Duration::from_millis(50));
    run.time.set_ms(1000);
    assert_eq!(s.stats().elapsed, Duration::from_millis(50), "clock frozen");
    s.resume();
    assert!(!s.stats().paused);
    run.shoot(1020);
    run.shoot(1040);
    assert_eq!(s.stats().elapsed, Duration::from_millis(90), "paused time is not recorded");
    s.stop().unwrap();
    assert_eq!(run.log.lock().unwrap().frames, ms(&[0, 20, 40, 70, 90]));
}

#[test]
fn session_mixes_audio_continuously() {
    let mut run = setup("audio", RecFormat::Mp4, 30);
    let q = Arc::new(Mutex::new(Vec::new()));
    let s = run.start(vec![Box::new(FeedMic(q.clone()))], Opts { enc_audio: true, ..Opts::default() });
    // 100 ms of input read at each 100 ms of clock, from 0 (each read is
    // waited for, so the first plays from 0, where it was read).
    for k in 0..=5 {
        run.time.set_ms(k * 100);
        if k < 5 {
            q.lock().unwrap().extend(std::iter::repeat_n(0.1f32, 4800));
            wait_until("the input read", || q.lock().unwrap().is_empty());
        }
        // Mixed up to 100 ms behind the clock.
        let want = (k as usize).saturating_sub(1) * 4800;
        wait_until("audio behind the clock", || run.log.lock().unwrap().audio_frames() == want);
    }
    s.stop().unwrap();
    let log = run.log.lock().unwrap();
    assert_contiguous(&log);
    assert_eq!(log.audio_frames(), 24_000, "exactly the recorded 0.5 s, tail flushed");
    assert!(log.pcm.iter().all(|&v| v == 0.1), "no gaps or silence");
    drop(log);
    // An encoder without audio (GIF) never gets any.
    let mut run = setup("noaudio", RecFormat::Gif, 15);
    let q = Arc::new(Mutex::new(vec![0.1; 4800]));
    let s = run.start(vec![Box::new(FeedMic(q))], Opts::default());
    run.shoot(100);
    s.stop().unwrap();
    assert!(run.log.lock().unwrap().audio.is_empty());
}

#[test]
fn session_pause_flushes_audio_and_skips_what_came_while_paused() {
    let mut run = setup("pause-audio", RecFormat::Mp4, 30);
    let q = Arc::new(Mutex::new(Vec::new()));
    let (entered, entered_rx) = mpsc::channel();
    let (release_tx, release) = mpsc::channel::<()>();
    let opts = Opts { enc_audio: true, gate: Some(Gate { entered, release }), ..Opts::default() };
    let s = run.start(vec![Box::new(FeedMic(q.clone()))], opts);
    let frames = |log: &Arc<Mutex<Log>>| log.lock().unwrap().audio_frames();
    q.lock().unwrap().extend(std::iter::repeat_n(0.1f32, 3 * 4800));
    wait_until("the input read at 0", || q.lock().unwrap().is_empty());
    run.time.set_ms(300);
    wait_until("audio to 200 ms", || frames(&run.log) == 9600);
    // The encode thread takes a frame and is held inside push_video, while
    // the recording is paused and resumed: one encode iteration.
    run.step(300);
    entered_rx.recv().unwrap();
    s.pause();
    q.lock().unwrap().extend(std::iter::repeat_n(0.9f32, 4800)); // sound while paused
    run.time.set_ms(2000);
    s.resume();
    drop(release_tx);
    // The pause is seen: audio up to it, then what came while paused is
    // dropped (before that audio is pushed, so nothing queued below is).
    wait_until("audio to the pause", || frames(&run.log) == 14_400);
    // Clock 450 ms: the output reaches 350 ms (silence, the mic has nothing
    // since the resume). Every read from then on is at 450 ms.
    run.time.set_ms(2150);
    wait_until("audio to 350 ms", || frames(&run.log) == 16_800);
    // 100 ms read at 450 ms: captured since 350 ms, and placed there.
    q.lock().unwrap().extend(std::iter::repeat_n(0.2f32, 4800));
    wait_until("the input read after the resume", || q.lock().unwrap().is_empty());
    s.stop().unwrap();
    let log = run.log.lock().unwrap();
    assert_eq!(log.frames, ms(&[300]));
    assert_contiguous(&log);
    assert_eq!(log.audio_frames(), 21_600, "0.3 s before the pause, 0.15 s after");
    let (before, after) = log.pcm.split_at(28_800);
    assert!(before.iter().all(|&v| v == 0.1), "up to the pause");
    let (silent, after) = after.split_at(4800);
    assert!(silent.iter().all(|&v| v == 0.0), "nothing from the pause");
    assert!(after.iter().all(|&v| v == 0.2), "after the resume, at its capture time");
}

#[test]
fn session_failure_to_finish_removes_the_part_file() {
    let mut run = setup("fail", RecFormat::Gif, 30);
    let s = run.start(Vec::new(), Opts { fail_finish: true, ..Opts::default() });
    run.shoot(0);
    run.shoot(33);
    let e = s.stop().unwrap_err();
    assert!(format!("{e:#}").contains("cannot finalise"), "{e:#}");
    assert!(run.files().is_empty(), "{:?}", run.files());
}

#[test]
fn session_push_error_stops_early_but_keeps_a_finished_file() {
    let mut run = setup("push", RecFormat::Mp4, 50);
    let s = run.start(Vec::new(), Opts { fail_push_at: Some(3), ..Opts::default() });
    for t in [0, 20, 40] {
        run.shoot(t);
    }
    assert!(!s.stats().failed);
    run.step(60);
    wait_until("the failure", || s.stats().failed);
    let e = format!("{:#}", s.stop().unwrap_err());
    assert!(e.contains("disk full") && e.contains("clip.mp4"), "{e}");
    assert_eq!(run.files(), ["clip.mp4"]);
    assert_eq!(run.log.lock().unwrap().frames, ms(&[0, 20, 40]));
    assert_eq!(run.made.load(Ordering::SeqCst), 4);
}

#[test]
fn session_output_name_taken_gets_a_numbered_sibling() {
    let mut run = setup("taken", RecFormat::Gif, 30);
    std::fs::write(&run.spec.out, b"old").unwrap();
    let s = run.start(Vec::new(), Opts::default());
    run.shoot(0);
    let out = s.stop().unwrap();
    assert_ne!(out, run.spec.out);
    assert_eq!(std::fs::read(&run.spec.out).unwrap(), b"old", "never overwritten");
    assert_eq!(run.files().len(), 2);
    // Dropping a session stops it and keeps the file.
    let mut run = setup("drop", RecFormat::Gif, 30);
    let s = run.start(Vec::new(), Opts::default());
    run.shoot(0);
    drop(s);
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
