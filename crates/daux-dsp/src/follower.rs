//! Peak envelope follower for meters and simple dynamics.

use crate::denormal::flush_denormal_f64;
use crate::range;

/// One-pole coefficient for a time constant of `time_ms` milliseconds.
///
/// `exp(-1 / (t · fs))` — the fraction of the envelope that survives one sample.
/// `time_ms == 0` gives `exp(-inf) == 0`, i.e. an instantaneous jump, so a zero
/// attack needs no special case. Negative and `NaN` times clamp to zero via
/// `f64::max`, which returns the other operand when one side is `NaN`, and an
/// infinite time gives `exp(-0) == 1`, i.e. an envelope that never moves —
/// degenerate, but finite and defined.
fn time_coeff(sample_rate: f64, time_ms: f64) -> f64 {
    let samples = time_ms.max(0.0) * 0.001 * range::sample_rate(sample_rate);
    (-1.0 / samples).exp()
}

/// A peak follower with independent attack and release time constants.
///
/// This is what a level meter and a simple compressor sidechain are made of:
/// rectify, then smooth asymmetrically — fast up so transients are not missed,
/// slow down so the reading is legible.
///
/// The time constants are exponential, in the usual convention: after
/// `release_ms` of silence the envelope has fallen to `1/e` (≈ −8.7 dB) of
/// where it started, not to zero.
///
/// ```
/// # use daux_dsp::PeakFollower;
/// // Instant attack, 300 ms release — a typical peak meter.
/// let mut meter = PeakFollower::new(48_000.0, 0.0, 300.0);
/// assert_eq!(meter.process(-0.8), 0.8); // rectified, and caught immediately
/// assert!(meter.process(0.0) < 0.8);    // and it starts to fall
/// ```
#[derive(Clone, Copy, Debug)]
pub struct PeakFollower {
    /// Survival fraction per sample while rising.
    attack: f64,
    /// Survival fraction per sample while falling.
    release: f64,
    /// Current envelope, always non-negative for non-`NaN` input.
    env: f64,
}

impl PeakFollower {
    /// Creates a cleared follower. [any-thread]
    ///
    /// `attack_ms` of `0.0` means "catch every peak instantly", which is the
    /// right setting for a peak meter. Evaluates two exponentials, so this
    /// belongs in `prepare` or at block rate.
    #[must_use]
    pub fn new(sample_rate: f64, attack_ms: f64, release_ms: f64) -> Self {
        Self {
            attack: time_coeff(sample_rate, attack_ms),
            release: time_coeff(sample_rate, release_ms),
            env: 0.0,
        }
    }

    /// Retunes the time constants, keeping the current envelope.
    /// [audio-thread]
    ///
    /// Evaluates two exponentials: block rate, not sample rate.
    pub fn set_times(&mut self, sample_rate: f64, attack_ms: f64, release_ms: f64) {
        self.attack = time_coeff(sample_rate, attack_ms);
        self.release = time_coeff(sample_rate, release_ms);
    }

    /// Feeds one sample and returns the updated envelope. [audio-thread]
    ///
    /// The input is rectified, so the envelope is always non-negative. A `NaN`
    /// input poisons the envelope until [`reset`](Self::reset) — this path
    /// deliberately does not test for it, because a branch per sample is a real
    /// cost and `NaN` in the audio stream is already a bug upstream.
    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let rectified = f64::from(x.abs());
        let coeff = if rectified > self.env {
            self.attack
        } else {
            self.release
        };
        // env = coeff·env + (1 - coeff)·rectified, rearranged to save a multiply.
        self.env = flush_denormal_f64(rectified + coeff * (self.env - rectified));
        self.env as f32
    }

    /// Feeds a whole block and returns the envelope after the last sample.
    /// [audio-thread]
    ///
    /// The block is read only — a meter observes the signal, it does not change
    /// it. An empty slice leaves the envelope untouched and returns it.
    #[inline]
    pub fn process_block(&mut self, buf: &[f32]) -> f32 {
        let (attack, release) = (self.attack, self.release);
        let mut env = self.env;
        for &x in buf {
            let rectified = f64::from(x.abs());
            let coeff = if rectified > env { attack } else { release };
            env = flush_denormal_f64(rectified + coeff * (env - rectified));
        }
        self.env = env;
        env as f32
    }

    /// The current envelope without advancing it. [audio-thread]
    #[must_use]
    pub fn value(&self) -> f32 {
        self.env as f32
    }

    /// Clears the envelope to zero. [audio-thread]
    pub const fn reset(&mut self) {
        self.env = 0.0;
    }

    /// Sets the envelope directly, e.g. to seed a meter after a preset load.
    /// [audio-thread]
    ///
    /// The magnitude is used, keeping the envelope non-negative.
    pub fn reset_to(&mut self, value: f32) {
        self.env = f64::from(value.abs());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 48_000.0;

    #[test]
    fn zero_attack_catches_a_peak_on_the_first_sample() {
        let mut f = PeakFollower::new(SR, 0.0, 100.0);
        assert_eq!(f.process(0.75), 0.75);
        assert_eq!(f.process(-0.9), 0.9);
    }

    #[test]
    fn output_is_rectified() {
        let mut f = PeakFollower::new(SR, 0.0, 1_000.0);
        assert_eq!(f.process(-0.5), 0.5);
        f.reset();
        for x in [-1.0_f32, -0.3, -0.7] {
            assert!(f.process(x) >= 0.0);
        }
    }

    #[test]
    fn release_falls_to_one_over_e_after_the_release_time() {
        let release_ms = 100.0;
        let mut f = PeakFollower::new(SR, 0.0, release_ms);
        f.process(1.0);
        let n = (release_ms * 0.001 * SR) as usize;
        let mut y = 0.0_f32;
        for _ in 0..n {
            y = f.process(0.0);
        }
        let expected = (-1.0_f64).exp();
        assert!(
            (f64::from(y) - expected).abs() < 1.0e-3,
            "got {y}, expected {expected}"
        );
    }

    #[test]
    fn attack_rises_to_one_minus_one_over_e_after_the_attack_time() {
        let attack_ms = 50.0;
        let mut f = PeakFollower::new(SR, attack_ms, 1_000.0);
        let n = (attack_ms * 0.001 * SR) as usize;
        let mut y = 0.0_f32;
        for _ in 0..n {
            y = f.process(1.0);
        }
        let expected = 1.0 - (-1.0_f64).exp();
        assert!(
            (f64::from(y) - expected).abs() < 1.0e-3,
            "got {y}, expected {expected}"
        );
    }

    #[test]
    fn envelope_never_exceeds_the_input_peak() {
        let mut f = PeakFollower::new(SR, 1.0, 200.0);
        for i in 0..48_000 {
            let x = (i as f32 * 0.017).sin() * 0.6;
            let env = f.process(x);
            assert!(env <= 0.6 + 1.0e-6, "envelope {env} overshot");
        }
    }

    #[test]
    fn process_block_matches_per_sample() {
        let mut a = PeakFollower::new(SR, 5.0, 250.0);
        let mut b = a;
        let input: Vec<f32> = (0..999).map(|i| (i as f32 * 0.11).sin() * 0.8).collect();
        let mut last = 0.0_f32;
        for &x in &input {
            last = a.process(x);
        }
        assert_eq!(last, b.process_block(&input));
        assert_eq!(a.value(), b.value());
    }

    #[test]
    fn empty_block_leaves_the_envelope_alone() {
        let mut f = PeakFollower::new(SR, 5.0, 250.0);
        f.process(0.5);
        let before = f.value();
        assert_eq!(f.process_block(&[]), before);
        assert_eq!(f.value(), before);
    }

    #[test]
    fn fresh_follower_reads_zero() {
        let f = PeakFollower::new(SR, 5.0, 250.0);
        assert_eq!(f.value(), 0.0);
        assert_eq!(PeakFollower::new(SR, 0.0, 0.0).value(), 0.0);
    }

    #[test]
    fn zero_release_tracks_the_signal_exactly() {
        let mut f = PeakFollower::new(SR, 0.0, 0.0);
        for x in [0.9_f32, 0.1, 0.5, -0.25, 0.0] {
            assert_eq!(f.process(x), x.abs());
        }
    }

    #[test]
    fn reset_and_reset_to() {
        let mut f = PeakFollower::new(SR, 0.0, 500.0);
        f.process(1.0);
        f.reset();
        assert_eq!(f.value(), 0.0);
        f.reset_to(-0.4);
        assert!((f.value() - 0.4).abs() < 1.0e-7, "reset_to must rectify");
    }

    #[test]
    fn set_times_keeps_the_envelope() {
        let mut f = PeakFollower::new(SR, 0.0, 500.0);
        f.process(0.6);
        let env = f.value();
        f.set_times(SR, 10.0, 20.0);
        assert_eq!(f.value(), env);
    }

    #[test]
    fn longer_release_decays_more_slowly() {
        let mut fast = PeakFollower::new(SR, 0.0, 50.0);
        let mut slow = PeakFollower::new(SR, 0.0, 500.0);
        fast.process(1.0);
        slow.process(1.0);
        for _ in 0..2_400 {
            fast.process(0.0);
            slow.process(0.0);
        }
        assert!(slow.value() > fast.value());
    }

    #[test]
    fn hostile_times_and_rates_stay_finite() {
        let hostile: &[(f64, f64, f64)] = &[
            (SR, -10.0, -10.0),
            (SR, f64::NAN, f64::NAN),
            (SR, f64::INFINITY, f64::INFINITY),
            (0.0, 10.0, 10.0),
            (-SR, 10.0, 10.0),
            (f64::NAN, 10.0, 10.0),
            (f64::INFINITY, 10.0, 10.0),
            (f64::INFINITY, f64::INFINITY, f64::INFINITY),
            (f64::NAN, f64::NAN, f64::NAN),
        ];
        for &(sr, a, r) in hostile {
            let mut f = PeakFollower::new(sr, a, r);
            assert!(
                f.attack.is_finite() && (0.0..=1.0).contains(&f.attack),
                "attack {}",
                f.attack
            );
            assert!(
                f.release.is_finite() && (0.0..=1.0).contains(&f.release),
                "release {}",
                f.release
            );
            for i in 0..1_000 {
                assert!(f.process((i as f32 * 0.1).sin()).is_finite());
            }
        }
    }

    #[test]
    fn decaying_envelope_is_flushed_instead_of_going_subnormal() {
        let mut f = PeakFollower::new(SR, 0.0, 10.0);
        f.process(1.0);
        for _ in 0..1_000_000 {
            f.process(0.0);
        }
        assert_eq!(f.value(), 0.0);
    }
}
