//! The event model: header, payload structs and the [`DauxEvent`] sum type.

use core::ops::{BitOr, BitOrAssign};

use daux_midi::{Midi1Message, SysEx7, Ump};

use crate::transport::TransportSnapshot;

/// Event kind codes, mirroring `DAUX_EVENT_*` in `docs/specifications/abi-v1.md` §9.
///
/// These are the values [`DauxEvent::kind_bits`] returns and the values an ABI writer puts in
/// `DauxEventHeaderV1::kind`. They are permanent: a code is never reused for a different
/// meaning.
pub mod kind {
    /// [`super::DauxEvent::NoteOn`].
    pub const NOTE_ON: u16 = 1;
    /// [`super::DauxEvent::NoteOff`].
    pub const NOTE_OFF: u16 = 2;
    /// [`super::DauxEvent::NoteChoke`].
    pub const NOTE_CHOKE: u16 = 3;
    /// [`super::DauxEvent::NoteEnd`], plug-in to host only.
    pub const NOTE_END: u16 = 4;
    /// [`super::DauxEvent::NoteExpression`].
    pub const NOTE_EXPRESSION: u16 = 5;
    /// [`super::DauxEvent::ParamValue`].
    pub const PARAM_VALUE: u16 = 6;
    /// [`super::DauxEvent::ParamMod`].
    pub const PARAM_MOD: u16 = 7;
    /// [`super::DauxEvent::ParamGestureBegin`].
    pub const PARAM_GESTURE_BEGIN: u16 = 8;
    /// [`super::DauxEvent::ParamGestureEnd`].
    pub const PARAM_GESTURE_END: u16 = 9;
    /// [`super::DauxEvent::Transport`].
    pub const TRANSPORT: u16 = 10;
    /// [`super::DauxEvent::Midi1`].
    pub const MIDI1: u16 = 11;
    /// [`super::DauxEvent::Midi2`].
    pub const MIDI2: u16 = 12;
    /// [`super::DauxEvent::SysEx`].
    pub const SYSEX: u16 = 13;
    /// First code of the vendor range. Every [`super::DauxEvent::Custom`] kind must be at or
    /// above this value so it can never collide with a standard code.
    pub const CUSTOM: u16 = 0x7000;
}

/// Per-event flags, mirroring `DAUX_EVENT_FLAG_*`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct EventFlags(u16);

impl EventFlags {
    /// No flags.
    pub const NONE: Self = Self(0);
    /// The event was performed live rather than played back from automation.
    pub const IS_LIVE: Self = Self(1 << 0);
    /// The host should not record this event.
    pub const DONT_RECORD: Self = Self(1 << 1);

    /// [any-thread] Wraps a raw bit set, preserving bits this version does not know about.
    pub const fn from_bits(bits: u16) -> Self {
        Self(bits)
    }

    /// [any-thread] The raw bit set.
    pub const fn bits(self) -> u16 {
        self.0
    }

    /// [any-thread] `true` when every flag in `other` is set.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// [any-thread] The union of two flag sets.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// [any-thread] Shorthand for `contains(EventFlags::IS_LIVE)`.
    pub const fn is_live(self) -> bool {
        self.contains(Self::IS_LIVE)
    }

    /// [any-thread] Shorthand for `contains(EventFlags::DONT_RECORD)`.
    pub const fn dont_record(self) -> bool {
        self.contains(Self::DONT_RECORD)
    }
}

impl BitOr for EventFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

impl BitOrAssign for EventFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// The part of an event every kind carries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct EventHeader {
    /// Sample offset inside the current block, `0 ..= frames - 1`.
    pub time: u32,
    /// Which event port the event belongs to.
    pub port_index: u16,
    /// `IS_LIVE` / `DONT_RECORD`.
    pub flags: EventFlags,
}

impl EventHeader {
    /// [any-thread] A header on port `0` with no flags.
    pub const fn at(time: u32) -> Self {
        Self {
            time,
            port_index: 0,
            flags: EventFlags::NONE,
        }
    }

    /// [any-thread] A fully specified header.
    pub const fn new(time: u32, port_index: u16, flags: EventFlags) -> Self {
        Self {
            time,
            port_index,
            flags,
        }
    }
}

/// Note on / off / choke / end.
///
/// `note_id` is the host's voice identifier, or `-1` when the host does not track voices, in
/// which case `(channel, key)` identifies the voice instead.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NoteEvent {
    /// Timing, port and flags.
    pub header: EventHeader,
    /// Host-assigned voice id, or `-1`.
    pub note_id: i32,
    /// MIDI channel `0..=15`, or `-1` as a wildcard on note off / choke.
    pub channel: i16,
    /// Key number `0..=127`, or `-1` as a wildcard on note off / choke.
    pub key: i16,
    /// Velocity `0.0 ..= 1.0`.
    pub velocity: f64,
    /// Tuning offset from equal temperament, in cents.
    pub tuning: f64,
}

impl Default for NoteEvent {
    /// [any-thread] A silent, unidentified note: every id field is the `-1` wildcard, not
    /// `0`, because `0` is a valid voice id, channel and key.
    fn default() -> Self {
        Self {
            header: EventHeader::at(0),
            note_id: -1,
            channel: -1,
            key: -1,
            velocity: 0.0,
            tuning: 0.0,
        }
    }
}

/// The dimension a [`NoteExpressionEvent`] modulates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NoteExpression {
    /// Per-note volume.
    Volume,
    /// Per-note pan.
    Pan,
    /// Per-note tuning, in cents.
    Tuning,
    /// Per-note vibrato depth.
    Vibrato,
    /// Per-note expression (MIDI CC 11 in spirit).
    Expression,
    /// Per-note brightness.
    Brightness,
    /// Per-note pressure / aftertouch.
    Pressure,
}

impl NoteExpression {
    /// [any-thread] The `DAUX_NOTE_EXPR_*` code for this expression.
    pub const fn as_bits(self) -> u32 {
        match self {
            Self::Volume => 0,
            Self::Pan => 1,
            Self::Tuning => 2,
            Self::Vibrato => 3,
            Self::Expression => 4,
            Self::Brightness => 5,
            Self::Pressure => 6,
        }
    }

    /// [any-thread] Decodes a `DAUX_NOTE_EXPR_*` code, or `None` for an unknown one.
    pub const fn from_bits(bits: u32) -> Option<Self> {
        Some(match bits {
            0 => Self::Volume,
            1 => Self::Pan,
            2 => Self::Tuning,
            3 => Self::Vibrato,
            4 => Self::Expression,
            5 => Self::Brightness,
            6 => Self::Pressure,
            _ => return None,
        })
    }
}

/// A per-note modulation of one expression dimension.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NoteExpressionEvent {
    /// Timing, port and flags.
    pub header: EventHeader,
    /// Which dimension is being modulated.
    pub expression: NoteExpression,
    /// Host-assigned voice id, or `-1`.
    pub note_id: i32,
    /// MIDI channel `0..=15`, or `-1`.
    pub channel: i16,
    /// Key number `0..=127`, or `-1`.
    pub key: i16,
    /// The new value. The meaningful range depends on `expression`.
    pub value: f64,
}

/// A parameter change: an absolute value, or a modulation offset.
///
/// When `note_id`, `channel` or `key` is set the change is scoped to a single voice
/// (per-note modulation); `-1` in all three means the change is channel-wide.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParamEvent {
    /// Timing, port and flags.
    pub header: EventHeader,
    /// The parameter's permanent id.
    pub param_id: u32,
    /// Host-assigned voice id, or `-1`.
    pub note_id: i32,
    /// MIDI channel `0..=15`, or `-1`.
    pub channel: i16,
    /// Key number `0..=127`, or `-1`.
    pub key: i16,
    /// Absolute plain value for `ParamValue`; a signed offset for `ParamMod`.
    pub value: f64,
}

impl Default for ParamEvent {
    /// [any-thread] A channel-wide change of parameter `0` to `0.0`: the voice-scoping
    /// fields are the `-1` wildcard, not `0`.
    fn default() -> Self {
        Self {
            header: EventHeader::at(0),
            param_id: 0,
            note_id: -1,
            channel: -1,
            key: -1,
            value: 0.0,
        }
    }
}

/// The beginning or end of a user gesture on a parameter (a knob grab and release).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ParamGestureEvent {
    /// Timing, port and flags.
    pub header: EventHeader,
    /// The parameter's permanent id.
    pub param_id: u32,
}

/// A sample-accurate transport discontinuity: a locate, a loop wrap or a tempo jump.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TransportEvent {
    /// Timing, port and flags.
    pub header: EventHeader,
    /// The host's transport state from this sample onwards.
    pub transport: TransportSnapshot,
}

/// A MIDI 1.0 message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Midi1Event {
    /// Timing, port and flags.
    pub header: EventHeader,
    /// The message itself.
    pub message: Midi1Message,
}

/// A MIDI 2.0 Universal MIDI Packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Midi2Event {
    /// Timing, port and flags.
    pub header: EventHeader,
    /// The packet itself, one to four words.
    pub packet: Ump,
}

/// A System Exclusive message whose bytes are borrowed for the duration of the block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SysExEvent<'a> {
    /// Timing, port and flags.
    pub header: EventHeader,
    /// The SysEx bytes, valid only for `'a` — never retain them past `process`.
    pub bytes: &'a [u8],
}

impl<'a> SysExEvent<'a> {
    /// [audio-thread] The payload as a [`SysEx7`] view, which knows how to strip the `0xF0`
    /// / `0xF7` delimiters and split the payload into UMP data packets.
    pub const fn sysex(&self) -> SysEx7<'a> {
        SysEx7::new(self.bytes)
    }
}

/// A vendor-defined event whose payload is borrowed for the duration of the block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CustomEvent<'a> {
    /// Timing, port and flags.
    pub header: EventHeader,
    /// The vendor event code. Must be at or above [`kind::CUSTOM`] so it cannot collide with
    /// a standard code.
    pub kind: u16,
    /// The payload, valid only for `'a`.
    pub bytes: &'a [u8],
}

/// One format-neutral, sample-accurate event.
///
/// Every variant is `Copy` and holds either inline data or a borrowed slice, so building,
/// reading and matching an event never allocates. The lifetime `'a` is the block the event
/// belongs to: `SysEx` and `Custom` payloads live in the host's event list (or in an
/// [`EventBuffer`](crate::EventBuffer)'s arena) and must not be retained past `process`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DauxEvent<'a> {
    /// A note starts.
    NoteOn(NoteEvent),
    /// A note is released.
    NoteOff(NoteEvent),
    /// A voice is cut without a release, e.g. by a drum choke group.
    NoteChoke(NoteEvent),
    /// A voice has finished; sent from the plug-in to the host so it can recycle the id.
    NoteEnd(NoteEvent),
    /// A per-note expression change.
    NoteExpression(NoteExpressionEvent),
    /// An absolute parameter value.
    ParamValue(ParamEvent),
    /// A signed parameter modulation offset.
    ParamMod(ParamEvent),
    /// The user grabbed a parameter.
    ParamGestureBegin(ParamGestureEvent),
    /// The user released a parameter.
    ParamGestureEnd(ParamGestureEvent),
    /// A transport discontinuity.
    Transport(TransportEvent),
    /// A MIDI 1.0 message.
    Midi1(Midi1Event),
    /// A MIDI 2.0 packet.
    Midi2(Midi2Event),
    /// A System Exclusive message.
    SysEx(SysExEvent<'a>),
    /// A vendor-defined event.
    Custom(CustomEvent<'a>),
}

impl<'a> DauxEvent<'a> {
    /// [audio-thread] The event's header.
    pub const fn header(&self) -> EventHeader {
        match *self {
            Self::NoteOn(e) | Self::NoteOff(e) | Self::NoteChoke(e) | Self::NoteEnd(e) => e.header,
            Self::NoteExpression(e) => e.header,
            Self::ParamValue(e) | Self::ParamMod(e) => e.header,
            Self::ParamGestureBegin(e) | Self::ParamGestureEnd(e) => e.header,
            Self::Transport(e) => e.header,
            Self::Midi1(e) => e.header,
            Self::Midi2(e) => e.header,
            Self::SysEx(e) => e.header,
            Self::Custom(e) => e.header,
        }
    }

    /// [audio-thread] Sample offset inside the current block.
    pub const fn time(&self) -> u32 {
        self.header().time
    }

    /// [audio-thread] Which event port the event belongs to.
    pub const fn port_index(&self) -> u16 {
        self.header().port_index
    }

    /// [audio-thread] The event's flags.
    pub const fn flags(&self) -> EventFlags {
        self.header().flags
    }

    /// [audio-thread] The `DAUX_EVENT_*` code for this event, as it would appear in a flat
    /// ABI record's `kind` field.
    ///
    /// For [`DauxEvent::Custom`] this is the vendor code carried by the event itself, not the
    /// [`kind::CUSTOM`] marker.
    pub const fn kind_bits(&self) -> u16 {
        match *self {
            Self::NoteOn(_) => kind::NOTE_ON,
            Self::NoteOff(_) => kind::NOTE_OFF,
            Self::NoteChoke(_) => kind::NOTE_CHOKE,
            Self::NoteEnd(_) => kind::NOTE_END,
            Self::NoteExpression(_) => kind::NOTE_EXPRESSION,
            Self::ParamValue(_) => kind::PARAM_VALUE,
            Self::ParamMod(_) => kind::PARAM_MOD,
            Self::ParamGestureBegin(_) => kind::PARAM_GESTURE_BEGIN,
            Self::ParamGestureEnd(_) => kind::PARAM_GESTURE_END,
            Self::Transport(_) => kind::TRANSPORT,
            Self::Midi1(_) => kind::MIDI1,
            Self::Midi2(_) => kind::MIDI2,
            Self::SysEx(_) => kind::SYSEX,
            Self::Custom(e) => e.kind,
        }
    }

    /// [audio-thread] The borrowed payload of a `SysEx` or `Custom` event, `None` otherwise.
    ///
    /// Useful for anything that has to know how many arena bytes an event needs. The slice
    /// keeps the event's own lifetime, not the borrow of `self`.
    pub const fn payload(&self) -> Option<&'a [u8]> {
        match *self {
            Self::SysEx(e) => Some(e.bytes),
            Self::Custom(e) => Some(e.bytes),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hdr(time: u32) -> EventHeader {
        EventHeader::at(time)
    }

    #[test]
    fn flags_behave_like_a_bit_set() {
        let none = EventFlags::NONE;
        assert_eq!(none.bits(), 0);
        assert_eq!(EventFlags::default(), none);
        assert!(!none.is_live());
        assert!(!none.dont_record());

        let both = EventFlags::IS_LIVE | EventFlags::DONT_RECORD;
        assert!(both.is_live());
        assert!(both.dont_record());
        assert!(both.contains(EventFlags::IS_LIVE));
        assert!(EventFlags::IS_LIVE.contains(EventFlags::NONE));
        assert!(!EventFlags::IS_LIVE.contains(both));

        let mut f = EventFlags::NONE;
        f |= EventFlags::DONT_RECORD;
        assert_eq!(f, EventFlags::DONT_RECORD);
        assert_eq!(f.bits(), 0b10);
    }

    #[test]
    fn unknown_flag_bits_survive_a_round_trip() {
        let raw = 0xF00F_u16;
        assert_eq!(EventFlags::from_bits(raw).bits(), raw);
    }

    #[test]
    fn kind_bits_match_the_abi_codes() {
        let note = NoteEvent {
            header: hdr(0),
            ..NoteEvent::default()
        };
        let param = ParamEvent {
            header: hdr(0),
            ..ParamEvent::default()
        };
        let gesture = ParamGestureEvent {
            header: hdr(0),
            param_id: 1,
        };
        let cases: [(DauxEvent<'_>, u16); 14] = [
            (DauxEvent::NoteOn(note), 1),
            (DauxEvent::NoteOff(note), 2),
            (DauxEvent::NoteChoke(note), 3),
            (DauxEvent::NoteEnd(note), 4),
            (
                DauxEvent::NoteExpression(NoteExpressionEvent {
                    header: hdr(0),
                    expression: NoteExpression::Volume,
                    note_id: -1,
                    channel: 0,
                    key: 60,
                    value: 0.5,
                }),
                5,
            ),
            (DauxEvent::ParamValue(param), 6),
            (DauxEvent::ParamMod(param), 7),
            (DauxEvent::ParamGestureBegin(gesture), 8),
            (DauxEvent::ParamGestureEnd(gesture), 9),
            (
                DauxEvent::Transport(TransportEvent {
                    header: hdr(0),
                    transport: TransportSnapshot::unknown(),
                }),
                10,
            ),
            (
                DauxEvent::Midi1(Midi1Event {
                    header: hdr(0),
                    message: Midi1Message::note_on(0, 60, 100),
                }),
                11,
            ),
            (
                DauxEvent::Midi2(Midi2Event {
                    header: hdr(0),
                    packet: Ump::from_word(0),
                }),
                12,
            ),
            (
                DauxEvent::SysEx(SysExEvent {
                    header: hdr(0),
                    bytes: &[],
                }),
                13,
            ),
            (
                DauxEvent::Custom(CustomEvent {
                    header: hdr(0),
                    kind: kind::CUSTOM + 7,
                    bytes: &[],
                }),
                kind::CUSTOM + 7,
            ),
        ];
        for (event, bits) in cases {
            assert_eq!(event.kind_bits(), bits, "{event:?}");
        }
    }

    #[test]
    fn header_accessors_read_through_every_variant() {
        let header = EventHeader::new(1234, 3, EventFlags::IS_LIVE);
        let note = NoteEvent {
            header,
            ..NoteEvent::default()
        };
        let events = [
            DauxEvent::NoteOn(note),
            DauxEvent::NoteExpression(NoteExpressionEvent {
                header,
                expression: NoteExpression::Pan,
                note_id: 1,
                channel: 0,
                key: 60,
                value: 0.0,
            }),
            DauxEvent::ParamValue(ParamEvent {
                header,
                ..ParamEvent::default()
            }),
            DauxEvent::ParamGestureEnd(ParamGestureEvent {
                header,
                param_id: 9,
            }),
            DauxEvent::Transport(TransportEvent {
                header,
                transport: TransportSnapshot::unknown(),
            }),
            DauxEvent::Midi1(Midi1Event {
                header,
                message: Midi1Message::timing_clock(),
            }),
            DauxEvent::Midi2(Midi2Event {
                header,
                packet: Ump::from_word(0),
            }),
            DauxEvent::SysEx(SysExEvent {
                header,
                bytes: &[1, 2, 3],
            }),
            DauxEvent::Custom(CustomEvent {
                header,
                kind: kind::CUSTOM,
                bytes: &[4],
            }),
        ];
        for e in events {
            assert_eq!(e.header(), header, "{e:?}");
            assert_eq!(e.time(), 1234, "{e:?}");
            assert_eq!(e.port_index(), 3, "{e:?}");
            assert!(e.flags().is_live(), "{e:?}");
        }
    }

    #[test]
    fn payload_is_only_present_for_borrowed_events() {
        let header = hdr(0);
        assert_eq!(
            DauxEvent::SysEx(SysExEvent {
                header,
                bytes: &[0xF0, 1, 0xF7]
            })
            .payload(),
            Some(&[0xF0u8, 1, 0xF7][..])
        );
        assert_eq!(
            DauxEvent::Custom(CustomEvent {
                header,
                kind: kind::CUSTOM,
                bytes: &[9]
            })
            .payload(),
            Some(&[9u8][..])
        );
        assert_eq!(
            DauxEvent::NoteOn(NoteEvent {
                header,
                ..NoteEvent::default()
            })
            .payload(),
            None
        );
    }

    #[test]
    fn sysex_events_expose_a_midi_view() {
        let e = SysExEvent {
            header: hdr(0),
            bytes: &[0xF0, 0x7E, 0x01, 0xF7],
        };
        assert_eq!(e.sysex().payload(), &[0x7E, 0x01]);
        assert!(e.sysex().is_valid());
    }

    #[test]
    fn note_expression_codes_round_trip() {
        let all = [
            NoteExpression::Volume,
            NoteExpression::Pan,
            NoteExpression::Tuning,
            NoteExpression::Vibrato,
            NoteExpression::Expression,
            NoteExpression::Brightness,
            NoteExpression::Pressure,
        ];
        for (i, e) in all.into_iter().enumerate() {
            assert_eq!(e.as_bits(), i as u32);
            assert_eq!(NoteExpression::from_bits(e.as_bits()), Some(e));
        }
        assert_eq!(NoteExpression::from_bits(7), None);
        assert_eq!(NoteExpression::from_bits(u32::MAX), None);
    }
}
