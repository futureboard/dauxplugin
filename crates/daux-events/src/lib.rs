//! Format-neutral, sample-accurate event model for DAUxPlug.
//!
//! Everything a plug-in receives or emits alongside audio — notes, per-note expression,
//! parameter automation and modulation, transport jumps, MIDI 1.0, MIDI 2.0 and System
//! Exclusive — is one [`DauxEvent`], stamped with a sample offset inside the current block.
//! The model is deliberately neutral: no VST3, CLAP or `.axt` concept appears here, and this
//! crate knows nothing about the flat ABI records. Translation to and from
//! `DauxEventHeaderV1` & co. lives in the format adapters, which depend on `daux-abi`; this
//! crate does not.
//!
//! # Real-time rules
//!
//! * [`DauxEvent`] is `Copy`. Variable-length payloads (`SysEx`, `Custom`) are **borrowed**
//!   from the block's storage, so constructing, matching and forwarding an event never
//!   allocates. The borrow is only valid for the duration of `process`.
//! * [`InputEvents`] is the read-only, time-sorted list for the block; [`OutputEvents`] is
//!   the bounded sink. `try_push` returning [`EventOverflow`] is a normal condition — the
//!   audio thread must handle it by dropping or deferring the event, never by allocating.
//! * [`EventBuffer`] is the owned implementation of both. It allocates exactly once, in
//!   [`EventBuffer::with_capacity`], and keeps a byte arena so variable-length events need no
//!   per-event allocation. [`EventBuffer::sort_by_time`] is a stable sort that needs no
//!   scratch buffer.
//!
//! # Ordering
//!
//! Input events arrive sorted by [`DauxEvent::time`], and events sharing a timestamp keep the
//! order the host queued them in. That tie-break is part of the ABI, not an implementation
//! detail: a note-off and the note-on that replaces it at the same sample must not swap.
//! [`EventBuffer::sort_by_time`] preserves it.
//!
//! ```
//! use daux_events::{DauxEvent, EventBuffer, EventHeader, InputEvents, NoteEvent};
//!
//! let mut buf = EventBuffer::with_capacity(8, 0);
//! for (time, key) in [(4u32, 60i16), (0, 62), (4, 64)] {
//!     let note = NoteEvent { header: EventHeader::at(time), key, ..NoteEvent::default() };
//!     buf.try_push(&DauxEvent::NoteOn(note)).unwrap();
//! }
//! buf.sort_by_time();
//!
//! let order: Vec<(u32, i16)> = buf
//!     .iter()
//!     .filter_map(|e| match e {
//!         DauxEvent::NoteOn(n) => Some((n.header.time, n.key)),
//!         _ => None,
//!     })
//!     .collect();
//! // Sorted by time, and the two events at time 4 kept their insertion order.
//! assert_eq!(order, [(0, 62), (4, 60), (4, 64)]);
//! ```

#![forbid(unsafe_code)]

mod buffer;
mod event;
mod traits;
mod transport;

pub use buffer::EventBuffer;
pub use event::{
    CustomEvent, DauxEvent, EventFlags, EventHeader, Midi1Event, Midi2Event, NoteEvent,
    NoteExpression, NoteExpressionEvent, ParamEvent, ParamGestureEvent, SysExEvent, TransportEvent,
    kind,
};
pub use traits::{EventOverflow, InputEventIter, InputEvents, OutputEvents};
pub use transport::{TransportSnapshot, transport_flags};

/// Re-exported so a downstream crate can name MIDI payloads without depending on
/// `daux-midi` directly.
pub use daux_midi;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffers_cross_threads_and_events_are_shareable() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<EventBuffer>();
        assert_send_sync::<DauxEvent<'static>>();
        assert_send_sync::<EventHeader>();
        assert_send_sync::<EventFlags>();
        assert_send_sync::<TransportSnapshot>();
        assert_send_sync::<EventOverflow>();
    }

    /// A realistic block: a host fills the input list out of order, the plug-in reads it and
    /// answers on a smaller output that eventually overflows.
    #[test]
    fn one_block_end_to_end() {
        let mut input = EventBuffer::with_capacity(8, 32);
        let mut output = EventBuffer::with_capacity(2, 8);

        let sysex = [0xF0u8, 0x7E, 0x7F, 0x06, 0x01, 0xF7];
        input
            .try_push(&DauxEvent::SysEx(SysExEvent {
                header: EventHeader::at(64),
                bytes: &sysex,
            }))
            .unwrap();
        for (time, key) in [(32u32, 60i16), (0, 48), (32, 62)] {
            let note = NoteEvent {
                header: EventHeader::at(time),
                key,
                ..NoteEvent::default()
            };
            input.try_push(&DauxEvent::NoteOn(note)).unwrap();
        }
        input.sort_by_time();

        // The plug-in echoes every note as a note-end; the output is only two deep.
        let mut overflowed = 0usize;
        let mut echoed = 0usize;
        for i in 0..InputEvents::len(&input) {
            let e = input.get(i).expect("in range");
            if let DauxEvent::NoteOn(n) = e {
                match output.try_push(&DauxEvent::NoteEnd(n)) {
                    Ok(()) => echoed += 1,
                    Err(EventOverflow) => overflowed += 1,
                }
            }
        }
        assert_eq!(echoed, 2);
        assert_eq!(overflowed, 1, "the third note must overflow, not allocate");

        let times: Vec<u32> = input.iter().map(|e| e.time()).collect();
        assert_eq!(times, [0, 32, 32, 64]);
        assert_eq!(input.get(3).and_then(|e| e.payload()), Some(&sysex[..]));
        assert_eq!(output.len(), 2);
        assert_eq!(output.get(0).map(|e| e.kind_bits()), Some(kind::NOTE_END));
    }

    #[test]
    fn midi_payloads_come_back_unchanged() {
        use daux_midi::{Midi1Message, midi1_to_midi2};

        let mut buf = EventBuffer::with_capacity(4, 0);
        let message = Midi1Message::control_change(2, 74, 127);
        let packet = midi1_to_midi2(message, 3).expect("translatable").to_ump();
        buf.try_push(&DauxEvent::Midi1(Midi1Event {
            header: EventHeader::at(0),
            message,
        }))
        .unwrap();
        buf.try_push(&DauxEvent::Midi2(Midi2Event {
            header: EventHeader::at(1),
            packet,
        }))
        .unwrap();

        match buf.get(0).expect("stored") {
            DauxEvent::Midi1(e) => assert_eq!(e.message, message),
            other => panic!("unexpected {other:?}"),
        }
        match buf.get(1).expect("stored") {
            DauxEvent::Midi2(e) => {
                assert_eq!(e.packet, packet);
                assert_eq!(e.packet.group(), 3);
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
