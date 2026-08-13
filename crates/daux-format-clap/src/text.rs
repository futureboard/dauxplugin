//! Moving text across the CLAP boundary without ever writing past a buffer.
//!
//! CLAP passes strings three ways, and each one has its own way of going wrong:
//!
//! * fixed inline arrays (`clap_param_info::name`) — must always end in a NUL, and a
//!   multi-byte character must not be cut in half;
//! * caller-owned buffers with a capacity (`value_to_text`) — the capacity includes the
//!   NUL, and CLAP hosts pass small ones;
//! * borrowed `const char *` from the host — may be null, may not be UTF-8, and is only
//!   valid for the call.
//!
//! Every helper here truncates on a character boundary and NUL-terminates unconditionally.
//! `[main-thread]` unless stated otherwise.

use core::ffi::{CStr, c_char};

/// Writes `s` into a fixed inline C buffer, truncating on a character boundary.
///
/// The buffer is fully overwritten — trailing bytes are zeroed — so no fragment of a
/// previous parameter's name can survive into this one.
pub(crate) fn write_fixed<const N: usize>(dst: &mut [c_char; N], s: &str) {
    dst.fill(0);
    if N < 2 {
        return;
    }
    let limit = truncation_point(s, N - 1);
    for (slot, byte) in dst.iter_mut().zip(s.as_bytes()[..limit].iter()) {
        *slot = *byte as c_char;
    }
}

/// Writes `s` into a host-owned buffer of `capacity` bytes, NUL included.
///
/// Returns `false` when the host passed a null pointer or zero capacity, which is the only
/// case in which nothing is written. A string that does not fit is truncated on a character
/// boundary rather than refused: a host asking for display text would rather have "−12.0 d"
/// than nothing.
///
/// # Safety
///
/// `dst` must be null, or point to `capacity` writable bytes for the duration of the call.
pub(crate) unsafe fn write_capped(dst: *mut c_char, capacity: u32, s: &str) -> bool {
    if dst.is_null() || capacity == 0 {
        return false;
    }
    let capacity = capacity as usize;
    let limit = truncation_point(s, capacity - 1);
    // SAFETY: the caller guarantees `capacity` writable bytes at `dst`, and `limit` is at
    // most `capacity - 1`, so the copy and the NUL that follows it both land inside the
    // buffer. `s` is a separate, immutable Rust string, so the regions cannot overlap.
    unsafe {
        core::ptr::copy_nonoverlapping(s.as_ptr().cast::<c_char>(), dst, limit);
        dst.add(limit).write(0);
    }
    true
}

/// Borrows a host-supplied C string as UTF-8, or `None` when it is null or not UTF-8.
///
/// # Safety
///
/// `p` must be null, or point to a NUL-terminated byte sequence that stays valid and
/// unmodified for `'a`.
pub(crate) unsafe fn borrow_str<'a>(p: *const c_char) -> Option<&'a str> {
    if p.is_null() {
        return None;
    }
    // SAFETY: the caller guarantees `p` is a live NUL-terminated string for `'a`, which is
    // exactly `CStr::from_ptr`'s contract. Non-UTF-8 becomes `None` rather than a panic.
    unsafe { CStr::from_ptr(p) }.to_str().ok()
}

/// The largest prefix length of `s` that is at most `max` bytes and ends on a character
/// boundary. `[any-thread]`
pub(crate) fn truncation_point(s: &str, max: usize) -> usize {
    if s.len() <= max {
        return s.len();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_fixed<const N: usize>(buf: &[c_char; N]) -> String {
        let bytes: Vec<u8> = buf
            .iter()
            .take_while(|b| **b != 0)
            .map(|b| *b as u8)
            .collect();
        String::from_utf8(bytes).expect("written text is valid UTF-8")
    }

    #[test]
    fn a_short_name_round_trips() {
        let mut buf = [0 as c_char; 16];
        write_fixed(&mut buf, "Gain");
        assert_eq!(read_fixed(&buf), "Gain");
        assert_eq!(buf[4], 0, "the NUL must be present");
        assert_eq!(buf[15], 0, "the tail must be zeroed");
    }

    #[test]
    fn a_long_name_is_truncated_and_still_terminated() {
        let mut buf = [0 as c_char; 8];
        write_fixed(&mut buf, "0123456789");
        assert_eq!(read_fixed(&buf), "0123456");
        assert_eq!(buf[7], 0);
    }

    #[test]
    fn truncation_never_splits_a_character() {
        // "é" is two bytes. A seven-byte window over "ééééé" must stop at three characters,
        // not cut the fourth in half and produce invalid UTF-8 in the host's UI.
        let mut buf = [0 as c_char; 8];
        write_fixed(&mut buf, "ééééé");
        assert_eq!(read_fixed(&buf), "ééé");
    }

    #[test]
    fn a_previous_longer_name_cannot_bleed_through() {
        let mut buf = [0 as c_char; 16];
        write_fixed(&mut buf, "A very long name");
        write_fixed(&mut buf, "Hi");
        assert_eq!(read_fixed(&buf), "Hi");
        assert!(buf[3..].iter().all(|b| *b == 0));
    }

    #[test]
    fn a_degenerate_buffer_is_left_alone() {
        let mut buf = [0 as c_char; 1];
        write_fixed(&mut buf, "x");
        assert_eq!(buf[0], 0);
    }

    #[test]
    fn capped_writes_respect_the_hosts_capacity() {
        let mut buf = [0x7f as c_char; 8];
        // SAFETY: `buf` is eight writable `c_char`s and the capacity passed matches.
        let ok = unsafe { write_capped(buf.as_mut_ptr(), 8, "-12.0 dB") };
        assert!(ok);
        assert_eq!(read_fixed(&buf), "-12.0 d");
        assert_eq!(buf[7], 0);
    }

    #[test]
    fn a_null_or_zero_capacity_target_is_refused_not_written() {
        // SAFETY: a null pointer is explicitly allowed by `write_capped`'s contract.
        assert!(!unsafe { write_capped(core::ptr::null_mut(), 32, "x") });
        let mut buf = [0x7f as c_char; 4];
        // SAFETY: `buf` is four writable `c_char`s; capacity zero must write nothing.
        assert!(!unsafe { write_capped(buf.as_mut_ptr(), 0, "x") });
        assert_eq!(buf[0], 0x7f, "a refused write must not touch the buffer");
    }

    #[test]
    fn capped_writes_of_multibyte_text_stay_on_a_boundary() {
        let mut buf = [0 as c_char; 6];
        // SAFETY: `buf` is six writable `c_char`s and the capacity passed matches.
        assert!(unsafe { write_capped(buf.as_mut_ptr(), 6, "ééé") });
        assert_eq!(read_fixed(&buf), "éé");
    }

    #[test]
    fn borrowing_a_host_string_tolerates_null_and_garbage() {
        // SAFETY: null is explicitly allowed.
        assert_eq!(unsafe { borrow_str(core::ptr::null()) }, None);

        let good = c"clap.params";
        // SAFETY: `good` is a live NUL-terminated literal for the whole test.
        assert_eq!(unsafe { borrow_str(good.as_ptr()) }, Some("clap.params"));

        let bad: [c_char; 3] = [0x66, 0xFF_u8 as c_char, 0];
        // SAFETY: `bad` is NUL-terminated and lives for the whole test; the middle byte is
        // deliberately not valid UTF-8.
        assert_eq!(unsafe { borrow_str(bad.as_ptr()) }, None);
    }
}
