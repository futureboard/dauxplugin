//! Decibel ↔ linear-gain conversion.

/// `log2(10) / 20` — converts decibels to a base-2 exponent.
const DB_TO_EXP2_F64: f64 = 0.166_096_404_744_368_1;
/// `20 / log2(10)` — converts a base-2 exponent to decibels.
const EXP2_TO_DB_F64: f64 = 6.020_599_913_279_624;
/// [`DB_TO_EXP2_F64`] rounded once, at compile time, to `f32`.
const DB_TO_EXP2_F32: f32 = DB_TO_EXP2_F64 as f32;
/// [`EXP2_TO_DB_F64`] rounded once, at compile time, to `f32`.
const EXP2_TO_DB_F32: f32 = EXP2_TO_DB_F64 as f32;

/// Converts decibels to a linear gain factor: `10^(db / 20)`. [any-thread]
///
/// Implemented as `exp2(db * log2(10) / 20)`, which is materially cheaper than
/// `powf` and, unlike a lookup table, allocation-free and exact at the anchor
/// points: `db_to_gain(0.0)` is exactly `1.0`.
///
/// Boundary behaviour, all of it useful in a gain stage:
///
/// * `-inf dB` → `0.0` (true silence, no special case needed)
/// * `+inf dB` → `+inf`
/// * `NaN` → `NaN` (nothing is clamped or branched on)
///
/// ```
/// # use daux_dsp::db_to_gain;
/// assert_eq!(db_to_gain(0.0), 1.0);
/// assert_eq!(db_to_gain(f32::NEG_INFINITY), 0.0);
/// assert!((db_to_gain(-6.020_6) - 0.5).abs() < 1.0e-5);
/// ```
#[inline]
#[must_use]
pub fn db_to_gain(db: f32) -> f32 {
    (db * DB_TO_EXP2_F32).exp2()
}

/// Converts a linear gain factor to decibels: `20 * log10(|gain|)`. [any-thread]
///
/// The *magnitude* is used, so a polarity-inverted gain reports the same level
/// as its positive twin and the function never produces `NaN` from a negative
/// input. `gain_to_db(1.0)` is exactly `0.0` and `gain_to_db(0.0)` is
/// `-inf` — the natural inverse of [`db_to_gain`], with no arbitrary floor
/// baked in. Clamp the result yourself if a meter needs a finite bottom.
///
/// ```
/// # use daux_dsp::gain_to_db;
/// assert_eq!(gain_to_db(1.0), 0.0);
/// assert_eq!(gain_to_db(0.0), f32::NEG_INFINITY);
/// assert_eq!(gain_to_db(-2.0), gain_to_db(2.0));
/// ```
#[inline]
#[must_use]
pub fn gain_to_db(gain: f32) -> f32 {
    gain.abs().log2() * EXP2_TO_DB_F32
}

/// `f64` counterpart of [`db_to_gain`]. [any-thread]
#[inline]
#[must_use]
pub fn db_to_gain_f64(db: f64) -> f64 {
    (db * DB_TO_EXP2_F64).exp2()
}

/// `f64` counterpart of [`gain_to_db`]. [any-thread]
#[inline]
#[must_use]
pub fn gain_to_db_f64(gain: f64) -> f64 {
    gain.abs().log2() * EXP2_TO_DB_F64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unity_is_exact_in_both_directions() {
        assert_eq!(db_to_gain(0.0), 1.0_f32);
        assert_eq!(gain_to_db(1.0), 0.0_f32);
        assert_eq!(db_to_gain_f64(0.0), 1.0_f64);
        assert_eq!(gain_to_db_f64(1.0), 0.0_f64);
    }

    #[test]
    fn known_anchor_points() {
        assert!((db_to_gain(20.0) - 10.0).abs() < 1.0e-4);
        assert!((db_to_gain(-20.0) - 0.1).abs() < 1.0e-6);
        assert!((db_to_gain(6.020_6) - 2.0).abs() < 1.0e-5);
        assert!((gain_to_db(10.0) - 20.0).abs() < 1.0e-4);
        assert!((gain_to_db(0.5) + 6.020_6).abs() < 1.0e-3);
    }

    #[test]
    fn silence_and_infinity() {
        assert_eq!(db_to_gain(f32::NEG_INFINITY), 0.0);
        assert_eq!(db_to_gain(f32::INFINITY), f32::INFINITY);
        assert_eq!(gain_to_db(0.0), f32::NEG_INFINITY);
        assert_eq!(gain_to_db(-0.0), f32::NEG_INFINITY);
        assert_eq!(gain_to_db(f32::INFINITY), f32::INFINITY);
        assert_eq!(db_to_gain_f64(f64::NEG_INFINITY), 0.0);
        assert_eq!(gain_to_db_f64(0.0), f64::NEG_INFINITY);
    }

    #[test]
    fn negative_gain_uses_magnitude() {
        assert_eq!(gain_to_db(-1.0), 0.0);
        assert_eq!(gain_to_db(-4.0), gain_to_db(4.0));
        assert_eq!(gain_to_db_f64(-4.0), gain_to_db_f64(4.0));
        assert!(!gain_to_db(-0.25).is_nan());
    }

    #[test]
    fn nan_propagates_without_panicking() {
        assert!(db_to_gain(f32::NAN).is_nan());
        assert!(gain_to_db(f32::NAN).is_nan());
    }

    #[test]
    fn round_trips_across_the_useful_range() {
        let mut db = -120.0_f32;
        while db <= 24.0 {
            let back = gain_to_db(db_to_gain(db));
            assert!(
                (back - db).abs() < 2.0e-3,
                "db {db} round-tripped to {back}"
            );
            db += 0.5;
        }
    }

    #[test]
    fn f64_round_trip_is_tighter_than_f32() {
        let mut db = -300.0_f64;
        while db <= 60.0 {
            let back = gain_to_db_f64(db_to_gain_f64(db));
            assert!(
                (back - db).abs() < 1.0e-10,
                "db {db} round-tripped to {back}"
            );
            db += 0.25;
        }
    }

    #[test]
    fn monotonic_in_db() {
        let mut prev = db_to_gain(-90.0);
        let mut db = -89.5_f32;
        while db <= 30.0 {
            let g = db_to_gain(db);
            assert!(g > prev, "not monotonic at {db}");
            prev = g;
            db += 0.5;
        }
    }
}
