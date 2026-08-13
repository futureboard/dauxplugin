//! First-order sections: a one-pole low/high-pass and a DC blocker.
//!
//! Both are recursive, so both flush their state through
//! [`flush_denormal_f64`](crate::flush_denormal_f64) and both keep that state
//! in `f64`. The whole point of these structures is to work at cutoffs of a few
//! hertz against sample rates of up to 768 kHz — ratios where `f32` state loses
//! most of its mantissa to the `1 - a1` cancellation.

use crate::denormal::flush_denormal_f64;
use crate::range;

/// The pole of a first-order lag, `exp(-2π·fc/fs)`.
///
/// [`range::omega`] guarantees the exponent is finite and strictly negative, so
/// the result always lands strictly inside `(0, 1)`: never `1.0` (which would
/// freeze the filter), never `0.0` (which would defeat it), never `NaN`.
fn pole(sample_rate: f64, cutoff_hz: f64) -> f64 {
    (-range::omega(sample_rate, cutoff_hz)).exp()
}

/// A one-pole (6 dB/octave) low-pass or high-pass.
///
/// This is the workhorse for control-rate smoothing, envelope shaping and
/// gentle tone controls: two multiplies and an add per sample, unconditionally
/// stable for any cutoff, and no resonance to blow up.
///
/// The high-pass is formed as `x - lowpass(x)`, which is exactly complementary:
/// the two outputs sum back to the input, sample for sample.
///
/// ```
/// # use daux_dsp::OnePole;
/// let mut lp = OnePole::lowpass(48_000.0, 500.0);
/// let mut block = [1.0_f32; 4096];
/// lp.process_block(&mut block);
/// assert!((block[4095] - 1.0).abs() < 1.0e-5); // unity at DC
/// ```
#[derive(Clone, Copy, Debug)]
pub struct OnePole {
    /// Feed-forward gain, `1 - a1`.
    b0: f64,
    /// Pole position, `exp(-2π·fc/fs)`.
    a1: f64,
    /// The single state word: the low-pass output.
    z1: f64,
    /// When true, `process` returns `x - z1` instead of `z1`.
    highpass: bool,
}

impl Default for OnePole {
    /// A cleared low-pass at a tenth of Nyquist for a 48 kHz stream — a sane
    /// value, but you should always call [`OnePole::set_lowpass`] or
    /// [`OnePole::set_highpass`] with the real sample rate in `prepare`.
    fn default() -> Self {
        Self::lowpass(48_000.0, 2_400.0)
    }
}

impl OnePole {
    /// Creates a cleared one-pole low-pass with its −3 dB point at `cutoff_hz`.
    /// [any-thread]
    ///
    /// Evaluates one exponential; call it per block at most, never per sample.
    #[must_use]
    pub fn lowpass(sample_rate: f64, cutoff_hz: f64) -> Self {
        let a1 = pole(sample_rate, cutoff_hz);
        Self {
            b0: 1.0 - a1,
            a1,
            z1: 0.0,
            highpass: false,
        }
    }

    /// Creates a cleared one-pole high-pass, the exact complement of
    /// [`OnePole::lowpass`] at the same cutoff. [any-thread]
    #[must_use]
    pub fn highpass(sample_rate: f64, cutoff_hz: f64) -> Self {
        let mut f = Self::lowpass(sample_rate, cutoff_hz);
        f.highpass = true;
        f
    }

    /// Retunes this filter as a low-pass, keeping its state. [audio-thread]
    ///
    /// Allocation-free and lock-free, but it evaluates an exponential: block
    /// rate, not sample rate.
    pub fn set_lowpass(&mut self, sample_rate: f64, cutoff_hz: f64) {
        self.a1 = pole(sample_rate, cutoff_hz);
        self.b0 = 1.0 - self.a1;
        self.highpass = false;
    }

    /// Retunes this filter as a high-pass, keeping its state. [audio-thread]
    pub fn set_highpass(&mut self, sample_rate: f64, cutoff_hz: f64) {
        self.set_lowpass(sample_rate, cutoff_hz);
        self.highpass = true;
    }

    /// True when this instance is configured as a high-pass. [any-thread]
    #[must_use]
    pub const fn is_highpass(&self) -> bool {
        self.highpass
    }

    /// Processes one sample. [audio-thread]
    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let x = f64::from(x);
        self.z1 = flush_denormal_f64(self.b0 * x + self.a1 * self.z1);
        if self.highpass {
            (x - self.z1) as f32
        } else {
            self.z1 as f32
        }
    }

    /// Processes a block in place. [audio-thread]
    ///
    /// An empty slice is a no-op. The low-pass/high-pass choice is hoisted out
    /// of the loop, so there is no per-sample branch.
    #[inline]
    pub fn process_block(&mut self, buf: &mut [f32]) {
        let (b0, a1) = (self.b0, self.a1);
        let mut z1 = self.z1;
        if self.highpass {
            for x in buf.iter_mut() {
                let xd = f64::from(*x);
                z1 = flush_denormal_f64(b0 * xd + a1 * z1);
                *x = (xd - z1) as f32;
            }
        } else {
            for x in buf.iter_mut() {
                z1 = flush_denormal_f64(b0 * f64::from(*x) + a1 * z1);
                *x = z1 as f32;
            }
        }
        self.z1 = z1;
    }

    /// Clears the state. [audio-thread]
    pub const fn reset(&mut self) {
        self.z1 = 0.0;
    }

    /// Jumps the state straight to `value`, as if the filter had been fed that
    /// constant forever. [audio-thread]
    ///
    /// Use this when a smoother must adopt a new value without a glide — after
    /// a preset load, say.
    pub const fn reset_to(&mut self, value: f32) {
        self.z1 = value as f64;
    }

    /// The current low-pass state, i.e. the value the filter is settling on.
    /// [audio-thread]
    #[must_use]
    pub fn value(&self) -> f32 {
        self.z1 as f32
    }
}

/// A DC blocker: a one-pole/one-zero high-pass with its zero pinned exactly on
/// DC.
///
/// `y[n] = x[n] - x[n-1] + R·y[n-1]`
///
/// The zero at `z = 1` removes DC *completely* — not "mostly", the way a plain
/// one-pole high-pass does — which is what you want after a waveshaper,
/// rectifier or asymmetric saturator, where an accumulating offset silently
/// eats headroom and can thump a downstream speaker.
///
/// The pole sits just inside the zero at `R = exp(-2π·fc/fs)`, so the response
/// is flat within a fraction of a decibel a couple of octaves above `fc`.
///
/// ```
/// # use daux_dsp::DcBlocker;
/// let mut dc = DcBlocker::new(48_000.0);
/// let mut y = 0.0;
/// for _ in 0..200_000 {
///     y = dc.process(0.5); // a pure DC offset
/// }
/// assert!(y.abs() < 1.0e-4); // ...is gone
/// ```
#[derive(Clone, Copy, Debug)]
pub struct DcBlocker {
    /// Pole position, `exp(-2π·fc/fs)`.
    r: f64,
    /// `x[n-1]`.
    x1: f64,
    /// `y[n-1]`.
    y1: f64,
}

impl DcBlocker {
    /// Cutoff used by [`DcBlocker::new`], in hertz.
    ///
    /// 20 Hz is the bottom of the audible band: low enough to leave bass alone,
    /// high enough to settle in a few tens of milliseconds.
    pub const DEFAULT_CUTOFF_HZ: f64 = 20.0;

    /// Creates a cleared DC blocker at [`DcBlocker::DEFAULT_CUTOFF_HZ`].
    /// [any-thread]
    #[must_use]
    pub fn new(sample_rate: f64) -> Self {
        Self::with_cutoff(sample_rate, Self::DEFAULT_CUTOFF_HZ)
    }

    /// Creates a cleared DC blocker with an explicit cutoff. [any-thread]
    #[must_use]
    pub fn with_cutoff(sample_rate: f64, cutoff_hz: f64) -> Self {
        Self {
            r: pole(sample_rate, cutoff_hz),
            x1: 0.0,
            y1: 0.0,
        }
    }

    /// Retunes the cutoff, keeping the state. [audio-thread]
    ///
    /// Evaluates an exponential: block rate, not sample rate.
    pub fn set_cutoff(&mut self, sample_rate: f64, cutoff_hz: f64) {
        self.r = pole(sample_rate, cutoff_hz);
    }

    /// Processes one sample. [audio-thread]
    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let x = f64::from(x);
        let y = x - self.x1 + self.r * self.y1;
        self.x1 = x;
        self.y1 = flush_denormal_f64(y);
        y as f32
    }

    /// Processes a block in place. [audio-thread]
    ///
    /// An empty slice is a no-op.
    #[inline]
    pub fn process_block(&mut self, buf: &mut [f32]) {
        let r = self.r;
        let (mut x1, mut y1) = (self.x1, self.y1);
        for x in buf.iter_mut() {
            let xd = f64::from(*x);
            let y = xd - x1 + r * y1;
            x1 = xd;
            y1 = flush_denormal_f64(y);
            *x = y as f32;
        }
        self.x1 = x1;
        self.y1 = y1;
    }

    /// Clears the state. [audio-thread]
    pub const fn reset(&mut self) {
        self.x1 = 0.0;
        self.y1 = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 48_000.0;

    #[test]
    fn lowpass_has_unity_dc_gain() {
        let mut f = OnePole::lowpass(SR, 500.0);
        let mut y = 0.0_f32;
        for _ in 0..50_000 {
            y = f.process(1.0);
        }
        assert!((y - 1.0).abs() < 1.0e-5, "settled at {y}");
    }

    #[test]
    fn lowpass_rejects_nyquist() {
        let mut f = OnePole::lowpass(SR, 500.0);
        let mut y = 0.0_f32;
        for i in 0..50_000 {
            y = f.process(if i % 2 == 0 { 1.0 } else { -1.0 });
        }
        // Analytic: b0 / (1 + a1) with a1 ~ 0.9354 -> ~0.0334.
        assert!(y.abs() < 0.04, "Nyquist leakage {y}");
    }

    #[test]
    fn highpass_rejects_dc() {
        let mut f = OnePole::highpass(SR, 500.0);
        let mut y = 0.0_f32;
        for _ in 0..50_000 {
            y = f.process(1.0);
        }
        assert!(y.abs() < 1.0e-5, "DC leakage {y}");
        assert!(f.is_highpass());
    }

    #[test]
    fn highpass_and_lowpass_sum_back_to_the_input() {
        let mut lp = OnePole::lowpass(SR, 900.0);
        let mut hp = OnePole::highpass(SR, 900.0);
        for i in 0..2_000 {
            let x = (i as f32 * 0.13).sin() * 0.7;
            let sum = lp.process(x) + hp.process(x);
            assert!((sum - x).abs() < 1.0e-6, "sum {sum} != {x}");
        }
    }

    #[test]
    fn lowpass_reaches_63_percent_after_one_time_constant() {
        // fc = 1/(2*pi*tau); one tau of samples must land on 1 - 1/e.
        let fc = 100.0;
        let tau_samples = (SR / (core::f64::consts::TAU * fc)).round() as usize;
        let mut f = OnePole::lowpass(SR, fc);
        let mut y = 0.0_f32;
        for _ in 0..tau_samples {
            y = f.process(1.0);
        }
        // `tau_samples` is rounded to an integer, so allow for the sampling grid.
        let expected = 1.0 - (-1.0_f64).exp();
        assert!(
            (f64::from(y) - expected).abs() < 5.0e-3,
            "got {y}, expected {expected}"
        );
    }

    #[test]
    fn process_block_matches_per_sample_for_both_modes() {
        for highpass in [false, true] {
            let mut a = if highpass {
                OnePole::highpass(SR, 700.0)
            } else {
                OnePole::lowpass(SR, 700.0)
            };
            let mut b = a;
            let input: Vec<f32> = (0..777).map(|i| (i as f32 * 0.21).cos() * 0.9).collect();
            let per_sample: Vec<f32> = input.iter().map(|&x| a.process(x)).collect();
            let mut block = input;
            b.process_block(&mut block);
            assert_eq!(per_sample, block, "highpass = {highpass}");
        }
    }

    #[test]
    fn empty_block_is_a_no_op() {
        let mut f = OnePole::lowpass(SR, 700.0);
        f.process(1.0);
        let before = f.value();
        f.process_block(&mut []);
        assert_eq!(before, f.value());

        let mut dc = DcBlocker::new(SR);
        dc.process(1.0);
        let state = (dc.x1, dc.y1);
        dc.process_block(&mut []);
        assert_eq!(state, (dc.x1, dc.y1));
    }

    #[test]
    fn reset_and_reset_to() {
        let mut f = OnePole::lowpass(SR, 700.0);
        for _ in 0..10_000 {
            f.process(1.0);
        }
        f.reset();
        assert_eq!(f.value(), 0.0);
        f.reset_to(0.25);
        assert!((f.value() - 0.25).abs() < 1.0e-7);
        // Feeding the same constant must not move a filter already sitting on it.
        assert!((f.process(0.25) - 0.25).abs() < 1.0e-7);
    }

    #[test]
    fn setters_switch_mode_and_keep_state() {
        let mut f = OnePole::lowpass(SR, 700.0);
        for _ in 0..1_000 {
            f.process(1.0);
        }
        let state = f.value();
        f.set_highpass(SR, 200.0);
        assert!(f.is_highpass());
        assert_eq!(state, f.value());
        f.set_lowpass(SR, 200.0);
        assert!(!f.is_highpass());
        assert_eq!(state, f.value());
    }

    #[test]
    fn default_is_a_cleared_lowpass() {
        let f = OnePole::default();
        assert!(!f.is_highpass());
        assert_eq!(f.value(), 0.0);
    }

    #[test]
    fn onepole_survives_hostile_tuning() {
        let hostile: &[(f64, f64)] = &[
            (SR, 0.0),
            (SR, -1.0),
            (SR, SR),
            (SR, f64::INFINITY),
            (SR, f64::NEG_INFINITY),
            (SR, f64::NAN),
            (0.0, 100.0),
            (-1.0, 100.0),
            (f64::NAN, 100.0),
            (f64::INFINITY, 100.0),
            (f64::INFINITY, f64::INFINITY),
            (f64::NAN, f64::NAN),
            (1.0e300, 1.0e300),
        ];
        for &(sr, fc) in hostile {
            let mut f = OnePole::lowpass(sr, fc);
            assert!(
                f.a1.is_finite() && f.a1 > 0.0 && f.a1 < 1.0,
                "pole {} @ {sr}/{fc}",
                f.a1
            );
            assert!(f.b0 > 0.0 && f.b0 <= 1.0, "gain {} @ {sr}/{fc}", f.b0);
            for _ in 0..1_000 {
                assert!(f.process(0.5).is_finite());
            }
            let mut d = DcBlocker::with_cutoff(sr, fc);
            assert!(
                d.r.is_finite() && d.r > 0.0 && d.r < 1.0,
                "R {} @ {sr}/{fc}",
                d.r
            );
            for _ in 0..1_000 {
                assert!(d.process(0.5).is_finite());
            }
        }
    }

    #[test]
    fn dc_blocker_removes_a_constant_offset() {
        let mut f = DcBlocker::new(SR);
        let mut y = 0.0_f32;
        for _ in 0..200_000 {
            y = f.process(0.5);
        }
        assert!(y.abs() < 1.0e-5, "residual DC {y}");
    }

    #[test]
    fn dc_blocker_leaves_a_mid_band_sine_essentially_alone() {
        // RMS rather than peak: with 48 samples per cycle the sampling grid
        // misses the true crest by up to 0.2 %, which would swamp the effect
        // being measured. The analytic gain of a 20 Hz blocker at 1 kHz is
        // 1.0083, so 2 % is a real bound, not a rubber stamp.
        let mut f = DcBlocker::new(SR);
        let w = core::f64::consts::TAU * 1_000.0 / SR;
        let mut energy = 0.0_f64;
        let mut count = 0.0_f64;
        for n in 0..96_000 {
            let y = f.process(((w * f64::from(n)).sin() * 0.5) as f32);
            if n >= 48_000 {
                energy += f64::from(y) * f64::from(y);
                count += 1.0;
            }
        }
        let rms = (energy / count).sqrt();
        let expected = 0.5 / core::f64::consts::SQRT_2;
        assert!(
            (rms / expected - 1.0).abs() < 0.02,
            "1 kHz gain {}",
            rms / expected
        );
    }

    #[test]
    fn dc_blocker_strips_the_offset_from_an_offset_sine() {
        let mut f = DcBlocker::new(SR);
        let w = core::f64::consts::TAU * 1_000.0 / SR;
        let mut mean = 0.0_f64;
        let mut count = 0.0_f64;
        for n in 0..96_000 {
            let x = ((w * f64::from(n)).sin() * 0.3 + 0.4) as f32;
            let y = f.process(x);
            if n > 48_000 {
                mean += f64::from(y);
                count += 1.0;
            }
        }
        assert!((mean / count).abs() < 1.0e-3, "mean {}", mean / count);
    }

    #[test]
    fn dc_blocker_has_near_unity_nyquist_gain() {
        let mut f = DcBlocker::new(SR);
        let mut y = 0.0_f32;
        for i in 0..50_000 {
            y = f.process(if i % 2 == 0 { 1.0 } else { -1.0 });
        }
        // Analytic |H(-1)| = 2 / (1 + R), a whisker above unity.
        let expected = 2.0 / (1.0 + f.r);
        assert!(
            (f64::from(y.abs()) - expected).abs() < 1.0e-4,
            "got {y}, expected {expected}"
        );
    }

    #[test]
    fn dc_blocker_block_matches_per_sample() {
        let mut a = DcBlocker::new(SR);
        let mut b = a;
        let input: Vec<f32> = (0..513)
            .map(|i| (i as f32 * 0.05).sin() * 0.6 + 0.2)
            .collect();
        let per_sample: Vec<f32> = input.iter().map(|&x| a.process(x)).collect();
        let mut block = input;
        b.process_block(&mut block);
        assert_eq!(per_sample, block);
    }

    #[test]
    fn dc_blocker_reset_clears_state() {
        let mut f = DcBlocker::new(SR);
        for _ in 0..100 {
            f.process(1.0);
        }
        f.reset();
        assert_eq!((f.x1, f.y1), (0.0, 0.0));
        // First sample after a reset is the input itself.
        assert_eq!(f.process(0.25), 0.25);
    }

    #[test]
    fn recursive_state_is_flushed_rather_than_left_subnormal() {
        let mut lp = OnePole::lowpass(SR, 20.0);
        lp.process(1.0);
        for _ in 0..1_000_000 {
            lp.process(0.0);
        }
        assert_eq!(lp.value(), 0.0);

        let mut dc = DcBlocker::new(SR);
        dc.process(1.0);
        for _ in 0..1_000_000 {
            dc.process(0.0);
        }
        assert_eq!(dc.y1, 0.0);
    }
}
