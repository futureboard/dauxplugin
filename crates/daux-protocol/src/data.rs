//! The data plane: fixed-size structures laid out for shared memory.
//!
//! Everything in this module is `#[repr(C)]`, has a size that is a compile-time constant,
//! contains no pointers and no Rust `enum`, and carries no implicit padding. That is not
//! style — it is what makes the structures usable as the literal bytes of a region mapped
//! into two processes:
//!
//! * **No pointers.** An address is meaningful only in the process that produced it. Every
//!   reference inside a block is a byte offset from the start of the region instead.
//! * **No padding.** Padding bytes are uninitialised, and copying uninitialised bytes into
//!   a region another process reads is both a leak and undefined behaviour. The layout
//!   tests assert the exact size *and* the exact offset of every field, so a reordering
//!   that introduces a gap fails the build rather than shipping.
//! * **Fixed size.** A peer that grows a structure changes
//!   [`AUDIO_BLOCK_VERSION`], which the reader checks, rather than silently shifting every
//!   field after it.
//!
//! # Trust
//!
//! The peer writing the region may be a plug-in that has just crashed mid-block, or one
//! that is actively hostile. Every offset and count in an [`AudioBlockHeader`] is
//! therefore *claimed*, not *known*, until [`AudioBlockHeader::validate`] has checked it
//! against the real length of the region. Nothing in this crate dereferences the region;
//! `daux-ipc` does, and only after validation.
//!
//! # Real-time safety
//!
//! Every function here is allocation-free, lock-free, branch-bounded and panic-free, and
//! is safe to call from `process`. [`AudioBlockHeader::validate`] is the most expensive
//! one and is bounded by a fixed number of comparisons.

use daux_abi::{
    DAUX_EVENT_CUSTOM, DAUX_EVENT_MIDI1, DAUX_EVENT_MIDI2, DAUX_EVENT_NOTE_CHOKE,
    DAUX_EVENT_NOTE_END, DAUX_EVENT_NOTE_EXPRESSION, DAUX_EVENT_NOTE_OFF, DAUX_EVENT_NOTE_ON,
    DAUX_EVENT_PARAM_GESTURE_BEGIN, DAUX_EVENT_PARAM_GESTURE_END, DAUX_EVENT_PARAM_MOD,
    DAUX_EVENT_PARAM_VALUE, DAUX_EVENT_SYSEX, DAUX_EVENT_TRANSPORT, DAUX_SAMPLE_FORMAT_F32,
    DAUX_SAMPLE_FORMAT_F64, DauxTransportV1,
};

use crate::error::{ProtocolError, ProtocolResult};
use crate::limits::ProtocolLimits;

/// Magic stamped into every [`AudioBlockHeader`]: `b"DXPA"` — DAUx Protocol, Audio.
pub const AUDIO_BLOCK_MAGIC: u32 = u32::from_le_bytes(*b"DXPA");

/// Layout revision of [`AudioBlockHeader`] and [`EventRecord`].
pub const AUDIO_BLOCK_VERSION: u16 = 1;

/// Alignment every sub-region inside a shared audio region starts on.
///
/// One cache line: two processes writing adjacent regions must not share a line, or every
/// block costs a round of false sharing on the interconnect.
pub const REGION_ALIGN: usize = 64;

/// Bytes of inline payload in an [`EventRecord`].
pub const EVENT_PAYLOAD_BYTES: usize = 48;

/// The output samples in this block are undefined and the host should silence them.
pub const AUDIO_BLOCK_FLAG_SILENCE_OUTPUT: u16 = 1 << 0;
/// Input and output planes deliberately point at the same bytes (in-place processing).
pub const AUDIO_BLOCK_FLAG_IN_PLACE: u16 = 1 << 1;
/// [`AudioBlockHeader::transport`] holds a valid snapshot; otherwise there is no transport.
pub const AUDIO_BLOCK_FLAG_TRANSPORT_VALID: u16 = 1 << 2;
/// The producer had more input events than the block could hold and dropped the rest.
pub const AUDIO_BLOCK_FLAG_INPUT_EVENTS_TRUNCATED: u16 = 1 << 3;
/// The plug-in produced more output events than the block could hold.
pub const AUDIO_BLOCK_FLAG_OUTPUT_EVENTS_TRUNCATED: u16 = 1 << 4;

/// [any-thread] Bytes per sample for a `DAUX_SAMPLE_FORMAT_*` value, or `None`.
#[inline]
#[must_use]
pub const fn sample_bytes(sample_format: u32) -> Option<usize> {
    match sample_format {
        DAUX_SAMPLE_FORMAT_F32 => Some(4),
        DAUX_SAMPLE_FORMAT_F64 => Some(8),
        _ => None,
    }
}

/// [any-thread] Rounds `value` up to the next multiple of [`REGION_ALIGN`], or `None` on
/// overflow.
#[inline]
#[must_use]
pub const fn align_up(value: usize) -> Option<usize> {
    match value.checked_add(REGION_ALIGN - 1) {
        Some(v) => Some(v & !(REGION_ALIGN - 1)),
        None => None,
    }
}

// ------------------------------------------------------------------------ transport ---

/// Transport state for one block, laid out for shared memory. [audio-thread]
///
/// A fixed-size mirror of [`DauxTransportV1`] with the platform-dependent `reserved:
/// [usize; 6]` tail replaced by an explicit `u32`, so that the structure is the same size
/// on a 32-bit and a 64-bit peer.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransportSnapshot {
    /// Bitset of `DAUX_TRANSPORT_*` flags; the `HAS_*` bits say which fields are valid.
    pub flags: u32,
    /// Bar number of the bar containing the current position.
    pub bar_number: i32,
    /// Position in samples since the start of the timeline.
    pub song_pos_samples: i64,
    /// Position in quarter notes.
    pub song_pos_beats: f64,
    /// Position in seconds.
    pub song_pos_seconds: f64,
    /// Tempo in quarter notes per minute.
    pub tempo: f64,
    /// Tempo change per sample, for ramps within a block.
    pub tempo_increment: f64,
    /// Beat position of the start of the current bar.
    pub bar_start_beats: f64,
    /// Loop start in quarter notes.
    pub loop_start_beats: f64,
    /// Loop end in quarter notes.
    pub loop_end_beats: f64,
    /// Loop start in seconds.
    pub loop_start_seconds: f64,
    /// Loop end in seconds.
    pub loop_end_seconds: f64,
    /// Time-signature numerator.
    pub time_sig_numerator: u16,
    /// Time-signature denominator.
    pub time_sig_denominator: u16,
    /// Reserved; MUST be zero.
    pub reserved: u32,
}

impl TransportSnapshot {
    /// Encoded size in bytes.
    pub const SIZE: usize = size_of::<Self>();

    /// [audio-thread] An all-zero snapshot: no flags set, so no field is valid.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            flags: 0,
            bar_number: 0,
            song_pos_samples: 0,
            song_pos_beats: 0.0,
            song_pos_seconds: 0.0,
            tempo: 0.0,
            tempo_increment: 0.0,
            bar_start_beats: 0.0,
            loop_start_beats: 0.0,
            loop_end_beats: 0.0,
            loop_start_seconds: 0.0,
            loop_end_seconds: 0.0,
            time_sig_numerator: 0,
            time_sig_denominator: 0,
            reserved: 0,
        }
    }

    /// [audio-thread] `true` when every `DAUX_TRANSPORT_*` bit in `flags` is set.
    #[inline]
    #[must_use]
    pub const fn has_flags(&self, flags: u32) -> bool {
        self.flags & flags == flags
    }
}

impl Default for TransportSnapshot {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl From<DauxTransportV1> for TransportSnapshot {
    #[inline]
    fn from(t: DauxTransportV1) -> Self {
        Self {
            flags: t.flags,
            bar_number: t.bar_number,
            song_pos_samples: t.song_pos_samples,
            song_pos_beats: t.song_pos_beats,
            song_pos_seconds: t.song_pos_seconds,
            tempo: t.tempo,
            tempo_increment: t.tempo_increment,
            bar_start_beats: t.bar_start_beats,
            loop_start_beats: t.loop_start_beats,
            loop_end_beats: t.loop_end_beats,
            loop_start_seconds: t.loop_start_seconds,
            loop_end_seconds: t.loop_end_seconds,
            time_sig_numerator: t.time_sig_numerator,
            time_sig_denominator: t.time_sig_denominator,
            reserved: 0,
        }
    }
}

impl From<TransportSnapshot> for DauxTransportV1 {
    #[inline]
    fn from(s: TransportSnapshot) -> Self {
        let mut t = Self::new();
        t.flags = s.flags;
        t.bar_number = s.bar_number;
        t.song_pos_samples = s.song_pos_samples;
        t.song_pos_beats = s.song_pos_beats;
        t.song_pos_seconds = s.song_pos_seconds;
        t.tempo = s.tempo;
        t.tempo_increment = s.tempo_increment;
        t.bar_start_beats = s.bar_start_beats;
        t.loop_start_beats = s.loop_start_beats;
        t.loop_end_beats = s.loop_end_beats;
        t.loop_start_seconds = s.loop_start_seconds;
        t.loop_end_seconds = s.loop_end_seconds;
        t.time_sig_numerator = s.time_sig_numerator;
        t.time_sig_denominator = s.time_sig_denominator;
        t
    }
}

// ---------------------------------------------------------------------------- events ---

/// A note event's inline payload. [audio-thread]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NotePayload {
    /// Host-assigned voice id, or `-1`.
    pub note_id: i32,
    /// MIDI channel, or `-1` as a wildcard.
    pub channel: i16,
    /// Key `0..=127`, or `-1` as a wildcard.
    pub key: i16,
    /// Velocity, `0.0 ..= 1.0`.
    pub velocity: f64,
    /// Cents offset from equal temperament.
    pub tuning: f64,
}

/// A per-note expression event's inline payload. [audio-thread]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NoteExpressionPayload {
    /// One of the `DAUX_NOTE_EXPR_*` constants.
    pub expression_id: u32,
    /// Host-assigned voice id, or `-1`.
    pub note_id: i32,
    /// MIDI channel, or `-1` as a wildcard.
    pub channel: i16,
    /// Key `0..=127`, or `-1` as a wildcard.
    pub key: i16,
    /// Expression value; the range depends on `expression_id`.
    pub value: f64,
}

/// A parameter event's inline payload. [audio-thread]
///
/// Values are plain (real-world) units, never normalised, matching `abi-v1` §9.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParamPayload {
    /// Permanent parameter id.
    pub param_id: u32,
    /// `-1` unless the change is scoped to one voice.
    pub note_id: i32,
    /// MIDI channel, or `-1` as a wildcard.
    pub channel: i16,
    /// Key `0..=127`, or `-1` as a wildcard.
    pub key: i16,
    /// Absolute value, or a signed offset for a modulation event.
    pub value: f64,
}

/// One MIDI 2.0 Universal MIDI Packet. [audio-thread]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Midi2Payload {
    /// Valid words in `words`, `1 ..= 4`.
    pub word_count: u32,
    /// The packet; entries at and beyond `word_count` are zero.
    pub words: [u32; 4],
}

/// A reference to bytes in the block's blob pool. [audio-thread]
///
/// SysEx, transport discontinuities and vendor-defined events are variable-length, so they
/// live in the pool and the fixed-size record points at them with an offset relative to
/// [`AudioBlockHeader::blob_offset`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BlobRef {
    /// Byte offset into the blob pool.
    pub offset: u32,
    /// Byte length.
    pub len: u32,
}

/// The decoded payload of an [`EventRecord`]. [audio-thread]
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum EventPayload {
    /// A note on, off, choke or end.
    Note(NotePayload),
    /// A per-note expression change.
    NoteExpression(NoteExpressionPayload),
    /// A parameter value, modulation or gesture boundary.
    Param(ParamPayload),
    /// A MIDI 1.0 message: status byte plus up to two data bytes.
    Midi1([u8; 3]),
    /// A MIDI 2.0 Universal MIDI Packet.
    Midi2(Midi2Payload),
    /// Variable-length bytes in the blob pool: SysEx, transport or vendor-defined.
    Blob(BlobRef),
}

/// One event in a shared-memory block: fixed size, no pointers. [audio-thread]
///
/// The record is exactly one cache line. `kind` reuses the `DAUX_EVENT_*` constants of
/// `abi-v1` §9 so that a sandbox can translate an event without a lookup table, and
/// `payload` holds the little-endian fields of whichever payload `kind` selects.
#[repr(C, align(8))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EventRecord {
    /// Sample offset within the block, `0 ..= frame_count - 1`.
    pub time: u32,
    /// One of the `DAUX_EVENT_*` constants.
    pub kind: u16,
    /// Bitset of `DAUX_EVENT_FLAG_*`.
    pub flags: u16,
    /// Which event port the event belongs to.
    pub port_index: u16,
    /// Meaningful bytes in `payload`; the rest MUST be zero.
    pub payload_len: u16,
    /// Reserved; MUST be zero.
    pub reserved: u32,
    /// Little-endian payload bytes, selected by `kind`.
    pub payload: [u8; EVENT_PAYLOAD_BYTES],
}

impl EventRecord {
    /// Encoded size in bytes: one cache line.
    pub const SIZE: usize = size_of::<Self>();

    /// [audio-thread] An all-zero record. `kind` is zero, which is not a valid event, so
    /// [`EventRecord::validate`] rejects it until a payload is set.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            time: 0,
            kind: 0,
            flags: 0,
            port_index: 0,
            payload_len: 0,
            reserved: 0,
            payload: [0; EVENT_PAYLOAD_BYTES],
        }
    }

    /// [audio-thread] Builds a record. Never allocates and never fails: the payload
    /// determines its own length, and every payload fits in [`EVENT_PAYLOAD_BYTES`].
    ///
    /// `kind` must match `payload`; [`EventRecord::validate`] checks that it does.
    #[must_use]
    pub fn with_payload(time: u32, kind: u16, port_index: u16, payload: EventPayload) -> Self {
        let mut r = Self::new();
        r.time = time;
        r.kind = kind;
        r.port_index = port_index;
        r.payload_len = r.write_payload(payload);
        r
    }

    /// [audio-thread] Sets the event flags, returning the record for chaining.
    #[inline]
    #[must_use]
    pub const fn with_flags(mut self, flags: u16) -> Self {
        self.flags = flags;
        self
    }

    fn write_payload(&mut self, payload: EventPayload) -> u16 {
        let mut w = PayloadWriter::new(&mut self.payload);
        match payload {
            EventPayload::Note(n) => {
                w.i32(n.note_id);
                w.i16(n.channel);
                w.i16(n.key);
                w.f64(n.velocity);
                w.f64(n.tuning);
            }
            EventPayload::NoteExpression(e) => {
                w.u32(e.expression_id);
                w.i32(e.note_id);
                w.i16(e.channel);
                w.i16(e.key);
                w.u32(0);
                w.f64(e.value);
            }
            EventPayload::Param(p) => {
                w.u32(p.param_id);
                w.i32(p.note_id);
                w.i16(p.channel);
                w.i16(p.key);
                w.u32(0);
                w.f64(p.value);
            }
            EventPayload::Midi1(bytes) => {
                w.u8(bytes[0]);
                w.u8(bytes[1]);
                w.u8(bytes[2]);
            }
            EventPayload::Midi2(m) => {
                w.u32(m.word_count);
                for word in m.words {
                    w.u32(word);
                }
            }
            EventPayload::Blob(b) => {
                w.u32(b.offset);
                w.u32(b.len);
            }
        }
        w.written()
    }

    /// [audio-thread] Bytes the payload of `kind` occupies.
    ///
    /// `None` for a kind this build does not know and that is below the vendor range.
    #[must_use]
    pub const fn payload_bytes_for(kind: u16) -> Option<u16> {
        match kind {
            DAUX_EVENT_NOTE_ON
            | DAUX_EVENT_NOTE_OFF
            | DAUX_EVENT_NOTE_CHOKE
            | DAUX_EVENT_NOTE_END => Some(24),
            DAUX_EVENT_NOTE_EXPRESSION => Some(24),
            DAUX_EVENT_PARAM_VALUE
            | DAUX_EVENT_PARAM_MOD
            | DAUX_EVENT_PARAM_GESTURE_BEGIN
            | DAUX_EVENT_PARAM_GESTURE_END => Some(24),
            DAUX_EVENT_MIDI1 => Some(3),
            DAUX_EVENT_MIDI2 => Some(20),
            DAUX_EVENT_SYSEX | DAUX_EVENT_TRANSPORT => Some(8),
            k if k >= DAUX_EVENT_CUSTOM => Some(8),
            _ => None,
        }
    }

    /// [audio-thread] Checks the record against its own `kind` before anything reads it.
    ///
    /// Rejects an unknown kind, a payload length that does not match the kind, a non-zero
    /// reserved word, meaningful-looking bytes past `payload_len`, and a MIDI 2.0 packet
    /// with an impossible word count. All of those are reachable from a corrupt or
    /// hostile region.
    ///
    /// # Errors
    ///
    /// [`ProtocolErrorKind::InvalidValue`](crate::ProtocolErrorKind::InvalidValue) for a
    /// field that cannot be acted on.
    pub fn validate(&self) -> ProtocolResult<()> {
        if self.reserved != 0 {
            return Err(ProtocolError::invalid("EventRecord::reserved"));
        }
        let expected = Self::payload_bytes_for(self.kind)
            .ok_or(ProtocolError::invalid("EventRecord::kind"))?;
        if self.payload_len != expected {
            return Err(ProtocolError::invalid("EventRecord::payload_len"));
        }
        // The tail must be zero so that one value has exactly one encoding: otherwise two
        // records that mean the same thing compare unequal, and a peer could hide data in
        // the slack.
        let used = expected as usize;
        if self.payload[used..].iter().any(|b| *b != 0) {
            return Err(ProtocolError::invalid("EventRecord::payload"));
        }
        if self.kind == DAUX_EVENT_MIDI2 {
            let m = self.read_midi2();
            if m.word_count == 0 || m.word_count > 4 {
                return Err(ProtocolError::invalid("EventRecord::word_count"));
            }
            if m.words[m.word_count as usize..].iter().any(|w| *w != 0) {
                return Err(ProtocolError::invalid("EventRecord::words"));
            }
        }
        Ok(())
    }

    /// [audio-thread] Decodes the payload according to `kind`.
    ///
    /// # Errors
    ///
    /// Whatever [`EventRecord::validate`] rejects; the record is validated first, so a
    /// caller never sees a payload decoded from an inconsistent record.
    pub fn payload(&self) -> ProtocolResult<EventPayload> {
        self.validate()?;
        let mut r = PayloadReader::new(&self.payload);
        Ok(match self.kind {
            DAUX_EVENT_NOTE_ON
            | DAUX_EVENT_NOTE_OFF
            | DAUX_EVENT_NOTE_CHOKE
            | DAUX_EVENT_NOTE_END => EventPayload::Note(NotePayload {
                note_id: r.i32(),
                channel: r.i16(),
                key: r.i16(),
                velocity: r.f64(),
                tuning: r.f64(),
            }),
            DAUX_EVENT_NOTE_EXPRESSION => {
                let expression_id = r.u32();
                let note_id = r.i32();
                let channel = r.i16();
                let key = r.i16();
                let _pad = r.u32();
                EventPayload::NoteExpression(NoteExpressionPayload {
                    expression_id,
                    note_id,
                    channel,
                    key,
                    value: r.f64(),
                })
            }
            DAUX_EVENT_PARAM_VALUE
            | DAUX_EVENT_PARAM_MOD
            | DAUX_EVENT_PARAM_GESTURE_BEGIN
            | DAUX_EVENT_PARAM_GESTURE_END => {
                let param_id = r.u32();
                let note_id = r.i32();
                let channel = r.i16();
                let key = r.i16();
                let _pad = r.u32();
                EventPayload::Param(ParamPayload {
                    param_id,
                    note_id,
                    channel,
                    key,
                    value: r.f64(),
                })
            }
            DAUX_EVENT_MIDI1 => EventPayload::Midi1([r.u8(), r.u8(), r.u8()]),
            DAUX_EVENT_MIDI2 => EventPayload::Midi2(self.read_midi2()),
            _ => EventPayload::Blob(BlobRef {
                offset: r.u32(),
                len: r.u32(),
            }),
        })
    }

    fn read_midi2(&self) -> Midi2Payload {
        let mut r = PayloadReader::new(&self.payload);
        let word_count = r.u32();
        Midi2Payload {
            word_count,
            words: [r.u32(), r.u32(), r.u32(), r.u32()],
        }
    }
}

impl Default for EventRecord {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

/// Fixed-capacity little-endian appender over an event payload.
///
/// Every payload is known at compile time to fit in [`EVENT_PAYLOAD_BYTES`], so writes
/// past the end are impossible; the `saturating` arithmetic is belt and braces so that a
/// future payload that did not fit would truncate rather than panic on the audio thread.
struct PayloadWriter<'a> {
    bytes: &'a mut [u8; EVENT_PAYLOAD_BYTES],
    pos: usize,
}

macro_rules! payload_write {
    ($name:ident, $ty:ty) => {
        fn $name(&mut self, value: $ty) {
            let raw = value.to_le_bytes();
            let end = self.pos.saturating_add(raw.len());
            if end <= EVENT_PAYLOAD_BYTES {
                self.bytes[self.pos..end].copy_from_slice(&raw);
                self.pos = end;
            }
        }
    };
}

impl<'a> PayloadWriter<'a> {
    fn new(bytes: &'a mut [u8; EVENT_PAYLOAD_BYTES]) -> Self {
        bytes.fill(0);
        Self { bytes, pos: 0 }
    }

    payload_write!(u8, u8);
    payload_write!(u32, u32);
    payload_write!(i16, i16);
    payload_write!(i32, i32);
    payload_write!(f64, f64);

    fn written(&self) -> u16 {
        // `EVENT_PAYLOAD_BYTES` is 48, so the cast is exact.
        self.pos as u16
    }
}

/// Fixed-capacity little-endian cursor over an event payload.
///
/// Reads past the end yield zero rather than failing: the caller has already run
/// [`EventRecord::validate`], which proves the payload is long enough for its kind, so an
/// out-of-range read is unreachable and defining it away removes an error path from the
/// audio thread.
struct PayloadReader<'a> {
    bytes: &'a [u8; EVENT_PAYLOAD_BYTES],
    pos: usize,
}

macro_rules! payload_read {
    ($name:ident, $ty:ty) => {
        fn $name(&mut self) -> $ty {
            const N: usize = size_of::<$ty>();
            let mut raw = [0u8; N];
            let end = self.pos.saturating_add(N);
            if end <= EVENT_PAYLOAD_BYTES {
                raw.copy_from_slice(&self.bytes[self.pos..end]);
                self.pos = end;
            }
            <$ty>::from_le_bytes(raw)
        }
    };
}

impl<'a> PayloadReader<'a> {
    fn new(bytes: &'a [u8; EVENT_PAYLOAD_BYTES]) -> Self {
        Self { bytes, pos: 0 }
    }

    payload_read!(u8, u8);
    payload_read!(u32, u32);
    payload_read!(i16, i16);
    payload_read!(i32, i32);
    payload_read!(f64, f64);
}

// ----------------------------------------------------------------------- audio block ---

/// The header at offset zero of a shared audio region. [audio-thread]
///
/// Everything the two processes need to agree on for one `process` call: where the sample
/// planes are, where the event arrays are, how many frames are live, and what came back.
/// All positions are byte offsets from the start of the region, so the same header is
/// meaningful in both processes regardless of where each mapped it.
///
/// # Handshake
///
/// `sequence` is written last by the producer and read first by the consumer. Ownership of
/// the region passes with the signal that carries the same sequence number (see
/// `daux_ipc::DataPlane`), so the two sides never write the region at the same time.
#[repr(C, align(8))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AudioBlockHeader {
    /// [`AUDIO_BLOCK_MAGIC`].
    pub magic: u32,
    /// [`AUDIO_BLOCK_VERSION`].
    pub version: u16,
    /// Bitset of `AUDIO_BLOCK_FLAG_*`.
    pub flags: u16,
    /// Monotonic block counter; identifies which submission this header describes.
    pub sequence: u64,
    /// The instance this block belongs to; see `daux_protocol::InstanceId`.
    pub instance: u64,
    /// Monotonic sample counter since processing started, or `-1` when unavailable.
    pub steady_time: i64,
    /// Live frames in this block, `1 ..= max_frames`.
    pub frame_count: u32,
    /// Exactly one `DAUX_SAMPLE_FORMAT_*` bit.
    pub sample_format: u32,
    /// Input channels laid out at `input_offset`.
    pub input_channels: u32,
    /// Output channels laid out at `output_offset`.
    pub output_channels: u32,
    /// Bytes between the start of one channel plane and the next.
    pub channel_stride: u64,
    /// Byte offset of input channel 0.
    pub input_offset: u64,
    /// Byte offset of output channel 0.
    pub output_offset: u64,
    /// Byte offset of the input [`EventRecord`] array.
    pub input_event_offset: u64,
    /// Byte offset of the output [`EventRecord`] array.
    pub output_event_offset: u64,
    /// Byte offset of the variable-length blob pool.
    pub blob_offset: u64,
    /// Records the input event array can hold.
    pub input_event_capacity: u32,
    /// Records actually present in the input event array.
    pub input_event_count: u32,
    /// Records the output event array can hold.
    pub output_event_capacity: u32,
    /// Records actually written to the output event array.
    pub output_event_count: u32,
    /// Bytes the blob pool can hold.
    pub blob_capacity: u32,
    /// Bytes of the blob pool in use.
    pub blob_used: u32,
    /// Bit `c` set means input channel `c` is constant for the whole block. A hint only.
    pub constant_mask_in: u64,
    /// Bit `c` set means output channel `c` is constant for the whole block. A hint only.
    pub constant_mask_out: u64,
    /// Transport state; valid only with [`AUDIO_BLOCK_FLAG_TRANSPORT_VALID`].
    pub transport: TransportSnapshot,
    /// One of the `DAUX_PROCESS_*` results, written back by the plug-in side.
    pub status: i32,
    /// Reserved; MUST be zero.
    pub reserved0: u32,
    /// Reserved; MUST be all zero.
    pub reserved: [u64; 4],
}

impl AudioBlockHeader {
    /// Encoded size in bytes.
    pub const SIZE: usize = size_of::<Self>();

    /// [audio-thread] A zeroed header with the magic and version set.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            magic: AUDIO_BLOCK_MAGIC,
            version: AUDIO_BLOCK_VERSION,
            flags: 0,
            sequence: 0,
            instance: 0,
            steady_time: -1,
            frame_count: 0,
            sample_format: DAUX_SAMPLE_FORMAT_F32,
            input_channels: 0,
            output_channels: 0,
            channel_stride: 0,
            input_offset: 0,
            output_offset: 0,
            input_event_offset: 0,
            output_event_offset: 0,
            blob_offset: 0,
            input_event_capacity: 0,
            input_event_count: 0,
            output_event_capacity: 0,
            output_event_count: 0,
            blob_capacity: 0,
            blob_used: 0,
            constant_mask_in: 0,
            constant_mask_out: 0,
            transport: TransportSnapshot::new(),
            status: 0,
            reserved0: 0,
            reserved: [0; 4],
        }
    }

    /// [audio-thread] `true` when every `AUDIO_BLOCK_FLAG_*` bit in `flags` is set.
    #[inline]
    #[must_use]
    pub const fn has_flags(&self, flags: u16) -> bool {
        self.flags & flags == flags
    }

    /// [audio-thread] Borrows the header as the bytes that go into the region.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        // SAFETY: `AudioBlockHeader` is `#[repr(C)]` and every field is an integer, a
        // float or a `#[repr(C)]` aggregate of those, so it owns no pointer, has no niche
        // and — as `layout_has_no_padding` asserts field offset by field offset — carries
        // no padding, meaning all `SIZE` bytes are initialised. The slice borrows `self`
        // for its own lifetime and is immutable, so no aliasing or mutation rule is
        // affected.
        unsafe { core::slice::from_raw_parts(core::ptr::from_ref(self).cast::<u8>(), Self::SIZE) }
    }

    /// [audio-thread] Copies a header out of the first [`AudioBlockHeader::SIZE`] bytes of
    /// `bytes`.
    ///
    /// The result is *not* validated; call [`AudioBlockHeader::validate`] before acting on
    /// any field.
    ///
    /// # Errors
    ///
    /// [`ProtocolErrorKind::Truncated`](crate::ProtocolErrorKind::Truncated) when `bytes`
    /// is shorter than the header.
    pub fn read_from(bytes: &[u8]) -> ProtocolResult<Self> {
        if bytes.len() < Self::SIZE {
            return Err(ProtocolError::truncated(
                "AudioBlockHeader",
                Self::SIZE,
                bytes.len(),
            ));
        }
        // SAFETY: the length check above guarantees `SIZE` readable bytes at `bytes`, and
        // every bit pattern of those bytes is a valid `AudioBlockHeader` because the type
        // is a `#[repr(C)]` aggregate of integers and floats with no niche and no padding.
        // `read_unaligned` imposes no alignment requirement on the source, which matters
        // because `bytes` may point anywhere inside a mapped region. The value is copied,
        // so the returned header borrows nothing.
        Ok(unsafe { core::ptr::read_unaligned(bytes.as_ptr().cast::<Self>()) })
    }

    /// [audio-thread] Checks every claim the header makes against the region that holds it.
    ///
    /// This is the trust boundary for the data plane. On success, and only then, all of
    /// the following hold, which is exactly what a reader needs in order to index the
    /// region without further checks:
    ///
    /// * the magic and version are this layout;
    /// * `frame_count`, the channel counts and the event capacities are within `limits`;
    /// * `channel_stride` covers `frame_count` samples of `sample_format`;
    /// * every sub-region lies entirely inside `region_len` and starts after the header;
    /// * no two sub-regions overlap, except input and output planes when
    ///   [`AUDIO_BLOCK_FLAG_IN_PLACE`] is set;
    /// * every count is within its capacity.
    ///
    /// # Errors
    ///
    /// [`ProtocolErrorKind::InvalidLayout`](crate::ProtocolErrorKind::InvalidLayout) or
    /// [`ProtocolErrorKind::InvalidValue`](crate::ProtocolErrorKind::InvalidValue),
    /// naming the field at fault.
    pub fn validate(&self, region_len: usize, limits: &ProtocolLimits) -> ProtocolResult<()> {
        if self.magic != AUDIO_BLOCK_MAGIC {
            return Err(ProtocolError::layout("AudioBlockHeader::magic"));
        }
        if self.version != AUDIO_BLOCK_VERSION {
            return Err(ProtocolError::layout("AudioBlockHeader::version"));
        }
        if self.reserved0 != 0 || self.reserved != [0; 4] || self.transport.reserved != 0 {
            return Err(ProtocolError::invalid("AudioBlockHeader::reserved"));
        }
        if region_len < Self::SIZE {
            return Err(ProtocolError::layout("AudioBlockHeader::region_len"));
        }
        let sample = sample_bytes(self.sample_format)
            .ok_or(ProtocolError::invalid("AudioBlockHeader::sample_format"))?;
        if self.frame_count == 0 || self.frame_count as usize > limits.max_frames {
            return Err(ProtocolError::invalid("AudioBlockHeader::frame_count"));
        }
        if self.input_channels as usize > limits.max_channels
            || self.output_channels as usize > limits.max_channels
        {
            return Err(ProtocolError::invalid("AudioBlockHeader::channels"));
        }
        if self.input_event_capacity as usize > limits.max_events
            || self.output_event_capacity as usize > limits.max_events
        {
            return Err(ProtocolError::invalid("AudioBlockHeader::event_capacity"));
        }
        if self.input_event_count > self.input_event_capacity
            || self.output_event_count > self.output_event_capacity
        {
            return Err(ProtocolError::invalid("AudioBlockHeader::event_count"));
        }
        if self.blob_used > self.blob_capacity {
            return Err(ProtocolError::invalid("AudioBlockHeader::blob_used"));
        }
        let needed = (self.frame_count as usize)
            .checked_mul(sample)
            .ok_or(ProtocolError::layout("AudioBlockHeader::frame_count"))?;
        let stride = usize::try_from(self.channel_stride)
            .map_err(|_| ProtocolError::layout("AudioBlockHeader::channel_stride"))?;
        if (self.input_channels > 0 || self.output_channels > 0) && stride < needed {
            return Err(ProtocolError::layout("AudioBlockHeader::channel_stride"));
        }

        let inputs = self.plane_span(
            "AudioBlockHeader::input",
            self.input_offset,
            self.input_channels,
            stride,
        )?;
        let outputs = self.plane_span(
            "AudioBlockHeader::output",
            self.output_offset,
            self.output_channels,
            stride,
        )?;
        let in_events = span(
            "AudioBlockHeader::input_event_offset",
            self.input_event_offset,
            (self.input_event_capacity as usize)
                .checked_mul(EventRecord::SIZE)
                .ok_or(ProtocolError::layout(
                    "AudioBlockHeader::input_event_capacity",
                ))?,
        )?;
        let out_events = span(
            "AudioBlockHeader::output_event_offset",
            self.output_event_offset,
            (self.output_event_capacity as usize)
                .checked_mul(EventRecord::SIZE)
                .ok_or(ProtocolError::layout(
                    "AudioBlockHeader::output_event_capacity",
                ))?,
        )?;
        let blob = span(
            "AudioBlockHeader::blob_offset",
            self.blob_offset,
            self.blob_capacity as usize,
        )?;

        let spans = [inputs, outputs, in_events, out_events, blob];
        for s in spans {
            if s.1 == 0 {
                continue;
            }
            if s.0 < Self::SIZE {
                return Err(ProtocolError::layout("AudioBlockHeader::overlaps_header"));
            }
            let end =
                s.0.checked_add(s.1)
                    .ok_or(ProtocolError::layout("AudioBlockHeader::span"))?;
            if end > region_len {
                return Err(ProtocolError::layout("AudioBlockHeader::past_region_end"));
            }
        }
        let aliasing_allowed = self.has_flags(AUDIO_BLOCK_FLAG_IN_PLACE);
        for i in 0..spans.len() {
            for j in i + 1..spans.len() {
                if aliasing_allowed && i == 0 && j == 1 {
                    continue;
                }
                if overlaps(spans[i], spans[j]) {
                    return Err(ProtocolError::layout("AudioBlockHeader::overlapping_spans"));
                }
            }
        }
        Ok(())
    }

    fn plane_span(
        &self,
        context: &'static str,
        offset: u64,
        channels: u32,
        stride: usize,
    ) -> ProtocolResult<(usize, usize)> {
        let len = (channels as usize)
            .checked_mul(stride)
            .ok_or(ProtocolError::layout(context))?;
        span(context, offset, len)
    }
}

impl Default for AudioBlockHeader {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

fn span(context: &'static str, offset: u64, len: usize) -> ProtocolResult<(usize, usize)> {
    let start = usize::try_from(offset).map_err(|_| ProtocolError::layout(context))?;
    Ok((start, len))
}

const fn overlaps(a: (usize, usize), b: (usize, usize)) -> bool {
    if a.1 == 0 || b.1 == 0 {
        return false;
    }
    // Both ends were bounded by `region_len` before this runs, so neither addition wraps.
    a.0 < b.0 + b.1 && b.0 < a.0 + a.1
}

/// How a shared audio region is carved up. [main-thread]
///
/// Produces the [`AudioBlockHeader`] a producer stamps into a fresh region and the region
/// size that has to be allocated for it. Every sub-region is padded to [`REGION_ALIGN`],
/// so no two of them share a cache line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AudioBlockLayout {
    /// Input channels to reserve.
    pub input_channels: u32,
    /// Output channels to reserve.
    pub output_channels: u32,
    /// Largest block, in frames, the region must be able to carry.
    pub max_frames: u32,
    /// Exactly one `DAUX_SAMPLE_FORMAT_*` bit.
    pub sample_format: u32,
    /// Input event records to reserve.
    pub input_events: u32,
    /// Output event records to reserve.
    pub output_events: u32,
    /// Bytes of blob pool to reserve, for SysEx and other variable-length payloads.
    pub blob_bytes: u32,
}

impl AudioBlockLayout {
    /// Event records reserved per direction when the defaults are used.
    pub const DEFAULT_EVENTS: u32 = 512;
    /// Blob pool bytes reserved when the defaults are used.
    pub const DEFAULT_BLOB_BYTES: u32 = 8 * 1024;

    /// [main-thread] A single-precision layout with the default event and blob budgets.
    #[must_use]
    pub const fn new(input_channels: u32, output_channels: u32, max_frames: u32) -> Self {
        Self {
            input_channels,
            output_channels,
            max_frames,
            sample_format: DAUX_SAMPLE_FORMAT_F32,
            input_events: Self::DEFAULT_EVENTS,
            output_events: Self::DEFAULT_EVENTS,
            blob_bytes: Self::DEFAULT_BLOB_BYTES,
        }
    }

    /// [main-thread] Returns the layout with a different sample format.
    #[must_use]
    pub const fn with_sample_format(mut self, sample_format: u32) -> Self {
        self.sample_format = sample_format;
        self
    }

    /// [main-thread] Returns the layout with different event budgets.
    #[must_use]
    pub const fn with_events(mut self, input_events: u32, output_events: u32) -> Self {
        self.input_events = input_events;
        self.output_events = output_events;
        self
    }

    /// [main-thread] Returns the layout with a different blob pool size.
    #[must_use]
    pub const fn with_blob_bytes(mut self, blob_bytes: u32) -> Self {
        self.blob_bytes = blob_bytes;
        self
    }

    /// [main-thread] Bytes per channel plane, padded to [`REGION_ALIGN`].
    ///
    /// # Errors
    ///
    /// [`ProtocolErrorKind::InvalidValue`](crate::ProtocolErrorKind::InvalidValue) for an
    /// unknown sample format, or
    /// [`ProtocolErrorKind::InvalidLayout`](crate::ProtocolErrorKind::InvalidLayout) if
    /// the product overflows.
    pub fn channel_stride(&self) -> ProtocolResult<usize> {
        let sample = sample_bytes(self.sample_format)
            .ok_or(ProtocolError::invalid("AudioBlockLayout::sample_format"))?;
        (self.max_frames as usize)
            .checked_mul(sample)
            .and_then(align_up)
            .ok_or(ProtocolError::layout("AudioBlockLayout::channel_stride"))
    }

    /// [main-thread] Total bytes the region must have.
    ///
    /// # Errors
    ///
    /// As [`AudioBlockLayout::header`].
    pub fn region_len(&self, limits: &ProtocolLimits) -> ProtocolResult<usize> {
        Ok(self.compute(limits)?.0)
    }

    /// [main-thread] The header to stamp at offset zero of a region of
    /// [`AudioBlockLayout::region_len`] bytes.
    ///
    /// `frame_count` starts at `max_frames`; a producer lowers it per block.
    ///
    /// # Errors
    ///
    /// [`ProtocolErrorKind::InvalidValue`](crate::ProtocolErrorKind::InvalidValue) for a
    /// zero `max_frames` or an unknown sample format,
    /// [`ProtocolErrorKind::LimitExceeded`](crate::ProtocolErrorKind::LimitExceeded) when
    /// the layout is outside `limits`, and
    /// [`ProtocolErrorKind::InvalidLayout`](crate::ProtocolErrorKind::InvalidLayout) on
    /// arithmetic overflow.
    pub fn header(
        &self,
        instance: u64,
        limits: &ProtocolLimits,
    ) -> ProtocolResult<AudioBlockHeader> {
        Ok(self.compute(limits)?.1.with_instance(instance))
    }

    fn compute(&self, limits: &ProtocolLimits) -> ProtocolResult<(usize, AudioBlockHeader)> {
        if self.max_frames == 0 {
            return Err(ProtocolError::invalid("AudioBlockLayout::max_frames"));
        }
        if self.max_frames as usize > limits.max_frames {
            return Err(ProtocolError::limit(
                "AudioBlockLayout::max_frames",
                limits.max_frames,
                self.max_frames as usize,
            ));
        }
        if self.input_channels as usize > limits.max_channels {
            return Err(ProtocolError::limit(
                "AudioBlockLayout::input_channels",
                limits.max_channels,
                self.input_channels as usize,
            ));
        }
        if self.output_channels as usize > limits.max_channels {
            return Err(ProtocolError::limit(
                "AudioBlockLayout::output_channels",
                limits.max_channels,
                self.output_channels as usize,
            ));
        }
        if self.input_events as usize > limits.max_events {
            return Err(ProtocolError::limit(
                "AudioBlockLayout::input_events",
                limits.max_events,
                self.input_events as usize,
            ));
        }
        if self.output_events as usize > limits.max_events {
            return Err(ProtocolError::limit(
                "AudioBlockLayout::output_events",
                limits.max_events,
                self.output_events as usize,
            ));
        }
        let stride = self.channel_stride()?;
        let mut cursor = Cursor::new();
        let input_offset = cursor.reserve_planes(self.input_channels, stride)?;
        let output_offset = cursor.reserve_planes(self.output_channels, stride)?;
        let input_event_offset = cursor.reserve_records(self.input_events)?;
        let output_event_offset = cursor.reserve_records(self.output_events)?;
        let blob_offset = cursor.reserve(self.blob_bytes as usize)?;

        let mut header = AudioBlockHeader::new();
        header.frame_count = self.max_frames;
        header.sample_format = self.sample_format;
        header.input_channels = self.input_channels;
        header.output_channels = self.output_channels;
        header.channel_stride = stride as u64;
        header.input_offset = input_offset as u64;
        header.output_offset = output_offset as u64;
        header.input_event_offset = input_event_offset as u64;
        header.output_event_offset = output_event_offset as u64;
        header.blob_offset = blob_offset as u64;
        header.input_event_capacity = self.input_events;
        header.output_event_capacity = self.output_events;
        header.blob_capacity = self.blob_bytes;
        Ok((cursor.end, header))
    }
}

impl AudioBlockHeader {
    #[inline]
    const fn with_instance(mut self, instance: u64) -> Self {
        self.instance = instance;
        self
    }
}

/// Running offset used while carving up a region; every reservation is `REGION_ALIGN`ed.
struct Cursor {
    end: usize,
}

impl Cursor {
    fn new() -> Self {
        Self {
            // The header always occupies the front of the region.
            end: align_up(AudioBlockHeader::SIZE).unwrap_or(AudioBlockHeader::SIZE),
        }
    }

    fn reserve(&mut self, len: usize) -> ProtocolResult<usize> {
        let start = self.end;
        if len == 0 {
            return Ok(start);
        }
        self.end = start
            .checked_add(len)
            .and_then(align_up)
            .ok_or(ProtocolError::layout("AudioBlockLayout::region_len"))?;
        Ok(start)
    }

    fn reserve_planes(&mut self, channels: u32, stride: usize) -> ProtocolResult<usize> {
        let len = (channels as usize)
            .checked_mul(stride)
            .ok_or(ProtocolError::layout("AudioBlockLayout::planes"))?;
        self.reserve(len)
    }

    fn reserve_records(&mut self, records: u32) -> ProtocolResult<usize> {
        let len = (records as usize)
            .checked_mul(EventRecord::SIZE)
            .ok_or(ProtocolError::layout("AudioBlockLayout::events"))?;
        self.reserve(len)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AUDIO_BLOCK_FLAG_IN_PLACE, AUDIO_BLOCK_MAGIC, AUDIO_BLOCK_VERSION, AudioBlockHeader,
        AudioBlockLayout, BlobRef, EVENT_PAYLOAD_BYTES, EventPayload, EventRecord, Midi2Payload,
        NoteExpressionPayload, NotePayload, ParamPayload, REGION_ALIGN, TransportSnapshot,
        align_up, sample_bytes,
    };
    use crate::error::ProtocolErrorKind;
    use crate::limits::ProtocolLimits;
    use core::mem::offset_of;
    use daux_abi::{
        DAUX_EVENT_MIDI1, DAUX_EVENT_MIDI2, DAUX_EVENT_NOTE_EXPRESSION, DAUX_EVENT_NOTE_ON,
        DAUX_EVENT_PARAM_VALUE, DAUX_EVENT_SYSEX, DAUX_SAMPLE_FORMAT_F32, DAUX_SAMPLE_FORMAT_F64,
        DAUX_TRANSPORT_HAS_TEMPO, DAUX_TRANSPORT_IS_PLAYING, DauxTransportV1,
    };

    // ---------------------------------------------------------------------- layout ---

    /// The layout *is* the contract with the other process. Every offset is pinned here,
    /// so a reordering or an inserted field fails this test instead of silently making
    /// two builds of DAUxPlug disagree about where `frame_count` lives.
    #[test]
    fn audio_block_header_layout_is_frozen() {
        assert_eq!(size_of::<AudioBlockHeader>(), 272);
        assert_eq!(align_of::<AudioBlockHeader>(), 8);
        assert_eq!(offset_of!(AudioBlockHeader, magic), 0);
        assert_eq!(offset_of!(AudioBlockHeader, version), 4);
        assert_eq!(offset_of!(AudioBlockHeader, flags), 6);
        assert_eq!(offset_of!(AudioBlockHeader, sequence), 8);
        assert_eq!(offset_of!(AudioBlockHeader, instance), 16);
        assert_eq!(offset_of!(AudioBlockHeader, steady_time), 24);
        assert_eq!(offset_of!(AudioBlockHeader, frame_count), 32);
        assert_eq!(offset_of!(AudioBlockHeader, sample_format), 36);
        assert_eq!(offset_of!(AudioBlockHeader, input_channels), 40);
        assert_eq!(offset_of!(AudioBlockHeader, output_channels), 44);
        assert_eq!(offset_of!(AudioBlockHeader, channel_stride), 48);
        assert_eq!(offset_of!(AudioBlockHeader, input_offset), 56);
        assert_eq!(offset_of!(AudioBlockHeader, output_offset), 64);
        assert_eq!(offset_of!(AudioBlockHeader, input_event_offset), 72);
        assert_eq!(offset_of!(AudioBlockHeader, output_event_offset), 80);
        assert_eq!(offset_of!(AudioBlockHeader, blob_offset), 88);
        assert_eq!(offset_of!(AudioBlockHeader, input_event_capacity), 96);
        assert_eq!(offset_of!(AudioBlockHeader, input_event_count), 100);
        assert_eq!(offset_of!(AudioBlockHeader, output_event_capacity), 104);
        assert_eq!(offset_of!(AudioBlockHeader, output_event_count), 108);
        assert_eq!(offset_of!(AudioBlockHeader, blob_capacity), 112);
        assert_eq!(offset_of!(AudioBlockHeader, blob_used), 116);
        assert_eq!(offset_of!(AudioBlockHeader, constant_mask_in), 120);
        assert_eq!(offset_of!(AudioBlockHeader, constant_mask_out), 128);
        assert_eq!(offset_of!(AudioBlockHeader, transport), 136);
        assert_eq!(offset_of!(AudioBlockHeader, status), 232);
        assert_eq!(offset_of!(AudioBlockHeader, reserved0), 236);
        assert_eq!(offset_of!(AudioBlockHeader, reserved), 240);
    }

    #[test]
    fn transport_snapshot_layout_is_frozen_and_platform_independent() {
        assert_eq!(size_of::<TransportSnapshot>(), 96);
        assert_eq!(align_of::<TransportSnapshot>(), 8);
        assert_eq!(offset_of!(TransportSnapshot, flags), 0);
        assert_eq!(offset_of!(TransportSnapshot, bar_number), 4);
        assert_eq!(offset_of!(TransportSnapshot, song_pos_samples), 8);
        assert_eq!(offset_of!(TransportSnapshot, song_pos_beats), 16);
        assert_eq!(offset_of!(TransportSnapshot, song_pos_seconds), 24);
        assert_eq!(offset_of!(TransportSnapshot, tempo), 32);
        assert_eq!(offset_of!(TransportSnapshot, tempo_increment), 40);
        assert_eq!(offset_of!(TransportSnapshot, bar_start_beats), 48);
        assert_eq!(offset_of!(TransportSnapshot, loop_start_beats), 56);
        assert_eq!(offset_of!(TransportSnapshot, loop_end_beats), 64);
        assert_eq!(offset_of!(TransportSnapshot, loop_start_seconds), 72);
        assert_eq!(offset_of!(TransportSnapshot, loop_end_seconds), 80);
        assert_eq!(offset_of!(TransportSnapshot, time_sig_numerator), 88);
        assert_eq!(offset_of!(TransportSnapshot, time_sig_denominator), 90);
        assert_eq!(offset_of!(TransportSnapshot, reserved), 92);
        // Unlike DauxTransportV1, whose reserved tail is `[usize; 6]`, this one is the
        // same size on a 32-bit and a 64-bit peer.
        assert_eq!(TransportSnapshot::SIZE, 96);
    }

    #[test]
    fn event_record_is_exactly_one_cache_line() {
        assert_eq!(size_of::<EventRecord>(), 64);
        assert_eq!(align_of::<EventRecord>(), 8);
        assert_eq!(EventRecord::SIZE, 64);
        assert_eq!(offset_of!(EventRecord, time), 0);
        assert_eq!(offset_of!(EventRecord, kind), 4);
        assert_eq!(offset_of!(EventRecord, flags), 6);
        assert_eq!(offset_of!(EventRecord, port_index), 8);
        assert_eq!(offset_of!(EventRecord, payload_len), 10);
        assert_eq!(offset_of!(EventRecord, reserved), 12);
        assert_eq!(offset_of!(EventRecord, payload), 16);
        assert_eq!(16 + EVENT_PAYLOAD_BYTES, 64);
    }

    /// Padding bytes are uninitialised, and shipping uninitialised bytes into a region
    /// another process reads is both an information leak and undefined behaviour. Prove
    /// there are none by adding the field sizes up.
    #[test]
    fn layout_has_no_padding() {
        let header_fields = 4
            + 2
            + 2
            + 8
            + 8
            + 8
            + 4
            + 4
            + 4
            + 4
            + 8 * 6
            + 4 * 6
            + 8
            + 8
            + TransportSnapshot::SIZE
            + 4
            + 4
            + 32;
        assert_eq!(header_fields, AudioBlockHeader::SIZE);
        let transport_fields = 4 + 4 + 8 + 8 * 9 + 2 + 2 + 4;
        assert_eq!(transport_fields, TransportSnapshot::SIZE);
        let event_fields = 4 + 2 + 2 + 2 + 2 + 4 + EVENT_PAYLOAD_BYTES;
        assert_eq!(event_fields, EventRecord::SIZE);
    }

    #[test]
    fn a_header_round_trips_through_raw_bytes() {
        let mut h = AudioBlockHeader::new();
        h.sequence = 0x0102_0304_0506_0708;
        h.frame_count = 512;
        h.transport.tempo = 120.0;
        let bytes = h.as_bytes();
        assert_eq!(bytes.len(), AudioBlockHeader::SIZE);
        assert_eq!(AudioBlockHeader::read_from(bytes).unwrap(), h);
        // The magic really is at offset 0, in little-endian order.
        assert_eq!(&bytes[0..4], b"DXPA");
        assert_eq!(AUDIO_BLOCK_MAGIC, u32::from_le_bytes(*b"DXPA"));
    }

    #[test]
    fn reading_a_header_from_too_few_bytes_is_an_error_not_a_read_past_the_end() {
        let h = AudioBlockHeader::new();
        let bytes = h.as_bytes().to_vec();
        for n in 0..AudioBlockHeader::SIZE {
            assert!(matches!(
                AudioBlockHeader::read_from(&bytes[..n]).unwrap_err().kind(),
                ProtocolErrorKind::Truncated { .. }
            ));
        }
        assert!(AudioBlockHeader::read_from(&bytes).is_ok());
    }

    // ------------------------------------------------------------------- transport ---

    #[test]
    fn transport_converts_both_ways_without_losing_a_field() {
        let mut abi = DauxTransportV1::new();
        abi.flags = DAUX_TRANSPORT_HAS_TEMPO | DAUX_TRANSPORT_IS_PLAYING;
        abi.song_pos_samples = 44_100;
        abi.song_pos_beats = 4.0;
        abi.song_pos_seconds = 1.0;
        abi.tempo = 128.5;
        abi.tempo_increment = 0.001;
        abi.bar_start_beats = 4.0;
        abi.bar_number = 2;
        abi.time_sig_numerator = 7;
        abi.time_sig_denominator = 8;
        abi.loop_start_beats = 0.0;
        abi.loop_end_beats = 16.0;
        abi.loop_start_seconds = 0.0;
        abi.loop_end_seconds = 8.0;

        let snap = TransportSnapshot::from(abi);
        assert!(snap.has_flags(DAUX_TRANSPORT_HAS_TEMPO));
        assert!(!snap.has_flags(daux_abi::DAUX_TRANSPORT_HAS_LOOP));
        let back = DauxTransportV1::from(snap);
        assert_eq!(back.flags, abi.flags);
        assert_eq!(back.song_pos_samples, abi.song_pos_samples);
        assert!((back.tempo - abi.tempo).abs() < f64::EPSILON);
        assert_eq!(back.time_sig_numerator, abi.time_sig_numerator);
        assert_eq!(back.time_sig_denominator, abi.time_sig_denominator);
        assert!((back.loop_end_seconds - abi.loop_end_seconds).abs() < f64::EPSILON);
        assert_eq!(TransportSnapshot::from(back), snap);
        assert_eq!(TransportSnapshot::default(), TransportSnapshot::new());
    }

    // ---------------------------------------------------------------------- events ---

    fn every_payload() -> Vec<(u16, EventPayload)> {
        vec![
            (
                DAUX_EVENT_NOTE_ON,
                EventPayload::Note(NotePayload {
                    note_id: -1,
                    channel: 3,
                    key: 60,
                    velocity: 0.75,
                    tuning: -12.5,
                }),
            ),
            (
                DAUX_EVENT_NOTE_EXPRESSION,
                EventPayload::NoteExpression(NoteExpressionPayload {
                    expression_id: 2,
                    note_id: 9,
                    channel: -1,
                    key: 127,
                    value: 0.25,
                }),
            ),
            (
                DAUX_EVENT_PARAM_VALUE,
                EventPayload::Param(ParamPayload {
                    param_id: 0xDEAD_BEEF,
                    note_id: -1,
                    channel: -1,
                    key: -1,
                    value: -6.0,
                }),
            ),
            (DAUX_EVENT_MIDI1, EventPayload::Midi1([0x90, 60, 100])),
            (
                DAUX_EVENT_MIDI2,
                EventPayload::Midi2(Midi2Payload {
                    word_count: 2,
                    words: [0x4090_3C00, 0xFFFF_0000, 0, 0],
                }),
            ),
            (
                DAUX_EVENT_SYSEX,
                EventPayload::Blob(BlobRef {
                    offset: 128,
                    len: 9,
                }),
            ),
            (
                daux_abi::DAUX_EVENT_CUSTOM + 4,
                EventPayload::Blob(BlobRef { offset: 0, len: 0 }),
            ),
        ]
    }

    #[test]
    fn every_event_payload_round_trips_and_validates() {
        for (kind, payload) in every_payload() {
            let record = EventRecord::with_payload(7, kind, 1, payload).with_flags(1);
            record.validate().unwrap();
            assert_eq!(record.payload().unwrap(), payload, "kind {kind}");
            assert_eq!(record.time, 7);
            assert_eq!(record.port_index, 1);
            assert_eq!(record.flags, 1);
            assert_eq!(
                record.payload_len,
                EventRecord::payload_bytes_for(kind).unwrap()
            );
            // The unused tail is zero, so a record has exactly one encoding.
            assert!(
                record.payload[record.payload_len as usize..]
                    .iter()
                    .all(|b| *b == 0)
            );
        }
    }

    #[test]
    fn an_unknown_event_kind_is_rejected_rather_than_read_as_something_else() {
        let mut r = EventRecord::with_payload(0, DAUX_EVENT_MIDI1, 0, EventPayload::Midi1([0; 3]));
        r.kind = 0;
        assert_eq!(
            r.validate().unwrap_err().kind(),
            ProtocolErrorKind::InvalidValue
        );
        r.kind = 4242; // below the vendor range and unassigned
        assert!(r.validate().is_err());
        assert!(r.payload().is_err());
        // The vendor range is open, so anything at or above it decodes as a blob.
        r.kind = daux_abi::DAUX_EVENT_CUSTOM;
        r.payload_len = 8;
        assert!(matches!(r.payload().unwrap(), EventPayload::Blob(_)));
    }

    #[test]
    fn a_record_with_a_wrong_length_or_dirty_slack_is_rejected() {
        let base =
            EventRecord::with_payload(0, DAUX_EVENT_MIDI1, 0, EventPayload::Midi1([0x90, 60, 100]));
        let mut short = base;
        short.payload_len = 2;
        assert!(short.validate().is_err());
        let mut long = base;
        long.payload_len = 60;
        assert!(long.validate().is_err());
        let mut dirty = base;
        dirty.payload[40] = 1;
        assert!(dirty.validate().is_err());
        let mut reserved = base;
        reserved.reserved = 1;
        assert!(reserved.validate().is_err());
        assert!(base.validate().is_ok());
    }

    #[test]
    fn an_impossible_midi2_word_count_is_rejected() {
        for (count, words) in [
            (0u32, [0u32; 4]),
            (5, [0; 4]),
            (u32::MAX, [0; 4]),
            (1, [1, 2, 0, 0]), // words past word_count must be zero
        ] {
            let r = EventRecord::with_payload(
                0,
                DAUX_EVENT_MIDI2,
                0,
                EventPayload::Midi2(Midi2Payload {
                    word_count: count,
                    words,
                }),
            );
            assert!(r.validate().is_err(), "count {count} words {words:?}");
        }
        assert!(
            EventRecord::with_payload(
                0,
                DAUX_EVENT_MIDI2,
                0,
                EventPayload::Midi2(Midi2Payload {
                    word_count: 4,
                    words: [1, 2, 3, 4],
                })
            )
            .validate()
            .is_ok()
        );
    }

    #[test]
    fn a_default_record_is_not_mistaken_for_a_valid_event() {
        assert!(EventRecord::default().validate().is_err());
        assert_eq!(EventRecord::new(), EventRecord::default());
    }

    // ----------------------------------------------------------------- block layout ---

    #[test]
    fn a_layout_produces_a_header_that_validates_against_its_own_region() {
        let limits = ProtocolLimits::new();
        let layout = AudioBlockLayout::new(2, 2, 512);
        let len = layout.region_len(&limits).unwrap();
        let header = layout.header(42, &limits).unwrap();
        header.validate(len, &limits).unwrap();
        assert_eq!(header.instance, 42);
        assert_eq!(header.frame_count, 512);
        assert_eq!(header.channel_stride, 2048);
        assert_eq!(header.magic, AUDIO_BLOCK_MAGIC);
        assert_eq!(header.version, AUDIO_BLOCK_VERSION);
        // Every sub-region is cache-line aligned and clear of the header.
        for offset in [
            header.input_offset,
            header.output_offset,
            header.input_event_offset,
            header.output_event_offset,
            header.blob_offset,
        ] {
            assert_eq!(offset as usize % REGION_ALIGN, 0);
            assert!(offset as usize >= AudioBlockHeader::SIZE);
        }
        assert!(len >= AudioBlockHeader::SIZE);
    }

    #[test]
    fn a_layout_with_no_audio_and_no_events_still_produces_a_valid_region() {
        let limits = ProtocolLimits::new();
        let layout = AudioBlockLayout::new(0, 0, 1)
            .with_events(0, 0)
            .with_blob_bytes(0);
        let len = layout.region_len(&limits).unwrap();
        let header = layout.header(1, &limits).unwrap();
        header.validate(len, &limits).unwrap();
        assert_eq!(len, align_up(AudioBlockHeader::SIZE).unwrap());
    }

    #[test]
    fn double_precision_doubles_the_stride() {
        let limits = ProtocolLimits::new();
        let f32_len = AudioBlockLayout::new(2, 2, 512)
            .region_len(&limits)
            .unwrap();
        let f64_layout =
            AudioBlockLayout::new(2, 2, 512).with_sample_format(DAUX_SAMPLE_FORMAT_F64);
        assert_eq!(f64_layout.channel_stride().unwrap(), 4096);
        assert!(f64_layout.region_len(&limits).unwrap() > f32_len);
        assert_eq!(sample_bytes(DAUX_SAMPLE_FORMAT_F32), Some(4));
        assert_eq!(sample_bytes(DAUX_SAMPLE_FORMAT_F64), Some(8));
        assert_eq!(sample_bytes(0), None);
        assert_eq!(sample_bytes(3), None);
    }

    #[test]
    fn a_layout_outside_the_limits_is_refused() {
        let limits = ProtocolLimits::new();
        for layout in [
            AudioBlockLayout::new(2, 2, 0),
            AudioBlockLayout::new(2, 2, 70_000),
            AudioBlockLayout::new(1000, 2, 64),
            AudioBlockLayout::new(2, 1000, 64),
            AudioBlockLayout::new(2, 2, 64).with_events(u32::MAX, 8),
            AudioBlockLayout::new(2, 2, 64).with_events(8, u32::MAX),
            AudioBlockLayout::new(2, 2, 64).with_sample_format(0),
        ] {
            assert!(
                layout.region_len(&limits).is_err(),
                "{layout:?} should be refused"
            );
            assert!(layout.header(0, &limits).is_err());
        }
    }

    #[test]
    fn align_up_rounds_to_a_cache_line_and_refuses_to_wrap() {
        assert_eq!(align_up(0), Some(0));
        assert_eq!(align_up(1), Some(64));
        assert_eq!(align_up(64), Some(64));
        assert_eq!(align_up(65), Some(128));
        assert_eq!(align_up(usize::MAX), None);
    }

    // -------------------------------------------------------------------- validation ---

    fn valid_pair() -> (AudioBlockHeader, usize, ProtocolLimits) {
        let limits = ProtocolLimits::new();
        let layout = AudioBlockLayout::new(2, 2, 64);
        let len = layout.region_len(&limits).unwrap();
        (layout.header(1, &limits).unwrap(), len, limits)
    }

    #[test]
    fn a_header_claiming_more_than_the_region_holds_is_rejected() {
        let (header, len, limits) = valid_pair();
        assert!(header.validate(len, &limits).is_ok());
        // One byte short of what the layout needs.
        assert!(matches!(
            header.validate(len - 1, &limits).unwrap_err().kind(),
            ProtocolErrorKind::InvalidLayout
        ));
        assert!(header.validate(0, &limits).is_err());

        let mut past_end = header;
        past_end.blob_offset = u64::MAX - 8;
        assert!(past_end.validate(len, &limits).is_err());

        let mut huge = header;
        huge.channel_stride = u64::MAX;
        assert!(huge.validate(len, &limits).is_err());
    }

    #[test]
    fn a_header_whose_regions_overlap_is_rejected_unless_it_is_in_place() {
        let (header, len, limits) = valid_pair();
        let mut aliased = header;
        aliased.output_offset = aliased.input_offset;
        assert!(matches!(
            aliased.validate(len, &limits).unwrap_err().kind(),
            ProtocolErrorKind::InvalidLayout
        ));
        aliased.flags |= AUDIO_BLOCK_FLAG_IN_PLACE;
        assert!(aliased.validate(len, &limits).is_ok());

        // In-place processing excuses input/output aliasing only; the event arrays may
        // never share bytes with the samples.
        let mut events_over_audio = header;
        events_over_audio.flags |= AUDIO_BLOCK_FLAG_IN_PLACE;
        events_over_audio.input_event_offset = events_over_audio.input_offset;
        assert!(events_over_audio.validate(len, &limits).is_err());
    }

    #[test]
    fn a_header_that_overwrites_itself_is_rejected() {
        let (header, len, limits) = valid_pair();
        let mut over_header = header;
        over_header.input_offset = 8;
        assert!(over_header.validate(len, &limits).is_err());
    }

    #[test]
    fn corrupt_scalar_fields_are_all_rejected() {
        let (header, len, limits) = valid_pair();
        let mutations: [(&str, fn(&mut AudioBlockHeader)); 12] = [
            ("magic", |h| h.magic = 0),
            ("version", |h| h.version = 2),
            ("reserved0", |h| h.reserved0 = 1),
            ("reserved", |h| h.reserved[3] = 1),
            ("transport.reserved", |h| h.transport.reserved = 1),
            ("sample_format", |h| h.sample_format = 7),
            ("frame_count zero", |h| h.frame_count = 0),
            ("frame_count huge", |h| h.frame_count = u32::MAX),
            ("input_channels", |h| h.input_channels = u32::MAX),
            ("event_capacity", |h| h.input_event_capacity = u32::MAX),
            ("event_count", |h| h.output_event_count = 9999),
            ("blob_used", |h| h.blob_used = u32::MAX),
        ];
        for (name, mutate) in mutations {
            let mut h = header;
            mutate(&mut h);
            assert!(h.validate(len, &limits).is_err(), "{name} was accepted");
        }
        assert!(header.validate(len, &limits).is_ok());
    }

    #[test]
    fn a_stride_too_small_for_the_live_frame_count_is_rejected() {
        let (mut header, len, limits) = valid_pair();
        header.frame_count = 64;
        header.channel_stride = 4 * 63;
        assert!(header.validate(len, &limits).is_err());
        header.channel_stride = 4 * 64;
        assert!(header.validate(len, &limits).is_ok());
    }

    /// Every byte pattern must either validate or fail, and never panic. A header read
    /// straight out of a region a crashed peer left behind is exactly this.
    #[test]
    fn validating_arbitrary_bytes_never_panics() {
        let limits = ProtocolLimits::new();
        let mut bytes = vec![0u8; AudioBlockHeader::SIZE];
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        for round in 0..2000 {
            for b in &mut bytes {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                *b = (seed >> 32) as u8;
            }
            // Half the rounds start from a plausible header so the deeper checks are
            // actually reached rather than tripping on the magic every time.
            if round % 2 == 0 {
                let base = AudioBlockLayout::new(2, 2, 64).header(1, &limits).unwrap();
                bytes[..16].copy_from_slice(&base.as_bytes()[..16]);
            }
            let header = AudioBlockHeader::read_from(&bytes).unwrap();
            let _ = header.validate(4096, &limits);
            let _ = header.validate(0, &limits);
            let _ = header.validate(usize::MAX, &limits);
        }
    }

    /// The same, for event records: a corrupt region yields arbitrary bytes here too.
    #[test]
    fn validating_arbitrary_event_records_never_panics() {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        for _ in 0..4000 {
            let mut r = EventRecord::new();
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            r.time = seed as u32;
            r.kind = (seed >> 32) as u16;
            r.flags = (seed >> 48) as u16;
            r.payload_len = (seed >> 16) as u16;
            r.reserved = (seed >> 8) as u32;
            for (i, b) in r.payload.iter_mut().enumerate() {
                *b = (seed >> (i % 8 * 8)) as u8;
            }
            let _ = r.validate();
            let _ = r.payload();
        }
    }
}
