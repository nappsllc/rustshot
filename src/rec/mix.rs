//! Audio mixer: every source resampled (linear) to 48 kHz stereo, scaled
//! by its gain, summed and clamped to [-1, 1].
//!
//! Timing. Each source's pending input is a run of samples placed on the
//! output timeline: `pos` is where the output position falls inside it, in
//! input frames, and a negative `pos` is lead silence before its first
//! sample. While a source delivers continuously, its samples follow on one
//! another, so jitter shorter than the time buffered ahead does nothing.
//! When it runs dry (everything it gave was played) it is in a *gap*: it
//! adds silence, and the first input read after the gap is placed to start
//! at the clock time it was read ([`Mixer::feed`]'s `now`). So a source
//! that sends nothing for seconds (WASAPI loopback while nothing plays) and
//! then resumes plays from the moment it resumed, whether the output runs
//! behind the clock (the session's lag) or not, and whether the input
//! comes in small packets or one burst. The error is at most one packet
//! (its samples were captured just before they were read, but are played
//! from the read on).

use super::AudioSource;
use std::time::Duration;

/// Output sample rate.
pub const RATE: u32 = 48_000;
/// Output channels (interleaved L, R).
pub const CHANNELS: usize = 2;

/// At most this much input (in seconds) is held per source; a source that
/// runs ahead of the clock loses its oldest samples instead of growing.
const MAX_AHEAD_SECS: usize = 1;

struct Input {
    src: Box<dyn AudioSource>,
    gain: f32,
    /// Pending input as stereo frames.
    buf: Vec<[f32; 2]>,
    /// Output position inside `buf` in input frames (fractional); negative
    /// = that much lead silence before `buf[0]`.
    pos: f64,
    /// Ran dry: silent, and the next input is placed at its arrival.
    gap: bool,
    scratch: Vec<f32>,
}

impl Input {
    /// Read what the source has and append it to `buf` as stereo frames.
    /// Input that ends a gap starts `lead` after the output position.
    fn read(&mut self, lead: Duration) {
        self.scratch.clear();
        self.src.read(&mut self.scratch);
        let ch = usize::from(self.src.channels().max(1));
        if ch == 1 {
            self.buf.extend(self.scratch.iter().map(|&s| [s, s]));
        } else {
            // Extra channels beyond the first two are not mixed.
            let whole = self.scratch.len() / ch * ch;
            self.buf.extend(self.scratch[..whole].chunks(ch).map(|f| [f[0], f[1]]));
        }
        if self.gap && !self.buf.is_empty() {
            self.gap = false;
            self.pos = -lead.as_secs_f64() * f64::from(self.src.rate());
        }
        let cap = self.src.rate() as usize * MAX_AHEAD_SECS;
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
    /// Every source starts in a gap: its first input plays from when it is read.
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
    /// of this read, where input that ends a gap starts (never before
    /// [`Mixer::position`]).
    pub fn feed(&mut self, now: Duration) {
        let lead = Duration::from_nanos(
            (u128::from(frames_at(now).saturating_sub(self.produced)) * 1_000_000_000 / u128::from(RATE)) as u64,
        );
        for s in &mut self.inputs {
            s.read(lead);
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
    /// then in a gap, so what it sends next plays from when it is read.
    pub fn discard(&mut self) {
        for s in &mut self.inputs {
            s.read(Duration::ZERO);
            s.buf.clear();
            s.pos = 0.0;
            s.gap = true;
        }
    }

    /// Whether a [`Mixer::pull`] to `until` would have every source's
    /// input (as of the last feed). A source in a gap does not count: what
    /// it sends next starts at its arrival, so waiting for it cannot fill
    /// the time before.
    pub fn covered(&self, until: Duration) -> bool {
        let frames = frames_at(until).saturating_sub(self.produced) as f64;
        self.inputs.iter().all(|s| s.gap || s.available() >= frames * f64::from(s.src.rate()) / f64::from(RATE))
    }
}

#[cfg(test)]
mod tests {
    use super::super::AUDIO_LAG;
    use super::*;

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
        m.feed(Duration::ZERO);
        m
    }

    fn sine(rate: u32, hz: f64, secs: f64) -> Vec<f32> {
        let n = (rate as f64 * secs) as usize;
        (0..n).map(|i| (0.5 * (2.0 * std::f64::consts::PI * hz * i as f64 / rate as f64).sin()) as f32).collect()
    }

    #[test]
    fn resamples_44k1_to_48k() {
        let mut m = fed(vec![fake(44_100, 1, sine(44_100, 1000.0, 1.1))]);
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
            parts.feed(Duration::from_millis(ms - 10));
            got.extend(parts.pull(Duration::from_millis(ms - 10)));
        }
        parts.feed(Duration::from_millis(500)); // the next samples, to interpolate towards
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

    /// Runs a mixer like the session's encode loop: every 10 ms of clock
    /// time `t` up to 4.5 s, `deliver(t)` samples are queued, the mixer is
    /// fed at `t` and pulled to `t - AUDIO_LAG`. Returns the output (stereo).
    fn run_lagged(deliver: impl Fn(u64) -> usize) -> Vec<f32> {
        let q = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut m = Mixer::new(vec![Box::new(Queued(q.clone()))]);
        let mut out = Vec::new();
        for ms in (0..=4500).step_by(10) {
            q.lock().unwrap().extend(std::iter::repeat_n(0.5f32, deliver(ms)));
            let t = Duration::from_millis(ms);
            m.feed(t);
            out.extend(m.pull(t.saturating_sub(AUDIO_LAG)));
        }
        out
    }

    /// Asserts that the 1 s tone of 0.5 is (nearly) all in `out` and starts
    /// within 10 ms of 3 s, when it was first read.
    fn assert_tone_at_3s(out: &[f32]) {
        let first = out.iter().position(|&v| v != 0.0).expect("the tone is in the output") / 2;
        let onset = Duration::from_secs_f64(first as f64 / 48_000.0);
        assert!(onset.abs_diff(Duration::from_secs(3)) <= Duration::from_millis(10), "tone at {onset:?}");
        let tone = out.iter().filter(|&&v| v == 0.5).count() / 2;
        assert!(tone >= 48_000 * 95 / 100, "{tone} of 48000 tone frames played");
        assert!(out.iter().all(|&v| v == 0.0 || v == 0.5));
    }

    #[test]
    fn a_long_gap_then_real_time_input_plays_at_its_arrival() {
        // Loopback sends nothing for 3 s (nothing plays), then a 1 s tone in
        // 10 ms packets, each read when it is due, while the output runs a
        // lag behind the clock.
        let out = run_lagged(|ms| if (3000..4000).contains(&ms) { 480 } else { 0 });
        assert_tone_at_3s(&out);
        assert_eq!(out.iter().filter(|&&v| v == 0.5).count() / 2, 48_000, "nothing dropped");
    }

    #[test]
    fn a_long_gap_then_a_burst_plays_from_its_arrival() {
        // The same tone, all at once at 3 s: not placed a lag early.
        let out = run_lagged(|ms| if ms == 3000 { 48_000 } else { 0 });
        assert_tone_at_3s(&out);
    }

    #[test]
    fn late_input_after_running_dry_is_played_not_skipped() {
        let q = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut m = Mixer::new(vec![Box::new(Queued(q.clone()))]);
        m.feed(Duration::ZERO);
        assert!(m.pull(Duration::from_millis(50)).iter().all(|&v| v == 0.0));
        assert!(m.covered(Duration::from_millis(60)), "a source in a gap is not waited for");
        q.lock().unwrap().extend((0..4800).map(|i| if i < 2400 { 0.25f32 } else { 0.75 }));
        m.feed(Duration::from_millis(50));
        assert!(m.covered(Duration::from_millis(150)) && !m.covered(Duration::from_millis(151)));
        let o = m.pull(Duration::from_millis(150));
        assert!(o[..4800].iter().all(|&v| v == 0.25) && o[4800..].iter().all(|&v| v == 0.75), "all of it, in order");
        // Input read ahead of the output position starts at its read time.
        q.lock().unwrap().extend([0.5f32; 480]);
        m.feed(Duration::from_millis(170));
        let o = m.pull(Duration::from_millis(180));
        assert!(o[..1920].iter().all(|&v| v == 0.0) && o[1920..].iter().all(|&v| v == 0.5));
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
