//! SSE2 and AVX2 kernels for x86 and x86-64.
//!
//! Every function here is `unsafe` and carries the same precondition: the
//! caller must have established, through `is_x86_feature_detected!`, that the
//! named feature exists on the CPU actually executing the code. [`super::isa`]
//! is the only caller, and it derives that fact from exactly that macro, cached
//! once in a `OnceLock`. Nothing else in the crate may call into this module.
//!
//! Each kernel is shaped identically: a vector body over `len & !(width - 1)`
//! elements, then a scalar tail. The tail duplicates the scalar reference
//! arithmetic verbatim rather than calling into it, so a tail element and a
//! vector element go through the same expression and produce the same bits.
//!
//! Loads and stores are unaligned (`loadu` / `storeu`). Slices from a host's
//! audio buffer carry no alignment guarantee beyond `align_of::<f32>()`, and on
//! every CPU that matters an unaligned load to an aligned address costs the same
//! as an aligned one — the aligned forms would buy nothing and could fault.

#[cfg(target_arch = "x86")]
use core::arch::x86::*;
#[cfg(target_arch = "x86_64")]
use core::arch::x86_64::*;

// ---------------------------------------------------------------------------
// SSE2 — 4 lanes
// ---------------------------------------------------------------------------

/// SSE2 [`super::apply_gain`].
///
/// # Safety
///
/// The CPU must support SSE2.
#[target_feature(enable = "sse2")]
pub unsafe fn apply_gain_sse2(buf: &mut [f32], gain: f32) {
    let len = buf.len();
    let body = len & !3;
    let p = buf.as_mut_ptr();
    // SAFETY: `p` is the base of a live, exclusively borrowed `[f32; len]`, so
    // `p.add(i)` for `i + 4 <= body <= len` addresses four initialised, writable
    // `f32`s inside that allocation. `loadu`/`storeu` require no alignment
    // beyond a byte. The `&mut` borrow rules out any concurrent reader or
    // writer, and `p` is not used after this block, so it cannot outlive the
    // borrow. SSE2 is guaranteed by `#[target_feature]` plus the caller's
    // runtime detection.
    unsafe {
        let g = _mm_set1_ps(gain);
        let mut i = 0;
        while i < body {
            let q = p.add(i);
            _mm_storeu_ps(q, _mm_mul_ps(_mm_loadu_ps(q), g));
            i += 4;
        }
    }
    for x in &mut buf[body..] {
        *x *= gain;
    }
}

/// SSE2 [`super::apply_gain_ramp`].
///
/// # Safety
///
/// The CPU must support SSE2.
#[target_feature(enable = "sse2")]
pub unsafe fn apply_gain_ramp_sse2(buf: &mut [f32], from: f32, to: f32) {
    let len = buf.len();
    if len == 0 {
        return;
    }
    let step = (to - from) / len as f32;
    let body = len & !3;
    let p = buf.as_mut_ptr();
    // SAFETY: as in `apply_gain_sse2` — `p.add(i)` with `i + 4 <= body <= len`
    // stays inside the exclusively borrowed slice, the accesses are unaligned,
    // and SSE2 is guaranteed by `#[target_feature]` plus the caller's runtime
    // detection. `p` does not escape the block.
    unsafe {
        let from_v = _mm_set1_ps(from);
        let step_v = _mm_set1_ps(step);
        // `_mm_set_ps` takes lanes from high to low.
        let lane = _mm_set_ps(3.0, 2.0, 1.0, 0.0);
        let mut i = 0;
        while i < body {
            // Recomputed from `i` rather than accumulated, so lane `i + k` gets
            // `from + step * (i + k)` exactly as the scalar kernel would.
            let idx = _mm_add_ps(_mm_set1_ps(i as f32), lane);
            let g = _mm_add_ps(from_v, _mm_mul_ps(step_v, idx));
            let q = p.add(i);
            _mm_storeu_ps(q, _mm_mul_ps(_mm_loadu_ps(q), g));
            i += 4;
        }
    }
    for (k, x) in buf[body..].iter_mut().enumerate() {
        *x *= from + step * (body + k) as f32;
    }
}

/// SSE2 [`super::add_from`].
///
/// # Safety
///
/// The CPU must support SSE2.
#[target_feature(enable = "sse2")]
pub unsafe fn add_from_sse2(dst: &mut [f32], src: &[f32]) {
    let len = dst.len().min(src.len());
    let body = len & !3;
    let dp = dst.as_mut_ptr();
    let sp = src.as_ptr();
    // SAFETY: `body <= len <= dst.len()` and `body <= len <= src.len()`, so both
    // `dp.add(i)` and `sp.add(i)` with `i + 4 <= body` address four initialised
    // `f32`s inside their own allocations. `dst` is borrowed exclusively and
    // `src` shared, and Rust's borrow rules make the two regions disjoint, so
    // the read-modify-write cannot alias. Accesses are unaligned; SSE2 comes
    // from `#[target_feature]` plus the caller's runtime detection.
    unsafe {
        let mut i = 0;
        while i < body {
            let q = dp.add(i);
            _mm_storeu_ps(q, _mm_add_ps(_mm_loadu_ps(q), _mm_loadu_ps(sp.add(i))));
            i += 4;
        }
    }
    for (d, &s) in dst[body..len].iter_mut().zip(&src[body..len]) {
        *d += s;
    }
}

/// SSE2 [`super::peak_abs`].
///
/// # Safety
///
/// The CPU must support SSE2.
#[target_feature(enable = "sse2")]
pub unsafe fn peak_abs_sse2(buf: &[f32]) -> f32 {
    let len = buf.len();
    let body = len & !3;
    let mut m = 0.0_f32;
    let p = buf.as_ptr();
    // SAFETY: `p.add(i)` with `i + 4 <= body <= len` reads four initialised
    // `f32`s inside the shared borrow; nothing is written to the slice. `tmp` is
    // a local `[f32; 4]`, so the 16-byte store into it is in bounds. Accesses
    // are unaligned; SSE2 comes from `#[target_feature]` plus the caller's
    // runtime detection.
    unsafe {
        // Clearing the sign bit is the branch-free `abs`.
        let sign_mask = _mm_castsi128_ps(_mm_set1_epi32(i32::MAX));
        let mut acc = _mm_setzero_ps();
        let mut i = 0;
        while i < body {
            let v = _mm_and_ps(_mm_loadu_ps(p.add(i)), sign_mask);
            // Candidate first: `MAXPS` yields its second operand when the
            // comparison is unordered, so a NaN candidate leaves `acc` alone —
            // matching the scalar `if a > m` exactly.
            acc = _mm_max_ps(v, acc);
            i += 4;
        }
        let mut tmp = [0.0_f32; 4];
        _mm_storeu_ps(tmp.as_mut_ptr(), acc);
        for v in tmp {
            if v > m {
                m = v;
            }
        }
    }
    for &x in &buf[body..] {
        let a = x.abs();
        if a > m {
            m = a;
        }
    }
    m
}

// ---------------------------------------------------------------------------
// AVX2 — 8 lanes
// ---------------------------------------------------------------------------

/// AVX2 [`super::apply_gain`].
///
/// # Safety
///
/// The CPU must support AVX2.
#[target_feature(enable = "avx2")]
pub unsafe fn apply_gain_avx2(buf: &mut [f32], gain: f32) {
    let len = buf.len();
    let body = len & !7;
    let p = buf.as_mut_ptr();
    // SAFETY: `p` is the base of a live, exclusively borrowed `[f32; len]`, so
    // `p.add(i)` for `i + 8 <= body <= len` addresses eight initialised,
    // writable `f32`s inside that allocation. `loadu`/`storeu` require no
    // alignment. The `&mut` borrow rules out concurrent access, and `p` does not
    // escape the block. AVX2 (and therefore AVX) is guaranteed by
    // `#[target_feature]` plus the caller's runtime detection.
    unsafe {
        let g = _mm256_set1_ps(gain);
        let mut i = 0;
        while i < body {
            let q = p.add(i);
            _mm256_storeu_ps(q, _mm256_mul_ps(_mm256_loadu_ps(q), g));
            i += 8;
        }
    }
    for x in &mut buf[body..] {
        *x *= gain;
    }
}

/// AVX2 [`super::apply_gain_ramp`].
///
/// # Safety
///
/// The CPU must support AVX2.
#[target_feature(enable = "avx2")]
pub unsafe fn apply_gain_ramp_avx2(buf: &mut [f32], from: f32, to: f32) {
    let len = buf.len();
    if len == 0 {
        return;
    }
    let step = (to - from) / len as f32;
    let body = len & !7;
    let p = buf.as_mut_ptr();
    // SAFETY: as in `apply_gain_avx2` — `p.add(i)` with `i + 8 <= body <= len`
    // stays inside the exclusively borrowed slice, the accesses are unaligned,
    // `p` does not escape, and AVX2 is guaranteed by `#[target_feature]` plus
    // the caller's runtime detection.
    unsafe {
        let from_v = _mm256_set1_ps(from);
        let step_v = _mm256_set1_ps(step);
        let lane = _mm256_set_ps(7.0, 6.0, 5.0, 4.0, 3.0, 2.0, 1.0, 0.0);
        let mut i = 0;
        while i < body {
            let idx = _mm256_add_ps(_mm256_set1_ps(i as f32), lane);
            let g = _mm256_add_ps(from_v, _mm256_mul_ps(step_v, idx));
            let q = p.add(i);
            _mm256_storeu_ps(q, _mm256_mul_ps(_mm256_loadu_ps(q), g));
            i += 8;
        }
    }
    for (k, x) in buf[body..].iter_mut().enumerate() {
        *x *= from + step * (body + k) as f32;
    }
}

/// AVX2 [`super::add_from`].
///
/// # Safety
///
/// The CPU must support AVX2.
#[target_feature(enable = "avx2")]
pub unsafe fn add_from_avx2(dst: &mut [f32], src: &[f32]) {
    let len = dst.len().min(src.len());
    let body = len & !7;
    let dp = dst.as_mut_ptr();
    let sp = src.as_ptr();
    // SAFETY: `body <= len` bounds both pointers, so `dp.add(i)` and
    // `sp.add(i)` with `i + 8 <= body` address eight initialised `f32`s inside
    // their own allocations. The exclusive borrow of `dst` and shared borrow of
    // `src` cannot overlap, so the read-modify-write does not alias. Accesses
    // are unaligned; AVX2 comes from `#[target_feature]` plus the caller's
    // runtime detection.
    unsafe {
        let mut i = 0;
        while i < body {
            let q = dp.add(i);
            _mm256_storeu_ps(
                q,
                _mm256_add_ps(_mm256_loadu_ps(q), _mm256_loadu_ps(sp.add(i))),
            );
            i += 8;
        }
    }
    for (d, &s) in dst[body..len].iter_mut().zip(&src[body..len]) {
        *d += s;
    }
}

/// AVX2 [`super::peak_abs`].
///
/// # Safety
///
/// The CPU must support AVX2.
#[target_feature(enable = "avx2")]
pub unsafe fn peak_abs_avx2(buf: &[f32]) -> f32 {
    let len = buf.len();
    let body = len & !7;
    let mut m = 0.0_f32;
    let p = buf.as_ptr();
    // SAFETY: `p.add(i)` with `i + 8 <= body <= len` reads eight initialised
    // `f32`s inside the shared borrow; nothing is written to the slice. `tmp` is
    // a local `[f32; 8]`, so the 32-byte store into it is in bounds. Accesses
    // are unaligned; AVX2 comes from `#[target_feature]` plus the caller's
    // runtime detection.
    unsafe {
        let sign_mask = _mm256_castsi256_ps(_mm256_set1_epi32(i32::MAX));
        let mut acc = _mm256_setzero_ps();
        let mut i = 0;
        while i < body {
            let v = _mm256_and_ps(_mm256_loadu_ps(p.add(i)), sign_mask);
            // Candidate first, for the same NaN reason as the SSE2 kernel.
            acc = _mm256_max_ps(v, acc);
            i += 8;
        }
        let mut tmp = [0.0_f32; 8];
        _mm256_storeu_ps(tmp.as_mut_ptr(), acc);
        for v in tmp {
            if v > m {
                m = v;
            }
        }
    }
    for &x in &buf[body..] {
        let a = x.abs();
        if a > m {
            m = a;
        }
    }
    m
}
