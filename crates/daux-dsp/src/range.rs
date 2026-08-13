//! Shared clamping of user-supplied filter parameters.
//!
//! Filter tunings arrive from parameters, from automation, from a host that may
//! not have told the plug-in its sample rate yet, and occasionally from a
//! modulation source that has gone `NaN`. None of that may produce a `NaN`
//! coefficient, an infinite one, or a pole on the unit circle, because the
//! damage does not stop at one block: a poisoned coefficient poisons the filter
//! state, and a poisoned state outputs silence-shaped garbage until the next
//! `reset` — which may never come.
//!
//! So every tuning entry point funnels through here first. Two properties do
//! the work:
//!
//! * Every clamp is `f64::max` / `f64::min`, which return the *other* operand
//!   when one side is `NaN`. Hostile input collapses to a defined, finite
//!   filter instead of propagating.
//! * The cutoff is bounded as a *fraction of the sample rate*, not in hertz.
//!   Conditioning depends on `w0`, not on frequency, so a ratio bound keeps
//!   `cos(w0)` strictly below 1 and `sin(w0)` strictly above 0 at every sample
//!   rate — which is exactly what keeps the poles strictly inside the unit
//!   circle and `1 - a1 + a2` away from zero.

/// Sample rates below this are treated as this.
pub(crate) const MIN_SAMPLE_RATE: f64 = 1.0;

/// Sample rates above this are treated as this.
///
/// A gigahertz is six orders of magnitude beyond any audio device that exists.
/// The bound is not a judgement about plausible rates; it exists so that an
/// infinite rate cannot turn `TAU * freq / rate` into `inf / inf`, which is
/// `NaN`, which would sail past every subsequent clamp.
pub(crate) const MAX_SAMPLE_RATE: f64 = 1.0e9;

/// Lowest cutoff, as a fraction of the sample rate.
///
/// `1e-6` is 0.048 Hz at 48 kHz — far below any musical use — and gives
/// `w0 >= 6.3e-6`, so `1 - cos(w0) >= 2e-11`, comfortably representable in
/// `f64`. Any smaller and `cos(w0)` would round to exactly `1.0` and the poles
/// would land *on* the unit circle.
pub(crate) const MIN_FREQ_RATIO: f64 = 1.0e-6;

/// Highest cutoff, as a fraction of the sample rate.
///
/// Just short of Nyquist, so `sin(w0)` stays strictly positive and the `alpha`
/// term of the RBJ cookbook stays strictly positive with it.
pub(crate) const MAX_FREQ_RATIO: f64 = 0.4999;

/// `x` bounded to `[lo, hi]`, mapping `NaN` to `lo`.
///
/// This is deliberately **not** `f64::clamp`. `clamp` returns `NaN` unchanged
/// and panics if a bound is `NaN` — both disqualifying here, since the entire
/// purpose of this module is that no input can produce a `NaN` coefficient.
/// `max` then `min` returns the other operand on an unordered comparison, so
/// `NaN` falls out at the lower bound and the result is always finite.
///
/// Callers must pass ordered, non-`NaN` bounds; every caller in this crate
/// passes compile-time constants or products of already-clamped values.
#[inline]
pub(crate) fn bounded(x: f64, lo: f64, hi: f64) -> f64 {
    x.max(lo).min(hi)
}

/// Clamps a sample rate into `MIN_SAMPLE_RATE ..= MAX_SAMPLE_RATE`, mapping
/// `NaN` to `MIN_SAMPLE_RATE`.
#[inline]
pub(crate) fn sample_rate(rate: f64) -> f64 {
    bounded(rate, MIN_SAMPLE_RATE, MAX_SAMPLE_RATE)
}

/// Clamps a cutoff into `MIN_FREQ_RATIO ..= MAX_FREQ_RATIO` of `rate`, mapping
/// `NaN` to the lower bound.
///
/// `rate` must already have been through [`sample_rate`].
#[inline]
pub(crate) fn cutoff(rate: f64, freq_hz: f64) -> f64 {
    bounded(freq_hz, rate * MIN_FREQ_RATIO, rate * MAX_FREQ_RATIO)
}

/// Angular frequency `w0 = 2π·f/fs` for a clamped cutoff, guaranteed to lie in
/// `(0, π)` and to be finite.
#[inline]
pub(crate) fn omega(rate: f64, freq_hz: f64) -> f64 {
    let rate = sample_rate(rate);
    core::f64::consts::TAU * cutoff(rate, freq_hz) / rate
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOSTILE_RATES: &[f64] = &[
        48_000.0,
        0.0,
        -1.0,
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        1.0e300,
        768_000.0,
    ];
    const HOSTILE_FREQS: &[f64] = &[
        1_000.0,
        0.0,
        -1.0,
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        1.0e300,
    ];

    #[test]
    fn sample_rate_is_always_finite_and_in_range() {
        for &r in HOSTILE_RATES {
            let clamped = sample_rate(r);
            assert!(clamped.is_finite(), "rate {r} -> {clamped}");
            assert!(
                (MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&clamped),
                "rate {r}"
            );
        }
        assert_eq!(sample_rate(48_000.0), 48_000.0);
        assert_eq!(sample_rate(f64::NAN), MIN_SAMPLE_RATE);
        assert_eq!(sample_rate(f64::INFINITY), MAX_SAMPLE_RATE);
    }

    #[test]
    fn omega_always_lands_strictly_inside_zero_to_pi() {
        for &r in HOSTILE_RATES {
            for &f in HOSTILE_FREQS {
                let w = omega(r, f);
                assert!(w.is_finite(), "({r}, {f}) -> {w}");
                assert!(w > 0.0 && w < core::f64::consts::PI, "({r}, {f}) -> {w}");
                // The bound that actually matters: cos(w0) must not round to 1.
                assert!(w.cos() < 1.0, "({r}, {f}) -> cos {}", w.cos());
                assert!(w.sin() > 0.0, "({r}, {f}) -> sin {}", w.sin());
            }
        }
    }

    #[test]
    fn bounded_sends_nan_to_the_lower_bound() {
        assert_eq!(bounded(f64::NAN, -2.0, 5.0), -2.0);
        assert_eq!(bounded(f64::INFINITY, -2.0, 5.0), 5.0);
        assert_eq!(bounded(f64::NEG_INFINITY, -2.0, 5.0), -2.0);
        assert_eq!(bounded(1.5, -2.0, 5.0), 1.5);
        assert_eq!(bounded(-9.0, -2.0, 5.0), -2.0);
        assert_eq!(bounded(9.0, -2.0, 5.0), 5.0);
    }

    #[test]
    fn a_normal_tuning_passes_through_untouched() {
        assert_eq!(cutoff(48_000.0, 1_000.0), 1_000.0);
        let w = omega(48_000.0, 1_000.0);
        assert!((w - core::f64::consts::TAU / 48.0).abs() < 1.0e-15);
    }
}
