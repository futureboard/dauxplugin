//! CLAP event lists ↔ the DAUx event model.
//!
//! The two models line up closely — both are sample-accurate, both are sorted by time, both
//! carry notes, per-note expression, parameter values and modulation, gestures, transport,
//! MIDI 1.0, SysEx and UMP — so this is a translation and not a redesign. What it must not
//! do is reorder: CLAP guarantees the input list is sorted by `time` and that events sharing
//! a timestamp keep the order the host queued them in, and abi-v1 §9 requires exactly the
//! same. Both directions therefore walk indices in order and never sort.
//!
//! # Hostile input
//!
//! Everything here reads out of a host's memory, so nothing is trusted:
//!
//! * `header.size` is checked against the concrete struct before it is read; a host that
//!   under-reports a size gets that event skipped, not a read past the end of its arena;
//! * every read is [`core::ptr::read_unaligned`], because a host's event arena is only
//!   guaranteed to be packed, not aligned to 8;
//! * events in an unknown space, of an unknown type, or with an unknown expression id are
//!   skipped rather than guessed at;
//! * a SysEx payload with a null pointer becomes an empty slice, never a dangling one.
//!
//! # Real-time behaviour
//!
//! Decoding produces `Copy` values and borrowed slices; encoding builds one CLAP struct on
//! the stack. Neither allocates. `[audio-thread]`

use core::marker::PhantomData;
use core::ptr;

use daux_plugin_api::{
    DauxEvent, EventFlags, EventHeader, EventOverflow, InputEvents, Midi1Event, Midi1Message,
    Midi2Event, NoteEvent, NoteExpression, NoteExpressionEvent, OutputEvents, ParamEvent,
    ParamGestureEvent, SysExEvent, Transport, TransportEvent, Ump,
};

use crate::abi::{
    CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_DONT_RECORD, CLAP_EVENT_IS_LIVE, CLAP_EVENT_MIDI,
    CLAP_EVENT_MIDI_SYSEX, CLAP_EVENT_MIDI2, CLAP_EVENT_NOTE_CHOKE, CLAP_EVENT_NOTE_END,
    CLAP_EVENT_NOTE_EXPRESSION, CLAP_EVENT_NOTE_OFF, CLAP_EVENT_NOTE_ON,
    CLAP_EVENT_PARAM_GESTURE_BEGIN, CLAP_EVENT_PARAM_GESTURE_END, CLAP_EVENT_PARAM_MOD,
    CLAP_EVENT_PARAM_VALUE, CLAP_EVENT_TRANSPORT, CLAP_NOTE_EXPRESSION_BRIGHTNESS,
    CLAP_NOTE_EXPRESSION_EXPRESSION, CLAP_NOTE_EXPRESSION_PAN, CLAP_NOTE_EXPRESSION_PRESSURE,
    CLAP_NOTE_EXPRESSION_TUNING, CLAP_NOTE_EXPRESSION_VIBRATO, CLAP_NOTE_EXPRESSION_VOLUME,
    ClapEventHeader, ClapEventMidi, ClapEventMidi2, ClapEventMidiSysex, ClapEventNote,
    ClapEventNoteExpression, ClapEventParamGesture, ClapEventParamMod, ClapEventParamValue,
    ClapEventTransport, ClapInputEvents, ClapOutputEvents,
};
use crate::transport::{transport_from_clap, transport_to_clap};

/// `[audio-thread]` DAUx event flags from CLAP's.
fn flags_from_clap(bits: u32) -> EventFlags {
    let mut flags = EventFlags::NONE;
    if bits & CLAP_EVENT_IS_LIVE != 0 {
        flags |= EventFlags::IS_LIVE;
    }
    if bits & CLAP_EVENT_DONT_RECORD != 0 {
        flags |= EventFlags::DONT_RECORD;
    }
    flags
}

/// `[audio-thread]` CLAP event flags from DAUx's.
fn flags_to_clap(flags: EventFlags) -> u32 {
    let mut bits = 0;
    if flags.is_live() {
        bits |= CLAP_EVENT_IS_LIVE;
    }
    if flags.dont_record() {
        bits |= CLAP_EVENT_DONT_RECORD;
    }
    bits
}

/// `[audio-thread]` A DAUx header from a CLAP one, folding the note port index in.
///
/// CLAP carries the port on the concrete event and allows `-1` as a wildcard; DAUx carries
/// it on the header as a `u16`. A wildcard becomes port `0`, which is the only port a
/// single-port plug-in has and the main port of a multi-port one.
fn header_from_clap(h: &ClapEventHeader, port_index: i16) -> EventHeader {
    EventHeader::new(
        h.time,
        u16::try_from(port_index).unwrap_or(0),
        flags_from_clap(h.flags),
    )
}

/// `[audio-thread]` A CLAP header for an event this plug-in produced.
fn header_to_clap(h: EventHeader, type_: u16, size: usize) -> ClapEventHeader {
    ClapEventHeader {
        size: size as u32,
        time: h.time,
        space_id: CLAP_CORE_EVENT_SPACE_ID,
        type_,
        flags: flags_to_clap(h.flags),
    }
}

/// `[audio-thread]` The CLAP note port index for a DAUx header, clamped into `i16`.
fn port_to_clap(h: EventHeader) -> i16 {
    i16::try_from(h.port_index).unwrap_or(i16::MAX)
}

/// `[audio-thread]` A CLAP `u16` note port index as the `i16` the note events use.
const fn clamp_port(port: u16) -> i16 {
    if port > i16::MAX as u16 {
        i16::MAX
    } else {
        port as i16
    }
}

/// `[audio-thread]` A DAUx note-expression dimension from CLAP's `expression_id`.
const fn expression_from_clap(id: i32) -> Option<NoteExpression> {
    Some(match id {
        CLAP_NOTE_EXPRESSION_VOLUME => NoteExpression::Volume,
        CLAP_NOTE_EXPRESSION_PAN => NoteExpression::Pan,
        CLAP_NOTE_EXPRESSION_TUNING => NoteExpression::Tuning,
        CLAP_NOTE_EXPRESSION_VIBRATO => NoteExpression::Vibrato,
        CLAP_NOTE_EXPRESSION_EXPRESSION => NoteExpression::Expression,
        CLAP_NOTE_EXPRESSION_BRIGHTNESS => NoteExpression::Brightness,
        CLAP_NOTE_EXPRESSION_PRESSURE => NoteExpression::Pressure,
        _ => return None,
    })
}

/// `[audio-thread]` CLAP's `expression_id` from a DAUx dimension.
const fn expression_to_clap(e: NoteExpression) -> i32 {
    match e {
        NoteExpression::Volume => CLAP_NOTE_EXPRESSION_VOLUME,
        NoteExpression::Pan => CLAP_NOTE_EXPRESSION_PAN,
        NoteExpression::Tuning => CLAP_NOTE_EXPRESSION_TUNING,
        NoteExpression::Vibrato => CLAP_NOTE_EXPRESSION_VIBRATO,
        NoteExpression::Expression => CLAP_NOTE_EXPRESSION_EXPRESSION,
        NoteExpression::Brightness => CLAP_NOTE_EXPRESSION_BRIGHTNESS,
        NoteExpression::Pressure => CLAP_NOTE_EXPRESSION_PRESSURE,
    }
}

/// Reads a `T` out of a CLAP event, refusing to read more than the header says is there.
///
/// # Safety
///
/// `p` must point at a live `ClapEventHeader` followed by at least `header.size` readable
/// bytes, all owned by the host for the duration of the call. `header` must be the value
/// already read from `p`.
unsafe fn read_event<T>(p: *const ClapEventHeader, header: &ClapEventHeader) -> Option<T> {
    if (header.size as usize) < size_of::<T>() {
        return None;
    }
    // SAFETY: the caller guarantees `header.size` readable bytes at `p`, and the check above
    // proves that is at least `size_of::<T>()`. `read_unaligned` is used because a host's
    // event arena is only packed, not aligned to `T`'s alignment. `T` is always a `Copy`
    // `#[repr(C)]` CLAP struct, so a bitwise read is a valid value of it.
    Some(unsafe { ptr::read_unaligned(p.cast::<T>()) })
}

/// The host's input event list for one block, seen as [`InputEvents`].
///
/// Holds the raw pointer rather than a reference so the lifetime is stated explicitly: it is
/// the duration of one `process` or `flush` call and nothing longer (abi-v1 §16.3).
pub struct ClapInputList<'a> {
    /// The host's list, or null for "no events at all".
    list: *const ClapInputEvents,
    /// Needed to turn a transport event's seconds timeline into a sample position.
    sample_rate: f64,
    /// Ties this view to the call that produced it.
    _marker: PhantomData<&'a ClapInputEvents>,
}

impl core::fmt::Debug for ClapInputList<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ClapInputList")
            .field("len", &self.len())
            .finish_non_exhaustive()
    }
}

impl<'a> ClapInputList<'a> {
    /// `[audio-thread]` Wraps a host list.
    ///
    /// # Safety
    ///
    /// `list` must be null, or point to a `clap_input_events` whose `size`/`get` callbacks
    /// stay callable and whose events stay readable for the whole of `'a`.
    #[must_use]
    pub const unsafe fn new(list: *const ClapInputEvents, sample_rate: f64) -> Self {
        Self {
            list,
            sample_rate,
            _marker: PhantomData,
        }
    }

    /// `[audio-thread]` An empty list, for a host that passed none.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            list: ptr::null(),
            sample_rate: 0.0,
            _marker: PhantomData,
        }
    }

    /// `[audio-thread]` The raw header at `index`, or null.
    fn raw(&self, index: usize) -> *const ClapEventHeader {
        let Ok(index) = u32::try_from(index) else {
            return ptr::null();
        };
        // SAFETY: `new`'s contract makes `self.list` either null or a live
        // `clap_input_events` for `'a`, so reading the `get` slot out of it is sound, and
        // calling `get` is what the slot is for. A host that left `get` null is tolerated
        // rather than jumped to.
        unsafe {
            let Some(list) = self.list.as_ref() else {
                return ptr::null();
            };
            let Some(get) = list.get else {
                return ptr::null();
            };
            get(self.list, index)
        }
    }
}

impl InputEvents for ClapInputList<'_> {
    fn len(&self) -> usize {
        // SAFETY: as in `raw` — the pointer is null or a live list for `'a`, and a null
        // `size` slot is tolerated rather than called.
        unsafe {
            let Some(list) = self.list.as_ref() else {
                return 0;
            };
            let Some(size) = list.size else {
                return 0;
            };
            size(self.list) as usize
        }
    }

    fn get(&self, index: usize) -> Option<DauxEvent<'_>> {
        let p = self.raw(index);
        if p.is_null() {
            return None;
        }
        // SAFETY: `p` came from the host's `get`, which returns either null (rejected just
        // above) or a pointer to a live event whose header is readable. `read_unaligned`
        // makes no alignment demand of the host's arena.
        let header = unsafe { ptr::read_unaligned(p) };
        if header.space_id != CLAP_CORE_EVENT_SPACE_ID {
            return None;
        }
        if (header.size as usize) < size_of::<ClapEventHeader>() {
            return None;
        }

        match header.type_ {
            CLAP_EVENT_NOTE_ON
            | CLAP_EVENT_NOTE_OFF
            | CLAP_EVENT_NOTE_CHOKE
            | CLAP_EVENT_NOTE_END => {
                // SAFETY: `p` points at a live event with at least `header.size` readable
                // bytes, which `read_event` checks against the struct it reads.
                let e: ClapEventNote = unsafe { read_event(p, &header) }?;
                let note = NoteEvent {
                    header: header_from_clap(&header, e.port_index),
                    note_id: e.note_id,
                    channel: e.channel,
                    key: e.key,
                    velocity: e.velocity,
                    // CLAP carries per-note tuning as a note expression, never on the note
                    // itself, so a decoded note always starts at equal temperament.
                    tuning: 0.0,
                };
                Some(match header.type_ {
                    CLAP_EVENT_NOTE_ON => DauxEvent::NoteOn(note),
                    CLAP_EVENT_NOTE_OFF => DauxEvent::NoteOff(note),
                    CLAP_EVENT_NOTE_CHOKE => DauxEvent::NoteChoke(note),
                    _ => DauxEvent::NoteEnd(note),
                })
            }
            CLAP_EVENT_NOTE_EXPRESSION => {
                // SAFETY: as above.
                let e: ClapEventNoteExpression = unsafe { read_event(p, &header) }?;
                Some(DauxEvent::NoteExpression(NoteExpressionEvent {
                    header: header_from_clap(&header, e.port_index),
                    expression: expression_from_clap(e.expression_id)?,
                    note_id: e.note_id,
                    channel: e.channel,
                    key: e.key,
                    value: e.value,
                }))
            }
            CLAP_EVENT_PARAM_VALUE => {
                // SAFETY: as above.
                let e: ClapEventParamValue = unsafe { read_event(p, &header) }?;
                Some(DauxEvent::ParamValue(ParamEvent {
                    header: header_from_clap(&header, e.port_index),
                    param_id: e.param_id,
                    note_id: e.note_id,
                    channel: e.channel,
                    key: e.key,
                    value: e.value,
                }))
            }
            CLAP_EVENT_PARAM_MOD => {
                // SAFETY: as above.
                let e: ClapEventParamMod = unsafe { read_event(p, &header) }?;
                Some(DauxEvent::ParamMod(ParamEvent {
                    header: header_from_clap(&header, e.port_index),
                    param_id: e.param_id,
                    note_id: e.note_id,
                    channel: e.channel,
                    key: e.key,
                    value: e.amount,
                }))
            }
            CLAP_EVENT_PARAM_GESTURE_BEGIN | CLAP_EVENT_PARAM_GESTURE_END => {
                // SAFETY: as above.
                let e: ClapEventParamGesture = unsafe { read_event(p, &header) }?;
                let gesture = ParamGestureEvent {
                    header: header_from_clap(&header, 0),
                    param_id: e.param_id,
                };
                Some(if header.type_ == CLAP_EVENT_PARAM_GESTURE_BEGIN {
                    DauxEvent::ParamGestureBegin(gesture)
                } else {
                    DauxEvent::ParamGestureEnd(gesture)
                })
            }
            CLAP_EVENT_TRANSPORT => {
                // SAFETY: as above.
                let e: ClapEventTransport = unsafe { read_event(p, &header) }?;
                Some(DauxEvent::Transport(TransportEvent {
                    header: header_from_clap(&header, 0),
                    transport: transport_from_clap(&e, self.sample_rate).into(),
                }))
            }
            CLAP_EVENT_MIDI => {
                // SAFETY: as above.
                let e: ClapEventMidi = unsafe { read_event(p, &header) }?;
                Some(DauxEvent::Midi1(Midi1Event {
                    header: header_from_clap(&header, clamp_port(e.port_index)),
                    message: Midi1Message::new(e.data),
                }))
            }
            CLAP_EVENT_MIDI2 => {
                // SAFETY: as above.
                let e: ClapEventMidi2 = unsafe { read_event(p, &header) }?;
                // CLAP always carries four words; how many of them are meaningful follows
                // from the UMP message type in the top nibble.
                let message_type = (e.data[0] >> 28) as u8;
                let packet = Ump::try_new(e.data, Ump::words_for_message_type(message_type))?;
                Some(DauxEvent::Midi2(Midi2Event {
                    header: header_from_clap(&header, clamp_port(e.port_index)),
                    packet,
                }))
            }
            CLAP_EVENT_MIDI_SYSEX => {
                // SAFETY: as above.
                let e: ClapEventMidiSysex = unsafe { read_event(p, &header) }?;
                let bytes: &[u8] = if e.buffer.is_null() || e.size == 0 {
                    &[]
                } else {
                    // SAFETY: CLAP requires the payload to be readable for `size` bytes for
                    // the duration of the call, and the slice borrows `self`, whose `'a`
                    // was chosen by `new` to be exactly that call.
                    unsafe { core::slice::from_raw_parts(e.buffer, e.size as usize) }
                };
                Some(DauxEvent::SysEx(SysExEvent {
                    header: header_from_clap(&header, clamp_port(e.port_index)),
                    bytes,
                }))
            }
            // Anything else — an event from a newer CLAP, or one this adapter does not
            // translate — is skipped. Skipping is the conforming answer: abi-v1 §18 and
            // CLAP both require an unknown event to be ignored, never to abort the block.
            _ => None,
        }
    }
}

/// One encoded CLAP event, built on the stack just long enough to hand to the host.
///
/// A fixed-size enum rather than a byte buffer, so the compiler still checks that every
/// variant is the struct its `type_` claims. The largest variant is `ClapEventTransport` at
/// 104 bytes, which is why this is affordable to build per event with no allocation.
enum Encoded {
    /// A note on / off / choke / end.
    Note(ClapEventNote),
    /// A per-note expression change.
    Expression(ClapEventNoteExpression),
    /// An absolute parameter value.
    ParamValue(ClapEventParamValue),
    /// A parameter modulation offset.
    ParamMod(ClapEventParamMod),
    /// A gesture begin or end.
    Gesture(ClapEventParamGesture),
    /// A transport discontinuity.
    Transport(ClapEventTransport),
    /// A MIDI 1.0 message.
    Midi(ClapEventMidi),
    /// A Universal MIDI Packet.
    Midi2(ClapEventMidi2),
    /// A SysEx message, pointing at the caller's borrowed payload.
    SysEx(ClapEventMidiSysex),
}

impl Encoded {
    /// `[audio-thread]` The header pointer to hand to `try_push`.
    ///
    /// Every variant is `#[repr(C)]` with `header` first, so the struct's address is its
    /// header's address.
    fn header(&self) -> *const ClapEventHeader {
        match self {
            Encoded::Note(e) => ptr::from_ref(e).cast(),
            Encoded::Expression(e) => ptr::from_ref(e).cast(),
            Encoded::ParamValue(e) => ptr::from_ref(e).cast(),
            Encoded::ParamMod(e) => ptr::from_ref(e).cast(),
            Encoded::Gesture(e) => ptr::from_ref(e).cast(),
            Encoded::Transport(e) => ptr::from_ref(e).cast(),
            Encoded::Midi(e) => ptr::from_ref(e).cast(),
            Encoded::Midi2(e) => ptr::from_ref(e).cast(),
            Encoded::SysEx(e) => ptr::from_ref(e).cast(),
        }
    }
}

/// `[audio-thread]` The CLAP event type code for a note variant.
const fn note_type_code(e: &DauxEvent<'_>) -> Option<u16> {
    Some(match e {
        DauxEvent::NoteOn(_) => CLAP_EVENT_NOTE_ON,
        DauxEvent::NoteOff(_) => CLAP_EVENT_NOTE_OFF,
        DauxEvent::NoteChoke(_) => CLAP_EVENT_NOTE_CHOKE,
        DauxEvent::NoteEnd(_) => CLAP_EVENT_NOTE_END,
        _ => return None,
    })
}

/// `[audio-thread]` Encodes a DAUx event as the CLAP struct that carries it.
///
/// Returns `None` for [`DauxEvent::Custom`]: CLAP's vendor events need a registered
/// `space_id`, and inventing one would put an event no host can interpret into a user's
/// recording lane. Refusing is the honest answer, and the caller reports it as an overflow.
fn encode(e: &DauxEvent<'_>) -> Option<Encoded> {
    let h = e.header();
    Some(match *e {
        DauxEvent::NoteOn(n)
        | DauxEvent::NoteOff(n)
        | DauxEvent::NoteChoke(n)
        | DauxEvent::NoteEnd(n) => Encoded::Note(ClapEventNote {
            header: header_to_clap(h, note_type_code(e)?, size_of::<ClapEventNote>()),
            note_id: n.note_id,
            port_index: port_to_clap(h),
            channel: n.channel,
            key: n.key,
            velocity: n.velocity,
        }),
        DauxEvent::NoteExpression(x) => Encoded::Expression(ClapEventNoteExpression {
            header: header_to_clap(
                h,
                CLAP_EVENT_NOTE_EXPRESSION,
                size_of::<ClapEventNoteExpression>(),
            ),
            expression_id: expression_to_clap(x.expression),
            note_id: x.note_id,
            port_index: port_to_clap(h),
            channel: x.channel,
            key: x.key,
            value: x.value,
        }),
        DauxEvent::ParamValue(p) => Encoded::ParamValue(ClapEventParamValue {
            header: header_to_clap(h, CLAP_EVENT_PARAM_VALUE, size_of::<ClapEventParamValue>()),
            param_id: p.param_id,
            cookie: ptr::null_mut(),
            note_id: p.note_id,
            port_index: port_to_clap(h),
            channel: p.channel,
            key: p.key,
            value: p.value,
        }),
        DauxEvent::ParamMod(p) => Encoded::ParamMod(ClapEventParamMod {
            header: header_to_clap(h, CLAP_EVENT_PARAM_MOD, size_of::<ClapEventParamMod>()),
            param_id: p.param_id,
            cookie: ptr::null_mut(),
            note_id: p.note_id,
            port_index: port_to_clap(h),
            channel: p.channel,
            key: p.key,
            amount: p.value,
        }),
        DauxEvent::ParamGestureBegin(g) => Encoded::Gesture(ClapEventParamGesture {
            header: header_to_clap(
                h,
                CLAP_EVENT_PARAM_GESTURE_BEGIN,
                size_of::<ClapEventParamGesture>(),
            ),
            param_id: g.param_id,
        }),
        DauxEvent::ParamGestureEnd(g) => Encoded::Gesture(ClapEventParamGesture {
            header: header_to_clap(
                h,
                CLAP_EVENT_PARAM_GESTURE_END,
                size_of::<ClapEventParamGesture>(),
            ),
            param_id: g.param_id,
        }),
        DauxEvent::Transport(t) => {
            let transport = Transport::from(t.transport);
            Encoded::Transport(transport_to_clap(&transport, h.time))
        }
        DauxEvent::Midi1(m) => Encoded::Midi(ClapEventMidi {
            header: header_to_clap(h, CLAP_EVENT_MIDI, size_of::<ClapEventMidi>()),
            port_index: h.port_index,
            data: m.message.bytes,
        }),
        DauxEvent::Midi2(m) => Encoded::Midi2(ClapEventMidi2 {
            header: header_to_clap(h, CLAP_EVENT_MIDI2, size_of::<ClapEventMidi2>()),
            port_index: h.port_index,
            data: m.packet.words,
        }),
        DauxEvent::SysEx(s) => Encoded::SysEx(ClapEventMidiSysex {
            header: header_to_clap(h, CLAP_EVENT_MIDI_SYSEX, size_of::<ClapEventMidiSysex>()),
            port_index: h.port_index,
            buffer: s.bytes.as_ptr(),
            size: u32::try_from(s.bytes.len()).unwrap_or(u32::MAX),
        }),
        DauxEvent::Custom(_) => return None,
    })
}

/// The host's output event sink for one block, seen as [`OutputEvents`].
pub struct ClapOutputList<'a> {
    /// The host's sink, or null for "there is nowhere to send events".
    list: *const ClapOutputEvents,
    /// Ties this view to the call that produced it.
    _marker: PhantomData<&'a ClapOutputEvents>,
}

impl core::fmt::Debug for ClapOutputList<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ClapOutputList")
            .field("connected", &!self.list.is_null())
            .finish_non_exhaustive()
    }
}

impl<'a> ClapOutputList<'a> {
    /// `[audio-thread]` Wraps a host sink.
    ///
    /// # Safety
    ///
    /// `list` must be null, or point to a `clap_output_events` whose `try_push` callback
    /// stays callable for the whole of `'a`.
    #[must_use]
    pub const unsafe fn new(list: *const ClapOutputEvents) -> Self {
        Self {
            list,
            _marker: PhantomData,
        }
    }

    /// `[audio-thread]` A sink with nowhere to send, for a host that passed none.
    #[must_use]
    pub const fn discarding() -> Self {
        Self {
            list: ptr::null(),
            _marker: PhantomData,
        }
    }
}

impl OutputEvents for ClapOutputList<'_> {
    fn try_push(&mut self, e: &DauxEvent<'_>) -> Result<(), EventOverflow> {
        // An event CLAP cannot carry is reported as an overflow rather than as success: the
        // plug-in asked for it to reach the host and it did not.
        let Some(encoded) = encode(e) else {
            return Err(EventOverflow);
        };
        // SAFETY: `new`'s contract makes `self.list` null or a live `clap_output_events`
        // for `'a`. `encoded` lives until the end of this statement, which outlives the
        // call — CLAP requires the host to copy whatever it wants before returning. A null
        // pointer or a null `try_push` slot is treated as a full sink rather than jumped to.
        let pushed = unsafe {
            let Some(list) = self.list.as_ref() else {
                return Err(EventOverflow);
            };
            let Some(try_push) = list.try_push else {
                return Err(EventOverflow);
            };
            try_push(self.list, encoded.header())
        };
        if pushed { Ok(()) } else { Err(EventOverflow) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use daux_plugin_api::{CustomEvent, EventBuffer, TransportBuilder, kind};
    use std::cell::RefCell;

    // ---- a fake host input list -------------------------------------------------------

    /// Byte blobs plus the `clap_input_events` that reads them, wired together through the
    /// `ctx` slot exactly as a DAW does.
    struct FakeInput {
        /// Owns the bytes the plug-in will read.
        ///
        /// Boxed on purpose: `ctx` is a raw pointer to this vector, so its address has to
        /// survive the `FakeInput` being moved — which is exactly what the extra
        /// indirection buys and why `clippy::box_collection` is wrong here.
        #[allow(clippy::box_collection)]
        blobs: Box<Vec<Vec<u8>>>,
        /// The view handed to the adapter.
        view: ClapInputEvents,
    }

    unsafe extern "C" fn fake_size(list: *const ClapInputEvents) -> u32 {
        // SAFETY: the adapter only ever passes back the pointer it was given, and
        // `FakeInput` set `ctx` to a live `Vec<Vec<u8>>` that outlives the view.
        let blobs = unsafe { &*(*list).ctx.cast::<Vec<Vec<u8>>>() };
        blobs.len() as u32
    }

    unsafe extern "C" fn fake_get(
        list: *const ClapInputEvents,
        index: u32,
    ) -> *const ClapEventHeader {
        // SAFETY: as in `fake_size`.
        let blobs = unsafe { &*(*list).ctx.cast::<Vec<Vec<u8>>>() };
        blobs
            .get(index as usize)
            .map_or(ptr::null(), |blob| blob.as_ptr().cast())
    }

    impl FakeInput {
        fn new(blobs: Vec<Vec<u8>>) -> Self {
            let mut blobs = Box::new(blobs);
            let ctx = ptr::from_mut(blobs.as_mut()).cast();
            Self {
                blobs,
                view: ClapInputEvents {
                    ctx,
                    size: Some(fake_size),
                    get: Some(fake_get),
                },
            }
        }

        fn list(&self) -> ClapInputList<'_> {
            assert!(!self.blobs.is_empty() || self.blobs.is_empty());
            // SAFETY: `self.view` and the blobs its `ctx` addresses live as long as `self`,
            // which outlives the returned borrow.
            unsafe { ClapInputList::new(ptr::from_ref(&self.view), 48_000.0) }
        }
    }

    /// Packs a CLAP event struct into the byte blob a host would store.
    fn blob<T>(event: &T) -> Vec<u8> {
        let bytes = ptr::from_ref(event).cast::<u8>();
        // SAFETY: `event` is a live `T`, so `size_of::<T>()` bytes behind it are readable.
        // The copy is byte-for-byte, which is exactly what a host's arena holds.
        unsafe { core::slice::from_raw_parts(bytes, size_of::<T>()) }.to_vec()
    }

    fn note(type_: u16, time: u32, key: i16) -> ClapEventNote {
        ClapEventNote {
            header: ClapEventHeader {
                size: size_of::<ClapEventNote>() as u32,
                time,
                space_id: CLAP_CORE_EVENT_SPACE_ID,
                type_,
                flags: CLAP_EVENT_IS_LIVE,
            },
            note_id: 7,
            port_index: 0,
            channel: 2,
            key,
            velocity: 0.75,
        }
    }

    #[test]
    fn notes_decode_with_their_identity_and_flags_intact() {
        let input = FakeInput::new(vec![blob(&note(CLAP_EVENT_NOTE_ON, 12, 60))]);
        let list = input.list();
        assert_eq!(list.len(), 1);
        match list.get(0).expect("one event") {
            DauxEvent::NoteOn(n) => {
                assert_eq!(n.header.time, 12);
                assert_eq!(n.header.port_index, 0);
                assert!(n.header.flags.is_live());
                assert_eq!(n.note_id, 7);
                assert_eq!(n.channel, 2);
                assert_eq!(n.key, 60);
                assert_eq!(n.velocity, 0.75);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn every_note_type_maps_to_its_own_variant() {
        let input = FakeInput::new(vec![
            blob(&note(CLAP_EVENT_NOTE_ON, 0, 60)),
            blob(&note(CLAP_EVENT_NOTE_OFF, 1, 60)),
            blob(&note(CLAP_EVENT_NOTE_CHOKE, 2, 60)),
            blob(&note(CLAP_EVENT_NOTE_END, 3, 60)),
        ]);
        let list = input.list();
        let kinds: Vec<u16> = (0..list.len())
            .filter_map(|i| list.get(i))
            .map(|e| e.kind_bits())
            .collect();
        assert_eq!(
            kinds,
            [
                kind::NOTE_ON,
                kind::NOTE_OFF,
                kind::NOTE_CHOKE,
                kind::NOTE_END
            ]
        );
    }

    #[test]
    fn ordering_and_timestamps_survive_decoding() {
        let input = FakeInput::new(vec![
            blob(&note(CLAP_EVENT_NOTE_OFF, 4, 60)),
            blob(&note(CLAP_EVENT_NOTE_ON, 4, 62)),
            blob(&note(CLAP_EVENT_NOTE_ON, 9, 64)),
        ]);
        let list = input.list();
        let seen: Vec<(u32, i16)> = (0..list.len())
            .filter_map(|i| list.get(i))
            .filter_map(|e| match e {
                DauxEvent::NoteOn(n) | DauxEvent::NoteOff(n) => Some((n.header.time, n.key)),
                _ => None,
            })
            .collect();
        assert_eq!(
            seen,
            [(4, 60), (4, 62), (9, 64)],
            "a note-off and the note-on that replaces it at the same sample must not swap"
        );
    }

    #[test]
    fn a_short_or_foreign_event_is_skipped_not_read_past() {
        let mut truncated = note(CLAP_EVENT_NOTE_ON, 0, 60);
        truncated.header.size = 20; // shorter than `ClapEventNote`
        let mut foreign = note(CLAP_EVENT_NOTE_ON, 1, 61);
        foreign.header.space_id = 9;
        let unknown = note(0x7fff, 2, 62);
        let mut tiny = note(CLAP_EVENT_NOTE_ON, 3, 63);
        tiny.header.size = 4; // shorter than the header itself

        let input = FakeInput::new(vec![
            blob(&truncated),
            blob(&foreign),
            blob(&unknown),
            blob(&tiny),
        ]);
        let list = input.list();
        assert_eq!(list.len(), 4);
        for i in 0..4 {
            assert!(list.get(i).is_none(), "event {i} must be skipped");
        }
    }

    #[test]
    fn a_null_list_and_a_list_with_null_callbacks_read_as_empty() {
        let empty = ClapInputList::empty();
        assert_eq!(empty.len(), 0);
        assert!(empty.get(0).is_none());

        let holes = ClapInputEvents {
            ctx: ptr::null_mut(),
            size: None,
            get: None,
        };
        // SAFETY: `holes` lives for the rest of the test and its callbacks are null, which
        // `ClapInputList` explicitly tolerates.
        let list = unsafe { ClapInputList::new(ptr::from_ref(&holes), 48_000.0) };
        assert_eq!(list.len(), 0);
        assert!(list.get(0).is_none());
    }

    #[test]
    fn out_of_range_indices_answer_none() {
        let input = FakeInput::new(vec![blob(&note(CLAP_EVENT_NOTE_ON, 0, 60))]);
        let list = input.list();
        assert!(list.get(1).is_none());
        assert!(list.get(usize::MAX).is_none());
    }

    #[test]
    fn parameter_events_keep_their_plain_values() {
        let value = ClapEventParamValue {
            header: ClapEventHeader {
                size: size_of::<ClapEventParamValue>() as u32,
                time: 3,
                space_id: CLAP_CORE_EVENT_SPACE_ID,
                type_: CLAP_EVENT_PARAM_VALUE,
                flags: 0,
            },
            param_id: 42,
            cookie: ptr::null_mut(),
            note_id: -1,
            port_index: -1,
            channel: -1,
            key: -1,
            value: -6.5,
        };
        let modulation = ClapEventParamMod {
            header: ClapEventHeader {
                size: size_of::<ClapEventParamMod>() as u32,
                time: 5,
                space_id: CLAP_CORE_EVENT_SPACE_ID,
                type_: CLAP_EVENT_PARAM_MOD,
                flags: 0,
            },
            param_id: 42,
            cookie: ptr::null_mut(),
            note_id: 11,
            port_index: 0,
            channel: 1,
            key: 64,
            amount: 0.25,
        };
        let input = FakeInput::new(vec![blob(&value), blob(&modulation)]);
        let list = input.list();
        match list.get(0).expect("value event") {
            DauxEvent::ParamValue(p) => {
                assert_eq!(p.param_id, 42);
                assert_eq!(p.value, -6.5, "CLAP values are plain, exactly like DAUx");
                assert_eq!(p.note_id, -1);
                assert_eq!(p.header.port_index, 0, "a -1 wildcard becomes port 0");
            }
            other => panic!("unexpected {other:?}"),
        }
        match list.get(1).expect("mod event") {
            DauxEvent::ParamMod(p) => {
                assert_eq!(p.value, 0.25);
                assert_eq!(p.note_id, 11);
                assert_eq!(p.key, 64);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn note_expression_dimensions_map_one_to_one_and_reject_unknown_ones() {
        let dims = [
            (CLAP_NOTE_EXPRESSION_VOLUME, NoteExpression::Volume),
            (CLAP_NOTE_EXPRESSION_PAN, NoteExpression::Pan),
            (CLAP_NOTE_EXPRESSION_TUNING, NoteExpression::Tuning),
            (CLAP_NOTE_EXPRESSION_VIBRATO, NoteExpression::Vibrato),
            (CLAP_NOTE_EXPRESSION_EXPRESSION, NoteExpression::Expression),
            (CLAP_NOTE_EXPRESSION_BRIGHTNESS, NoteExpression::Brightness),
            (CLAP_NOTE_EXPRESSION_PRESSURE, NoteExpression::Pressure),
        ];
        for (id, expected) in dims {
            assert_eq!(expression_from_clap(id), Some(expected));
            assert_eq!(expression_to_clap(expected), id);
        }
        assert_eq!(expression_from_clap(7), None);
        assert_eq!(expression_from_clap(-1), None);
    }

    #[test]
    fn an_unknown_expression_id_skips_the_event_rather_than_inventing_a_dimension() {
        let e = ClapEventNoteExpression {
            header: ClapEventHeader {
                size: size_of::<ClapEventNoteExpression>() as u32,
                time: 0,
                space_id: CLAP_CORE_EVENT_SPACE_ID,
                type_: CLAP_EVENT_NOTE_EXPRESSION,
                flags: 0,
            },
            expression_id: 99,
            note_id: 1,
            port_index: 0,
            channel: 0,
            key: 60,
            value: 0.5,
        };
        let input = FakeInput::new(vec![blob(&e)]);
        assert!(input.list().get(0).is_none());
    }

    #[test]
    fn sysex_borrows_the_hosts_payload_and_tolerates_a_null_one() {
        let payload = [0xF0u8, 0x7E, 0x01, 0xF7];
        let good = ClapEventMidiSysex {
            header: ClapEventHeader {
                size: size_of::<ClapEventMidiSysex>() as u32,
                time: 0,
                space_id: CLAP_CORE_EVENT_SPACE_ID,
                type_: CLAP_EVENT_MIDI_SYSEX,
                flags: 0,
            },
            port_index: 0,
            buffer: payload.as_ptr(),
            size: payload.len() as u32,
        };
        let mut empty = good;
        empty.buffer = ptr::null();
        empty.size = 99;

        let input = FakeInput::new(vec![blob(&good), blob(&empty)]);
        let list = input.list();
        match list.get(0).expect("sysex") {
            DauxEvent::SysEx(s) => assert_eq!(s.bytes, &payload[..]),
            other => panic!("unexpected {other:?}"),
        }
        match list.get(1).expect("null sysex") {
            DauxEvent::SysEx(s) => assert!(s.bytes.is_empty(), "a null payload must be empty"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn midi1_and_midi2_payloads_survive_the_trip() {
        let m1 = ClapEventMidi {
            header: ClapEventHeader {
                size: size_of::<ClapEventMidi>() as u32,
                time: 1,
                space_id: CLAP_CORE_EVENT_SPACE_ID,
                type_: CLAP_EVENT_MIDI,
                flags: 0,
            },
            port_index: 1,
            data: [0xB2, 74, 127],
        };
        // A MIDI 2.0 channel-voice message: message type 4, two words.
        let m2 = ClapEventMidi2 {
            header: ClapEventHeader {
                size: size_of::<ClapEventMidi2>() as u32,
                time: 2,
                space_id: CLAP_CORE_EVENT_SPACE_ID,
                type_: CLAP_EVENT_MIDI2,
                flags: 0,
            },
            port_index: 0,
            data: [0x4093_3C00, 0xFFFF_0000, 0, 0],
        };
        let input = FakeInput::new(vec![blob(&m1), blob(&m2)]);
        let list = input.list();
        match list.get(0).expect("midi1") {
            DauxEvent::Midi1(e) => {
                assert_eq!(e.message.bytes, [0xB2, 74, 127]);
                assert_eq!(e.header.port_index, 1);
            }
            other => panic!("unexpected {other:?}"),
        }
        match list.get(1).expect("midi2") {
            DauxEvent::Midi2(e) => {
                assert_eq!(e.packet.message_type(), 4);
                assert_eq!(e.packet.len, 2, "a channel-voice UMP is two words");
                assert_eq!(e.packet.words[0], 0x4093_3C00);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    // ---- a fake host output sink -------------------------------------------------------

    /// What a `try_push` call recorded.
    #[derive(Default)]
    struct Recorder {
        /// `(type_, time)` of every accepted event, in push order.
        pushed: Vec<(u16, u32)>,
        /// The raw bytes of every accepted event, so a test can decode them again.
        bytes: Vec<Vec<u8>>,
        /// When true, everything is refused.
        refuse: bool,
    }

    /// A `clap_output_events` backed by a [`Recorder`].
    struct FakeOutput {
        /// Owns the recorder the callback writes into.
        recorder: Box<RefCell<Recorder>>,
        /// The view handed to the adapter.
        view: ClapOutputEvents,
    }

    unsafe extern "C" fn fake_push(
        list: *const ClapOutputEvents,
        event: *const ClapEventHeader,
    ) -> bool {
        // SAFETY: the adapter passes back the pointer it was given, and `FakeOutput` set
        // `ctx` to a live `RefCell<Recorder>` that outlives the view.
        let recorder = unsafe { &*(*list).ctx.cast::<RefCell<Recorder>>() };
        let mut recorder = recorder.borrow_mut();
        if recorder.refuse {
            return false;
        }
        // SAFETY: the adapter only ever passes a pointer to a live, fully initialised CLAP
        // event whose `size` field describes it, which is `try_push`'s contract.
        let header = unsafe { ptr::read_unaligned(event) };
        recorder.pushed.push((header.type_, header.time));
        // SAFETY: `header.size` is the size the adapter wrote, so that many bytes behind
        // `event` are readable.
        let bytes =
            unsafe { core::slice::from_raw_parts(event.cast::<u8>(), header.size as usize) };
        recorder.bytes.push(bytes.to_vec());
        true
    }

    impl FakeOutput {
        fn new() -> Self {
            let mut recorder = Box::new(RefCell::new(Recorder::default()));
            let ctx = ptr::from_mut(recorder.as_mut()).cast();
            Self {
                recorder,
                view: ClapOutputEvents {
                    ctx,
                    try_push: Some(fake_push),
                },
            }
        }

        fn refusing() -> Self {
            let out = Self::new();
            out.recorder.borrow_mut().refuse = true;
            out
        }

        fn list(&self) -> ClapOutputList<'_> {
            // SAFETY: `self.view` and the recorder its `ctx` addresses outlive the borrow.
            unsafe { ClapOutputList::new(ptr::from_ref(&self.view)) }
        }
    }

    #[test]
    fn output_events_reach_the_host_in_the_order_the_plugin_produced_them() {
        let host = FakeOutput::new();
        let mut sink = host.list();
        let mut buffer = EventBuffer::with_capacity(8, 64);
        for (time, key) in [(0u32, 60i16), (0, 62), (7, 64)] {
            buffer
                .try_push(&DauxEvent::NoteEnd(NoteEvent {
                    header: EventHeader::at(time),
                    key,
                    ..NoteEvent::default()
                }))
                .unwrap();
        }
        for i in 0..InputEvents::len(&buffer) {
            let e = buffer.get(i).expect("in range");
            OutputEvents::try_push(&mut sink, &e).expect("the fake host accepts");
        }
        assert_eq!(
            host.recorder.borrow().pushed,
            [
                (CLAP_EVENT_NOTE_END, 0),
                (CLAP_EVENT_NOTE_END, 0),
                (CLAP_EVENT_NOTE_END, 7)
            ]
        );
    }

    #[test]
    fn a_full_host_sink_reports_overflow_instead_of_allocating() {
        let host = FakeOutput::refusing();
        let mut sink = host.list();
        let e = DauxEvent::NoteEnd(NoteEvent::default());
        assert_eq!(sink.try_push(&e), Err(EventOverflow));
    }

    #[test]
    fn a_disconnected_sink_reports_overflow_rather_than_pretending() {
        let mut sink = ClapOutputList::discarding();
        assert_eq!(
            sink.try_push(&DauxEvent::NoteEnd(NoteEvent::default())),
            Err(EventOverflow)
        );
    }

    #[test]
    fn a_custom_event_is_refused_because_clap_has_no_space_for_it() {
        let host = FakeOutput::new();
        let mut sink = host.list();
        let e = DauxEvent::Custom(CustomEvent {
            header: EventHeader::at(0),
            kind: kind::CUSTOM + 3,
            bytes: &[1, 2, 3],
        });
        assert_eq!(sink.try_push(&e), Err(EventOverflow));
        assert!(host.recorder.borrow().pushed.is_empty());
    }

    #[test]
    fn every_encodable_event_round_trips_through_the_wire_format() {
        let host = FakeOutput::new();
        let transport = TransportBuilder::new().playing(true).tempo(90.0).build();
        let sysex = [0xF0u8, 1, 2, 0xF7];
        let events = [
            DauxEvent::NoteOn(NoteEvent {
                header: EventHeader::new(1, 0, EventFlags::IS_LIVE),
                note_id: 5,
                channel: 3,
                key: 48,
                velocity: 0.5,
                tuning: 0.0,
            }),
            DauxEvent::NoteExpression(NoteExpressionEvent {
                header: EventHeader::at(2),
                expression: NoteExpression::Brightness,
                note_id: 5,
                channel: 3,
                key: 48,
                value: 0.25,
            }),
            DauxEvent::ParamValue(ParamEvent {
                header: EventHeader::at(3),
                param_id: 9,
                value: 1.5,
                ..ParamEvent::default()
            }),
            DauxEvent::ParamMod(ParamEvent {
                header: EventHeader::at(4),
                param_id: 9,
                value: -0.5,
                ..ParamEvent::default()
            }),
            DauxEvent::ParamGestureBegin(ParamGestureEvent {
                header: EventHeader::at(5),
                param_id: 9,
            }),
            DauxEvent::ParamGestureEnd(ParamGestureEvent {
                header: EventHeader::at(6),
                param_id: 9,
            }),
            DauxEvent::Transport(TransportEvent {
                header: EventHeader::at(7),
                transport: transport.into(),
            }),
            DauxEvent::Midi1(Midi1Event {
                header: EventHeader::at(8),
                message: Midi1Message::control_change(2, 74, 100),
            }),
            DauxEvent::Midi2(Midi2Event {
                header: EventHeader::at(9),
                packet: Ump::from_words2(0x4093_3C00, 0xFFFF_0000),
            }),
            DauxEvent::SysEx(SysExEvent {
                header: EventHeader::at(10),
                bytes: &sysex,
            }),
        ];
        {
            let mut sink = host.list();
            for e in &events {
                sink.try_push(e).expect("the fake host accepts");
            }
        }

        let recorded = host.recorder.borrow();
        assert_eq!(
            recorded.pushed.iter().map(|(t, _)| *t).collect::<Vec<_>>(),
            [
                CLAP_EVENT_NOTE_ON,
                CLAP_EVENT_NOTE_EXPRESSION,
                CLAP_EVENT_PARAM_VALUE,
                CLAP_EVENT_PARAM_MOD,
                CLAP_EVENT_PARAM_GESTURE_BEGIN,
                CLAP_EVENT_PARAM_GESTURE_END,
                CLAP_EVENT_TRANSPORT,
                CLAP_EVENT_MIDI,
                CLAP_EVENT_MIDI2,
                CLAP_EVENT_MIDI_SYSEX,
            ]
        );
        assert_eq!(
            recorded
                .pushed
                .iter()
                .map(|(_, time)| *time)
                .collect::<Vec<_>>(),
            (1..=10).collect::<Vec<u32>>()
        );

        // The bytes the host received must decode back into the same events, which is what
        // proves each `size` and `type_` agrees with the struct that was actually written.
        let echoed = FakeInput::new(recorded.bytes.clone());
        let list = echoed.list();
        assert_eq!(list.len(), 10);
        match list.get(0).expect("note on") {
            DauxEvent::NoteOn(n) => {
                assert_eq!(n.key, 48);
                assert_eq!(n.velocity, 0.5);
                assert!(n.header.flags.is_live());
            }
            other => panic!("unexpected {other:?}"),
        }
        match list.get(1).expect("expression") {
            DauxEvent::NoteExpression(x) => {
                assert_eq!(x.expression, NoteExpression::Brightness);
                assert_eq!(x.value, 0.25);
            }
            other => panic!("unexpected {other:?}"),
        }
        match list.get(6).expect("transport") {
            DauxEvent::Transport(t) => {
                let back = Transport::from(t.transport);
                assert!(back.is_playing());
                assert_eq!(back.tempo(), Some(90.0));
            }
            other => panic!("unexpected {other:?}"),
        }
        match list.get(8).expect("midi2") {
            DauxEvent::Midi2(m) => assert_eq!(m.packet, Ump::from_words2(0x4093_3C00, 0xFFFF_0000)),
            other => panic!("unexpected {other:?}"),
        }
        match list.get(9).expect("sysex") {
            DauxEvent::SysEx(s) => assert_eq!(s.bytes, &sysex[..]),
            other => panic!("unexpected {other:?}"),
        }
    }
}
