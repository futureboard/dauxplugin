//! The control-plane transport abstraction: an ordered, reliable byte stream.
//!
//! # Why bytes and not messages
//!
//! Every transport this crate will ever grow — a Windows named pipe, a Unix domain socket,
//! a socket to another machine — delivers bytes, not messages. A `recv` on a pipe returns
//! whatever happened to be in the kernel buffer: half a frame, one frame, or three frames
//! and a bit. Modelling the transport as a message channel would mean each implementation
//! reinventing the reassembly, and the in-process implementation would be the only one that
//! never exercised it — which is exactly the bug factory the loopback exists to prevent.
//!
//! So [`ControlTransport`] moves bytes, [`ControlChannel`](crate::ControlChannel) turns
//! them back into [`ControlMessage`](daux_protocol::ControlMessage)s, and the reassembly
//! code is the same code in every configuration.
//!
//! # The contract
//!
//! An implementation promises all of the following. `ControlChannel` relies on every one of
//! them, and the loopback tests check them directly.
//!
//! * Bytes arrive in the order they were sent, exactly once, with nothing inserted.
//! * Frame boundaries are **not** preserved. One `send` may take several `recv`s to
//!   collect, and one `recv` may return bytes from more than one `send`.
//! * [`ControlTransport::try_recv`] never blocks and never returns `Ok(0)`: a transport
//!   with nothing to give reports [`IpcErrorKind::WouldBlock`].
//! * A `send` that fails wrote **no** bytes. A partially written frame would desynchronise
//!   the peer's reader permanently, so it is never allowed to happen.
//! * After the peer closes, the bytes it wrote beforehand are still delivered; only once
//!   they run out does a receive report [`IpcErrorKind::Closed`].

use crate::error::{IpcErrorKind, IpcResult};

/// An ordered, reliable, bidirectional byte stream to one peer. [main-thread]
///
/// The control plane is not real-time: it is polled or waited on from a dedicated thread,
/// and implementations may allocate. The audio path uses [`DataPlane`](crate::DataPlane)
/// instead, which never does either.
///
/// # What an implementation promises
///
/// [`ControlChannel`](crate::ControlChannel) relies on every one of these, and the loopback
/// tests check them directly.
///
/// * Bytes arrive in the order they were sent, exactly once, with nothing inserted.
/// * Frame boundaries are **not** preserved. One `send` may take several `recv`s to
///   collect, and one `recv` may return bytes from more than one `send`. That is why the
///   transport moves bytes and the channel reassembles them: every operating-system
///   transport behaves this way, so the reassembly path had better be the one the
///   in-process transport exercises too.
/// * [`ControlTransport::try_recv`] never blocks and never returns `Ok(0)`: a transport
///   with nothing to give reports [`IpcErrorKind::WouldBlock`].
/// * A `send` that fails wrote **no** bytes. A partially written frame would desynchronise
///   the peer's reader permanently, so it is never allowed to happen.
/// * After the peer closes, the bytes it wrote beforehand are still delivered; only once
///   they run out does a receive report [`IpcErrorKind::Closed`].
pub trait ControlTransport {
    /// [main-thread] Writes one complete frame.
    ///
    /// `frame` is the whole frame — header and payload — as
    /// [`ControlMessage::encode`](daux_protocol::ControlMessage::encode) produced it. The
    /// transport may split it on the wire; what it may never do is write part of it and
    /// then fail.
    ///
    /// # Errors
    ///
    /// [`IpcErrorKind::Closed`] when the connection is gone, [`IpcErrorKind::Full`] when
    /// the outbound queue has no room (the caller may retry the same frame),
    /// [`IpcErrorKind::InvalidArgument`] for an empty frame, and
    /// [`IpcErrorKind::Io`] when the operating system refuses.
    fn send(&mut self, frame: &[u8]) -> IpcResult<()>;

    /// [main-thread] Appends whatever bytes have arrived to `buf` and returns how many.
    ///
    /// `buf` is **appended to, never cleared**: the caller keeps one buffer across calls
    /// and takes frames off the front of it as they complete. The return value is always
    /// greater than zero.
    ///
    /// This call may wait for the peer. A transport with nothing to wait on — the
    /// in-process [`LoopbackTransport`](crate::LoopbackTransport), for one — behaves
    /// exactly like [`ControlTransport::try_recv`] and says so in its own documentation.
    ///
    /// # Errors
    ///
    /// [`IpcErrorKind::WouldBlock`] when no bytes are available and the transport does not
    /// wait, [`IpcErrorKind::Closed`] when the peer is gone and every byte it wrote has
    /// been delivered, and [`IpcErrorKind::Io`] when the operating system refuses.
    fn recv(&mut self, buf: &mut Vec<u8>) -> IpcResult<usize>;

    /// [main-thread] As [`ControlTransport::recv`], but never waits.
    ///
    /// This is the method [`ControlChannel`](crate::ControlChannel) polls with, so that a
    /// caller driving several connections from one thread is never parked on one of them.
    ///
    /// # Errors
    ///
    /// As [`ControlTransport::recv`]; [`IpcErrorKind::WouldBlock`] is the normal result
    /// when the peer has sent nothing since the last call.
    fn try_recv(&mut self, buf: &mut Vec<u8>) -> IpcResult<usize>;

    /// [main-thread] Pushes any buffered outbound bytes towards the peer.
    ///
    /// The default does nothing, which is correct for a transport that hands each frame
    /// straight to the peer. A buffering transport overrides it.
    ///
    /// # Errors
    ///
    /// As [`ControlTransport::send`].
    fn flush(&mut self) -> IpcResult<()> {
        Ok(())
    }

    /// [any-thread] `true` while the connection can still carry traffic.
    ///
    /// `false` does not mean the receive path is finished: bytes the peer wrote before it
    /// went away are still delivered by [`ControlTransport::recv`] until they run out.
    fn is_open(&self) -> bool;

    /// [main-thread] Closes this end of the connection.
    ///
    /// Idempotent, infallible and safe to call from `Drop`. Any subsequent
    /// [`ControlTransport::send`] reports [`IpcErrorKind::Closed`].
    fn close(&mut self);
}

/// [any-thread] `true` when `result` is the "nothing yet" outcome of a receive.
///
/// Small enough to inline by hand, and worth a name anyway: `Err(e) if e.is_would_block()`
/// appears in every poll loop, and getting it wrong turns an idle connection into a
/// dropped one.
#[inline]
#[must_use]
pub fn is_would_block<T>(result: &IpcResult<T>) -> bool {
    matches!(result, Err(e) if e.kind() == IpcErrorKind::WouldBlock)
}

#[cfg(test)]
mod tests {
    use super::{ControlTransport, is_would_block};
    use crate::error::{IpcError, IpcResult};

    /// A transport that only implements the three required methods, to prove the defaults
    /// are usable and that the trait is object-safe.
    struct Minimal {
        open: bool,
    }

    impl ControlTransport for Minimal {
        fn send(&mut self, frame: &[u8]) -> IpcResult<()> {
            if frame.is_empty() {
                return Err(IpcError::invalid_argument("Minimal::send"));
            }
            Ok(())
        }

        fn recv(&mut self, buf: &mut Vec<u8>) -> IpcResult<usize> {
            self.try_recv(buf)
        }

        fn try_recv(&mut self, _buf: &mut Vec<u8>) -> IpcResult<usize> {
            Err(IpcError::would_block("Minimal::try_recv"))
        }

        fn is_open(&self) -> bool {
            self.open
        }

        fn close(&mut self) {
            self.open = false;
        }
    }

    #[test]
    fn the_trait_is_object_safe_so_a_host_can_hold_boxed_transports() {
        let mut boxed: Box<dyn ControlTransport> = Box::new(Minimal { open: true });
        assert!(boxed.is_open());
        assert!(boxed.send(b"frame").is_ok());
        assert!(boxed.flush().is_ok(), "the default flush succeeds");
        let mut buf = Vec::new();
        assert!(is_would_block(&boxed.try_recv(&mut buf)));
        boxed.close();
        assert!(!boxed.is_open());
    }

    #[test]
    fn is_would_block_only_matches_the_one_kind() {
        let block: IpcResult<usize> = Err(IpcError::would_block("x"));
        let closed: IpcResult<usize> = Err(IpcError::closed("x"));
        let ok: IpcResult<usize> = Ok(3);
        assert!(is_would_block(&block));
        assert!(!is_would_block(&closed));
        assert!(!is_would_block(&ok));
    }
}
