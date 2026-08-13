//! Copying Rust strings into VST3's fixed-size character arrays, and back.
//!
//! Every string that crosses the VST3 boundary lives in a fixed array inside a `#[repr(C)]`
//! struct: `char8 name[64]`, `char16 title[128]`. There is no length field, only a null
//! terminator, so a string that does not fit must be truncated *and* still terminated. This
//! module is the only place that happens, so "the host showed half a character" can only be
//! a bug here.
//!
//! Truncation is on a character boundary, never in the middle of a UTF-8 sequence or a
//! UTF-16 surrogate pair, and one slot is always reserved for the terminator.

use crate::com::{Char16, FidString};

/// `[main-thread]` Writes `src` into an ASCII/UTF-8 array, truncating and null-terminating.
///
/// Returns `true` when the whole string fitted. VST3's `char8` fields are documented as
/// ASCII, but hosts read them as UTF-8 when `PFactoryInfo::kUnicode` is set, which this
/// adapter does set — so non-ASCII vendor names survive as long as they fit.
pub fn write_utf8(dst: &mut [u8], src: &str) -> bool {
    dst.fill(0);
    if dst.is_empty() {
        return src.is_empty();
    }
    let room = dst.len() - 1;
    let mut end = src.len().min(room);
    // Back off to a character boundary so a truncated name is never invalid UTF-8.
    while end > 0 && !src.is_char_boundary(end) {
        end -= 1;
    }
    dst[..end].copy_from_slice(&src.as_bytes()[..end]);
    end == src.len()
}

/// `[main-thread]` Writes `src` into a UTF-16 array, truncating and null-terminating.
///
/// Returns `true` when the whole string fitted. A surrogate pair is never split: if only
/// one half would fit, neither is written.
pub fn write_utf16(dst: &mut [Char16], src: &str) -> bool {
    dst.fill(0);
    if dst.is_empty() {
        return src.is_empty();
    }
    let room = dst.len() - 1;
    let mut written = 0;
    for ch in src.chars() {
        let needed = ch.len_utf16();
        if written + needed > room {
            return false;
        }
        ch.encode_utf16(&mut dst[written..written + needed]);
        written += needed;
    }
    true
}

/// `[main-thread]` Reads a null-terminated UTF-16 string the host wrote.
///
/// `capacity` is the size of the array the host promised, and is an upper bound on how far
/// this will read — a host that forgets the terminator costs us a truncated string, never a
/// walk off the end of the buffer. Unpaired surrogates become `U+FFFD` rather than an error,
/// because refusing to parse a parameter the user typed is worse than showing a placeholder.
///
/// # Safety
///
/// `ptr` must be null or point to `capacity` readable, aligned [`Char16`]s.
#[must_use]
pub unsafe fn read_utf16(ptr: *const Char16, capacity: usize) -> String {
    if ptr.is_null() || capacity == 0 {
        return String::new();
    }
    // SAFETY: the caller promises `capacity` readable, aligned code units at `ptr`, which is
    // exactly what `from_raw_parts` needs; the slice borrows nothing beyond this expression.
    let units = unsafe { core::slice::from_raw_parts(ptr, capacity) };
    let len = units.iter().position(|&u| u == 0).unwrap_or(capacity);
    char::decode_utf16(units[..len].iter().copied())
        .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

/// `[main-thread]` Reads a null-terminated ASCII string the host passed as an `FIDString`.
///
/// `max` bounds the search, so a missing terminator truncates rather than reads for ever.
/// Returns `None` for a null pointer.
///
/// # Safety
///
/// `ptr` must be null or point to at most `max` readable bytes, terminated by a `0` at or
/// before that limit.
#[must_use]
pub unsafe fn read_c_str(ptr: FidString, max: usize) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    let bytes = ptr.cast::<u8>();
    let mut len = 0;
    // SAFETY: the caller bounds the readable region at `max` bytes, and the loop never
    // indexes past `len < max`; each read is one byte, which needs no alignment.
    while len < max && unsafe { *bytes.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: `len` bytes starting at `bytes` were just proven readable by the loop above.
    let slice = unsafe { core::slice::from_raw_parts(bytes, len) };
    Some(String::from_utf8_lossy(slice).into_owned())
}

/// `[main-thread]` `true` when a host-supplied `FIDString` equals a null-terminated literal.
///
/// Used for the platform-type and view-name strings, where allocating a `String` to compare
/// four characters would be silly.
///
/// # Safety
///
/// As [`read_c_str`]; `expected` must itself end in a `0` byte.
#[must_use]
pub unsafe fn c_str_eq(ptr: FidString, expected: &[u8]) -> bool {
    if ptr.is_null() {
        return false;
    }
    let bytes = ptr.cast::<u8>();
    for (i, &want) in expected.iter().enumerate() {
        // SAFETY: `expected` is null-terminated, so the loop stops at or before the first
        // `0`; up to that point the caller promises the host's string is readable, because a
        // conforming `FIDString` is null-terminated too and the two agree byte for byte
        // until one of them ends.
        let got = unsafe { *bytes.add(i) };
        if got != want {
            return false;
        }
        if want == 0 {
            return true;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_fields_are_truncated_on_a_character_boundary() {
        let mut buf = [0xFFu8; 8];
        assert!(write_utf8(&mut buf, "abc"));
        assert_eq!(&buf, b"abc\0\0\0\0\0");

        // Seven usable bytes; the four-byte emoji cannot start at index 5.
        let mut buf = [0xFFu8; 8];
        assert!(!write_utf8(&mut buf, "hello\u{1F600}"));
        assert_eq!(&buf[..5], b"hello");
        assert_eq!(buf[5], 0, "the terminator must be present after truncation");
        assert_eq!(
            core::str::from_utf8(&buf[..5]).unwrap(),
            "hello",
            "a truncated field must still be valid UTF-8"
        );

        // Exactly filling the array still leaves room for the terminator.
        let mut buf = [0xFFu8; 4];
        assert!(!write_utf8(&mut buf, "abcd"));
        assert_eq!(&buf, b"abc\0");
        assert!(write_utf8(&mut buf, "abc"));
        assert_eq!(&buf, b"abc\0");
    }

    #[test]
    fn utf16_fields_never_split_a_surrogate_pair() {
        let mut buf = [0xFFFFu16; 8];
        assert!(write_utf16(&mut buf, "Gain"));
        assert_eq!(
            &buf[..5],
            &[b'G' as u16, b'a' as u16, b'i' as u16, b'n' as u16, 0]
        );

        // Six usable slots: five ASCII plus a two-unit emoji does not fit.
        let mut buf = [0xFFFFu16; 7];
        assert!(!write_utf16(&mut buf, "hello\u{1F600}"));
        assert_eq!(buf[5], 0);
        // SAFETY: `buf` is a live array of seven code units.
        let round_trip = unsafe { read_utf16(buf.as_ptr(), buf.len()) };
        assert_eq!(round_trip, "hello");
    }

    #[test]
    fn utf16_round_trips_non_ascii() {
        let mut buf = [0u16; 128];
        assert!(write_utf16(&mut buf, "Frequência \u{1F3B9}"));
        // SAFETY: `buf` is a live array of 128 code units.
        let back = unsafe { read_utf16(buf.as_ptr(), buf.len()) };
        assert_eq!(back, "Frequência \u{1F3B9}");
    }

    #[test]
    fn reading_tolerates_a_host_that_forgets_the_terminator() {
        let unterminated: [u16; 4] = [b'a' as u16, b'b' as u16, b'c' as u16, b'd' as u16];
        // SAFETY: exactly four readable code units, and the capacity says so.
        let s = unsafe { read_utf16(unterminated.as_ptr(), unterminated.len()) };
        assert_eq!(s, "abcd");

        // SAFETY: a null pointer is checked for before any read.
        assert_eq!(unsafe { read_utf16(core::ptr::null(), 16) }, "");
        // SAFETY: a zero capacity is checked for before any read.
        assert_eq!(unsafe { read_utf16(unterminated.as_ptr(), 0) }, "");
    }

    #[test]
    fn an_unpaired_surrogate_becomes_a_replacement_rather_than_an_error() {
        let hostile: [u16; 3] = [0xD800, b'x' as u16, 0];
        // SAFETY: three readable code units.
        let s = unsafe { read_utf16(hostile.as_ptr(), hostile.len()) };
        assert_eq!(s, "\u{FFFD}x");
    }

    #[test]
    fn c_strings_are_compared_and_read_without_running_off_the_end() {
        let hwnd = b"HWND\0";
        let ptr = hwnd.as_ptr().cast::<core::ffi::c_char>();
        // SAFETY: `hwnd` is a live, null-terminated byte string.
        unsafe {
            assert!(c_str_eq(ptr, b"HWND\0"));
            assert!(!c_str_eq(ptr, b"NSView\0"));
            assert!(!c_str_eq(ptr, b"HWN\0"));
            assert!(!c_str_eq(core::ptr::null(), b"HWND\0"));
            assert_eq!(read_c_str(ptr, 64).as_deref(), Some("HWND"));
            assert_eq!(read_c_str(core::ptr::null(), 64), None);
            // A missing terminator truncates at `max` instead of reading on.
            assert_eq!(read_c_str(ptr, 2).as_deref(), Some("HW"));
        }
    }
}
