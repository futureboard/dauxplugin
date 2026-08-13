//! Scalar reference kernels.
//!
//! These are always compiled, on every target, with no feature gates: they are
//! the definition of what the vector kernels must reproduce, and the fallback
//! when no vector unit is available. Every one of them is trivially auto-
//! vectorisable, so on a target where the compiler already knows the ISA these
//! are not a slow path at all — they are simply the honest one.
//!
//! Two details are load-bearing for bit-identity with the vector kernels:
//!
//! * `apply_gain_ramp` computes each coefficient as `from + step * i`, never
//!   incrementally, so no rounding accumulates along the block and the vector
//!   kernels can compute lane indices independently.
//! * `peak_abs` accumulates with `if a > m`, not `f32::max`. The two differ on
//!   `NaN`: `a > m` is false for `NaN`, which ignores it, and that is exactly
//!   what `MAXPS` / `FMAXNM` do when the candidate is the first operand. A
//!   `NaN` in the buffer therefore cannot change the answer on any path.

/// Multiplies every sample by `gain`.
pub fn apply_gain(buf: &mut [f32], gain: f32) {
    for x in buf.iter_mut() {
        *x *= gain;
    }
}

/// Multiplies sample `i` of `buf` by `from + (to - from) * i / len`.
pub fn apply_gain_ramp(buf: &mut [f32], from: f32, to: f32) {
    let len = buf.len();
    if len == 0 {
        return;
    }
    let step = (to - from) / len as f32;
    for (i, x) in buf.iter_mut().enumerate() {
        *x *= from + step * i as f32;
    }
}

/// Adds `src` into `dst`, element-wise, over the shorter of the two.
pub fn add_from(dst: &mut [f32], src: &[f32]) {
    for (d, &s) in dst.iter_mut().zip(src.iter()) {
        *d += s;
    }
}

/// Copies `src` into `dst`, over the shorter of the two.
pub fn copy_from(dst: &mut [f32], src: &[f32]) {
    let n = dst.len().min(src.len());
    dst[..n].copy_from_slice(&src[..n]);
}

/// Largest absolute sample value, or `0.0` for an empty slice.
pub fn peak_abs(buf: &[f32]) -> f32 {
    let mut m = 0.0_f32;
    for &x in buf {
        let a = x.abs();
        if a > m {
            m = a;
        }
    }
    m
}
