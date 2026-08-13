//! Second-order IIR sections using the RBJ audio-EQ-cookbook coefficient set,
//! evaluated in transposed direct form II.
//!
//! Transposed direct form II is the right choice for floating-point audio: it
//! needs two state words instead of four, and its state holds partial sums of
//! the *output* rather than a delayed copy of the input, which keeps rounding
//! noise far below the signal even at low cutoff ratios.
//!
//! Coefficients are computed in `f64` and *stored* in `f64`, and the recursion
//! runs in `f64` with `f32` in and out. The cost is one convert at each end of
//! `process`; the benefit is that the catastrophic cancellation which makes
//! `f32` biquads noisy below roughly `0.001 * sample_rate` simply does not
//! occur. A biquad is a handful of instructions either way — precision is worth
//! more here than register pressure.

use crate::denormal::flush_denormal_f64;
use crate::range;

/// Q is clamped to this range. Zero would divide by zero; unbounded Q parks the
/// poles on the unit circle and the section rings forever.
const MIN_Q: f64 = 1.0e-4;
/// Upper end of the Q clamp.
const MAX_Q: f64 = 1.0e4;
/// Shelf and peak gains are clamped to ±this, in decibels.
const MAX_ABS_GAIN_DB: f64 = 120.0;

/// The `cos(w0)` and `alpha` terms shared by every cookbook shape.
struct Rbj {
    cos_w0: f64,
    alpha: f64,
}

/// Clamps the tuning into a range where the cookbook formulas are well
/// conditioned (see [`crate::range`]), then evaluates the shared trigonometric
/// terms.
fn rbj_terms(sample_rate: f64, freq_hz: f64, q: f64) -> Rbj {
    let w0 = range::omega(sample_rate, freq_hz);
    let q = range::bounded(q, MIN_Q, MAX_Q);
    let (sin_w0, cos_w0) = w0.sin_cos();
    Rbj {
        cos_w0,
        alpha: sin_w0 / (2.0 * q),
    }
}

/// `A` in the cookbook: the square root of the peak/shelf gain.
fn shelf_amplitude(gain_db: f64) -> f64 {
    let db = range::bounded(gain_db, -MAX_ABS_GAIN_DB, MAX_ABS_GAIN_DB);
    crate::db_to_gain_f64(db * 0.5)
}

/// Divides through by `a0`, which the clamps above keep strictly positive.
fn normalize(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> BiquadCoeffs {
    let inv = 1.0 / a0;
    BiquadCoeffs {
        b0: b0 * inv,
        b1: b1 * inv,
        b2: b2 * inv,
        a1: a1 * inv,
        a2: a2 * inv,
    }
}

/// The five normalised coefficients of a biquad section (`a0` already divided
/// out), in the sign convention `y = b0·x + b1·x' + b2·x'' - a1·y' - a2·y''`.
///
/// Splitting the coefficients out from [`Biquad`] lets a controller compute an
/// EQ curve on the main thread and hand the finished numbers to the processor,
/// which is the only way to retune a filter from the audio thread without
/// paying for eight transcendental functions per block.
///
/// [any-thread]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BiquadCoeffs {
    /// Feed-forward coefficient for `x[n]`.
    pub b0: f64,
    /// Feed-forward coefficient for `x[n-1]`.
    pub b1: f64,
    /// Feed-forward coefficient for `x[n-2]`.
    pub b2: f64,
    /// Feedback coefficient for `y[n-1]`.
    pub a1: f64,
    /// Feedback coefficient for `y[n-2]`.
    pub a2: f64,
}

impl Default for BiquadCoeffs {
    /// Returns [`BiquadCoeffs::IDENTITY`], not all-zeros — a default-constructed
    /// filter passes audio through rather than silencing it.
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl BiquadCoeffs {
    /// Unity transfer function: the section passes its input through unchanged.
    ///
    /// [any-thread]
    pub const IDENTITY: Self = Self {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    /// Cookbook low-pass, −12 dB/octave above `freq_hz`. [any-thread]
    #[must_use]
    pub fn lowpass(sample_rate: f64, freq_hz: f64, q: f64) -> Self {
        let Rbj { cos_w0, alpha } = rbj_terms(sample_rate, freq_hz, q);
        let b = (1.0 - cos_w0) * 0.5;
        normalize(b, 2.0 * b, b, 1.0 + alpha, -2.0 * cos_w0, 1.0 - alpha)
    }

    /// Cookbook high-pass, −12 dB/octave below `freq_hz`. [any-thread]
    #[must_use]
    pub fn highpass(sample_rate: f64, freq_hz: f64, q: f64) -> Self {
        let Rbj { cos_w0, alpha } = rbj_terms(sample_rate, freq_hz, q);
        let b = (1.0 + cos_w0) * 0.5;
        normalize(b, -2.0 * b, b, 1.0 + alpha, -2.0 * cos_w0, 1.0 - alpha)
    }

    /// Cookbook band-pass with **constant 0 dB peak gain** (the `alpha`-scaled
    /// variant, not the `Q`-scaled one). [any-thread]
    #[must_use]
    pub fn bandpass(sample_rate: f64, freq_hz: f64, q: f64) -> Self {
        let Rbj { cos_w0, alpha } = rbj_terms(sample_rate, freq_hz, q);
        normalize(alpha, 0.0, -alpha, 1.0 + alpha, -2.0 * cos_w0, 1.0 - alpha)
    }

    /// Cookbook notch (band-reject): unity everywhere but a null at `freq_hz`.
    /// [any-thread]
    #[must_use]
    pub fn notch(sample_rate: f64, freq_hz: f64, q: f64) -> Self {
        let Rbj { cos_w0, alpha } = rbj_terms(sample_rate, freq_hz, q);
        normalize(
            1.0,
            -2.0 * cos_w0,
            1.0,
            1.0 + alpha,
            -2.0 * cos_w0,
            1.0 - alpha,
        )
    }

    /// Cookbook all-pass: unity magnitude at every frequency, phase rotating
    /// through 180° at `freq_hz`. [any-thread]
    #[must_use]
    pub fn allpass(sample_rate: f64, freq_hz: f64, q: f64) -> Self {
        let Rbj { cos_w0, alpha } = rbj_terms(sample_rate, freq_hz, q);
        normalize(
            1.0 - alpha,
            -2.0 * cos_w0,
            1.0 + alpha,
            1.0 + alpha,
            -2.0 * cos_w0,
            1.0 - alpha,
        )
    }

    /// Cookbook peaking EQ: `gain_db` of boost or cut centred on `freq_hz`.
    /// [any-thread]
    #[must_use]
    pub fn peak(sample_rate: f64, freq_hz: f64, q: f64, gain_db: f64) -> Self {
        let Rbj { cos_w0, alpha } = rbj_terms(sample_rate, freq_hz, q);
        let a = shelf_amplitude(gain_db);
        normalize(
            1.0 + alpha * a,
            -2.0 * cos_w0,
            1.0 - alpha * a,
            1.0 + alpha / a,
            -2.0 * cos_w0,
            1.0 - alpha / a,
        )
    }

    /// Cookbook low shelf: `gain_db` applied below `freq_hz`, unity above.
    ///
    /// `freq_hz` is the shelf midpoint (`gain_db / 2`). [any-thread]
    #[must_use]
    pub fn lowshelf(sample_rate: f64, freq_hz: f64, q: f64, gain_db: f64) -> Self {
        let Rbj { cos_w0, alpha } = rbj_terms(sample_rate, freq_hz, q);
        let a = shelf_amplitude(gain_db);
        let ap1 = a + 1.0;
        let am1 = a - 1.0;
        let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
        normalize(
            a * (ap1 - am1 * cos_w0 + two_sqrt_a_alpha),
            2.0 * a * (am1 - ap1 * cos_w0),
            a * (ap1 - am1 * cos_w0 - two_sqrt_a_alpha),
            ap1 + am1 * cos_w0 + two_sqrt_a_alpha,
            -2.0 * (am1 + ap1 * cos_w0),
            ap1 + am1 * cos_w0 - two_sqrt_a_alpha,
        )
    }

    /// Cookbook high shelf: `gain_db` applied above `freq_hz`, unity below.
    ///
    /// `freq_hz` is the shelf midpoint (`gain_db / 2`). [any-thread]
    #[must_use]
    pub fn highshelf(sample_rate: f64, freq_hz: f64, q: f64, gain_db: f64) -> Self {
        let Rbj { cos_w0, alpha } = rbj_terms(sample_rate, freq_hz, q);
        let a = shelf_amplitude(gain_db);
        let ap1 = a + 1.0;
        let am1 = a - 1.0;
        let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
        normalize(
            a * (ap1 + am1 * cos_w0 + two_sqrt_a_alpha),
            -2.0 * a * (am1 + ap1 * cos_w0),
            a * (ap1 + am1 * cos_w0 - two_sqrt_a_alpha),
            ap1 - am1 * cos_w0 + two_sqrt_a_alpha,
            2.0 * (am1 - ap1 * cos_w0),
            ap1 - am1 * cos_w0 - two_sqrt_a_alpha,
        )
    }

    /// Signed transfer function at DC, `H(z = 1)`. [any-thread]
    ///
    /// Closed-form — no need to run a signal through the filter to find out what
    /// it does to a constant.
    ///
    /// The denominator `1 + a1 + a2` cancels catastrophically when both poles
    /// crowd `z = 1`, which is what a very low cutoff ratio means: at
    /// `freq / sample_rate = 5e-5` it is around `1e-7` computed from terms of
    /// order 2, costing roughly nine significant digits. The result is still
    /// good to about 1e-8 relative there, and to full precision at ordinary
    /// cutoffs, but do not read the last digits as gospel for a sub-bass filter.
    #[must_use]
    pub fn dc_gain(&self) -> f64 {
        (self.b0 + self.b1 + self.b2) / (1.0 + self.a1 + self.a2)
    }

    /// Signed transfer function at Nyquist, `H(z = -1)`. [any-thread]
    ///
    /// Mirrors [`dc_gain`](Self::dc_gain), including its conditioning: the
    /// cancellation in `1 - a1 + a2` bites when the poles crowd `z = -1`, i.e.
    /// at cutoff ratios approaching 0.5.
    #[must_use]
    pub fn nyquist_gain(&self) -> f64 {
        (self.b0 - self.b1 + self.b2) / (1.0 - self.a1 + self.a2)
    }

    /// True when both poles lie strictly inside the unit circle. [any-thread]
    ///
    /// Schur–Cohn for a second-order section: `|a2| < 1` and `|a1| < 1 + a2`.
    #[must_use]
    pub fn is_stable(&self) -> bool {
        self.a2.abs() < 1.0 && self.a1.abs() < 1.0 + self.a2
    }
}

/// A single biquad section with its own state. [any-thread] to construct,
/// [audio-thread] to run.
///
/// ```
/// # use daux_dsp::Biquad;
/// let mut lp = Biquad::lowpass(48_000.0, 1_000.0, std::f64::consts::FRAC_1_SQRT_2);
/// let mut block = [1.0_f32; 256];
/// lp.process_block(&mut block);
/// // A low-pass passes DC, so a constant input settles back to itself.
/// assert!((block[255] - 1.0).abs() < 1.0e-4);
/// ```
#[derive(Clone, Copy, Debug, Default)]
pub struct Biquad {
    coeffs: BiquadCoeffs,
    s1: f64,
    s2: f64,
}

/// Generates the `Biquad::<shape>` constructor and `Biquad::set_<shape>`
/// retuner for every cookbook shape parameterised by `(sample_rate, freq, q)`.
macro_rules! q_shapes {
    ($($shape:ident, $setter:ident, $what:literal;)*) => {
        impl Biquad {
            $(
                #[doc = concat!("Creates a ", $what, " with cleared state. [any-thread]")]
                ///
                /// Allocation-free and lock-free, but it evaluates a sine and a
                /// cosine: call it per block at most, never per sample.
                #[must_use]
                pub fn $shape(sample_rate: f64, freq_hz: f64, q: f64) -> Self {
                    Self::from_coefficients(BiquadCoeffs::$shape(sample_rate, freq_hz, q))
                }

                #[doc = concat!("Retunes this section as a ", $what, ", **keeping** its")]
                /// state so a live filter does not click. [audio-thread]
                ///
                /// Same cost caveat as the constructor — this belongs at block
                /// rate, not sample rate.
                pub fn $setter(&mut self, sample_rate: f64, freq_hz: f64, q: f64) {
                    self.coeffs = BiquadCoeffs::$shape(sample_rate, freq_hz, q);
                }
            )*
        }
    };
}

/// Same as [`q_shapes`], for the shapes that also take `gain_db`.
macro_rules! gain_shapes {
    ($($shape:ident, $setter:ident, $what:literal;)*) => {
        impl Biquad {
            $(
                #[doc = concat!("Creates a ", $what, " with cleared state. [any-thread]")]
                ///
                /// Allocation-free and lock-free, but it evaluates a sine, a
                /// cosine and an exponential: call it per block at most.
                #[must_use]
                pub fn $shape(sample_rate: f64, freq_hz: f64, q: f64, gain_db: f64) -> Self {
                    Self::from_coefficients(BiquadCoeffs::$shape(sample_rate, freq_hz, q, gain_db))
                }

                #[doc = concat!("Retunes this section as a ", $what, ", **keeping** its")]
                /// state so a live filter does not click. [audio-thread]
                pub fn $setter(
                    &mut self,
                    sample_rate: f64,
                    freq_hz: f64,
                    q: f64,
                    gain_db: f64,
                ) {
                    self.coeffs = BiquadCoeffs::$shape(sample_rate, freq_hz, q, gain_db);
                }
            )*
        }
    };
}

q_shapes! {
    lowpass,  set_lowpass,  "second-order low-pass";
    highpass, set_highpass, "second-order high-pass";
    bandpass, set_bandpass, "constant-0 dB-peak band-pass";
    notch,    set_notch,    "notch (band-reject)";
    allpass,  set_allpass,  "second-order all-pass";
}

gain_shapes! {
    peak,      set_peak,      "peaking EQ band";
    lowshelf,  set_lowshelf,  "low shelf";
    highshelf, set_highshelf, "high shelf";
}

impl Biquad {
    /// A pass-through section. [any-thread]
    #[must_use]
    pub const fn identity() -> Self {
        Self {
            coeffs: BiquadCoeffs::IDENTITY,
            s1: 0.0,
            s2: 0.0,
        }
    }

    /// Wraps precomputed coefficients in a section with cleared state.
    /// [any-thread]
    #[must_use]
    pub const fn from_coefficients(coeffs: BiquadCoeffs) -> Self {
        Self {
            coeffs,
            s1: 0.0,
            s2: 0.0,
        }
    }

    /// The coefficients currently in use. [any-thread]
    #[must_use]
    pub const fn coefficients(&self) -> BiquadCoeffs {
        self.coeffs
    }

    /// Installs coefficients computed elsewhere — typically on the main thread —
    /// keeping the filter state. [audio-thread]
    ///
    /// This is the allocation-free, transcendental-free way to sweep a filter
    /// from the audio thread: precompute a table, then swap coefficients in.
    pub const fn set_coefficients(&mut self, coeffs: BiquadCoeffs) {
        self.coeffs = coeffs;
    }

    /// Processes one sample. [audio-thread]
    ///
    /// Transposed direct form II:
    ///
    /// ```text
    /// y  = b0·x + s1
    /// s1 = b1·x - a1·y + s2
    /// s2 = b2·x - a2·y
    /// ```
    ///
    /// Both state words are pushed through [`flush_denormal_f64`] so a decaying
    /// tail cannot park the CPU in subnormal microcode.
    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let x = f64::from(x);
        let c = self.coeffs;
        let y = c.b0 * x + self.s1;
        self.s1 = flush_denormal_f64(c.b1 * x - c.a1 * y + self.s2);
        self.s2 = flush_denormal_f64(c.b2 * x - c.a2 * y);
        y as f32
    }

    /// Processes a block in place. [audio-thread]
    ///
    /// An empty slice is a no-op.
    #[inline]
    pub fn process_block(&mut self, buf: &mut [f32]) {
        // Hoisted so the coefficients live in registers for the whole block
        // instead of being reloaded through `&mut self` on every sample.
        let c = self.coeffs;
        let (mut s1, mut s2) = (self.s1, self.s2);
        for x in buf.iter_mut() {
            let xd = f64::from(*x);
            let y = c.b0 * xd + s1;
            s1 = flush_denormal_f64(c.b1 * xd - c.a1 * y + s2);
            s2 = flush_denormal_f64(c.b2 * xd - c.a2 * y);
            *x = y as f32;
        }
        self.s1 = s1;
        self.s2 = s2;
    }

    /// Clears the filter state without touching the coefficients.
    /// [audio-thread]
    pub const fn reset(&mut self) {
        self.s1 = 0.0;
        self.s2 = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::f64::consts::FRAC_1_SQRT_2;

    const SR: f64 = 48_000.0;
    const Q: f64 = FRAC_1_SQRT_2;

    /// Drives a constant `1.0` until the section settles, then reports the
    /// steady-state output — the measured `H(1)`.
    fn measured_dc_gain(mut f: Biquad) -> f64 {
        let mut y = 0.0_f32;
        for _ in 0..50_000 {
            y = f.process(1.0);
        }
        f64::from(y)
    }

    /// Drives an alternating `+1, -1` until the section settles, then reports
    /// the magnitude of the steady-state output — the measured `|H(-1)|`.
    fn measured_nyquist_gain(mut f: Biquad) -> f64 {
        let mut y = 0.0_f32;
        for i in 0..50_000 {
            y = f.process(if i % 2 == 0 { 1.0 } else { -1.0 });
        }
        f64::from(y.abs())
    }

    #[test]
    fn lowpass_passes_dc_and_stops_nyquist() {
        let c = BiquadCoeffs::lowpass(SR, 1_000.0, Q);
        assert!(
            (c.dc_gain() - 1.0).abs() < 1.0e-12,
            "analytic DC gain {}",
            c.dc_gain()
        );
        assert!(
            c.nyquist_gain().abs() < 1.0e-12,
            "analytic Nyquist gain {}",
            c.nyquist_gain()
        );

        let f = Biquad::from_coefficients(c);
        assert!((measured_dc_gain(f) - 1.0).abs() < 1.0e-5);
        assert!(measured_nyquist_gain(f) < 1.0e-5);
    }

    #[test]
    fn highpass_stops_dc_and_passes_nyquist() {
        let c = BiquadCoeffs::highpass(SR, 1_000.0, Q);
        assert!(
            c.dc_gain().abs() < 1.0e-12,
            "analytic DC gain {}",
            c.dc_gain()
        );
        assert!(
            (c.nyquist_gain().abs() - 1.0).abs() < 1.0e-12,
            "analytic Nyquist gain {}",
            c.nyquist_gain()
        );

        let f = Biquad::from_coefficients(c);
        assert!(measured_dc_gain(f).abs() < 1.0e-5);
        assert!((measured_nyquist_gain(f) - 1.0).abs() < 1.0e-5);
    }

    #[test]
    fn lowpass_and_highpass_hold_across_the_frequency_range() {
        // The pass-band assertions use 1e-6 rather than machine epsilon because
        // `dc_gain` divides by `1 + a1 + a2`, which at 10 Hz / 192 kHz is ~1e-7
        // formed from terms of order 2 — nine digits lost to cancellation
        // before the division. The stop-band numerators cancel to *exactly*
        // zero (`b1 == ±2·b0` bit for bit), so those stay at 1e-12.
        for &sr in &[44_100.0, 48_000.0, 96_000.0, 192_000.0] {
            for &f in &[10.0, 100.0, 1_000.0, 5_000.0, 15_000.0] {
                let lp = BiquadCoeffs::lowpass(sr, f, Q);
                assert!((lp.dc_gain() - 1.0).abs() < 1.0e-6, "lp dc @ {sr}/{f}");
                assert!(lp.nyquist_gain().abs() < 1.0e-12, "lp nyq @ {sr}/{f}");
                assert!(lp.is_stable(), "lp unstable @ {sr}/{f}");

                let hp = BiquadCoeffs::highpass(sr, f, Q);
                assert!(hp.dc_gain().abs() < 1.0e-12, "hp dc @ {sr}/{f}");
                assert!(
                    (hp.nyquist_gain().abs() - 1.0).abs() < 1.0e-6,
                    "hp nyq @ {sr}/{f}"
                );
                assert!(hp.is_stable(), "hp unstable @ {sr}/{f}");
            }
        }
    }

    #[test]
    fn bandpass_and_notch_are_complementary_at_the_edges() {
        let bp = BiquadCoeffs::bandpass(SR, 1_000.0, Q);
        assert!(bp.dc_gain().abs() < 1.0e-12);
        assert!(bp.nyquist_gain().abs() < 1.0e-12);

        let notch = BiquadCoeffs::notch(SR, 1_000.0, Q);
        assert!((notch.dc_gain() - 1.0).abs() < 1.0e-12);
        assert!((notch.nyquist_gain() - 1.0).abs() < 1.0e-12);
    }

    #[test]
    fn bandpass_peaks_at_its_centre_frequency() {
        // Feed the centre frequency and measure the steady-state amplitude.
        let mut f = Biquad::bandpass(SR, 1_000.0, 4.0);
        let w = core::f64::consts::TAU * 1_000.0 / SR;
        let mut peak = 0.0_f32;
        for n in 0..40_000 {
            let y = f.process((w * f64::from(n)).sin() as f32);
            if n > 20_000 && y.abs() > peak {
                peak = y.abs();
            }
        }
        assert!(
            (f64::from(peak) - 1.0).abs() < 1.0e-3,
            "band-pass peak gain {peak}"
        );
    }

    #[test]
    fn allpass_has_unity_magnitude_at_dc_and_nyquist() {
        let c = BiquadCoeffs::allpass(SR, 1_000.0, Q);
        assert!((c.dc_gain().abs() - 1.0).abs() < 1.0e-12);
        assert!((c.nyquist_gain().abs() - 1.0).abs() < 1.0e-12);
    }

    #[test]
    fn allpass_preserves_energy_of_a_sine() {
        let mut f = Biquad::allpass(SR, 1_000.0, Q);
        let w = core::f64::consts::TAU * 700.0 / SR;
        let mut energy_in = 0.0_f64;
        let mut energy_out = 0.0_f64;
        for n in 0..48_000 {
            let x = (w * f64::from(n)).sin() as f32;
            let y = f.process(x);
            if n > 4_000 {
                energy_in += f64::from(x) * f64::from(x);
                energy_out += f64::from(y) * f64::from(y);
            }
        }
        let ratio = energy_out / energy_in;
        assert!(
            (ratio - 1.0).abs() < 1.0e-3,
            "all-pass energy ratio {ratio}"
        );
    }

    #[test]
    fn peak_reaches_its_gain_at_the_centre_and_unity_at_the_edges() {
        let c = BiquadCoeffs::peak(SR, 1_000.0, 1.0, 12.0);
        assert!((c.dc_gain() - 1.0).abs() < 1.0e-10);
        assert!((c.nyquist_gain() - 1.0).abs() < 1.0e-10);

        let mut f = Biquad::from_coefficients(c);
        let w = core::f64::consts::TAU * 1_000.0 / SR;
        let mut peak = 0.0_f32;
        for n in 0..80_000 {
            let y = f.process((w * f64::from(n)).sin() as f32);
            if n > 40_000 && y.abs() > peak {
                peak = y.abs();
            }
        }
        let db = crate::gain_to_db(peak);
        assert!((db - 12.0).abs() < 0.1, "peak gain {db} dB");
    }

    #[test]
    fn shelves_hit_their_target_gain_on_the_correct_side() {
        let low = BiquadCoeffs::lowshelf(SR, 1_000.0, Q, 9.0);
        let expected = crate::db_to_gain_f64(9.0);
        assert!(
            (low.dc_gain() - expected).abs() < 1.0e-6,
            "low shelf DC {}",
            low.dc_gain()
        );
        assert!((low.nyquist_gain() - 1.0).abs() < 1.0e-10);

        let high = BiquadCoeffs::highshelf(SR, 1_000.0, Q, 9.0);
        assert!((high.dc_gain() - 1.0).abs() < 1.0e-10);
        assert!(
            (high.nyquist_gain() - expected).abs() < 1.0e-6,
            "high shelf Nyquist {}",
            high.nyquist_gain()
        );
    }

    #[test]
    fn negative_shelf_gain_cuts() {
        let low = BiquadCoeffs::lowshelf(SR, 1_000.0, Q, -9.0);
        let expected = crate::db_to_gain_f64(-9.0);
        assert!((low.dc_gain() - expected).abs() < 1.0e-6);
    }

    #[test]
    fn zero_gain_shelves_and_peaks_are_transparent() {
        for c in [
            BiquadCoeffs::peak(SR, 1_000.0, Q, 0.0),
            BiquadCoeffs::lowshelf(SR, 1_000.0, Q, 0.0),
            BiquadCoeffs::highshelf(SR, 1_000.0, Q, 0.0),
        ] {
            assert!((c.dc_gain() - 1.0).abs() < 1.0e-10);
            assert!((c.nyquist_gain() - 1.0).abs() < 1.0e-10);
        }
    }

    #[test]
    fn identity_is_a_wire() {
        let mut f = Biquad::identity();
        for i in 0..64 {
            let x = i as f32 * 0.01 - 0.3;
            assert_eq!(f.process(x), x);
        }
        assert_eq!(Biquad::default().coefficients(), BiquadCoeffs::IDENTITY);
    }

    #[test]
    fn process_block_matches_per_sample_processing() {
        let mut a = Biquad::lowpass(SR, 800.0, Q);
        let mut b = a;
        let input: Vec<f32> = (0..1_000).map(|i| (i as f32 * 0.07).sin() * 0.8).collect();

        let per_sample: Vec<f32> = input.iter().map(|&x| a.process(x)).collect();
        let mut block = input;
        b.process_block(&mut block);

        assert_eq!(per_sample, block);
        assert_eq!(a.s1.to_bits(), b.s1.to_bits());
        assert_eq!(a.s2.to_bits(), b.s2.to_bits());
    }

    #[test]
    fn empty_block_is_a_no_op() {
        let mut f = Biquad::lowpass(SR, 800.0, Q);
        f.process(1.0);
        let before = (f.s1, f.s2);
        f.process_block(&mut []);
        assert_eq!(before, (f.s1, f.s2));
    }

    #[test]
    fn reset_clears_state_but_keeps_the_tuning() {
        let mut f = Biquad::lowpass(SR, 800.0, Q);
        let coeffs = f.coefficients();
        for _ in 0..100 {
            f.process(1.0);
        }
        assert!(f.s1 != 0.0);
        f.reset();
        assert_eq!((f.s1, f.s2), (0.0, 0.0));
        assert_eq!(f.coefficients(), coeffs);
    }

    #[test]
    fn retuning_preserves_state() {
        let mut f = Biquad::lowpass(SR, 800.0, Q);
        for _ in 0..100 {
            f.process(1.0);
        }
        let state = (f.s1, f.s2);
        f.set_lowpass(SR, 2_000.0, Q);
        assert_eq!(state, (f.s1, f.s2));
        assert_ne!(f.coefficients(), BiquadCoeffs::lowpass(SR, 800.0, Q));
    }

    #[test]
    fn every_setter_matches_its_constructor() {
        let mut f = Biquad::identity();
        f.set_lowpass(SR, 900.0, Q);
        assert_eq!(f.coefficients(), BiquadCoeffs::lowpass(SR, 900.0, Q));
        f.set_highpass(SR, 900.0, Q);
        assert_eq!(f.coefficients(), BiquadCoeffs::highpass(SR, 900.0, Q));
        f.set_bandpass(SR, 900.0, Q);
        assert_eq!(f.coefficients(), BiquadCoeffs::bandpass(SR, 900.0, Q));
        f.set_notch(SR, 900.0, Q);
        assert_eq!(f.coefficients(), BiquadCoeffs::notch(SR, 900.0, Q));
        f.set_allpass(SR, 900.0, Q);
        assert_eq!(f.coefficients(), BiquadCoeffs::allpass(SR, 900.0, Q));
        f.set_peak(SR, 900.0, Q, 3.0);
        assert_eq!(f.coefficients(), BiquadCoeffs::peak(SR, 900.0, Q, 3.0));
        f.set_lowshelf(SR, 900.0, Q, 3.0);
        assert_eq!(f.coefficients(), BiquadCoeffs::lowshelf(SR, 900.0, Q, 3.0));
        f.set_highshelf(SR, 900.0, Q, 3.0);
        assert_eq!(f.coefficients(), BiquadCoeffs::highshelf(SR, 900.0, Q, 3.0));
    }

    #[test]
    fn hostile_parameters_still_produce_a_stable_finite_filter() {
        let hostile: &[(f64, f64, f64)] = &[
            (48_000.0, 0.0, 0.7),                // zero frequency
            (48_000.0, -1_000.0, 0.7),           // negative frequency
            (48_000.0, 48_000.0, 0.7),           // frequency above Nyquist
            (48_000.0, 1_000.0, 0.0),            // zero Q
            (48_000.0, 1_000.0, -5.0),           // negative Q
            (48_000.0, 1_000.0, f64::INFINITY),  // unbounded Q
            (0.0, 1_000.0, 0.7),                 // zero sample rate
            (-48_000.0, 1_000.0, 0.7),           // negative sample rate
            (f64::NAN, 1_000.0, 0.7),            // NaN sample rate
            (48_000.0, f64::NAN, 0.7),           // NaN frequency
            (48_000.0, 1_000.0, f64::NAN),       // NaN Q
            (f64::INFINITY, f64::INFINITY, 0.7), // everything at once
        ];
        for &(sr, f, q) in hostile {
            for c in [
                BiquadCoeffs::lowpass(sr, f, q),
                BiquadCoeffs::highpass(sr, f, q),
                BiquadCoeffs::bandpass(sr, f, q),
                BiquadCoeffs::notch(sr, f, q),
                BiquadCoeffs::allpass(sr, f, q),
                BiquadCoeffs::peak(sr, f, q, 6.0),
                BiquadCoeffs::lowshelf(sr, f, q, 6.0),
                BiquadCoeffs::highshelf(sr, f, q, 6.0),
            ] {
                for v in [c.b0, c.b1, c.b2, c.a1, c.a2] {
                    assert!(
                        v.is_finite(),
                        "non-finite coefficient from ({sr}, {f}, {q})"
                    );
                }
                assert!(c.is_stable(), "unstable filter from ({sr}, {f}, {q})");
            }
        }
    }

    #[test]
    fn hostile_gain_values_are_clamped() {
        for db in [f64::INFINITY, f64::NEG_INFINITY, f64::NAN, 1.0e9, -1.0e9] {
            let c = BiquadCoeffs::peak(SR, 1_000.0, Q, db);
            for v in [c.b0, c.b1, c.b2, c.a1, c.a2] {
                assert!(v.is_finite(), "non-finite coefficient from gain {db}");
            }
        }
    }

    #[test]
    fn state_is_flushed_instead_of_decaying_into_subnormals() {
        let mut f = Biquad::lowpass(SR, 100.0, Q);
        f.process(1.0);
        // Ring down on silence; the guard must snap the tail to exactly zero.
        // The pole radius here is ~0.9908, so ~75 000 samples suffice to cross
        // `DENORMAL_THRESHOLD_F64`; the extra headroom keeps the test honest.
        for _ in 0..1_000_000 {
            f.process(0.0);
        }
        assert_eq!(f.s1, 0.0);
        assert_eq!(f.s2, 0.0);
    }

    #[test]
    fn a_finite_signal_never_becomes_non_finite() {
        let mut f = Biquad::peak(SR, 1_000.0, 20.0, 24.0);
        for i in 0..100_000 {
            let y = f.process((i as f32 * 0.31).sin());
            assert!(y.is_finite());
        }
    }
}
