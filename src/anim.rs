//! Tiny ease-out tweens sampled at frame time (spec "Motion", all ≤ 150 ms).

use std::time::{Duration, Instant};

/// Cubic ease-out on 0..=1.
pub fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

#[derive(Clone, Copy, Debug)]
pub struct Tween {
    from: f32,
    to: f32,
    start: Instant,
    dur: Duration,
}

impl Tween {
    pub fn new(v: f32, now: Instant) -> Self {
        Tween { from: v, to: v, start: now, dur: Duration::ZERO }
    }

    pub fn value(&self, now: Instant) -> f32 {
        if self.dur.is_zero() {
            return self.to;
        }
        let t = now.saturating_duration_since(self.start).as_secs_f32() / self.dur.as_secs_f32();
        self.from + (self.to - self.from) * ease_out(t)
    }

    #[cfg(test)] // only the preview test settles tweens this way
    pub fn target(&self) -> f32 {
        self.to
    }

    /// Animate toward `to` over `ms`, starting from the current value. A
    /// repeated call with the same target keeps the running animation.
    pub fn set(&mut self, to: f32, ms: u64, now: Instant) {
        if (to - self.to).abs() < f32::EPSILON {
            return;
        }
        self.from = self.value(now);
        self.to = to;
        self.start = now;
        self.dur = Duration::from_millis(ms);
    }

    pub fn snap(&mut self, v: f32) {
        self.from = v;
        self.to = v;
        self.dur = Duration::ZERO;
    }

    pub fn active(&self, now: Instant) -> bool {
        !self.dur.is_zero() && now < self.start + self.dur
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn settled_value_is_constant() {
        let now = Instant::now();
        let t = Tween::new(0.3, now);
        assert_eq!(t.value(now + ms(500)), 0.3);
        assert!(!t.active(now));
    }

    #[test]
    fn eases_out_to_target() {
        let now = Instant::now();
        let mut t = Tween::new(0.0, now);
        t.set(1.0, 100, now);
        assert!(t.active(now));
        let mid = t.value(now + ms(50));
        assert!((mid - 0.875).abs() < 1e-3, "ease-out cubic at t=0.5: {mid}");
        assert_eq!(t.value(now + ms(100)), 1.0);
        assert!(!t.active(now + ms(100)));
    }

    #[test]
    fn retarget_starts_from_current_value() {
        let now = Instant::now();
        let mut t = Tween::new(0.0, now);
        t.set(1.0, 100, now);
        let mid = t.value(now + ms(50));
        t.set(0.0, 100, now + ms(50));
        assert!((t.value(now + ms(50)) - mid).abs() < 1e-6);
        assert_eq!(t.target(), 0.0);
    }

    #[test]
    fn same_target_does_not_restart() {
        let now = Instant::now();
        let mut t = Tween::new(0.0, now);
        t.set(1.0, 100, now);
        t.set(1.0, 100, now + ms(90));
        assert!(!t.active(now + ms(100)));
    }

    #[test]
    fn snap_jumps() {
        let now = Instant::now();
        let mut t = Tween::new(0.0, now);
        t.set(1.0, 100, now);
        t.snap(0.0);
        assert_eq!(t.value(now + ms(10)), 0.0);
        assert!(!t.active(now));
    }
}
