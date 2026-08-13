//! MIDI 1.0 and MIDI 2.0 / UMP message types for DAUxPlug.
//!
//! A zero-dependency, allocation-free model of the two MIDI protocols a plug-in has to speak.
//! Every type here is `Copy` and small enough to pass by value; every function is a pure
//! transformation of its arguments. Nothing allocates, locks, blocks or panics, so the whole
//! crate is callable from the audio thread.
//!
//! # Layers
//!
//! * [`Midi1Message`] — a MIDI 1.0 byte-stream message, at most three bytes.
//! * [`Ump`] — a MIDI 2.0 Universal MIDI Packet: one to four 32-bit words, the transport for
//!   *everything* in MIDI 2.0. It mirrors `DauxEventMidi2V1` in
//!   `docs/specifications/abi-v1.md` §9 word for word.
//! * [`Midi2Message`] — a decoded MIDI 2.0 Channel Voice message, with the full 16-bit
//!   velocity, 32-bit controllers and note attributes. Packets it does not model are kept
//!   verbatim in [`Midi2Message::Other`].
//! * [`SysEx7`] — a borrowed view of a System Exclusive payload, with an allocation-free
//!   iterator that splits it into UMP data packets.
//!
//! # Translating between the two
//!
//! [`midi1_to_midi2`] and [`midi2_to_midi1`] use the MIDI 2.0 specification's
//! *min-center-max* scaling (see [`scaling`]), not a naive bit shift, so `0`, the centre and
//! the maximum all map exactly and widening then narrowing is the identity. The lossy
//! directions are enumerated in the [`convert`] module documentation.
//!
//! ```
//! use daux_midi::{Midi1Message, Midi2Message, midi1_to_midi2, midi2_to_midi1};
//!
//! let cc = Midi1Message::control_change(0, 7, 127);
//! let wide = midi1_to_midi2(cc, 0).unwrap();
//! assert_eq!(wide, Midi2Message::ControlChange {
//!     group: 0, channel: 0, index: 7, value: u32::MAX,
//! });
//! assert_eq!(midi2_to_midi1(&wide), Some(cc));
//! ```

#![forbid(unsafe_code)]

pub mod convert;
pub mod scaling;

mod midi1;
mod midi2;
mod sysex;
mod ump;

pub use convert::{midi1_to_midi2, midi1_to_ump, midi2_to_midi1, ump_to_midi1};
pub use midi1::{Midi1Kind, Midi1Message, status};
pub use midi2::{Midi2Message, NoteAttribute, midi2_status};
pub use sysex::{SysEx7, SysEx7UmpIter};
pub use ump::{Ump, message_type};

#[cfg(test)]
mod tests {
    use super::*;

    /// Everything crossing the audio thread must be `Send`; everything shared with the UI
    /// must be `Sync`.
    #[test]
    fn public_types_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Midi1Message>();
        assert_send_sync::<Midi1Kind>();
        assert_send_sync::<Ump>();
        assert_send_sync::<Midi2Message>();
        assert_send_sync::<NoteAttribute>();
        assert_send_sync::<SysEx7<'static>>();
    }

    #[test]
    fn message_types_stay_small_enough_to_pass_by_value() {
        assert_eq!(size_of::<Midi1Message>(), 3);
        assert_eq!(size_of::<Ump>(), 20);
        assert!(
            size_of::<Midi2Message>() <= 24,
            "{}",
            size_of::<Midi2Message>()
        );
    }

    /// A full trip through every layer: MIDI 1.0 bytes → MIDI 2.0 → UMP → MIDI 2.0 → bytes.
    #[test]
    fn end_to_end_layer_round_trip() {
        let original = Midi1Message::note_on(5, 64, 100);
        let wide = midi1_to_midi2(original, 2).expect("translatable");
        let packet = wide.to_ump();
        assert_eq!(packet.message_type(), message_type::MIDI2_CHANNEL_VOICE);
        assert_eq!(packet.group(), 2);
        let decoded = Midi2Message::from_ump(packet).expect("well formed");
        assert_eq!(decoded, wide);
        assert_eq!(midi2_to_midi1(&decoded), Some(original));
    }
}
