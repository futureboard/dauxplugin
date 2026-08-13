//! On-disk constants of the DAUx state container.
//!
//! The container is deliberately boring: little-endian, length-prefixed, tagged, and
//! written in insertion order so that identical input always produces identical bytes.
//! Serialising a document is a pure function of the values put into it — no hash-map
//! iteration order, no floating-point formatting, no timestamps.
//!
//! ```text
//! header (20 bytes)
//!   0   [u8; 8]  magic = "DAUXST\0\0"
//!   8   u32      container format version (FORMAT_VERSION)
//!  12   u32      plug-in schema version (StateVersion)
//!  16   u32      total entry count, including group markers
//!
//! entry (repeated `entry count` times)
//!   u32          key length in bytes
//!   [u8]         key, UTF-8, no '/' (the path separator), empty only for TAG_GROUP_END
//!   u8           type tag
//!   ...          value, determined by the tag:
//!                  TAG_F64          8 bytes, IEEE-754 binary64, little-endian
//!                  TAG_I64          8 bytes, two's complement, little-endian
//!                  TAG_BOOL         1 byte, exactly 0 or 1
//!                  TAG_STR          u32 length + that many bytes of UTF-8
//!                  TAG_BYTES        u32 length + that many bytes
//!                  TAG_GROUP_BEGIN  no payload; entries until the matching end are
//!                                   nested one level deeper
//!                  TAG_GROUP_END    no payload; key length must be 0
//! ```
//!
//! Nesting is expressed purely by the begin/end markers, and lookups address a nested
//! value with a `'/'`-separated path such as `"filter/cutoff"`.

/// Magic bytes at offset 0: `DAUXST\0\0`.
pub const MAGIC: [u8; 8] = *b"DAUXST\0\0";

/// Version of the container format this build writes.
///
/// A reader accepts any format version from `1` up to and including this one and rejects
/// anything newer with [`StateErrorKind::UnsupportedVersion`](crate::StateErrorKind).
pub const FORMAT_VERSION: u32 = 1;

/// Size of the fixed header in bytes.
pub const HEADER_LEN: usize = 20;

/// Byte offset of the container format version field.
pub const OFFSET_FORMAT_VERSION: usize = 8;

/// Byte offset of the plug-in schema version field.
pub const OFFSET_SCHEMA_VERSION: usize = 12;

/// Byte offset of the entry-count field.
pub const OFFSET_ENTRY_COUNT: usize = 16;

/// Type tag for an IEEE-754 binary64 value.
pub const TAG_F64: u8 = 1;
/// Type tag for a two's-complement 64-bit signed integer.
pub const TAG_I64: u8 = 2;
/// Type tag for a boolean, encoded as a single `0` or `1` byte.
pub const TAG_BOOL: u8 = 3;
/// Type tag for a length-prefixed UTF-8 string.
pub const TAG_STR: u8 = 4;
/// Type tag for a length-prefixed opaque byte string.
pub const TAG_BYTES: u8 = 5;
/// Type tag opening a nested group.
pub const TAG_GROUP_BEGIN: u8 = 6;
/// Type tag closing the innermost open group. Carries an empty key and no payload.
pub const TAG_GROUP_END: u8 = 7;

/// The character that separates path segments in a lookup key. Keys may not contain it.
pub const PATH_SEPARATOR: char = '/';
