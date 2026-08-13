//! Per-sample parameter ramping.

/// How a processor should ramp a parameter towards a new value.
///
/// A parameter itself never smooths — it is a shared atomic cell with no idea what the
/// sample rate is. This describes the *intent*, recorded at build time with
/// `with_smoothing`, and [`Smoother`] carries it out on the audio thread.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum Smoothing {
    /// Jump straight to the new value. The right choice for parameters that already
    /// have their own crossfade, or that only take effect between blocks.
    #[default]
    None,
    /// Constant-rate ramp that reaches the target after `ms` milliseconds.
    Linear {
        /// Ramp length in milliseconds.
        ms: f32,
    },
    /// One-pole ramp that approaches the target and arrives after `ms` milliseconds.
    ///
    /// The coefficient is chosen so the remaining error after `ms` is one millionth of
    /// the step, at which point the smoother snaps: the curve sounds exponential but
    /// still finishes in bounded time, so [`Smoother::is_smoothing`] eventually goes
    /// false and the fast path comes back.
    Exponential {
        /// Time to arrive, in milliseconds.
        ms: f32,
    },
}

impl Smoothing {
    /// `[any-thread]` Ramp length in milliseconds; `0.0` for [`Smoothing::None`].
    #[inline]
    #[must_use]
    pub fn milliseconds(self) -> f32 {
        match self {
            Self::None => 0.0,
            Self::Linear { ms } | Self::Exponential { ms } => ms,
        }
    }

    /// `[any-thread]` True when this actually ramps.
    #[inline]
    #[must_use]
    pub fn is_active(self) -> bool {
        !matches!(self, Self::None) && self.milliseconds() > 0.0
    }
}

/// Residual error, as a fraction of the step, at which an exponential ramp is
/// considered finished.
const EXPONENTIAL_SETTLE: f64 = 1e-6;

/// Ramps a value towards a target, one sample at a time.
///
/// ```
/// use daux_parameter::{Smoother, Smoothing};
///
/// let mut smoother = Smoother::new(Smoothing::Linear { ms: 1.0 });
/// smoother.prepare(48_000.0);       // 48 samples per ramp
/// smoother.reset_to(0.0);
/// smoother.set_target(1.0);
///
/// let mut block = [0.0f32; 48];
/// smoother.next_block(&mut block);
/// assert!(!smoother.is_smoothing());
/// assert_eq!(block[47], 1.0);
/// ```
///
/// # Threads
///
/// [`prepare`](Smoother::prepare) is `[main-thread]` and does the only division and
/// exponential involved. [`set_target`](Smoother::set_target),
/// [`next`](Smoother::next), [`next_block`](Smoother::next_block),
/// [`is_smoothing`](Smoother::is_smoothing) and [`reset_to`](Smoother::reset_to) are
/// `[audio-thread]`: no allocation, no lock, no `panic!`, and at most a compare and a
/// multiply-add per sample.
///
/// A `Smoother` belongs to the processor, not to the parameter — one per voice if the
/// parameter is per-voice. It is `Send` but not shared: only the thread that owns it
/// may touch it.
#[derive(Clone, Copy, Debug)]
pub struct Smoother {
    smoothing: Smoothing,
    sample_rate: f64,
    /// Ramp length in samples; `0` means "jump".
    steps_total: u32,
    /// Samples left in the current ramp; `0` means settled.
    steps_left: u32,
    current: f32,
    target: f32,
    /// Per-sample increment for a linear ramp.
    step: f32,
    /// Per-sample error fraction consumed by an exponential ramp.
    coefficient: f32,
}

impl Smoother {
    /// `[main-thread]` Builds an unprepared smoother resting at `0.0`.
    ///
    /// Until [`prepare`](Smoother::prepare) has been called there is no sample rate, so
    /// every target is reached immediately — which is exactly what an offline or
    /// unprepared context wants.
    #[must_use]
    pub fn new(smoothing: Smoothing) -> Self {
        Self {
            smoothing,
            sample_rate: 0.0,
            steps_total: 0,
            steps_left: 0,
            current: 0.0,
            target: 0.0,
            step: 0.0,
            coefficient: 1.0,
        }
    }

    /// `[main-thread]` Adopts a sample rate and recomputes the ramp constants.
    ///
    /// Allocates nothing — the smoother is a fixed-size value with no buffers — so it
    /// is safe to call from `prepare` or `activate`. Any ramp in flight is finished
    /// immediately, because a ramp computed for the old sample rate would run at the
    /// wrong speed. A non-positive or non-finite rate disables ramping.
    pub fn prepare(&mut self, sample_rate: f64) {
        self.sample_rate = if sample_rate.is_finite() && sample_rate > 0.0 {
            sample_rate
        } else {
            0.0
        };
        self.recompute();
        self.current = self.target;
        self.steps_left = 0;
    }

    /// `[main-thread]` Changes the ramp style, keeping the current value.
    pub fn set_smoothing(&mut self, smoothing: Smoothing) {
        self.smoothing = smoothing;
        self.recompute();
        self.current = self.target;
        self.steps_left = 0;
    }

    /// `[any-thread]` The configured ramp style.
    #[inline]
    #[must_use]
    pub fn smoothing(&self) -> Smoothing {
        self.smoothing
    }

    /// `[any-thread]` Ramp length in samples at the prepared sample rate.
    #[inline]
    #[must_use]
    pub fn steps(&self) -> u32 {
        self.steps_total
    }

    /// `[audio-thread]` Aims at a new value.
    ///
    /// Cheap enough to call once per block — or once per parameter event — with the
    /// value straight out of the parameter. Re-targeting mid-ramp restarts the ramp
    /// from wherever the value currently is, so there is never a click.
    ///
    /// `target` must be finite; a `NaN` target would poison the ramp for good. Debug
    /// builds assert, release builds simply store it.
    #[inline]
    pub fn set_target(&mut self, target: f32) {
        debug_assert!(target.is_finite(), "smoother targets must be finite");
        if target == self.target {
            return;
        }
        self.target = target;
        if self.steps_total == 0 {
            self.current = target;
            self.steps_left = 0;
            return;
        }
        self.steps_left = self.steps_total;
        self.step = (target - self.current) / self.steps_total as f32;
    }

    /// `[audio-thread]` Produces the next sample of the ramp.
    ///
    /// Once settled this is a single compare and a load; while ramping it is one
    /// multiply-add. It never allocates, locks or panics.
    // Deliberately not `Iterator::next`: a smoother is infinite, is driven per sample
    // from `process`, and must not pay for `Option` on the audio thread. The name is
    // fixed by `docs/architecture/crate-contracts.md`.
    #[allow(clippy::should_implement_trait)]
    #[inline]
    pub fn next(&mut self) -> f32 {
        if self.steps_left == 0 {
            return self.current;
        }
        self.steps_left -= 1;
        if self.steps_left == 0 {
            // Land exactly on the target rather than near it, so that comparisons
            // downstream (and the next `set_target`) see a clean value.
            self.current = self.target;
        } else {
            self.current = match self.smoothing {
                Smoothing::None => self.target,
                Smoothing::Linear { .. } => self.current + self.step,
                Smoothing::Exponential { .. } => {
                    self.current + self.coefficient * (self.target - self.current)
                }
            };
        }
        self.current
    }

    /// `[audio-thread]` Fills `out` with the next `out.len()` samples of the ramp.
    ///
    /// Takes the settled fast path when the value is not moving, which is the common
    /// case and costs one branch for the whole block.
    pub fn next_block(&mut self, out: &mut [f32]) {
        if self.steps_left == 0 {
            out.fill(self.current);
            return;
        }
        for sample in out.iter_mut() {
            *sample = self.next();
        }
    }

    /// `[audio-thread]` True while a ramp is in flight.
    ///
    /// Lets a processor take a cheap scalar path — `apply_gain(buf, smoother.next())` —
    /// instead of a per-sample one whenever nothing is moving.
    #[inline]
    #[must_use]
    pub fn is_smoothing(&self) -> bool {
        self.steps_left != 0
    }

    /// `[audio-thread]` Jumps to `value` and cancels any ramp.
    ///
    /// Use it in `reset`, when the transport relocates, or when a voice is stolen.
    #[inline]
    pub fn reset_to(&mut self, value: f32) {
        self.current = value;
        self.target = value;
        self.steps_left = 0;
    }

    /// `[audio-thread]` Current value without advancing the ramp.
    #[inline]
    #[must_use]
    pub fn current(&self) -> f32 {
        self.current
    }

    /// `[audio-thread]` Value the ramp is heading for.
    #[inline]
    #[must_use]
    pub fn target(&self) -> f32 {
        self.target
    }

    /// Recomputes `steps_total` and the exponential coefficient. Main thread only:
    /// this is where the division and the `exp` live.
    fn recompute(&mut self) {
        let ms = f64::from(self.smoothing.milliseconds());
        let steps = if self.sample_rate > 0.0 && ms.is_finite() && ms > 0.0 {
            // `as u32` saturates, so an absurd ramp length cannot wrap around to zero.
            (ms * 0.001 * self.sample_rate).round() as u32
        } else {
            0
        };
        self.steps_total = match self.smoothing {
            Smoothing::None => 0,
            Smoothing::Linear { .. } | Smoothing::Exponential { .. } => steps,
        };
        self.coefficient = if self.steps_total > 1 {
            // Solve `(1 - c)^n = settle` for c, so the ramp is done when it says it is.
            (1.0 - (EXPONENTIAL_SETTLE.ln() / f64::from(self.steps_total)).exp()) as f32
        } else {
            1.0
        };
        self.step = 0.0;
    }
}

impl Default for Smoother {
    /// An unprepared smoother that does not ramp.
    fn default() -> Self {
        Self::new(Smoothing::None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 48_000.0;

    #[test]
    fn smoothing_describes_itself() {
        assert_eq!(Smoothing::default(), Smoothing::None);
        assert_eq!(Smoothing::None.milliseconds(), 0.0);
        assert_eq!(Smoothing::Linear { ms: 20.0 }.milliseconds(), 20.0);
        assert_eq!(Smoothing::Exponential { ms: 5.0 }.milliseconds(), 5.0);
        assert!(!Smoothing::None.is_active());
        assert!(Smoothing::Linear { ms: 1.0 }.is_active());
        assert!(!Smoothing::Linear { ms: 0.0 }.is_active());
    }

    #[test]
    fn none_jumps_immediately() {
        let mut s = Smoother::new(Smoothing::None);
        s.prepare(SR);
        s.reset_to(0.0);
        s.set_target(1.0);
        assert!(!s.is_smoothing());
        assert_eq!(s.next(), 1.0);
        assert_eq!(s.current(), 1.0);
        assert_eq!(s.target(), 1.0);
        assert_eq!(s.steps(), 0);
    }

    #[test]
    fn an_unprepared_smoother_jumps() {
        let mut s = Smoother::new(Smoothing::Linear { ms: 50.0 });
        s.reset_to(0.0);
        s.set_target(1.0);
        assert!(!s.is_smoothing());
        assert_eq!(s.next(), 1.0);
    }

    #[test]
    fn a_non_positive_sample_rate_disables_ramping() {
        for rate in [0.0, -48_000.0, f64::NAN, f64::INFINITY] {
            let mut s = Smoother::new(Smoothing::Linear { ms: 10.0 });
            s.prepare(rate);
            s.set_target(1.0);
            assert_eq!(s.steps(), 0, "rate {rate}");
            assert!(!s.is_smoothing());
            assert_eq!(s.next(), 1.0);
        }
    }

    #[test]
    fn linear_is_monotonic_and_lands_exactly() {
        let mut s = Smoother::new(Smoothing::Linear { ms: 1.0 });
        s.prepare(SR);
        assert_eq!(s.steps(), 48);
        s.reset_to(-1.0);
        s.set_target(1.0);
        assert!(s.is_smoothing());

        let mut previous = -1.0f32;
        for i in 0..48 {
            let v = s.next();
            assert!(v >= previous, "step {i} went backwards: {v} < {previous}");
            assert!((-1.0..=1.0).contains(&v), "step {i} overshot: {v}");
            previous = v;
        }
        assert_eq!(previous, 1.0);
        assert!(!s.is_smoothing());
        // Settled: further calls stay put.
        assert_eq!(s.next(), 1.0);
        assert_eq!(s.next(), 1.0);
    }

    #[test]
    fn linear_ramps_downwards_too() {
        let mut s = Smoother::new(Smoothing::Linear { ms: 1.0 });
        s.prepare(SR);
        s.reset_to(1.0);
        s.set_target(0.0);
        let mut previous = 1.0f32;
        for _ in 0..48 {
            let v = s.next();
            assert!(v <= previous);
            previous = v;
        }
        assert_eq!(previous, 0.0);
        assert!(!s.is_smoothing());
    }

    #[test]
    fn linear_is_evenly_spaced() {
        let mut s = Smoother::new(Smoothing::Linear { ms: 1.0 });
        s.prepare(SR);
        s.reset_to(0.0);
        s.set_target(48.0);
        // 48 samples, so one unit per sample until the final snap.
        for expected in 1..48 {
            let v = s.next();
            assert!(
                (v - expected as f32).abs() < 1e-3,
                "expected {expected}, got {v}"
            );
        }
        assert_eq!(s.next(), 48.0);
    }

    #[test]
    fn exponential_converges_monotonically_and_finishes() {
        let mut s = Smoother::new(Smoothing::Exponential { ms: 20.0 });
        s.prepare(SR);
        assert_eq!(s.steps(), 960);
        s.reset_to(0.0);
        s.set_target(1.0);

        let mut previous = 0.0f32;
        let mut settled_at = None;
        for i in 0..960 {
            let v = s.next();
            assert!(v >= previous, "step {i} went backwards");
            assert!(v <= 1.0, "step {i} overshot: {v}");
            previous = v;
            if !s.is_smoothing() && settled_at.is_none() {
                settled_at = Some(i);
            }
        }
        assert_eq!(
            settled_at,
            Some(959),
            "the ramp must finish exactly on time"
        );
        assert_eq!(previous, 1.0);
        assert!(!s.is_smoothing());

        // The curve must actually be exponential: most of the distance is covered
        // early, unlike a linear ramp.
        let mut s2 = Smoother::new(Smoothing::Exponential { ms: 20.0 });
        s2.prepare(SR);
        s2.reset_to(0.0);
        s2.set_target(1.0);
        for _ in 0..480 {
            s2.next();
        }
        assert!(
            s2.current() > 0.9,
            "half way in time should be most of the way in value"
        );
    }

    #[test]
    fn exponential_ramps_downwards_too() {
        let mut s = Smoother::new(Smoothing::Exponential { ms: 5.0 });
        s.prepare(SR);
        s.reset_to(4.0);
        s.set_target(-2.0);
        let mut previous = 4.0f32;
        while s.is_smoothing() {
            let v = s.next();
            assert!(v <= previous, "{v} > {previous}");
            assert!(v >= -2.0);
            previous = v;
        }
        assert_eq!(previous, -2.0);
    }

    #[test]
    fn retargeting_mid_ramp_restarts_from_the_current_value() {
        let mut s = Smoother::new(Smoothing::Linear { ms: 1.0 });
        s.prepare(SR);
        s.reset_to(0.0);
        s.set_target(1.0);
        for _ in 0..24 {
            s.next();
        }
        let midpoint = s.current();
        assert!(midpoint > 0.4 && midpoint < 0.6, "midpoint {midpoint}");

        s.set_target(0.0);
        assert!(s.is_smoothing());
        assert_eq!(s.current(), midpoint, "re-targeting must not jump");
        let next = s.next();
        assert!(next < midpoint, "the ramp must turn around");
        while s.is_smoothing() {
            s.next();
        }
        assert_eq!(s.current(), 0.0);
    }

    #[test]
    fn setting_the_same_target_does_not_restart_the_ramp() {
        let mut s = Smoother::new(Smoothing::Linear { ms: 1.0 });
        s.prepare(SR);
        s.reset_to(0.0);
        s.set_target(1.0);
        for _ in 0..24 {
            s.next();
        }
        let before = s.current();
        s.set_target(1.0);
        assert_eq!(s.current(), before);
        // Still 24 samples left, not 48.
        for _ in 0..24 {
            s.next();
        }
        assert!(!s.is_smoothing());
        assert_eq!(s.current(), 1.0);
    }

    #[test]
    fn next_block_matches_next() {
        let mut a = Smoother::new(Smoothing::Exponential { ms: 1.0 });
        a.prepare(SR);
        a.reset_to(0.0);
        a.set_target(1.0);
        let mut b = a;

        let mut block = [0.0f32; 64];
        a.next_block(&mut block);
        for (i, expected) in block.iter().enumerate() {
            let got = b.next();
            assert_eq!(got, *expected, "sample {i}");
        }
        assert_eq!(a.is_smoothing(), b.is_smoothing());
    }

    #[test]
    fn next_block_takes_the_settled_fast_path() {
        let mut s = Smoother::new(Smoothing::Linear { ms: 1.0 });
        s.prepare(SR);
        s.reset_to(0.25);
        let mut block = [0.0f32; 8];
        s.next_block(&mut block);
        assert_eq!(block, [0.25f32; 8]);
        assert!(!s.is_smoothing());
    }

    #[test]
    fn next_block_handles_an_empty_slice() {
        let mut s = Smoother::new(Smoothing::Linear { ms: 1.0 });
        s.prepare(SR);
        s.reset_to(0.0);
        s.set_target(1.0);
        let mut empty: [f32; 0] = [];
        s.next_block(&mut empty);
        assert!(s.is_smoothing(), "an empty block must not advance the ramp");
        assert_eq!(s.current(), 0.0);
    }

    #[test]
    fn next_block_spanning_the_end_of_a_ramp_is_continuous() {
        let mut s = Smoother::new(Smoothing::Linear { ms: 1.0 });
        s.prepare(SR);
        s.reset_to(0.0);
        s.set_target(1.0);
        let mut block = [0.0f32; 64];
        s.next_block(&mut block);
        assert_eq!(block[47], 1.0);
        assert_eq!(block[63], 1.0, "the tail of the block holds the target");
        assert!(!s.is_smoothing());
    }

    #[test]
    fn a_one_sample_ramp_still_lands() {
        let mut s = Smoother::new(Smoothing::Linear { ms: 1.0 });
        s.prepare(1_000.0); // 1 sample per millisecond
        assert_eq!(s.steps(), 1);
        s.reset_to(0.0);
        s.set_target(1.0);
        assert!(s.is_smoothing());
        assert_eq!(s.next(), 1.0);
        assert!(!s.is_smoothing());
    }

    #[test]
    fn a_sub_sample_ramp_degenerates_to_a_jump() {
        let mut s = Smoother::new(Smoothing::Linear { ms: 0.001 });
        s.prepare(SR); // 0.048 samples, rounds to zero
        assert_eq!(s.steps(), 0);
        s.reset_to(0.0);
        s.set_target(1.0);
        assert!(!s.is_smoothing());
        assert_eq!(s.next(), 1.0);
    }

    #[test]
    fn an_absurd_ramp_length_saturates_instead_of_wrapping() {
        let mut s = Smoother::new(Smoothing::Linear { ms: f32::MAX });
        s.prepare(SR);
        assert_eq!(s.steps(), u32::MAX);
        s.reset_to(0.0);
        s.set_target(1.0);
        assert!(s.is_smoothing());
        assert!(s.next() >= 0.0);
    }

    #[test]
    fn preparing_again_settles_the_ramp() {
        let mut s = Smoother::new(Smoothing::Linear { ms: 10.0 });
        s.prepare(SR);
        s.reset_to(0.0);
        s.set_target(1.0);
        for _ in 0..10 {
            s.next();
        }
        s.prepare(96_000.0);
        assert!(!s.is_smoothing());
        assert_eq!(
            s.current(),
            1.0,
            "a rate change lands on the target, not somewhere"
        );
        assert_eq!(s.steps(), 960);
    }

    #[test]
    fn changing_the_style_keeps_the_value() {
        let mut s = Smoother::new(Smoothing::Linear { ms: 1.0 });
        s.prepare(SR);
        s.reset_to(0.5);
        s.set_smoothing(Smoothing::Exponential { ms: 2.0 });
        assert_eq!(s.smoothing(), Smoothing::Exponential { ms: 2.0 });
        assert_eq!(s.current(), 0.5);
        assert_eq!(s.steps(), 96);
        assert!(!s.is_smoothing());
    }

    #[test]
    fn reset_to_cancels_a_ramp() {
        let mut s = Smoother::new(Smoothing::Exponential { ms: 10.0 });
        s.prepare(SR);
        s.reset_to(0.0);
        s.set_target(1.0);
        assert!(s.is_smoothing());
        s.reset_to(-3.0);
        assert!(!s.is_smoothing());
        assert_eq!(s.next(), -3.0);
        assert_eq!(s.target(), -3.0);
    }

    #[test]
    fn default_is_a_settled_non_smoothing_smoother() {
        let mut s = Smoother::default();
        assert_eq!(s.smoothing(), Smoothing::None);
        assert!(!s.is_smoothing());
        assert_eq!(s.next(), 0.0);
    }

    #[test]
    fn is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<Smoother>();
    }
}
