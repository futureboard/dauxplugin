//! The byte-level little-endian reader and writer shared by every message.
//!
//! Both halves live in one module so the encoder and the decoder cannot drift apart. The
//! rules they enforce, once, for every field:
//!
//! * Every integer and float is explicitly little-endian, regardless of host byte order.
//!   The control plane is a *wire* format; the fact that every platform DAUxPlug targets
//!   happens to be little-endian is not a licence to depend on it.
//! * Every read is bounds-checked against the bytes actually remaining. There is no
//!   indexing, no slicing by a decoded length, and no `unwrap` anywhere in this file.
//! * Every variable-length field is checked against a [`ProtocolLimits`] bound *before*
//!   the allocation that would hold it. A hostile `u32::MAX` length prefix costs a
//!   comparison, not four gigabytes.

use crate::error::{ProtocolError, ProtocolErrorKind, ProtocolResult};
use crate::limits::ProtocolLimits;

// ----------------------------------------------------------------------------- read ---

/// Bounds-checked little-endian cursor over a payload.
///
/// The primitive set is deliberately complete rather than trimmed to today's callers: a codec
/// with a hole in it invites the next message type to hand-roll the missing read, which is
/// how an unchecked one gets in. Wire compatibility means the set only ever grows.
#[allow(dead_code, reason = "a codec keeps a complete primitive set, not a used-today set")]
pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

macro_rules! read_int {
    ($name:ident, $ty:ty) => {
        /// Reads one little-endian value.
        pub(crate) fn $name(&mut self, context: &'static str) -> ProtocolResult<$ty> {
            const N: usize = size_of::<$ty>();
            let raw = self.take(N, context)?;
            let mut buf = [0u8; N];
            buf.copy_from_slice(raw);
            Ok(<$ty>::from_le_bytes(buf))
        }
    };
}

impl<'a> Reader<'a> {
    /// Starts a cursor at the beginning of `bytes`.
    pub(crate) const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    /// Bytes not yet consumed.
    pub(crate) const fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }

    /// Consumes exactly `n` bytes, or fails without advancing.
    fn take(&mut self, n: usize, context: &'static str) -> ProtocolResult<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| ProtocolError::truncated(context, n, self.remaining()))?;
        if end > self.bytes.len() {
            return Err(ProtocolError::truncated(context, n, self.remaining()));
        }
        let slice = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    read_int!(u8, u8);
    read_int!(u16, u16);
    read_int!(u32, u32);
    read_int!(u64, u64);
    read_int!(i16, i16);
    read_int!(i32, i32);
    read_int!(i64, i64);
    read_int!(f64, f64);

    /// Reads a strict boolean: any encoding other than `0` or `1` is malformed.
    pub(crate) fn bool(&mut self, context: &'static str) -> ProtocolResult<bool> {
        match self.u8(context)? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(ProtocolError::invalid(context)),
        }
    }

    /// Reads a reserved word and requires it to be zero.
    ///
    /// Reserved fields are the growth mechanism of the framing, so a non-zero one means
    /// the peer is writing a layout this build does not understand. Rejecting it is
    /// safer than guessing.
    pub(crate) fn reserved_u32(&mut self, context: &'static str) -> ProtocolResult<()> {
        if self.u32(context)? == 0 {
            Ok(())
        } else {
            Err(ProtocolError::invalid(context))
        }
    }

    /// Reads a `u32` length prefix, validating it against `max` and against the bytes
    /// actually left, then borrows that many bytes.
    ///
    /// Both checks matter and neither subsumes the other: `max` stops a plausible-looking
    /// length from exhausting memory, and the remaining-bytes check stops a length that
    /// is under the limit but still longer than the frame.
    pub(crate) fn length_prefixed(
        &mut self,
        context: &'static str,
        max: usize,
    ) -> ProtocolResult<&'a [u8]> {
        let len = self.u32(context)? as usize;
        if len > max {
            return Err(ProtocolError::limit(context, max, len));
        }
        self.take(len, context)
    }

    /// Reads a length-prefixed UTF-8 string, bounded by `limits.max_string_bytes`.
    pub(crate) fn string(
        &mut self,
        context: &'static str,
        limits: &ProtocolLimits,
    ) -> ProtocolResult<String> {
        let raw = self.length_prefixed(context, limits.max_string_bytes)?;
        // Only reached once the length is known to be both within the limit and backed by
        // real bytes, so this is the first and only allocation the field can cause.
        core::str::from_utf8(raw)
            .map(str::to_owned)
            .map_err(|_| ProtocolError::new(ProtocolErrorKind::InvalidUtf8, context))
    }

    /// Reads a length-prefixed opaque blob, bounded by `limits.max_blob_bytes`.
    pub(crate) fn blob(
        &mut self,
        context: &'static str,
        limits: &ProtocolLimits,
    ) -> ProtocolResult<Vec<u8>> {
        let raw = self.length_prefixed(context, limits.max_blob_bytes)?;
        Ok(raw.to_vec())
    }

    /// Finishes decoding, rejecting a payload that has bytes left over.
    pub(crate) fn finish(self, context: &'static str) -> ProtocolResult<()> {
        let extra = self.remaining();
        if extra == 0 {
            Ok(())
        } else {
            Err(ProtocolError::new(
                ProtocolErrorKind::TrailingBytes { extra },
                context,
            ))
        }
    }
}

// ---------------------------------------------------------------------------- write ---

/// Little-endian appender that enforces the same limits the reader applies.
///
/// Complete for the same reason [`Reader`] is: the two must stay symmetric, or a value that
/// can be written has no checked way back.
#[allow(dead_code, reason = "kept symmetric with Reader; see the note there")]
pub(crate) struct Writer<'a> {
    buf: &'a mut Vec<u8>,
    limits: ProtocolLimits,
}

macro_rules! write_int {
    ($name:ident, $ty:ty) => {
        /// Appends one little-endian value.
        pub(crate) fn $name(&mut self, value: $ty) {
            self.buf.extend_from_slice(&value.to_le_bytes());
        }
    };
}

impl<'a> Writer<'a> {
    /// Appends to `buf`, which may already hold earlier frames.
    pub(crate) fn new(buf: &'a mut Vec<u8>, limits: ProtocolLimits) -> Self {
        Self { buf, limits }
    }

    write_int!(u8, u8);
    write_int!(u16, u16);
    write_int!(u32, u32);
    write_int!(u64, u64);
    write_int!(i16, i16);
    write_int!(i32, i32);
    write_int!(i64, i64);
    write_int!(f64, f64);

    /// Appends a boolean as a single `0`/`1` byte.
    pub(crate) fn bool(&mut self, value: bool) {
        self.u8(u8::from(value));
    }

    /// Appends a zero reserved word.
    pub(crate) fn reserved_u32(&mut self) {
        self.u32(0);
    }

    /// Appends a length-prefixed byte run, rejecting anything over `max`.
    fn length_prefixed(
        &mut self,
        context: &'static str,
        max: usize,
        bytes: &[u8],
    ) -> ProtocolResult<()> {
        if bytes.len() > max {
            return Err(ProtocolError::limit(context, max, bytes.len()));
        }
        // The limit is at most `usize::MAX`, but the wire prefix is a `u32`; a 4 GiB field
        // is rejected here rather than silently truncated by the cast.
        let len = u32::try_from(bytes.len())
            .map_err(|_| ProtocolError::limit(context, u32::MAX as usize, bytes.len()))?;
        self.u32(len);
        self.buf.extend_from_slice(bytes);
        Ok(())
    }

    /// Appends a length-prefixed string, bounded by `max_string_bytes`.
    pub(crate) fn string(&mut self, context: &'static str, value: &str) -> ProtocolResult<()> {
        let max = self.limits.max_string_bytes;
        self.length_prefixed(context, max, value.as_bytes())
    }

    /// Appends a length-prefixed blob, bounded by `max_blob_bytes`.
    pub(crate) fn blob(&mut self, context: &'static str, value: &[u8]) -> ProtocolResult<()> {
        let max = self.limits.max_blob_bytes;
        self.length_prefixed(context, max, value)
    }
}

#[cfg(test)]
mod tests {
    use super::{Reader, Writer};
    use crate::error::ProtocolErrorKind;
    use crate::limits::ProtocolLimits;

    #[test]
    fn integers_round_trip_little_endian_regardless_of_host_order() {
        let mut buf = Vec::new();
        let mut w = Writer::new(&mut buf, ProtocolLimits::new());
        w.u16(0x0102);
        w.u32(0x0304_0506);
        w.i64(-2);
        w.f64(0.5);
        assert_eq!(&buf[0..2], &[0x02, 0x01]);
        assert_eq!(&buf[2..6], &[0x06, 0x05, 0x04, 0x03]);

        let mut r = Reader::new(&buf);
        assert_eq!(r.u16("a").unwrap(), 0x0102);
        assert_eq!(r.u32("b").unwrap(), 0x0304_0506);
        assert_eq!(r.i64("c").unwrap(), -2);
        assert!((r.f64("d").unwrap() - 0.5).abs() < f64::EPSILON);
        r.finish("end").unwrap();
    }

    #[test]
    fn reading_past_the_end_fails_without_advancing_or_panicking() {
        let bytes = [1u8, 2, 3];
        let mut r = Reader::new(&bytes);
        let err = r.u32("field").unwrap_err();
        assert_eq!(
            err.kind(),
            ProtocolErrorKind::Truncated {
                needed: 4,
                available: 3
            }
        );
        // The cursor did not move, so a caller can still read the bytes that are there.
        assert_eq!(r.remaining(), 3);
        assert_eq!(r.u8("field").unwrap(), 1);
    }

    #[test]
    fn a_hostile_length_prefix_is_rejected_before_it_can_allocate() {
        // A four-byte payload claiming a 4 GiB string.
        let bytes = [0xFF, 0xFF, 0xFF, 0xFF];
        let limits = ProtocolLimits::new();
        let mut r = Reader::new(&bytes);
        let err = r.string("name", &limits).unwrap_err();
        assert_eq!(
            err.kind(),
            ProtocolErrorKind::LimitExceeded {
                limit: limits.max_string_bytes,
                requested: u32::MAX as usize,
            }
        );
    }

    #[test]
    fn a_length_under_the_limit_but_past_the_input_is_still_rejected() {
        let mut bytes = 16u32.to_le_bytes().to_vec();
        bytes.extend_from_slice(b"only four");
        let mut r = Reader::new(&bytes);
        let err = r.string("name", &ProtocolLimits::new()).unwrap_err();
        assert!(matches!(
            err.kind(),
            ProtocolErrorKind::Truncated { needed: 16, .. }
        ));
    }

    #[test]
    fn strings_must_be_utf8() {
        let mut bytes = 2u32.to_le_bytes().to_vec();
        bytes.extend_from_slice(&[0xF0, 0x28]);
        let mut r = Reader::new(&bytes);
        assert_eq!(
            r.string("name", &ProtocolLimits::new()).unwrap_err().kind(),
            ProtocolErrorKind::InvalidUtf8
        );
    }

    #[test]
    fn booleans_and_reserved_words_are_strict() {
        let bytes = [2u8];
        assert_eq!(
            Reader::new(&bytes).bool("flag").unwrap_err().kind(),
            ProtocolErrorKind::InvalidValue
        );
        let bytes = 1u32.to_le_bytes();
        assert_eq!(
            Reader::new(&bytes).reserved_u32("reserved").unwrap_err().kind(),
            ProtocolErrorKind::InvalidValue
        );
        let bytes = 0u32.to_le_bytes();
        assert!(Reader::new(&bytes).reserved_u32("reserved").is_ok());
    }

    #[test]
    fn trailing_bytes_are_an_error_not_a_shrug() {
        let bytes = [1u8, 0, 0, 0, 9];
        let mut r = Reader::new(&bytes);
        assert_eq!(r.u32("a").unwrap(), 1);
        assert_eq!(
            r.finish("payload").unwrap_err().kind(),
            ProtocolErrorKind::TrailingBytes { extra: 1 }
        );
    }

    #[test]
    fn the_writer_refuses_to_emit_a_field_the_reader_would_reject() {
        let limits = ProtocolLimits::new().with_max_string_bytes(4);
        let mut buf = Vec::new();
        let mut w = Writer::new(&mut buf, limits);
        assert!(w.string("name", "abcd").is_ok());
        let err = w.string("name", "abcde").unwrap_err();
        assert_eq!(
            err.kind(),
            ProtocolErrorKind::LimitExceeded {
                limit: 4,
                requested: 5
            }
        );
    }

    #[test]
    fn blobs_round_trip_including_the_empty_one() {
        let limits = ProtocolLimits::new();
        for payload in [vec![], vec![0u8], vec![7u8; 1000]] {
            let mut buf = Vec::new();
            Writer::new(&mut buf, limits).blob("state", &payload).unwrap();
            let mut r = Reader::new(&buf);
            assert_eq!(r.blob("state", &limits).unwrap(), payload);
            r.finish("payload").unwrap();
        }
    }
}
