//! The wire format for hosting a DAUx plug-in out of process.
//!
//! Sandboxing is not implemented yet, but the architecture is committed to it, and this crate
//! is where that commitment is made concrete. It defines the bytes on the wire and nothing
//! else: no sockets, no pipes, no shared memory, no threads. Transports live in `daux-ipc`,
//! which means every message type here can be tested by encoding it into a buffer and
//! decoding it back, with no I/O anywhere in sight.
//!
//! # Two planes
//!
//! | Plane | Type | Carries | Cost model |
//! |---|---|---|---|
//! | Control | [`ControlMessage`] | lifecycle, parameters, state, editor | length-prefixed frames, allocation allowed |
//! | Data | [`AudioBlockHeader`] | one block of audio and events | fixed layout in shared memory, no allocation |
//!
//! They are separate because their constraints are opposite. A control message may be
//! kilobytes of state and is sent when something happens; a data-plane block is sent every
//! few milliseconds under a hard deadline and must be readable in place, without parsing and
//! without allocating. Squeezing both through one channel would force the audio path to pay
//! for the control path's flexibility.
//!
//! # Every input is hostile
//!
//! A sandbox exists precisely because the other end may be compromised. Nothing here trusts a
//! length, a count or an offset from the wire:
//!
//! - [`ProtocolLimits`] bounds every allocation before it is made;
//! - [`peek_frame_len`] tells a reader how much to buffer without allocating for it first;
//! - [`AudioBlockLayout`] recomputes every region offset rather than believing the sender's,
//!   so a malicious header cannot point a "channel" at the receiver's own memory;
//! - decoding returns [`ProtocolError`] and never panics, whatever the bytes are.

#![deny(unsafe_op_in_unsafe_fn)]

mod codec;
mod control;
mod data;
mod error;
mod framing;
mod limits;

pub use control::{
    ControlMessage, Diagnostics, EditorWindow, ErrorMessage, FeatureFlags, GesturePhase,
    Handshake, InstanceId, MessageKind, PeerRole, ProcessConfigMsg, RequestId, RestartFlags,
    Tail,
};
pub use data::{
    AUDIO_BLOCK_FLAG_IN_PLACE, AUDIO_BLOCK_FLAG_INPUT_EVENTS_TRUNCATED,
    AUDIO_BLOCK_FLAG_OUTPUT_EVENTS_TRUNCATED, AUDIO_BLOCK_FLAG_SILENCE_OUTPUT,
    AUDIO_BLOCK_FLAG_TRANSPORT_VALID, AUDIO_BLOCK_MAGIC, AUDIO_BLOCK_VERSION, AudioBlockHeader,
    AudioBlockLayout, BlobRef, EVENT_PAYLOAD_BYTES, EventPayload, EventRecord, Midi2Payload,
    NoteExpressionPayload, NotePayload, ParamPayload, REGION_ALIGN, TransportSnapshot,
    align_up, sample_bytes,
};
pub use error::{ProtocolError, ProtocolErrorKind, ProtocolResult};
pub use framing::{
    FRAME_HEADER_LEN, FRAME_MAGIC, FrameFlags, FrameHeader, PROTOCOL_VERSION, crc32,
    peek_frame_len,
};
pub use limits::ProtocolLimits;
