//! Translation between MIDI 1.0 and MIDI 2.0.
//!
//! # What is exact and what is lossy
//!
//! **MIDI 1.0 → MIDI 2.0 is lossless.** Every 7-bit value is widened with the specification's
//! min-center-max scaling ([`crate::scaling`]), which pins minimum, centre and maximum, so
//! `midi2_to_midi1(midi1_to_midi2(m))` returns `m` for every channel voice message. Two
//! deliberate exceptions, both mandated by the MIDI 2.0 translation rules:
//!
//! * A MIDI 1.0 Note On with velocity `0` means "note off" and is translated to a MIDI 2.0
//!   **Note Off** with velocity `0`. Round-tripping it therefore yields a Note Off, not the
//!   original Note On.
//! * A MIDI 2.0 Note On whose velocity scales down to `0` is emitted as a MIDI 1.0 Note On
//!   with velocity `1`, because velocity `0` would silently become a Note Off.
//!
//! **MIDI 2.0 → MIDI 1.0 is lossy** wherever MIDI 1.0 has no equivalent:
//!
//! * Controller resolution drops from 32 to 7 bits (14 for pitch bend) by truncation.
//! * Note attributes (including Pitch 7.9 microtuning) are dropped.
//! * Bank select on a Program Change is dropped — expressing it needs two extra Control
//!   Change messages, and this function returns a single message.
//! * Per-note pitch bend, registered and assignable controllers return `None`: MIDI 1.0 needs
//!   a multi-message RPN/NRPN sequence for them, which cannot be represented by one
//!   [`Midi1Message`].
//!
//! # What is out of scope
//!
//! [`midi1_to_midi2`] is **stateless**. The MIDI 2.0 specification's full translator is a
//! state machine that folds `CC 0/32` into a bank select and `CC 6/38/98/99/100/101` into
//! RPN/NRPN messages. Doing that requires per-channel state that must live in the caller (a
//! plug-in's own MIDI input stage), so bank-select and RPN/NRPN controllers are passed
//! through here as plain Control Change messages.

use crate::midi1::{Midi1Kind, Midi1Message, status};
use crate::midi2::{Midi2Message, NoteAttribute};
use crate::scaling;
use crate::ump::{Ump, message_type};

/// [audio-thread] Packs a MIDI 1.0 message into a Universal MIDI Packet without changing its
/// resolution.
///
/// Channel voice messages become message type `0x2` (MIDI 1.0 Channel Voice) and system
/// messages become message type `0x1` (System), both one word. Returns `None` for a malformed
/// message and for the System Exclusive delimiters `0xF0`/`0xF7`, whose payload does not fit
/// in a `Midi1Message` — use [`crate::SysEx7::ump_packets`] for those.
pub fn midi1_to_ump(m: Midi1Message, group: u8) -> Option<Ump> {
    if !m.is_valid() {
        return None;
    }
    let mt = if m.is_system() {
        if matches!(m.status_byte(), status::SYSEX_START | status::SYSEX_END) {
            return None;
        }
        message_type::SYSTEM
    } else {
        message_type::MIDI1_CHANNEL_VOICE
    };
    Some(Ump::from_word(
        ((mt as u32) << 28)
            | (((group as u32) & 0x0F) << 24)
            | ((m.bytes[0] as u32) << 16)
            | (((m.bytes[1] as u32) & 0x7F) << 8)
            | ((m.bytes[2] as u32) & 0x7F),
    ))
}

/// [audio-thread] Unpacks a MIDI 1.0 message from a message type `0x1` or `0x2` Universal
/// MIDI Packet. Returns `None` for any other message type or a malformed packet.
pub fn ump_to_midi1(u: Ump) -> Option<Midi1Message> {
    if !u.is_well_formed() {
        return None;
    }
    match u.message_type() {
        message_type::SYSTEM | message_type::MIDI1_CHANNEL_VOICE => {}
        _ => return None,
    }
    let m = Midi1Message::new([
        ((u.words[0] >> 16) & 0xFF) as u8,
        ((u.words[0] >> 8) & 0x7F) as u8,
        (u.words[0] & 0x7F) as u8,
    ]);
    if m.is_valid() { Some(m) } else { None }
}

/// [audio-thread] Widens a MIDI 1.0 message to MIDI 2.0.
///
/// Channel voice messages become the matching [`Midi2Message`] variant with min-center-max
/// scaled values; system messages become [`Midi2Message::Other`] holding a message type `0x1`
/// packet. Returns `None` for a malformed message or a System Exclusive delimiter. See the
/// [module documentation](self) for the exact translation rules.
pub fn midi1_to_midi2(m: Midi1Message, group: u8) -> Option<Midi2Message> {
    if !m.is_valid() {
        return None;
    }
    let group = group & 0x0F;
    let channel = m.channel();
    Some(match m.kind() {
        Midi1Kind::NoteOff => Midi2Message::NoteOff {
            group,
            channel,
            note: m.data1() & 0x7F,
            velocity: scaling::u7_to_u16(m.data2()),
            attribute: NoteAttribute::None,
        },
        Midi1Kind::NoteOn => {
            // MIDI 1.0 Note On with velocity 0 is a Note Off; MIDI 2.0 has no such rule, so
            // the meaning must be made explicit here.
            if m.data2() == 0 {
                Midi2Message::NoteOff {
                    group,
                    channel,
                    note: m.data1() & 0x7F,
                    velocity: 0,
                    attribute: NoteAttribute::None,
                }
            } else {
                Midi2Message::NoteOn {
                    group,
                    channel,
                    note: m.data1() & 0x7F,
                    velocity: scaling::u7_to_u16(m.data2()),
                    attribute: NoteAttribute::None,
                }
            }
        }
        Midi1Kind::PolyPressure => Midi2Message::PolyPressure {
            group,
            channel,
            note: m.data1() & 0x7F,
            value: scaling::u7_to_u32(m.data2()),
        },
        Midi1Kind::ControlChange => Midi2Message::ControlChange {
            group,
            channel,
            index: m.data1() & 0x7F,
            value: scaling::u7_to_u32(m.data2()),
        },
        Midi1Kind::ProgramChange => Midi2Message::ProgramChange {
            group,
            channel,
            program: m.data1() & 0x7F,
            bank: None,
        },
        Midi1Kind::ChannelPressure => Midi2Message::ChannelPressure {
            group,
            channel,
            value: scaling::u7_to_u32(m.data1()),
        },
        Midi1Kind::PitchBend => Midi2Message::PitchBend {
            group,
            channel,
            value: scaling::u14_to_u32(m.pitch_bend_value()),
        },
        Midi1Kind::System => Midi2Message::Other(midi1_to_ump(m, group)?),
    })
}

/// [audio-thread] Narrows a MIDI 2.0 message to MIDI 1.0.
///
/// Returns `None` when the message has no single-message MIDI 1.0 equivalent: per-note pitch
/// bend, registered and assignable controllers, and any [`Midi2Message::Other`] packet that
/// is not a message type `0x1`/`0x2` MIDI 1.0 packet. See the
/// [module documentation](self) for what is dropped.
pub fn midi2_to_midi1(m: &Midi2Message) -> Option<Midi1Message> {
    Some(match *m {
        Midi2Message::NoteOff {
            channel,
            note,
            velocity,
            ..
        } => Midi1Message::note_off(channel, note, scaling::u16_to_u7(velocity)),
        Midi2Message::NoteOn {
            channel,
            note,
            velocity,
            ..
        } => {
            // Velocity 0 would be read as a Note Off by a MIDI 1.0 receiver.
            let v = scaling::u16_to_u7(velocity).max(1);
            Midi1Message::note_on(channel, note, v)
        }
        Midi2Message::PolyPressure {
            channel,
            note,
            value,
            ..
        } => Midi1Message::poly_pressure(channel, note, scaling::u32_to_u7(value)),
        Midi2Message::ControlChange {
            channel,
            index,
            value,
            ..
        } => Midi1Message::control_change(channel, index, scaling::u32_to_u7(value)),
        Midi2Message::ProgramChange {
            channel, program, ..
        } => Midi1Message::program_change(channel, program),
        Midi2Message::ChannelPressure { channel, value, .. } => {
            Midi1Message::channel_pressure(channel, scaling::u32_to_u7(value))
        }
        Midi2Message::PitchBend { channel, value, .. } => {
            Midi1Message::pitch_bend(channel, scaling::u32_to_u14(value))
        }
        Midi2Message::PerNotePitchBend { .. }
        | Midi2Message::RegisteredController { .. }
        | Midi2Message::AssignableController { .. } => return None,
        Midi2Message::Other(u) => return ump_to_midi1(u),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::midi2::midi2_status;

    #[test]
    fn channel_voice_messages_round_trip_exactly() {
        for ch in [0u8, 7, 15] {
            let cases = [
                Midi1Message::note_off(ch, 60, 64),
                Midi1Message::note_on(ch, 60, 1),
                Midi1Message::note_on(ch, 127, 127),
                Midi1Message::poly_pressure(ch, 60, 90),
                Midi1Message::control_change(ch, 74, 0),
                Midi1Message::control_change(ch, 74, 127),
                Midi1Message::program_change(ch, 42),
                Midi1Message::channel_pressure(ch, 33),
                Midi1Message::pitch_bend(ch, 8192),
                Midi1Message::pitch_bend(ch, 0),
                Midi1Message::pitch_bend(ch, 16383),
            ];
            for m in cases {
                let m2 = midi1_to_midi2(m, 3).expect("translatable");
                let back = midi2_to_midi1(&m2).expect("representable");
                assert_eq!(back, m, "{m:?} -> {m2:?}");
                assert_eq!(m2.group(), 3, "{m:?}");
            }
        }
    }

    #[test]
    fn every_velocity_and_controller_value_round_trips() {
        for v in 0u8..=127 {
            let cc = Midi1Message::control_change(0, 7, v);
            assert_eq!(midi2_to_midi1(&midi1_to_midi2(cc, 0).unwrap()).unwrap(), cc);

            let pressure = Midi1Message::channel_pressure(0, v);
            assert_eq!(
                midi2_to_midi1(&midi1_to_midi2(pressure, 0).unwrap()).unwrap(),
                pressure
            );

            let off = Midi1Message::note_off(0, 60, v);
            assert_eq!(
                midi2_to_midi1(&midi1_to_midi2(off, 0).unwrap()).unwrap(),
                off
            );

            if v > 0 {
                let on = Midi1Message::note_on(0, 60, v);
                assert_eq!(midi2_to_midi1(&midi1_to_midi2(on, 0).unwrap()).unwrap(), on);
            }
        }
        for v in (0u16..=16383).step_by(11) {
            let pb = Midi1Message::pitch_bend(2, v);
            assert_eq!(midi2_to_midi1(&midi1_to_midi2(pb, 0).unwrap()).unwrap(), pb);
        }
    }

    #[test]
    fn note_on_velocity_zero_becomes_a_note_off() {
        let m = Midi1Message::note_on(4, 60, 0);
        let m2 = midi1_to_midi2(m, 0).expect("translatable");
        assert_eq!(
            m2,
            Midi2Message::NoteOff {
                group: 0,
                channel: 4,
                note: 60,
                velocity: 0,
                attribute: NoteAttribute::None,
            }
        );
        assert_eq!(midi2_to_midi1(&m2), Some(Midi1Message::note_off(4, 60, 0)));
    }

    #[test]
    fn tiny_midi2_velocity_never_becomes_a_note_off() {
        for velocity in [0u16, 1, 0x01FF, 0x0200] {
            let m = Midi2Message::NoteOn {
                group: 0,
                channel: 0,
                note: 60,
                velocity,
                attribute: NoteAttribute::None,
            };
            let m1 = midi2_to_midi1(&m).expect("representable");
            assert_eq!(m1.kind(), Midi1Kind::NoteOn);
            assert!(
                m1.data2() >= 1,
                "velocity {velocity} collapsed to a note off"
            );
        }
        // A Note Off may legitimately keep velocity 0.
        let off = Midi2Message::NoteOff {
            group: 0,
            channel: 0,
            note: 60,
            velocity: 0,
            attribute: NoteAttribute::None,
        };
        assert_eq!(midi2_to_midi1(&off).unwrap().data2(), 0);
    }

    #[test]
    fn scaled_values_land_on_the_documented_anchors() {
        let full = midi1_to_midi2(Midi1Message::control_change(0, 7, 127), 0).unwrap();
        assert_eq!(
            full,
            Midi2Message::ControlChange {
                group: 0,
                channel: 0,
                index: 7,
                value: 0xFFFF_FFFF
            }
        );
        let center = midi1_to_midi2(Midi1Message::control_change(0, 7, 64), 0).unwrap();
        assert_eq!(
            center,
            Midi2Message::ControlChange {
                group: 0,
                channel: 0,
                index: 7,
                value: 0x8000_0000
            }
        );
        let zero = midi1_to_midi2(Midi1Message::control_change(0, 7, 0), 0).unwrap();
        assert_eq!(
            zero,
            Midi2Message::ControlChange {
                group: 0,
                channel: 0,
                index: 7,
                value: 0
            }
        );

        let bend = midi1_to_midi2(Midi1Message::pitch_bend(0, 8192), 0).unwrap();
        assert_eq!(
            bend,
            Midi2Message::PitchBend {
                group: 0,
                channel: 0,
                value: 0x8000_0000
            }
        );

        let vel = midi1_to_midi2(Midi1Message::note_on(0, 60, 127), 0).unwrap();
        assert_eq!(
            vel,
            Midi2Message::NoteOn {
                group: 0,
                channel: 0,
                note: 60,
                velocity: 0xFFFF,
                attribute: NoteAttribute::None,
            }
        );
    }

    #[test]
    fn system_messages_become_type_1_packets_and_come_back() {
        for m in [
            Midi1Message::timing_clock(),
            Midi1Message::start(),
            Midi1Message::continue_(),
            Midi1Message::stop(),
            Midi1Message::active_sensing(),
            Midi1Message::system_reset(),
            Midi1Message::tune_request(),
            Midi1Message::song_select(9),
            Midi1Message::song_position(1234),
            Midi1Message::time_code_quarter_frame(0x21),
        ] {
            let m2 = midi1_to_midi2(m, 6).expect("translatable");
            match m2 {
                Midi2Message::Other(u) => {
                    assert_eq!(u.message_type(), message_type::SYSTEM, "{m:?}");
                    assert_eq!(u.group(), 6, "{m:?}");
                    assert_eq!(u.len, 1, "{m:?}");
                }
                other => panic!("{m:?} became {other:?}"),
            }
            assert_eq!(midi2_to_midi1(&m2), Some(m), "{m:?}");
        }
    }

    #[test]
    fn sysex_delimiters_and_malformed_messages_are_rejected() {
        for raw in [status::SYSEX_START, status::SYSEX_END, 0x00, 0x7F] {
            let m = Midi1Message::new([raw, 0, 0]);
            assert_eq!(midi1_to_midi2(m, 0), None, "0x{raw:02X}");
            assert_eq!(midi1_to_ump(m, 0), None, "0x{raw:02X}");
        }
    }

    #[test]
    fn midi1_packing_uses_message_type_2_for_channel_voice() {
        let u = midi1_to_ump(Midi1Message::note_on(9, 60, 100), 2).unwrap();
        assert_eq!(u.message_type(), message_type::MIDI1_CHANNEL_VOICE);
        assert_eq!(u.group(), 2);
        assert_eq!(u.words[0], 0x2299_3C64);
        assert_eq!(u.len, 1);
        assert_eq!(ump_to_midi1(u), Some(Midi1Message::note_on(9, 60, 100)));
    }

    #[test]
    fn ump_to_midi1_rejects_other_message_types() {
        assert_eq!(ump_to_midi1(Ump::from_words2(0x4090_3C00, 0)), None);
        assert_eq!(ump_to_midi1(Ump::from_words2(0x3000_0000, 0)), None);
        assert_eq!(ump_to_midi1(Ump::from_word(0x0000_0000)), None);
        // Malformed length.
        assert_eq!(
            ump_to_midi1(Ump {
                words: [0x2090_3C64, 0, 0, 0],
                len: 2
            }),
            None
        );
        // Type 2 packet whose "status byte" is not a status byte.
        assert_eq!(ump_to_midi1(Ump::from_word(0x2000_0000)), None);
    }

    #[test]
    fn midi2_only_messages_have_no_single_midi1_equivalent() {
        let cases = [
            Midi2Message::PerNotePitchBend {
                group: 0,
                channel: 0,
                note: 60,
                value: 0,
            },
            Midi2Message::RegisteredController {
                group: 0,
                channel: 0,
                bank: 0,
                index: 1,
                value: 0,
            },
            Midi2Message::AssignableController {
                group: 0,
                channel: 0,
                bank: 0,
                index: 1,
                value: 0,
            },
        ];
        for m in cases {
            assert_eq!(midi2_to_midi1(&m), None, "{m:?}");
            // …but they still survive as UMP.
            assert_eq!(Midi2Message::from_ump(m.to_ump()), Some(m), "{m:?}");
        }
    }

    #[test]
    fn program_change_bank_is_dropped_when_narrowing() {
        let m = Midi2Message::ProgramChange {
            group: 0,
            channel: 3,
            program: 42,
            bank: Some((1, 2)),
        };
        assert_eq!(
            midi2_to_midi1(&m),
            Some(Midi1Message::program_change(3, 42))
        );
    }

    #[test]
    fn note_attributes_are_dropped_when_narrowing() {
        let m = Midi2Message::NoteOn {
            group: 0,
            channel: 0,
            note: 60,
            velocity: 0xFFFF,
            attribute: NoteAttribute::Pitch(0x0180),
        };
        assert_eq!(midi2_to_midi1(&m), Some(Midi1Message::note_on(0, 60, 127)));
    }

    #[test]
    fn translated_messages_encode_to_the_expected_ump_status() {
        let m2 = midi1_to_midi2(Midi1Message::note_on(0, 60, 100), 0).unwrap();
        assert_eq!(m2.to_ump().status(), midi2_status::NOTE_ON);
        let m2 = midi1_to_midi2(Midi1Message::poly_pressure(0, 60, 100), 0).unwrap();
        assert_eq!(m2.to_ump().status(), midi2_status::POLY_PRESSURE);
        let m2 = midi1_to_midi2(Midi1Message::pitch_bend(0, 100), 0).unwrap();
        assert_eq!(m2.to_ump().status(), midi2_status::PITCH_BEND);
    }

    #[test]
    fn group_is_masked_on_the_way_in() {
        let m2 = midi1_to_midi2(Midi1Message::note_on(0, 60, 1), 0xFF).unwrap();
        assert_eq!(m2.group(), 0x0F);
        let u = midi1_to_ump(Midi1Message::note_on(0, 60, 1), 0xFF).unwrap();
        assert_eq!(u.group(), 0x0F);
    }
}
