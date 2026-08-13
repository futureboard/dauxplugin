//! MIDI 2.0 resolution scaling.
//!
//! MIDI 2.0 widens every controller from MIDI 1.0's 7 bits (or 14 bits for pitch bend) to
//! 16 or 32 bits. A naive `value << n` is **wrong**: it maps the 7-bit maximum `127` to
//! `0x7F << 9 == 0xFE00`, so a controller pushed fully open in MIDI 1.0 would never reach
//! the MIDI 2.0 maximum, and a "centred" `64` would not land on the 16-bit centre.
//!
//! The MIDI 2.0 specification therefore mandates *min-center-max* scaling, which pins three
//! points exactly:
//!
//! | source (7-bit) | destination (16-bit) | destination (32-bit) |
//! | -------------- | -------------------- | -------------------- |
//! | `0`   (min)    | `0x0000`             | `0x0000_0000`        |
//! | `64`  (centre) | `0x8000`             | `0x8000_0000`        |
//! | `127` (max)    | `0xFFFF`             | `0xFFFF_FFFF`        |
//!
//! Values below the centre are a plain left shift. Values above the centre are a left shift
//! plus a repeated copy of the source's low bits, which fills the destination's low bits so
//! the top of the range saturates exactly. Down-scaling is defined by the specification as a
//! plain truncating right shift, which makes `scale_down(scale_up(v)) == v` for every `v`.
//!
//! Every function here is a pure `const fn`: no allocation, no panic, no branch on external
//! state.

/// [any-thread] Widens `src` from `src_bits` to `dst_bits` using MIDI 2.0 min-center-max
/// scaling.
///
/// Bits of `src` above `src_bits` are ignored. Returns `src` unchanged when the request is
/// nonsensical (`src_bits < 2`, `dst_bits > 32`, or `src_bits >= dst_bits`) rather than
/// panicking, because this function is reachable from the audio thread.
pub const fn scale_up(src: u32, src_bits: u32, dst_bits: u32) -> u32 {
    if src_bits < 2 || dst_bits > 32 || src_bits >= dst_bits {
        return src;
    }
    let src = src & ((1u32 << src_bits) - 1);
    let scale_bits = dst_bits - src_bits;
    let mut result = src << scale_bits;

    let src_center = 1u32 << (src_bits - 1);
    if src <= src_center {
        // Bottom half (and the centre itself) is an exact left shift.
        return result;
    }

    // Top half: repeat the source's low bits downwards so `max` maps to `max`.
    let repeat_bits = src_bits - 1;
    let repeat_mask = (1u32 << repeat_bits) - 1;
    let mut repeat_value = src & repeat_mask;
    if scale_bits > repeat_bits {
        repeat_value <<= scale_bits - repeat_bits;
    } else {
        repeat_value >>= repeat_bits - scale_bits;
    }
    // `repeat_bits >= 1` because `src_bits >= 2`, so this always terminates.
    while repeat_value != 0 {
        result |= repeat_value;
        repeat_value >>= repeat_bits;
    }
    result
}

/// [any-thread] Narrows `src` from `src_bits` to `dst_bits` by truncation, as the MIDI 2.0
/// specification defines down-scaling.
///
/// Returns `src` unchanged for nonsensical requests (`dst_bits == 0`, `src_bits > 32`, or
/// `dst_bits >= src_bits`).
pub const fn scale_down(src: u32, src_bits: u32, dst_bits: u32) -> u32 {
    if dst_bits == 0 || src_bits > 32 || dst_bits >= src_bits {
        return src;
    }
    let src = if src_bits == 32 {
        src
    } else {
        src & ((1u32 << src_bits) - 1)
    };
    src >> (src_bits - dst_bits)
}

/// [any-thread] 7-bit MIDI 1.0 data value to a 16-bit MIDI 2.0 velocity.
pub const fn u7_to_u16(v: u8) -> u16 {
    scale_up(v as u32, 7, 16) as u16
}

/// [any-thread] 7-bit MIDI 1.0 data value to a 32-bit MIDI 2.0 controller value.
pub const fn u7_to_u32(v: u8) -> u32 {
    scale_up(v as u32, 7, 32)
}

/// [any-thread] 14-bit MIDI 1.0 pitch bend to a 32-bit MIDI 2.0 pitch bend.
pub const fn u14_to_u32(v: u16) -> u32 {
    scale_up(v as u32, 14, 32)
}

/// [any-thread] 16-bit MIDI 2.0 velocity to a 7-bit MIDI 1.0 velocity (lossy).
pub const fn u16_to_u7(v: u16) -> u8 {
    scale_down(v as u32, 16, 7) as u8
}

/// [any-thread] 32-bit MIDI 2.0 controller value to a 7-bit MIDI 1.0 value (lossy).
pub const fn u32_to_u7(v: u32) -> u8 {
    scale_down(v, 32, 7) as u8
}

/// [any-thread] 32-bit MIDI 2.0 pitch bend to a 14-bit MIDI 1.0 pitch bend (lossy).
pub const fn u32_to_u14(v: u32) -> u16 {
    scale_down(v, 32, 14) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seven_to_sixteen_pins_min_center_max() {
        assert_eq!(u7_to_u16(0), 0x0000);
        assert_eq!(u7_to_u16(64), 0x8000);
        assert_eq!(u7_to_u16(127), 0xFFFF);
    }

    #[test]
    fn seven_to_thirtytwo_pins_min_center_max() {
        assert_eq!(u7_to_u32(0), 0x0000_0000);
        assert_eq!(u7_to_u32(64), 0x8000_0000);
        assert_eq!(u7_to_u32(127), 0xFFFF_FFFF);
    }

    #[test]
    fn fourteen_to_thirtytwo_pins_min_center_max() {
        assert_eq!(u14_to_u32(0), 0x0000_0000);
        assert_eq!(u14_to_u32(8192), 0x8000_0000);
        assert_eq!(u14_to_u32(16383), 0xFFFF_FFFF);
    }

    #[test]
    fn scaling_up_is_strictly_monotonic() {
        for v in 0u8..127 {
            assert!(u7_to_u16(v) < u7_to_u16(v + 1), "not monotonic at {v}");
            assert!(u7_to_u32(v) < u7_to_u32(v + 1), "not monotonic at {v}");
        }
        for v in 0u16..16383 {
            assert!(u14_to_u32(v) < u14_to_u32(v + 1), "not monotonic at {v}");
        }
    }

    #[test]
    fn up_then_down_is_the_identity() {
        for v in 0u8..=127 {
            assert_eq!(
                u16_to_u7(u7_to_u16(v)),
                v,
                "16-bit round trip failed at {v}"
            );
            assert_eq!(
                u32_to_u7(u7_to_u32(v)),
                v,
                "32-bit round trip failed at {v}"
            );
        }
        for v in 0u16..=16383 {
            assert_eq!(
                u32_to_u14(u14_to_u32(v)),
                v,
                "14-bit round trip failed at {v}"
            );
        }
    }

    #[test]
    fn naive_shift_would_have_been_wrong() {
        // The whole point: a plain shift never reaches the maximum.
        assert_ne!(u7_to_u16(127), u16::from(127u8) << 9);
        assert_eq!(u16::from(127u8) << 9, 0xFE00);
    }

    #[test]
    fn bits_above_src_bits_are_ignored() {
        assert_eq!(scale_up(0xFF, 7, 16), scale_up(0x7F, 7, 16));
        assert_eq!(scale_down(0xFFFF_FF80 | 0x7F, 7, 4), scale_down(0x7F, 7, 4));
    }

    #[test]
    fn degenerate_requests_are_the_identity() {
        assert_eq!(scale_up(5, 0, 16), 5);
        assert_eq!(scale_up(5, 1, 16), 5);
        assert_eq!(scale_up(5, 16, 16), 5);
        assert_eq!(scale_up(5, 16, 8), 5);
        assert_eq!(scale_up(5, 8, 33), 5);
        assert_eq!(scale_down(5, 8, 0), 5);
        assert_eq!(scale_down(5, 8, 8), 5);
        assert_eq!(scale_down(5, 8, 16), 5);
        assert_eq!(scale_down(5, 33, 8), 5);
    }

    #[test]
    fn thirtytwo_bit_source_down_scales_without_masking() {
        assert_eq!(scale_down(0xFFFF_FFFF, 32, 7), 127);
        assert_eq!(scale_down(0x8000_0000, 32, 7), 64);
        assert_eq!(scale_down(0x0000_0000, 32, 7), 0);
    }

    #[test]
    fn works_in_const_context() {
        const MAX: u16 = u7_to_u16(127);
        const CENTER: u32 = u7_to_u32(64);
        assert_eq!(MAX, 0xFFFF);
        assert_eq!(CENTER, 0x8000_0000);
    }
}
