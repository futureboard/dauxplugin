//! MIDI 1.0 byte-stream messages.

/// MIDI 1.0 status bytes.
///
/// Channel voice statuses are the high nibble only; the low nibble carries the channel.
/// System statuses are complete bytes.
pub mod status {
    /// Note Off, `0x8n`.
    pub const NOTE_OFF: u8 = 0x80;
    /// Note On, `0x9n`. A velocity of zero means "note off" in MIDI 1.0.
    pub const NOTE_ON: u8 = 0x90;
    /// Polyphonic Key Pressure (aftertouch), `0xAn`.
    pub const POLY_PRESSURE: u8 = 0xA0;
    /// Control Change, `0xBn`.
    pub const CONTROL_CHANGE: u8 = 0xB0;
    /// Program Change, `0xCn`.
    pub const PROGRAM_CHANGE: u8 = 0xC0;
    /// Channel Pressure (aftertouch), `0xDn`.
    pub const CHANNEL_PRESSURE: u8 = 0xD0;
    /// Pitch Bend Change, `0xEn`.
    pub const PITCH_BEND: u8 = 0xE0;

    /// System Exclusive start.
    pub const SYSEX_START: u8 = 0xF0;
    /// MIDI Time Code Quarter Frame.
    pub const TIME_CODE_QUARTER_FRAME: u8 = 0xF1;
    /// Song Position Pointer.
    pub const SONG_POSITION: u8 = 0xF2;
    /// Song Select.
    pub const SONG_SELECT: u8 = 0xF3;
    /// Tune Request.
    pub const TUNE_REQUEST: u8 = 0xF6;
    /// End of System Exclusive.
    pub const SYSEX_END: u8 = 0xF7;
    /// Timing Clock.
    pub const TIMING_CLOCK: u8 = 0xF8;
    /// Start.
    pub const START: u8 = 0xFA;
    /// Continue.
    pub const CONTINUE: u8 = 0xFB;
    /// Stop.
    pub const STOP: u8 = 0xFC;
    /// Active Sensing.
    pub const ACTIVE_SENSING: u8 = 0xFE;
    /// System Reset.
    pub const SYSTEM_RESET: u8 = 0xFF;
}

/// The kind of a [`Midi1Message`], derived from its status byte.
///
/// Everything that is not a channel voice message — system common, system real time, and
/// System Exclusive delimiters — reports [`Midi1Kind::System`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Midi1Kind {
    /// `0x8n` Note Off.
    NoteOff,
    /// `0x9n` Note On.
    NoteOn,
    /// `0xAn` Polyphonic Key Pressure.
    PolyPressure,
    /// `0xBn` Control Change.
    ControlChange,
    /// `0xCn` Program Change.
    ProgramChange,
    /// `0xDn` Channel Pressure.
    ChannelPressure,
    /// `0xEn` Pitch Bend Change.
    PitchBend,
    /// `0xF0`–`0xFF` system common / system real time, or a malformed status byte.
    System,
}

impl Midi1Kind {
    /// [any-thread] Classifies a raw status byte.
    pub const fn from_status_byte(byte: u8) -> Self {
        match byte & 0xF0 {
            status::NOTE_OFF => Self::NoteOff,
            status::NOTE_ON => Self::NoteOn,
            status::POLY_PRESSURE => Self::PolyPressure,
            status::CONTROL_CHANGE => Self::ControlChange,
            status::PROGRAM_CHANGE => Self::ProgramChange,
            status::CHANNEL_PRESSURE => Self::ChannelPressure,
            status::PITCH_BEND => Self::PitchBend,
            _ => Self::System,
        }
    }

    /// [any-thread] `true` for the seven channel voice kinds.
    pub const fn is_channel_voice(self) -> bool {
        !matches!(self, Self::System)
    }
}

/// A single MIDI 1.0 message, stored as up to three bytes.
///
/// The buffer is always three bytes wide; [`Midi1Message::byte_len`] says how many of them
/// are meaningful. Running status is never used — `bytes[0]` is always a status byte for a
/// well-formed message. Messages whose payload cannot fit (System Exclusive) are carried by
/// [`crate::SysEx7`] instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Midi1Message {
    /// Status byte followed by up to two data bytes. Unused trailing bytes are zero.
    pub bytes: [u8; 3],
}

impl Midi1Message {
    /// [any-thread] Wraps three raw bytes without validating them.
    pub const fn new(bytes: [u8; 3]) -> Self {
        Self { bytes }
    }

    /// [any-thread] The raw status byte, channel nibble included.
    pub const fn status_byte(&self) -> u8 {
        self.bytes[0]
    }

    /// [any-thread] The status *without* the channel: the high nibble for channel voice
    /// messages, the whole byte for system messages.
    pub const fn status(&self) -> u8 {
        let s = self.bytes[0];
        if s >= status::SYSEX_START {
            s
        } else {
            s & 0xF0
        }
    }

    /// [any-thread] The zero-based channel `0..=15`, or `0` for system messages.
    pub const fn channel(&self) -> u8 {
        let s = self.bytes[0];
        if s >= status::SYSEX_START {
            0
        } else {
            s & 0x0F
        }
    }

    /// [any-thread] The first data byte, or `0` when the message has none.
    pub const fn data1(&self) -> u8 {
        self.bytes[1]
    }

    /// [any-thread] The second data byte, or `0` when the message has fewer than two.
    pub const fn data2(&self) -> u8 {
        self.bytes[2]
    }

    /// [any-thread] Classifies the message.
    pub const fn kind(&self) -> Midi1Kind {
        Midi1Kind::from_status_byte(self.bytes[0])
    }

    /// [any-thread] `true` when `bytes[0]` really is a status byte (`>= 0x80`).
    pub const fn is_valid(&self) -> bool {
        self.bytes[0] >= 0x80
    }

    /// [any-thread] `true` for `0x80..=0xEF`.
    pub const fn is_channel_voice(&self) -> bool {
        matches!(self.bytes[0] & 0xF0, status::NOTE_OFF..=status::PITCH_BEND)
    }

    /// [any-thread] `true` for `0xF0..=0xFF`.
    pub const fn is_system(&self) -> bool {
        self.bytes[0] >= status::SYSEX_START
    }

    /// [any-thread] `true` for system real time messages, `0xF8..=0xFF`.
    pub const fn is_realtime(&self) -> bool {
        self.bytes[0] >= status::TIMING_CLOCK
    }

    /// [any-thread] Number of meaningful bytes: `1`, `2` or `3`, and `0` when `bytes[0]` is
    /// not a status byte.
    ///
    /// `0xF0` (System Exclusive start) and `0xF7` (end) report `1` because their payload is
    /// not carried by this type.
    pub const fn byte_len(&self) -> usize {
        let s = self.bytes[0];
        if s < 0x80 {
            return 0;
        }
        match s & 0xF0 {
            status::NOTE_OFF
            | status::NOTE_ON
            | status::POLY_PRESSURE
            | status::CONTROL_CHANGE
            | status::PITCH_BEND => 3,
            status::PROGRAM_CHANGE | status::CHANNEL_PRESSURE => 2,
            _ => match s {
                status::TIME_CODE_QUARTER_FRAME | status::SONG_SELECT => 2,
                status::SONG_POSITION => 3,
                _ => 1,
            },
        }
    }

    /// [any-thread] The meaningful bytes only. Empty for a malformed message.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.byte_len()]
    }

    // ---- channel voice constructors -------------------------------------------------

    /// [any-thread] Note Off. `channel` is masked to 4 bits, `key`/`velocity` to 7.
    pub const fn note_off(channel: u8, key: u8, velocity: u8) -> Self {
        Self::channel_voice(status::NOTE_OFF, channel, key, velocity)
    }

    /// [any-thread] Note On. A `velocity` of zero means "note off" in MIDI 1.0.
    pub const fn note_on(channel: u8, key: u8, velocity: u8) -> Self {
        Self::channel_voice(status::NOTE_ON, channel, key, velocity)
    }

    /// [any-thread] Polyphonic Key Pressure.
    pub const fn poly_pressure(channel: u8, key: u8, pressure: u8) -> Self {
        Self::channel_voice(status::POLY_PRESSURE, channel, key, pressure)
    }

    /// [any-thread] Control Change.
    pub const fn control_change(channel: u8, controller: u8, value: u8) -> Self {
        Self::channel_voice(status::CONTROL_CHANGE, channel, controller, value)
    }

    /// [any-thread] Program Change (two bytes).
    pub const fn program_change(channel: u8, program: u8) -> Self {
        Self {
            bytes: [status::PROGRAM_CHANGE | (channel & 0x0F), program & 0x7F, 0],
        }
    }

    /// [any-thread] Channel Pressure (two bytes).
    pub const fn channel_pressure(channel: u8, pressure: u8) -> Self {
        Self {
            bytes: [
                status::CHANNEL_PRESSURE | (channel & 0x0F),
                pressure & 0x7F,
                0,
            ],
        }
    }

    /// [any-thread] Pitch Bend. `value` is the 14-bit position `0..=16383`, centre `8192`;
    /// bits above 14 are ignored.
    pub const fn pitch_bend(channel: u8, value: u16) -> Self {
        let v = value & 0x3FFF;
        Self {
            bytes: [
                status::PITCH_BEND | (channel & 0x0F),
                (v & 0x7F) as u8,
                ((v >> 7) & 0x7F) as u8,
            ],
        }
    }

    /// [any-thread] The 14-bit pitch bend position carried by this message.
    ///
    /// Meaningful only when [`Midi1Message::kind`] is [`Midi1Kind::PitchBend`].
    pub const fn pitch_bend_value(&self) -> u16 {
        (self.bytes[1] as u16 & 0x7F) | ((self.bytes[2] as u16 & 0x7F) << 7)
    }

    // ---- system constructors --------------------------------------------------------

    /// [any-thread] MIDI Time Code Quarter Frame.
    pub const fn time_code_quarter_frame(data: u8) -> Self {
        Self {
            bytes: [status::TIME_CODE_QUARTER_FRAME, data & 0x7F, 0],
        }
    }

    /// [any-thread] Song Position Pointer, in 14-bit MIDI beats (sixteenth notes).
    pub const fn song_position(beats: u16) -> Self {
        let v = beats & 0x3FFF;
        Self {
            bytes: [
                status::SONG_POSITION,
                (v & 0x7F) as u8,
                ((v >> 7) & 0x7F) as u8,
            ],
        }
    }

    /// [any-thread] The 14-bit value of a Song Position Pointer.
    pub const fn song_position_value(&self) -> u16 {
        (self.bytes[1] as u16 & 0x7F) | ((self.bytes[2] as u16 & 0x7F) << 7)
    }

    /// [any-thread] Song Select.
    pub const fn song_select(song: u8) -> Self {
        Self {
            bytes: [status::SONG_SELECT, song & 0x7F, 0],
        }
    }

    /// [any-thread] Tune Request.
    pub const fn tune_request() -> Self {
        Self {
            bytes: [status::TUNE_REQUEST, 0, 0],
        }
    }

    /// [any-thread] Timing Clock (24 per quarter note).
    pub const fn timing_clock() -> Self {
        Self {
            bytes: [status::TIMING_CLOCK, 0, 0],
        }
    }

    /// [any-thread] Start.
    pub const fn start() -> Self {
        Self {
            bytes: [status::START, 0, 0],
        }
    }

    /// [any-thread] Continue. Named with a trailing underscore because `continue` is a
    /// Rust keyword.
    pub const fn continue_() -> Self {
        Self {
            bytes: [status::CONTINUE, 0, 0],
        }
    }

    /// [any-thread] Stop.
    pub const fn stop() -> Self {
        Self {
            bytes: [status::STOP, 0, 0],
        }
    }

    /// [any-thread] Active Sensing.
    pub const fn active_sensing() -> Self {
        Self {
            bytes: [status::ACTIVE_SENSING, 0, 0],
        }
    }

    /// [any-thread] System Reset.
    pub const fn system_reset() -> Self {
        Self {
            bytes: [status::SYSTEM_RESET, 0, 0],
        }
    }

    const fn channel_voice(status: u8, channel: u8, d1: u8, d2: u8) -> Self {
        Self {
            bytes: [status | (channel & 0x0F), d1 & 0x7F, d2 & 0x7F],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_voice_constructors_encode_status_and_channel() {
        assert_eq!(Midi1Message::note_off(3, 60, 64).bytes, [0x83, 60, 64]);
        assert_eq!(Midi1Message::note_on(0, 60, 100).bytes, [0x90, 60, 100]);
        assert_eq!(Midi1Message::poly_pressure(15, 60, 1).bytes, [0xAF, 60, 1]);
        assert_eq!(
            Midi1Message::control_change(1, 7, 100).bytes,
            [0xB1, 7, 100]
        );
        assert_eq!(Midi1Message::program_change(2, 42).bytes, [0xC2, 42, 0]);
        assert_eq!(Midi1Message::channel_pressure(4, 90).bytes, [0xD4, 90, 0]);
        assert_eq!(Midi1Message::pitch_bend(5, 8192).bytes, [0xE5, 0x00, 0x40]);
    }

    #[test]
    fn constructors_mask_out_of_range_arguments() {
        // Channel 0xFF must not corrupt the status nibble, key 0xFF must stay 7-bit.
        let m = Midi1Message::note_on(0xFF, 0xFF, 0xFF);
        assert_eq!(m.bytes, [0x9F, 0x7F, 0x7F]);
        assert_eq!(
            Midi1Message::pitch_bend(0, 0xFFFF).pitch_bend_value(),
            0x3FFF
        );
    }

    #[test]
    fn status_channel_and_kind_decode() {
        let m = Midi1Message::control_change(9, 64, 127);
        assert_eq!(m.status_byte(), 0xB9);
        assert_eq!(m.status(), status::CONTROL_CHANGE);
        assert_eq!(m.channel(), 9);
        assert_eq!(m.kind(), Midi1Kind::ControlChange);
        assert!(m.is_channel_voice());
        assert!(!m.is_system());
        assert!(!m.is_realtime());
    }

    #[test]
    fn system_messages_report_channel_zero_and_full_status() {
        for m in [
            Midi1Message::timing_clock(),
            Midi1Message::start(),
            Midi1Message::continue_(),
            Midi1Message::stop(),
            Midi1Message::active_sensing(),
            Midi1Message::system_reset(),
            Midi1Message::tune_request(),
        ] {
            assert_eq!(m.channel(), 0, "{m:?}");
            assert_eq!(m.status(), m.status_byte(), "{m:?}");
            assert_eq!(m.kind(), Midi1Kind::System, "{m:?}");
            assert!(m.is_system(), "{m:?}");
            assert_eq!(m.byte_len(), 1, "{m:?}");
        }
        assert!(Midi1Message::timing_clock().is_realtime());
        assert!(!Midi1Message::tune_request().is_realtime());
    }

    #[test]
    fn byte_len_matches_the_wire_format() {
        assert_eq!(Midi1Message::note_on(0, 1, 2).byte_len(), 3);
        assert_eq!(Midi1Message::note_off(0, 1, 2).byte_len(), 3);
        assert_eq!(Midi1Message::poly_pressure(0, 1, 2).byte_len(), 3);
        assert_eq!(Midi1Message::control_change(0, 1, 2).byte_len(), 3);
        assert_eq!(Midi1Message::pitch_bend(0, 1).byte_len(), 3);
        assert_eq!(Midi1Message::program_change(0, 1).byte_len(), 2);
        assert_eq!(Midi1Message::channel_pressure(0, 1).byte_len(), 2);
        assert_eq!(Midi1Message::time_code_quarter_frame(1).byte_len(), 2);
        assert_eq!(Midi1Message::song_select(1).byte_len(), 2);
        assert_eq!(Midi1Message::song_position(1).byte_len(), 3);
        assert_eq!(Midi1Message::new([status::SYSEX_START, 0, 0]).byte_len(), 1);
        assert_eq!(Midi1Message::new([status::SYSEX_END, 0, 0]).byte_len(), 1);
    }

    #[test]
    fn malformed_leading_byte_is_reported_not_panicked_on() {
        let m = Midi1Message::new([0x40, 0x00, 0x00]);
        assert!(!m.is_valid());
        assert_eq!(m.byte_len(), 0);
        assert!(m.as_bytes().is_empty());
        assert_eq!(m.kind(), Midi1Kind::System);
        assert!(!m.is_channel_voice());
    }

    #[test]
    fn as_bytes_is_the_meaningful_prefix() {
        assert_eq!(Midi1Message::program_change(0, 42).as_bytes(), &[0xC0, 42]);
        assert_eq!(Midi1Message::timing_clock().as_bytes(), &[0xF8]);
        assert_eq!(Midi1Message::note_on(0, 60, 64).as_bytes(), &[0x90, 60, 64]);
    }

    #[test]
    fn pitch_bend_round_trips_every_14_bit_value() {
        for v in (0u16..=0x3FFF).step_by(37) {
            assert_eq!(Midi1Message::pitch_bend(7, v).pitch_bend_value(), v);
        }
        assert_eq!(Midi1Message::pitch_bend(0, 0).pitch_bend_value(), 0);
        assert_eq!(
            Midi1Message::pitch_bend(0, 0x3FFF).pitch_bend_value(),
            0x3FFF
        );
    }

    #[test]
    fn song_position_round_trips() {
        assert_eq!(Midi1Message::song_position(0).song_position_value(), 0);
        assert_eq!(
            Midi1Message::song_position(12345).song_position_value(),
            12345
        );
        assert_eq!(
            Midi1Message::song_position(0x3FFF).song_position_value(),
            0x3FFF
        );
    }

    #[test]
    fn kind_covers_every_channel_voice_status() {
        let cases = [
            (0x80u8, Midi1Kind::NoteOff),
            (0x90, Midi1Kind::NoteOn),
            (0xA0, Midi1Kind::PolyPressure),
            (0xB0, Midi1Kind::ControlChange),
            (0xC0, Midi1Kind::ProgramChange),
            (0xD0, Midi1Kind::ChannelPressure),
            (0xE0, Midi1Kind::PitchBend),
            (0xF0, Midi1Kind::System),
        ];
        for (byte, kind) in cases {
            for ch in 0u8..16 {
                let raw = if byte >= 0xF0 { byte } else { byte | ch };
                assert_eq!(Midi1Kind::from_status_byte(raw), kind, "0x{raw:02X}");
            }
            assert_eq!(kind.is_channel_voice(), kind != Midi1Kind::System);
        }
    }
}
