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

pub use mix::Mixer;

use crate::capture::IRect;
use anyhow::{anyhow, Context, Result};
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

    fn start_at(t: Instant) -> Clock {
        Clock { started: t, paused_at: None, paused_total: Duration::ZERO }
    }

    fn pause_at(&mut self, t: Instant) {
        self.paused_at.get_or_insert(t);
    }

    fn resume_at(&mut self, t: Instant) {
        if let Some(p) = self.paused_at.take() {
            self.paused_total += t.saturating_duration_since(p);
        }
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
pub trait AudioSource: Send {
    fn rate(&self) -> u32;
    fn channels(&self) -> u16;
    /// Append the samples available now (interleaved f32) to `out`, without
    /// blocking; returns how many samples were appended.
    fn read(&mut self, out: &mut Vec<f32>) -> usize;
}

/// Writes the recording; created by the caller on [`RecSpec::part_path`].
pub trait VideoEncoder: Send {
    fn push_video(&mut self, f: &Frame) -> Result<()>;
    /// 48 kHz stereo interleaved samples starting at `ts`.
    fn push_audio(&mut self, pcm: &[f32], ts: Duration) -> Result<()>;
    /// Finalise the file (it is playable after an `Ok`).
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
/// bursts (every 10-20 ms) are not cut short; the rest is flushed on stop.
const AUDIO_LAG: Duration = Duration::from_millis(100);
/// How often the encode thread wakes without frames (to pull audio).
const ENCODE_TICK: Duration = Duration::from_millis(10);

/// What the encode thread ends with: a push error (recording stopped
/// early) and the encoder's own `finish`.
type EncodeEnd = (Option<anyhow::Error>, Result<()>);

/// A recording in progress. Dropping it without [`Session::stop`] stops
/// it too (keeping the file).
pub struct Session {
    clock: Arc<Mutex<Clock>>,
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
    /// [`RecSpec::part_path`].
    pub fn start(
        spec: RecSpec,
        source: Box<dyn FrameSource>,
        audio: Vec<Box<dyn AudioSource>>,
        encoder: Box<dyn VideoEncoder>,
    ) -> Session {
        let clock = Arc::new(Mutex::new(Clock::start()));
        let queue = Arc::new(FrameQueue::new());
        let frames = Arc::new(AtomicU64::new(0));
        let failed = Arc::new(AtomicBool::new(false));
        let (ctl, rx) = mpsc::channel();
        let interval = Duration::from_secs_f64(1.0 / spec.capture_fps() as f64);
        let capture = {
            let (clock, queue) = (clock.clone(), queue.clone());
            std::thread::Builder::new()
                .name("rec-capture".into())
                .spawn(move || capture_loop(source, interval, &clock, &queue, &rx))
                .expect("spawn capture thread")
        };
        let encode = {
            let (clock, queue, frames, failed) = (clock.clone(), queue.clone(), frames.clone(), failed.clone());
            std::thread::Builder::new()
                .name("rec-encode".into())
                .spawn(move || encode_loop(encoder, audio, &clock, &queue, &frames, &failed))
                .expect("spawn encode thread")
        };
        Session { clock, ctl, queue, frames, failed, capture: Some(capture), encode: Some(encode), out: spec.out }
    }

    pub fn pause(&self) {
        lock_clock(&self.clock).pause();
        let _ = self.ctl.send(Ctl::Pause);
    }

    pub fn resume(&self) {
        lock_clock(&self.clock).resume();
        let _ = self.ctl.send(Ctl::Resume);
    }

    pub fn stats(&self) -> Stats {
        let c = *lock_clock(&self.clock);
        Stats {
            frames: self.frames.load(Ordering::Relaxed),
            dropped: self.queue.dropped(),
            elapsed: c.now(),
            paused: c.is_paused(),
            failed: self.failed.load(Ordering::Relaxed),
        }
    }

    /// Stop, finish the file and move it into place; returns the saved
    /// path (the output, or a numbered sibling when that name is taken).
    /// When the encoder failed mid-way but could still finish the file, it
    /// is kept and the error says where; otherwise `.part` is removed.
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
        if let Some(dir) = self.out.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        }
        let dst = crate::export::unique_path(&self.out);
        std::fs::rename(&part, &dst)
            .with_context(|| format!("move {} to {}", part.display(), dst.display()))?;
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
    queue: &FrameQueue,
    rx: &mpsc::Receiver<Ctl>,
) {
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
            f.ts = lock_clock(clock).now();
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
    queue: &FrameQueue,
    frames: &AtomicU64,
    failed: &AtomicBool,
) -> EncodeEnd {
    let mut mixer = (enc.audio() && !audio.is_empty()).then(|| Mixer::new(audio));
    let mut was_paused = false;
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
                let now = c.now();
                if closed || (c.is_paused() && !was_paused) {
                    // Stop or pause: everything up to now.
                    push_mixed(enc.as_mut(), m, now)?;
                } else if c.is_paused() {
                    m.discard(); // sound while paused is not recorded
                } else {
                    push_mixed(enc.as_mut(), m, now.saturating_sub(AUDIO_LAG))?;
                }
                was_paused = c.is_paused();
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

fn push_mixed(enc: &mut dyn VideoEncoder, m: &mut Mixer, until: Duration) -> Result<()> {
    let ts = m.position();
    let pcm = m.pull(until);
    if pcm.is_empty() { Ok(()) } else { enc.push_audio(&pcm, ts) }
}

#[cfg(test)]
mod tests;
