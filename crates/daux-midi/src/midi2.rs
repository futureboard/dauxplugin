//! MIDI 2.0 Channel Voice messages (UMP message type `0x4`).

use crate::ump::{Ump, message_type};

/// MIDI 2.0 Channel Voice status nibbles (bits 23..20 of the first UMP word).
pub mod midi2_status {
    /// Registered Per-Note Controller.
    pub const REGISTERED_PER_NOTE_CONTROLLER: u8 = 0x0;
    /// Assignable Per-Note Controller.
    pub const ASSIGNABLE_PER_NOTE_CONTROLLER: u8 = 0x1;
    /// Registered Controller (RPN).
    pub const REGISTERED_CONTROLLER: u8 = 0x2;
    /// Assignable Controller (NRPN).
    pub const ASSIGNABLE_CONTROLLER: u8 = 0x3;
    /// Relative Registered Controller.
    pub const RELATIVE_REGISTERED_CONTROLLER: u8 = 0x4;
    /// Relative Assignable Controller.
    pub const RELATIVE_ASSIGNABLE_CONTROLLER: u8 = 0x5;
    /// Per-Note Pitch Bend.
    pub const PER_NOTE_PITCH_BEND: u8 = 0x6;
    /// Note Off.
    pub const NOTE_OFF: u8 = 0x8;
    /// Note On.
    pub const NOTE_ON: u8 = 0x9;
    /// Polyphonic Pressure.
    pub const POLY_PRESSURE: u8 = 0xA;
    /// Control Change.
    pub const CONTROL_CHANGE: u8 = 0xB;
    /// Program Change.
    pub const PROGRAM_CHANGE: u8 = 0xC;
    /// Channel Pressure.
    pub const CHANNEL_PRESSURE: u8 = 0xD;
    /// Pitch Bend.
    pub const PITCH_BEND: u8 = 0xE;
    /// Per-Note Management.
    pub const PER_NOTE_MANAGEMENT: u8 = 0xF;
}

/// The attribute carried by a MIDI 2.0 Note On / Note Off message.
///
/// The attribute is a `(type, data)` pair: an 8-bit type in the first UMP word and 16 bits of
/// data in the low half of the second. [`NoteAttribute::from_parts`] is the canonical
/// constructor and is what [`Midi2Message::from_ump`] produces, so
/// `NoteAttribute::from_parts(a.kind(), a.data()) == a` holds for every value it returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum NoteAttribute {
    /// Type `0x00`: no attribute. The data field is zero.
    #[default]
    None,
    /// Type `0x01`: manufacturer specific.
    ManufacturerSpecific(u16),
    /// Type `0x02`: profile specific.
    ProfileSpecific(u16),
    /// Type `0x03`: Pitch 7.9 — a note number in Q7.9 fixed point, so the low nine bits are
    /// a fraction of a semitone.
    Pitch(u16),
    /// Any attribute type this crate does not model.
    Other {
        /// The raw attribute type byte.
        kind: u8,
        /// The raw 16-bit attribute data.
        data: u16,
    },
}

impl NoteAttribute {
    /// [any-thread] Interprets a raw `(type, data)` pair.
    pub const fn from_parts(kind: u8, data: u16) -> Self {
        match kind {
            0x00 => Self::None,
            0x01 => Self::ManufacturerSpecific(data),
            0x02 => Self::ProfileSpecific(data),
            0x03 => Self::Pitch(data),
            _ => Self::Other { kind, data },
        }
    }

    /// [any-thread] The attribute type byte.
    pub const fn kind(self) -> u8 {
        match self {
            Self::None => 0x00,
            Self::ManufacturerSpecific(_) => 0x01,
            Self::ProfileSpecific(_) => 0x02,
            Self::Pitch(_) => 0x03,
            Self::Other { kind, .. } => kind,
        }
    }

    /// [any-thread] The 16-bit attribute data, zero for [`NoteAttribute::None`].
    pub const fn data(self) -> u16 {
        match self {
            Self::None => 0,
            Self::ManufacturerSpecific(d) | Self::ProfileSpecific(d) | Self::Pitch(d) => d,
            Self::Other { data, .. } => data,
        }
    }
}

/// A decoded MIDI 2.0 message.
///
/// Every modelled variant is a Channel Voice message (UMP message type `0x4`, two words).
/// Anything else — utility, system, SysEx, flex data, stream, and the Channel Voice statuses
/// this crate does not decode — is carried verbatim in [`Midi2Message::Other`], so no
/// information is lost when routing a stream through this type.
///
/// `group` and `channel` are always masked to four bits and `note`/`index`/`bank` to seven on
/// encode; controller values are full-range as MIDI 2.0 intends.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Midi2Message {
    /// Note Off with 16-bit velocity and an attribute.
    NoteOff {
        /// UMP group `0..=15`.
        group: u8,
        /// Channel `0..=15`.
        channel: u8,
        /// Note number `0..=127`.
        note: u8,
        /// 16-bit release velocity.
        velocity: u16,
        /// Note attribute.
        attribute: NoteAttribute,
    },
    /// Note On with 16-bit velocity and an attribute.
    ///
    /// Unlike MIDI 1.0, a velocity of zero is a genuine Note On and does **not** mean
    /// Note Off.
    NoteOn {
        /// UMP group `0..=15`.
        group: u8,
        /// Channel `0..=15`.
        channel: u8,
        /// Note number `0..=127`.
        note: u8,
        /// 16-bit attack velocity.
        velocity: u16,
        /// Note attribute.
        attribute: NoteAttribute,
    },
    /// Polyphonic (per-note) pressure with 32-bit resolution.
    PolyPressure {
        /// UMP group `0..=15`.
        group: u8,
        /// Channel `0..=15`.
        channel: u8,
        /// Note number `0..=127`.
        note: u8,
        /// 32-bit pressure.
        value: u32,
    },
    /// Control Change with 32-bit resolution.
    ControlChange {
        /// UMP group `0..=15`.
        group: u8,
        /// Channel `0..=15`.
        channel: u8,
        /// Controller number `0..=127`.
        index: u8,
        /// 32-bit controller value.
        value: u32,
    },
    /// Program Change, optionally with a 14-bit bank select.
    ProgramChange {
        /// UMP group `0..=15`.
        group: u8,
        /// Channel `0..=15`.
        channel: u8,
        /// Program number `0..=127`.
        program: u8,
        /// `Some((msb, lsb))` when the bank valid option flag is set, each `0..=127`.
        bank: Option<(u8, u8)>,
    },
    /// Channel Pressure with 32-bit resolution.
    ChannelPressure {
        /// UMP group `0..=15`.
        group: u8,
        /// Channel `0..=15`.
        channel: u8,
        /// 32-bit pressure.
        value: u32,
    },
    /// Channel-wide Pitch Bend with 32-bit resolution, centre `0x8000_0000`.
    PitchBend {
        /// UMP group `0..=15`.
        group: u8,
        /// Channel `0..=15`.
        channel: u8,
        /// 32-bit bend position.
        value: u32,
    },
    /// Per-note Pitch Bend with 32-bit resolution, centre `0x8000_0000`.
    PerNotePitchBend {
        /// UMP group `0..=15`.
        group: u8,
        /// Channel `0..=15`.
        channel: u8,
        /// Note number `0..=127`.
        note: u8,
        /// 32-bit bend position.
        value: u32,
    },
    /// Registered Controller (the MIDI 2.0 successor to RPN).
    RegisteredController {
        /// UMP group `0..=15`.
        group: u8,
        /// Channel `0..=15`.
        channel: u8,
        /// Controller bank `0..=127`.
        bank: u8,
        /// Controller index within the bank, `0..=127`.
        index: u8,
        /// 32-bit controller value.
        value: u32,
    },
    /// Assignable Controller (the MIDI 2.0 successor to NRPN).
    AssignableController {
        /// UMP group `0..=15`.
        group: u8,
        /// Channel `0..=15`.
        channel: u8,
        /// Controller bank `0..=127`.
        bank: u8,
        /// Controller index within the bank, `0..=127`.
        index: u8,
        /// 32-bit controller value.
        value: u32,
    },
    /// Any packet this crate does not decode, kept verbatim.
    Other(
        /// The undecoded packet.
        Ump,
    ),
}

impl Midi2Message {
    /// [any-thread] Encodes the message as a Universal MIDI Packet. Never allocates.
    pub const fn to_ump(self) -> Ump {
        match self {
            Self::NoteOff {
                group,
                channel,
                note,
                velocity,
                attribute,
            } => cv2(
                group,
                midi2_status::NOTE_OFF,
                channel,
                note & 0x7F,
                attribute.kind(),
                ((velocity as u32) << 16) | attribute.data() as u32,
            ),
            Self::NoteOn {
                group,
                channel,
                note,
                velocity,
                attribute,
            } => cv2(
                group,
                midi2_status::NOTE_ON,
                channel,
                note & 0x7F,
                attribute.kind(),
                ((velocity as u32) << 16) | attribute.data() as u32,
            ),
            Self::PolyPressure {
                group,
                channel,
                note,
                value,
            } => cv2(
                group,
                midi2_status::POLY_PRESSURE,
                channel,
                note & 0x7F,
                0,
                value,
            ),
            Self::ControlChange {
                group,
                channel,
                index,
                value,
            } => cv2(
                group,
                midi2_status::CONTROL_CHANGE,
                channel,
                index & 0x7F,
                0,
                value,
            ),
            Self::ProgramChange {
                group,
                channel,
                program,
                bank,
            } => {
                let (options, bank_bytes) = match bank {
                    Some((msb, lsb)) => {
                        (0x01u8, (((msb & 0x7F) as u32) << 8) | ((lsb & 0x7F) as u32))
                    }
                    None => (0x00u8, 0),
                };
                cv2(
                    group,
                    midi2_status::PROGRAM_CHANGE,
                    channel,
                    0,
                    options,
                    (((program & 0x7F) as u32) << 24) | bank_bytes,
                )
            }
            Self::ChannelPressure {
                group,
                channel,
                value,
            } => cv2(group, midi2_status::CHANNEL_PRESSURE, channel, 0, 0, value),
            Self::PitchBend {
                group,
                channel,
                value,
            } => cv2(group, midi2_status::PITCH_BEND, channel, 0, 0, value),
            Self::PerNotePitchBend {
                group,
                channel,
                note,
                value,
            } => cv2(
                group,
                midi2_status::PER_NOTE_PITCH_BEND,
                channel,
                note & 0x7F,
                0,
                value,
            ),
            Self::RegisteredController {
                group,
                channel,
                bank,
                index,
                value,
            } => cv2(
                group,
                midi2_status::REGISTERED_CONTROLLER,
                channel,
                bank & 0x7F,
                index & 0x7F,
                value,
            ),
            Self::AssignableController {
                group,
                channel,
                bank,
                index,
                value,
            } => cv2(
                group,
                midi2_status::ASSIGNABLE_CONTROLLER,
                channel,
                bank & 0x7F,
                index & 0x7F,
                value,
            ),
            Self::Other(u) => u,
        }
    }

    /// [any-thread] Decodes a Universal MIDI Packet.
    ///
    /// Returns `None` only when the packet is malformed — a `len` outside `1..=4` or a `len`
    /// that disagrees with the packet's message type. A well-formed packet always decodes:
    /// modelled MIDI 2.0 Channel Voice statuses become their own variant and everything else
    /// becomes [`Midi2Message::Other`].
    pub const fn from_ump(u: Ump) -> Option<Self> {
        if !u.is_well_formed() {
            return None;
        }
        if u.message_type() != message_type::MIDI2_CHANNEL_VOICE {
            return Some(Self::Other(u));
        }
        let group = u.group();
        let channel = u.channel();
        let byte2 = ((u.words[0] >> 8) & 0x7F) as u8;
        let byte3 = (u.words[0] & 0xFF) as u8;
        let data = u.words[1];
        Some(match u.status() {
            midi2_status::NOTE_OFF => Self::NoteOff {
                group,
                channel,
                note: byte2,
                velocity: (data >> 16) as u16,
                attribute: NoteAttribute::from_parts(byte3, data as u16),
            },
            midi2_status::NOTE_ON => Self::NoteOn {
                group,
                channel,
                note: byte2,
                velocity: (data >> 16) as u16,
                attribute: NoteAttribute::from_parts(byte3, data as u16),
            },
            midi2_status::POLY_PRESSURE => Self::PolyPressure {
                group,
                channel,
                note: byte2,
                value: data,
            },
            midi2_status::CONTROL_CHANGE => Self::ControlChange {
                group,
                channel,
                index: byte2,
                value: data,
            },
            midi2_status::PROGRAM_CHANGE => Self::ProgramChange {
                group,
                channel,
                program: ((data >> 24) & 0x7F) as u8,
                bank: if byte3 & 0x01 != 0 {
                    Some((((data >> 8) & 0x7F) as u8, (data & 0x7F) as u8))
                } else {
                    None
                },
            },
            midi2_status::CHANNEL_PRESSURE => Self::ChannelPressure {
                group,
                channel,
                value: data,
            },
            midi2_status::PITCH_BEND => Self::PitchBend {
                group,
                channel,
                value: data,
            },
            midi2_status::PER_NOTE_PITCH_BEND => Self::PerNotePitchBend {
                group,
                channel,
                note: byte2,
                value: data,
            },
            midi2_status::REGISTERED_CONTROLLER => Self::RegisteredController {
                group,
                channel,
                bank: byte2,
                index: byte3 & 0x7F,
                value: data,
            },
            midi2_status::ASSIGNABLE_CONTROLLER => Self::AssignableController {
                group,
                channel,
                bank: byte2,
                index: byte3 & 0x7F,
                value: data,
            },
            _ => Self::Other(u),
        })
    }

    /// [any-thread] The UMP group the message belongs to.
    pub const fn group(&self) -> u8 {
        match *self {
            Self::NoteOff { group, .. }
            | Self::NoteOn { group, .. }
            | Self::PolyPressure { group, .. }
            | Self::ControlChange { group, .. }
            | Self::ProgramChange { group, .. }
            | Self::ChannelPressure { group, .. }
            | Self::PitchBend { group, .. }
            | Self::PerNotePitchBend { group, .. }
            | Self::RegisteredController { group, .. }
            | Self::AssignableController { group, .. } => group & 0x0F,
            Self::Other(u) => u.group(),
        }
    }

    /// [any-thread] The channel, or `None` for [`Midi2Message::Other`] packets that are not
    /// channel voice messages.
    pub const fn channel(&self) -> Option<u8> {
        match *self {
            Self::NoteOff { channel, .. }
            | Self::NoteOn { channel, .. }
            | Self::PolyPressure { channel, .. }
            | Self::ControlChange { channel, .. }
            | Self::ProgramChange { channel, .. }
            | Self::ChannelPressure { channel, .. }
            | Self::PitchBend { channel, .. }
            | Self::PerNotePitchBend { channel, .. }
            | Self::RegisteredController { channel, .. }
            | Self::AssignableController { channel, .. } => Some(channel & 0x0F),
            Self::Other(u) => match u.message_type() {
                message_type::MIDI1_CHANNEL_VOICE | message_type::MIDI2_CHANNEL_VOICE => {
                    Some(u.channel())
                }
                _ => None,
            },
        }
    }
}

/// Packs a MIDI 2.0 Channel Voice packet.
const fn cv2(group: u8, status: u8, channel: u8, byte2: u8, byte3: u8, data: u32) -> Ump {
    let w0 = ((message_type::MIDI2_CHANNEL_VOICE as u32) << 28)
        | (((group as u32) & 0x0F) << 24)
        | (((status as u32) & 0x0F) << 20)
        | (((channel as u32) & 0x0F) << 16)
        | ((byte2 as u32) << 8)
        | (byte3 as u32);
    Ump::from_words2(w0, data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples() -> [Midi2Message; 11] {
        [
            Midi2Message::NoteOn {
                group: 1,
                channel: 2,
                note: 60,
                velocity: 0xFFFF,
                attribute: NoteAttribute::Pitch(0x1234),
            },
            Midi2Message::NoteOff {
                group: 0,
                channel: 15,
                note: 127,
                velocity: 0x0001,
                attribute: NoteAttribute::None,
            },
            Midi2Message::PolyPressure {
                group: 3,
                channel: 4,
                note: 7,
                value: 0xDEAD_BEEF,
            },
            Midi2Message::ControlChange {
                group: 5,
                channel: 6,
                index: 74,
                value: 0x8000_0000,
            },
            Midi2Message::ProgramChange {
                group: 7,
                channel: 8,
                program: 42,
                bank: None,
            },
            Midi2Message::ProgramChange {
                group: 7,
                channel: 8,
                program: 42,
                bank: Some((12, 34)),
            },
            Midi2Message::ChannelPressure {
                group: 9,
                channel: 10,
                value: 0x1234_5678,
            },
            Midi2Message::PitchBend {
                group: 11,
                channel: 12,
                value: 0x8000_0000,
            },
            Midi2Message::PerNotePitchBend {
                group: 13,
                channel: 14,
                note: 64,
                value: 0xFFFF_FFFF,
            },
            Midi2Message::RegisteredController {
                group: 15,
                channel: 0,
                bank: 3,
                index: 7,
                value: 0x0000_0001,
            },
            Midi2Message::AssignableController {
                group: 0,
                channel: 1,
                bank: 127,
                index: 127,
                value: 0xFFFF_FFFF,
            },
        ]
    }

    #[test]
    fn every_variant_round_trips_through_ump() {
        for m in samples() {
            let u = m.to_ump();
            assert_eq!(u.len, 2, "{m:?}");
            assert_eq!(u.message_type(), message_type::MIDI2_CHANNEL_VOICE, "{m:?}");
            assert!(u.is_well_formed(), "{m:?}");
            assert_eq!(Midi2Message::from_ump(u), Some(m), "{m:?}");
        }
    }

    #[test]
    fn group_and_channel_accessors_agree_with_the_packet() {
        for m in samples() {
            let u = m.to_ump();
            assert_eq!(m.group(), u.group(), "{m:?}");
            assert_eq!(m.channel(), Some(u.channel()), "{m:?}");
        }
    }

    #[test]
    fn note_on_packs_velocity_and_attribute_as_the_spec_says() {
        let m = Midi2Message::NoteOn {
            group: 0,
            channel: 0,
            note: 60,
            velocity: 0xABCD,
            attribute: NoteAttribute::ManufacturerSpecific(0x1234),
        };
        let u = m.to_ump();
        assert_eq!(u.words[0], 0x4090_3C01);
        assert_eq!(u.words[1], 0xABCD_1234);
    }

    #[test]
    fn note_attribute_parts_round_trip() {
        for kind in 0u8..=255 {
            let a = NoteAttribute::from_parts(kind, 0x9876);
            assert_eq!(a.kind(), kind);
            if kind == 0 {
                assert_eq!(a, NoteAttribute::None);
                assert_eq!(a.data(), 0);
            } else {
                assert_eq!(a.data(), 0x9876);
            }
            assert_eq!(NoteAttribute::from_parts(a.kind(), a.data()), a);
        }
        assert_eq!(NoteAttribute::default(), NoteAttribute::None);
    }

    #[test]
    fn program_change_bank_flag_controls_the_bank_field() {
        let without = Midi2Message::ProgramChange {
            group: 0,
            channel: 0,
            program: 5,
            bank: Some((1, 2)),
        }
        .to_ump();
        assert_eq!(without.words[0] & 0xFF, 0x01);
        assert_eq!(without.words[1], 0x0500_0102);

        let plain = Midi2Message::ProgramChange {
            group: 0,
            channel: 0,
            program: 5,
            bank: None,
        }
        .to_ump();
        assert_eq!(plain.words[0] & 0xFF, 0x00);
        assert_eq!(plain.words[1], 0x0500_0000);
    }

    #[test]
    fn out_of_range_fields_are_masked_not_leaked() {
        let u = Midi2Message::NoteOn {
            group: 0xFF,
            channel: 0xFF,
            note: 0xFF,
            velocity: 0xFFFF,
            attribute: NoteAttribute::None,
        }
        .to_ump();
        assert_eq!(u.message_type(), message_type::MIDI2_CHANNEL_VOICE);
        assert_eq!(u.group(), 0x0F);
        assert_eq!(u.channel(), 0x0F);
        assert_eq!(u.status(), midi2_status::NOTE_ON);
        assert_eq!((u.words[0] >> 8) & 0xFF, 0x7F);
    }

    #[test]
    fn unmodelled_statuses_and_message_types_survive_as_other() {
        // Per-Note Management: a MIDI 2.0 channel voice status we do not decode.
        let raw = Ump::from_words2(0x40F0_3C03, 0x0000_0000);
        let m = Midi2Message::from_ump(raw).expect("well formed");
        assert_eq!(m, Midi2Message::Other(raw));
        assert_eq!(m.to_ump(), raw);
        assert_eq!(m.channel(), Some(0));

        // A utility packet: not channel voice at all.
        let util = Ump::from_word(0x0000_0000);
        let m = Midi2Message::from_ump(util).expect("well formed");
        assert_eq!(m, Midi2Message::Other(util));
        assert_eq!(m.channel(), None);
        assert_eq!(m.group(), 0);
    }

    #[test]
    fn malformed_packets_are_rejected() {
        assert!(
            Midi2Message::from_ump(Ump {
                words: [0x4000_0000, 0, 0, 0],
                len: 0
            })
            .is_none()
        );
        assert!(
            Midi2Message::from_ump(Ump {
                words: [0x4000_0000, 0, 0, 0],
                len: 1
            })
            .is_none()
        );
        assert!(
            Midi2Message::from_ump(Ump {
                words: [0x4000_0000, 0, 0, 0],
                len: 5
            })
            .is_none()
        );
        assert!(
            Midi2Message::from_ump(Ump {
                words: [0x2000_0000, 0, 0, 0],
                len: 2
            })
            .is_none()
        );
    }

    #[test]
    fn note_on_velocity_zero_is_a_real_note_on() {
        let m = Midi2Message::NoteOn {
            group: 0,
            channel: 0,
            note: 60,
            velocity: 0,
            attribute: NoteAttribute::None,
        };
        assert_eq!(Midi2Message::from_ump(m.to_ump()), Some(m));
    }
}
