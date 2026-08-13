//! Errors produced by transports, channels and shared regions.
//!
//! An IPC failure is not exceptional: the peer is a separate process that may be slow,
//! busy, gone, or hostile, and "nothing has arrived yet" is the single most common outcome
//! of a receive. [`IpcError`] therefore has to be cheap enough to return thousands of times
//! a second and precise enough to act on, which is why it is `Copy`, carries only a
//! `&'static str` context, and never allocates.
//!
//! Three questions a caller asks of every error are answered by a method rather than a
//! match: [`IpcError::is_would_block`] ("try again later"), [`IpcError::is_closed`] ("give
//! up on this peer") and [`IpcError::is_fatal`] ("tear the connection down"). Framing
//! failures delegate that last one to [`ProtocolError::is_fatal_to_stream`], because
//! `daux-protocol` is the crate that knows whether a byte stream can be resynchronised.

use core::fmt;
use std::error::Error;

use daux_protocol::ProtocolError;

/// Result alias used throughout this crate.
pub type IpcResult<T> = Result<T, IpcError>;

/// What went wrong. [any-thread]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum IpcErrorKind {
    /// Nothing has arrived yet. The transport is healthy; the caller should come back
    /// later. This is not a failure and must never tear a connection down.
    WouldBlock,
    /// The connection is closed: either side called `close`, or the peer went away. Any
    /// bytes the peer wrote before closing have already been delivered.
    Closed,
    /// The outbound queue has no room. The frame was **not** sent and no bytes were
    /// written, so the caller may retry it unchanged once the peer drains.
    Full,
    /// This build does not implement the transport that was asked for. Every platform
    /// transport that has not landed yet reports this rather than pretending to work.
    Unsupported,
    /// An argument could not be acted on: an empty frame, an out-of-range region offset, a
    /// misaligned sample plane.
    InvalidArgument,
    /// The operation is not legal in the current state, such as publishing a shared region
    /// this endpoint does not currently own.
    InvalidState,
    /// A shared region could not be allocated.
    OutOfMemory,
    /// A bound was exceeded — most often
    /// [`ProtocolLimits::max_frame_bytes`](daux_protocol::ProtocolLimits::max_frame_bytes)
    /// while encoding a message that is too large to send.
    LimitExceeded {
        /// The bound that was applied.
        limit: usize,
        /// The size that was asked for.
        requested: usize,
    },
    /// The bytes on the wire were not a well-formed frame. The inner error names the field
    /// at fault and says whether the stream can be resynchronised.
    Protocol(ProtocolError),
    /// The operating system refused. `code` is the raw platform error number, kept as an
    /// integer so that constructing the error never allocates a message.
    Io {
        /// Raw OS error number, as `GetLastError`/`errno` reported it.
        code: i32,
    },
}

impl IpcErrorKind {
    /// [any-thread] Short, stable identifier for logs.
    ///
    /// A framing failure reports the underlying protocol slug rather than a flat
    /// `"protocol"`, so a log line still distinguishes a checksum mismatch from a version
    /// skew.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WouldBlock => "would-block",
            Self::Closed => "closed",
            Self::Full => "full",
            Self::Unsupported => "unsupported",
            Self::InvalidArgument => "invalid-argument",
            Self::InvalidState => "invalid-state",
            Self::OutOfMemory => "out-of-memory",
            Self::LimitExceeded { .. } => "limit-exceeded",
            Self::Protocol(e) => e.kind().as_str(),
            Self::Io { .. } => "io",
        }
    }
}

impl fmt::Display for IpcErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WouldBlock => f.write_str("no data has arrived yet"),
            Self::Closed => f.write_str("the connection is closed"),
            Self::Full => f.write_str("the outbound queue is full"),
            Self::Unsupported => f.write_str("this build does not implement that transport"),
            Self::InvalidArgument => f.write_str("argument cannot be acted on"),
            Self::InvalidState => f.write_str("operation is not legal in this state"),
            Self::OutOfMemory => f.write_str("the shared region could not be allocated"),
            Self::LimitExceeded { limit, requested } => {
                write!(f, "asked for {requested}, the limit is {limit}")
            }
            Self::Protocol(e) => write!(f, "{e}"),
            Self::Io { code } => write!(f, "operating system error {code}"),
        }
    }
}

/// An IPC failure, with the operation or field that produced it. [any-thread]
///
/// `Copy` and allocation-free by construction: an error is returned from the receive path
/// on every empty poll, and a receive path that allocated in order to say "nothing yet"
/// would be worse than useless.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct IpcError {
    kind: IpcErrorKind,
    context: &'static str,
}

impl IpcError {
    /// [any-thread] Builds an error. `context` names the operation or field at fault and
    /// must be a literal, so that constructing an error never allocates.
    #[must_use]
    pub const fn new(kind: IpcErrorKind, context: &'static str) -> Self {
        Self { kind, context }
    }

    /// [any-thread] What went wrong.
    #[inline]
    #[must_use]
    pub const fn kind(&self) -> IpcErrorKind {
        self.kind
    }

    /// [any-thread] The operation or field at fault.
    #[inline]
    #[must_use]
    pub const fn context(&self) -> &'static str {
        self.context
    }

    /// [any-thread] Nothing has arrived yet; try again later.
    #[inline]
    #[must_use]
    pub const fn would_block(context: &'static str) -> Self {
        Self::new(IpcErrorKind::WouldBlock, context)
    }

    /// [any-thread] The connection is closed.
    #[inline]
    #[must_use]
    pub const fn closed(context: &'static str) -> Self {
        Self::new(IpcErrorKind::Closed, context)
    }

    /// [any-thread] The outbound queue had no room; nothing was sent.
    #[inline]
    #[must_use]
    pub const fn full(context: &'static str) -> Self {
        Self::new(IpcErrorKind::Full, context)
    }

    /// [any-thread] This build cannot provide what was asked for.
    #[inline]
    #[must_use]
    pub const fn unsupported(context: &'static str) -> Self {
        Self::new(IpcErrorKind::Unsupported, context)
    }

    /// [any-thread] An argument cannot be acted on.
    #[inline]
    #[must_use]
    pub const fn invalid_argument(context: &'static str) -> Self {
        Self::new(IpcErrorKind::InvalidArgument, context)
    }

    /// [any-thread] The operation is not legal in the current state.
    #[inline]
    #[must_use]
    pub const fn invalid_state(context: &'static str) -> Self {
        Self::new(IpcErrorKind::InvalidState, context)
    }

    /// [any-thread] A shared region could not be allocated.
    #[inline]
    #[must_use]
    pub const fn out_of_memory(context: &'static str) -> Self {
        Self::new(IpcErrorKind::OutOfMemory, context)
    }

    /// [any-thread] A size bound was exceeded.
    #[inline]
    #[must_use]
    pub const fn limit(context: &'static str, limit: usize, requested: usize) -> Self {
        Self::new(IpcErrorKind::LimitExceeded { limit, requested }, context)
    }

    /// [any-thread] The operating system refused, with its raw error number.
    #[inline]
    #[must_use]
    pub const fn io(context: &'static str, code: i32) -> Self {
        Self::new(IpcErrorKind::Io { code }, context)
    }

    /// [any-thread] Wraps a framing or decoding failure, keeping the field name
    /// `daux-protocol` already attached to it.
    #[inline]
    #[must_use]
    pub const fn protocol(error: ProtocolError) -> Self {
        Self::new(IpcErrorKind::Protocol(error), error.context())
    }

    /// [any-thread] `true` for [`IpcErrorKind::WouldBlock`].
    #[inline]
    #[must_use]
    pub const fn is_would_block(&self) -> bool {
        matches!(self.kind, IpcErrorKind::WouldBlock)
    }

    /// [any-thread] `true` for [`IpcErrorKind::Closed`].
    #[inline]
    #[must_use]
    pub const fn is_closed(&self) -> bool {
        matches!(self.kind, IpcErrorKind::Closed)
    }

    /// [any-thread] `true` for [`IpcErrorKind::Unsupported`].
    #[inline]
    #[must_use]
    pub const fn is_unsupported(&self) -> bool {
        matches!(self.kind, IpcErrorKind::Unsupported)
    }

    /// [any-thread] `true` when the connection cannot carry further traffic.
    ///
    /// A closed peer, an OS failure, a missing transport and a memory failure are all
    /// terminal. A framing failure defers to [`ProtocolError::is_fatal_to_stream`]: a bad
    /// magic or an absurd length prefix leaves the reader with no way to find the next
    /// frame boundary, whereas an unknown message kind does not. Backpressure, an
    /// unencodable message and a wrong-state call are all recoverable and report `false`.
    #[must_use]
    pub const fn is_fatal(&self) -> bool {
        match self.kind {
            IpcErrorKind::Closed
            | IpcErrorKind::Unsupported
            | IpcErrorKind::OutOfMemory
            | IpcErrorKind::Io { .. } => true,
            IpcErrorKind::Protocol(e) => e.is_fatal_to_stream(),
            IpcErrorKind::WouldBlock
            | IpcErrorKind::Full
            | IpcErrorKind::InvalidArgument
            | IpcErrorKind::InvalidState
            | IpcErrorKind::LimitExceeded { .. } => false,
        }
    }

    /// [any-thread] The underlying framing failure, when there is one.
    #[inline]
    #[must_use]
    pub const fn protocol_error(&self) -> Option<ProtocolError> {
        match self.kind {
            IpcErrorKind::Protocol(e) => Some(e),
            _ => None,
        }
    }
}

impl From<ProtocolError> for IpcError {
    #[inline]
    fn from(error: ProtocolError) -> Self {
        Self::protocol(error)
    }
}

impl fmt::Display for IpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.context, self.kind)
    }
}

impl Error for IpcError {}

#[cfg(test)]
mod tests {
    use super::{IpcError, IpcErrorKind};
    use daux_protocol::{ProtocolError, ProtocolErrorKind};

    #[test]
    fn errors_carry_their_context_into_the_message() {
        let e = IpcError::would_block("LoopbackTransport::recv");
        assert_eq!(
            e.to_string(),
            "LoopbackTransport::recv: no data has arrived yet"
        );
        assert_eq!(e.kind().as_str(), "would-block");
        assert!(e.is_would_block());
        assert!(!e.is_fatal());
    }

    #[test]
    fn a_wrapped_protocol_error_keeps_the_field_name_and_the_slug() {
        let inner = ProtocolError::truncated("frame.header", 20, 7);
        let e = IpcError::protocol(inner);
        assert_eq!(e.context(), "frame.header");
        assert_eq!(e.kind().as_str(), "truncated");
        assert_eq!(e.protocol_error(), Some(inner));
        assert_eq!(IpcError::from(inner), e);
        assert!(e.to_string().contains("needed 20 bytes"));
    }

    /// The whole point of `is_fatal` is that a caller can decide whether to keep the
    /// connection without matching on ten variants. Pin the classification down.
    #[test]
    fn only_terminal_failures_are_fatal() {
        let fatal = [
            IpcError::closed("x"),
            IpcError::unsupported("x"),
            IpcError::out_of_memory("x"),
            IpcError::io("x", 5),
            IpcError::protocol(ProtocolError::new(ProtocolErrorKind::BadMagic, "frame")),
            IpcError::protocol(ProtocolError::limit("frame", 16, 1 << 30)),
        ];
        for e in fatal {
            assert!(e.is_fatal(), "{e} should be fatal");
        }
        let recoverable = [
            IpcError::would_block("x"),
            IpcError::full("x"),
            IpcError::invalid_argument("x"),
            IpcError::invalid_state("x"),
            IpcError::limit("x", 1, 2),
            // A newer peer sending a message this build does not know is not a reason to
            // drop the connection.
            IpcError::protocol(ProtocolError::new(
                ProtocolErrorKind::UnknownMessage { kind: 999 },
                "frame.kind",
            )),
            IpcError::protocol(ProtocolError::truncated("payload", 4, 1)),
        ];
        for e in recoverable {
            assert!(!e.is_fatal(), "{e} should be recoverable");
        }
    }

    #[test]
    fn every_kind_has_a_slug_and_a_message() {
        let kinds = [
            IpcErrorKind::WouldBlock,
            IpcErrorKind::Closed,
            IpcErrorKind::Full,
            IpcErrorKind::Unsupported,
            IpcErrorKind::InvalidArgument,
            IpcErrorKind::InvalidState,
            IpcErrorKind::OutOfMemory,
            IpcErrorKind::LimitExceeded {
                limit: 1,
                requested: 2,
            },
            IpcErrorKind::Io { code: 232 },
        ];
        let mut slugs: Vec<&str> = kinds.iter().map(|k| k.as_str()).collect();
        let count = slugs.len();
        slugs.sort_unstable();
        slugs.dedup();
        assert_eq!(slugs.len(), count, "two kinds share a slug");
        for k in kinds {
            assert!(!k.to_string().is_empty());
        }
        assert_eq!(
            IpcErrorKind::LimitExceeded {
                limit: 16,
                requested: 4096
            }
            .to_string(),
            "asked for 4096, the limit is 16"
        );
        assert_eq!(
            IpcErrorKind::Io { code: 232 }.to_string(),
            "operating system error 232"
        );
    }

    #[test]
    fn errors_are_copy_and_comparable_so_a_poisoned_channel_can_keep_one() {
        let e = IpcError::closed("ControlChannel::poll");
        let copy = e;
        assert_eq!(e, copy);
        assert!(copy.is_closed());
        // Two errors differing only in context are not equal: the field name is part of
        // the identity, not decoration.
        assert_ne!(e, IpcError::closed("LoopbackTransport::send"));
    }
}
