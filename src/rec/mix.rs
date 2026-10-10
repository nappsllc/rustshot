//! Audio mixer: every source resampled (linear) to 48 kHz stereo, scaled
//! by its gain, summed and clamped to [-1, 1].

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
    /// Read position into `buf` in input frames (fractional).
    pos: f64,
    scratch: Vec<f32>,
}

impl Input {
    /// Read what the source has and append it to `buf` as stereo frames.
    fn read(&mut self) {
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
        let cap = self.src.rate() as usize * MAX_AHEAD_SECS;
        let ahead = self.buf.len().saturating_sub(self.pos as usize);
        if ahead > cap {
            let cut = ahead - cap;
            self.buf.drain(..cut);
            self.pos = (self.pos - cut as f64).max(0.0);
        }
    }

    /// Add `frames` output frames of this source into `out` (stereo).
    fn mix_into(&mut self, out: &mut [f32], frames: usize) {
        let step = f64::from(self.src.rate().max(1)) / f64::from(RATE);
        let n = self.buf.len();
        for (k, o) in out.as_chunks_mut::<2>().0.iter_mut().take(frames).enumerate() {
            let p = self.pos + k as f64 * step;
            let i = p as usize;
            if i >= n {
                break; // underrun: silence
            }
            let f = (p - i as f64) as f32;
            let a = self.buf[i];
            let b = if i + 1 < n { self.buf[i + 1] } else { a };
            o[0] += self.gain * (a[0] + (b[0] - a[0]) * f);
            o[1] += self.gain * (a[1] + (b[1] - a[1]) * f);
        }
        self.pos += frames as f64 * step;
        let used = (self.pos as usize).min(n);
        self.buf.drain(..used);
        self.pos -= used as f64;
    }
}

/// Mixes [`AudioSource`]s into 48 kHz stereo, by time.
pub struct Mixer {
    inputs: Vec<Input>,
    /// Output frames produced so far.
    produced: u64,
}

impl Mixer {
    pub fn new(sources: Vec<Box<dyn AudioSource>>) -> Mixer {
        let inputs = sources
            .into_iter()
            .map(|src| Input { src, gain: 1.0, buf: Vec::new(), pos: 0.0, scratch: Vec::new() })
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

    /// Mixed samples (interleaved stereo) from [`Mixer::position`] up to
    /// `until` on the recording clock; a source with too little input is
    /// silent for the rest. Empty when `until` is not past the position.
    pub fn pull(&mut self, until: Duration) -> Vec<f32> {
        let target = (until.as_nanos() * u128::from(RATE) / 1_000_000_000) as u64;
        let frames = target.saturating_sub(self.produced) as usize;
        for s in &mut self.inputs {
            s.read();
        }
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

    /// Throw away the input available now (while paused).
    pub fn discard(&mut self) {
        for s in &mut self.inputs {
            s.read();
            s.buf.clear();
            s.pos = 0.0;
        }
    }
}

#[cfg(test)]
mod tests {
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

    fn sine(rate: u32, hz: f64, secs: f64) -> Vec<f32> {
        let n = (rate as f64 * secs) as usize;
        (0..n).map(|i| (0.5 * (2.0 * std::f64::consts::PI * hz * i as f64 / rate as f64).sin()) as f32).collect()
    }

    #[test]
    fn resamples_44k1_to_48k() {
        let mut m = Mixer::new(vec![fake(44_100, 1, sine(44_100, 1000.0, 1.1))]);
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
        let mut whole = Mixer::new(vec![fake(44_100, 1, src.clone())]);
        let one = whole.pull(Duration::from_millis(500));
        let chunk = 441; // 10 ms per read
        let mut parts = Mixer::new(vec![Box::new(Fake { rate: 44_100, ch: 1, data: src, at: 0, chunk })]);
        // Feed ahead of the pulls (as the session's audio lag does).
        let mut got = Vec::new();
        for ms in (10u64..=500).step_by(10) {
            parts.feed();
            got.extend(parts.pull(Duration::from_millis(ms - 10)));
        }
        got.extend(parts.pull(Duration::from_millis(500)));
        assert_eq!(got.len(), one.len());
        let worst = got.iter().zip(&one).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(worst < 1e-4, "{worst}");
    }

    impl Mixer {
        /// Read input without producing output.
        fn feed(&mut self) {
            for s in &mut self.inputs {
                s.read();
            }
        }
    }

    #[test]
    fn sums_with_gain_and_clamps() {
        let a = fake(48_000, 2, [0.75, -0.75].repeat(480));
        let b = fake(48_000, 2, [0.5, -0.1].repeat(480));
        let mut m = Mixer::new(vec![a, b]);
        let out = m.pull(Duration::from_millis(10));
        assert_eq!(out.len(), 960);
        assert!(out.as_chunks::<2>().0.iter().all(|f| f[0] == 1.0 && (f[1] + 0.85).abs() < 1e-6), "{:?}", &out[..4]);
        let a = fake(48_000, 2, [0.75, -0.75].repeat(480));
        let b = fake(48_000, 2, [0.5, -0.5].repeat(480));
        let mut m = Mixer::new(vec![a, b]);
        m.set_gain(0, 0.5);
        m.set_gain(1, -2.0);
        let out = m.pull(Duration::from_millis(10));
        assert!(out.as_chunks::<2>().0.iter().all(|f| (f[0] + 0.625).abs() < 1e-6 && (f[1] - 0.625).abs() < 1e-6));
    }

    #[test]
    fn mono_to_stereo_and_extra_channels() {
        let mut m = Mixer::new(vec![fake(48_000, 1, vec![0.25; 480])]);
        assert!(m.pull(Duration::from_millis(10)).iter().all(|&v| v == 0.25));
        // 4 channels: the first two are kept.
        let quad = [0.1, 0.2, 0.9, 0.9].repeat(480);
        let mut m = Mixer::new(vec![fake(48_000, 4, quad)]);
        let out = m.pull(Duration::from_millis(10));
        assert!(out.as_chunks::<2>().0.iter().all(|f| *f == [0.1, 0.2]));
    }

    #[test]
    fn underrun_is_silence_and_input_stays_bounded() {
        let mut m = Mixer::new(vec![fake(48_000, 1, vec![0.5; 240])]);
        let out = m.pull(Duration::from_millis(10));
        assert_eq!(out.len(), 960);
        assert!(out[..480].iter().all(|&v| v == 0.5) && out[480..].iter().all(|&v| v == 0.0));
        // No sources: silence of the right length.
        assert_eq!(Mixer::new(Vec::new()).pull(Duration::from_millis(5)), vec![0.0; 480]);
        // A source far ahead of the clock keeps at most a second of input.
        let mut m = Mixer::new(vec![fake(48_000, 1, vec![0.1; 48_000 * 5])]);
        m.feed();
        assert!(m.inputs[0].buf.len() <= 48_000);
        // Discarding (pause) drops what arrived.
        m.discard();
        assert!(m.inputs[0].buf.is_empty());
    }
}
