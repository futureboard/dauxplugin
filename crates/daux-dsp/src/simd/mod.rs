//! Runtime-dispatched vector helpers: always correct, never required.
//!
//! Nothing in DAUxPlug *depends* on these. They exist so that the handful of
//! operations every plug-in does to whole blocks — apply a gain, ramp a gain
//! across a block, mix a bus in, copy a bus out, meter a peak — run at the width
//! of the CPU the user actually has, from a binary that also boots on the CPU
//! they had five years ago.
//!
//! # Dispatch
//!
//! One `OnceLock<Isa>` holds the answer to "what does this CPU have", decided by
//! `is_x86_feature_detected!` on x86 and by `cfg(target_arch)` on AArch64, where
//! Advanced SIMD is architecturally mandatory and there is nothing to ask. Every
//! entry point below reads that cell and branches to a kernel. A binary built on
//! an AVX2 machine and run on a pre-AVX one takes the SSE2 path; it does not
//! fault, because the AVX2 code is never entered.
//!
//! After the first call the read is a single acquire load with no lock, so these
//! functions are audio-thread safe. The *first* call performs the detection and
//! could in principle contend with another thread doing the same — call
//! [`prime`] from `prepare` and the audio thread never sees it.
//!
//! # Agreement between paths
//!
//! Every kernel produces **bit-identical** results to the scalar reference:
//!
//! * [`copy_from`] and [`add_from`] are element-wise, and IEEE-754 addition of
//!   corresponding elements does not care how the elements were grouped.
//! * [`apply_gain`] is an element-wise multiply, likewise.
//! * [`apply_gain_ramp`] computes each coefficient as `from + step * i` from the
//!   absolute index, never by accumulating a step, so lanes and the scalar loop
//!   evaluate the same expression. No fused multiply-add is used anywhere, since
//!   contracting the multiply and the add would change the rounding.
//! * [`peak_abs`] is a maximum, which is associative and commutative over
//!   non-`NaN` values. `NaN` is ignored identically on every path: the scalar
//!   loop tests `a > m`, and the vector kernels put the candidate first in
//!   `MAXPS` / `FMAXNM`, which yields the accumulator when the comparison is
//!   unordered.
//!
//! The unit tests assert exact equality, not a tolerance, for every kernel at
//! every length from 0 through the vector-width boundaries and beyond.
//!
//! # Length mismatches
//!
//! [`copy_from`] and [`add_from`] operate on `min(dst.len(), src.len())`
//! elements. A mismatch is a caller bug, and `debug_assert_eq!` says so in
//! development builds, but a release build must not panic on the audio thread
//! for it, so it silently does the well-defined thing.

use std::sync::OnceLock;

mod scalar;

#[cfg(all(feature = "simd", any(target_arch = "x86", target_arch = "x86_64")))]
mod x86;

#[cfg(all(feature = "simd", target_arch = "aarch64"))]
mod neon;

/// The instruction set the kernels below will use.
///
/// Variants exist only where they could be selected, so a build for one
/// architecture carries no dead arms for another.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Isa {
    /// Portable code; always available.
    Scalar,
    /// 4-lane SSE2.
    #[cfg(all(feature = "simd", any(target_arch = "x86", target_arch = "x86_64")))]
    Sse2,
    /// 8-lane AVX2.
    #[cfg(all(feature = "simd", any(target_arch = "x86", target_arch = "x86_64")))]
    Avx2,
    /// 4-lane AArch64 Advanced SIMD.
    #[cfg(all(feature = "simd", target_arch = "aarch64"))]
    Neon,
}

/// Cached answer to the detection below. Written at most once.
static ISA: OnceLock<Isa> = OnceLock::new();

/// Asks the CPU what it supports. Called at most once per process.
fn detect() -> Isa {
    #[cfg(all(feature = "simd", any(target_arch = "x86", target_arch = "x86_64")))]
    {
        // Widest first. AVX2 implies AVX and SSE2; the kernels use only AVX
        // floating-point instructions, but gating on AVX2 keeps the check to
        // the single feature name the contract specifies and rules out the
        // early AVX-without-AVX2 parts where 256-bit code was a wash anyway.
        if std::is_x86_feature_detected!("avx2") {
            return Isa::Avx2;
        }
        // SSE2 is part of the x86-64 baseline, so this is only ever false on
        // 32-bit x86 — where it genuinely can be.
        if std::is_x86_feature_detected!("sse2") {
            return Isa::Sse2;
        }
        Isa::Scalar
    }

    // Advanced SIMD is mandatory in ARMv8-A, so there is no CPU feature to
    // probe at runtime. It can still be switched off at *build* time — with
    // `-C target-feature=-neon`, or on a soft-float bare-metal target — and
    // then the vector registers may genuinely not be usable. Honour the build's
    // own view rather than assuming the architecture's.
    #[cfg(all(feature = "simd", target_arch = "aarch64"))]
    {
        if cfg!(target_feature = "neon") {
            Isa::Neon
        } else {
            Isa::Scalar
        }
    }

    #[cfg(not(all(
        feature = "simd",
        any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")
    )))]
    {
        Isa::Scalar
    }
}

/// Reads the cached ISA, detecting it on the first call.
#[inline]
fn isa() -> Isa {
    *ISA.get_or_init(detect)
}

/// Forces CPU feature detection now, so no audio-thread call has to do it.
/// [main-thread]
///
/// Detection is idempotent and cheap; calling this from `prepare` (or anywhere
/// else before the first block) simply guarantees that every later call is a
/// plain atomic load.
pub fn prime() {
    let _ = isa();
}

/// Name of the kernel family in use: `"scalar"`, `"sse2"`, `"avx2"` or
/// `"neon"`. [any-thread]
///
/// Intended for logs and `daux inspect` output. Triggers detection if it has
/// not happened yet.
#[must_use]
pub fn dispatch_name() -> &'static str {
    match isa() {
        Isa::Scalar => "scalar",
        #[cfg(all(feature = "simd", any(target_arch = "x86", target_arch = "x86_64")))]
        Isa::Sse2 => "sse2",
        #[cfg(all(feature = "simd", any(target_arch = "x86", target_arch = "x86_64")))]
        Isa::Avx2 => "avx2",
        #[cfg(all(feature = "simd", target_arch = "aarch64"))]
        Isa::Neon => "neon",
    }
}

/// Multiplies every sample in `buf` by `gain`. [audio-thread]
///
/// An empty slice is a no-op.
#[inline]
pub fn apply_gain(buf: &mut [f32], gain: f32) {
    match isa() {
        Isa::Scalar => scalar::apply_gain(buf, gain),
        #[cfg(all(feature = "simd", any(target_arch = "x86", target_arch = "x86_64")))]
        Isa::Sse2 => {
            // SAFETY: `Isa::Sse2` is produced only by `detect`, and only after
            // `is_x86_feature_detected!("sse2")` returned true for the CPU
            // executing this process — exactly the kernel's precondition.
            unsafe { x86::apply_gain_sse2(buf, gain) }
        }
        #[cfg(all(feature = "simd", any(target_arch = "x86", target_arch = "x86_64")))]
        Isa::Avx2 => {
            // SAFETY: `Isa::Avx2` is produced only by `detect`, and only after
            // `is_x86_feature_detected!("avx2")` returned true for the CPU
            // executing this process — exactly the kernel's precondition.
            unsafe { x86::apply_gain_avx2(buf, gain) }
        }
        #[cfg(all(feature = "simd", target_arch = "aarch64"))]
        Isa::Neon => {
            // SAFETY: `Isa::Neon` is produced only under
            // `cfg(target_arch = "aarch64")`, where Advanced SIMD is
            // architecturally mandatory, so the kernel's precondition holds for
            // every CPU that can run this code at all.
            unsafe { neon::apply_gain_neon(buf, gain) }
        }
    }
}

/// Applies a linear gain ramp across `buf`. [audio-thread]
///
/// Sample `i` of `len` is multiplied by `from + (to - from) * i / len`. The
/// denominator is `len`, not `len - 1`, so the ramp stops one step short of
/// `to`: consecutive blocks ramped `a → b` then `b → c` join without a
/// discontinuity, which is what a per-block parameter smoother needs.
///
/// An empty slice is a no-op; a one-element slice is multiplied by `from`.
#[inline]
pub fn apply_gain_ramp(buf: &mut [f32], from: f32, to: f32) {
    match isa() {
        Isa::Scalar => scalar::apply_gain_ramp(buf, from, to),
        #[cfg(all(feature = "simd", any(target_arch = "x86", target_arch = "x86_64")))]
        Isa::Sse2 => {
            // SAFETY: `Isa::Sse2` implies `is_x86_feature_detected!("sse2")`
            // returned true for this CPU; see `apply_gain`.
            unsafe { x86::apply_gain_ramp_sse2(buf, from, to) }
        }
        #[cfg(all(feature = "simd", any(target_arch = "x86", target_arch = "x86_64")))]
        Isa::Avx2 => {
            // SAFETY: `Isa::Avx2` implies `is_x86_feature_detected!("avx2")`
            // returned true for this CPU; see `apply_gain`.
            unsafe { x86::apply_gain_ramp_avx2(buf, from, to) }
        }
        #[cfg(all(feature = "simd", target_arch = "aarch64"))]
        Isa::Neon => {
            // SAFETY: `Isa::Neon` occurs only on AArch64, where Advanced SIMD is
            // architecturally mandatory; see `apply_gain`.
            unsafe { neon::apply_gain_ramp_neon(buf, from, to) }
        }
    }
}

/// Adds `src` into `dst`, element-wise. [audio-thread]
///
/// Processes `min(dst.len(), src.len())` samples; a length mismatch trips a
/// `debug_assert` but never panics in release.
#[inline]
pub fn add_from(dst: &mut [f32], src: &[f32]) {
    debug_assert_eq!(dst.len(), src.len(), "add_from length mismatch");
    match isa() {
        Isa::Scalar => scalar::add_from(dst, src),
        #[cfg(all(feature = "simd", any(target_arch = "x86", target_arch = "x86_64")))]
        Isa::Sse2 => {
            // SAFETY: `Isa::Sse2` implies `is_x86_feature_detected!("sse2")`
            // returned true for this CPU; see `apply_gain`.
            unsafe { x86::add_from_sse2(dst, src) }
        }
        #[cfg(all(feature = "simd", any(target_arch = "x86", target_arch = "x86_64")))]
        Isa::Avx2 => {
            // SAFETY: `Isa::Avx2` implies `is_x86_feature_detected!("avx2")`
            // returned true for this CPU; see `apply_gain`.
            unsafe { x86::add_from_avx2(dst, src) }
        }
        #[cfg(all(feature = "simd", target_arch = "aarch64"))]
        Isa::Neon => {
            // SAFETY: `Isa::Neon` occurs only on AArch64, where Advanced SIMD is
            // architecturally mandatory; see `apply_gain`.
            unsafe { neon::add_from_neon(dst, src) }
        }
    }
}

/// Copies `src` into `dst`. [audio-thread]
///
/// Processes `min(dst.len(), src.len())` samples; a length mismatch trips a
/// `debug_assert` but never panics in release.
///
/// There is deliberately no hand-written vector kernel here. This lowers to
/// `memcpy`, which every platform already implements with the widest moves it
/// has, non-temporal hints included — a hand-rolled loop would only be slower.
#[inline]
pub fn copy_from(dst: &mut [f32], src: &[f32]) {
    debug_assert_eq!(dst.len(), src.len(), "copy_from length mismatch");
    scalar::copy_from(dst, src);
}

/// Largest absolute sample value in `buf`, or `0.0` if it is empty.
/// [audio-thread]
///
/// `NaN` samples are ignored on every dispatch path; infinities are not.
#[inline]
#[must_use]
pub fn peak_abs(buf: &[f32]) -> f32 {
    match isa() {
        Isa::Scalar => scalar::peak_abs(buf),
        #[cfg(all(feature = "simd", any(target_arch = "x86", target_arch = "x86_64")))]
        Isa::Sse2 => {
            // SAFETY: `Isa::Sse2` implies `is_x86_feature_detected!("sse2")`
            // returned true for this CPU; see `apply_gain`.
            unsafe { x86::peak_abs_sse2(buf) }
        }
        #[cfg(all(feature = "simd", any(target_arch = "x86", target_arch = "x86_64")))]
        Isa::Avx2 => {
            // SAFETY: `Isa::Avx2` implies `is_x86_feature_detected!("avx2")`
            // returned true for this CPU; see `apply_gain`.
            unsafe { x86::peak_abs_avx2(buf) }
        }
        #[cfg(all(feature = "simd", target_arch = "aarch64"))]
        Isa::Neon => {
            // SAFETY: `Isa::Neon` occurs only on AArch64, where Advanced SIMD is
            // architecturally mandatory; see `apply_gain`.
            unsafe { neon::peak_abs_neon(buf) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lengths that straddle every vector-width boundary in the crate (4 and 8),
    /// including zero, one, and a long odd block.
    const LENS: &[usize] = &[
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 11, 15, 16, 17, 23, 24, 31, 32, 33, 64, 65, 100, 127, 128,
        129, 257, 1_000,
    ];

    /// A signal with mixed signs, a wide dynamic range and no repeating pattern.
    fn signal(len: usize, phase: f32) -> Vec<f32> {
        (0..len)
            .map(|i| {
                let t = i as f32 * 0.37 + phase;
                t.sin() * (1.0 + t.cos() * 0.5) - 0.125
            })
            .collect()
    }

    // -- reference behaviour of the scalar kernels ---------------------------

    #[test]
    fn scalar_apply_gain_is_a_plain_multiply() {
        let mut buf = signal(10, 0.0);
        let expected: Vec<f32> = buf.iter().map(|&x| x * 0.5).collect();
        scalar::apply_gain(&mut buf, 0.5);
        assert_eq!(buf, expected);
    }

    #[test]
    fn scalar_ramp_starts_at_from_and_stops_one_step_short_of_to() {
        let mut buf = vec![1.0_f32; 4];
        scalar::apply_gain_ramp(&mut buf, 0.0, 1.0);
        assert_eq!(buf, vec![0.0, 0.25, 0.5, 0.75]);
    }

    #[test]
    fn scalar_single_element_ramp_uses_from() {
        let mut buf = [1.0_f32];
        scalar::apply_gain_ramp(&mut buf, 0.25, 0.75);
        assert_eq!(buf, [0.25]);
    }

    #[test]
    fn consecutive_ramps_join_without_a_step() {
        // The whole reason the denominator is `len`: block boundaries must be
        // continuous when a smoother hands over `a -> b` then `b -> c`.
        let mut first = vec![1.0_f32; 8];
        let mut second = vec![1.0_f32; 8];
        apply_gain_ramp(&mut first, 0.0, 0.5);
        apply_gain_ramp(&mut second, 0.5, 1.0);
        let step = first[1] - first[0];
        assert!((second[0] - (first[7] + step)).abs() < 1.0e-6);
    }

    #[test]
    fn peak_abs_of_an_empty_slice_is_zero() {
        assert_eq!(peak_abs(&[]), 0.0);
        assert_eq!(scalar::peak_abs(&[]), 0.0);
    }

    #[test]
    fn peak_abs_finds_negative_extremes() {
        assert_eq!(peak_abs(&[0.1, -0.9, 0.3]), 0.9);
        assert_eq!(peak_abs(&[-0.0, 0.0]), 0.0);
        assert_eq!(peak_abs(&[f32::INFINITY, 1.0]), f32::INFINITY);
    }

    #[test]
    fn peak_abs_ignores_nan_on_the_dispatched_path() {
        // Long enough to exercise the vector body as well as the tail.
        let mut buf = signal(64, 0.0);
        buf[3] = f32::NAN;
        buf[62] = f32::NAN;
        let clean: Vec<f32> = buf.iter().copied().filter(|v| !v.is_nan()).collect();
        assert_eq!(peak_abs(&buf), scalar::peak_abs(&clean));
    }

    // -- length handling shared by every path -------------------------------

    #[test]
    fn empty_slices_are_no_ops_everywhere() {
        apply_gain(&mut [], 2.0);
        apply_gain_ramp(&mut [], 0.0, 1.0);
        add_from(&mut [], &[]);
        copy_from(&mut [], &[]);
        assert_eq!(peak_abs(&[]), 0.0);
    }

    #[test]
    fn mismatched_lengths_use_the_shorter_slice() {
        // `debug_assert_eq!` fires in test builds, so exercise the kernels
        // directly rather than the checked entry points.
        let mut dst = vec![1.0_f32; 8];
        scalar::add_from(&mut dst, &[1.0; 3]);
        assert_eq!(dst, vec![2.0, 2.0, 2.0, 1.0, 1.0, 1.0, 1.0, 1.0]);

        let mut dst = vec![9.0_f32; 3];
        scalar::copy_from(&mut dst, &[1.0; 8]);
        assert_eq!(dst, vec![1.0, 1.0, 1.0]);

        let mut dst = vec![9.0_f32; 5];
        scalar::copy_from(&mut dst, &[1.0; 2]);
        assert_eq!(dst, vec![1.0, 1.0, 9.0, 9.0, 9.0]);
    }

    // -- dispatch ------------------------------------------------------------

    #[test]
    fn dispatch_name_is_one_of_the_documented_values() {
        prime();
        let name = dispatch_name();
        assert!(
            matches!(name, "scalar" | "sse2" | "avx2" | "neon"),
            "unexpected dispatch name {name}"
        );
        // Detection is cached, so it must never change within a process.
        assert_eq!(name, dispatch_name());
    }

    #[test]
    fn dispatched_entry_points_agree_with_the_scalar_reference() {
        for &len in LENS {
            let mut a = signal(len, 0.0);
            let mut b = a.clone();
            scalar::apply_gain(&mut a, 0.37);
            apply_gain(&mut b, 0.37);
            assert_eq!(a, b, "apply_gain len {len}");

            let mut a = signal(len, 0.0);
            let mut b = a.clone();
            scalar::apply_gain_ramp(&mut a, 0.25, 1.75);
            apply_gain_ramp(&mut b, 0.25, 1.75);
            assert_eq!(a, b, "apply_gain_ramp len {len}");

            let src = signal(len, 1.7);
            let mut a = signal(len, 0.0);
            let mut b = a.clone();
            scalar::add_from(&mut a, &src);
            add_from(&mut b, &src);
            assert_eq!(a, b, "add_from len {len}");

            let mut a = vec![0.0_f32; len];
            let mut b = vec![0.0_f32; len];
            scalar::copy_from(&mut a, &src);
            copy_from(&mut b, &src);
            assert_eq!(a, b, "copy_from len {len}");
            assert_eq!(a, src, "copy_from must reproduce src, len {len}");

            assert_eq!(scalar::peak_abs(&src), peak_abs(&src), "peak_abs len {len}");
        }
    }

    // -- every backend, whether or not it is the dispatched one --------------

    /// Runs one kernel family against the scalar reference at every length in
    /// [`LENS`], asserting bit-identical results.
    ///
    /// The four closures are the backend's `apply_gain`, `apply_gain_ramp`,
    /// `add_from` and `peak_abs`.
    fn compare_backend(
        name: &str,
        gain: impl Fn(&mut [f32], f32),
        ramp: impl Fn(&mut [f32], f32, f32),
        add: impl Fn(&mut [f32], &[f32]),
        peak: impl Fn(&[f32]) -> f32,
    ) {
        for &len in LENS {
            let mut want = signal(len, 0.0);
            let mut got = want.clone();
            scalar::apply_gain(&mut want, -0.618);
            gain(&mut got, -0.618);
            assert_eq!(want, got, "{name} apply_gain len {len}");

            let mut want = signal(len, 0.0);
            let mut got = want.clone();
            scalar::apply_gain_ramp(&mut want, 0.125, 2.0);
            ramp(&mut got, 0.125, 2.0);
            assert_eq!(want, got, "{name} apply_gain_ramp len {len}");

            // A descending ramp, and one that does not move at all.
            let mut want = signal(len, 0.4);
            let mut got = want.clone();
            scalar::apply_gain_ramp(&mut want, 1.0, 0.0);
            ramp(&mut got, 1.0, 0.0);
            assert_eq!(want, got, "{name} descending ramp len {len}");

            let mut want = signal(len, 0.4);
            let mut got = want.clone();
            scalar::apply_gain_ramp(&mut want, 0.5, 0.5);
            ramp(&mut got, 0.5, 0.5);
            assert_eq!(want, got, "{name} flat ramp len {len}");

            let src = signal(len, 2.9);
            let mut want = signal(len, 0.0);
            let mut got = want.clone();
            scalar::add_from(&mut want, &src);
            add(&mut got, &src);
            assert_eq!(want, got, "{name} add_from len {len}");

            assert_eq!(
                scalar::peak_abs(&src),
                peak(&src),
                "{name} peak_abs len {len}"
            );

            // Peak on a buffer whose maximum lives in the scalar tail.
            let mut tail_heavy = vec![0.25_f32; len];
            if let Some(last) = tail_heavy.last_mut() {
                *last = -7.5;
            }
            assert_eq!(
                scalar::peak_abs(&tail_heavy),
                peak(&tail_heavy),
                "{name} peak_abs tail len {len}"
            );

            // And one containing NaN, which every path must ignore.
            let mut nan_buf = signal(len, 1.1);
            if len > 2 {
                nan_buf[len / 2] = f32::NAN;
                nan_buf[len - 1] = f32::NAN;
            }
            assert_eq!(
                scalar::peak_abs(&nan_buf),
                peak(&nan_buf),
                "{name} peak_abs NaN len {len}"
            );
        }
    }

    #[test]
    fn scalar_backend_is_self_consistent() {
        compare_backend(
            "scalar",
            scalar::apply_gain,
            scalar::apply_gain_ramp,
            scalar::add_from,
            scalar::peak_abs,
        );
    }

    #[cfg(all(feature = "simd", any(target_arch = "x86", target_arch = "x86_64")))]
    #[test]
    fn sse2_backend_matches_scalar_bit_for_bit() {
        if !std::is_x86_feature_detected!("sse2") {
            eprintln!("skipping: no SSE2 on this CPU");
            return;
        }
        compare_backend(
            "sse2",
            // SAFETY: guarded by the `is_x86_feature_detected!("sse2")` check
            // immediately above, which is the kernels' only precondition.
            |b, g| unsafe { x86::apply_gain_sse2(b, g) },
            // SAFETY: as above.
            |b, f, t| unsafe { x86::apply_gain_ramp_sse2(b, f, t) },
            // SAFETY: as above.
            |d, s| unsafe { x86::add_from_sse2(d, s) },
            // SAFETY: as above.
            |b| unsafe { x86::peak_abs_sse2(b) },
        );
    }

    #[cfg(all(feature = "simd", any(target_arch = "x86", target_arch = "x86_64")))]
    #[test]
    fn avx2_backend_matches_scalar_bit_for_bit() {
        if !std::is_x86_feature_detected!("avx2") {
            eprintln!("skipping: no AVX2 on this CPU");
            return;
        }
        compare_backend(
            "avx2",
            // SAFETY: guarded by the `is_x86_feature_detected!("avx2")` check
            // immediately above, which is the kernels' only precondition.
            |b, g| unsafe { x86::apply_gain_avx2(b, g) },
            // SAFETY: as above.
            |b, f, t| unsafe { x86::apply_gain_ramp_avx2(b, f, t) },
            // SAFETY: as above.
            |d, s| unsafe { x86::add_from_avx2(d, s) },
            // SAFETY: as above.
            |b| unsafe { x86::peak_abs_avx2(b) },
        );
    }

    #[cfg(all(feature = "simd", target_arch = "aarch64"))]
    #[test]
    fn neon_backend_matches_scalar_bit_for_bit() {
        compare_backend(
            "neon",
            // SAFETY: Advanced SIMD is architecturally mandatory on AArch64, so
            // the kernels' precondition holds wherever this code can run.
            |b, g| unsafe { neon::apply_gain_neon(b, g) },
            // SAFETY: as above.
            |b, f, t| unsafe { neon::apply_gain_ramp_neon(b, f, t) },
            // SAFETY: as above.
            |d, s| unsafe { neon::add_from_neon(d, s) },
            // SAFETY: as above.
            |b| unsafe { neon::peak_abs_neon(b) },
        );
    }

    #[cfg(all(feature = "simd", any(target_arch = "x86", target_arch = "x86_64")))]
    #[test]
    fn simd_kernels_respect_slice_bounds_exactly() {
        // Guard elements on both sides catch an off-by-one in the vector body
        // or the tail: a kernel that writes one element past the end would
        // corrupt the trailing sentinel.
        for &len in LENS {
            let mut owned = vec![-99.0_f32; len + 2];
            owned[1..=len].copy_from_slice(&signal(len, 0.0));

            if std::is_x86_feature_detected!("sse2") {
                // SAFETY: SSE2 checked immediately above.
                unsafe { x86::apply_gain_sse2(&mut owned[1..=len], 2.0) };
            }
            if std::is_x86_feature_detected!("avx2") {
                // SAFETY: AVX2 checked immediately above.
                unsafe { x86::apply_gain_avx2(&mut owned[1..=len], 2.0) };
            }
            assert_eq!(owned[0], -99.0, "underrun at len {len}");
            assert_eq!(owned[len + 1], -99.0, "overrun at len {len}");
        }
    }
}
