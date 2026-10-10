//! Audio mixer: every source resampled (linear) to 48 kHz stereo, scaled
//! by its gain, summed and clamped to [-1, 1].
//!
//! Timing. Each source's pending input is a run of samples placed on the
//! output timeline: `pos` is where the output position falls inside it, in
//! input frames, and a negative `pos` is lead silence before its first
//! sample. While a source delivers continuously, its samples follow on one
//! another, so jitter shorter than the time buffered ahead (the session's
//! audio lag) does nothing.
//!
//! When it runs dry (everything it gave was played) it is in a *gap*: it
//! adds silence, and the first input read after the gap is placed by when
//! it was captured. A source that knows that passes the capture instant
//! from [`AudioSource::read_timed`], mapped to clock time with
//! [`Clock::time_of`] (input captured during a pause is dropped);
//! otherwise the input is assumed to *end* at the clock time it was read
//! ([`Mixer::feed`]'s `now`), i.e. to start one input-length before it.
//! That one rule covers both ways a gap ends:
//!
//! - after a true gap (WASAPI loopback sends nothing while nothing plays),
//!   real-time packets start one packet before their read, about when they
//!   were captured; the error is at most the time between capture and read
//!   (one packet plus one encode tick), and early rather than late;
//! - after a delivery stall, the backlog captured during it arrives at
//!   once and lands at its capture time, so the audio after the stall is
//!   not offset (placing it from its read on would leave the source a
//!   stall-length late for the rest of the recording).
//!
//! Input placed before the output position covers time that already went
//! out (as silence): that part is dropped, so what follows stays in place
//! instead of everything after it shifting late. A backlog no longer than
//! the lag is never cut this way.
//!
//! Timestamped input that does not end a gap still follows on, unless its
//! timestamp is more than [`REPLACE_NS`] away from where it would follow
//! on: then it is re-placed by the timestamp (silence inserted when it is
//! late, its head dropped when it overlaps what is pending). That covers a
//! discontinuity the source reports (a device glitch, its ring dropping
//! samples) and a device sample clock that drifts against the recording
//! clock (corrected in steps of about the tolerance).
//!
//! Known limitation: a source without timestamps just follows on, so a
//! device whose sample clock drifts against the recording clock (tens of
//! ppm are common) slowly runs ahead or behind until a gap re-places it.

use super::{AudioSource, Clock};
use std::time::Duration;

/// Output sample rate.
pub const RATE: u32 = 48_000;
/// Output channels (interleaved L, R).
pub const CHANNELS: usize = 2;

/// At most this much input (in seconds) is held per source; a source that
/// runs ahead of the clock loses its oldest samples instead of growing.
const MAX_AHEAD_SECS: usize = 1;

/// Timestamped input more than this (ns) away from where it would follow
/// on is re-placed by its timestamp.
const REPLACE_NS: f64 = 15_000_000.0;

struct Input {
    src: Box<dyn AudioSource>,
    gain: f32,
    /// Pending input as stereo frames.
    buf: Vec<[f32; 2]>,
    /// Output position inside `buf` in input frames (fractional); negative
    /// = that much lead silence before `buf[0]`.
    pos: f64,
    /// Ran dry: silent (and `buf` empty); the next input is placed by its
    /// capture time.
    gap: bool,
    scratch: Vec<f32>,
}

/// Where output frame `produced` (48 kHz) falls in input that starts at
/// `start_ns` (clock time, may be negative), in input frames at `rate`
/// (negative = lead silence before it). Exact for whole frames, so float
/// error never skips a sample.
fn place(produced: u64, start_ns: i128, rate: u32) -> f64 {
    let (rate, out) = (i128::from(rate), i128::from(RATE));
    let num = i128::from(produced) * rate * 1_000_000_000 - start_ns * out * rate;
    let den = out * 1_000_000_000;
    num.div_euclid(den) as f64 + num.rem_euclid(den) as f64 / den as f64
}

impl Input {
    /// Read what the source has and append it to `buf` as stereo frames.
    /// Input that ends a gap is placed by its capture time (see the module
    /// docs); `now` is the clock time of the read, `produced` the output
    /// position in output frames, and `clock` maps capture instants.
    fn read(&mut self, now: Duration, produced: u64, clock: &Clock) {
        self.scratch.clear();
        let (_, ts) = self.src.read_timed(&mut self.scratch);
        let stamp = match ts.map(|t| clock.time_of(t)) {
            Some(None) => return, // captured during a pause: not recorded
            Some(Some(ns)) => Some(ns),
            None => None,
        };
        let before = self.buf.len();
        let ch = usize::from(self.src.channels().max(1));
        if ch == 1 {
            self.buf.extend(self.scratch.iter().map(|&s| [s, s]));
        } else {
            // Extra channels beyond the first two are not mixed.
            let whole = self.scratch.len() / ch * ch;
            self.buf.extend(self.scratch[..whole].chunks(ch).map(|f| [f[0], f[1]]));
        }
        let added = self.buf.len() - before;
        let rate = self.src.rate().max(1);
        if self.gap && added > 0 {
            // Untimed input from before time zero starts at zero; timed
            // input captured before the start loses that part.
            let start = stamp.unwrap_or_else(|| {
                (now.as_nanos() as i128 - (added as i128 * 1_000_000_000 / i128::from(rate))).max(0)
            });
            self.pos = place(produced, start, rate);
            if self.pos >= self.buf.len() as f64 {
                // All of it is older than the output position.
                self.buf.clear();
                self.pos = 0.0;
                return;
            }
            self.gap = false;
            if self.pos >= 1.0 {
                // Its head covers time already produced: dropped.
                let past = self.pos as usize;
                self.buf.drain(..past);
                self.pos -= past as f64;
            }
        } else if let Some(start) = stamp
            && added > 0
        {
            // Where it would follow on: the output time of `buf[before]`.
            let at = produced as f64 * 1e9 / f64::from(RATE) + (before as f64 - self.pos) * 1e9 / f64::from(rate);
            let off = start as f64 - at;
            let frames = (off.abs() * f64::from(rate) / 1e9).round() as usize;
            if off > REPLACE_NS {
                self.buf.splice(before..before, std::iter::repeat_n([0.0; 2], frames));
            } else if off < -REPLACE_NS {
                self.buf.drain(before..before + frames.min(added));
            }
        }
        let cap = rate as usize * MAX_AHEAD_SECS;
        let ahead = self.buf.len().saturating_sub(self.pos.max(0.0) as usize);
        if ahead > cap {
            let cut = ahead - cap;
            self.buf.drain(..cut);
            // Lead silence stays; otherwise the position moves with the data.
            if self.pos > 0.0 {
                self.pos = (self.pos - cut as f64).max(0.0);
            }
        }
    }

    /// Add `frames` output frames of this source into `out` (stereo).
    fn mix_into(&mut self, out: &mut [f32], frames: usize) {
        if self.gap {
            return;
        }
        let step = f64::from(self.src.rate().max(1)) / f64::from(RATE);
        let n = self.buf.len();
        for (k, o) in out.as_chunks_mut::<2>().0.iter_mut().take(frames).enumerate() {
            let p = self.pos + k as f64 * step;
            if p < 0.0 {
                continue; // lead silence
            }
            let i = p as usize;
            if i >= n {
                break; // ran dry: silence
            }
            let f = (p - i as f64) as f32;
            let a = self.buf[i];
            let b = if i + 1 < n { self.buf[i + 1] } else { a };
            o[0] += self.gain * (a[0] + (b[0] - a[0]) * f);
            o[1] += self.gain * (a[1] + (b[1] - a[1]) * f);
        }
        self.pos += frames as f64 * step;
        if self.pos >= n as f64 {
            // Everything was played: a gap until the source sends again.
            self.buf.clear();
            self.pos = 0.0;
            self.gap = true;
            return;
        }
        if self.pos > 0.0 {
            let used = (self.pos as usize).min(n);
            self.buf.drain(..used);
            self.pos -= used as f64;
        }
    }

    /// Input frames pending beyond the output position (lead silence counts).
    fn available(&self) -> f64 {
        self.buf.len() as f64 - self.pos
    }
}

/// Mixes [`AudioSource`]s into 48 kHz stereo, by time.
pub struct Mixer {
    inputs: Vec<Input>,
    /// Output frames produced so far.
    produced: u64,
}

fn frames_at(t: Duration) -> u64 {
    (t.as_nanos() * u128::from(RATE) / 1_000_000_000) as u64
}

impl Mixer {
    /// Every source starts in a gap: its first input is placed by its
    /// capture time (input from before time zero starts at zero).
    pub fn new(sources: Vec<Box<dyn AudioSource>>) -> Mixer {
        let inputs = sources
            .into_iter()
            .map(|src| Input { src, gain: 1.0, buf: Vec::new(), pos: 0.0, gap: true, scratch: Vec::new() })
            .collect();
        Mixer { inputs, produced: 0 }
    }

    /// Gain of source `i` (in the order given to [`Mixer::new`]).
    pub fn set_gain(&mut self, i: usize, gain: f32) {
        if let Some(s) = self.inputs.get_mut(i) {
            s.gain = gain;
        }
    }

    /// Time of the next sample [`Mixer::pull`] returns.
    pub fn position(&self) -> Duration {
        Duration::from_nanos((u128::from(self.produced) * 1_000_000_000 / u128::from(RATE)) as u64)
    }

    /// Read what every source has now; `now` is the recording clock's time
    /// of this read, where input that ends a gap (and has no timestamp)
    /// ends, and `clock` maps capture timestamps to clock time. Input
    /// placed before [`Mixer::position`] loses that part.
    pub fn feed(&mut self, now: Duration, clock: &Clock) {
        for s in &mut self.inputs {
            s.read(now, self.produced, clock);
        }
    }

    /// Mixed samples (interleaved stereo) from [`Mixer::position`] up to
    /// `until`, from the input fed so far; a source without input there is
    /// silent. Empty when `until` is not past the position.
    pub fn pull(&mut self, until: Duration) -> Vec<f32> {
        let frames = frames_at(until).saturating_sub(self.produced) as usize;
        if frames == 0 {
            return Vec::new();
        }
        let mut out = vec![0.0f32; frames * CHANNELS];
        for s in &mut self.inputs {
            s.mix_into(&mut out, frames);
        }
        for v in &mut out {
            *v = v.clamp(-1.0, 1.0);
        }
        self.produced += frames as u64;
        out
    }

    /// Throw away the input available now (while paused); every source is
    /// then in a gap, so what it sends next is placed by its capture time.
    pub fn discard(&mut self) {
        for s in &mut self.inputs {
            s.scratch.clear();
            s.src.read_timed(&mut s.scratch);
            s.buf.clear();
            s.pos = 0.0;
            s.gap = true;
        }
    }

    /// Whether a [`Mixer::pull`] to `until` would have every source's
    /// input (as of the last feed). A source in a gap does not count: it
    /// has nothing pending, and what it sends next is placed by its own
    /// capture time whenever it comes.
    pub fn covered(&self, until: Duration) -> bool {
        let frames = frames_at(until).saturating_sub(self.produced) as f64;
        self.inputs.iter().all(|s| s.gap || s.available() >= frames * f64::from(s.src.rate()) / f64::from(RATE))
    }
}

#[cfg(test)]
mod tests {
    use super::super::AUDIO_LAG;
    use super::*;
    use std::time::Instant;

    /// Hands out `data` (interleaved) in pieces of `chunk` samples per read.
    struct Fake {
        rate: u32,
        ch: u16,
        data: Vec<f32>,
        at: usize,
        chunk: usize,
    }

    impl AudioSource for Fake {
        fn rate(&self) -> u32 {
            self.rate
        }
        fn channels(&self) -> u16 {
            self.ch
        }
        fn read(&mut self, out: &mut Vec<f32>) -> usize {
            let end = (self.at + self.chunk).min(self.data.len());
            out.extend_from_slice(&self.data[self.at..end]);
            let n = end - self.at;
            self.at = end;
            n
        }
    }

    fn fake(rate: u32, ch: u16, data: Vec<f32>) -> Box<dyn AudioSource> {
        let chunk = data.len();
        Box::new(Fake { rate, ch, data, at: 0, chunk })
    }

    /// A mixer over `sources` that has read all of them at time zero.
    fn fed(sources: Vec<Box<dyn AudioSource>>) -> Mixer {
        let mut m = Mixer::new(sources);
        m.feed(Duration::ZERO, &clock());
        m
    }

    /// The tests' recording clock: started at [`base`], never paused.
    fn clock() -> Clock {
        Clock::start_at(base())
    }

    fn base() -> Instant {
        static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        *BASE.get_or_init(|| Instant::now() + Duration::from_secs(10))
    }

    fn sine(rate: u32, hz: f64, secs: f64) -> Vec<f32> {
        let n = (rate as f64 * secs) as usize;
        (0..n).map(|i| (0.5 * (2.0 * std::f64::consts::PI * hz * i as f64 / rate as f64).sin()) as f32).collect()
    }

    #[test]
    fn resamples_44k1_to_48k() {
        let mut m = fed(vec![fake(44_100, 1, sine(44_100, 1000.0, 1.0))]);
        assert_eq!(m.inputs[0].buf.len(), 44_100, "within the cap: nothing trimmed");
        let out = m.pull(Duration::from_secs(1));
        assert_eq!(out.len(), 48_000 * 2, "one second of stereo");
        assert_eq!(m.position(), Duration::from_secs(1));
        let mut worst = 0.0f64;
        for (k, f) in out.as_chunks::<2>().0.iter().enumerate() {
            let want = 0.5 * (2.0 * std::f64::consts::PI * 1000.0 * k as f64 / 48_000.0).sin();
            worst = worst.max((f[0] as f64 - want).abs());
            assert_eq!(f[0], f[1], "mono goes to both channels");
        }
        assert!(worst < 0.01, "linear interpolation error {worst}");
        // Pulling up to the same time gives nothing more.
        assert!(m.pull(Duration::from_secs(1)).is_empty());
    }

    #[test]
    fn pulls_are_continuous_across_calls() {
        let src = sine(44_100, 440.0, 1.0);
        let mut whole = fed(vec![fake(44_100, 1, src.clone())]);
        let one = whole.pull(Duration::from_millis(500));
        let chunk = 441; // 10 ms per read
        let mut parts = Mixer::new(vec![Box::new(Fake { rate: 44_100, ch: 1, data: src, at: 0, chunk })]);
        // Feed ahead of the pulls (as the session's audio lag does).
        let mut got = Vec::new();
        for ms in (10u64..=500).step_by(10) {
            parts.feed(Duration::from_millis(ms - 10), &clock());
            got.extend(parts.pull(Duration::from_millis(ms - 10)));
        }
        parts.feed(Duration::from_millis(500), &clock()); // the next samples, to interpolate towards
        got.extend(parts.pull(Duration::from_millis(500)));
        assert_eq!(got.len(), one.len());
        let worst = got.iter().zip(&one).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(worst < 1e-4, "{worst}");
    }

    #[test]
    fn sums_with_gain_and_clamps() {
        let a = fake(48_000, 2, [0.75, -0.75].repeat(480));
        let b = fake(48_000, 2, [0.5, -0.1].repeat(480));
        let mut m = fed(vec![a, b]);
        let out = m.pull(Duration::from_millis(10));
        assert_eq!(out.len(), 960);
        assert!(out.as_chunks::<2>().0.iter().all(|f| f[0] == 1.0 && (f[1] + 0.85).abs() < 1e-6), "{:?}", &out[..4]);
        let a = fake(48_000, 2, [0.75, -0.75].repeat(480));
        let b = fake(48_000, 2, [0.5, -0.5].repeat(480));
        let mut m = fed(vec![a, b]);
        m.set_gain(0, 0.5);
        m.set_gain(1, -2.0);
        let out = m.pull(Duration::from_millis(10));
        assert!(out.as_chunks::<2>().0.iter().all(|f| (f[0] + 0.625).abs() < 1e-6 && (f[1] - 0.625).abs() < 1e-6));
    }

    #[test]
    fn mono_to_stereo_and_extra_channels() {
        let mut m = fed(vec![fake(48_000, 1, vec![0.25; 480])]);
        assert!(m.pull(Duration::from_millis(10)).iter().all(|&v| v == 0.25));
        // 4 channels: the first two are kept.
        let quad = [0.1, 0.2, 0.9, 0.9].repeat(480);
        let mut m = fed(vec![fake(48_000, 4, quad)]);
        let out = m.pull(Duration::from_millis(10));
        assert!(out.as_chunks::<2>().0.iter().all(|f| *f == [0.1, 0.2]));
    }

    /// Hands out whatever the test queued since the last read.
    struct Queued(std::sync::Arc<std::sync::Mutex<Vec<f32>>>);

    impl AudioSource for Queued {
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

    /// The sample a test device captures at frame `c` (48 kHz): a 1 s
    /// sawtooth in [0.1, 0.9), so each output sample tells when (within a
    /// second) it was captured.
    fn saw(c: u64) -> f32 {
        0.1 + 0.8 * (c % 48_000) as f32 / 48_000.0
    }

    /// For each non-silent frame of `out` (stereo, from [`saw`]): its output
    /// frame and how many frames after its capture it plays.
    fn offsets(out: &[f32]) -> Vec<(usize, i64)> {
        let frames = out.as_chunks::<2>().0.iter().enumerate();
        frames
            .filter(|(_, f)| f[0] != 0.0)
            .map(|(k, f)| {
                let c = ((f64::from(f[0]) - 0.1) / 0.8 * 48_000.0).round() as i64;
                let d = (k as i64 - c).rem_euclid(48_000);
                (k, if d > 24_000 { d - 48_000 } else { d })
            })
            .collect()
    }

    fn worst(o: &[(usize, i64)]) -> Option<(usize, i64)> {
        o.iter().copied().max_by_key(|&(_, d)| d.abs())
    }

    /// Runs a mixer like the session's encode loop over a 48 kHz mono
    /// device for `total_ms` of clock time. At every 10 ms tick `t` the
    /// device captures the last 10 ms if `capturing(t)` (loopback captures
    /// nothing while nothing plays) and hands over everything it holds if
    /// `delivers(t)` (otherwise it is stalled and holds it); then the mixer
    /// is fed at `t` and pulled to `t - AUDIO_LAG`. Returns the output
    /// (stereo) and how many frames were captured.
    fn run_device(total_ms: u64, capturing: impl Fn(u64) -> bool, delivers: impl Fn(u64) -> bool) -> (Vec<f32>, usize) {
        let q = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut m = Mixer::new(vec![Box::new(Queued(q.clone()))]);
        let (mut held, mut out, mut captured) = (Vec::new(), Vec::new(), 0);
        for ms in (0..=total_ms).step_by(10) {
            if ms > 0 && capturing(ms) {
                held.extend((ms * 48 - 480..ms * 48).map(saw));
                captured += 480;
            }
            if delivers(ms) {
                q.lock().unwrap().append(&mut held);
            }
            let t = Duration::from_millis(ms);
            m.feed(t, &clock());
            out.extend(m.pull(t.saturating_sub(AUDIO_LAG)));
        }
        (out, captured)
    }

    #[test]
    fn a_long_gap_then_real_time_input_plays_at_its_capture_time() {
        // Loopback captures nothing for 3 s (nothing plays), then a 1 s
        // tone in 10 ms packets, each read when it is due.
        let (out, captured) = run_device(4500, |ms| (3010..=4000).contains(&ms), |_| true);
        let o = offsets(&out);
        let onset = Duration::from_secs_f64(o[0].0 as f64 / 48_000.0);
        assert!(onset.abs_diff(Duration::from_secs(3)) <= Duration::from_millis(10), "onset at {onset:?}");
        assert_eq!(o.len(), captured, "every frame played");
        assert!(o.iter().all(|&(_, d)| d.abs() <= 480), "worst {:?}", worst(&o));
    }

    #[test]
    fn a_burst_after_a_gap_is_a_backlog() {
        // 3 s of nothing, then 80 ms handed over at once at 3 s (within the
        // lag): all of it, ending at its arrival, i.e. at its capture time.
        let (out, captured) = run_device(3500, |ms| (2930..=3000).contains(&ms), |ms| ms >= 3000);
        let o = offsets(&out);
        assert_eq!((o[0].0, o.len(), captured), (48 * 2920, 48 * 80, 48 * 80));
        assert!(o.iter().all(|&(_, d)| d == 0), "worst {:?}", worst(&o));
        // A 1 s backlog: what is older than the output position (2.89 s)
        // already went out as silence and is dropped; the rest is in place.
        let (out, _) = run_device(3500, |ms| (2010..=3000).contains(&ms), |ms| ms >= 3000);
        let o = offsets(&out);
        assert_eq!((o[0].0, o.len()), (48 * 2890, 48 * 110));
        assert!(o.iter().all(|&(_, d)| d == 0), "worst {:?}", worst(&o));
    }

    #[test]
    fn a_stall_longer_than_the_lag_leaves_no_offset() {
        // A continuous device stalls for 150 ms (the lag is 100 ms), then
        // hands over its backlog at once.
        let (out, _) = run_device(3000, |_| true, |ms| !(1510..1660).contains(&ms));
        let o = offsets(&out);
        assert!(o.iter().all(|&(_, d)| d.abs() <= 480), "worst {:?}", worst(&o));
        // After the stall: in place and contiguous up to the output's end.
        let after: Vec<_> = o.iter().filter(|&&(k, _)| k >= 48 * 1700).collect();
        assert!(after.iter().all(|&&(_, d)| d == 0));
        assert_eq!(after.len(), 48 * (2900 - 1700));
    }

    #[test]
    fn repeated_stalls_do_not_accumulate() {
        // A 150 ms stall every second from 1.5 s on.
        let (out, _) = run_device(6000, |_| true, |ms| ms < 1000 || !(510..660).contains(&(ms % 1000)));
        let o = offsets(&out);
        assert!(o.iter().all(|&(_, d)| d.abs() <= 480), "worst {:?}", worst(&o));
        let last: Vec<_> = o.iter().filter(|&&(k, _)| k >= 48 * 5700).collect();
        assert!(last.iter().all(|&&(_, d)| d == 0));
        assert_eq!(last.len(), 48 * (5900 - 5700));
    }

    #[test]
    fn jitter_within_the_lag_is_seamless() {
        // Packets 0 and 20 ms apart, alternating.
        let (out, captured) = run_device(3000, |_| true, |ms| ms % 20 == 0);
        let o = offsets(&out);
        assert_eq!(o.len(), out.len() / 2, "no inserted silence");
        assert_eq!(o.len(), 48 * 2900);
        assert!(captured >= o.len() && o.iter().all(|&(_, d)| d == 0), "worst {:?}", worst(&o));
    }

    #[test]
    fn sources_at_different_rates_stay_aligned() {
        // A at 44.1 kHz on the left, B at 48 kHz on the right, both a square
        // wave switching every 100 ms, in real-time 10 ms packets.
        let sq = |c: usize, rate: usize| if (c * 10 / rate) % 2 == 1 { 0.5f32 } else { 0.25 };
        let a: Vec<f32> = (0..44_100 * 103 / 10).flat_map(|c| [sq(c, 44_100), 0.0]).collect();
        let b: Vec<f32> = (0..48_000 * 103 / 10).flat_map(|c| [0.0, sq(c, 48_000)]).collect();
        let mut m = Mixer::new(vec![
            Box::new(Fake { rate: 44_100, ch: 2, data: a, at: 0, chunk: 441 * 2 }),
            Box::new(Fake { rate: 48_000, ch: 2, data: b, at: 0, chunk: 480 * 2 }),
        ]);
        let mut out = Vec::new();
        for ms in (10..=10_200).step_by(10) {
            let t = Duration::from_millis(ms);
            m.feed(t, &clock());
            out.extend(m.pull(t.saturating_sub(AUDIO_LAG)));
        }
        let edges = |ch: usize| -> Vec<usize> {
            let v: Vec<bool> = out.as_chunks::<2>().0.iter().map(|f| f[ch] > 0.375).collect();
            (1..v.len()).filter(|&k| v[k] != v[k - 1]).collect()
        };
        let (l, r) = (edges(0), edges(1));
        assert_eq!(l.len(), r.len());
        assert!(l.len() >= 99, "{} edges", l.len());
        for (i, (&x, &y)) in l.iter().zip(&r).enumerate() {
            let want = (i + 1) * 4800;
            assert!(x.abs_diff(want) <= 240 && y.abs_diff(want) <= 240, "edge {i}: {x} / {y}, want {want}");
        }
    }

    /// Hands out what the test queued, stamped with the time it gave.
    #[allow(clippy::type_complexity)]
    struct Timed(std::sync::Arc<std::sync::Mutex<(Vec<f32>, Option<Instant>)>>);

    impl AudioSource for Timed {
        fn rate(&self) -> u32 {
            48_000
        }
        fn channels(&self) -> u16 {
            1
        }
        fn read(&mut self, out: &mut Vec<f32>) -> usize {
            self.read_timed(out).0
        }
        fn read_timed(&mut self, out: &mut Vec<f32>) -> (usize, Option<Instant>) {
            let mut q = self.0.lock().unwrap();
            let n = q.0.len();
            out.append(&mut q.0);
            (n, q.1.take())
        }
    }

    #[test]
    fn timed_input_is_placed_by_its_timestamp() {
        let q = std::sync::Arc::new(std::sync::Mutex::new((Vec::new(), None)));
        let mut m = Mixer::new(vec![Box::new(Timed(q.clone()))]);
        let ms = Duration::from_millis;
        m.feed(ms(0), &clock());
        assert!(m.pull(ms(1000)).iter().all(|&v| v == 0.0));
        // 100 ms captured from 0.95 s, read at 1.2 s (the end anchor would
        // say 1.1 s): the 50 ms before the position are dropped.
        *q.lock().unwrap() = (vec![0.5; 4800], Some(base() + ms(950)));
        m.feed(ms(1200), &clock());
        let o = m.pull(ms(1100));
        assert!(o[..4800].iter().all(|&v| v == 0.5) && o[4800..].iter().all(|&v| v == 0.0));
        // Stamped ahead of the position: lead silence up to it.
        *q.lock().unwrap() = (vec![0.25; 480], Some(base() + ms(1300)));
        m.feed(ms(1150), &clock());
        let o = m.pull(ms(1400));
        assert!(o[..19_200].iter().all(|&v| v == 0.0) && o[19_200..20_160].iter().all(|&v| v == 0.25));
        assert!(o[20_160..].iter().all(|&v| v == 0.0));
        // Entirely before the position: dropped, still in a gap.
        *q.lock().unwrap() = (vec![0.75; 480], Some(base() + ms(1000)));
        m.feed(ms(1450), &clock());
        assert!(m.covered(ms(1500)));
        assert!(m.pull(ms(1500)).iter().all(|&v| v == 0.0));
    }

    #[test]
    fn timed_input_follows_on_and_is_replaced_when_its_stamp_jumps() {
        // A real-time device in 10 ms packets whose stamps jitter by a few
        // ms; it loses 40 ms at 1.0 s (its next stamp jumps ahead) and at
        // 2.0 s hands over 30 ms it had already sent (stamped back).
        let q = std::sync::Arc::new(std::sync::Mutex::new((Vec::new(), None)));
        let mut m = Mixer::new(vec![Box::new(Timed(q.clone()))]);
        let ms = Duration::from_millis;
        let mut out = Vec::new();
        for t in (10..=3000u64).step_by(10) {
            let jitter = [0i64, 3, -2, 1][((t / 10 + 3) % 4) as usize];
            let (from, stamp) = match t - 10 {
                1000..1040 => (None, 0),
                _ if t == 2000 => (Some(1960), 1960),
                s => (Some(s), (s as i64 + jitter) as u64),
            };
            if let Some(from) = from {
                *q.lock().unwrap() = ((from * 48..t * 48).map(saw).collect(), Some(base() + ms(stamp)));
            }
            m.feed(ms(t), &clock());
            out.extend(m.pull(ms(t).saturating_sub(AUDIO_LAG)));
        }
        let o = offsets(&out);
        assert!(o.iter().all(|&(_, d)| d == 0), "every sample at its capture time; worst {:?}", worst(&o));
        assert_eq!(o.len(), 48 * (2900 - 40), "only the lost 40 ms are silent");
        assert!(o.iter().all(|&(k, _)| !(48 * 1000..48 * 1040).contains(&k)));
    }

    #[test]
    fn timed_input_captured_during_a_pause_is_dropped() {
        let q = std::sync::Arc::new(std::sync::Mutex::new((Vec::new(), None)));
        let mut m = Mixer::new(vec![Box::new(Timed(q.clone()))]);
        let ms = Duration::from_millis;
        let mut c = clock();
        c.pause_at(base() + ms(1000));
        c.resume_at(base() + ms(2000));
        m.feed(ms(0), &c);
        assert!(m.pull(ms(900)).iter().all(|&v| v == 0.0));
        *q.lock().unwrap() = (vec![0.5; 480], Some(base() + ms(1500)));
        m.feed(ms(1000), &c);
        assert!(m.covered(ms(2000)), "nothing pending: still in a gap");
        // After the resume: clock time 1.1 s.
        *q.lock().unwrap() = (vec![0.25; 480], Some(base() + ms(2100)));
        m.feed(ms(1120), &c);
        let o = m.pull(ms(1200));
        assert!(o[..19_200].iter().all(|&v| v == 0.0) && o[19_200..20_160].iter().all(|&v| v == 0.25));
        assert!(o[20_160..].iter().all(|&v| v == 0.0));
    }

    #[test]
    fn input_after_running_dry_lands_at_its_capture_time() {
        let q = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut m = Mixer::new(vec![Box::new(Queued(q.clone()))]);
        m.feed(Duration::ZERO, &clock());
        assert!(m.pull(Duration::from_millis(50)).iter().all(|&v| v == 0.0));
        assert!(m.covered(Duration::from_millis(60)), "a source in a gap is not waited for");
        // 100 ms read at 150 ms: it ends there, so it starts at the position.
        q.lock().unwrap().extend((0..4800).map(|i| if i < 2400 { 0.25f32 } else { 0.75 }));
        m.feed(Duration::from_millis(150), &clock());
        assert!(m.covered(Duration::from_millis(150)) && !m.covered(Duration::from_millis(151)));
        let o = m.pull(Duration::from_millis(150));
        assert!(o[..4800].iter().all(|&v| v == 0.25) && o[4800..].iter().all(|&v| v == 0.75), "all of it, in order");
        // 10 ms read at 170 ms, ahead of the position: 160..170 ms.
        q.lock().unwrap().extend([0.5f32; 480]);
        m.feed(Duration::from_millis(170), &clock());
        let o = m.pull(Duration::from_millis(180));
        assert!(o[..960].iter().all(|&v| v == 0.0) && o[960..1920].iter().all(|&v| v == 0.5));
        assert!(o[1920..].iter().all(|&v| v == 0.0));
    }

    #[test]
    fn underrun_is_silence_and_input_stays_bounded() {
        let mut m = fed(vec![fake(48_000, 1, vec![0.5; 240])]);
        let out = m.pull(Duration::from_millis(10));
        assert_eq!(out.len(), 960);
        assert!(out[..480].iter().all(|&v| v == 0.5) && out[480..].iter().all(|&v| v == 0.0));
        // No sources: silence of the right length.
        assert_eq!(Mixer::new(Vec::new()).pull(Duration::from_millis(5)), vec![0.0; 480]);
        // A source far ahead of the clock keeps at most a second of input.
        let mut m = fed(vec![fake(48_000, 1, vec![0.1; 48_000 * 5])]);
        assert!(m.inputs[0].buf.len() <= 48_000);
        // Discarding (pause) drops what arrived.
        m.discard();
        assert!(m.inputs[0].buf.is_empty());
    }
}
