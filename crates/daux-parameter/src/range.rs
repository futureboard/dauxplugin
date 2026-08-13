//! Value ranges and the plain ↔ normalised mapping.

use core::fmt;

/// Why a [`ParamRange`] cannot be used.
///
/// Ranges are built on the main thread while the plug-in is constructed, so reporting
/// a problem here is cheap and never happens on the audio thread.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RangeError {
    /// A bound is `NaN` or infinite.
    NotFinite,
    /// `min` and `max` are equal, so the mapping cannot be inverted.
    EmptyRange,
    /// A logarithmic range was given a bound that is zero or negative.
    NonPositiveLogarithmicBound,
    /// A skewed range was given a factor that is not finite and greater than zero.
    InvalidSkewFactor,
    /// A stepped range was given `min > max`.
    InvertedStepRange,
}

impl fmt::Display for RangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            Self::NotFinite => "parameter range bounds must be finite",
            Self::EmptyRange => "parameter range bounds must differ (min == max is not invertible)",
            Self::NonPositiveLogarithmicBound => {
                "a logarithmic parameter range needs strictly positive bounds \
                 (0 Hz has no logarithm; use something like 20.0..=20_000.0)"
            }
            Self::InvalidSkewFactor => {
                "a skewed parameter range needs a finite factor greater than zero \
                 (< 1 gives more resolution near min, > 1 near max, 1 is linear)"
            }
            Self::InvertedStepRange => "a stepped parameter range needs min <= max",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for RangeError {}

/// How plain values map onto the normalised `0..=1` line a host automation lane uses.
///
/// # Plain in, plain out
///
/// [`normalize`](ParamRange::normalize) and [`denormalize`](ParamRange::denormalize)
/// are exact inverses of each other over the whole range, including both endpoints,
/// for the continuous variants. For the quantised variants ([`Stepped`](Self::Stepped),
/// [`Boolean`](Self::Boolean)) they are inverses *on the grid*: `denormalize` snaps to
/// the nearest step, so `normalize(denormalize(x))` is the grid point nearest `x` and
/// applying it again changes nothing (it is idempotent). That is what makes dragging a
/// stepped control feel right and makes a recalled normalised value land back on the
/// same step.
///
/// # Robustness
///
/// The variants are plain data with public fields, so nothing stops a caller from
/// building `Logarithmic { min: -1.0, .. }`. Rather than panic on the audio thread,
/// [`normalize`](ParamRange::normalize) and [`denormalize`](ParamRange::denormalize)
/// degrade to a defined answer (`0.0` and [`min`](ParamRange::min) respectively) for
/// any range [`validate`](ParamRange::validate) would reject, and they never return
/// `NaN`. Use the checked constructors, or call `validate`, to catch mistakes at build
/// time instead.
///
/// `[any-thread]` — every method is allocation-free, wait-free and panic-free.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ParamRange {
    /// Uniform mapping: the knob moves the value at a constant rate.
    Linear {
        /// Value at normalised `0.0`.
        min: f64,
        /// Value at normalised `1.0`.
        max: f64,
    },
    /// Power curve: `normalized = t.powf(factor)` where `t` is the linear position.
    ///
    /// `factor < 1` spends more of the knob's travel near `min`, `factor > 1` near
    /// `max`, `factor == 1` is exactly [`Linear`](Self::Linear).
    Skewed {
        /// Value at normalised `0.0`.
        min: f64,
        /// Value at normalised `1.0`.
        max: f64,
        /// Exponent applied to the linear position; must be finite and `> 0`.
        factor: f64,
    },
    /// Constant-ratio mapping, the right curve for frequency and time.
    ///
    /// Both bounds must be strictly positive; equal *ratios* occupy equal knob
    /// distance, so 100→200 Hz feels the same as 1000→2000 Hz.
    Logarithmic {
        /// Value at normalised `0.0`; must be `> 0`.
        min: f64,
        /// Value at normalised `1.0`; must be `> 0`.
        max: f64,
    },
    /// Discrete integer values from `min` to `max` inclusive.
    Stepped {
        /// Lowest value, at normalised `0.0`.
        min: i64,
        /// Highest value, at normalised `1.0`.
        max: i64,
    },
    /// Two states: plain `0.0` (off) and `1.0` (on).
    Boolean,
}

/// Smallest bound a logarithmic range accepts: anything below this (including zero,
/// negatives and subnormals) has no usable logarithm.
const MIN_POSITIVE_BOUND: f64 = f64::MIN_POSITIVE;

/// Clamps to `0..=1`, mapping `NaN` to `0.0` (`f64::clamp` propagates `NaN`).
#[inline]
fn clamp01(v: f64) -> f64 {
    if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) }
}

/// Number of distinct values in an inclusive integer range, computed in `i128` so
/// `i64::MIN..=i64::MAX` cannot overflow.
#[inline]
fn step_span(min: i64, max: i64) -> i128 {
    if max <= min {
        1
    } else {
        (max as i128) - (min as i128) + 1
    }
}

impl ParamRange {
    /// The `0.0..=1.0` range, handy for gain-like and mix-like controls.
    pub const UNIT: Self = Self::Linear { min: 0.0, max: 1.0 };

    /// `[main-thread]` Builds a linear range.
    ///
    /// # Panics
    ///
    /// Panics if the bounds are not finite or are equal. Constructors run once, on the
    /// main thread, while the plug-in is being built, so a panic here surfaces the
    /// mistake during the author's first run instead of silently producing a dead
    /// control in a user's session. Use [`try_linear`](Self::try_linear) when the
    /// bounds come from data rather than from source code.
    #[must_use]
    pub fn linear(min: f64, max: f64) -> Self {
        Self::try_linear(min, max).unwrap_or_else(|e| panic!("invalid linear range: {e}"))
    }

    /// `[main-thread]` Checked [`linear`](Self::linear).
    ///
    /// # Errors
    ///
    /// Returns [`RangeError`] when the bounds are not finite or are equal.
    pub fn try_linear(min: f64, max: f64) -> Result<Self, RangeError> {
        let r = Self::Linear { min, max };
        r.validate()?;
        Ok(r)
    }

    /// `[main-thread]` Builds a skewed (power-curve) range.
    ///
    /// # Panics
    ///
    /// Panics if the bounds are not finite or equal, or if `factor` is not finite and
    /// positive. See [`linear`](Self::linear) for why panicking is the right answer at
    /// build time.
    #[must_use]
    pub fn skewed(min: f64, max: f64, factor: f64) -> Self {
        Self::try_skewed(min, max, factor).unwrap_or_else(|e| panic!("invalid skewed range: {e}"))
    }

    /// `[main-thread]` Checked [`skewed`](Self::skewed).
    ///
    /// # Errors
    ///
    /// Returns [`RangeError`] when the bounds or the factor are unusable.
    pub fn try_skewed(min: f64, max: f64, factor: f64) -> Result<Self, RangeError> {
        let r = Self::Skewed { min, max, factor };
        r.validate()?;
        Ok(r)
    }

    /// `[main-thread]` Builds a logarithmic range.
    ///
    /// # Panics
    ///
    /// Panics if either bound is zero, negative or not finite, or if the bounds are
    /// equal — a logarithmic control from 0 Hz is not a curve choice, it is a bug, and
    /// it must not reach a user. See [`linear`](Self::linear) for the reasoning behind
    /// panicking in constructors. Use [`try_logarithmic`](Self::try_logarithmic) when
    /// the bounds are not literals.
    ///
    /// ```
    /// use daux_parameter::ParamRange;
    /// let cutoff = ParamRange::logarithmic(20.0, 20_000.0);
    /// assert!((cutoff.normalize(632.4555320336759) - 0.5).abs() < 1e-12);
    /// ```
    #[must_use]
    pub fn logarithmic(min: f64, max: f64) -> Self {
        Self::try_logarithmic(min, max).unwrap_or_else(|e| panic!("invalid logarithmic range: {e}"))
    }

    /// `[main-thread]` Checked [`logarithmic`](Self::logarithmic).
    ///
    /// # Errors
    ///
    /// Returns [`RangeError::NonPositiveLogarithmicBound`] when a bound is `<= 0`, and
    /// the usual finiteness/emptiness errors otherwise.
    pub fn try_logarithmic(min: f64, max: f64) -> Result<Self, RangeError> {
        let r = Self::Logarithmic { min, max };
        r.validate()?;
        Ok(r)
    }

    /// `[main-thread]` Builds a stepped range covering `min..=max` inclusive.
    ///
    /// # Panics
    ///
    /// Panics if `min > max`.
    #[must_use]
    pub fn stepped(min: i64, max: i64) -> Self {
        Self::try_stepped(min, max).unwrap_or_else(|e| panic!("invalid stepped range: {e}"))
    }

    /// `[main-thread]` Checked [`stepped`](Self::stepped).
    ///
    /// # Errors
    ///
    /// Returns [`RangeError::InvertedStepRange`] when `min > max`.
    pub fn try_stepped(min: i64, max: i64) -> Result<Self, RangeError> {
        let r = Self::Stepped { min, max };
        r.validate()?;
        Ok(r)
    }

    /// `[any-thread]` Checks that this range can be mapped both ways.
    ///
    /// # Errors
    ///
    /// Returns the first problem found; see [`RangeError`].
    pub fn validate(&self) -> Result<(), RangeError> {
        match *self {
            Self::Linear { min, max } => {
                if !min.is_finite() || !max.is_finite() {
                    return Err(RangeError::NotFinite);
                }
                if min == max {
                    return Err(RangeError::EmptyRange);
                }
                Ok(())
            }
            Self::Skewed { min, max, factor } => {
                if !min.is_finite() || !max.is_finite() {
                    return Err(RangeError::NotFinite);
                }
                if min == max {
                    return Err(RangeError::EmptyRange);
                }
                if !factor.is_finite() || factor <= 0.0 {
                    return Err(RangeError::InvalidSkewFactor);
                }
                Ok(())
            }
            Self::Logarithmic { min, max } => {
                if !min.is_finite() || !max.is_finite() {
                    return Err(RangeError::NotFinite);
                }
                if min < MIN_POSITIVE_BOUND || max < MIN_POSITIVE_BOUND {
                    return Err(RangeError::NonPositiveLogarithmicBound);
                }
                if min == max {
                    return Err(RangeError::EmptyRange);
                }
                Ok(())
            }
            Self::Stepped { min, max } => {
                if min > max {
                    return Err(RangeError::InvertedStepRange);
                }
                Ok(())
            }
            Self::Boolean => Ok(()),
        }
    }

    /// `[any-thread]` Plain value at normalised `0.0`.
    #[inline]
    #[must_use]
    pub fn min(&self) -> f64 {
        match *self {
            Self::Linear { min, .. } | Self::Skewed { min, .. } | Self::Logarithmic { min, .. } => {
                min
            }
            Self::Stepped { min, .. } => min as f64,
            Self::Boolean => 0.0,
        }
    }

    /// `[any-thread]` Plain value at normalised `1.0`.
    #[inline]
    #[must_use]
    pub fn max(&self) -> f64 {
        match *self {
            Self::Linear { max, .. } | Self::Skewed { max, .. } | Self::Logarithmic { max, .. } => {
                max
            }
            Self::Stepped { max, .. } => max as f64,
            Self::Boolean => 1.0,
        }
    }

    /// `[any-thread]` The bounds in ascending order, `(lower, upper)`.
    ///
    /// [`min`](Self::min) and [`max`](Self::max) are the values at normalised `0.0` and
    /// `1.0`, which for a deliberately inverted range are the wrong way round. Hosts
    /// and the ABI want `min_value <= max_value`, so `ParamInfo` is built from this.
    #[inline]
    #[must_use]
    pub fn bounds(&self) -> (f64, f64) {
        let (a, b) = (self.min(), self.max());
        if a <= b { (a, b) } else { (b, a) }
    }

    /// `[any-thread]` Number of *intervals* between the discrete values, matching
    /// `DauxParamInfoV1::step_count`.
    ///
    /// A parameter therefore has `step_count + 1` distinct values, and `0` means
    /// continuous. A [`Boolean`](Self::Boolean) reports `1`; `Stepped { min: 0, max: 3 }`
    /// reports `3`. A degenerate `Stepped { min: 5, max: 5 }` reports `0` — it has a
    /// single value and behaves like a constant.
    #[inline]
    #[must_use]
    pub fn step_count(&self) -> u32 {
        match *self {
            Self::Linear { .. } | Self::Skewed { .. } | Self::Logarithmic { .. } => 0,
            Self::Stepped { min, max } => (step_span(min, max) - 1).min(u32::MAX as i128) as u32,
            Self::Boolean => 1,
        }
    }

    /// `[any-thread]` True for [`Stepped`](Self::Stepped) and [`Boolean`](Self::Boolean).
    #[inline]
    #[must_use]
    pub fn is_stepped(&self) -> bool {
        matches!(self, Self::Stepped { .. } | Self::Boolean)
    }

    /// `[any-thread]` Maps a plain value onto `0..=1`.
    ///
    /// The result is always within `0..=1` and never `NaN`: values outside the range
    /// are clamped, `NaN` becomes `0.0`, and an unusable range (see
    /// [`validate`](Self::validate)) yields `0.0`.
    #[must_use]
    pub fn normalize(&self, plain: f64) -> f64 {
        match *self {
            Self::Linear { min, max } => {
                if min == max || !min.is_finite() || !max.is_finite() {
                    return 0.0;
                }
                clamp01((plain - min) / (max - min))
            }
            Self::Skewed { min, max, factor } => {
                if min == max || !min.is_finite() || !max.is_finite() {
                    return 0.0;
                }
                if !factor.is_finite() || factor <= 0.0 {
                    return 0.0;
                }
                let t = clamp01((plain - min) / (max - min));
                clamp01(t.powf(factor))
            }
            Self::Logarithmic { min, max } => {
                if self.validate().is_err() {
                    return 0.0;
                }
                if plain <= 0.0 || plain.is_nan() {
                    // Below the representable part of the curve; the lowest position.
                    return if max > min { 0.0 } else { 1.0 };
                }
                clamp01((plain / min).ln() / (max / min).ln())
            }
            Self::Stepped { min, max } => {
                let span = step_span(min, max);
                if span <= 1 {
                    return 0.0;
                }
                if plain.is_nan() {
                    return 0.0;
                }
                let index = (plain.round() as i64).clamp(min, max) as i128 - min as i128;
                clamp01(index as f64 / (span - 1) as f64)
            }
            Self::Boolean => {
                if plain >= 0.5 {
                    1.0
                } else {
                    0.0
                }
            }
        }
    }

    /// `[any-thread]` Maps a normalised `0..=1` position back to a plain value.
    ///
    /// The input is clamped (and `NaN` treated as `0.0`), the output is always inside
    /// the range and never `NaN`, and for the quantised variants it lands exactly on a
    /// step. An unusable range yields [`min`](Self::min).
    #[must_use]
    pub fn denormalize(&self, norm: f64) -> f64 {
        let n = clamp01(norm);
        match *self {
            Self::Linear { min, max } => {
                if !min.is_finite() || !max.is_finite() {
                    return min;
                }
                min + n * (max - min)
            }
            Self::Skewed { min, max, factor } => {
                if !min.is_finite() || !max.is_finite() {
                    return min;
                }
                if !factor.is_finite() || factor <= 0.0 {
                    return min;
                }
                min + n.powf(factor.recip()) * (max - min)
            }
            Self::Logarithmic { min, max } => {
                if self.validate().is_err() {
                    return min;
                }
                // `min * (max/min)^n`, written as an exponential so that both
                // endpoints come back bit-exact.
                if n == 0.0 {
                    min
                } else if n == 1.0 {
                    max
                } else {
                    min * (max / min).powf(n)
                }
            }
            Self::Stepped { min, max } => {
                let span = step_span(min, max);
                if span <= 1 {
                    return min as f64;
                }
                // Centred quantisation: each step owns the half-interval either side of
                // its own normalised position, so `round` both snaps predictably while
                // dragging and round-trips exactly. The index is carried in `i128`
                // because a span may legitimately exceed `i64::MAX`.
                let index = (n * (span - 1) as f64).round() as i128;
                let value = (min as i128 + index).clamp(min as i128, max as i128);
                value as f64
            }
            Self::Boolean => {
                if n >= 0.5 {
                    1.0
                } else {
                    0.0
                }
            }
        }
    }

    /// `[any-thread]` Clamps a plain value into the range, snapping quantised ranges
    /// onto their grid. `NaN` becomes the lower bound.
    #[must_use]
    pub fn clamp(&self, plain: f64) -> f64 {
        match *self {
            Self::Linear { min, max }
            | Self::Skewed { min, max, .. }
            | Self::Logarithmic { min, max } => {
                let (lo, hi) = if min <= max { (min, max) } else { (max, min) };
                if plain.is_nan() {
                    lo
                } else {
                    plain.clamp(lo, hi)
                }
            }
            Self::Stepped { min, max } => {
                if plain.is_nan() {
                    return min as f64;
                }
                let (lo, hi) = if min <= max { (min, max) } else { (max, min) };
                (plain.round() as i64).clamp(lo, hi) as f64
            }
            Self::Boolean => {
                if plain >= 0.5 {
                    1.0
                } else {
                    0.0
                }
            }
        }
    }

    /// `[any-thread]` Snaps a normalised position onto the nearest representable one.
    ///
    /// A no-op for continuous ranges; for quantised ones it is exactly
    /// `normalize(denormalize(norm))`, which is what a host should store so that the
    /// value it recalls is the value the user saw.
    #[inline]
    #[must_use]
    pub fn snap_normalized(&self, norm: f64) -> f64 {
        self.normalize(self.denormalize(norm))
    }
}

impl Default for ParamRange {
    /// `0.0..=1.0`, the least surprising range for a control with no other information.
    fn default() -> Self {
        Self::UNIT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Positions used by every round-trip test: both endpoints, both near-endpoints,
    /// and a spread of interior points.
    const POSITIONS: [f64; 13] = [
        0.0,
        1e-12,
        0.001,
        0.05,
        0.1,
        0.25,
        1.0 / 3.0,
        0.5,
        0.75,
        0.9,
        0.999,
        1.0 - 1e-12,
        1.0,
    ];

    fn assert_round_trip(range: &ParamRange) {
        for n in POSITIONS {
            let plain = range.denormalize(n);
            let back = range.normalize(plain);
            assert!(
                (back - n).abs() <= 1e-9,
                "{range:?}: normalize(denormalize({n})) = {back}, plain = {plain}"
            );
            // And the plain value must survive the trip in the other direction too.
            let plain_again = range.denormalize(back);
            assert!(
                (plain_again - plain).abs() <= 1e-9 * plain.abs().max(1.0),
                "{range:?}: plain {plain} -> {plain_again}"
            );
        }
    }

    #[test]
    fn linear_round_trips() {
        assert_round_trip(&ParamRange::linear(-60.0, 12.0));
        assert_round_trip(&ParamRange::UNIT);
        assert_round_trip(&ParamRange::linear(-1.0, 1.0));
        // Inverted bounds are legal and simply run the knob backwards.
        assert_round_trip(&ParamRange::linear(1.0, -1.0));
        assert_round_trip(&ParamRange::linear(1e-6, 1e6));
    }

    #[test]
    fn skewed_round_trips_for_both_directions() {
        for factor in [0.25, 0.5, 1.0, 2.0, 3.0] {
            assert_round_trip(&ParamRange::skewed(0.0, 1.0, factor));
            assert_round_trip(&ParamRange::skewed(-20.0, 20.0, factor));
        }
    }

    #[test]
    fn logarithmic_round_trips() {
        assert_round_trip(&ParamRange::logarithmic(20.0, 20_000.0));
        assert_round_trip(&ParamRange::logarithmic(0.001, 1.0));
        // Descending logarithmic ranges are legal too.
        assert_round_trip(&ParamRange::logarithmic(20_000.0, 20.0));
    }

    #[test]
    fn stepped_round_trips_on_the_grid() {
        for range in [
            ParamRange::stepped(0, 1),
            ParamRange::stepped(0, 3),
            ParamRange::stepped(-7, 7),
            ParamRange::stepped(1, 128),
        ] {
            let steps = range.step_count();
            for k in 0..=steps {
                let n = f64::from(k) / f64::from(steps);
                let plain = range.denormalize(n);
                assert_eq!(plain, range.min() + f64::from(k));
                assert!(
                    (range.normalize(plain) - n).abs() <= 1e-9,
                    "{range:?} step {k}"
                );
            }
        }
    }

    #[test]
    fn quantised_ranges_are_idempotent_off_grid() {
        for range in [ParamRange::stepped(0, 4), ParamRange::Boolean] {
            for n in POSITIONS {
                let once = range.snap_normalized(n);
                let twice = range.snap_normalized(once);
                assert_eq!(once, twice, "{range:?} at {n}");
                assert_eq!(range.denormalize(once), range.denormalize(n));
            }
        }
    }

    #[test]
    fn stepped_quantisation_is_centred() {
        // Five values, so each interior step owns +-0.125 around its position.
        let range = ParamRange::stepped(0, 4);
        assert_eq!(range.denormalize(0.0), 0.0);
        assert_eq!(range.denormalize(0.124), 0.0);
        assert_eq!(range.denormalize(0.126), 1.0);
        assert_eq!(range.denormalize(0.25), 1.0);
        assert_eq!(range.denormalize(0.374), 1.0);
        assert_eq!(range.denormalize(0.376), 2.0);
        assert_eq!(range.denormalize(1.0), 4.0);
        // Endpoints keep half-width bins, which is what makes the extremes reachable.
        assert_eq!(range.normalize(0.0), 0.0);
        assert_eq!(range.normalize(4.0), 1.0);
    }

    #[test]
    fn boolean_maps_at_the_half() {
        let b = ParamRange::Boolean;
        assert_eq!(b.denormalize(0.0), 0.0);
        assert_eq!(b.denormalize(0.4999), 0.0);
        assert_eq!(b.denormalize(0.5), 1.0);
        assert_eq!(b.denormalize(1.0), 1.0);
        assert_eq!(b.normalize(0.0), 0.0);
        assert_eq!(b.normalize(0.49), 0.0);
        assert_eq!(b.normalize(1.0), 1.0);
        assert_eq!(b.normalize(17.0), 1.0);
        assert_eq!(b.step_count(), 1);
        assert!(b.is_stepped());
    }

    #[test]
    fn endpoints_are_exact() {
        let log = ParamRange::logarithmic(20.0, 20_000.0);
        assert_eq!(log.denormalize(0.0), 20.0);
        assert_eq!(log.denormalize(1.0), 20_000.0);
        assert_eq!(log.normalize(20.0), 0.0);
        assert_eq!(log.normalize(20_000.0), 1.0);

        let lin = ParamRange::linear(-60.0, 12.0);
        assert_eq!(lin.denormalize(0.0), -60.0);
        assert_eq!(lin.denormalize(1.0), 12.0);

        let skew = ParamRange::skewed(-60.0, 12.0, 0.3);
        assert_eq!(skew.denormalize(0.0), -60.0);
        assert_eq!(skew.denormalize(1.0), 12.0);
        assert_eq!(skew.normalize(-60.0), 0.0);
        assert_eq!(skew.normalize(12.0), 1.0);
    }

    #[test]
    fn logarithmic_is_ratio_uniform() {
        let r = ParamRange::logarithmic(20.0, 20_000.0);
        // Half the travel is the geometric mean.
        let mid = r.denormalize(0.5);
        assert!((mid - (20.0f64 * 20_000.0).sqrt()).abs() < 1e-9);
        // Equal ratios cover equal distance.
        let d1 = r.normalize(200.0) - r.normalize(100.0);
        let d2 = r.normalize(2000.0) - r.normalize(1000.0);
        assert!((d1 - d2).abs() < 1e-12);
    }

    #[test]
    fn skew_factor_below_one_favours_the_bottom() {
        let r = ParamRange::skewed(0.0, 100.0, 0.5);
        // Half the knob travel reaches a quarter of the value.
        assert!((r.denormalize(0.5) - 25.0).abs() < 1e-9);
        assert!((r.normalize(25.0) - 0.5).abs() < 1e-9);
        // factor == 1 must be indistinguishable from linear.
        let l = ParamRange::linear(0.0, 100.0);
        let s = ParamRange::skewed(0.0, 100.0, 1.0);
        for n in POSITIONS {
            assert!((l.denormalize(n) - s.denormalize(n)).abs() < 1e-12);
        }
    }

    #[test]
    fn clamping_and_out_of_range_input() {
        let r = ParamRange::linear(-60.0, 12.0);
        assert_eq!(r.clamp(-100.0), -60.0);
        assert_eq!(r.clamp(100.0), 12.0);
        assert_eq!(r.clamp(0.0), 0.0);
        assert_eq!(r.normalize(-100.0), 0.0);
        assert_eq!(r.normalize(100.0), 1.0);
        assert_eq!(r.denormalize(-3.0), -60.0);
        assert_eq!(r.denormalize(3.0), 12.0);

        // Inverted bounds still clamp to the actual interval.
        let inv = ParamRange::linear(12.0, -60.0);
        assert_eq!(inv.clamp(100.0), 12.0);
        assert_eq!(inv.clamp(-100.0), -60.0);
    }

    #[test]
    fn nan_never_escapes() {
        for r in [
            ParamRange::linear(-1.0, 1.0),
            ParamRange::skewed(-1.0, 1.0, 0.5),
            ParamRange::logarithmic(1.0, 10.0),
            ParamRange::stepped(-2, 2),
            ParamRange::Boolean,
        ] {
            assert!(r.normalize(f64::NAN).is_finite());
            assert!(r.denormalize(f64::NAN).is_finite());
            assert!(r.clamp(f64::NAN).is_finite());
            assert!(r.normalize(f64::INFINITY).is_finite());
            assert!(r.normalize(f64::NEG_INFINITY).is_finite());
            assert!(r.denormalize(f64::INFINITY).is_finite());
            assert_eq!(r.normalize(f64::INFINITY), 1.0);
            assert_eq!(r.normalize(f64::NEG_INFINITY), 0.0);
        }
    }

    #[test]
    fn degenerate_ranges_do_not_panic() {
        let empty = ParamRange::Linear { min: 5.0, max: 5.0 };
        assert_eq!(empty.validate(), Err(RangeError::EmptyRange));
        assert_eq!(empty.normalize(5.0), 0.0);
        assert_eq!(empty.denormalize(0.7), 5.0);

        let single_step = ParamRange::stepped(5, 5);
        assert_eq!(single_step.step_count(), 0);
        assert_eq!(single_step.normalize(5.0), 0.0);
        assert_eq!(single_step.denormalize(1.0), 5.0);
        assert_eq!(single_step.clamp(9.0), 5.0);
    }

    #[test]
    fn invalid_ranges_are_rejected_but_still_answer() {
        let bad_log = ParamRange::Logarithmic {
            min: 0.0,
            max: 1000.0,
        };
        assert_eq!(
            bad_log.validate(),
            Err(RangeError::NonPositiveLogarithmicBound)
        );
        assert_eq!(bad_log.normalize(100.0), 0.0);
        assert_eq!(bad_log.denormalize(0.5), 0.0);

        let neg_log = ParamRange::Logarithmic {
            min: -20.0,
            max: 20.0,
        };
        assert_eq!(
            neg_log.validate(),
            Err(RangeError::NonPositiveLogarithmicBound)
        );

        let bad_skew = ParamRange::Skewed {
            min: 0.0,
            max: 1.0,
            factor: 0.0,
        };
        assert_eq!(bad_skew.validate(), Err(RangeError::InvalidSkewFactor));
        assert_eq!(bad_skew.normalize(0.5), 0.0);
        assert_eq!(bad_skew.denormalize(0.5), 0.0);

        assert_eq!(
            ParamRange::Linear {
                min: f64::NAN,
                max: 1.0
            }
            .validate(),
            Err(RangeError::NotFinite)
        );
        assert_eq!(
            ParamRange::Stepped { min: 3, max: 1 }.validate(),
            Err(RangeError::InvertedStepRange)
        );
        assert_eq!(ParamRange::Boolean.validate(), Ok(()));
    }

    #[test]
    fn checked_constructors_report_instead_of_panicking() {
        assert!(ParamRange::try_logarithmic(0.0, 1.0).is_err());
        assert!(ParamRange::try_logarithmic(20.0, 20_000.0).is_ok());
        assert!(ParamRange::try_skewed(0.0, 1.0, -1.0).is_err());
        assert!(ParamRange::try_linear(1.0, 1.0).is_err());
        assert!(ParamRange::try_stepped(4, 2).is_err());
        assert!(ParamRange::try_stepped(2, 2).is_ok());
    }

    #[test]
    #[should_panic(expected = "logarithmic parameter range needs strictly positive bounds")]
    fn logarithmic_constructor_panics_on_zero() {
        let _ = ParamRange::logarithmic(0.0, 20_000.0);
    }

    #[test]
    #[should_panic(expected = "logarithmic parameter range needs strictly positive bounds")]
    fn logarithmic_constructor_panics_on_negative() {
        let _ = ParamRange::logarithmic(-1.0, 20_000.0);
    }

    #[test]
    fn extreme_step_ranges_do_not_overflow() {
        let r = ParamRange::stepped(i64::MIN, i64::MAX);
        // The span exceeds u32, so `step_count` saturates rather than wrapping.
        assert_eq!(r.step_count(), u32::MAX);
        assert_eq!(r.normalize(i64::MIN as f64), 0.0);
        assert_eq!(r.normalize(i64::MAX as f64), 1.0);
        assert_eq!(r.denormalize(0.0), i64::MIN as f64);
        assert_eq!(r.denormalize(1.0), i64::MAX as f64);
        assert!(r.denormalize(0.5).is_finite());
    }

    #[test]
    fn monotonicity_holds_across_the_curve() {
        for r in [
            ParamRange::linear(-60.0, 12.0),
            ParamRange::skewed(-60.0, 12.0, 0.3),
            ParamRange::skewed(-60.0, 12.0, 2.5),
            ParamRange::logarithmic(20.0, 20_000.0),
            ParamRange::stepped(0, 16),
        ] {
            let mut previous = f64::NEG_INFINITY;
            for i in 0..=1000 {
                let v = r.denormalize(f64::from(i) / 1000.0);
                assert!(v >= previous, "{r:?} is not monotonic at {i}");
                previous = v;
            }
        }
    }

    #[test]
    fn defaults_and_accessors() {
        assert_eq!(ParamRange::default(), ParamRange::UNIT);
        assert_eq!(ParamRange::UNIT.min(), 0.0);
        assert_eq!(ParamRange::UNIT.max(), 1.0);
        assert_eq!(ParamRange::stepped(-3, 4).min(), -3.0);
        assert_eq!(ParamRange::stepped(-3, 4).max(), 4.0);
        assert_eq!(ParamRange::stepped(-3, 4).step_count(), 7);
        assert_eq!(ParamRange::Boolean.min(), 0.0);
        assert_eq!(ParamRange::Boolean.max(), 1.0);
        assert!(!ParamRange::UNIT.is_stepped());
        assert_eq!(ParamRange::UNIT.step_count(), 0);
        assert_eq!(ParamRange::UNIT.snap_normalized(0.3), 0.3);
        assert_eq!(ParamRange::linear(-60.0, 12.0).bounds(), (-60.0, 12.0));
        assert_eq!(ParamRange::linear(12.0, -60.0).bounds(), (-60.0, 12.0));
        assert_eq!(ParamRange::Boolean.bounds(), (0.0, 1.0));
    }

    #[test]
    fn errors_display_usefully() {
        let msg = RangeError::NonPositiveLogarithmicBound.to_string();
        assert!(msg.contains("20_000.0"), "{msg}");
        assert!(!RangeError::InvalidSkewFactor.to_string().is_empty());
        assert!(!RangeError::NotFinite.to_string().is_empty());
        assert!(!RangeError::EmptyRange.to_string().is_empty());
        assert!(!RangeError::InvertedStepRange.to_string().is_empty());
    }
}
