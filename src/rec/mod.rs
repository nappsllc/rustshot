//! Screen recording pipeline, shared by every platform:
//!
//! ```text
//! FrameSource ──► FrameQueue (cap 4, drop-oldest) ──► VideoEncoder
//! AudioSource(s) ──► Mixer (48 kHz stereo f32) ─────► (same encoder)
//! ```
//!
//! A [`Session`] runs a capture thread (paces the [`FrameSource`], stamps
//! frames from one [`Clock`]) and an encode thread (drains the queue, pulls
//! mixed audio, feeds the [`VideoEncoder`]). The encoder writes
//! `<out>.part`; [`Session::stop`] renames it to the output on success.
//! Nothing is buffered without bound: at most [`FrameQueue::CAP`] frames
//! wait for the encoder, and encoders stream to disk.

pub mod gif;
pub mod mix;
#[cfg(windows)]
pub mod win;

pub use mix::Mixer;

use crate::capture::IRect;
use anyhow::{anyhow, Result};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// One captured frame: BGRA, top-down rows of `w * 4` bytes, stamped with
/// the recording [`Clock`] (paused time excluded).
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub w: u32,
    pub h: u32,
    pub bgra: Vec<u8>,
    pub ts: Duration,
}

/// The recording's monotonic clock: time since [`Clock::start`] minus the
/// time spent paused (paused time is cut, not recorded as frozen frames).
#[derive(Clone, Copy, Debug)]
pub struct Clock {
    started: Instant,
    paused_at: Option<Instant>,
    paused_total: Duration,
    /// Pauses so far: a reader that saw a different count knows a pause
    /// happened since, even if it was already resumed.
    pauses: u64,
    /// Clock time at the latest pause.
    last_pause: Duration,
    /// When the latest pause began and ended (`None` while it lasts), and
    /// the paused time before it: [`Clock::time_of`] maps instants around it.
    pause_span: Option<(Instant, Option<Instant>)>,
    paused_before: Duration,
}

impl Clock {
    pub fn start() -> Clock {
        Clock::start_at(Instant::now())
    }

    pub fn pause(&mut self) {
        self.pause_at(Instant::now());
    }

    pub fn resume(&mut self) {
        self.resume_at(Instant::now());
    }

    pub fn now(&self) -> Duration {
        self.now_at(Instant::now())
    }

    pub fn is_paused(&self) -> bool {
        self.paused_at.is_some()
    }

    /// How many times the clock was paused (the pause epoch).
    pub fn pauses(&self) -> u64 {
        self.pauses
    }

    /// Clock time at the latest pause (zero before any).
    pub fn last_pause(&self) -> Duration {
        self.last_pause
    }

    fn start_at(t: Instant) -> Clock {
        Clock {
            started: t,
            paused_at: None,
            paused_total: Duration::ZERO,
            pauses: 0,
            last_pause: Duration::ZERO,
            pause_span: None,
            paused_before: Duration::ZERO,
        }
    }

    fn pause_at(&mut self, t: Instant) {
        if self.paused_at.is_none() {
            self.last_pause = self.now_at(t);
            self.paused_at = Some(t);
            self.pauses += 1;
            self.pause_span = Some((t, None));
            self.paused_before = self.paused_total;
        }
    }

    fn resume_at(&mut self, t: Instant) {
        if let Some(p) = self.paused_at.take() {
            self.paused_total += t.saturating_duration_since(p);
            self.pause_span = Some((p, Some(t)));
        }
    }

    /// Clock time of the instant `t` in nanoseconds, negative before the
    /// start; `None` when `t` falls in a pause (the current or the latest
    /// one: what was captured then is not recorded). Used for capture
    /// timestamps, which come a little after the fact. Instants before the
    /// latest pause are mapped with the paused time up to it (one before an
    /// earlier pause, which nothing delivers that late, would map as if
    /// that pause had not happened).
    pub fn time_of(&self, t: Instant) -> Option<i128> {
        let since = if t >= self.started {
            (t - self.started).as_nanos() as i128
        } else {
            -((self.started - t).as_nanos() as i128)
        };
        let paused = match self.pause_span {
            None => Duration::ZERO,
            Some((p, _)) if t < p => self.paused_before,
            Some((_, Some(r))) if t >= r => self.paused_total,
            Some(_) => return None,
        };
        Some(since - paused.as_nanos() as i128)
    }

    fn now_at(&self, t: Instant) -> Duration {
        let t = self.paused_at.unwrap_or(t);
        t.saturating_duration_since(self.started).saturating_sub(self.paused_total)
    }
}

/// Produces frames of the recorded area.
pub trait FrameSource: Send {
    /// The frame for now, blocking at most until `deadline`; `None` when no
    /// frame could be had in time (the tick is skipped). A source whose
    /// screen did not change returns its last frame again.
    fn next(&mut self, deadline: Instant) -> Option<Frame>;
    /// Frame size in pixels.
    fn size(&self) -> (u32, u32);
}

/// Produces PCM samples as they arrive.
///
/// A source may have gaps: it can deliver nothing for a while (WASAPI
/// loopback sends no packets while nothing plays). The mixer treats a gap
/// as silence and places what comes after it so that it ends at the time
/// it was read (or at its own timestamp, see [`AudioSource::read_timed`]);
/// samples are assumed to arrive in step with the clock otherwise (see
/// [`mix`] for the timing).
pub trait AudioSource: Send {
    fn rate(&self) -> u32;
    fn channels(&self) -> u16;
    /// Append the samples available now (interleaved f32) to `out`, without
    /// blocking; returns how many samples were appended.
    fn read(&mut self, out: &mut Vec<f32>) -> usize;
    /// [`AudioSource::read`], plus the instant at which the first appended
    /// sample was captured, when the source knows it (e.g. from the
    /// device's capture timestamps). A source with timestamps must not hand
    /// out samples across a discontinuity in one read (what follows one
    /// comes in the next read, with its own timestamp). The mixer maps it
    /// with the recording [`Clock`] to place input that ends a gap, and to
    /// re-place input whose timestamp disagrees with where it would follow
    /// on (see [`mix`]); `None` (the default) makes it assume the input
    /// ends at the time it was read.
    fn read_timed(&mut self, out: &mut Vec<f32>) -> (usize, Option<Instant>) {
        (self.read(out), None)
    }
}

/// Writes the recording; created by the caller on [`RecSpec::part_path`].
pub trait VideoEncoder: Send {
    /// Frame timestamps normally increase, but may repeat (a frame in flight
    /// at a pause is stamped with the frozen time) or, in principle, go back.
    fn push_video(&mut self, f: &Frame) -> Result<()>;
    /// 48 kHz stereo interleaved samples starting at `ts`.
    fn push_audio(&mut self, pcm: &[f32], ts: Duration) -> Result<()>;
    /// Finalise the file. Contract: `Ok` means the file is playable. An
    /// encoder that hit a write error earlier must return `Err` here rather
    /// than leave a corrupt file that looks finished (the session then
    /// removes it).
    fn finish(self: Box<Self>) -> Result<()>;
    /// Whether this encoder takes audio (GIF does not).
    fn audio(&self) -> bool;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecFormat {
    Mp4,
    Gif,
}

impl RecFormat {
    /// The `rec_format` config value (MP4 unless "gif").
    pub fn from_config(s: &str) -> RecFormat {
        if s.trim().eq_ignore_ascii_case("gif") { RecFormat::Gif } else { RecFormat::Mp4 }
    }

    pub fn ext(self) -> &'static str {
        match self {
            RecFormat::Mp4 => "mp4",
            RecFormat::Gif => "gif",
        }
    }

    /// Highest frame rate recorded in this format.
    pub fn max_fps(self) -> u32 {
        match self {
            RecFormat::Mp4 => 60,
            RecFormat::Gif => 15,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quality {
    Low,
    Medium,
    High,
}

impl Quality {
    /// The `rec_quality` config value (Medium unless "low"/"high").
    pub fn from_config(s: &str) -> Quality {
        match s.trim().to_ascii_lowercase().as_str() {
            "low" => Quality::Low,
            "high" => Quality::High,
            _ => Quality::Medium,
        }
    }
}

/// What to record and where.
#[derive(Clone, Debug, PartialEq)]
pub struct RecSpec {
    pub format: RecFormat,
    pub fps: u32,
    pub quality: Quality,
    /// Recorded area in physical screen pixels.
    pub area: IRect,
    pub system_audio: bool,
    /// Microphone device id (`Some("")` = the default device); `None` = off.
    pub mic: Option<String>,
    /// Final output path; the encoder writes [`RecSpec::part_path`].
    pub out: PathBuf,
}

impl RecSpec {
    /// A spec from the config's recording defaults, saved where Ctrl+S
    /// would save a capture now (with `.mp4` / `.gif`).
    pub fn from_config(cfg: &crate::config::Config, area: IRect, now: crate::export::Tm) -> RecSpec {
        let format = RecFormat::from_config(&cfg.rec_format);
        let gif = format == RecFormat::Gif;
        RecSpec {
            format,
            fps: if gif { cfg.rec_gif_fps } else { cfg.rec_fps },
            quality: Quality::from_config(&cfg.rec_quality),
            area,
            system_audio: !gif && cfg.rec_system_audio,
            mic: (!gif && cfg.rec_mic).then(|| cfg.rec_mic_device.clone()),
            out: crate::export::auto_save_path_ext(cfg, now, format.ext()),
        }
    }

    /// The file being written while recording: `<out>.part`.
    pub fn part_path(&self) -> PathBuf {
        part_path(&self.out)
    }

    /// The frame rate actually captured: 1 ..= the format's maximum.
    pub fn capture_fps(&self) -> u32 {
        self.fps.clamp(1, self.format.max_fps())
    }
}

fn part_path(out: &Path) -> PathBuf {
    let mut s = out.as_os_str().to_os_string();
    s.push(".part");
    PathBuf::from(s)
}

/// Bounded hand-off between the capture and encode threads: holds at most
/// [`FrameQueue::CAP`] frames; pushing onto a full queue drops the oldest.
pub struct FrameQueue {
    inner: Mutex<QInner>,
    ready: Condvar,
}

struct QInner {
    q: VecDeque<Frame>,
    dropped: u64,
    closed: bool,
}

/// What [`FrameQueue::pop`] got.
#[derive(Debug, PartialEq)]
pub enum Pop {
    Frame(Frame),
    /// Nothing arrived within the timeout.
    Empty,
    /// Closed and drained: no frame will come.
    Closed,
}

impl Default for FrameQueue {
    fn default() -> Self {
        FrameQueue::new()
    }
}

impl FrameQueue {
    pub const CAP: usize = 4;

    pub fn new() -> FrameQueue {
        let q = VecDeque::with_capacity(Self::CAP);
        FrameQueue { inner: Mutex::new(QInner { q, dropped: 0, closed: false }), ready: Condvar::new() }
    }

    fn lock(&self) -> MutexGuard<'_, QInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Queue `f`, dropping the oldest frame when full; `false` (and `f`
    /// discarded) once the queue is closed.
    pub fn push(&self, f: Frame) -> bool {
        let mut g = self.lock();
        if g.closed {
            return false;
        }
        if g.q.len() >= Self::CAP {
            g.q.pop_front();
            g.dropped += 1;
        }
        g.q.push_back(f);
        drop(g);
        self.ready.notify_one();
        true
    }

    /// The oldest frame, waiting up to `timeout` for one.
    pub fn pop(&self, timeout: Duration) -> Pop {
        let end = Instant::now() + timeout;
        let mut g = self.lock();
        loop {
            if let Some(f) = g.q.pop_front() {
                return Pop::Frame(f);
            }
            if g.closed {
                return Pop::Closed;
            }
            let now = Instant::now();
            if now >= end {
                return Pop::Empty;
            }
            g = self.ready.wait_timeout(g, end - now).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    /// No more pushes; queued frames can still be popped.
    pub fn close(&self) {
        self.lock().closed = true;
        self.ready.notify_all();
    }

    pub fn is_closed(&self) -> bool {
        self.lock().closed
    }

    /// Frames dropped because the encoder fell behind.
    pub fn dropped(&self) -> u64 {
        self.lock().dropped
    }

    pub fn len(&self) -> usize {
        self.lock().q.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A snapshot of a running session.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stats {
    /// Frames handed to the encoder.
    pub frames: u64,
    /// Frames dropped because the encoder fell behind.
    pub dropped: u64,
    /// Recorded time (paused time excluded).
    pub elapsed: Duration,
    pub paused: bool,
    /// The encoder failed; the session stopped recording.
    pub failed: bool,
}

enum Ctl {
    Pause,
    Resume,
    Stop,
}

/// Audio is mixed this far behind the clock so sources that deliver in
/// bursts (every 10-20 ms) are not cut short; on pause and stop the mixer
/// waits up to this long for the input to catch up before the final pull.
const AUDIO_LAG: Duration = Duration::from_millis(100);
/// How often the encode thread wakes without frames (to pull audio).
const ENCODE_TICK: Duration = Duration::from_millis(10);

/// What the encode thread ends with: a push error (recording stopped
/// early) and the encoder's own `finish`.
type EncodeEnd = (Option<anyhow::Error>, Result<()>);

/// Where a session reads the time: [`Instant::now`] normally; tests inject
/// a stepped clock with [`Session::start_with`].
pub type TimeFn = Arc<dyn Fn() -> Instant + Send + Sync>;

/// A recording in progress. Dropping it without [`Session::stop`] stops
/// it too (keeping the file).
pub struct Session {
    clock: Arc<Mutex<Clock>>,
    time: TimeFn,
    ctl: mpsc::Sender<Ctl>,
    queue: Arc<FrameQueue>,
    frames: Arc<AtomicU64>,
    failed: Arc<AtomicBool>,
    capture: Option<JoinHandle<()>>,
    encode: Option<JoinHandle<EncodeEnd>>,
    out: PathBuf,
}

fn lock_clock(c: &Mutex<Clock>) -> MutexGuard<'_, Clock> {
    c.lock().unwrap_or_else(|e| e.into_inner())
}

impl Session {
    /// Start recording now: `source` is paced at the spec's frame rate,
    /// `audio` is mixed when the encoder takes audio. `encoder` must write
    /// [`RecSpec::part_path`]. When a thread cannot be started, nothing
    /// keeps running, `.part` is removed and the error is returned.
    pub fn start(
        spec: RecSpec,
        source: Box<dyn FrameSource>,
        audio: Vec<Box<dyn AudioSource>>,
        encoder: Box<dyn VideoEncoder>,
    ) -> Result<Session> {
        Session::start_with(spec, source, audio, encoder, Arc::new(Instant::now))
    }

    /// [`Session::start`] with the clock read from `time`.
    pub fn start_with(
        spec: RecSpec,
        source: Box<dyn FrameSource>,
        audio: Vec<Box<dyn AudioSource>>,
        encoder: Box<dyn VideoEncoder>,
        time: TimeFn,
    ) -> Result<Session> {
        let part = spec.part_path();
        let clock = Arc::new(Mutex::new(Clock::start_at(time())));
        let queue = Arc::new(FrameQueue::new());
        let frames = Arc::new(AtomicU64::new(0));
        let failed = Arc::new(AtomicBool::new(false));
        let (ctl, rx) = mpsc::channel();
        let interval = Duration::from_secs_f64(1.0 / spec.capture_fps() as f64);
        let capture = {
            let (clock, queue, time) = (clock.clone(), queue.clone(), time.clone());
            std::thread::Builder::new()
                .name("rec-capture".into())
                .spawn(move || capture_loop(source, interval, &clock, &time, &queue, &rx))
        };
        let capture = match capture {
            Ok(h) => h,
            Err(e) => {
                // The encoder holds `.part` open (Windows cannot remove an open file).
                drop(encoder);
                let _ = std::fs::remove_file(&part);
                return Err(anyhow::Error::new(e).context("start the capture thread"));
            }
        };
        let encode = {
            let (clock, queue, frames, failed, time) =
                (clock.clone(), queue.clone(), frames.clone(), failed.clone(), time.clone());
            std::thread::Builder::new()
                .name("rec-encode".into())
                .spawn(move || encode_loop(encoder, audio, &clock, &time, &queue, &frames, &failed))
        };
        let encode = match encode {
            Ok(h) => h,
            Err(e) => {
                // The failed spawn dropped its closure, and the encoder with
                // it. Capture already runs: stop it before giving up.
                let _ = ctl.send(Ctl::Stop);
                queue.close();
                let _ = capture.join();
                let _ = std::fs::remove_file(&part);
                return Err(anyhow::Error::new(e).context("start the encode thread"));
            }
        };
        Ok(Session {
            clock,
            time,
            ctl,
            queue,
            frames,
            failed,
            capture: Some(capture),
            encode: Some(encode),
            out: spec.out,
        })
    }

    /// Pause: the clock freezes and no more frames are captured. A frame
    /// already being captured when this is called is still delivered,
    /// stamped with the frozen time, so it shares its timestamp with the
    /// first frame after [`Session::resume`] (encoders accept duplicate
    /// timestamps). Audio is flushed up to the pause and what arrives while
    /// paused is discarded.
    pub fn pause(&self) {
        lock_clock(&self.clock).pause_at((self.time)());
        let _ = self.ctl.send(Ctl::Pause);
    }

    pub fn resume(&self) {
        lock_clock(&self.clock).resume_at((self.time)());
        let _ = self.ctl.send(Ctl::Resume);
    }

    pub fn stats(&self) -> Stats {
        let c = *lock_clock(&self.clock);
        Stats {
            frames: self.frames.load(Ordering::Relaxed),
            dropped: self.queue.dropped(),
            elapsed: c.now_at((self.time)()),
            paused: c.is_paused(),
            failed: self.failed.load(Ordering::Relaxed),
        }
    }

    /// Stop, finish the file and move it into place; returns the saved
    /// path (the output, or a numbered sibling when that name is taken).
    /// When the encoder failed mid-way but could still finish the file, it
    /// is kept and the error says where; otherwise `.part` is removed.
    ///
    /// Blocks until the encoder has drained the queue, flushed the audio
    /// (up to [`AUDIO_LAG`] more) and finished the file, which for a long
    /// MP4 can take a while: call it off the UI thread.
    pub fn stop(mut self) -> Result<PathBuf> {
        self.end()
    }

    fn end(&mut self) -> Result<PathBuf> {
        let _ = self.ctl.send(Ctl::Stop);
        if let Some(h) = self.capture.take() {
            let _ = h.join();
        }
        // The capture thread closes the queue; close it again in case it panicked.
        self.queue.close();
        let Some(h) = self.encode.take() else { return Err(anyhow!("recording already stopped")) };
        let part = part_path(&self.out);
        let (push_err, finished) = h.join().unwrap_or_else(|_| (None, Err(anyhow!("the encoder crashed"))));
        if let Err(e) = finished {
            let _ = std::fs::remove_file(&part);
            let e = e.context("finish the recording");
            return Err(match push_err {
                Some(p) => e.context(format!("{p:#}")),
                None => e,
            });
        }
        // The output directory exists: the encoder created `.part` in it.
        let dst = crate::export::unique_path(&self.out);
        if let Err(e) = std::fs::rename(&part, &dst) {
            return Err(anyhow::Error::new(e)
                .context(format!("move the recording to {}; it is kept at {}", dst.display(), part.display())));
        }
        match push_err {
            Some(e) => Err(e.context(format!("recording stopped early; saved {}", dst.display()))),
            None => Ok(dst),
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.encode.is_some() {
            let _ = self.end();
        }
    }
}

fn capture_loop(
    mut source: Box<dyn FrameSource>,
    interval: Duration,
    clock: &Mutex<Clock>,
    time: &TimeFn,
    queue: &FrameQueue,
    rx: &mpsc::Receiver<Ctl>,
) {
    // Pacing (`tick`, the deadline given to the source) is real time,
    // `Instant::now`; the session's `time` only stamps frames. The session
    // tests depend on this: their stepped source does not keep to the
    // deadlines (a frame comes when the test sends a step, and a step that
    // misses one tick is taken on a later one), and every frame is stamped
    // with the test's clock however long the real wait was.
    let mut paused = false;
    let mut tick = Instant::now();
    'run: while !queue.is_closed() {
        // Handle control messages; while paused, wait for one.
        loop {
            let msg = if paused {
                rx.recv().map_err(|_| RecvTimeoutError::Disconnected)
            } else {
                rx.recv_timeout(tick.saturating_duration_since(Instant::now()))
            };
            match msg {
                Ok(Ctl::Stop) | Err(RecvTimeoutError::Disconnected) => break 'run,
                Ok(Ctl::Pause) => paused = true,
                Ok(Ctl::Resume) => {
                    if paused {
                        paused = false;
                        tick = Instant::now();
                    }
                }
                Err(RecvTimeoutError::Timeout) => break,
            }
        }
        let deadline = tick + interval;
        if let Some(mut f) = source.next(deadline) {
            f.ts = lock_clock(clock).now_at(time());
            if !queue.push(f) {
                break;
            }
        }
        // Next tick; after a stall, resume from now instead of bursting.
        tick += interval;
        let now = Instant::now();
        if tick + interval < now {
            tick = now;
        }
    }
    queue.close();
}

fn encode_loop(
    mut enc: Box<dyn VideoEncoder>,
    audio: Vec<Box<dyn AudioSource>>,
    clock: &Mutex<Clock>,
    time: &TimeFn,
    queue: &FrameQueue,
    frames: &AtomicU64,
    failed: &AtomicBool,
) -> EncodeEnd {
    let mut mixer = (enc.audio() && !audio.is_empty()).then(|| Mixer::new(audio));
    let mut pauses = 0;
    let mut err = None;
    let r = (|| -> Result<()> {
        loop {
            let closed = match queue.pop(ENCODE_TICK) {
                Pop::Frame(f) => {
                    enc.push_video(&f)?;
                    frames.fetch_add(1, Ordering::Relaxed);
                    false
                }
                Pop::Empty => false,
                Pop::Closed => true,
            };
            if let Some(m) = mixer.as_mut() {
                let c = *lock_clock(clock);
                if c.pauses() != pauses {
                    // Paused since the last look (maybe resumed already):
                    // everything up to the pause, then drop what came
                    // after. The discard comes before the push, so input
                    // read after the push is never dropped.
                    pauses = c.pauses();
                    let (ts, pcm) = flush(m, c.last_pause(), &c);
                    m.discard();
                    push_pcm(enc.as_mut(), &pcm, ts)?;
                }
                let now = c.now_at(time());
                if closed {
                    let (ts, pcm) = flush(m, now, &c);
                    push_pcm(enc.as_mut(), &pcm, ts)?;
                } else if c.is_paused() {
                    m.discard(); // sound while paused is not recorded
                } else {
                    m.feed(now, &c);
                    let ts = m.position();
                    let pcm = m.pull(now.saturating_sub(AUDIO_LAG));
                    push_pcm(enc.as_mut(), &pcm, ts)?;
                }
            }
            if closed {
                return Ok(());
            }
        }
    })();
    if let Err(e) = r {
        failed.store(true, Ordering::Relaxed);
        queue.close(); // stops the capture thread
        err = Some(e);
    }
    (err, enc.finish())
}

fn push_pcm(enc: &mut dyn VideoEncoder, pcm: &[f32], ts: Duration) -> Result<()> {
    if pcm.is_empty() { Ok(()) } else { enc.push_audio(pcm, ts) }
}

/// The mixed audio up to `until` (a pause or the stop), as its start time
/// and samples: up to `until - lag` at once, then, waiting at most one lag
/// (real time) for the input to reach `until`, the rest, so the tail is not
/// cut off by sources that deliver late.
///
/// Only sources still delivering are waited for. One that delivered nothing
/// it had not already played by `until - lag` (it ran dry, e.g. a silent
/// loopback) is in a gap, and [`Mixer::covered`] skips it: there is no sign
/// it has anything more (what it does send during the wait is still placed
/// by its capture time and mixed). With only such sources there is no wait
/// at all.
fn flush(m: &mut Mixer, until: Duration, clock: &Clock) -> (Duration, Vec<f32>) {
    let ts = m.position();
    m.feed(until, clock);
    let mut pcm = m.pull(until.saturating_sub(AUDIO_LAG));
    let give_up = Instant::now() + AUDIO_LAG;
    while !m.covered(until) && Instant::now() < give_up {
        std::thread::sleep(Duration::from_millis(5));
        m.feed(until, clock);
    }
    pcm.extend(m.pull(until));
    (ts, pcm)
}

#[cfg(test)]
mod tests;
