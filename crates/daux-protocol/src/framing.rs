//! The length-prefixed binary framing that carries control-plane messages.
//!
//! # Layout
//!
//! Every control frame is a fixed 20-byte header followed by exactly `payload_len` bytes.
//! All fields are little-endian:
//!
//! ```text
//! offset  size  field         notes
//! ------  ----  ------------  ---------------------------------------------------------
//!      0     4  magic         b"DXPC"; a stream that does not start with it is not ours
//!      4     2  version       framing revision, currently PROTOCOL_VERSION == 1
//!      6     2  kind          message discriminant, so a router need not parse the body
//!      8     2  flags         FrameFlags bitset
//!     10     2  reserved      MUST be zero; a non-zero value is rejected
//!     12     4  payload_len   bytes following the header
//!     16     4  payload_crc   CRC-32/ISO-HDLC over exactly those bytes
//!     20   ...  payload       message-specific, see `control`
//! ```
//!
//! # Why each field is there
//!
//! The **magic** and **version** turn "the peer is speaking something else" into a clean
//! error instead of a wild length. The **kind** is duplicated in the header so a
//! supervisor can route or count frames without trusting the payload. The **CRC** exists
//! because the peer is a process that can die *mid-write*: a truncated-then-reused shared
//! pipe buffer produces a frame whose header is plausible and whose body is garbage, and
//! a checksum is the only thing that distinguishes it from a valid frame. The
//! **reserved** word is the growth mechanism, and is checked to be zero so that a future
//! writer cannot silently be misread by an older reader.
//!
//! # Reading from a stream
//!
//! A byte-stream transport does not preserve frame boundaries, so a reader peeks the
//! header, learns the total length, and waits for that many bytes:
//!
//! ```
//! use daux_protocol::{ControlMessage, ProtocolLimits, peek_frame_len, FRAME_HEADER_LEN};
//!
//! let limits = ProtocolLimits::new();
//! let frame = ControlMessage::RequestRestart {
//!     instance: daux_protocol::InstanceId(1),
//!     flags: daux_protocol::RestartFlags::PROCESS,
//! }
//! .encode(&limits)
//! .unwrap();
//!
//! // Only the header has arrived so far.
//! assert!(peek_frame_len(&frame[..FRAME_HEADER_LEN - 1], &limits).unwrap().is_none());
//! let total = peek_frame_len(&frame[..FRAME_HEADER_LEN], &limits).unwrap().unwrap();
//! assert_eq!(total, frame.len());
//! ```

use crate::error::{ProtocolError, ProtocolErrorKind, ProtocolResult};
use crate::limits::ProtocolLimits;

/// Magic word at the start of every control frame: `b"DXPC"` — DAUx Protocol, Control.
pub const FRAME_MAGIC: [u8; 4] = *b"DXPC";

/// Framing revision this build writes and is the newest it can read.
pub const PROTOCOL_VERSION: u16 = 1;

/// Size of the fixed frame header in bytes.
pub const FRAME_HEADER_LEN: usize = 20;

const OFF_MAGIC: usize = 0;
const OFF_VERSION: usize = 4;
const OFF_KIND: usize = 6;
const OFF_FLAGS: usize = 8;
const OFF_RESERVED: usize = 10;
const OFF_PAYLOAD_LEN: usize = 12;
const OFF_PAYLOAD_CRC: usize = 16;

/// Bitset of per-frame hints. [any-thread]
///
/// Unknown bits are preserved and ignored rather than rejected: a newer peer may set a
/// hint an older one has no use for, and dropping the connection over a hint would be a
/// gratuitous incompatibility. Semantic changes ride on [`PROTOCOL_VERSION`] instead.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FrameFlags(pub u16);

impl FrameFlags {
    /// No flags set.
    pub const NONE: Self = Self(0);
    /// This frame answers an earlier request rather than initiating one.
    pub const RESPONSE: Self = Self(1 << 0);
    /// The sender does not expect and will ignore any reply.
    pub const NO_REPLY: Self = Self(1 << 1);
    /// Every bit this build assigns meaning to.
    pub const KNOWN: Self = Self(0b11);

    /// [any-thread] `true` when every bit of `other` is set.
    #[inline]
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// [any-thread] The union of two flag sets.
    #[inline]
    #[must_use]
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// [any-thread] Bits this build does not understand.
    #[inline]
    #[must_use]
    pub const fn unknown_bits(self) -> u16 {
        self.0 & !Self::KNOWN.0
    }
}

/// The decoded fixed header of a control frame. [any-thread]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FrameHeader {
    /// Framing revision the sender used.
    pub version: u16,
    /// Message discriminant; see [`MessageKind`](crate::MessageKind).
    pub kind: u16,
    /// Per-frame hints.
    pub flags: FrameFlags,
    /// Bytes of payload following the header.
    pub payload_len: u32,
    /// CRC-32/ISO-HDLC of the payload bytes.
    pub payload_crc: u32,
}

impl FrameHeader {
    /// Size of the encoded header, in bytes.
    pub const LEN: usize = FRAME_HEADER_LEN;

    /// [any-thread] Builds a header for a payload that has already been produced.
    pub fn for_payload(kind: u16, flags: FrameFlags, payload: &[u8]) -> ProtocolResult<Self> {
        let payload_len = u32::try_from(payload.len()).map_err(|_| {
            ProtocolError::limit("frame.payload_len", u32::MAX as usize, payload.len())
        })?;
        Ok(Self {
            version: PROTOCOL_VERSION,
            kind,
            flags,
            payload_len,
            payload_crc: crc32(payload),
        })
    }

    /// [any-thread] Encodes the header into its fixed 20 bytes.
    #[must_use]
    pub fn encode(&self) -> [u8; FRAME_HEADER_LEN] {
        let mut out = [0u8; FRAME_HEADER_LEN];
        out[OFF_MAGIC..OFF_MAGIC + 4].copy_from_slice(&FRAME_MAGIC);
        out[OFF_VERSION..OFF_VERSION + 2].copy_from_slice(&self.version.to_le_bytes());
        out[OFF_KIND..OFF_KIND + 2].copy_from_slice(&self.kind.to_le_bytes());
        out[OFF_FLAGS..OFF_FLAGS + 2].copy_from_slice(&self.flags.0.to_le_bytes());
        out[OFF_RESERVED..OFF_RESERVED + 2].copy_from_slice(&0u16.to_le_bytes());
        out[OFF_PAYLOAD_LEN..OFF_PAYLOAD_LEN + 4].copy_from_slice(&self.payload_len.to_le_bytes());
        out[OFF_PAYLOAD_CRC..OFF_PAYLOAD_CRC + 4].copy_from_slice(&self.payload_crc.to_le_bytes());
        out
    }

    /// [any-thread] Decodes and validates a header from the first [`FRAME_HEADER_LEN`]
    /// bytes of `bytes`.
    ///
    /// Checks, in order: enough bytes for a header, the magic, the version, the reserved
    /// word, and that the total frame length is within `limits.max_frame_bytes`. It does
    /// **not** require the payload to be present — that is exactly what a stream reader
    /// uses this for.
    ///
    /// # Errors
    ///
    /// [`ProtocolErrorKind::Truncated`] when fewer than [`FRAME_HEADER_LEN`] bytes are
    /// available, [`ProtocolErrorKind::BadMagic`], [`ProtocolErrorKind::UnsupportedVersion`],
    /// [`ProtocolErrorKind::InvalidValue`] for a non-zero reserved word, or
    /// [`ProtocolErrorKind::LimitExceeded`] when the declared frame is too large.
    pub fn parse(bytes: &[u8], limits: &ProtocolLimits) -> ProtocolResult<Self> {
        if bytes.len() < FRAME_HEADER_LEN {
            return Err(ProtocolError::truncated(
                "frame.header",
                FRAME_HEADER_LEN,
                bytes.len(),
            ));
        }
        if bytes[OFF_MAGIC..OFF_MAGIC + 4] != FRAME_MAGIC {
            return Err(ProtocolError::new(
                ProtocolErrorKind::BadMagic,
                "frame.magic",
            ));
        }
        let version = le_u16(bytes, OFF_VERSION);
        if version > PROTOCOL_VERSION {
            return Err(ProtocolError::new(
                ProtocolErrorKind::UnsupportedVersion {
                    found: version,
                    supported: PROTOCOL_VERSION,
                },
                "frame.version",
            ));
        }
        if le_u16(bytes, OFF_RESERVED) != 0 {
            return Err(ProtocolError::invalid("frame.reserved"));
        }
        let payload_len = le_u32(bytes, OFF_PAYLOAD_LEN);
        let total = (payload_len as usize)
            .checked_add(FRAME_HEADER_LEN)
            .ok_or_else(|| {
                ProtocolError::limit(
                    "frame.payload_len",
                    limits.max_frame_bytes,
                    payload_len as usize,
                )
            })?;
        if total > limits.max_frame_bytes {
            return Err(ProtocolError::limit(
                "frame.payload_len",
                limits.max_frame_bytes,
                total,
            ));
        }
        Ok(Self {
            version,
            kind: le_u16(bytes, OFF_KIND),
            flags: FrameFlags(le_u16(bytes, OFF_FLAGS)),
            payload_len,
            payload_crc: le_u32(bytes, OFF_PAYLOAD_CRC),
        })
    }

    /// [any-thread] Total encoded size of the frame, header included.
    ///
    /// Never overflows: [`FrameHeader::parse`] has already bounded `payload_len` by
    /// `max_frame_bytes`, and [`FrameHeader::for_payload`] by the payload that exists.
    #[inline]
    #[must_use]
    pub const fn frame_len(&self) -> usize {
        FRAME_HEADER_LEN + self.payload_len as usize
    }

    /// [any-thread] Verifies `payload` against the declared length and CRC.
    ///
    /// # Errors
    ///
    /// [`ProtocolErrorKind::Truncated`] when the payload is shorter than declared,
    /// [`ProtocolErrorKind::TrailingBytes`] when it is longer, and
    /// [`ProtocolErrorKind::ChecksumMismatch`] when the bytes are damaged.
    pub fn verify_payload(&self, payload: &[u8]) -> ProtocolResult<()> {
        let declared = self.payload_len as usize;
        if payload.len() < declared {
            return Err(ProtocolError::truncated(
                "frame.payload",
                declared,
                payload.len(),
            ));
        }
        if payload.len() > declared {
            return Err(ProtocolError::new(
                ProtocolErrorKind::TrailingBytes {
                    extra: payload.len() - declared,
                },
                "frame.payload",
            ));
        }
        let found = crc32(payload);
        if found != self.payload_crc {
            return Err(ProtocolError::new(
                ProtocolErrorKind::ChecksumMismatch {
                    expected: self.payload_crc,
                    found,
                },
                "frame.payload_crc",
            ));
        }
        Ok(())
    }
}

/// [any-thread] Total length of the frame that starts at `prefix`, if the header has
/// arrived.
///
/// Returns `Ok(None)` when fewer than [`FRAME_HEADER_LEN`] bytes are available, which is
/// the normal case for a stream transport that has only delivered part of a frame.
///
/// # Errors
///
/// Any error [`FrameHeader::parse`] can produce other than a short read. Every one of them
/// is unrecoverable for a byte stream: without a trustworthy length there is no way to
/// find the next frame boundary, so the caller must drop the connection.
pub fn peek_frame_len(prefix: &[u8], limits: &ProtocolLimits) -> ProtocolResult<Option<usize>> {
    if prefix.len() < FRAME_HEADER_LEN {
        return Ok(None);
    }
    Ok(Some(FrameHeader::parse(prefix, limits)?.frame_len()))
}

#[inline]
fn le_u16(bytes: &[u8], at: usize) -> u16 {
    // The caller has already checked `bytes.len() >= FRAME_HEADER_LEN` and every offset
    // used here is a compile-time constant inside that header, so the two indices are in
    // range and the array conversion cannot fail.
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

#[inline]
fn le_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

// ------------------------------------------------------------------------------ crc ---

/// CRC-32/ISO-HDLC lookup table, built at compile time so the crate keeps zero runtime
/// initialisation and no `OnceLock`.
static CRC_TABLE: [u32; 256] = build_crc_table();

const fn build_crc_table() -> [u32; 256] {
    const POLY: u32 = 0xEDB8_8320;
    let mut table = [0u32; 256];
    let mut i = 0usize;
    while i < 256 {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ POLY
            } else {
                crc >> 1
            };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

/// [any-thread] CRC-32/ISO-HDLC (the zlib/PNG variant) of `bytes`.
///
/// Allocation-free and table-driven; the check value for `b"123456789"` is `0xCBF43926`.
#[must_use]
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in bytes {
        let index = ((crc ^ u32::from(b)) & 0xFF) as usize;
        crc = (crc >> 8) ^ CRC_TABLE[index];
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::{
        FRAME_HEADER_LEN, FRAME_MAGIC, FrameFlags, FrameHeader, PROTOCOL_VERSION, crc32,
        peek_frame_len,
    };
    use crate::error::ProtocolErrorKind;
    use crate::limits::ProtocolLimits;

    #[test]
    fn crc32_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"a"), 0xE8B7_BE43);
    }

    #[test]
    fn crc32_notices_a_single_flipped_bit() {
        let mut data = vec![0u8; 512];
        for (i, b) in data.iter_mut().enumerate() {
            *b = i as u8;
        }
        let clean = crc32(&data);
        for bit in [0usize, 7, 8, 4095] {
            let mut corrupt = data.clone();
            corrupt[bit / 8] ^= 1 << (bit % 8);
            assert_ne!(crc32(&corrupt), clean, "flipping bit {bit} went unnoticed");
        }
    }

    #[test]
    fn a_header_round_trips_exactly() {
        let payload = b"some payload".as_slice();
        let header = FrameHeader::for_payload(7, FrameFlags::RESPONSE, payload).unwrap();
        let bytes = header.encode();
        assert_eq!(bytes.len(), FRAME_HEADER_LEN);
        assert_eq!(&bytes[0..4], &FRAME_MAGIC);
        let parsed = FrameHeader::parse(&bytes, &ProtocolLimits::new()).unwrap();
        assert_eq!(parsed, header);
        assert_eq!(parsed.version, PROTOCOL_VERSION);
        assert_eq!(parsed.frame_len(), FRAME_HEADER_LEN + payload.len());
        parsed.verify_payload(payload).unwrap();
    }

    #[test]
    fn a_short_header_is_a_clean_truncation_not_a_panic() {
        let header = FrameHeader::for_payload(1, FrameFlags::NONE, b"x").unwrap();
        let bytes = header.encode();
        let limits = ProtocolLimits::new();
        for n in 0..FRAME_HEADER_LEN {
            let err = FrameHeader::parse(&bytes[..n], &limits).unwrap_err();
            assert_eq!(
                err.kind(),
                ProtocolErrorKind::Truncated {
                    needed: FRAME_HEADER_LEN,
                    available: n
                }
            );
            assert_eq!(peek_frame_len(&bytes[..n], &limits).unwrap(), None);
        }
    }

    #[test]
    fn a_foreign_stream_is_rejected_on_the_magic() {
        let mut bytes = FrameHeader::for_payload(1, FrameFlags::NONE, b"")
            .unwrap()
            .encode();
        bytes[0] = b'X';
        assert_eq!(
            FrameHeader::parse(&bytes, &ProtocolLimits::new())
                .unwrap_err()
                .kind(),
            ProtocolErrorKind::BadMagic
        );
    }

    #[test]
    fn a_newer_framing_revision_is_rejected_but_an_older_one_is_not() {
        let mut bytes = FrameHeader::for_payload(1, FrameFlags::NONE, b"")
            .unwrap()
            .encode();
        bytes[4..6].copy_from_slice(&(PROTOCOL_VERSION + 1).to_le_bytes());
        assert_eq!(
            FrameHeader::parse(&bytes, &ProtocolLimits::new())
                .unwrap_err()
                .kind(),
            ProtocolErrorKind::UnsupportedVersion {
                found: PROTOCOL_VERSION + 1,
                supported: PROTOCOL_VERSION,
            }
        );
        bytes[4..6].copy_from_slice(&0u16.to_le_bytes());
        assert_eq!(
            FrameHeader::parse(&bytes, &ProtocolLimits::new())
                .unwrap()
                .version,
            0
        );
    }

    #[test]
    fn a_non_zero_reserved_word_is_rejected() {
        let mut bytes = FrameHeader::for_payload(1, FrameFlags::NONE, b"")
            .unwrap()
            .encode();
        bytes[10..12].copy_from_slice(&1u16.to_le_bytes());
        assert_eq!(
            FrameHeader::parse(&bytes, &ProtocolLimits::new())
                .unwrap_err()
                .kind(),
            ProtocolErrorKind::InvalidValue
        );
    }

    #[test]
    fn an_absurd_length_prefix_is_capped_before_anyone_allocates() {
        let mut bytes = FrameHeader::for_payload(1, FrameFlags::NONE, b"")
            .unwrap()
            .encode();
        bytes[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        let limits = ProtocolLimits::new();
        let err = FrameHeader::parse(&bytes, &limits).unwrap_err();
        assert!(matches!(
            err.kind(),
            ProtocolErrorKind::LimitExceeded { limit, .. } if limit == limits.max_frame_bytes
        ));
        assert!(err.is_fatal_to_stream());
    }

    #[test]
    fn a_corrupt_payload_fails_the_checksum_rather_than_decoding_as_garbage() {
        let payload = b"the quick brown fox".to_vec();
        let header = FrameHeader::for_payload(3, FrameFlags::NONE, &payload).unwrap();
        let mut corrupt = payload.clone();
        corrupt[4] ^= 0x20;
        assert!(matches!(
            header.verify_payload(&corrupt).unwrap_err().kind(),
            ProtocolErrorKind::ChecksumMismatch { .. }
        ));
        assert!(matches!(
            header.verify_payload(&payload[..3]).unwrap_err().kind(),
            ProtocolErrorKind::Truncated { .. }
        ));
        let mut longer = payload.clone();
        longer.push(0);
        assert!(matches!(
            header.verify_payload(&longer).unwrap_err().kind(),
            ProtocolErrorKind::TrailingBytes { extra: 1 }
        ));
    }

    #[test]
    fn flags_keep_unknown_bits_instead_of_rejecting_them() {
        let f = FrameFlags(0b1011);
        assert!(f.contains(FrameFlags::RESPONSE));
        assert!(f.contains(FrameFlags::NO_REPLY));
        assert_eq!(f.unknown_bits(), 0b1000);
        assert_eq!(
            FrameFlags::RESPONSE.with(FrameFlags::NO_REPLY),
            FrameFlags::KNOWN
        );
        let bytes = FrameHeader {
            version: PROTOCOL_VERSION,
            kind: 1,
            flags: f,
            payload_len: 0,
            payload_crc: crc32(b""),
        }
        .encode();
        assert_eq!(
            FrameHeader::parse(&bytes, &ProtocolLimits::new())
                .unwrap()
                .flags,
            f
        );
    }
}
