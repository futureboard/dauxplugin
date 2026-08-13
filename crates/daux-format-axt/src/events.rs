//! The event list adapters (abi-v1 §9).
//!
//! Two stack values built at the top of every `process` call: [`AbiInputEvents`] reads the
//! host's list, [`AbiOutputEvents`] appends to it. Neither owns anything and neither allocates.
//!
//! # Reading is hostile-input handling
//!
//! An event stream is the one part of a block whose *shape* the host chooses per record, so
//! everything is checked before it is believed:
//!
//! * a null record pointer, or one whose `size` does not cover the concrete record the `kind`
//!   names, is skipped rather than decoded;
//! * every record is read with [`read_unaligned`](core::ptr::read_unaligned), so a host that
//!   packs its list without regard for the 8-byte alignment `f64` fields want cannot cause
//!   undefined behaviour;
//! * a note-expression code this build does not know is skipped, not guessed at;
//! * a SysEx record with a null payload is read as an empty payload, not as a 4 GiB slice.
//!
//! Skipping is deliberate: [`InputEventIter`](daux_plugin_api::InputEventIter) walks past a
//! `None` and carries on, so one malformed record cannot silence a whole block.
//!
//! # Vendor events
//!
//! abi-v1 §9 reserves `kind >= DAUX_EVENT_CUSTOM` for vendors without defining a record layout.
//! This crate uses the only self-describing one the header already allows: the payload follows
//! the 16-byte header inline, and `header.size` bounds it. Both sides of a DAUx pair must agree
//! on that, and this is where it is written down.

use core::mem::size_of;

use daux_abi::{
    DAUX_ERR_OUT_OF_MEMORY, DAUX_EVENT_CUSTOM, DAUX_EVENT_MIDI1, DAUX_EVENT_MIDI2,
    DAUX_EVENT_NOTE_CHOKE, DAUX_EVENT_NOTE_END, DAUX_EVENT_NOTE_EXPRESSION, DAUX_EVENT_NOTE_OFF,
    DAUX_EVENT_NOTE_ON, DAUX_EVENT_PARAM_GESTURE_BEGIN, DAUX_EVENT_PARAM_GESTURE_END,
    DAUX_EVENT_PARAM_MOD, DAUX_EVENT_PARAM_VALUE, DAUX_EVENT_SYSEX, DAUX_EVENT_TRANSPORT,
    DauxEventHeaderV1, DauxEventListV1, DauxEventMidi1V1, DauxEventMidi2V1,
    DauxEventNoteExpressionV1, DauxEventNoteV1, DauxEventParamV1, DauxEventSysExV1,
    DauxEventTransportV1,
};
use daux_plugin_api::{
    CustomEvent, DauxEvent, EventFlags, EventHeader, EventOverflow, InputEvents, Midi1Event,
    Midi1Message, Midi2Event, NoteEvent, NoteExpression, NoteExpressionEvent, OutputEvents,
    ParamEvent, ParamGestureEvent, SysExEvent, TransportEvent, Ump,
};

use crate::transport::{snapshot_from_abi, snapshot_to_abi};

/// Bytes of inline payload a vendor event may carry out of the plug-in.
///
/// Sized so the whole stack record is 256 bytes. `process` must not allocate, so an outgoing
/// vendor payload larger than this is refused with [`EventOverflow`] — which the caller already
/// has to handle, because a bounded output queue can be full for entirely ordinary reasons.
pub(crate) const CUSTOM_PAYLOAD_CAPACITY: usize = 256 - size_of::<DauxEventHeaderV1>();

/// Bytes of SysEx a plug-in may push out in one event, for the same reason.
pub(crate) const SYSEX_PAYLOAD_CAPACITY: usize = 4096;

/// A vendor event as it crosses the boundary: header, then `size - 16` payload bytes.
#[repr(C, align(8))]
struct CustomRecordV1 {
    header: DauxEventHeaderV1,
    payload: [u8; CUSTOM_PAYLOAD_CAPACITY],
}

/// [audio-thread] Borrows a list pointer, rejecting one that is null or too short to call.
///
/// # Safety
///
/// `list` is null or points at a [`DauxEventListV1`] that stays valid for `'a`, which must not
/// outlive the `process` call it came from (abi-v1 §16.3).
unsafe fn borrow_list<'a>(list: *const DauxEventListV1) -> Option<&'a DauxEventListV1> {
    if list.is_null() {
        return None;
    }
    // SAFETY: non-null was just checked and the caller guarantees the pointee is live for `'a`.
    let list = unsafe { &*list };
    // A table too short to hold the v1.0 entries must not be called at all (abi-v1 §3).
    list.is_v1_0_compatible().then_some(list)
}

/// The host's input list for one block. `[audio-thread]`
pub(crate) struct AbiInputEvents<'a> {
    list: Option<&'a DauxEventListV1>,
}

impl<'a> AbiInputEvents<'a> {
    /// [audio-thread] Wraps the host's `in_events`.
    ///
    /// A null or malformed list becomes an empty one: abi-v1 §8 says `in_events` is never null,
    /// and a plug-in that trusted that and dereferenced it would crash a host that got it
    /// wrong.
    ///
    /// # Safety
    ///
    /// See [`borrow_list`].
    pub(crate) unsafe fn new(list: *const DauxEventListV1) -> Self {
        Self {
            // SAFETY: forwarded verbatim from this function's own contract.
            list: unsafe { borrow_list(list) },
        }
    }
}

impl InputEvents for AbiInputEvents<'_> {
    fn len(&self) -> usize {
        match self.list {
            // SAFETY: `list` was validated at construction, so the table is live and its
            // v1.0 entries are callable; `count` is documented `[audio-thread]` (abi-v1 §9).
            Some(list) => unsafe { (list.count)(list.ctx) as usize },
            None => 0,
        }
    }

    fn get(&self, index: usize) -> Option<DauxEvent<'_>> {
        let list = self.list?;
        let index = u32::try_from(index).ok()?;
        // SAFETY: the table is live (checked at construction) and `get` returns either null or
        // a record that stays valid until the current `process` returns — which outlives the
        // borrow of `self` this event is tied to.
        let record = unsafe { (list.get)(list.ctx, index) };
        // SAFETY: `record` is null or a host-owned event record valid for the rest of this
        // call; `decode` reads it unaligned and re-checks `size` before trusting any field.
        unsafe { decode(record) }
    }
}

/// The host's output list for one block. `[audio-thread]`
pub(crate) struct AbiOutputEvents<'a> {
    list: Option<&'a DauxEventListV1>,
}

impl<'a> AbiOutputEvents<'a> {
    /// [audio-thread] Wraps the host's `out_events`.
    ///
    /// A null or malformed list makes every `try_push` report [`EventOverflow`], which is a
    /// condition every plug-in must already handle without allocating or panicking.
    ///
    /// # Safety
    ///
    /// See [`borrow_list`].
    pub(crate) unsafe fn new(list: *const DauxEventListV1) -> Self {
        Self {
            // SAFETY: forwarded verbatim from this function's own contract.
            list: unsafe { borrow_list(list) },
        }
    }

    /// Hands `record` to the host, mapping every failure onto [`EventOverflow`].
    fn push_raw(&mut self, record: *const DauxEventHeaderV1) -> Result<(), EventOverflow> {
        let Some(list) = self.list else {
            return Err(EventOverflow);
        };
        // SAFETY: the table was validated at construction; `record` points at a fully
        // initialised, correctly aligned stack record that outlives the call, and the host
        // copies what it needs before returning (abi-v1 §9).
        let status = unsafe { (list.push)(list.ctx, record) };
        if status.is_ok() {
            Ok(())
        } else {
            // `DAUX_ERR_OUT_OF_MEMORY` is the documented "queue full"; anything else is a host
            // refusing the event, which the caller must handle the same way — by dropping it.
            debug_assert!(
                status == DAUX_ERR_OUT_OF_MEMORY || status.is_err(),
                "push returned a positive status, which abi-v1 §2 reserves"
            );
            Err(EventOverflow)
        }
    }
}

impl OutputEvents for AbiOutputEvents<'_> {
    fn try_push(&mut self, event: &DauxEvent<'_>) -> Result<(), EventOverflow> {
        match *event {
            DauxEvent::NoteOn(note) => self.push_note(DAUX_EVENT_NOTE_ON, &note),
            DauxEvent::NoteOff(note) => self.push_note(DAUX_EVENT_NOTE_OFF, &note),
            DauxEvent::NoteChoke(note) => self.push_note(DAUX_EVENT_NOTE_CHOKE, &note),
            DauxEvent::NoteEnd(note) => self.push_note(DAUX_EVENT_NOTE_END, &note),
            DauxEvent::NoteExpression(expression) => self.push_expression(&expression),
            DauxEvent::ParamValue(param) => self.push_param(DAUX_EVENT_PARAM_VALUE, &param),
            DauxEvent::ParamMod(param) => self.push_param(DAUX_EVENT_PARAM_MOD, &param),
            DauxEvent::ParamGestureBegin(gesture) => {
                self.push_gesture(DAUX_EVENT_PARAM_GESTURE_BEGIN, &gesture)
            }
            DauxEvent::ParamGestureEnd(gesture) => {
                self.push_gesture(DAUX_EVENT_PARAM_GESTURE_END, &gesture)
            }
            DauxEvent::Transport(transport) => self.push_transport(&transport),
            DauxEvent::Midi1(midi) => self.push_midi1(&midi),
            DauxEvent::Midi2(midi) => self.push_midi2(&midi),
            DauxEvent::SysEx(sysex) => self.push_sysex(&sysex),
            DauxEvent::Custom(custom) => self.push_custom(&custom),
        }
    }
}

impl AbiOutputEvents<'_> {
    fn push_note(&mut self, kind: u16, note: &NoteEvent) -> Result<(), EventOverflow> {
        let mut record = DauxEventNoteV1::new();
        record.header = abi_header(kind, DauxEventNoteV1::SIZE, note.header);
        record.note_id = note.note_id;
        record.channel = note.channel;
        record.key = note.key;
        record.velocity = note.velocity;
        record.tuning = note.tuning;
        self.push_raw(&raw const record.header)
    }

    fn push_expression(&mut self, event: &NoteExpressionEvent) -> Result<(), EventOverflow> {
        let mut record = DauxEventNoteExpressionV1::new();
        record.header = abi_header(
            DAUX_EVENT_NOTE_EXPRESSION,
            DauxEventNoteExpressionV1::SIZE,
            event.header,
        );
        record.expression_id = event.expression.as_bits();
        record.note_id = event.note_id;
        record.channel = event.channel;
        record.key = event.key;
        record.value = event.value;
        self.push_raw(&raw const record.header)
    }

    fn push_param(&mut self, kind: u16, event: &ParamEvent) -> Result<(), EventOverflow> {
        let mut record = DauxEventParamV1::new();
        record.header = abi_header(kind, DauxEventParamV1::SIZE, event.header);
        record.param_id = event.param_id;
        record.note_id = event.note_id;
        record.channel = event.channel;
        record.key = event.key;
        // Plain, never normalised (abi-v1 §11.2). The host cookie is an input-side accelerator
        // the neutral event model does not carry, so it goes out null, which the ABI allows.
        record.value = event.value;
        self.push_raw(&raw const record.header)
    }

    fn push_gesture(&mut self, kind: u16, event: &ParamGestureEvent) -> Result<(), EventOverflow> {
        let mut record = DauxEventParamV1::new();
        record.header = abi_header(kind, DauxEventParamV1::SIZE, event.header);
        record.param_id = event.param_id;
        record.note_id = -1;
        record.channel = -1;
        record.key = -1;
        self.push_raw(&raw const record.header)
    }

    fn push_transport(&mut self, event: &TransportEvent) -> Result<(), EventOverflow> {
        let mut record = DauxEventTransportV1::new();
        record.header = abi_header(
            DAUX_EVENT_TRANSPORT,
            DauxEventTransportV1::SIZE,
            event.header,
        );
        record.transport = snapshot_to_abi(&event.transport);
        self.push_raw(&raw const record.header)
    }

    fn push_midi1(&mut self, event: &Midi1Event) -> Result<(), EventOverflow> {
        let mut record = DauxEventMidi1V1::new();
        record.header = abi_header(DAUX_EVENT_MIDI1, DauxEventMidi1V1::SIZE, event.header);
        record.data = event.message.bytes;
        self.push_raw(&raw const record.header)
    }

    fn push_midi2(&mut self, event: &Midi2Event) -> Result<(), EventOverflow> {
        let mut record = DauxEventMidi2V1::new();
        record.header = abi_header(DAUX_EVENT_MIDI2, DauxEventMidi2V1::SIZE, event.header);
        record.word_count = event.packet.word_count() as u32;
        record.words = event.packet.words;
        // Words past `word_count` must be zero (abi-v1 §9).
        for word in &mut record.words[event.packet.word_count()..] {
            *word = 0;
        }
        self.push_raw(&raw const record.header)
    }

    fn push_sysex(&mut self, event: &SysExEvent<'_>) -> Result<(), EventOverflow> {
        if event.bytes.len() > SYSEX_PAYLOAD_CAPACITY {
            return Err(EventOverflow);
        }
        let mut record = DauxEventSysExV1::new();
        record.header = abi_header(DAUX_EVENT_SYSEX, DauxEventSysExV1::SIZE, event.header);
        record.byte_count = event.bytes.len() as u32;
        // The payload stays where the plug-in put it; the host copies it during `push`, which
        // is the only reading abi-v1 §9 allows given the pointer is borrowed for the call.
        record.bytes = event.bytes.as_ptr();
        self.push_raw(&raw const record.header)
    }

    fn push_custom(&mut self, event: &CustomEvent<'_>) -> Result<(), EventOverflow> {
        if event.bytes.len() > CUSTOM_PAYLOAD_CAPACITY || event.kind < DAUX_EVENT_CUSTOM {
            return Err(EventOverflow);
        }
        let size = size_of::<DauxEventHeaderV1>() + event.bytes.len();
        let mut record = CustomRecordV1 {
            header: abi_header(event.kind, size as u32, event.header),
            payload: [0; CUSTOM_PAYLOAD_CAPACITY],
        };
        record.payload[..event.bytes.len()].copy_from_slice(event.bytes);
        self.push_raw(&raw const record.header)
    }
}

/// The ABI header for an outgoing record of `size` bytes.
fn abi_header(kind: u16, size: u32, header: EventHeader) -> DauxEventHeaderV1 {
    DauxEventHeaderV1 {
        size,
        time: header.time,
        kind,
        flags: header.flags.bits(),
        port_index: header.port_index,
        _pad0: 0,
    }
}

/// The neutral header for an incoming record.
fn neutral_header(header: &DauxEventHeaderV1) -> EventHeader {
    EventHeader::new(
        header.time,
        header.port_index,
        EventFlags::from_bits(header.flags),
    )
}

/// Reads a `T` from an event record, first checking the record declares enough bytes for it.
///
/// # Safety
///
/// `record` is non-null and points at `header.size` readable bytes owned by the host.
unsafe fn read_record<T>(
    record: *const DauxEventHeaderV1,
    header: &DauxEventHeaderV1,
) -> Option<T> {
    if (header.size as usize) < size_of::<T>() {
        return None;
    }
    // SAFETY: the caller guarantees `size` bytes are readable and the check above proves
    // `size >= size_of::<T>()`. `read_unaligned` imposes no alignment requirement, so a host
    // that packed its list tightly is handled rather than trusted.
    Some(unsafe { record.cast::<T>().read_unaligned() })
}

/// [audio-thread] Decodes one host record into a neutral event, or `None` to skip it.
///
/// # Safety
///
/// `record` is null or points at a live event record of at least `size` bytes that stays valid
/// for `'a` — the duration of the current `process` call (abi-v1 §16.3).
unsafe fn decode<'a>(record: *const DauxEventHeaderV1) -> Option<DauxEvent<'a>> {
    if record.is_null() {
        return None;
    }
    // SAFETY: non-null was checked; the header is the one field every revision of every record
    // has, and reading it unaligned is always valid for a pointer the host says is a record.
    let header = unsafe { record.read_unaligned() };
    if (header.size as usize) < size_of::<DauxEventHeaderV1>() {
        return None;
    }
    let neutral = neutral_header(&header);

    if header.kind >= DAUX_EVENT_CUSTOM {
        // SAFETY: the header declares `size` readable bytes and `size >= 16` was just checked,
        // so the payload is the `size - 16` bytes that follow. `add` stays inside the record.
        let bytes = unsafe {
            let start = record.cast::<u8>().add(size_of::<DauxEventHeaderV1>());
            core::slice::from_raw_parts(
                start,
                header.size as usize - size_of::<DauxEventHeaderV1>(),
            )
        };
        return Some(DauxEvent::Custom(CustomEvent {
            header: neutral,
            kind: header.kind,
            bytes,
        }));
    }

    match header.kind {
        DAUX_EVENT_NOTE_ON | DAUX_EVENT_NOTE_OFF | DAUX_EVENT_NOTE_CHOKE | DAUX_EVENT_NOTE_END => {
            // SAFETY: forwarded from this function's contract.
            let record: DauxEventNoteV1 = unsafe { read_record(record, &header) }?;
            let note = NoteEvent {
                header: neutral,
                note_id: record.note_id,
                channel: record.channel,
                key: record.key,
                velocity: record.velocity,
                tuning: record.tuning,
            };
            Some(match header.kind {
                DAUX_EVENT_NOTE_ON => DauxEvent::NoteOn(note),
                DAUX_EVENT_NOTE_OFF => DauxEvent::NoteOff(note),
                DAUX_EVENT_NOTE_CHOKE => DauxEvent::NoteChoke(note),
                _ => DauxEvent::NoteEnd(note),
            })
        }
        DAUX_EVENT_NOTE_EXPRESSION => {
            // SAFETY: forwarded from this function's contract.
            let record: DauxEventNoteExpressionV1 = unsafe { read_record(record, &header) }?;
            Some(DauxEvent::NoteExpression(NoteExpressionEvent {
                header: neutral,
                // An expression code this build does not know is skipped rather than guessed:
                // applying "volume" to something the host meant as "brightness" is worse than
                // ignoring it.
                expression: NoteExpression::from_bits(record.expression_id)?,
                note_id: record.note_id,
                channel: record.channel,
                key: record.key,
                value: record.value,
            }))
        }
        DAUX_EVENT_PARAM_VALUE | DAUX_EVENT_PARAM_MOD => {
            // SAFETY: forwarded from this function's contract.
            let record: DauxEventParamV1 = unsafe { read_record(record, &header) }?;
            let param = ParamEvent {
                header: neutral,
                param_id: record.param_id,
                note_id: record.note_id,
                channel: record.channel,
                key: record.key,
                value: record.value,
            };
            Some(if header.kind == DAUX_EVENT_PARAM_VALUE {
                DauxEvent::ParamValue(param)
            } else {
                DauxEvent::ParamMod(param)
            })
        }
        DAUX_EVENT_PARAM_GESTURE_BEGIN | DAUX_EVENT_PARAM_GESTURE_END => {
            // SAFETY: forwarded from this function's contract.
            let record: DauxEventParamV1 = unsafe { read_record(record, &header) }?;
            let gesture = ParamGestureEvent {
                header: neutral,
                param_id: record.param_id,
            };
            Some(if header.kind == DAUX_EVENT_PARAM_GESTURE_BEGIN {
                DauxEvent::ParamGestureBegin(gesture)
            } else {
                DauxEvent::ParamGestureEnd(gesture)
            })
        }
        DAUX_EVENT_TRANSPORT => {
            // SAFETY: forwarded from this function's contract.
            let record: DauxEventTransportV1 = unsafe { read_record(record, &header) }?;
            Some(DauxEvent::Transport(TransportEvent {
                header: neutral,
                transport: snapshot_from_abi(&record.transport),
            }))
        }
        DAUX_EVENT_MIDI1 => {
            // SAFETY: forwarded from this function's contract.
            let record: DauxEventMidi1V1 = unsafe { read_record(record, &header) }?;
            Some(DauxEvent::Midi1(Midi1Event {
                header: neutral,
                message: Midi1Message::new(record.data),
            }))
        }
        DAUX_EVENT_MIDI2 => {
            // SAFETY: forwarded from this function's contract.
            let record: DauxEventMidi2V1 = unsafe { read_record(record, &header) }?;
            let len = u8::try_from(record.word_count).ok()?;
            Some(DauxEvent::Midi2(Midi2Event {
                header: neutral,
                // A packet claiming 0 or 5+ words is malformed and is skipped.
                packet: Ump::try_new(record.words, len)?,
            }))
        }
        DAUX_EVENT_SYSEX => {
            // SAFETY: forwarded from this function's contract.
            let record: DauxEventSysExV1 = unsafe { read_record(record, &header) }?;
            let bytes = if record.bytes.is_null() || record.byte_count == 0 {
                &[][..]
            } else {
                // SAFETY: the host published a payload pointer and a length in a record it
                // owns; abi-v1 §9 makes both valid until the current `process` returns, which
                // is what `'a` names. A null pointer was excluded above.
                unsafe { core::slice::from_raw_parts(record.bytes, record.byte_count as usize) }
            };
            Some(DauxEvent::SysEx(SysExEvent {
                header: neutral,
                bytes,
            }))
        }
        _ => None,
    }
}

/// Storage for one host-side event list, used by the tests here and by the in-crate harness.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use core::ffi::c_void;
    use std::cell::RefCell;

    /// A minimal host event list: a byte arena plus record offsets, driven through the same
    /// `DauxEventListV1` a real host would publish.
    pub(crate) struct FakeList {
        inner: Box<Inner>,
        table: DauxEventListV1,
    }

    struct Inner {
        arena: RefCell<Vec<u8>>,
        offsets: RefCell<Vec<usize>>,
        capacity: usize,
        /// SysEx payloads, kept alive separately so the record's pointer stays valid.
        payloads: RefCell<Vec<Box<[u8]>>>,
    }

    impl FakeList {
        pub(crate) fn with_capacity(capacity: usize) -> Self {
            let inner = Box::new(Inner {
                // Both are reserved up front so that a test which asserts "`process`
                // allocated nothing" is measuring the plug-in and not this fake host.
                arena: RefCell::new(Vec::with_capacity(64 * 1024)),
                offsets: RefCell::new(Vec::with_capacity(capacity)),
                capacity,
                payloads: RefCell::new(Vec::new()),
            });
            let ctx = (&raw const *inner).cast::<c_void>().cast_mut();
            let table = DauxEventListV1 {
                size: DauxEventListV1::SIZE,
                _pad0: 0,
                ctx,
                count,
                get,
                push,
                reserved: [0; 4],
            };
            Self { inner, table }
        }

        pub(crate) fn table(&self) -> *const DauxEventListV1 {
            &raw const self.table
        }

        pub(crate) fn len(&self) -> usize {
            self.inner.offsets.borrow().len()
        }

        /// Appends a raw record, exactly as a host would lay one out.
        pub(crate) fn push_bytes(&self, bytes: &[u8]) {
            let mut arena = self.inner.arena.borrow_mut();
            // Records are 8-aligned in a well-behaved host; the tests deliberately use both.
            while arena.len() % 8 != 0 {
                arena.push(0);
            }
            let offset = arena.len();
            arena.extend_from_slice(bytes);
            self.inner.offsets.borrow_mut().push(offset);
        }

        /// Appends a record at a deliberately odd offset, to prove the reader never assumes
        /// alignment.
        pub(crate) fn push_bytes_misaligned(&self, bytes: &[u8]) {
            let mut arena = self.inner.arena.borrow_mut();
            arena.push(0xAA);
            let offset = arena.len();
            arena.extend_from_slice(bytes);
            self.inner.offsets.borrow_mut().push(offset);
        }

        pub(crate) fn record_bytes(&self, index: usize) -> Vec<u8> {
            let offsets = self.inner.offsets.borrow();
            let arena = self.inner.arena.borrow();
            let start = offsets[index];
            let size = u32::from_ne_bytes(arena[start..start + 4].try_into().unwrap()) as usize;
            arena[start..start + size].to_vec()
        }

        pub(crate) fn keep_payload(&self, bytes: &[u8]) -> *const u8 {
            let boxed: Box<[u8]> = bytes.to_vec().into_boxed_slice();
            let ptr = boxed.as_ptr();
            self.inner.payloads.borrow_mut().push(boxed);
            ptr
        }
    }

    unsafe extern "C" fn count(ctx: *mut c_void) -> u32 {
        // SAFETY: `ctx` is the `Inner` the table was built with and outlives the table.
        let inner = unsafe { &*ctx.cast::<Inner>() };
        inner.offsets.borrow().len() as u32
    }

    unsafe extern "C" fn get(ctx: *mut c_void, index: u32) -> *const DauxEventHeaderV1 {
        // SAFETY: as `count`.
        let inner = unsafe { &*ctx.cast::<Inner>() };
        let offsets = inner.offsets.borrow();
        match offsets.get(index as usize) {
            // The arena never reallocates while a block is being read, because the tests fill
            // it before they read it.
            Some(offset) => inner.arena.borrow()[*offset..].as_ptr().cast(),
            None => core::ptr::null(),
        }
    }

    unsafe extern "C" fn push(
        ctx: *mut c_void,
        event: *const DauxEventHeaderV1,
    ) -> daux_abi::DauxStatus {
        // SAFETY: as `count`.
        let inner = unsafe { &*ctx.cast::<Inner>() };
        if inner.offsets.borrow().len() >= inner.capacity {
            return DAUX_ERR_OUT_OF_MEMORY;
        }
        // SAFETY: the caller guarantees a live record of `size` bytes.
        let header = unsafe { event.read_unaligned() };
        // SAFETY: as above; `size` bytes are readable from the record's start.
        let bytes =
            unsafe { core::slice::from_raw_parts(event.cast::<u8>(), header.size as usize) };
        let mut arena = inner.arena.borrow_mut();
        while arena.len() % 8 != 0 {
            arena.push(0);
        }
        let offset = arena.len();
        arena.extend_from_slice(bytes);
        // A real host copies a SysEx payload too; this one does the same so the round-trip
        // tests are honest about what survives.
        if header.kind == DAUX_EVENT_SYSEX
            && (header.size as usize) >= size_of::<DauxEventSysExV1>()
        {
            // SAFETY: the size check above proves the record holds a full SysEx record.
            let record: DauxEventSysExV1 =
                unsafe { event.cast::<DauxEventSysExV1>().read_unaligned() };
            if !record.bytes.is_null() && record.byte_count > 0 {
                // SAFETY: the pushing side guarantees `byte_count` readable bytes for the
                // duration of this call.
                let payload = unsafe {
                    core::slice::from_raw_parts(record.bytes, record.byte_count as usize)
                };
                let boxed: Box<[u8]> = payload.to_vec().into_boxed_slice();
                let ptr = boxed.as_ptr();
                inner.payloads.borrow_mut().push(boxed);
                let patched = offset + core::mem::offset_of!(DauxEventSysExV1, bytes);
                arena[patched..patched + size_of::<*const u8>()]
                    .copy_from_slice(&(ptr as usize).to_ne_bytes());
            }
        }
        inner.offsets.borrow_mut().push(offset);
        daux_abi::DAUX_OK
    }
}

#[cfg(test)]
mod tests {
    use super::testing::FakeList;
    use super::*;
    use daux_plugin_api::InputEventIter;

    fn bytes_of<T>(value: &T) -> &[u8] {
        // SAFETY: `T` here is always a `#[repr(C)]` ABI record made of integers, floats and
        // raw pointers, so every byte of it is initialised and readable as `u8`.
        unsafe { core::slice::from_raw_parts((value as *const T).cast::<u8>(), size_of::<T>()) }
    }

    fn note_record(kind: u16, time: u32, key: i16) -> DauxEventNoteV1 {
        let mut record = DauxEventNoteV1::new();
        record.header = DauxEventHeaderV1::with(kind, DauxEventNoteV1::SIZE, time);
        record.note_id = 7;
        record.channel = 1;
        record.key = key;
        record.velocity = 0.75;
        record.tuning = -12.5;
        record
    }

    #[test]
    fn a_block_of_events_decodes_in_order() {
        let list = FakeList::with_capacity(16);
        list.push_bytes(bytes_of(&note_record(DAUX_EVENT_NOTE_ON, 0, 60)));
        list.push_bytes(bytes_of(&note_record(DAUX_EVENT_NOTE_OFF, 32, 60)));

        // SAFETY: the list outlives the adapter, which is exactly the block scope here.
        let input = unsafe { AbiInputEvents::new(list.table()) };
        assert_eq!(input.len(), 2);

        let events: Vec<DauxEvent<'_>> = InputEventIter::new(&input).collect();
        assert_eq!(events.len(), 2);
        match events[0] {
            DauxEvent::NoteOn(n) => {
                assert_eq!(n.header.time, 0);
                assert_eq!(n.note_id, 7);
                assert_eq!(n.channel, 1);
                assert_eq!(n.key, 60);
                assert_eq!(n.velocity, 0.75);
                assert_eq!(n.tuning, -12.5);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(matches!(events[1], DauxEvent::NoteOff(_)));
        assert_eq!(events[1].time(), 32);
    }

    /// A host is free to lay its records out however it likes; `f64` fields inside them must
    /// not turn that into undefined behaviour.
    #[test]
    fn a_misaligned_record_still_decodes() {
        let list = FakeList::with_capacity(4);
        list.push_bytes_misaligned(bytes_of(&note_record(DAUX_EVENT_NOTE_ON, 5, 61)));
        // SAFETY: the list outlives the adapter.
        let input = unsafe { AbiInputEvents::new(list.table()) };
        match input.get(0) {
            Some(DauxEvent::NoteOn(n)) => {
                assert_eq!(n.key, 61);
                assert_eq!(n.velocity, 0.75);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn a_record_too_short_for_its_kind_is_skipped_not_read() {
        let list = FakeList::with_capacity(4);
        let mut truncated = note_record(DAUX_EVENT_NOTE_ON, 0, 60);
        // The host claims a note event but only published a header's worth of bytes.
        truncated.header.size = size_of::<DauxEventHeaderV1>() as u32;
        list.push_bytes(&bytes_of(&truncated)[..size_of::<DauxEventHeaderV1>()]);
        list.push_bytes(bytes_of(&note_record(DAUX_EVENT_NOTE_OFF, 9, 62)));

        // SAFETY: the list outlives the adapter.
        let input = unsafe { AbiInputEvents::new(list.table()) };
        assert_eq!(input.len(), 2, "the host still reports two records");
        assert!(input.get(0).is_none(), "the malformed one must be skipped");
        // ...and the iterator walks past it rather than stopping the block.
        let decoded: Vec<u32> = InputEventIter::new(&input).map(|e| e.time()).collect();
        assert_eq!(decoded, [9]);
    }

    #[test]
    fn a_size_below_the_header_is_rejected() {
        let list = FakeList::with_capacity(4);
        let mut broken = note_record(DAUX_EVENT_NOTE_ON, 0, 60);
        broken.header.size = 4;
        list.push_bytes(bytes_of(&broken));
        // SAFETY: the list outlives the adapter.
        let input = unsafe { AbiInputEvents::new(list.table()) };
        assert!(input.get(0).is_none());
    }

    #[test]
    fn unknown_kinds_and_expressions_are_skipped() {
        let list = FakeList::with_capacity(8);
        let mut unknown = note_record(DAUX_EVENT_NOTE_ON, 0, 60);
        unknown.header.kind = 999; // below the vendor range, but not a v1 kind
        list.push_bytes(bytes_of(&unknown));

        let mut expression = DauxEventNoteExpressionV1::new();
        expression.header = DauxEventHeaderV1::with(
            DAUX_EVENT_NOTE_EXPRESSION,
            DauxEventNoteExpressionV1::SIZE,
            1,
        );
        expression.expression_id = 4_242;
        list.push_bytes(bytes_of(&expression));

        // SAFETY: the list outlives the adapter.
        let input = unsafe { AbiInputEvents::new(list.table()) };
        assert!(input.get(0).is_none());
        assert!(input.get(1).is_none());
        assert_eq!(InputEventIter::new(&input).count(), 0);
    }

    #[test]
    fn a_malformed_midi2_packet_is_skipped() {
        let list = FakeList::with_capacity(4);
        for word_count in [0u32, 5, 4] {
            let mut record = DauxEventMidi2V1::new();
            record.header =
                DauxEventHeaderV1::with(DAUX_EVENT_MIDI2, DauxEventMidi2V1::SIZE, word_count);
            record.word_count = word_count;
            record.words = [1, 2, 3, 4];
            list.push_bytes(bytes_of(&record));
        }
        // SAFETY: the list outlives the adapter.
        let input = unsafe { AbiInputEvents::new(list.table()) };
        assert!(input.get(0).is_none(), "0 words is not a packet");
        assert!(input.get(1).is_none(), "5 words is not a packet");
        match input.get(2) {
            Some(DauxEvent::Midi2(m)) => assert_eq!(m.packet.as_words(), &[1, 2, 3, 4]),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn a_sysex_with_a_null_payload_reads_as_empty() {
        let list = FakeList::with_capacity(4);
        let mut record = DauxEventSysExV1::new();
        record.header = DauxEventHeaderV1::with(DAUX_EVENT_SYSEX, DauxEventSysExV1::SIZE, 0);
        // A host that lies about its payload must not make us build a 4 GiB slice.
        record.byte_count = u32::MAX;
        record.bytes = core::ptr::null();
        list.push_bytes(bytes_of(&record));

        // SAFETY: the list outlives the adapter.
        let input = unsafe { AbiInputEvents::new(list.table()) };
        match input.get(0) {
            Some(DauxEvent::SysEx(e)) => assert!(e.bytes.is_empty()),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn a_sysex_payload_is_borrowed_from_the_host() {
        let list = FakeList::with_capacity(4);
        let payload = [0xF0u8, 0x7E, 0x01, 0xF7];
        let mut record = DauxEventSysExV1::new();
        record.header = DauxEventHeaderV1::with(DAUX_EVENT_SYSEX, DauxEventSysExV1::SIZE, 3);
        record.byte_count = payload.len() as u32;
        record.bytes = list.keep_payload(&payload);
        list.push_bytes(bytes_of(&record));

        // SAFETY: the list outlives the adapter.
        let input = unsafe { AbiInputEvents::new(list.table()) };
        match input.get(0) {
            Some(DauxEvent::SysEx(e)) => {
                assert_eq!(e.bytes, &payload);
                assert_eq!(e.header.time, 3);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn a_vendor_event_carries_its_payload_inline() {
        let list = FakeList::with_capacity(4);
        let payload = [1u8, 2, 3, 4, 5];
        let mut record = [0u8; size_of::<DauxEventHeaderV1>() + 5];
        let header = DauxEventHeaderV1::with(
            DAUX_EVENT_CUSTOM + 3,
            (size_of::<DauxEventHeaderV1>() + payload.len()) as u32,
            11,
        );
        record[..size_of::<DauxEventHeaderV1>()].copy_from_slice(bytes_of(&header));
        record[size_of::<DauxEventHeaderV1>()..].copy_from_slice(&payload);
        list.push_bytes(&record);

        // SAFETY: the list outlives the adapter.
        let input = unsafe { AbiInputEvents::new(list.table()) };
        match input.get(0) {
            Some(DauxEvent::Custom(e)) => {
                assert_eq!(e.kind, DAUX_EVENT_CUSTOM + 3);
                assert_eq!(e.bytes, &payload);
                assert_eq!(e.header.time, 11);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn a_null_or_short_list_reads_empty_and_swallows_output() {
        // SAFETY: a null pointer is explicitly allowed by the constructor's contract.
        let input = unsafe { AbiInputEvents::new(core::ptr::null()) };
        assert_eq!(input.len(), 0);
        assert!(input.get(0).is_none());

        // SAFETY: as above.
        let mut output = unsafe { AbiOutputEvents::new(core::ptr::null()) };
        let note = NoteEvent::default();
        assert_eq!(
            output.try_push(&DauxEvent::NoteOn(note)),
            Err(EventOverflow)
        );

        // A table whose `size` does not cover the v1.0 entries must not be called at all,
        // even though every entry in it happens to be valid here.
        let list = FakeList::with_capacity(4);
        list.push_bytes(bytes_of(&note_record(DAUX_EVENT_NOTE_ON, 0, 60)));
        // SAFETY: the table was published by `FakeList` and is a plain `Copy` structure.
        let mut short = unsafe { *list.table() };
        short.size = 8;
        // SAFETY: `short` is a live local for the rest of this block.
        let input = unsafe { AbiInputEvents::new(&raw const short) };
        assert_eq!(input.len(), 0, "a short table must not be called");
        assert!(input.get(0).is_none());
    }

    #[test]
    fn every_event_kind_round_trips_through_the_host() {
        let list = FakeList::with_capacity(32);
        let sysex = [0xF0u8, 0x11, 0xF7];
        let custom_payload = [9u8, 8, 7];
        let outgoing: Vec<DauxEvent<'_>> = vec![
            DauxEvent::NoteOn(NoteEvent {
                header: EventHeader::new(1, 2, EventFlags::IS_LIVE),
                note_id: 3,
                channel: 4,
                key: 5,
                velocity: 0.5,
                tuning: 1.5,
            }),
            DauxEvent::NoteEnd(NoteEvent::default()),
            DauxEvent::NoteExpression(NoteExpressionEvent {
                header: EventHeader::at(6),
                expression: NoteExpression::Brightness,
                note_id: 1,
                channel: 2,
                key: 3,
                value: 0.25,
            }),
            DauxEvent::ParamValue(ParamEvent {
                header: EventHeader::at(7),
                param_id: 42,
                note_id: -1,
                channel: -1,
                key: -1,
                value: -6.0,
            }),
            DauxEvent::ParamGestureBegin(ParamGestureEvent {
                header: EventHeader::at(8),
                param_id: 42,
            }),
            DauxEvent::Midi1(Midi1Event {
                header: EventHeader::at(9),
                message: Midi1Message::new([0x90, 60, 100]),
            }),
            DauxEvent::Midi2(Midi2Event {
                header: EventHeader::at(10),
                packet: Ump::from_words2(0x4090_3C00, 0xFFFF_0000),
            }),
            DauxEvent::SysEx(SysExEvent {
                header: EventHeader::at(11),
                bytes: &sysex,
            }),
            DauxEvent::Custom(CustomEvent {
                header: EventHeader::at(12),
                kind: DAUX_EVENT_CUSTOM,
                bytes: &custom_payload,
            }),
        ];

        {
            // SAFETY: the list outlives the adapter.
            let mut output = unsafe { AbiOutputEvents::new(list.table()) };
            for event in &outgoing {
                output.try_push(event).expect("capacity is 32");
            }
        }
        assert_eq!(list.len(), outgoing.len());

        // SAFETY: the list outlives the adapter.
        let input = unsafe { AbiInputEvents::new(list.table()) };
        let decoded: Vec<DauxEvent<'_>> = (0..input.len()).filter_map(|i| input.get(i)).collect();
        assert_eq!(decoded.len(), outgoing.len(), "an event was lost");
        for (sent, back) in outgoing.iter().zip(&decoded) {
            assert_eq!(sent.kind_bits(), back.kind_bits());
            assert_eq!(sent.time(), back.time());
            assert_eq!(sent.header().flags, back.header().flags);
            assert_eq!(sent.header().port_index, back.header().port_index);
            assert_eq!(sent.payload(), back.payload());
        }
        assert_eq!(decoded[0], outgoing[0]);
        assert_eq!(decoded[3], outgoing[3]);
    }

    #[test]
    fn a_full_output_reports_overflow_instead_of_allocating() {
        let list = FakeList::with_capacity(1);
        // SAFETY: the list outlives the adapter.
        let mut output = unsafe { AbiOutputEvents::new(list.table()) };
        let note = DauxEvent::NoteOn(NoteEvent::default());
        assert_eq!(output.try_push(&note), Ok(()));
        assert_eq!(output.try_push(&note), Err(EventOverflow));
    }

    #[test]
    fn an_oversized_payload_is_refused_rather_than_allocated_for() {
        let list = FakeList::with_capacity(8);
        // SAFETY: the list outlives the adapter.
        let mut output = unsafe { AbiOutputEvents::new(list.table()) };

        let big = vec![0u8; CUSTOM_PAYLOAD_CAPACITY + 1];
        let custom = DauxEvent::Custom(CustomEvent {
            header: EventHeader::at(0),
            kind: DAUX_EVENT_CUSTOM,
            bytes: &big,
        });
        assert_eq!(output.try_push(&custom), Err(EventOverflow));

        // A vendor code below the reserved range would collide with a standard kind.
        let colliding = DauxEvent::Custom(CustomEvent {
            header: EventHeader::at(0),
            kind: DAUX_EVENT_NOTE_ON,
            bytes: &[],
        });
        assert_eq!(output.try_push(&colliding), Err(EventOverflow));

        let huge = vec![0u8; SYSEX_PAYLOAD_CAPACITY + 1];
        let sysex = DauxEvent::SysEx(SysExEvent {
            header: EventHeader::at(0),
            bytes: &huge,
        });
        assert_eq!(output.try_push(&sysex), Err(EventOverflow));
        assert_eq!(list.len(), 0);
    }

    #[test]
    fn outgoing_records_declare_the_size_they_really_are() {
        let list = FakeList::with_capacity(8);
        {
            // SAFETY: the list outlives the adapter.
            let mut output = unsafe { AbiOutputEvents::new(list.table()) };
            output
                .try_push(&DauxEvent::NoteOn(NoteEvent::default()))
                .unwrap();
            output
                .try_push(&DauxEvent::Custom(CustomEvent {
                    header: EventHeader::at(0),
                    kind: DAUX_EVENT_CUSTOM,
                    bytes: &[1, 2, 3],
                }))
                .unwrap();
        }
        assert_eq!(list.record_bytes(0).len(), DauxEventNoteV1::SIZE as usize);
        assert_eq!(
            list.record_bytes(1).len(),
            size_of::<DauxEventHeaderV1>() + 3
        );
    }
}
