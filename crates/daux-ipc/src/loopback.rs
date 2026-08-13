//! An in-process [`ControlTransport`] built from two lock-free queues.
//!
//! This is not a mock. It is the transport the sandbox message layer, the framing, the
//! crash policy and every test actually run on in v1, and when the platform transports land
//! it is the one thing that will not change. That is why it is written to be *hostile* in
//! the same ways a pipe is:
//!
//! * a receive returns at most [`LoopbackTransport::max_recv_chunk`] bytes, so a frame
//!   really does arrive in pieces;
//! * frame boundaries are not preserved in either direction;
//! * closing one end still lets the other drain what was already written, so a peer that
//!   dies mid-frame produces a *truncated frame*, not a hang;
//! * the outbound queue is bounded, so backpressure is a real, testable outcome rather
//!   than an unbounded `Vec` that quietly eats memory until the machine dies.
//!
//! # Example
//!
//! ```
//! use daux_ipc::{ControlTransport, LoopbackTransport};
//!
//! let (mut host, mut sandbox) = LoopbackTransport::pair();
//! host.send(b"hello")?;
//!
//! let mut buf = Vec::new();
//! assert_eq!(sandbox.try_recv(&mut buf)?, 5);
//! assert_eq!(&buf, b"hello");
//! # Ok::<(), daux_ipc::IpcError>(())
//! ```

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use daux_rt::{Consumer, Producer, SpscRingBuffer};

use crate::error::{IpcError, IpcResult};
use crate::transport::ControlTransport;

/// One end of an in-process control connection. [main-thread]
///
/// Created in pairs by [`LoopbackTransport::pair`]. Each end owns the producing half of one
/// queue and the consuming half of the other, so the two directions are independent and
/// neither side can block the other.
///
/// `Send` but not `Sync`: an endpoint belongs to one thread at a time, exactly like the
/// file descriptor a real transport would wrap.
pub struct LoopbackTransport {
    /// Segments this end writes; the peer consumes them.
    outbound: Producer<Vec<u8>>,
    /// Segments the peer wrote; this end consumes them.
    inbound: Consumer<Vec<u8>>,
    /// The segment currently being handed out, and how much of it has gone.
    pending: Vec<u8>,
    cursor: usize,
    /// Largest number of bytes one receive will hand back.
    max_recv_chunk: usize,
    /// Shared by both ends: `false` once either side has closed the connection.
    open: Arc<AtomicBool>,
}

impl LoopbackTransport {
    /// Segments each direction can hold before [`ControlTransport::send`] reports
    /// backpressure.
    pub const DEFAULT_CAPACITY: usize = 64;

    /// [main-thread] Creates a connected pair with [`LoopbackTransport::DEFAULT_CAPACITY`]
    /// segments of queue in each direction.
    ///
    /// Allocates both queues; call it while setting a connection up, not while using one.
    #[must_use]
    pub fn pair() -> (Self, Self) {
        Self::pair_with_capacity(Self::DEFAULT_CAPACITY)
    }

    /// [main-thread] Creates a connected pair whose queues hold at least `capacity`
    /// segments each.
    ///
    /// `capacity` is rounded up to a power of two and is at least one. A small capacity is
    /// the point when testing backpressure.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is so large that rounding it up to a power of two overflows
    /// `usize`, or if the queue allocation fails.
    #[must_use]
    pub fn pair_with_capacity(capacity: usize) -> (Self, Self) {
        let (a_out, b_in) = SpscRingBuffer::with_capacity::<Vec<u8>>(capacity);
        let (b_out, a_in) = SpscRingBuffer::with_capacity::<Vec<u8>>(capacity);
        let open = Arc::new(AtomicBool::new(true));
        (
            Self::new(a_out, a_in, Arc::clone(&open)),
            Self::new(b_out, b_in, open),
        )
    }

    fn new(outbound: Producer<Vec<u8>>, inbound: Consumer<Vec<u8>>, open: Arc<AtomicBool>) -> Self {
        Self {
            outbound,
            inbound,
            pending: Vec::new(),
            cursor: 0,
            max_recv_chunk: usize::MAX,
            open,
        }
    }

    /// [main-thread] Caps how many bytes a single receive hands back.
    ///
    /// The default is unbounded, which delivers each `send` in one piece. Setting a small
    /// value reproduces what a real pipe does to a large frame, and is how the reassembly
    /// path is tested. A value of zero is raised to one, because a receive that returned
    /// nothing while data was waiting would break the transport contract.
    #[must_use]
    pub fn with_max_recv_chunk(mut self, bytes: usize) -> Self {
        self.max_recv_chunk = if bytes == 0 { 1 } else { bytes };
        self
    }

    /// [any-thread] The current receive chunk cap.
    #[inline]
    #[must_use]
    pub const fn max_recv_chunk(&self) -> usize {
        self.max_recv_chunk
    }

    /// [any-thread] Segments the peer has written that this end has not started reading.
    ///
    /// Diagnostics and tests only: bytes held in a partially delivered segment are not
    /// counted.
    #[inline]
    #[must_use]
    pub fn queued_segments(&self) -> usize {
        self.inbound.len()
    }

    /// [any-thread] Segments this end may have outstanding before sends are refused.
    #[inline]
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.outbound.capacity()
    }

    /// [any-thread] Bytes left in the segment currently being delivered.
    #[inline]
    #[must_use]
    pub fn buffered_bytes(&self) -> usize {
        self.pending.len() - self.cursor
    }

    /// `true` once the peer can no longer produce new bytes.
    fn peer_gone(&self) -> bool {
        !self.open.load(Ordering::Acquire) || self.inbound.is_abandoned()
    }

    /// Moves up to `max_recv_chunk` bytes of the pending segment into `buf`.
    fn drain_pending(&mut self, buf: &mut Vec<u8>) -> usize {
        let end = self
            .cursor
            .saturating_add(self.max_recv_chunk)
            .min(self.pending.len());
        buf.extend_from_slice(&self.pending[self.cursor..end]);
        let taken = end - self.cursor;
        self.cursor = end;
        taken
    }
}

impl ControlTransport for LoopbackTransport {
    fn send(&mut self, frame: &[u8]) -> IpcResult<()> {
        if frame.is_empty() {
            // An empty write would put a zero-length segment in the queue, and a receive
            // that returned `Ok(0)` would break the contract every reader relies on.
            return Err(IpcError::invalid_argument("LoopbackTransport::send"));
        }
        if !self.is_open() {
            return Err(IpcError::closed("LoopbackTransport::send"));
        }
        self.outbound
            .push(frame.to_vec())
            .map_err(|_| IpcError::full("LoopbackTransport::send"))
    }

    /// [main-thread] Identical to [`LoopbackTransport::try_recv`].
    ///
    /// There is no operating system to wait on and no thread to park against: both ends
    /// live in this process, so a caller that wants to wait must do so itself.
    fn recv(&mut self, buf: &mut Vec<u8>) -> IpcResult<usize> {
        self.try_recv(buf)
    }

    fn try_recv(&mut self, buf: &mut Vec<u8>) -> IpcResult<usize> {
        if self.cursor < self.pending.len() {
            return Ok(self.drain_pending(buf));
        }
        if let Some(segment) = self.inbound.pop() {
            self.pending = segment;
            self.cursor = 0;
            return Ok(self.drain_pending(buf));
        }
        // Only now, with nothing left to hand over, does a closed connection matter: a peer
        // that wrote and then died must still have its last bytes delivered, or a
        // half-written frame would look like an idle connection instead of a truncation.
        if self.peer_gone() {
            Err(IpcError::closed("LoopbackTransport::try_recv"))
        } else {
            Err(IpcError::would_block("LoopbackTransport::try_recv"))
        }
    }

    fn is_open(&self) -> bool {
        self.open.load(Ordering::Acquire) && !self.outbound.is_abandoned()
    }

    fn close(&mut self) {
        self.open.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::LoopbackTransport;
    use crate::error::IpcErrorKind;
    use crate::transport::ControlTransport;

    fn recv_all(t: &mut LoopbackTransport) -> Vec<u8> {
        let mut buf = Vec::new();
        while t.try_recv(&mut buf).is_ok() {}
        buf
    }

    #[test]
    fn bytes_cross_in_both_directions_in_order() {
        let (mut host, mut sandbox) = LoopbackTransport::pair();
        host.send(b"one").unwrap();
        host.send(b"two").unwrap();
        sandbox.send(b"back").unwrap();
        assert_eq!(recv_all(&mut sandbox), b"onetwo");
        assert_eq!(recv_all(&mut host), b"back");
    }

    #[test]
    fn a_receive_never_returns_zero_and_appends_rather_than_clears() {
        let (mut host, mut sandbox) = LoopbackTransport::pair();
        host.send(b"abc").unwrap();
        let mut buf = b"existing:".to_vec();
        let n = sandbox.try_recv(&mut buf).unwrap();
        assert_eq!(n, 3);
        assert_eq!(buf, b"existing:abc");
        assert_eq!(
            sandbox.try_recv(&mut buf).unwrap_err().kind(),
            IpcErrorKind::WouldBlock
        );
        assert_eq!(buf, b"existing:abc", "a failed receive must not touch buf");
    }

    #[test]
    fn a_chunk_cap_splits_one_send_across_several_receives() {
        let (mut host, sandbox) = LoopbackTransport::pair();
        let mut sandbox = sandbox.with_max_recv_chunk(2);
        assert_eq!(sandbox.max_recv_chunk(), 2);
        host.send(b"abcdefg").unwrap();

        let mut buf = Vec::new();
        assert_eq!(sandbox.try_recv(&mut buf).unwrap(), 2);
        assert_eq!(sandbox.buffered_bytes(), 5);
        assert_eq!(sandbox.try_recv(&mut buf).unwrap(), 2);
        assert_eq!(sandbox.try_recv(&mut buf).unwrap(), 2);
        assert_eq!(sandbox.try_recv(&mut buf).unwrap(), 1);
        assert_eq!(buf, b"abcdefg");
        assert_eq!(sandbox.buffered_bytes(), 0);
        assert_eq!(
            sandbox.try_recv(&mut buf).unwrap_err().kind(),
            IpcErrorKind::WouldBlock
        );
    }

    #[test]
    fn a_zero_chunk_cap_is_raised_to_one_rather_than_stalling_the_reader() {
        let (mut host, sandbox) = LoopbackTransport::pair();
        let mut sandbox = sandbox.with_max_recv_chunk(0);
        assert_eq!(sandbox.max_recv_chunk(), 1);
        host.send(b"xy").unwrap();
        let mut buf = Vec::new();
        assert_eq!(sandbox.try_recv(&mut buf).unwrap(), 1);
        assert_eq!(sandbox.try_recv(&mut buf).unwrap(), 1);
        assert_eq!(buf, b"xy");
    }

    #[test]
    fn one_receive_never_mixes_two_sends_but_the_reader_still_gets_both() {
        let (mut host, mut sandbox) = LoopbackTransport::pair();
        host.send(b"first").unwrap();
        host.send(b"second").unwrap();
        let mut buf = Vec::new();
        assert_eq!(sandbox.try_recv(&mut buf).unwrap(), 5);
        assert_eq!(sandbox.queued_segments(), 1);
        assert_eq!(sandbox.try_recv(&mut buf).unwrap(), 6);
        assert_eq!(buf, b"firstsecond");
    }

    #[test]
    fn an_empty_send_is_refused_because_it_would_break_the_receive_contract() {
        let (mut host, mut sandbox) = LoopbackTransport::pair();
        assert_eq!(
            host.send(b"").unwrap_err().kind(),
            IpcErrorKind::InvalidArgument
        );
        let mut buf = Vec::new();
        assert_eq!(
            sandbox.try_recv(&mut buf).unwrap_err().kind(),
            IpcErrorKind::WouldBlock,
            "the refused send must not have queued anything"
        );
    }

    #[test]
    fn a_full_queue_reports_backpressure_and_loses_nothing() {
        let (mut host, mut sandbox) = LoopbackTransport::pair_with_capacity(2);
        assert_eq!(host.capacity(), 2);
        host.send(b"a").unwrap();
        host.send(b"b").unwrap();
        assert_eq!(host.send(b"c").unwrap_err().kind(), IpcErrorKind::Full);
        assert!(
            host.is_open(),
            "backpressure must not tear the connection down"
        );

        // Draining one segment makes room again, and nothing was dropped or duplicated.
        let mut buf = Vec::new();
        sandbox.try_recv(&mut buf).unwrap();
        host.send(b"c").unwrap();
        assert_eq!(recv_all(&mut sandbox), b"bc");
        assert_eq!(buf, b"a");
    }

    #[test]
    fn closing_one_end_stops_sends_on_both() {
        let (mut host, mut sandbox) = LoopbackTransport::pair();
        host.close();
        assert!(!host.is_open());
        assert!(!sandbox.is_open());
        assert_eq!(host.send(b"x").unwrap_err().kind(), IpcErrorKind::Closed);
        assert_eq!(sandbox.send(b"x").unwrap_err().kind(), IpcErrorKind::Closed);
        // Closing twice is not an error.
        host.close();
        assert!(!host.is_open());
    }

    /// The behaviour the whole crash story rests on: a peer that dies mid-write leaves
    /// bytes behind, and the reader must see them *and then* see the close, so that a
    /// half-written frame is reported as a truncation instead of looking like an idle link.
    #[test]
    fn bytes_written_before_a_close_are_still_delivered_and_then_the_close_is_reported() {
        let (mut host, mut sandbox) = LoopbackTransport::pair();
        host.send(b"half a frame").unwrap();
        host.close();

        let mut buf = Vec::new();
        assert_eq!(sandbox.try_recv(&mut buf).unwrap(), 12);
        assert_eq!(buf, b"half a frame");
        assert_eq!(
            sandbox.try_recv(&mut buf).unwrap_err().kind(),
            IpcErrorKind::Closed
        );
        // And it keeps saying so rather than reverting to WouldBlock.
        assert_eq!(
            sandbox.try_recv(&mut buf).unwrap_err().kind(),
            IpcErrorKind::Closed
        );
    }

    #[test]
    fn dropping_one_end_closes_the_other_without_an_explicit_close() {
        let (host, mut sandbox) = LoopbackTransport::pair();
        drop(host);
        assert!(!sandbox.is_open());
        let mut buf = Vec::new();
        assert_eq!(
            sandbox.try_recv(&mut buf).unwrap_err().kind(),
            IpcErrorKind::Closed
        );
        assert_eq!(sandbox.send(b"x").unwrap_err().kind(), IpcErrorKind::Closed);
    }

    #[test]
    fn a_dropped_reader_leaves_the_writer_able_to_report_it() {
        let (mut host, sandbox) = LoopbackTransport::pair();
        host.send(b"queued before the drop").unwrap();
        drop(sandbox);
        assert!(!host.is_open());
        assert_eq!(host.send(b"x").unwrap_err().kind(), IpcErrorKind::Closed);
    }

    #[test]
    fn recv_and_try_recv_agree_because_there_is_nothing_to_wait_on() {
        let (mut host, mut sandbox) = LoopbackTransport::pair();
        host.send(b"data").unwrap();
        let mut buf = Vec::new();
        assert_eq!(sandbox.recv(&mut buf).unwrap(), 4);
        assert_eq!(
            sandbox.recv(&mut buf).unwrap_err().kind(),
            IpcErrorKind::WouldBlock
        );
        assert!(sandbox.flush().is_ok());
    }

    #[test]
    fn an_endpoint_can_be_moved_to_another_thread() {
        let (mut host, mut sandbox) = LoopbackTransport::pair();
        let worker = std::thread::spawn(move || {
            let mut buf = Vec::new();
            // Spin until the segment shows up; the queue is lock-free, not blocking.
            while sandbox.try_recv(&mut buf).is_err() {
                std::thread::yield_now();
            }
            buf
        });
        host.send(b"across threads").unwrap();
        assert_eq!(worker.join().unwrap(), b"across threads");
    }
}
