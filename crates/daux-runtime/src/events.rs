//! The host's side of `DauxEventListV1` (`abi-v1` §9).
//!
//! One block has two lists: the input the host fills before `process` and the output the
//! plug-in appends to during it. Both are the same `#[repr(C)]` interface, and both are
//! backed by [`EventList`] — a bounded arena that allocates once, in
//! [`EventList::with_capacity`], and never again.
//!
//! # Why the storage is a word arena
//!
//! Event records contain `f64` and raw pointers, so they need 8-byte alignment. A `Vec<u8>`
//! only guarantees 1, so the records are stored in a `Vec<u64>` and every record starts on
//! a word boundary. That makes the alignment a property of the container rather than
//! something each caller has to get right.
//!
//! # SysEx
//!
//! A `DauxEventSysExV1` carries a pointer to bytes that live outside the record and are
//! valid only for the duration of `process` (`abi-v1` §16.3). Storing that pointer would
//! leave the host holding a dangling one the moment the call returned, so the payload is
//! **copied into the arena** immediately after its record and the pointer is recomputed
//! from the arena on every read. Nothing the host reads back afterwards points into the
//! plug-in.

use core::ffi::c_void;

use daux_abi::{
    DAUX_ERR_INVALID_ARG, DAUX_ERR_OUT_OF_MEMORY, DAUX_EVENT_MIDI1, DAUX_EVENT_MIDI2,
    DAUX_EVENT_NOTE_CHOKE, DAUX_EVENT_NOTE_END, DAUX_EVENT_NOTE_EXPRESSION, DAUX_EVENT_NOTE_OFF,
    DAUX_EVENT_NOTE_ON, DAUX_EVENT_PARAM_GESTURE_BEGIN, DAUX_EVENT_PARAM_GESTURE_END,
    DAUX_EVENT_PARAM_MOD, DAUX_EVENT_PARAM_VALUE, DAUX_EVENT_SYSEX, DAUX_EVENT_TRANSPORT, DAUX_OK,
    DauxEventHeaderV1, DauxEventListV1, DauxEventMidi1V1, DauxEventMidi2V1,
    DauxEventNoteExpressionV1, DauxEventNoteV1, DauxEventParamV1, DauxEventSysExV1,
    DauxEventTransportV1, DauxStatus,
};

/// Bytes in one machine word of the arena.
const WORD: usize = size_of::<u64>();

/// Byte size of the common event header.
const HEADER_BYTES: usize = size_of::<DauxEventHeaderV1>();

/// Largest single record this host will store, payload excluded.
///
/// A bound is needed before `size` is used as a length at all: a module that reports
/// `0xFFFF_FFFF` must be refused, not believed. Nothing in ABI v1 is anywhere near this
/// large — the biggest fixed record is a transport event at well under 200 bytes.
pub const MAX_EVENT_BYTES: u32 = 64 * 1024;

/// Largest SysEx payload this host will copy out of a plug-in in one event.
pub const MAX_SYSEX_BYTES: u32 = 1024 * 1024;

/// The bounded output queue is full. [audio-thread]
///
/// A normal, non-fatal condition: `abi-v1` §9 says the caller must drop or defer the event
/// rather than allocate to work around it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct EventListFull;

impl core::fmt::Display for EventListFull {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("the bounded event list is full")
    }
}

impl std::error::Error for EventListFull {}

/// Bounded, host-owned storage for one block's events, in ABI wire form.
///
/// Allocates exactly once, in [`EventList::with_capacity`]. Every other method is safe to
/// call from the audio thread.
#[derive(Debug)]
pub struct EventList {
    /// Fixed-size arena. `len == capacity` from construction, so indexing never grows it.
    words: Vec<u64>,
    /// Words of `words` in use.
    used: usize,
    /// Word offset of each stored record, in insertion order.
    offsets: Vec<usize>,
    /// Events refused since the last [`EventList::clear`].
    dropped: u32,
}

impl EventList {
    /// Preallocates room for `max_events` records totalling `max_bytes`. [main-thread]
    ///
    /// `max_bytes` covers records *and* SysEx payloads, and is rounded up to a whole number
    /// of 8-byte words. Both bounds are hard: exceeding either makes `push` report the
    /// overflow rather than allocate.
    #[must_use]
    pub fn with_capacity(max_events: usize, max_bytes: usize) -> Self {
        Self {
            words: vec![0; max_bytes.div_ceil(WORD)],
            used: 0,
            offsets: Vec::with_capacity(max_events),
            dropped: 0,
        }
    }

    /// Drops every stored event and resets the overflow counter. [audio-thread]
    #[inline]
    pub fn clear(&mut self) {
        self.offsets.clear();
        self.used = 0;
        self.dropped = 0;
    }

    /// Number of stored events. [audio-thread]
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.offsets.len()
    }

    /// `true` when no event is stored. [audio-thread]
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.offsets.is_empty()
    }

    /// How many events fit. [audio-thread]
    #[inline]
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.offsets.capacity()
    }

    /// How many bytes of record and payload storage exist. [audio-thread]
    #[inline]
    #[must_use]
    pub fn byte_capacity(&self) -> usize {
        self.words.len() * WORD
    }

    /// How many bytes of that storage are in use. [audio-thread]
    #[inline]
    #[must_use]
    pub fn bytes_used(&self) -> usize {
        self.used * WORD
    }

    /// How many events were refused since the last [`clear`](EventList::clear).
    /// [audio-thread]
    ///
    /// A non-zero count on the output list after `process` means the plug-in produced more
    /// than the host reserved room for — a capacity problem in the host, not a plug-in bug.
    #[inline]
    #[must_use]
    pub const fn dropped(&self) -> u32 {
        self.dropped
    }

    /// The header of the event at `index`. [audio-thread]
    #[must_use]
    pub fn header(&self, index: usize) -> Option<DauxEventHeaderV1> {
        self.read_typed(index, HEADER_BYTES, None)
    }

    /// The event at `index` as a note record, when it is one. [audio-thread]
    #[must_use]
    pub fn note(&self, index: usize) -> Option<DauxEventNoteV1> {
        self.read_typed(
            index,
            DauxEventNoteV1::MIN_SIZE_V1_0,
            Some(&[
                DAUX_EVENT_NOTE_ON,
                DAUX_EVENT_NOTE_OFF,
                DAUX_EVENT_NOTE_CHOKE,
                DAUX_EVENT_NOTE_END,
            ]),
        )
    }

    /// The event at `index` as a per-note expression record, when it is one.
    /// [audio-thread]
    #[must_use]
    pub fn note_expression(&self, index: usize) -> Option<DauxEventNoteExpressionV1> {
        self.read_typed(
            index,
            DauxEventNoteExpressionV1::MIN_SIZE_V1_0,
            Some(&[DAUX_EVENT_NOTE_EXPRESSION]),
        )
    }

    /// The event at `index` as a parameter record, when it is one. [audio-thread]
    #[must_use]
    pub fn param(&self, index: usize) -> Option<DauxEventParamV1> {
        self.read_typed(
            index,
            DauxEventParamV1::MIN_SIZE_V1_0,
            Some(&[
                DAUX_EVENT_PARAM_VALUE,
                DAUX_EVENT_PARAM_MOD,
                DAUX_EVENT_PARAM_GESTURE_BEGIN,
                DAUX_EVENT_PARAM_GESTURE_END,
            ]),
        )
    }

    /// The event at `index` as a MIDI 1.0 record, when it is one. [audio-thread]
    #[must_use]
    pub fn midi1(&self, index: usize) -> Option<DauxEventMidi1V1> {
        self.read_typed(
            index,
            DauxEventMidi1V1::MIN_SIZE_V1_0,
            Some(&[DAUX_EVENT_MIDI1]),
        )
    }

    /// The event at `index` as a MIDI 2.0 record, when it is one. [audio-thread]
    #[must_use]
    pub fn midi2(&self, index: usize) -> Option<DauxEventMidi2V1> {
        self.read_typed(
            index,
            DauxEventMidi2V1::MIN_SIZE_V1_0,
            Some(&[DAUX_EVENT_MIDI2]),
        )
    }

    /// The event at `index` as a transport discontinuity, when it is one. [audio-thread]
    #[must_use]
    pub fn transport(&self, index: usize) -> Option<DauxEventTransportV1> {
        self.read_typed(
            index,
            DauxEventTransportV1::MIN_SIZE_V1_0,
            Some(&[DAUX_EVENT_TRANSPORT]),
        )
    }

    /// The event at `index` as a SysEx record and its payload, when it is one.
    /// [audio-thread]
    ///
    /// The payload is a slice of this list's own arena, not of the plug-in's memory, so it
    /// stays readable after `process` returns.
    #[must_use]
    pub fn sysex(&self, index: usize) -> Option<(DauxEventSysExV1, &[u8])> {
        let record: DauxEventSysExV1 = self.read_typed(
            index,
            DauxEventSysExV1::MIN_SIZE_V1_0,
            Some(&[DAUX_EVENT_SYSEX]),
        )?;
        let start = *self.offsets.get(index)?;
        let payload_start = start + payload_offset_words(record.header.size);
        let len = record.byte_count as usize;
        let bytes = self.arena_bytes();
        let from = payload_start * WORD;
        Some((record, bytes.get(from..from.checked_add(len)?)?))
    }

    /// Appends a note event. [audio-thread]
    ///
    /// # Errors
    ///
    /// [`EventListFull`] when the list is at either of its bounds.
    pub fn push_note(&mut self, event: &DauxEventNoteV1) -> Result<(), EventListFull> {
        self.push_typed(event, DauxEventNoteV1::SIZE, &[])
    }

    /// Appends a per-note expression event. [audio-thread]
    ///
    /// # Errors
    ///
    /// [`EventListFull`] when the list is at either of its bounds.
    pub fn push_note_expression(
        &mut self,
        event: &DauxEventNoteExpressionV1,
    ) -> Result<(), EventListFull> {
        self.push_typed(event, DauxEventNoteExpressionV1::SIZE, &[])
    }

    /// Appends a parameter value, modulation or gesture event. [audio-thread]
    ///
    /// # Errors
    ///
    /// [`EventListFull`] when the list is at either of its bounds.
    pub fn push_param(&mut self, event: &DauxEventParamV1) -> Result<(), EventListFull> {
        self.push_typed(event, DauxEventParamV1::SIZE, &[])
    }

    /// Appends a MIDI 1.0 message. [audio-thread]
    ///
    /// # Errors
    ///
    /// [`EventListFull`] when the list is at either of its bounds.
    pub fn push_midi1(&mut self, event: &DauxEventMidi1V1) -> Result<(), EventListFull> {
        self.push_typed(event, DauxEventMidi1V1::SIZE, &[])
    }

    /// Appends a MIDI 2.0 Universal MIDI Packet. [audio-thread]
    ///
    /// # Errors
    ///
    /// [`EventListFull`] when the list is at either of its bounds.
    pub fn push_midi2(&mut self, event: &DauxEventMidi2V1) -> Result<(), EventListFull> {
        self.push_typed(event, DauxEventMidi2V1::SIZE, &[])
    }

    /// Appends a transport discontinuity. [audio-thread]
    ///
    /// # Errors
    ///
    /// [`EventListFull`] when the list is at either of its bounds.
    pub fn push_transport(&mut self, event: &DauxEventTransportV1) -> Result<(), EventListFull> {
        self.push_typed(event, DauxEventTransportV1::SIZE, &[])
    }

    /// Appends a SysEx event, copying `payload` into the list's arena. [audio-thread]
    ///
    /// `event.byte_count` and `event.bytes` are overwritten: the count becomes
    /// `payload.len()` and the pointer is recomputed on every read, so a caller cannot
    /// accidentally publish a pointer into memory the list does not own.
    ///
    /// # Errors
    ///
    /// [`EventListFull`] when the record and its payload do not fit, and when `payload` is
    /// longer than [`MAX_SYSEX_BYTES`].
    pub fn push_sysex(
        &mut self,
        event: &DauxEventSysExV1,
        payload: &[u8],
    ) -> Result<(), EventListFull> {
        if payload.len() > MAX_SYSEX_BYTES as usize {
            return Err(EventListFull);
        }
        let mut record = *event;
        record.header.size = DauxEventSysExV1::SIZE;
        record.header.kind = DAUX_EVENT_SYSEX;
        record.byte_count = payload.len() as u32;
        record.bytes = core::ptr::null();
        self.push_typed(&record, DauxEventSysExV1::SIZE, payload)
    }

    /// Sorts the stored events by time, keeping the order of equal timestamps.
    /// [audio-thread]
    ///
    /// `abi-v1` §9 lets a plug-in push output events in any order and requires the host to
    /// sort defensively; the tie-break is part of the contract, so the sort is stable. Only
    /// the offset table moves — no record is copied.
    ///
    /// This is an insertion sort on purpose. `slice::sort_by_key` allocates a merge buffer
    /// above a small length, and this runs on the audio thread, where allocating is not an
    /// option. Event lists are bounded and output events arrive very nearly in order, so
    /// the usual cost is linear; the worst case is quadratic in a bound the host chose.
    pub fn sort_by_time(&mut self) {
        for i in 1..self.offsets.len() {
            let start = self.offsets[i];
            let time = self.time_at(start);
            let mut j = i;
            while j > 0 && self.time_at(self.offsets[j - 1]) > time {
                self.offsets[j] = self.offsets[j - 1];
                j -= 1;
            }
            self.offsets[j] = start;
        }
    }

    /// The timestamp of the record starting at word offset `start`.
    fn time_at(&self, start: usize) -> u32 {
        // SAFETY: every entry of `offsets` is a word offset at which `write_record` wrote a
        // record of at least `HEADER_BYTES`, so reading the header back reads memory this
        // list initialised. The read is unaligned and by value.
        unsafe {
            self.words
                .as_ptr()
                .add(start)
                .cast::<DauxEventHeaderV1>()
                .read_unaligned()
                .time
        }
    }

    /// The ABI view of this list, for one `process` call. [audio-thread]
    ///
    /// The returned value borrows `self`; it must not outlive the call it is handed to,
    /// which is why it is only ever built inside
    /// [`HostBlock::with_raw`](crate::HostBlock::with_raw).
    pub(crate) fn as_abi(&mut self) -> DauxEventListV1 {
        DauxEventListV1 {
            size: DauxEventListV1::SIZE,
            _pad0: 0,
            ctx: (&raw mut *self).cast::<c_void>(),
            count: list_count,
            get: list_get,
            push: list_push,
            reserved: [0; 4],
        }
    }

    /// The arena as bytes. Every byte of it was initialised at construction.
    fn arena_bytes(&self) -> &[u8] {
        // SAFETY: `words` is a live `Vec<u64>` whose whole length is initialised (it is
        // built with `vec![0; n]`), and `u64` has no padding or invalid bit patterns, so
        // viewing it as `len * 8` initialised bytes is sound. The borrow of `self` keeps
        // the allocation alive and unaliased-by-writers for the lifetime of the slice.
        unsafe {
            core::slice::from_raw_parts(self.words.as_ptr().cast::<u8>(), self.byte_capacity())
        }
    }

    /// Reads the record at `index` as a `T`, if it is long enough and of an accepted kind.
    fn read_typed<T: Copy>(
        &self,
        index: usize,
        min_size: usize,
        kinds: Option<&[u16]>,
    ) -> Option<T> {
        let start = *self.offsets.get(index)?;
        // SAFETY: `start` was produced by `push_typed`, which only records offsets at which
        // it wrote a whole record, so at least `header.size` bytes there are initialised.
        let header: DauxEventHeaderV1 = unsafe {
            self.words
                .as_ptr()
                .add(start)
                .cast::<DauxEventHeaderV1>()
                .read_unaligned()
        };
        if (header.size as usize) < min_size.max(size_of::<T>()) {
            return None;
        }
        if let Some(kinds) = kinds
            && !kinds.contains(&header.kind)
        {
            return None;
        }
        // SAFETY: the record is `header.size` bytes long and that is at least
        // `size_of::<T>()`, checked above, so the read stays inside memory `push_typed`
        // initialised. `T` is `Copy` and every ABI event record is a `#[repr(C)]` aggregate
        // of integers, floats and raw pointers, for which any bit pattern is valid. The
        // read is unaligned and by value, so no reference into the arena is created.
        Some(unsafe { self.words.as_ptr().add(start).cast::<T>().read_unaligned() })
    }

    /// Appends `record` (`size` bytes) followed by `payload`.
    fn push_typed<T: Copy>(
        &mut self,
        record: &T,
        size: u32,
        payload: &[u8],
    ) -> Result<(), EventListFull> {
        debug_assert!(size as usize <= size_of::<T>());
        let source = (record as *const T).cast::<u8>();
        // SAFETY: `record` is a live, fully initialised `T`, so `size <= size_of::<T>()`
        // bytes at `source` are readable — the debug assertion above pins that down, and
        // every caller passes `T::SIZE`. `record` is a caller-owned value, so it cannot
        // overlap this list's arena.
        unsafe { self.write_record(source, size, payload) }
    }

    /// Copies `size` bytes from `source` plus `payload` into the arena.
    ///
    /// # Safety
    ///
    /// `source` must point to at least `size` readable bytes that do not overlap the arena.
    unsafe fn write_record(
        &mut self,
        source: *const u8,
        size: u32,
        payload: &[u8],
    ) -> Result<(), EventListFull> {
        if self.offsets.len() == self.offsets.capacity() {
            self.dropped = self.dropped.saturating_add(1);
            return Err(EventListFull);
        }
        let record_words = (size as usize).div_ceil(WORD);
        let payload_words = payload.len().div_ceil(WORD);
        let Some(end) = self
            .used
            .checked_add(record_words)
            .and_then(|n| n.checked_add(payload_words))
        else {
            self.dropped = self.dropped.saturating_add(1);
            return Err(EventListFull);
        };
        if end > self.words.len() {
            self.dropped = self.dropped.saturating_add(1);
            return Err(EventListFull);
        }

        let start = self.used;
        // SAFETY: `start + record_words <= self.words.len()` was just established, so the
        // destination holds `record_words * 8 >= size` bytes inside the arena's allocation.
        // The caller guarantees `source` is readable for `size` bytes and does not overlap
        // the arena.
        unsafe {
            core::ptr::copy_nonoverlapping(
                source,
                self.words.as_mut_ptr().add(start).cast::<u8>(),
                size as usize,
            );
        }
        if !payload.is_empty() {
            let at = (start + record_words) * WORD;
            // SAFETY: `start + record_words + payload_words <= self.words.len()`, so the
            // byte range `at .. at + payload.len()` is inside the arena.
            let destination = unsafe {
                core::slice::from_raw_parts_mut(
                    self.words.as_mut_ptr().cast::<u8>().add(at),
                    payload.len(),
                )
            };
            destination.copy_from_slice(payload);
        }
        self.used = end;
        self.offsets.push(start);
        Ok(())
    }

    /// Accepts a record a plug-in pushed during `process`.
    ///
    /// Everything about `event` is treated as untrusted: the pointer may be null, the size
    /// may be absurd, the kind may not match the payload, and a SysEx pointer may be null
    /// or lie about its length.
    ///
    /// # Safety
    ///
    /// `event` must be null or point to a readable `DauxEventHeaderV1` followed by
    /// `header.size - 16` further readable bytes, as `abi-v1` §9 requires of every record
    /// handed to `push`.
    unsafe fn push_from_module(&mut self, event: *const DauxEventHeaderV1) -> DauxStatus {
        if event.is_null() {
            return DAUX_ERR_INVALID_ARG;
        }
        // SAFETY: the caller guarantees a readable header at `event`. The read is unaligned
        // and by value; `DauxEventHeaderV1` is six integers, so any bit pattern is valid.
        let header = unsafe { event.read_unaligned() };
        if (header.size as usize) < HEADER_BYTES || header.size > MAX_EVENT_BYTES {
            return DAUX_ERR_INVALID_ARG;
        }

        let source = event.cast::<u8>();
        if header.kind == DAUX_EVENT_SYSEX {
            if (header.size as usize) < DauxEventSysExV1::MIN_SIZE_V1_0 {
                return DAUX_ERR_INVALID_ARG;
            }
            // SAFETY: `header.size` is at least the v1.0 size of a SysEx record, so the
            // whole record is readable per the caller's guarantee.
            let record = unsafe { event.cast::<DauxEventSysExV1>().read_unaligned() };
            if record.byte_count > MAX_SYSEX_BYTES {
                return DAUX_ERR_INVALID_ARG;
            }
            if record.byte_count > 0 && record.bytes.is_null() {
                return DAUX_ERR_INVALID_ARG;
            }
            let payload: &[u8] = if record.byte_count == 0 {
                &[]
            } else {
                // SAFETY: `abi-v1` §9 says a SysEx record's `bytes` addresses `byte_count`
                // readable bytes for the duration of the call; the pointer was just checked
                // non-null and the count is bounded by `MAX_SYSEX_BYTES`. The borrow ends
                // inside this function, well before `process` returns.
                unsafe { core::slice::from_raw_parts(record.bytes, record.byte_count as usize) }
            };
            // SAFETY: `source` addresses `header.size` readable bytes and points into the
            // plug-in's own memory, which cannot overlap this host-owned arena.
            return match unsafe { self.write_record(source, header.size, payload) } {
                Ok(()) => DAUX_OK,
                Err(EventListFull) => DAUX_ERR_OUT_OF_MEMORY,
            };
        }

        // SAFETY: as above; `header.size` bytes at `source` are readable and disjoint from
        // the arena.
        match unsafe { self.write_record(source, header.size, &[]) } {
            Ok(()) => DAUX_OK,
            Err(EventListFull) => DAUX_ERR_OUT_OF_MEMORY,
        }
    }

    /// Returns a borrowed pointer to the record at `index`, with any SysEx payload pointer
    /// repointed at this list's arena.
    fn borrow_record(&mut self, index: usize) -> *const DauxEventHeaderV1 {
        let Some(&start) = self.offsets.get(index) else {
            return core::ptr::null();
        };
        // SAFETY: `start` is an offset `push_typed`/`write_record` recorded after writing a
        // whole record there, so the header is initialised memory inside the arena.
        let record = unsafe { self.words.as_mut_ptr().add(start).cast::<u8>() };
        // SAFETY: as above.
        let header = unsafe { record.cast::<DauxEventHeaderV1>().read_unaligned() };
        if header.kind == DAUX_EVENT_SYSEX
            && (header.size as usize) >= DauxEventSysExV1::MIN_SIZE_V1_0
        {
            let payload = (start + payload_offset_words(header.size)) * WORD;
            // SAFETY: `payload` is the byte offset of the payload `write_record` copied in
            // right after the record, so it is inside the arena. Writing the recomputed
            // pointer into the stored record's `bytes` field touches `size_of::<*const u8>()`
            // bytes at a field offset that is inside a record of at least the v1.0 SysEx
            // size, checked above. The write is unaligned.
            unsafe {
                record
                    .add(core::mem::offset_of!(DauxEventSysExV1, bytes))
                    .cast::<*const u8>()
                    .write_unaligned(self.words.as_ptr().cast::<u8>().add(payload));
            }
        }
        record.cast::<DauxEventHeaderV1>()
    }
}

/// Word offset of a record's payload relative to the record's own start.
const fn payload_offset_words(record_size: u32) -> usize {
    (record_size as usize).div_ceil(WORD)
}

/// Recovers the list a `ctx` names.
///
/// # Safety
///
/// `ctx` must be null or the pointer [`EventList::as_abi`] put in the table, addressing a
/// list that is alive and not otherwise borrowed for the duration of the call. `abi-v1` §15
/// guarantees the audio-thread calls for one instance are never concurrent, so no two
/// callbacks can hold this reference at once.
#[inline]
unsafe fn list_of<'a>(ctx: *mut c_void) -> Option<&'a mut EventList> {
    if ctx.is_null() {
        return None;
    }
    // SAFETY: forwarded verbatim from this function's own contract.
    Some(unsafe { &mut *ctx.cast::<EventList>() })
}

unsafe extern "C" fn list_count(ctx: *mut c_void) -> u32 {
    // SAFETY: `ctx` is the pointer this crate published in the table.
    let Some(list) = (unsafe { list_of(ctx) }) else {
        return 0;
    };
    // The list cannot hold more events than its `u32`-bounded capacity, but saturating
    // keeps the conversion total rather than relying on that.
    u32::try_from(list.len()).unwrap_or(u32::MAX)
}

unsafe extern "C" fn list_get(ctx: *mut c_void, index: u32) -> *const DauxEventHeaderV1 {
    // SAFETY: `ctx` is the pointer this crate published in the table.
    let Some(list) = (unsafe { list_of(ctx) }) else {
        return core::ptr::null();
    };
    match usize::try_from(index) {
        Ok(index) => list.borrow_record(index),
        Err(_) => core::ptr::null(),
    }
}

unsafe extern "C" fn list_push(ctx: *mut c_void, event: *const DauxEventHeaderV1) -> DauxStatus {
    // SAFETY: `ctx` is the pointer this crate published in the table.
    let Some(list) = (unsafe { list_of(ctx) }) else {
        return DAUX_ERR_INVALID_ARG;
    };
    // SAFETY: forwarded from `push`'s own contract in `abi-v1` §9: the caller passes a
    // readable record whose `size` covers it.
    unsafe { list.push_from_module(event) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(time: u32, key: i16) -> DauxEventNoteV1 {
        let mut e = DauxEventNoteV1::new();
        e.header.time = time;
        e.key = key;
        e.velocity = 0.8;
        e
    }

    #[test]
    fn records_round_trip_through_the_arena() {
        let mut list = EventList::with_capacity(8, 1024);
        list.push_note(&note(3, 60)).unwrap();

        let mut param = DauxEventParamV1::new();
        param.header.time = 5;
        param.param_id = 11;
        param.value = -6.0;
        list.push_param(&param).unwrap();

        let mut midi = DauxEventMidi1V1::new();
        midi.header.time = 7;
        midi.data = [0x90, 60, 100];
        list.push_midi1(&midi).unwrap();

        assert_eq!(list.len(), 3);
        assert_eq!(list.note(0).unwrap().key, 60);
        assert_eq!(list.param(1).unwrap().param_id, 11);
        assert_eq!(list.midi1(2).unwrap().data, [0x90, 60, 100]);
        assert_eq!(list.header(2).unwrap().time, 7);

        // Asking for the wrong layout must fail rather than reinterpret the bytes.
        assert!(list.param(0).is_none());
        assert!(list.note(1).is_none());
        assert!(list.midi2(2).is_none());
        assert!(list.note(99).is_none());
    }

    #[test]
    fn clear_returns_all_capacity() {
        let mut list = EventList::with_capacity(2, 256);
        list.push_note(&note(0, 60)).unwrap();
        list.push_note(&note(1, 61)).unwrap();
        assert_eq!(list.push_note(&note(2, 62)), Err(EventListFull));
        assert_eq!(list.dropped(), 1);
        assert!(list.bytes_used() > 0);

        list.clear();
        assert_eq!(list.len(), 0);
        assert_eq!(list.dropped(), 0);
        assert_eq!(list.bytes_used(), 0);
        list.push_note(&note(0, 60)).unwrap();
    }

    #[test]
    fn a_byte_bound_is_enforced_independently_of_the_event_bound() {
        // Room for many events but only two note records' worth of bytes.
        let mut list = EventList::with_capacity(64, DauxEventNoteV1::SIZE as usize * 2);
        list.push_note(&note(0, 60)).unwrap();
        list.push_note(&note(1, 61)).unwrap();
        assert_eq!(list.push_note(&note(2, 62)), Err(EventListFull));
        assert_eq!(list.len(), 2, "a refused push must not be stored");
        assert_eq!(list.dropped(), 1);
    }

    /// SysEx payloads must be copied, not referenced: the plug-in's buffer is gone the
    /// moment `process` returns.
    #[test]
    fn sysex_payloads_are_copied_into_the_list() {
        let mut list = EventList::with_capacity(4, 4096);
        let payload = [0xf0u8, 0x7e, 0x01, 0x02, 0xf7];
        let mut record = DauxEventSysExV1::new();
        record.header.time = 2;
        list.push_sysex(&record, &payload).unwrap();
        list.push_note(&note(4, 62)).unwrap();

        let (stored, bytes) = list.sysex(0).expect("a sysex event");
        assert_eq!(stored.byte_count, payload.len() as u32);
        assert_eq!(bytes, payload);
        assert_eq!(stored.header.time, 2);
        // The record that follows is unaffected by the payload sitting between them.
        assert_eq!(list.note(1).unwrap().key, 62);

        // An empty payload is legal.
        let mut empty = DauxEventSysExV1::new();
        empty.header.time = 9;
        list.push_sysex(&empty, &[]).unwrap();
        let (_, bytes) = list.sysex(2).unwrap();
        assert!(bytes.is_empty());
    }

    /// The pointer a plug-in reads out of `get` must address this list's arena, and it must
    /// be recomputed on every call so that moving the list cannot leave it stale.
    #[test]
    fn get_repoints_sysex_at_the_lists_own_storage() {
        let mut list = EventList::with_capacity(4, 4096);
        let payload = [1u8, 2, 3, 4, 5, 6, 7, 8, 9];
        list.push_sysex(&DauxEventSysExV1::new(), &payload).unwrap();

        let abi = list.as_abi();
        // SAFETY: `abi` was just built from `list`, which is alive and not otherwise
        // borrowed here, so the context pointer is valid for these calls.
        let (count, record) = unsafe { ((abi.count)(abi.ctx), (abi.get)(abi.ctx, 0)) };
        assert_eq!(count, 1);
        assert!(!record.is_null());
        // SAFETY: `get` returned a pointer to a stored SysEx record inside `list`'s arena.
        let stored = unsafe { record.cast::<DauxEventSysExV1>().read_unaligned() };
        assert_eq!(stored.byte_count, payload.len() as u32);
        assert!(!stored.bytes.is_null());
        // SAFETY: the pointer `get` published addresses `byte_count` bytes of the arena.
        let seen = unsafe { core::slice::from_raw_parts(stored.bytes, payload.len()) };
        assert_eq!(seen, payload);

        // SAFETY: as above; out-of-range indices must answer null rather than read.
        assert!(unsafe { (abi.get)(abi.ctx, 1) }.is_null());
        // SAFETY: a null context is explicitly handled by every callback.
        assert!(unsafe { (abi.get)(core::ptr::null_mut(), 0) }.is_null());
        // SAFETY: as above.
        assert_eq!(unsafe { (abi.count)(core::ptr::null_mut()) }, 0);
    }

    /// Everything a plug-in hands to `push` is untrusted.
    #[test]
    fn push_refuses_hostile_records() {
        let mut list = EventList::with_capacity(8, 4096);
        let abi = list.as_abi();

        // A null record.
        // SAFETY: `abi` borrows the live `list`; null is a value `push` must handle without
        // dereferencing it.
        let status = unsafe { (abi.push)(abi.ctx, core::ptr::null()) };
        assert_eq!(status.0, DAUX_ERR_INVALID_ARG.0, "a null record");

        // A size below the header itself.
        let mut runt = DauxEventNoteV1::new();
        runt.header.size = 4;
        // SAFETY: `runt` is a live, fully initialised record; only its declared size lies,
        // and `push` must reject it before using that size as a length.
        let status = unsafe { (abi.push)(abi.ctx, (&raw const runt).cast()) };
        assert_eq!(status.0, DAUX_ERR_INVALID_ARG.0, "a sub-header size");

        // An absurd size.
        let mut giant = DauxEventNoteV1::new();
        giant.header.size = u32::MAX;
        // SAFETY: as above — a live record whose declared size is the only lie, refused
        // before that size is used to read or copy anything.
        let status = unsafe { (abi.push)(abi.ctx, (&raw const giant).cast()) };
        assert_eq!(status.0, DAUX_ERR_INVALID_ARG.0, "a 4 GiB event");

        // A SysEx record with a null payload pointer but a non-zero count.
        let mut lying = DauxEventSysExV1::new();
        lying.byte_count = 32;
        lying.bytes = core::ptr::null();
        // SAFETY: `lying` is a live, fully initialised record; `push` must check the payload
        // pointer before dereferencing it.
        let status = unsafe { (abi.push)(abi.ctx, (&raw const lying).cast()) };
        assert_eq!(status.0, DAUX_ERR_INVALID_ARG.0, "a null payload pointer");

        // A SysEx record claiming more payload than the host will ever copy.
        let bytes = [0u8; 8];
        let mut absurd = DauxEventSysExV1::new();
        absurd.byte_count = MAX_SYSEX_BYTES + 1;
        absurd.bytes = bytes.as_ptr();
        // SAFETY: `absurd` is a live record pointing at eight real bytes. The count is
        // refused before it is used as a slice length, so those eight bytes are never
        // over-read — which is the whole point of the bound.
        let status = unsafe { (abi.push)(abi.ctx, (&raw const absurd).cast()) };
        assert_eq!(status.0, DAUX_ERR_INVALID_ARG.0, "an oversized payload");

        // A SysEx record too short to hold its own layout.
        let mut short = DauxEventSysExV1::new();
        short.header.size = HEADER_BYTES as u32;
        // SAFETY: as above — a live record whose declared size is too small for the layout
        // its `kind` names, which must be caught before the record is read as one.
        let status = unsafe { (abi.push)(abi.ctx, (&raw const short).cast()) };
        assert_eq!(status.0, DAUX_ERR_INVALID_ARG.0, "a truncated sysex record");

        assert_eq!(list.len(), 0, "no hostile record may be stored");
    }

    /// A full output queue is a normal condition and must be reported as
    /// `DAUX_ERR_OUT_OF_MEMORY`, never worked around by allocating.
    #[test]
    fn a_full_list_reports_out_of_memory_to_the_module() {
        let mut list = EventList::with_capacity(1, 4096);
        let abi = list.as_abi();
        let event = note(0, 60);
        // SAFETY: `abi` borrows the live `list`; `event` is a complete record whose declared
        // size matches its layout.
        let first = unsafe { (abi.push)(abi.ctx, (&raw const event).cast()) };
        assert_eq!(first.0, DAUX_OK.0);
        // SAFETY: as above — the same live record, pushed into a list that is now full.
        let second = unsafe { (abi.push)(abi.ctx, (&raw const event).cast()) };
        assert_eq!(second.0, DAUX_ERR_OUT_OF_MEMORY.0);
        assert_eq!(list.len(), 1);
        assert_eq!(list.dropped(), 1);
    }

    /// A plug-in that pushes a SysEx event must not leave the host holding a pointer into
    /// the plug-in's memory once `process` has returned.
    #[test]
    fn a_pushed_sysex_payload_survives_the_source_buffer() {
        let mut list = EventList::with_capacity(4, 4096);
        {
            let source = [9u8, 8, 7, 6, 5];
            let mut record = DauxEventSysExV1::new();
            record.byte_count = source.len() as u32;
            record.bytes = source.as_ptr();
            let abi = list.as_abi();
            // SAFETY: `abi` borrows the live `list`, and `record` describes `source`, which
            // is alive for the whole call — exactly the guarantee `abi-v1` §9 gives.
            let status = unsafe { (abi.push)(abi.ctx, (&raw const record).cast()) };
            assert_eq!(status.0, DAUX_OK.0);
        }
        // `source` is gone; the payload must still read back.
        let (stored, bytes) = list.sysex(0).expect("stored");
        assert_eq!(stored.byte_count, 5);
        assert_eq!(bytes, [9, 8, 7, 6, 5]);
    }

    /// `abi-v1` §9: hosts must sort output events defensively, and events sharing a
    /// timestamp must keep the order the plug-in pushed them in.
    #[test]
    fn sorting_is_stable_and_by_time() {
        let mut list = EventList::with_capacity(8, 2048);
        for (time, key) in [(4u32, 60i16), (0, 62), (4, 64), (2, 61)] {
            list.push_note(&note(time, key)).unwrap();
        }
        list.sort_by_time();
        let order: Vec<(u32, i16)> = (0..list.len())
            .map(|i| {
                let n = list.note(i).unwrap();
                (n.header.time, n.key)
            })
            .collect();
        assert_eq!(order, [(0, 62), (2, 61), (4, 60), (4, 64)]);
    }

    #[test]
    fn every_record_starts_on_an_eight_byte_boundary() {
        let mut list = EventList::with_capacity(8, 4096);
        // A three-byte payload forces the next record off a natural boundary unless the
        // arena rounds up.
        list.push_sysex(&DauxEventSysExV1::new(), &[1, 2, 3])
            .unwrap();
        list.push_note(&note(1, 60)).unwrap();
        let abi = list.as_abi();
        for index in 0..2 {
            // SAFETY: `abi` borrows the live `list` and both indices are in range.
            let record = unsafe { (abi.get)(abi.ctx, index) };
            assert!(!record.is_null());
            assert_eq!(
                record as usize % align_of::<DauxEventNoteV1>(),
                0,
                "record {index} is misaligned"
            );
        }
    }
}
