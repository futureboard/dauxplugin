//! NEON kernels for AArch64.
//!
//! Advanced SIMD is architecturally mandatory in AArch64, so there is nothing to
//! detect at runtime: `#[cfg(target_arch = "aarch64")]` alone is a sufficient
//! guarantee, and `#[target_feature(enable = "neon")]` merely restates what the
//! base target already enables. That is why [`super::detect`] returns
//! [`super::Isa::Neon`] unconditionally on this architecture — there is no CPU
//! it can fault on.
//!
//! The kernels mirror `x86.rs` one for one: a 4-lane body over
//! `len & !3` elements, then a scalar tail written with the same expressions as
//! the scalar reference so tail and body agree bit for bit.

use core::arch::aarch64::*;

/// NEON [`super::apply_gain`].
///
/// # Safety
///
/// The CPU must support NEON, which every AArch64 CPU does.
#[target_feature(enable = "neon")]
pub unsafe fn apply_gain_neon(buf: &mut [f32], gain: f32) {
    let len = buf.len();
    let body = len & !3;
    let p = buf.as_mut_ptr();
    // SAFETY: `p` is the base of a live, exclusively borrowed `[f32; len]`, so
    // `p.add(i)` for `i + 4 <= body <= len` addresses four initialised, writable
    // `f32`s inside that allocation. `vld1q_f32`/`vst1q_f32` need only natural
    // `f32` alignment, which a `&mut [f32]` guarantees. The exclusive borrow
    // rules out concurrent access and `p` does not escape the block.
    unsafe {
        let g = vdupq_n_f32(gain);
        let mut i = 0;
        while i < body {
            let q = p.add(i);
            vst1q_f32(q, vmulq_f32(vld1q_f32(q), g));
            i += 4;
        }
    }
    for x in &mut buf[body..] {
        *x *= gain;
    }
}

/// NEON [`super::apply_gain_ramp`].
///
/// # Safety
///
/// The CPU must support NEON, which every AArch64 CPU does.
#[target_feature(enable = "neon")]
pub unsafe fn apply_gain_ramp_neon(buf: &mut [f32], from: f32, to: f32) {
    let len = buf.len();
    if len == 0 {
        return;
    }
    let step = (to - from) / len as f32;
    let body = len & !3;
    let lanes = [0.0_f32, 1.0, 2.0, 3.0];
    let p = buf.as_mut_ptr();
    // SAFETY: as in `apply_gain_neon` — `p.add(i)` with `i + 4 <= body <= len`
    // stays inside the exclusively borrowed slice and `p` does not escape.
    // `lanes` is a live local `[f32; 4]`, so the 16-byte load from it is in
    // bounds and naturally aligned.
    unsafe {
        let from_v = vdupq_n_f32(from);
        let step_v = vdupq_n_f32(step);
        let lane = vld1q_f32(lanes.as_ptr());
        let mut i = 0;
        while i < body {
            // Recomputed from `i`, never accumulated: lane `i + k` gets
            // `from + step * (i + k)`, exactly the scalar expression.
            let idx = vaddq_f32(vdupq_n_f32(i as f32), lane);
            let g = vaddq_f32(from_v, vmulq_f32(step_v, idx));
            let q = p.add(i);
            vst1q_f32(q, vmulq_f32(vld1q_f32(q), g));
            i += 4;
        }
    }
    for (k, x) in buf[body..].iter_mut().enumerate() {
        *x *= from + step * (body + k) as f32;
    }
}

/// NEON [`super::add_from`].
///
/// # Safety
///
/// The CPU must support NEON, which every AArch64 CPU does.
#[target_feature(enable = "neon")]
pub unsafe fn add_from_neon(dst: &mut [f32], src: &[f32]) {
    let len = dst.len().min(src.len());
    let body = len & !3;
    let dp = dst.as_mut_ptr();
    let sp = src.as_ptr();
    // SAFETY: `body <= len` bounds both pointers, so `dp.add(i)` and
    // `sp.add(i)` with `i + 4 <= body` address four initialised `f32`s inside
    // their own allocations. The exclusive borrow of `dst` and the shared borrow
    // of `src` cannot overlap, so the read-modify-write does not alias.
    unsafe {
        let mut i = 0;
        while i < body {
            let q = dp.add(i);
            vst1q_f32(q, vaddq_f32(vld1q_f32(q), vld1q_f32(sp.add(i))));
            i += 4;
        }
    }
    for (d, &s) in dst[body..len].iter_mut().zip(&src[body..len]) {
        *d += s;
    }
}

/// NEON [`super::peak_abs`].
///
/// # Safety
///
/// The CPU must support NEON, which every AArch64 CPU does.
#[target_feature(enable = "neon")]
pub unsafe fn peak_abs_neon(buf: &[f32]) -> f32 {
    let len = buf.len();
    let body = len & !3;
    let mut m = 0.0_f32;
    let p = buf.as_ptr();
    // SAFETY: `p.add(i)` with `i + 4 <= body <= len` reads four initialised
    // `f32`s inside the shared borrow; nothing is written to the slice. `tmp` is
    // a local `[f32; 4]`, so the 16-byte store into it is in bounds and
    // naturally aligned.
    unsafe {
        let mut acc = vdupq_n_f32(0.0);
        let mut i = 0;
        while i < body {
            let v = vabsq_f32(vld1q_f32(p.add(i)));
            // `FMAXNM`, not `FMAX`: it returns the numeric operand when the
            // other is NaN, so a NaN candidate leaves `acc` alone — matching the
            // scalar `if a > m`.
            acc = vmaxnmq_f32(v, acc);
            i += 4;
        }
        let mut tmp = [0.0_f32; 4];
        vst1q_f32(tmp.as_mut_ptr(), acc);
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
