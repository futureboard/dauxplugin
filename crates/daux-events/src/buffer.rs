//! Owned, bounded event storage.

use crate::event::{
    CustomEvent, DauxEvent, EventHeader, Midi1Event, Midi2Event, NoteEvent, NoteExpressionEvent,
    ParamEvent, ParamGestureEvent, SysExEvent, TransportEvent,
};
use crate::traits::{EventOverflow, InputEventIter, InputEvents, OutputEvents};

/// The owned form of one event. Byte payloads become a `(offset, len)` slice of the arena so
/// every record is the same `Copy` size and no event needs its own allocation.
#[derive(Clone, Copy, Debug)]
enum Record {
    NoteOn(NoteEvent),
    NoteOff(NoteEvent),
    NoteChoke(NoteEvent),
    NoteEnd(NoteEvent),
    NoteExpression(NoteExpressionEvent),
    ParamValue(ParamEvent),
    ParamMod(ParamEvent),
    ParamGestureBegin(ParamGestureEvent),
    ParamGestureEnd(ParamGestureEvent),
    Transport(TransportEvent),
    Midi1(Midi1Event),
    Midi2(Midi2Event),
    SysEx {
        header: EventHeader,
        offset: usize,
        len: usize,
    },
    Custom {
        header: EventHeader,
        kind: u16,
        offset: usize,
        len: usize,
    },
}

impl Record {
    fn time(&self) -> u32 {
        match *self {
            Self::NoteOn(e) | Self::NoteOff(e) | Self::NoteChoke(e) | Self::NoteEnd(e) => {
                e.header.time
            }
            Self::NoteExpression(e) => e.header.time,
            Self::ParamValue(e) | Self::ParamMod(e) => e.header.time,
            Self::ParamGestureBegin(e) | Self::ParamGestureEnd(e) => e.header.time,
            Self::Transport(e) => e.header.time,
            Self::Midi1(e) => e.header.time,
            Self::Midi2(e) => e.header.time,
            Self::SysEx { header, .. } | Self::Custom { header, .. } => header.time,
        }
    }
}

/// A record plus the position it was pushed at, so a stable sort needs no scratch buffer.
#[derive(Clone, Copy, Debug)]
struct Slot {
    seq: u32,
    record: Record,
}

/// Owned, bounded event storage that implements both [`InputEvents`] and [`OutputEvents`].
///
/// This is the concrete list hosts, adapters, offline renderers and tests hand to a plug-in.
/// Two preallocated regions back it:
///
/// * a fixed number of event records, and
/// * a **byte arena** for variable-length `SysEx` and `Custom` payloads.
///
/// Both are allocated once by [`EventBuffer::with_capacity`] and never grow, so
/// [`EventBuffer::try_push`], [`EventBuffer::get`], [`EventBuffer::clear`] and
/// [`EventBuffer::sort_by_time`] are all allocation-free and safe on the audio thread.
/// A push that does not fit — because the record list is full *or* because the payload does
/// not fit in the arena — returns [`EventOverflow`] and leaves the buffer untouched.
///
/// [`EventBuffer::clear`] keeps both allocations, so a host that clears the buffer at the top
/// of every block reuses the same arena forever.
///
/// ```
/// use daux_events::{DauxEvent, EventBuffer, EventHeader, SysExEvent};
///
/// let mut buf = EventBuffer::with_capacity(4, 16);
/// let bytes = [0xF0, 0x7E, 0x00, 0xF7];
/// buf.try_push(&DauxEvent::SysEx(SysExEvent { header: EventHeader::at(0), bytes: &bytes }))
///     .unwrap();
/// assert_eq!(buf.len(), 1);
/// assert_eq!(buf.bytes_used(), 4);
///
/// buf.clear();                       // frees nothing, reuses everything
/// assert_eq!(buf.bytes_used(), 0);
/// ```
#[derive(Debug)]
pub struct EventBuffer {
    slots: Vec<Slot>,
    bytes: Vec<u8>,
    max_events: usize,
    max_bytes: usize,
}

impl EventBuffer {
    /// [main-thread] Allocates room for `events` records and `bytes` of variable-length
    /// payload. This is the only method that allocates.
    pub fn with_capacity(events: usize, bytes: usize) -> Self {
        Self {
            slots: Vec::with_capacity(events),
            bytes: Vec::with_capacity(bytes),
            max_events: events,
            max_bytes: bytes,
        }
    }

    /// [audio-thread] Number of events currently held.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// [audio-thread] `true` when no events are held.
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// [audio-thread] How many events fit before [`EventBuffer::try_push`] starts failing.
    pub fn capacity(&self) -> usize {
        self.max_events
    }

    /// [audio-thread] Total size of the payload arena in bytes.
    pub fn byte_capacity(&self) -> usize {
        self.max_bytes
    }

    /// [audio-thread] How much of the payload arena is in use.
    pub fn bytes_used(&self) -> usize {
        self.bytes.len()
    }

    /// [audio-thread] Drops every event and rewinds the payload arena, keeping both
    /// allocations for reuse.
    pub fn clear(&mut self) {
        self.slots.clear();
        self.bytes.clear();
    }

    /// [audio-thread] The event at `index`, or `None` when `index` is out of range.
    ///
    /// Borrowed payloads point into this buffer's arena and live as long as the borrow of
    /// `self`.
    pub fn get(&self, index: usize) -> Option<DauxEvent<'_>> {
        let record = self.slots.get(index)?.record;
        Some(match record {
            Record::NoteOn(e) => DauxEvent::NoteOn(e),
            Record::NoteOff(e) => DauxEvent::NoteOff(e),
            Record::NoteChoke(e) => DauxEvent::NoteChoke(e),
            Record::NoteEnd(e) => DauxEvent::NoteEnd(e),
            Record::NoteExpression(e) => DauxEvent::NoteExpression(e),
            Record::ParamValue(e) => DauxEvent::ParamValue(e),
            Record::ParamMod(e) => DauxEvent::ParamMod(e),
            Record::ParamGestureBegin(e) => DauxEvent::ParamGestureBegin(e),
            Record::ParamGestureEnd(e) => DauxEvent::ParamGestureEnd(e),
            Record::Transport(e) => DauxEvent::Transport(e),
            Record::Midi1(e) => DauxEvent::Midi1(e),
            Record::Midi2(e) => DauxEvent::Midi2(e),
            Record::SysEx {
                header,
                offset,
                len,
            } => DauxEvent::SysEx(SysExEvent {
                header,
                bytes: self.slice(offset, len)?,
            }),
            Record::Custom {
                header,
                kind,
                offset,
                len,
            } => DauxEvent::Custom(CustomEvent {
                header,
                kind,
                bytes: self.slice(offset, len)?,
            }),
        })
    }

    /// [audio-thread] Iterates the buffer in storage order.
    pub fn iter(&self) -> InputEventIter<'_> {
        InputEventIter::new(self)
    }

    /// [audio-thread] Appends a copy of `e`, interning any borrowed payload in the arena.
    ///
    /// Never allocates. Returns `Err(EventOverflow)` when the record list is full or the
    /// payload does not fit in the remaining arena; the buffer is left exactly as it was.
    pub fn try_push(&mut self, e: &DauxEvent<'_>) -> Result<(), EventOverflow> {
        if self.slots.len() >= self.max_events {
            return Err(EventOverflow);
        }
        let record = match *e {
            DauxEvent::NoteOn(n) => Record::NoteOn(n),
            DauxEvent::NoteOff(n) => Record::NoteOff(n),
            DauxEvent::NoteChoke(n) => Record::NoteChoke(n),
            DauxEvent::NoteEnd(n) => Record::NoteEnd(n),
            DauxEvent::NoteExpression(n) => Record::NoteExpression(n),
            DauxEvent::ParamValue(p) => Record::ParamValue(p),
            DauxEvent::ParamMod(p) => Record::ParamMod(p),
            DauxEvent::ParamGestureBegin(p) => Record::ParamGestureBegin(p),
            DauxEvent::ParamGestureEnd(p) => Record::ParamGestureEnd(p),
            DauxEvent::Transport(t) => Record::Transport(t),
            DauxEvent::Midi1(m) => Record::Midi1(m),
            DauxEvent::Midi2(m) => Record::Midi2(m),
            DauxEvent::SysEx(s) => {
                let (offset, len) = self.intern(s.bytes)?;
                Record::SysEx {
                    header: s.header,
                    offset,
                    len,
                }
            }
            DauxEvent::Custom(c) => {
                let (offset, len) = self.intern(c.bytes)?;
                Record::Custom {
                    header: c.header,
                    kind: c.kind,
                    offset,
                    len,
                }
            }
        };
        // The record list was checked at the top and `Vec::with_capacity(max_events)` cannot
        // be exceeded, so this push never reallocates.
        let seq = self.slots.len() as u32;
        self.slots.push(Slot { seq, record });
        Ok(())
    }

    /// [audio-thread] Sorts the events by [`DauxEvent::time`], **stably**: events with equal
    /// timestamps keep the order they were pushed in, as abi-v1 §9 requires.
    ///
    /// Stability comes from the insertion index stored beside every record, so this uses an
    /// in-place unstable sort and needs no scratch allocation — unlike `slice::sort_by`,
    /// which allocates and is therefore unusable on the audio thread. Sorting is idempotent
    /// and events pushed after a sort still order after equal-timestamped earlier ones.
    pub fn sort_by_time(&mut self) {
        self.slots
            .sort_unstable_by_key(|s| (s.record.time(), s.seq));
    }

    /// [audio-thread] This buffer as a read-only event list.
    pub fn as_input(&self) -> &dyn InputEvents {
        self
    }

    /// [audio-thread] This buffer as an event sink.
    pub fn as_output(&mut self) -> &mut dyn OutputEvents {
        self
    }

    /// Copies `bytes` into the arena, returning its `(offset, len)`.
    fn intern(&mut self, bytes: &[u8]) -> Result<(usize, usize), EventOverflow> {
        let offset = self.bytes.len();
        // `offset <= max_bytes` always holds, so this cannot underflow.
        if bytes.len() > self.max_bytes - offset {
            return Err(EventOverflow);
        }
        // Within the capacity reserved by `with_capacity`, so this never reallocates.
        self.bytes.extend_from_slice(bytes);
        Ok((offset, bytes.len()))
    }

    fn slice(&self, offset: usize, len: usize) -> Option<&[u8]> {
        self.bytes.get(offset..offset.checked_add(len)?)
    }
}

impl InputEvents for EventBuffer {
    fn len(&self) -> usize {
        Self::len(self)
    }

    fn is_empty(&self) -> bool {
        Self::is_empty(self)
    }

    fn get(&self, index: usize) -> Option<DauxEvent<'_>> {
        Self::get(self, index)
    }
}

impl OutputEvents for EventBuffer {
    fn try_push(&mut self, e: &DauxEvent<'_>) -> Result<(), EventOverflow> {
        Self::try_push(self, e)
    }
}

#[cfg(test)]
mod tests {
    use daux_midi::{Midi1Message, Ump};

    use super::*;
    use crate::event::{EventFlags, NoteExpression, kind};
    use crate::transport::{TransportSnapshot, transport_flags};

    fn note_at(time: u32, key: i16) -> NoteEvent {
        NoteEvent {
            header: EventHeader::at(time),
            note_id: key as i32,
            channel: 0,
            key,
            velocity: 0.75,
            tuning: 0.0,
        }
    }

    fn push_note(buf: &mut EventBuffer, time: u32, key: i16) {
        buf.try_push(&DauxEvent::NoteOn(note_at(time, key)))
            .expect("fits");
    }

    fn keys(buf: &EventBuffer) -> Vec<i16> {
        buf.iter()
            .map(|e| match e {
                DauxEvent::NoteOn(n) => n.key,
                other => panic!("unexpected {other:?}"),
            })
            .collect()
    }

    #[test]
    fn a_fresh_buffer_is_empty() {
        let buf = EventBuffer::with_capacity(8, 64);
        assert!(buf.is_empty());
        assert_eq!(buf.len(), 0);
        assert_eq!(buf.capacity(), 8);
        assert_eq!(buf.byte_capacity(), 64);
        assert_eq!(buf.bytes_used(), 0);
        assert!(buf.get(0).is_none());
        assert_eq!(buf.iter().count(), 0);
    }

    #[test]
    fn every_variant_survives_a_store_and_load() {
        let mut buf = EventBuffer::with_capacity(16, 32);
        let header = EventHeader::new(7, 2, EventFlags::IS_LIVE);
        let sysex = [0xF0u8, 0x7E, 0x00, 0x06, 0xF7];
        let custom = [1u8, 2, 3];
        let originals: Vec<DauxEvent<'_>> = vec![
            DauxEvent::NoteOn(NoteEvent {
                header,
                ..NoteEvent::default()
            }),
            DauxEvent::NoteOff(NoteEvent {
                header,
                ..NoteEvent::default()
            }),
            DauxEvent::NoteChoke(NoteEvent {
                header,
                ..NoteEvent::default()
            }),
            DauxEvent::NoteEnd(NoteEvent {
                header,
                ..NoteEvent::default()
            }),
            DauxEvent::NoteExpression(NoteExpressionEvent {
                header,
                expression: NoteExpression::Brightness,
                note_id: 3,
                channel: 1,
                key: 64,
                value: -0.25,
            }),
            DauxEvent::ParamValue(ParamEvent {
                header,
                param_id: 11,
                ..ParamEvent::default()
            }),
            DauxEvent::ParamMod(ParamEvent {
                header,
                param_id: 12,
                ..ParamEvent::default()
            }),
            DauxEvent::ParamGestureBegin(ParamGestureEvent {
                header,
                param_id: 13,
            }),
            DauxEvent::ParamGestureEnd(ParamGestureEvent {
                header,
                param_id: 13,
            }),
            DauxEvent::Transport(TransportEvent {
                header,
                transport: TransportSnapshot {
                    flags: transport_flags::HAS_TEMPO,
                    tempo: 120.0,
                    ..TransportSnapshot::unknown()
                },
            }),
            DauxEvent::Midi1(Midi1Event {
                header,
                message: Midi1Message::note_on(1, 60, 100),
            }),
            DauxEvent::Midi2(Midi2Event {
                header,
                packet: Ump::from_words2(0x4091_3C00, 1),
            }),
            DauxEvent::SysEx(SysExEvent {
                header,
                bytes: &sysex,
            }),
            DauxEvent::Custom(CustomEvent {
                header,
                kind: kind::CUSTOM + 1,
                bytes: &custom,
            }),
        ];
        for e in &originals {
            buf.try_push(e).expect("fits");
        }
        assert_eq!(buf.len(), originals.len());
        for (i, want) in originals.iter().enumerate() {
            let got = buf.get(i).expect("stored");
            assert_eq!(got, *want, "at {i}");
            assert_eq!(got.kind_bits(), want.kind_bits(), "at {i}");
        }
    }

    #[test]
    fn sorting_is_stable_for_equal_timestamps() {
        let mut buf = EventBuffer::with_capacity(16, 0);
        // Insertion order is the key order; times deliberately collide.
        for (time, key) in [(5u32, 0i16), (0, 1), (5, 2), (0, 3), (5, 4), (2, 5), (0, 6)] {
            push_note(&mut buf, time, key);
        }
        buf.sort_by_time();
        // Times ascending; inside each time, original insertion order preserved.
        assert_eq!(keys(&buf), [1, 3, 6, 5, 0, 2, 4]);
        let times: Vec<u32> = buf.iter().map(|e| e.time()).collect();
        assert_eq!(times, [0, 0, 0, 2, 5, 5, 5]);
    }

    #[test]
    fn sorting_is_idempotent() {
        let mut buf = EventBuffer::with_capacity(8, 0);
        for (time, key) in [(3u32, 0i16), (1, 1), (3, 2), (1, 3)] {
            push_note(&mut buf, time, key);
        }
        buf.sort_by_time();
        let once = keys(&buf);
        buf.sort_by_time();
        buf.sort_by_time();
        assert_eq!(keys(&buf), once);
        assert_eq!(once, [1, 3, 0, 2]);
    }

    #[test]
    fn events_pushed_after_a_sort_still_order_last_within_their_timestamp() {
        let mut buf = EventBuffer::with_capacity(8, 0);
        push_note(&mut buf, 4, 0);
        push_note(&mut buf, 1, 1);
        buf.sort_by_time();
        push_note(&mut buf, 4, 2);
        push_note(&mut buf, 1, 3);
        buf.sort_by_time();
        assert_eq!(keys(&buf), [1, 3, 0, 2]);
    }

    #[test]
    fn a_single_event_and_an_empty_buffer_sort_fine() {
        let mut empty = EventBuffer::with_capacity(4, 0);
        empty.sort_by_time();
        assert!(empty.is_empty());

        let mut one = EventBuffer::with_capacity(4, 0);
        push_note(&mut one, 9, 42);
        one.sort_by_time();
        assert_eq!(keys(&one), [42]);
    }

    #[test]
    fn already_sorted_and_reversed_inputs_both_come_out_sorted() {
        let mut ascending = EventBuffer::with_capacity(8, 0);
        let mut descending = EventBuffer::with_capacity(8, 0);
        for i in 0..8u32 {
            push_note(&mut ascending, i, i as i16);
            push_note(&mut descending, 7 - i, (7 - i) as i16);
        }
        ascending.sort_by_time();
        descending.sort_by_time();
        assert_eq!(keys(&ascending), [0, 1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(keys(&descending), [0, 1, 2, 3, 4, 5, 6, 7]);
    }

    #[test]
    fn a_full_record_list_overflows_instead_of_growing() {
        let mut buf = EventBuffer::with_capacity(2, 0);
        assert_eq!(buf.try_push(&DauxEvent::NoteOn(note_at(0, 1))), Ok(()));
        assert_eq!(buf.try_push(&DauxEvent::NoteOn(note_at(0, 2))), Ok(()));
        assert_eq!(
            buf.try_push(&DauxEvent::NoteOn(note_at(0, 3))),
            Err(EventOverflow)
        );
        assert_eq!(buf.len(), 2);
        assert_eq!(keys(&buf), [1, 2]);
        // Still full after a failed push, and still usable after a clear.
        assert_eq!(
            buf.try_push(&DauxEvent::NoteOn(note_at(0, 4))),
            Err(EventOverflow)
        );
        buf.clear();
        assert_eq!(buf.try_push(&DauxEvent::NoteOn(note_at(0, 5))), Ok(()));
    }

    #[test]
    fn a_capacity_one_buffer_holds_exactly_one_event() {
        let mut buf = EventBuffer::with_capacity(1, 1);
        assert_eq!(buf.capacity(), 1);
        assert_eq!(buf.try_push(&DauxEvent::NoteOn(note_at(0, 1))), Ok(()));
        assert_eq!(
            buf.try_push(&DauxEvent::NoteOn(note_at(0, 2))),
            Err(EventOverflow)
        );
        assert_eq!(buf.len(), 1);
        buf.sort_by_time();
        assert_eq!(keys(&buf), [1]);
    }

    #[test]
    fn a_zero_capacity_buffer_rejects_everything() {
        let mut buf = EventBuffer::with_capacity(0, 0);
        assert_eq!(
            buf.try_push(&DauxEvent::NoteOn(note_at(0, 1))),
            Err(EventOverflow)
        );
        assert!(buf.is_empty());
        buf.sort_by_time();
        buf.clear();
        assert!(buf.get(0).is_none());
    }

    #[test]
    fn a_full_arena_overflows_and_leaves_the_buffer_untouched() {
        let mut buf = EventBuffer::with_capacity(8, 4);
        let header = EventHeader::at(0);
        buf.try_push(&DauxEvent::SysEx(SysExEvent {
            header,
            bytes: &[1, 2, 3],
        }))
        .expect("fits");
        assert_eq!(buf.bytes_used(), 3);

        // Two more bytes do not fit in the remaining one.
        assert_eq!(
            buf.try_push(&DauxEvent::SysEx(SysExEvent {
                header,
                bytes: &[4, 5]
            })),
            Err(EventOverflow)
        );
        assert_eq!(buf.len(), 1, "the failed push must not add a record");
        assert_eq!(
            buf.bytes_used(),
            3,
            "the failed push must not consume arena bytes"
        );

        // Exactly one byte still fits.
        buf.try_push(&DauxEvent::SysEx(SysExEvent {
            header,
            bytes: &[4],
        }))
        .expect("fits");
        assert_eq!(buf.bytes_used(), 4);
        assert_eq!(
            buf.try_push(&DauxEvent::SysEx(SysExEvent {
                header,
                bytes: &[5]
            })),
            Err(EventOverflow)
        );

        // A zero-length payload always fits, even in a full arena.
        buf.try_push(&DauxEvent::SysEx(SysExEvent { header, bytes: &[] }))
            .expect("fits");
        assert_eq!(buf.len(), 3);
    }

    #[test]
    fn the_arena_is_reused_across_clears() {
        let mut buf = EventBuffer::with_capacity(4, 8);
        let header = EventHeader::at(0);
        let payload = [1u8, 2, 3, 4, 5, 6, 7, 8];
        // Far more total bytes than the arena holds, but never more at once.
        for round in 0..64u8 {
            buf.clear();
            assert_eq!(buf.bytes_used(), 0, "round {round}");
            buf.try_push(&DauxEvent::SysEx(SysExEvent {
                header,
                bytes: &payload,
            }))
            .unwrap_or_else(|_| panic!("round {round} should fit"));
            assert_eq!(buf.bytes_used(), 8, "round {round}");
            match buf.get(0).expect("stored") {
                DauxEvent::SysEx(s) => assert_eq!(s.bytes, &payload, "round {round}"),
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    #[test]
    fn several_payloads_share_the_arena_without_aliasing() {
        let mut buf = EventBuffer::with_capacity(4, 16);
        let header = EventHeader::at(0);
        buf.try_push(&DauxEvent::SysEx(SysExEvent {
            header,
            bytes: &[1, 2],
        }))
        .unwrap();
        buf.try_push(&DauxEvent::Custom(CustomEvent {
            header,
            kind: kind::CUSTOM,
            bytes: &[3, 4, 5],
        }))
        .unwrap();
        buf.try_push(&DauxEvent::SysEx(SysExEvent {
            header,
            bytes: &[6],
        }))
        .unwrap();
        assert_eq!(buf.bytes_used(), 6);
        assert_eq!(buf.get(0).unwrap().payload(), Some(&[1u8, 2][..]));
        assert_eq!(buf.get(1).unwrap().payload(), Some(&[3u8, 4, 5][..]));
        assert_eq!(buf.get(2).unwrap().payload(), Some(&[6u8][..]));
    }

    #[test]
    fn sorting_keeps_payloads_attached_to_their_events() {
        let mut buf = EventBuffer::with_capacity(4, 16);
        buf.try_push(&DauxEvent::SysEx(SysExEvent {
            header: EventHeader::at(9),
            bytes: &[9, 9],
        }))
        .unwrap();
        buf.try_push(&DauxEvent::SysEx(SysExEvent {
            header: EventHeader::at(1),
            bytes: &[1],
        }))
        .unwrap();
        buf.sort_by_time();
        assert_eq!(buf.get(0).unwrap().time(), 1);
        assert_eq!(buf.get(0).unwrap().payload(), Some(&[1u8][..]));
        assert_eq!(buf.get(1).unwrap().time(), 9);
        assert_eq!(buf.get(1).unwrap().payload(), Some(&[9u8, 9][..]));
    }

    #[test]
    fn reads_past_the_end_return_none() {
        let mut buf = EventBuffer::with_capacity(2, 0);
        push_note(&mut buf, 0, 1);
        assert!(buf.get(1).is_none());
        assert!(buf.get(usize::MAX).is_none());
    }

    #[test]
    fn it_works_through_both_trait_objects() {
        let mut buf = EventBuffer::with_capacity(4, 8);
        {
            let out: &mut dyn OutputEvents = buf.as_output();
            out.try_push(&DauxEvent::NoteOn(note_at(3, 1)))
                .expect("fits");
            out.try_push(&DauxEvent::SysEx(SysExEvent {
                header: EventHeader::at(1),
                bytes: &[7, 7],
            }))
            .expect("fits");
        }
        buf.sort_by_time();
        let input: &dyn InputEvents = buf.as_input();
        assert_eq!(input.len(), 2);
        assert!(!input.is_empty());
        let times: Vec<u32> = InputEventIter::new(input).map(|e| e.time()).collect();
        assert_eq!(times, [1, 3]);
    }

    #[test]
    fn pushing_does_not_reallocate_after_construction() {
        let mut buf = EventBuffer::with_capacity(64, 256);
        let slot_ptr = buf.slots.as_ptr();
        let byte_ptr = buf.bytes.as_ptr();
        let header = EventHeader::at(0);
        for i in 0..64u32 {
            let e = if i % 2 == 0 {
                DauxEvent::NoteOn(note_at(i, i as i16))
            } else {
                DauxEvent::SysEx(SysExEvent {
                    header,
                    bytes: &[1, 2, 3, 4],
                })
            };
            buf.try_push(&e).expect("fits");
        }
        buf.sort_by_time();
        assert_eq!(
            buf.slots.as_ptr(),
            slot_ptr,
            "record storage moved: it reallocated"
        );
        assert_eq!(buf.bytes.as_ptr(), byte_ptr, "arena moved: it reallocated");
    }
}
