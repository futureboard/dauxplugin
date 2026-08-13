//! Assertion helpers with failure messages worth reading.
//!
//! `assert_eq!` on two 512-sample buffers prints two screenfuls of floats and says nothing
//! about *where* they diverged. The helpers here report the first offending frame, its two
//! values and the tolerance, which is the difference between a five-second diagnosis and a
//! twenty-minute one.
//!
//! Measurement functions ([`peak`], [`rms`], [`all_finite`]) are `[audio-thread]`: they
//! allocate nothing and may be called inside a [`daux_rt::AllocGuard`] scope. The
//! `assert_*` functions format a message on failure and are therefore `[main-thread]` —
//! but only on the failing path, so an assertion that passes inside an `AllocGuard` scope
//! still counts zero allocations.

/// [audio-thread] The largest absolute sample value, or `0.0` for an empty slice.
#[must_use]
pub fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0f32, |acc, s| acc.max(s.abs()))
}

/// [audio-thread] Root mean square of `samples`, or `0.0` for an empty slice.
#[must_use]
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples.iter().map(|s| f64::from(*s) * f64::from(*s)).sum();
    (sum / samples.len() as f64).sqrt() as f32
}

/// [audio-thread] `true` when every sample is finite — neither NaN nor infinite.
#[must_use]
pub fn all_finite(samples: &[f32]) -> bool {
    samples.iter().all(|s| s.is_finite())
}

/// [main-thread] Asserts `actual` matches `expected` frame by frame within `tolerance`.
///
/// # Panics
///
/// If the lengths differ, or any pair differs by more than `tolerance`. The message names
/// `context`, the first differing frame index and both values.
#[track_caller]
pub fn assert_samples_close(actual: &[f32], expected: &[f32], tolerance: f32, context: &str) {
    assert_eq!(
        actual.len(),
        expected.len(),
        "{context}: length mismatch ({} vs {})",
        actual.len(),
        expected.len()
    );
    for (frame, (a, e)) in actual.iter().zip(expected).enumerate() {
        assert!(
            (a - e).abs() <= tolerance,
            "{context}: frame {frame} is {a} but should be {e} (tolerance {tolerance})"
        );
    }
}

/// [main-thread] Asserts every sample is finite.
///
/// A NaN in an output buffer is the single most destructive failure a plug-in has: it
/// spreads through every downstream summing bus and is inaudible until the master goes
/// silent.
///
/// # Panics
///
/// If any sample is NaN or infinite, naming the first one.
#[track_caller]
pub fn assert_all_finite(samples: &[f32], context: &str) {
    if let Some((frame, value)) = samples.iter().enumerate().find(|(_, s)| !s.is_finite()) {
        panic!("{context}: frame {frame} is {value}, which is not finite");
    }
}

/// [main-thread] Asserts the buffer holds nothing above `floor` in absolute value.
///
/// # Panics
///
/// If any sample exceeds `floor`, naming the first one.
#[track_caller]
pub fn assert_silent(samples: &[f32], floor: f32, context: &str) {
    if let Some((frame, value)) = samples.iter().enumerate().find(|(_, s)| s.abs() > floor) {
        panic!("{context}: frame {frame} is {value}, above the silence floor {floor}");
    }
}

/// [main-thread] Asserts the buffer holds something above `floor` in absolute value.
///
/// The counterpart of [`assert_silent`], and the one that catches the commonest fixture
/// bug of all: a test that "passes" because the plug-in produced nothing at all.
///
/// # Panics
///
/// If every sample is at or below `floor`.
#[track_caller]
pub fn assert_not_silent(samples: &[f32], floor: f32, context: &str) {
    let observed = peak(samples);
    assert!(
        observed > floor,
        "{context}: peak is {observed}, at or below the silence floor {floor} — \
         the fixture produced nothing, so nothing was really tested"
    );
}

/// [main-thread] Asserts two scalars agree within `tolerance`.
///
/// # Panics
///
/// If they differ by more than `tolerance`.
#[track_caller]
pub fn assert_close(actual: f64, expected: f64, tolerance: f64, context: &str) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "{context}: {actual} should be {expected} (tolerance {tolerance})"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peak_and_rms_agree_with_hand_calculation() {
        assert_eq!(peak(&[]), 0.0);
        assert_eq!(rms(&[]), 0.0);
        assert_eq!(peak(&[0.25, -0.75, 0.5]), 0.75);
        // A square wave's RMS is its amplitude.
        assert!((rms(&[1.0, -1.0, 1.0, -1.0]) - 1.0).abs() < 1e-6);
        assert!((rms(&[0.0, 0.0]) - 0.0).abs() < 1e-6);
    }

    #[test]
    fn finiteness_is_detected() {
        assert!(all_finite(&[0.0, 1.0, -1.0]));
        assert!(!all_finite(&[0.0, f32::NAN]));
        assert!(!all_finite(&[f32::INFINITY]));
        assert!(all_finite(&[]));
    }

    #[test]
    #[should_panic(expected = "frame 2 is 0.5 but should be 0")]
    fn a_sample_mismatch_names_the_frame() {
        assert_samples_close(&[0.0, 0.0, 0.5], &[0.0, 0.0, 0.0], 1e-6, "gain");
    }

    #[test]
    #[should_panic(expected = "length mismatch")]
    fn a_length_mismatch_is_reported_before_the_comparison() {
        assert_samples_close(&[0.0], &[0.0, 0.0], 1e-6, "gain");
    }

    #[test]
    #[should_panic(expected = "is not finite")]
    fn a_nan_is_caught() {
        assert_all_finite(&[0.0, f32::NAN], "output");
    }

    #[test]
    #[should_panic(expected = "the fixture produced nothing")]
    fn an_empty_fixture_is_caught_rather_than_passing_vacuously() {
        assert_not_silent(&[0.0; 16], 1e-6, "synth");
    }

    #[test]
    fn measurement_allocates_nothing() {
        let buffer = [0.5f32; 128];
        let ((), allocations) = daux_rt::AllocGuard::scope(|| {
            assert!(peak(&buffer) > 0.0);
            assert!(rms(&buffer) > 0.0);
            assert!(all_finite(&buffer));
            assert_not_silent(&buffer, 1e-9, "measurement");
            assert_samples_close(&buffer, &buffer, 0.0, "identity");
        });
        assert_eq!(allocations, 0);
    }
}
