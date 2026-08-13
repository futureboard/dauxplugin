//! Errors produced while encoding, decoding or validating protocol data.
//!
//! Decoding is a security boundary: the bytes come from a separate process that may have
//! crashed mid-write, been compiled against a different revision, or be actively hostile.
//! Every failure mode therefore has a named kind, and [`ProtocolError`] is `Copy` and
//! carries only a `&'static str` so that producing one never allocates — a decoder that
//! allocated in order to report "this input is too big" would defeat the point.

use core::fmt;
use std::error::Error;

use daux_abi::{
    DAUX_ERR_ABI_MISMATCH, DAUX_ERR_INVALID_ARG, DAUX_ERR_UNSUPPORTED, DAUX_ERR_VERSION, DauxStatus,
};

/// Result alias used throughout this crate.
pub type ProtocolResult<T> = Result<T, ProtocolError>;

/// What went wrong. [any-thread]
///
/// The variants are deliberately specific: an out-of-process host logs them, and
/// "malformed frame" is useless when diagnosing a version skew between two builds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ProtocolErrorKind {
    /// The frame did not start with [`FRAME_MAGIC`](crate::FRAME_MAGIC). Either the peer
    /// is not speaking this protocol or the stream has desynchronised.
    BadMagic,
    /// The peer speaks a framing revision this build cannot decode.
    UnsupportedVersion {
        /// Version found in the frame header.
        found: u16,
        /// Newest version this build understands.
        supported: u16,
    },
    /// The message kind is not one this build knows. A newer peer may legitimately send
    /// one; the receiver skips the frame rather than treating the stream as broken.
    UnknownMessage {
        /// The unrecognised discriminant.
        kind: u16,
    },
    /// The input ended before a field that the already-decoded part promised.
    Truncated {
        /// Bytes the decoder needed.
        needed: usize,
        /// Bytes actually left.
        available: usize,
    },
    /// A payload decoded successfully but bytes were left over. A well-formed writer never
    /// produces this, so it means the two peers disagree about the layout.
    TrailingBytes {
        /// Number of undecoded bytes at the end of the payload.
        extra: usize,
    },
    /// A length field exceeded a [`ProtocolLimits`](crate::ProtocolLimits) bound. Hitting
    /// this on untrusted input is the intended outcome, not a bug: it is checked *before*
    /// any allocation is made on the strength of that length.
    LimitExceeded {
        /// The bound that was applied.
        limit: usize,
        /// The value the input asked for.
        requested: usize,
    },
    /// A field was structurally present but semantically impossible: a zero block size, a
    /// NaN sample rate, a reserved word that is not zero, an enum discriminant with no
    /// meaning.
    InvalidValue,
    /// The payload CRC did not match, so the frame was damaged in transit or the peer died
    /// halfway through writing it.
    ChecksumMismatch {
        /// CRC the header claimed.
        expected: u32,
        /// CRC of the bytes actually received.
        found: u32,
    },
    /// A string field was not valid UTF-8.
    InvalidUtf8,
    /// A shared-memory structure describes a layout that does not fit its region, overlaps
    /// another region, or is misaligned.
    InvalidLayout,
}

impl ProtocolErrorKind {
    /// Short, stable identifier for logs. [any-thread]
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BadMagic => "bad-magic",
            Self::UnsupportedVersion { .. } => "unsupported-version",
            Self::UnknownMessage { .. } => "unknown-message",
            Self::Truncated { .. } => "truncated",
            Self::TrailingBytes { .. } => "trailing-bytes",
            Self::LimitExceeded { .. } => "limit-exceeded",
            Self::InvalidValue => "invalid-value",
            Self::ChecksumMismatch { .. } => "checksum-mismatch",
            Self::InvalidUtf8 => "invalid-utf8",
            Self::InvalidLayout => "invalid-layout",
        }
    }
}

impl fmt::Display for ProtocolErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadMagic => f.write_str("frame does not start with the DAUx protocol magic"),
            Self::UnsupportedVersion { found, supported } => write!(
                f,
                "framing version {found} is newer than the supported version {supported}"
            ),
            Self::UnknownMessage { kind } => write!(f, "unknown message kind {kind}"),
            Self::Truncated { needed, available } => {
                write!(f, "needed {needed} bytes but only {available} remain")
            }
            Self::TrailingBytes { extra } => write!(f, "{extra} undecoded bytes after the payload"),
            Self::LimitExceeded { limit, requested } => {
                write!(f, "input asked for {requested}, the limit is {limit}")
            }
            Self::InvalidValue => f.write_str("field value is out of range"),
            Self::ChecksumMismatch { expected, found } => {
                write!(
                    f,
                    "payload CRC {found:#010x} does not match header {expected:#010x}"
                )
            }
            Self::InvalidUtf8 => f.write_str("string field is not valid UTF-8"),
            Self::InvalidLayout => f.write_str("shared-memory layout is inconsistent"),
        }
    }
}

/// A protocol failure, with the field or structure that produced it. [any-thread]
///
/// `Copy` and allocation-free by construction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProtocolError {
    kind: ProtocolErrorKind,
    context: &'static str,
}

impl ProtocolError {
    /// [any-thread] Builds an error. `context` names the field or structure at fault and
    /// must be a literal, so that constructing an error never allocates.
    #[must_use]
    pub const fn new(kind: ProtocolErrorKind, context: &'static str) -> Self {
        Self { kind, context }
    }

    /// [any-thread] What went wrong.
    #[must_use]
    pub const fn kind(&self) -> ProtocolErrorKind {
        self.kind
    }

    /// [any-thread] The field or structure at fault.
    #[must_use]
    pub const fn context(&self) -> &'static str {
        self.context
    }

    /// [any-thread] The input ended too early.
    #[must_use]
    pub const fn truncated(context: &'static str, needed: usize, available: usize) -> Self {
        Self::new(ProtocolErrorKind::Truncated { needed, available }, context)
    }

    /// [any-thread] A length field asked for more than the configured bound allows.
    #[must_use]
    pub const fn limit(context: &'static str, limit: usize, requested: usize) -> Self {
        Self::new(
            ProtocolErrorKind::LimitExceeded { limit, requested },
            context,
        )
    }

    /// [any-thread] A field was structurally present but semantically impossible.
    #[must_use]
    pub const fn invalid(context: &'static str) -> Self {
        Self::new(ProtocolErrorKind::InvalidValue, context)
    }

    /// [any-thread] A shared-memory structure does not describe a usable layout.
    #[must_use]
    pub const fn layout(context: &'static str) -> Self {
        Self::new(ProtocolErrorKind::InvalidLayout, context)
    }

    /// [any-thread] The stable ABI status code an adapter reports for this failure.
    ///
    /// Mirrors `docs/specifications/abi-v1.md` §2: a version skew is
    /// [`DAUX_ERR_VERSION`], an unknown message is [`DAUX_ERR_UNSUPPORTED`], a frame that
    /// is not this protocol at all is [`DAUX_ERR_ABI_MISMATCH`], and everything else is a
    /// malformed argument.
    #[must_use]
    pub const fn status(&self) -> DauxStatus {
        match self.kind {
            ProtocolErrorKind::BadMagic => DAUX_ERR_ABI_MISMATCH,
            ProtocolErrorKind::UnsupportedVersion { .. } => DAUX_ERR_VERSION,
            ProtocolErrorKind::UnknownMessage { .. } => DAUX_ERR_UNSUPPORTED,
            _ => DAUX_ERR_INVALID_ARG,
        }
    }

    /// [any-thread] `true` when the stream cannot be resynchronised after this failure.
    ///
    /// Framing errors (magic, version, an over-long or corrupt frame) leave the reader
    /// with no way to find the next frame boundary, so the connection must be torn down.
    /// Payload errors do not: the frame length was already known and trusted, so the
    /// reader can skip the frame and carry on.
    #[must_use]
    pub const fn is_fatal_to_stream(&self) -> bool {
        matches!(
            self.kind,
            ProtocolErrorKind::BadMagic
                | ProtocolErrorKind::UnsupportedVersion { .. }
                | ProtocolErrorKind::LimitExceeded { .. }
                | ProtocolErrorKind::ChecksumMismatch { .. }
        )
    }
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.context, self.kind)
    }
}

impl Error for ProtocolError {}

#[cfg(test)]
mod tests {
    use super::{ProtocolError, ProtocolErrorKind};
    use daux_abi::{DAUX_ERR_ABI_MISMATCH, DAUX_ERR_INVALID_ARG, DAUX_ERR_UNSUPPORTED};

    #[test]
    fn errors_carry_their_context_into_the_message() {
        let e = ProtocolError::truncated("Handshake::peer_name", 12, 3);
        assert_eq!(
            e.to_string(),
            "Handshake::peer_name: needed 12 bytes but only 3 remain"
        );
        assert_eq!(e.kind().as_str(), "truncated");
    }

    #[test]
    fn framing_errors_are_fatal_and_payload_errors_are_not() {
        assert!(ProtocolError::new(ProtocolErrorKind::BadMagic, "frame").is_fatal_to_stream());
        assert!(ProtocolError::limit("frame", 16, 32).is_fatal_to_stream());
        assert!(!ProtocolError::invalid("config.sample_rate").is_fatal_to_stream());
        assert!(!ProtocolError::truncated("payload", 4, 0).is_fatal_to_stream());
    }

    #[test]
    fn status_codes_distinguish_the_three_interesting_cases() {
        assert_eq!(
            ProtocolError::new(ProtocolErrorKind::BadMagic, "frame").status(),
            DAUX_ERR_ABI_MISMATCH
        );
        assert_eq!(
            ProtocolError::new(ProtocolErrorKind::UnknownMessage { kind: 999 }, "frame").status(),
            DAUX_ERR_UNSUPPORTED
        );
        assert_eq!(ProtocolError::invalid("x").status(), DAUX_ERR_INVALID_ARG);
    }

    #[test]
    fn every_kind_has_a_distinct_slug_and_a_message() {
        let kinds = [
            ProtocolErrorKind::BadMagic,
            ProtocolErrorKind::UnsupportedVersion {
                found: 2,
                supported: 1,
            },
            ProtocolErrorKind::UnknownMessage { kind: 7 },
            ProtocolErrorKind::Truncated {
                needed: 1,
                available: 0,
            },
            ProtocolErrorKind::TrailingBytes { extra: 3 },
            ProtocolErrorKind::LimitExceeded {
                limit: 1,
                requested: 2,
            },
            ProtocolErrorKind::InvalidValue,
            ProtocolErrorKind::ChecksumMismatch {
                expected: 1,
                found: 2,
            },
            ProtocolErrorKind::InvalidUtf8,
            ProtocolErrorKind::InvalidLayout,
        ];
        let mut slugs: Vec<&str> = kinds.iter().map(|k| k.as_str()).collect();
        slugs.sort_unstable();
        let count = slugs.len();
        slugs.dedup();
        assert_eq!(slugs.len(), count, "two kinds share a slug");
        for k in kinds {
            assert!(!k.to_string().is_empty());
        }
    }
}
