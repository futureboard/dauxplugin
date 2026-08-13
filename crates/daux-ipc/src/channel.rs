//! Message framing over any [`ControlTransport`].
//!
//! [`ControlChannel`] is the only place in the sandbox stack that turns a byte stream back
//! into messages, so it is the only place that has to get reassembly right. Everything
//! below it moves bytes; everything above it sees whole
//! [`ControlMessage`]s.
//!
//! # Reading is a security boundary
//!
//! The bytes come from another process. The reader therefore:
//!
//! * learns the frame length from [`peek_frame_len`] — which validates the magic, the
//!   framing version and the declared length against [`ProtocolLimits`] — **before**
//!   reserving anything for it, so a hostile four-gigabyte length prefix costs twenty bytes
//!   and an error;
//! * consumes the frame's bytes before decoding its payload, so a payload this build does
//!   not understand leaves the stream on a frame boundary and the next message still
//!   decodes;
//! * treats an unrecoverable framing failure as terminal: the channel is poisoned, the
//!   transport is closed, and every later call returns the same error rather than trying to
//!   resynchronise on bytes it cannot interpret;
//! * reports a peer that vanished mid-frame as a truncation, naming how many bytes were
//!   still owed, instead of waiting for a process that no longer exists.
//!
//! # Example
//!
//! ```
//! use daux_ipc::{ControlChannel, LoopbackTransport};
//! use daux_protocol::{ControlMessage, InstanceId, RestartFlags};
//!
//! let (host, sandbox) = LoopbackTransport::pair();
//! let mut host = ControlChannel::new(host);
//! let mut sandbox = ControlChannel::new(sandbox);
//!
//! let message = ControlMessage::RequestRestart {
//!     instance: InstanceId(1),
//!     flags: RestartFlags::LATENCY,
//! };
//! sandbox.send(&message)?;
//!
//! assert_eq!(host.poll()?, Some(message));
//! assert_eq!(host.poll()?, None); // nothing further has arrived
//! # Ok::<(), daux_ipc::IpcError>(())
//! ```

use daux_protocol::{
    ControlMessage, FRAME_HEADER_LEN, FrameFlags, ProtocolError, ProtocolErrorKind, ProtocolLimits,
    peek_frame_len,
};

use crate::error::{IpcError, IpcResult};
use crate::transport::ControlTransport;

/// Framed [`ControlMessage`] traffic over a byte-stream transport. [main-thread]
///
/// Owns the reassembly buffer and the encode buffer, both of which are reused across calls,
/// so a steady stream of small messages settles into zero allocations per message once the
/// buffers have grown.
///
/// Reading is a security boundary — the bytes come from another process, which may be
/// compromised — so an over-long, corrupt or foreign frame is refused before anything is
/// reserved for it. [`ControlChannel::poll`] documents each case and what it does to the
/// connection.
pub struct ControlChannel<T> {
    transport: T,
    limits: ProtocolLimits,
    /// Bytes received and not yet consumed by a decoded frame.
    inbox: Vec<u8>,
    /// Scratch for encoding; reused so that sending does not allocate every time.
    outbox: Vec<u8>,
    /// Set by the first unrecoverable failure and returned by every call afterwards.
    poison: Option<IpcError>,
}

impl<T: ControlTransport> ControlChannel<T> {
    /// [main-thread] Wraps `transport` with the default [`ProtocolLimits`].
    #[must_use]
    pub fn new(transport: T) -> Self {
        Self::with_limits(transport, ProtocolLimits::new())
    }

    /// [main-thread] Wraps `transport` with explicit limits.
    ///
    /// The limits apply to both directions: a message this side cannot encode within them
    /// is refused before it reaches the transport, so a peer is never handed a frame it
    /// would be obliged to reject.
    #[must_use]
    pub fn with_limits(transport: T, limits: ProtocolLimits) -> Self {
        Self {
            transport,
            limits,
            inbox: Vec::new(),
            outbox: Vec::new(),
            poison: None,
        }
    }

    /// [any-thread] The limits applied to both directions.
    #[inline]
    #[must_use]
    pub const fn limits(&self) -> &ProtocolLimits {
        &self.limits
    }

    /// [any-thread] The transport underneath.
    #[inline]
    #[must_use]
    pub const fn transport(&self) -> &T {
        &self.transport
    }

    /// [any-thread] The transport underneath, mutably.
    ///
    /// Writing raw bytes through it desynchronises the reader on the other side; this
    /// exists for configuration and for tests that need to inject malformed traffic.
    #[inline]
    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    /// [main-thread] Unwraps the channel, discarding any partially received frame.
    #[must_use]
    pub fn into_transport(self) -> T {
        self.transport
    }

    /// [any-thread] Bytes received but not yet consumed by a complete frame.
    ///
    /// Bounded by [`ProtocolLimits::max_frame_bytes`] plus whatever one receive hands over,
    /// because the reader stops asking for more as soon as a frame is complete.
    #[inline]
    #[must_use]
    pub fn buffered_bytes(&self) -> usize {
        self.inbox.len()
    }

    /// [any-thread] `true` while the channel can still be used.
    ///
    /// `false` once the transport has closed or an unrecoverable framing failure has
    /// poisoned the channel.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.poison.is_none() && self.transport.is_open()
    }

    /// [any-thread] The failure that poisoned the channel, if one has.
    #[inline]
    #[must_use]
    pub const fn poison(&self) -> Option<IpcError> {
        self.poison
    }

    /// [main-thread] Closes the channel and the transport underneath.
    pub fn close(&mut self) {
        self.transport.close();
    }

    /// [main-thread] Encodes and sends `message`.
    ///
    /// On failure nothing is written: encoding happens into an internal buffer and the
    /// transport is only touched once a complete frame exists.
    ///
    /// # Errors
    ///
    /// [`IpcErrorKind::LimitExceeded`](crate::IpcErrorKind::LimitExceeded) when the message
    /// does not fit the channel's limits, plus anything
    /// [`ControlTransport::send`] reports. A poisoned channel returns its original failure.
    pub fn send(&mut self, message: &ControlMessage) -> IpcResult<()> {
        self.send_with_flags(message, FrameFlags::NONE)
    }

    /// [main-thread] Encodes and sends `message` with explicit frame flags.
    ///
    /// # Errors
    ///
    /// As [`ControlChannel::send`].
    pub fn send_with_flags(
        &mut self,
        message: &ControlMessage,
        flags: FrameFlags,
    ) -> IpcResult<()> {
        if let Some(e) = self.poison {
            return Err(e);
        }
        self.outbox.clear();
        // `encode_into` truncates the buffer back on failure, so a rejected message never
        // leaves a partial frame behind to be sent later.
        message
            .encode_into(&mut self.outbox, flags, &self.limits)
            .map_err(encode_error)?;
        self.transport.send(&self.outbox)
    }

    /// [main-thread] Returns the next complete message, or `None` when none has arrived.
    ///
    /// Pulls from the transport until either a whole frame is buffered or the transport
    /// says it has nothing more, so a caller polls in a loop and stops on `Ok(None)`.
    ///
    /// # Errors
    ///
    /// [`IpcErrorKind::Closed`](crate::IpcErrorKind::Closed) when the peer is gone and
    /// nothing is left to read, and
    /// [`IpcErrorKind::Protocol`](crate::IpcErrorKind::Protocol) for every malformed-input
    /// case:
    ///
    /// * a truncated frame — the peer closed mid-frame — naming the bytes still owed;
    /// * a frame larger than [`ProtocolLimits::max_frame_bytes`], refused before it is
    ///   buffered;
    /// * a bad magic, an unsupported framing version or a failed payload checksum, all of
    ///   which poison the channel;
    /// * a payload this build cannot decode, which does **not** poison the channel: the
    ///   frame has already been consumed, so the next call resumes on a frame boundary.
    pub fn poll(&mut self) -> IpcResult<Option<ControlMessage>> {
        if let Some(e) = self.poison {
            return Err(e);
        }
        loop {
            if let Some(message) = self.take_buffered_frame()? {
                return Ok(Some(message));
            }
            match self.transport.try_recv(&mut self.inbox) {
                Ok(0) => {
                    // The transport broke its own contract. Treating this as success would
                    // spin this loop forever.
                    return Err(self.poisoned(IpcError::invalid_state("ControlChannel::poll")));
                }
                Ok(_) => {}
                Err(e) if e.is_would_block() => return Ok(None),
                Err(e) if e.is_closed() => return Err(self.closed_mid_frame(e)),
                Err(e) => return Err(self.poisoned(e)),
            }
        }
    }

    /// Decodes and removes the frame at the front of the inbox, if a whole one is there.
    fn take_buffered_frame(&mut self) -> IpcResult<Option<ControlMessage>> {
        // `peek_frame_len` validates the magic, the version and the declared length against
        // the limits, so an absurd length prefix is refused here — before a single byte is
        // reserved on the strength of it.
        let total = match peek_frame_len(&self.inbox, &self.limits) {
            Ok(Some(total)) => total,
            Ok(None) => return Ok(None),
            Err(e) => return Err(self.poisoned(IpcError::protocol(e))),
        };
        if self.inbox.len() < total {
            return Ok(None);
        }
        let decoded = ControlMessage::decode(&self.inbox[..total], &self.limits);
        // Consume the frame either way: its length was validated, so the stream is left on
        // a frame boundary and a payload this build cannot read costs one message, not the
        // connection.
        self.inbox.drain(..total);
        match decoded {
            Ok(message) => Ok(Some(message)),
            Err(e) if e.is_fatal_to_stream() => Err(self.poisoned(IpcError::protocol(e))),
            Err(e) => Err(IpcError::protocol(e)),
        }
    }

    /// Turns "the peer went away" into the more useful "it went away owing us `n` bytes".
    fn closed_mid_frame(&mut self, closed: IpcError) -> IpcError {
        let available = self.inbox.len();
        if available == 0 {
            return self.poisoned(closed);
        }
        let needed = match peek_frame_len(&self.inbox, &self.limits) {
            Ok(Some(total)) => total,
            // The header itself never arrived, so all we know is that a header was owed.
            Ok(None) => FRAME_HEADER_LEN,
            Err(e) => return self.poisoned(IpcError::protocol(e)),
        };
        self.poisoned(IpcError::protocol(ProtocolError::truncated(
            "ControlChannel::frame",
            needed,
            available,
        )))
    }

    /// Records the first unrecoverable failure, closes the transport and returns it.
    fn poisoned(&mut self, error: IpcError) -> IpcError {
        if self.poison.is_none() {
            self.poison = Some(error);
            self.transport.close();
        }
        error
    }
}

/// Translates an encoding failure into an IPC failure.
///
/// A message too large for the limits is a fault in *this* process, not a broken link:
/// [`ProtocolError::is_fatal_to_stream`] answers the reader's question ("can I find the
/// next frame boundary?"), which has nothing to do with a frame that was never written. So
/// an over-long message becomes a plain, recoverable
/// [`IpcErrorKind::LimitExceeded`](crate::IpcErrorKind::LimitExceeded), keeping the field
/// name `daux-protocol` attached to it.
fn encode_error(error: ProtocolError) -> IpcError {
    match error.kind() {
        ProtocolErrorKind::LimitExceeded { limit, requested } => {
            IpcError::limit(error.context(), limit, requested)
        }
        _ => IpcError::protocol(error),
    }
}

#[cfg(test)]
mod tests {
    use super::ControlChannel;
    use crate::error::{IpcError, IpcErrorKind};
    use crate::loopback::LoopbackTransport;
    use crate::transport::ControlTransport;
    use daux_protocol::{
        ControlMessage, Diagnostics, ErrorMessage, FRAME_HEADER_LEN, FeatureFlags, FrameFlags,
        GesturePhase, Handshake, InstanceId, PeerRole, ProtocolErrorKind, ProtocolLimits,
        RequestId, RestartFlags, Tail, crc32,
    };

    /// A representative message of every shape the codec has: fixed fields, strings, a
    /// blob, an enum with a payload and a notification with no request id.
    fn sample_messages() -> Vec<ControlMessage> {
        vec![
            ControlMessage::Handshake(Handshake {
                role: PeerRole::Sandbox,
                protocol_version: 1,
                abi_version_major: 1,
                abi_version_minor: 0,
                features: FeatureFlags::SHARED_MEMORY_AUDIO.with(FeatureFlags::MIDI2),
                process_id: 4242,
                peer_version: Default::default(),
                peer_name: "daux-sandbox".to_owned(),
            }),
            ControlMessage::CreateInstance {
                request: RequestId(1),
                instance: InstanceId(1),
                plugin_id: "studio.futureboard.gain".to_owned(),
                bundle_path: "C:/plugins/Gain.axt".to_owned(),
            },
            ControlMessage::StateBlob {
                request: RequestId(6),
                instance: InstanceId(1),
                bytes: vec![0xDA, 0x00, 0xFF, 0x10],
            },
            ControlMessage::Error(ErrorMessage {
                request: RequestId(9),
                instance: InstanceId(3),
                status: -5,
                detail: "instance is not activated".to_owned(),
            }),
            ControlMessage::Diagnostics(Diagnostics {
                instance: InstanceId::NONE,
                level: 0,
                text: "scan took 4.2 s".to_owned(),
            }),
            ControlMessage::ReportTail {
                instance: InstanceId(1),
                tail: Tail::Samples(48_000),
            },
            ControlMessage::ParamGesture {
                instance: InstanceId(1),
                param_id: 7,
                phase: GesturePhase::Begin,
                value: -6.0,
            },
            ControlMessage::RequestRestart {
                instance: InstanceId(1),
                flags: RestartFlags::PROCESS.with(RestartFlags::LATENCY),
            },
        ]
    }

    fn channel_pair() -> (
        ControlChannel<LoopbackTransport>,
        ControlChannel<LoopbackTransport>,
    ) {
        let (a, b) = LoopbackTransport::pair();
        (ControlChannel::new(a), ControlChannel::new(b))
    }

    #[test]
    fn every_sample_message_round_trips_through_the_loopback() {
        let (mut host, mut sandbox) = channel_pair();
        for message in sample_messages() {
            sandbox.send(&message).unwrap();
            assert_eq!(host.poll().unwrap(), Some(message.clone()));
            assert_eq!(host.poll().unwrap(), None);
            // And the same message the other way, to prove the directions are independent.
            host.send(&message).unwrap();
            assert_eq!(sandbox.poll().unwrap(), Some(message));
        }
        assert!(host.is_open() && sandbox.is_open());
        assert_eq!(host.buffered_bytes(), 0);
    }

    #[test]
    fn frame_flags_survive_the_round_trip_without_changing_the_message() {
        let (mut host, mut sandbox) = channel_pair();
        let message = ControlMessage::Ack {
            request: RequestId(4),
            instance: InstanceId(2),
        };
        sandbox
            .send_with_flags(&message, FrameFlags::RESPONSE)
            .unwrap();
        assert_eq!(host.poll().unwrap(), Some(message));
    }

    /// The reassembly case a byte-stream transport forces on every reader: the peer's first
    /// write carried only part of the frame.
    #[test]
    fn a_frame_split_across_two_sends_is_reassembled() {
        let (raw_host, raw_sandbox) = LoopbackTransport::pair();
        let mut host = ControlChannel::new(raw_host);
        let mut sandbox = raw_sandbox;

        let message = ControlMessage::CreateInstance {
            request: RequestId(1),
            instance: InstanceId(1),
            plugin_id: "studio.futureboard.gain".to_owned(),
            bundle_path: "C:/plugins/Gain.axt".to_owned(),
        };
        let frame = message.encode(&ProtocolLimits::new()).unwrap();
        let split = FRAME_HEADER_LEN + 3;

        sandbox.send(&frame[..split]).unwrap();
        assert_eq!(host.poll().unwrap(), None, "half a frame is not a message");
        assert_eq!(host.buffered_bytes(), split);

        sandbox.send(&frame[split..]).unwrap();
        assert_eq!(host.poll().unwrap(), Some(message));
        assert_eq!(host.buffered_bytes(), 0);
    }

    /// The same frame arriving one byte at a time, which is what a small pipe buffer does.
    #[test]
    fn a_frame_delivered_one_byte_per_receive_still_decodes() {
        let (raw_host, mut sandbox) = LoopbackTransport::pair();
        let mut host = ControlChannel::new(raw_host.with_max_recv_chunk(1));
        let message = ControlMessage::ReportLatency {
            instance: InstanceId(1),
            samples: 64,
        };
        let frame = message.encode(&ProtocolLimits::new()).unwrap();
        // One send, but the reader only ever gets a byte at a time out of it.
        sandbox.send(&frame).unwrap();
        assert_eq!(host.poll().unwrap(), Some(message));
    }

    #[test]
    fn two_frames_in_one_write_are_returned_one_at_a_time() {
        let (raw_host, mut sandbox) = LoopbackTransport::pair();
        let mut host = ControlChannel::new(raw_host);
        let limits = ProtocolLimits::new();
        let first = ControlMessage::Activate {
            request: RequestId(1),
            instance: InstanceId(1),
        };
        let second = ControlMessage::Deactivate {
            request: RequestId(2),
            instance: InstanceId(1),
        };
        let mut both = first.encode(&limits).unwrap();
        second
            .encode_into(&mut both, FrameFlags::NONE, &limits)
            .unwrap();
        sandbox.send(&both).unwrap();

        assert_eq!(host.poll().unwrap(), Some(first));
        assert_eq!(host.poll().unwrap(), Some(second));
        assert_eq!(host.poll().unwrap(), None);
    }

    #[test]
    fn an_idle_channel_reports_nothing_rather_than_failing() {
        let (mut host, _sandbox) = channel_pair();
        for _ in 0..3 {
            assert_eq!(host.poll().unwrap(), None);
        }
        assert!(host.is_open());
        assert_eq!(host.poison(), None);
    }

    /// A four-gigabyte length prefix must cost twenty bytes and an error, never an
    /// allocation.
    #[test]
    fn an_oversized_frame_is_refused_before_it_is_buffered() {
        let (raw_host, mut sandbox) = LoopbackTransport::pair();
        let limits = ProtocolLimits::new().with_max_frame_bytes(1024);
        let mut host = ControlChannel::with_limits(raw_host, limits);

        // A structurally valid header that claims a colossal payload.
        let mut header = ControlMessage::Ack {
            request: RequestId(1),
            instance: InstanceId(1),
        }
        .encode(&ProtocolLimits::new())
        .unwrap()[..FRAME_HEADER_LEN]
            .to_vec();
        header[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        sandbox.send(&header).unwrap();

        let err = host.poll().unwrap_err();
        assert!(matches!(
            err.protocol_error().map(|e| e.kind()),
            Some(ProtocolErrorKind::LimitExceeded { limit: 1024, .. })
        ));
        assert!(err.is_fatal());
        assert!(
            host.buffered_bytes() <= FRAME_HEADER_LEN,
            "nothing was reserved for the claimed payload"
        );
        // Terminal: the channel keeps reporting the same failure and shuts the link down.
        assert!(!host.is_open());
        assert_eq!(host.poll().unwrap_err(), err);
        assert_eq!(host.poison(), Some(err));
    }

    #[test]
    fn a_message_too_large_for_the_limits_is_refused_before_it_reaches_the_wire() {
        let (raw_host, mut raw_sandbox) = LoopbackTransport::pair();
        let mut host =
            ControlChannel::with_limits(raw_host, ProtocolLimits::new().with_max_frame_bytes(256));
        let err = host
            .send(&ControlMessage::StateBlob {
                request: RequestId(1),
                instance: InstanceId(1),
                bytes: vec![0u8; 4096],
            })
            .unwrap_err();
        assert!(
            matches!(err.kind(), IpcErrorKind::LimitExceeded { limit: 256, .. }),
            "got {err:?}"
        );
        assert_eq!(err.context(), "frame.payload_len");
        assert!(!err.is_fatal(), "an unsendable message is not a dead link");
        assert!(host.is_open());

        let mut peer_bytes = Vec::new();
        assert!(
            raw_sandbox.try_recv(&mut peer_bytes).is_err(),
            "no partial frame may reach the peer"
        );
        // And the channel still works afterwards.
        let ok = ControlMessage::Ack {
            request: RequestId(1),
            instance: InstanceId(1),
        };
        host.send(&ok).unwrap();
        let mut sandbox = ControlChannel::new(raw_sandbox);
        assert_eq!(sandbox.poll().unwrap(), Some(ok));
    }

    /// The crash case from `docs/architecture/sandboxing.md`: the peer died halfway through
    /// a write. The reader must say so, with numbers, rather than wait for a dead process.
    #[test]
    fn a_peer_that_dies_mid_frame_reports_a_truncation_rather_than_hanging() {
        let (raw_host, mut sandbox) = LoopbackTransport::pair();
        let mut host = ControlChannel::new(raw_host);
        let message = ControlMessage::LoadState {
            request: RequestId(1),
            instance: InstanceId(1),
            bytes: vec![7u8; 64],
        };
        let frame = message.encode(&ProtocolLimits::new()).unwrap();
        let sent = FRAME_HEADER_LEN + 10;
        sandbox.send(&frame[..sent]).unwrap();
        sandbox.close();

        let err = host.poll().unwrap_err();
        assert_eq!(
            err.protocol_error().map(|e| e.kind()),
            Some(ProtocolErrorKind::Truncated {
                needed: frame.len(),
                available: sent,
            })
        );
        assert_eq!(host.poll().unwrap_err(), err, "and it stays reported");
        assert!(!host.is_open());
    }

    /// The same, but the peer died before even the header was complete: the reader knows
    /// only that a header was owed.
    #[test]
    fn a_peer_that_dies_inside_the_header_reports_the_header_as_the_debt() {
        let (raw_host, mut sandbox) = LoopbackTransport::pair();
        let mut host = ControlChannel::new(raw_host);
        sandbox.send(&[0u8; 6]).unwrap();
        sandbox.close();
        let err = host.poll().unwrap_err();
        assert_eq!(
            err.protocol_error().map(|e| e.kind()),
            Some(ProtocolErrorKind::Truncated {
                needed: FRAME_HEADER_LEN,
                available: 6,
            })
        );
    }

    #[test]
    fn a_closed_peer_with_nothing_pending_is_a_plain_close() {
        let (mut host, sandbox) = channel_pair();
        drop(sandbox);
        let err = host.poll().unwrap_err();
        assert_eq!(err.kind(), IpcErrorKind::Closed);
        assert!(err.is_fatal());
        assert!(!host.is_open());
    }

    #[test]
    fn a_corrupt_payload_fails_the_checksum_and_poisons_the_channel() {
        let (raw_host, mut sandbox) = LoopbackTransport::pair();
        let mut host = ControlChannel::new(raw_host);
        let mut frame = ControlMessage::CreateInstance {
            request: RequestId(1),
            instance: InstanceId(1),
            plugin_id: "studio.futureboard.gain".to_owned(),
            bundle_path: "C:/plugins/Gain.axt".to_owned(),
        }
        .encode(&ProtocolLimits::new())
        .unwrap();
        let last = frame.len() - 1;
        frame[last] ^= 0xFF;
        sandbox.send(&frame).unwrap();

        let err = host.poll().unwrap_err();
        assert!(matches!(
            err.protocol_error().map(|e| e.kind()),
            Some(ProtocolErrorKind::ChecksumMismatch { .. })
        ));
        assert!(err.is_fatal());
        assert!(!host.is_open());
    }

    #[test]
    fn a_foreign_stream_is_rejected_on_the_magic_and_never_resynchronised() {
        let (raw_host, mut sandbox) = LoopbackTransport::pair();
        let mut host = ControlChannel::new(raw_host);
        sandbox.send(b"GET / HTTP/1.1\r\nHost: x\r\n").unwrap();
        let err = host.poll().unwrap_err();
        assert_eq!(
            err.protocol_error().map(|e| e.kind()),
            Some(ProtocolErrorKind::BadMagic)
        );
        assert!(!host.is_open());
    }

    /// A newer peer may legitimately send a message this build has never heard of. That
    /// costs one message, not the connection — and the frame after it must still decode.
    #[test]
    fn an_unknown_message_kind_is_skipped_and_the_stream_carries_on() {
        let (raw_host, mut sandbox) = LoopbackTransport::pair();
        let mut host = ControlChannel::new(raw_host);
        let limits = ProtocolLimits::new();

        let mut unknown = ControlMessage::Activate {
            request: RequestId(1),
            instance: InstanceId(1),
        }
        .encode(&limits)
        .unwrap();
        unknown[6..8].copy_from_slice(&999u16.to_le_bytes());
        let known = ControlMessage::Deactivate {
            request: RequestId(2),
            instance: InstanceId(1),
        };
        sandbox.send(&unknown).unwrap();
        sandbox.send(&known.encode(&limits).unwrap()).unwrap();

        let err = host.poll().unwrap_err();
        assert_eq!(
            err.protocol_error().map(|e| e.kind()),
            Some(ProtocolErrorKind::UnknownMessage { kind: 999 })
        );
        assert!(!err.is_fatal());
        assert!(host.is_open(), "a skippable frame must not poison anything");
        assert_eq!(host.poll().unwrap(), Some(known));
    }

    /// A payload whose *contents* are impossible — here a sample rate of zero — is caught
    /// by the decoder. The CRC is recomputed so the frame is otherwise beyond reproach.
    #[test]
    fn a_semantically_impossible_payload_is_reported_without_killing_the_link() {
        let (raw_host, mut sandbox) = LoopbackTransport::pair();
        let mut host = ControlChannel::new(raw_host);
        let limits = ProtocolLimits::new();
        let mut frame = ControlMessage::SetConfig {
            request: RequestId(1),
            instance: InstanceId(1),
            config: daux_protocol::ProcessConfigMsg {
                sample_rate: 48_000.0,
                min_block_size: 1,
                max_block_size: 512,
                sample_format: 1,
                process_mode: 0,
            },
        }
        .encode(&limits)
        .unwrap();
        // sample_rate sits right after the two u64 ids at the start of the payload.
        let at = FRAME_HEADER_LEN + 16;
        frame[at..at + 8].copy_from_slice(&0.0f64.to_le_bytes());
        let crc = crc32(&frame[FRAME_HEADER_LEN..]);
        frame[16..20].copy_from_slice(&crc.to_le_bytes());
        sandbox.send(&frame).unwrap();

        let err = host.poll().unwrap_err();
        assert_eq!(
            err.protocol_error().map(|e| e.kind()),
            Some(ProtocolErrorKind::InvalidValue)
        );
        assert!(host.is_open());
        assert_eq!(host.buffered_bytes(), 0, "the bad frame was consumed whole");
    }

    #[test]
    fn a_poisoned_channel_refuses_to_send_as_well_as_to_poll() {
        let (raw_host, mut sandbox) = LoopbackTransport::pair();
        let mut host = ControlChannel::new(raw_host);
        sandbox.send(b"not a daux frame at all!").unwrap();
        let err = host.poll().unwrap_err();
        assert_eq!(
            host.send(&ControlMessage::Ack {
                request: RequestId(1),
                instance: InstanceId(1),
            })
            .unwrap_err(),
            err
        );
    }

    #[test]
    fn closing_a_channel_closes_the_transport_under_it() {
        let (mut host, mut sandbox) = channel_pair();
        host.close();
        assert!(!host.is_open());
        assert!(!sandbox.is_open());
        assert_eq!(
            sandbox
                .send(&ControlMessage::Ack {
                    request: RequestId(1),
                    instance: InstanceId(1),
                })
                .unwrap_err()
                .kind(),
            IpcErrorKind::Closed
        );
    }

    /// A transport that returns `Ok(0)` breaks the contract the poll loop relies on. It
    /// must be caught, not spun on.
    #[test]
    fn a_transport_that_returns_zero_bytes_is_caught_instead_of_looping_forever() {
        struct Liar;
        impl ControlTransport for Liar {
            fn send(&mut self, _frame: &[u8]) -> Result<(), IpcError> {
                Ok(())
            }
            fn recv(&mut self, buf: &mut Vec<u8>) -> Result<usize, IpcError> {
                self.try_recv(buf)
            }
            fn try_recv(&mut self, _buf: &mut Vec<u8>) -> Result<usize, IpcError> {
                Ok(0)
            }
            fn is_open(&self) -> bool {
                true
            }
            fn close(&mut self) {}
        }
        let mut channel = ControlChannel::new(Liar);
        assert_eq!(
            channel.poll().unwrap_err().kind(),
            IpcErrorKind::InvalidState
        );
        assert!(channel.poison().is_some());
    }

    #[test]
    fn a_channel_can_be_unwrapped_back_into_its_transport() {
        let (host, mut sandbox) = LoopbackTransport::pair();
        let channel = ControlChannel::new(host);
        assert_eq!(channel.limits().max_frame_bytes, 16 * 1024 * 1024);
        assert!(channel.transport().is_open());
        let mut raw = channel.into_transport();
        raw.send(b"raw bytes").unwrap();
        let mut buf = Vec::new();
        sandbox.try_recv(&mut buf).unwrap();
        assert_eq!(buf, b"raw bytes");
    }

    #[test]
    fn a_long_conversation_does_not_grow_the_reassembly_buffer() {
        let (mut host, mut sandbox) = channel_pair();
        for i in 0..200u64 {
            let message = ControlMessage::ReportLatency {
                instance: InstanceId(i),
                samples: i as u32,
            };
            sandbox.send(&message).unwrap();
            assert_eq!(host.poll().unwrap(), Some(message));
            assert_eq!(host.buffered_bytes(), 0);
        }
    }
}
