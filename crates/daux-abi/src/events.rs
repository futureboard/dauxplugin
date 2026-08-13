//! Events and the event list interface (`abi-v1` §9).
//!
//! Events are flat `#[repr(C)]` records with a common header, accessed through a
//! host-provided list interface. The list owns the storage; the plug-in MUST NOT retain
//! pointers past the end of `process`.
//!
//! Input events are delivered sorted by `time`, then by list order for equal timestamps.
//! Output events SHOULD be pushed in non-decreasing `time` order; hosts MUST sort
//! defensively.

use core::ffi::c_void;

use crate::compat::{impl_abi_default, impl_abi_struct};
use crate::status::DauxStatus;
use crate::transport::DauxTransportV1;

/// Header shared by every event record.
///
/// `size` is the byte size of the **whole** record, header included, which is what makes
/// the event stream forward compatible: a reader validates `size` before touching any
/// field of the concrete record type.
///
/// [audio-thread]
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DauxEventHeaderV1 {
    /// Total byte size of this event including the header.
    pub size: u32,
    /// Sample offset within the current block: `0 ..= frame_count - 1`.
    pub time: u32,
    /// One of the `DAUX_EVENT_*` constants.
    pub kind: u16,
    /// Bitset of `DAUX_EVENT_FLAG_*`.
    pub flags: u16,
    /// Which event port the event belongs to.
    pub port_index: u16,
    /// Reserved for alignment; MUST be zero.
    pub _pad0: u16,
}

impl DauxEventHeaderV1 {
    /// [audio-thread] An all-zero header.
    ///
    /// `size` is left at zero on purpose: only the concrete record type knows how many
    /// bytes follow, so each of them sets it in its own `new`.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            size: 0,
            time: 0,
            kind: 0,
            flags: 0,
            port_index: 0,
            _pad0: 0,
        }
    }

    /// [audio-thread] A header for a record of `size` bytes of the given kind.
    #[inline]
    #[must_use]
    pub const fn with(kind: u16, size: u32, time: u32) -> Self {
        Self {
            size,
            time,
            kind,
            flags: 0,
            port_index: 0,
            _pad0: 0,
        }
    }

    /// [audio-thread] `true` when every `DAUX_EVENT_FLAG_*` bit in `flags` is set.
    #[inline]
    #[must_use]
    pub const fn has_flags(&self, flags: u16) -> bool {
        self.flags & flags == flags
    }
}

impl_abi_struct!(DauxEventHeaderV1);
impl_abi_default!(DauxEventHeaderV1);

/// A note started. Payload: [`DauxEventNoteV1`].
pub const DAUX_EVENT_NOTE_ON: u16 = 1;
/// A note was released. Payload: [`DauxEventNoteV1`].
pub const DAUX_EVENT_NOTE_OFF: u16 = 2;
/// A voice must be silenced immediately. Payload: [`DauxEventNoteV1`].
pub const DAUX_EVENT_NOTE_CHOKE: u16 = 3;
/// Plug-in → host: a voice finished releasing. Payload: [`DauxEventNoteV1`].
pub const DAUX_EVENT_NOTE_END: u16 = 4;
/// Per-note expression change. Payload: [`DauxEventNoteExpressionV1`].
pub const DAUX_EVENT_NOTE_EXPRESSION: u16 = 5;
/// Absolute parameter value. Payload: [`DauxEventParamV1`].
pub const DAUX_EVENT_PARAM_VALUE: u16 = 6;
/// Signed parameter modulation offset. Payload: [`DauxEventParamV1`].
pub const DAUX_EVENT_PARAM_MOD: u16 = 7;
/// Start of a user gesture. Payload: [`DauxEventParamV1`].
pub const DAUX_EVENT_PARAM_GESTURE_BEGIN: u16 = 8;
/// End of a user gesture. Payload: [`DauxEventParamV1`].
pub const DAUX_EVENT_PARAM_GESTURE_END: u16 = 9;
/// Transport discontinuity. Payload: [`DauxEventTransportV1`].
pub const DAUX_EVENT_TRANSPORT: u16 = 10;
/// MIDI 1.0 message. Payload: [`DauxEventMidi1V1`].
pub const DAUX_EVENT_MIDI1: u16 = 11;
/// MIDI 2.0 Universal MIDI Packet. Payload: [`DauxEventMidi2V1`].
pub const DAUX_EVENT_MIDI2: u16 = 12;
/// System-exclusive bytes. Payload: [`DauxEventSysExV1`].
pub const DAUX_EVENT_SYSEX: u16 = 13;
/// First id of the vendor range; layouts above this value are vendor-defined.
pub const DAUX_EVENT_CUSTOM: u16 = 0x7000;

/// The event was performed live rather than played back from automation.
pub const DAUX_EVENT_FLAG_IS_LIVE: u16 = 1 << 0;
/// The host SHOULD NOT record this event.
pub const DAUX_EVENT_FLAG_DONT_RECORD: u16 = 1 << 1;

/// Note on / off / choke / end.
///
/// [audio-thread]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DauxEventNoteV1 {
    /// Common header; `kind` is one of `DAUX_EVENT_NOTE_*`.
    pub header: DauxEventHeaderV1,
    /// Host-assigned voice id, or `-1` when the host does not track voices.
    pub note_id: i32,
    /// MIDI channel, or `-1` as a wildcard.
    pub channel: i16,
    /// Key `0..=127`, or `-1` as a wildcard on note-off/choke.
    pub key: i16,
    /// Reserved for alignment; MUST be zero.
    pub _pad0: i32,
    /// Velocity, `0.0 ..= 1.0`.
    pub velocity: f64,
    /// Cents offset from equal temperament.
    pub tuning: f64,
}

/// Volume expression.
pub const DAUX_NOTE_EXPR_VOLUME: u32 = 0;
/// Pan expression.
pub const DAUX_NOTE_EXPR_PAN: u32 = 1;
/// Tuning expression, in cents.
pub const DAUX_NOTE_EXPR_TUNING: u32 = 2;
/// Vibrato depth.
pub const DAUX_NOTE_EXPR_VIBRATO: u32 = 3;
/// Generic expression controller.
pub const DAUX_NOTE_EXPR_EXPRESSION: u32 = 4;
/// Brightness / timbre.
pub const DAUX_NOTE_EXPR_BRIGHTNESS: u32 = 5;
/// Channel or poly pressure.
pub const DAUX_NOTE_EXPR_PRESSURE: u32 = 6;

/// Per-note expression change.
///
/// [audio-thread]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DauxEventNoteExpressionV1 {
    /// Common header; `kind` is [`DAUX_EVENT_NOTE_EXPRESSION`].
    pub header: DauxEventHeaderV1,
    /// One of the `DAUX_NOTE_EXPR_*` constants.
    pub expression_id: u32,
    /// Host-assigned voice id, or `-1`.
    pub note_id: i32,
    /// MIDI channel, or `-1` as a wildcard.
    pub channel: i16,
    /// Key `0..=127`, or `-1` as a wildcard.
    pub key: i16,
    /// Reserved for alignment; MUST be zero.
    pub _pad0: u32,
    /// Expression value; range depends on `expression_id`.
    pub value: f64,
}

/// Parameter value, modulation or gesture boundary.
///
/// Values crossing the ABI are always **plain** (real-world) values, never normalised.
///
/// [audio-thread]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DauxEventParamV1 {
    /// Common header; `kind` is one of `DAUX_EVENT_PARAM_*`.
    pub header: DauxEventHeaderV1,
    /// Permanent parameter id (§14).
    pub param_id: u32,
    /// `-1` unless the change is scoped to a single voice.
    pub note_id: i32,
    /// MIDI channel, or `-1` as a wildcard.
    pub channel: i16,
    /// Key `0..=127`, or `-1` as a wildcard.
    pub key: i16,
    /// Reserved for alignment; MUST be zero.
    pub _pad0: u32,
    /// Absolute plain value for `PARAM_VALUE`; signed offset for `PARAM_MOD`.
    pub value: f64,
    /// Opaque host cookie, echoed back on output events. May be null.
    pub cookie: *mut c_void,
}

/// A MIDI 1.0 message.
///
/// [audio-thread]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DauxEventMidi1V1 {
    /// Common header; `kind` is [`DAUX_EVENT_MIDI1`].
    pub header: DauxEventHeaderV1,
    /// Status byte followed by up to two data bytes, zero-padded.
    pub data: [u8; 3],
    /// Reserved for alignment; MUST be zero.
    pub _pad0: u8,
}

/// One MIDI 2.0 Universal MIDI Packet, 1–4 words, `word_count` valid words.
///
/// [audio-thread]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DauxEventMidi2V1 {
    /// Common header; `kind` is [`DAUX_EVENT_MIDI2`].
    pub header: DauxEventHeaderV1,
    /// Number of valid words in `words`, `1 ..= 4`.
    pub word_count: u32,
    /// The packet words; entries at and beyond `word_count` MUST be zero.
    pub words: [u32; 4],
}

/// System-exclusive bytes, borrowed from the event list and valid only during `process`.
///
/// [audio-thread]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DauxEventSysExV1 {
    /// Common header; `kind` is [`DAUX_EVENT_SYSEX`].
    pub header: DauxEventHeaderV1,
    /// Number of bytes at `bytes`.
    pub byte_count: u32,
    /// Reserved for alignment; MUST be zero.
    pub _pad0: u32,
    /// Borrowed payload; valid only for the duration of the current `process` call.
    pub bytes: *const u8,
}

/// A transport discontinuity (locate, loop wrap, tempo jump) at a sample-accurate offset.
///
/// `abi-v1` §10 specifies that a [`DAUX_EVENT_TRANSPORT`] event "carries a
/// [`DauxTransportV1`] immediately after its header"; this structure is that layout. The
/// header is 16 bytes and 8-byte aligned, so the embedded transport starts at offset 16
/// with no implicit padding.
///
/// [audio-thread]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DauxEventTransportV1 {
    /// Common header; `kind` is [`DAUX_EVENT_TRANSPORT`].
    pub header: DauxEventHeaderV1,
    /// Transport state in effect from `header.time` onwards.
    pub transport: DauxTransportV1,
}

/// Defines `new()`/`empty()`/`Default` for a concrete event record.
macro_rules! event_record {
    ($ty:ty, $kind:expr) => {
        impl $ty {
            /// [audio-thread] An all-zero record with `header.size` and `header.kind` set.
            #[inline]
            #[must_use]
            pub const fn new() -> Self {
                // SAFETY: every field of this record is a plain integer, float, byte
                // array or raw pointer (`cookie`/`bytes`, for which null is the
                // specified "absent" value), or a `#[repr(C)]` aggregate of those. No
                // field is a reference, function pointer or enum, so the all-zero bit
                // pattern is a valid, fully initialised value with no niche violated.
                // Zeroing also clears the implicit padding this layout carries, keeping
                // the bytes that cross the boundary deterministic.
                let mut this: Self = unsafe { core::mem::zeroed() };
                this.header.size = <$ty>::SIZE;
                this.header.kind = $kind;
                this
            }

            /// [audio-thread] Alias of [`Self::new`].
            #[inline]
            #[must_use]
            pub const fn empty() -> Self {
                Self::new()
            }
        }

        impl Default for $ty {
            #[inline]
            fn default() -> Self {
                Self::new()
            }
        }
    };
}

event_record!(DauxEventNoteV1, DAUX_EVENT_NOTE_ON);
event_record!(DauxEventNoteExpressionV1, DAUX_EVENT_NOTE_EXPRESSION);
event_record!(DauxEventParamV1, DAUX_EVENT_PARAM_VALUE);
event_record!(DauxEventMidi1V1, DAUX_EVENT_MIDI1);
event_record!(DauxEventMidi2V1, DAUX_EVENT_MIDI2);
event_record!(DauxEventSysExV1, DAUX_EVENT_SYSEX);
event_record!(DauxEventTransportV1, DAUX_EVENT_TRANSPORT);

impl_abi_struct!(header:
    DauxEventNoteV1,
    DauxEventNoteExpressionV1,
    DauxEventParamV1,
    DauxEventMidi1V1,
    DauxEventMidi2V1,
    DauxEventSysExV1,
    DauxEventTransportV1,
);

/// Borrowed, host-owned list of events for one block.
///
/// The same table serves input and output: `count`/`get` read, `push` appends.
///
/// [audio-thread]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DauxEventListV1 {
    /// `size_of::<DauxEventListV1>()` as written by the producer.
    pub size: u32,
    /// Reserved for alignment; MUST be zero.
    pub _pad0: u32,
    /// Opaque context passed back to every entry below.
    pub ctx: *mut c_void,

    /// Number of events. [audio-thread]
    pub count: unsafe extern "C" fn(ctx: *mut c_void) -> u32,

    /// Borrowed event at `index`, or null. The pointed-to record is valid until the
    /// current `process` returns. [audio-thread]
    pub get: unsafe extern "C" fn(ctx: *mut c_void, index: u32) -> *const DauxEventHeaderV1,

    /// Appends a copy of `event`. Returns
    /// [`DAUX_ERR_OUT_OF_MEMORY`](crate::DAUX_ERR_OUT_OF_MEMORY) when the bounded output
    /// queue is full — this is a normal, non-fatal condition and the caller MUST NOT
    /// allocate to work around it. [audio-thread]
    pub push: unsafe extern "C" fn(ctx: *mut c_void, event: *const DauxEventHeaderV1) -> DauxStatus,

    /// Reserved for future minor revisions; MUST be all zero.
    pub reserved: [usize; 4],
}

impl_abi_struct!(DauxEventListV1);
