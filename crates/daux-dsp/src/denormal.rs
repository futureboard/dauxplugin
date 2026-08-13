//! Denormal (subnormal) protection for recursive filter state.
//!
//! Every feedback path in this crate decays geometrically towards zero. Once the
//! state reaches the subnormal range some CPUs leave the fast path and trap into
//! microcode; a single plug-in instance can then cost tens of times its normal
//! budget. That is a real-time failure which, maddeningly, only appears *after*
//! the music stops — exactly when nobody is looking.
//!
//! Rather than depend on a process-wide flush-to-zero MXCSR/FPCR mode (which the
//! host owns, which no plug-in may change behind the host's back, and which does
//! not exist portably), every recursive structure here pushes its state through
//! these helpers. The comparison lowers to a compare plus a conditional move, so
//! there is no branch to mispredict in the audio loop.

/// Magnitudes strictly below this are flushed to zero by [`flush_denormal`].
///
/// `1e-30` sits roughly 600 dB below full scale: far beneath anything audible,
/// and still eight orders of magnitude above the `f32` subnormal range
/// (`~1.18e-38`), so the guard fires long before the CPU can slow down.
///
/// [any-thread]
pub const DENORMAL_THRESHOLD_F32: f32 = 1.0e-30;

/// Magnitudes strictly below this are flushed to zero by [`flush_denormal_f64`].
///
/// Chosen on the same principle as [`DENORMAL_THRESHOLD_F32`], relative to the
/// `f64` subnormal range (`~2.2e-308`).
///
/// [any-thread]
pub const DENORMAL_THRESHOLD_F64: f64 = 1.0e-300;

/// Flushes a subnormal-range magnitude to `+0.0`, leaving every other value
/// bit-identical. [audio-thread]
///
/// `NaN` is returned unchanged: the comparison `|x| < threshold` is false for
/// `NaN`, so this helper never *creates* a special value and never branches on
/// one. A `NaN` that reaches a filter's state stays there until `reset`.
///
/// ```
/// # use daux_dsp::flush_denormal;
/// assert_eq!(flush_denormal(1.0e-40), 0.0);
/// assert_eq!(flush_denormal(-1.0e-40), 0.0);
/// assert_eq!(flush_denormal(0.5), 0.5);
/// ```
#[inline]
#[must_use]
pub fn flush_denormal(x: f32) -> f32 {
    if x.abs() < DENORMAL_THRESHOLD_F32 {
        0.0
    } else {
        x
    }
}

/// `f64` counterpart of [`flush_denormal`]. [audio-thread]
///
/// ```
/// # use daux_dsp::flush_denormal_f64;
/// assert_eq!(flush_denormal_f64(1.0e-320), 0.0);
/// assert_eq!(flush_denormal_f64(0.5), 0.5);
/// ```
#[inline]
#[must_use]
pub fn flush_denormal_f64(x: f64) -> f64 {
    if x.abs() < DENORMAL_THRESHOLD_F64 {
        0.0
    } else {
        x
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flushes_subnormals_and_tiny_normals() {
        assert_eq!(flush_denormal(f32::from_bits(1)), 0.0);
        assert_eq!(flush_denormal(-f32::from_bits(1)), 0.0);
        assert_eq!(flush_denormal(f32::MIN_POSITIVE), 0.0);
        assert_eq!(flush_denormal(1.0e-31), 0.0);
    }

    #[test]
    fn preserves_audible_values_bit_exactly() {
        for &v in &[1.0e-20_f32, -1.0e-20, 1.0e-6, -0.5, 1.0, -1.0, 1.0e30] {
            assert_eq!(flush_denormal(v).to_bits(), v.to_bits());
        }
    }

    #[test]
    fn zero_maps_to_positive_zero() {
        assert_eq!(flush_denormal(0.0).to_bits(), 0.0_f32.to_bits());
        assert_eq!(flush_denormal(-0.0).to_bits(), 0.0_f32.to_bits());
    }

    #[test]
    fn infinities_and_nan_pass_through() {
        assert_eq!(flush_denormal(f32::INFINITY), f32::INFINITY);
        assert_eq!(flush_denormal(f32::NEG_INFINITY), f32::NEG_INFINITY);
        assert!(flush_denormal(f32::NAN).is_nan());
    }

    #[test]
    fn f64_variant_matches() {
        assert_eq!(flush_denormal_f64(f64::from_bits(1)), 0.0);
        assert_eq!(flush_denormal_f64(f64::MIN_POSITIVE), 0.0);
        assert_eq!(
            flush_denormal_f64(1.0e-299).to_bits(),
            1.0e-299_f64.to_bits()
        );
        assert!(flush_denormal_f64(f64::NAN).is_nan());
    }
}
