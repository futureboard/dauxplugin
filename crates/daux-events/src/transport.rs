//! A plain, dependency-free mirror of the transport state.

/// Transport state flags, mirroring `DAUX_TRANSPORT_*` in
/// `docs/specifications/abi-v1.md` §10.
///
/// A `HAS_*` flag says the corresponding field of a [`TransportSnapshot`] is meaningful. A
/// host must not fabricate values for fields it does not flag, and a plug-in must not read
/// them; the accessors on [`TransportSnapshot`] enforce that by returning `Option`.
pub mod transport_flags {
    /// `tempo` and `tempo_increment` are valid.
    pub const HAS_TEMPO: u32 = 1 << 0;
    /// `song_pos_beats` is valid.
    pub const HAS_BEATS: u32 = 1 << 1;
    /// `song_pos_seconds` is valid.
    pub const HAS_SECONDS: u32 = 1 << 2;
    /// `time_sig_numerator` and `time_sig_denominator` are valid.
    pub const HAS_TIME_SIG: u32 = 1 << 3;
    /// The four loop fields are valid.
    pub const HAS_LOOP: u32 = 1 << 4;
    /// `bar_start_beats` and `bar_number` are valid.
    pub const HAS_BAR: u32 = 1 << 5;
    /// The transport is rolling.
    pub const IS_PLAYING: u32 = 1 << 6;
    /// The transport is recording.
    pub const IS_RECORDING: u32 = 1 << 7;
    /// Loop playback is enabled.
    pub const IS_LOOPING: u32 = 1 << 8;
    /// The transport is in pre-roll.
    pub const IS_PREROLL: u32 = 1 << 9;
}

/// A copy of the host's transport state at one point in the timeline.
///
/// This is a plain `Copy` mirror of `DauxTransportV1`. It lives in `daux-events` rather than
/// `daux-transport` because `daux-events` must not depend on `daux-transport` — the event
/// model has to stay at the bottom of the dependency graph. `daux-transport::Transport`
/// provides `From`/`Into` conversions in the other direction.
///
/// Read every field through its accessor: a field whose `HAS_*` flag is clear holds an
/// unspecified value.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TransportSnapshot {
    /// `transport_flags::*` bit set.
    pub flags: u32,
    /// Playback position in samples from the start of the timeline.
    pub song_pos_samples: i64,
    /// Playback position in quarter notes. Requires [`transport_flags::HAS_BEATS`].
    pub song_pos_beats: f64,
    /// Playback position in seconds. Requires [`transport_flags::HAS_SECONDS`].
    pub song_pos_seconds: f64,
    /// Tempo in BPM. Requires [`transport_flags::HAS_TEMPO`].
    pub tempo: f64,
    /// Tempo change per sample in BPM, `0.0` when steady. Requires
    /// [`transport_flags::HAS_TEMPO`].
    pub tempo_increment: f64,
    /// Beat position of the current bar's downbeat. Requires [`transport_flags::HAS_BAR`].
    pub bar_start_beats: f64,
    /// Zero-based bar number. Requires [`transport_flags::HAS_BAR`].
    pub bar_number: i32,
    /// Time signature numerator. Requires [`transport_flags::HAS_TIME_SIG`].
    pub time_sig_numerator: u16,
    /// Time signature denominator. Requires [`transport_flags::HAS_TIME_SIG`].
    pub time_sig_denominator: u16,
    /// Loop start in quarter notes. Requires [`transport_flags::HAS_LOOP`].
    pub loop_start_beats: f64,
    /// Loop end in quarter notes. Requires [`transport_flags::HAS_LOOP`].
    pub loop_end_beats: f64,
    /// Loop start in seconds. Requires [`transport_flags::HAS_LOOP`].
    pub loop_start_seconds: f64,
    /// Loop end in seconds. Requires [`transport_flags::HAS_LOOP`].
    pub loop_end_seconds: f64,
}

impl TransportSnapshot {
    /// [any-thread] A snapshot with no flags set: nothing at all is known about the host's
    /// timeline.
    pub const fn unknown() -> Self {
        Self {
            flags: 0,
            song_pos_samples: 0,
            song_pos_beats: 0.0,
            song_pos_seconds: 0.0,
            tempo: 0.0,
            tempo_increment: 0.0,
            bar_start_beats: 0.0,
            bar_number: 0,
            time_sig_numerator: 0,
            time_sig_denominator: 0,
            loop_start_beats: 0.0,
            loop_end_beats: 0.0,
            loop_start_seconds: 0.0,
            loop_end_seconds: 0.0,
        }
    }

    /// [audio-thread] `true` when every bit of `mask` is set.
    pub const fn has(&self, mask: u32) -> bool {
        self.flags & mask == mask
    }

    /// [audio-thread] The transport is rolling.
    pub const fn is_playing(&self) -> bool {
        self.has(transport_flags::IS_PLAYING)
    }

    /// [audio-thread] The transport is recording.
    pub const fn is_recording(&self) -> bool {
        self.has(transport_flags::IS_RECORDING)
    }

    /// [audio-thread] Loop playback is enabled.
    pub const fn is_looping(&self) -> bool {
        self.has(transport_flags::IS_LOOPING)
    }

    /// [audio-thread] The transport is in pre-roll.
    pub const fn is_preroll(&self) -> bool {
        self.has(transport_flags::IS_PREROLL)
    }

    /// [audio-thread] Tempo in BPM, or `None` when the host did not provide one.
    pub const fn tempo(&self) -> Option<f64> {
        if self.has(transport_flags::HAS_TEMPO) {
            Some(self.tempo)
        } else {
            None
        }
    }

    /// [audio-thread] Position in quarter notes, or `None`.
    pub const fn beats(&self) -> Option<f64> {
        if self.has(transport_flags::HAS_BEATS) {
            Some(self.song_pos_beats)
        } else {
            None
        }
    }

    /// [audio-thread] Position in seconds, or `None`.
    pub const fn seconds(&self) -> Option<f64> {
        if self.has(transport_flags::HAS_SECONDS) {
            Some(self.song_pos_seconds)
        } else {
            None
        }
    }

    /// [audio-thread] `(numerator, denominator)`, or `None`.
    pub const fn time_signature(&self) -> Option<(u16, u16)> {
        if self.has(transport_flags::HAS_TIME_SIG) {
            Some((self.time_sig_numerator, self.time_sig_denominator))
        } else {
            None
        }
    }

    /// [audio-thread] `(bar_start_beats, bar_number)`, or `None`.
    pub const fn bar(&self) -> Option<(f64, i32)> {
        if self.has(transport_flags::HAS_BAR) {
            Some((self.bar_start_beats, self.bar_number))
        } else {
            None
        }
    }

    /// [audio-thread] `(start, end)` of the loop in quarter notes, or `None`.
    pub const fn loop_range_beats(&self) -> Option<(f64, f64)> {
        if self.has(transport_flags::HAS_LOOP) {
            Some((self.loop_start_beats, self.loop_end_beats))
        } else {
            None
        }
    }

    /// [audio-thread] `(start, end)` of the loop in seconds, or `None`.
    pub const fn loop_range_seconds(&self) -> Option<(f64, f64)> {
        if self.has(transport_flags::HAS_LOOP) {
            Some((self.loop_start_seconds, self.loop_end_seconds))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_snapshot_answers_none_to_everything() {
        let t = TransportSnapshot::unknown();
        assert_eq!(t, TransportSnapshot::default());
        assert_eq!(t.tempo(), None);
        assert_eq!(t.beats(), None);
        assert_eq!(t.seconds(), None);
        assert_eq!(t.time_signature(), None);
        assert_eq!(t.bar(), None);
        assert_eq!(t.loop_range_beats(), None);
        assert_eq!(t.loop_range_seconds(), None);
        assert!(!t.is_playing());
        assert!(!t.is_recording());
        assert!(!t.is_looping());
        assert!(!t.is_preroll());
    }

    #[test]
    fn unflagged_fields_are_never_handed_out_even_when_populated() {
        // A hostile or careless host may leave junk in unflagged fields.
        let t = TransportSnapshot {
            flags: transport_flags::HAS_BEATS,
            tempo: 999.0,
            song_pos_beats: 4.0,
            song_pos_seconds: 123.0,
            time_sig_numerator: 7,
            time_sig_denominator: 8,
            loop_start_beats: 1.0,
            loop_end_beats: 5.0,
            ..TransportSnapshot::unknown()
        };
        assert_eq!(t.beats(), Some(4.0));
        assert_eq!(t.tempo(), None);
        assert_eq!(t.seconds(), None);
        assert_eq!(t.time_signature(), None);
        assert_eq!(t.loop_range_beats(), None);
    }

    #[test]
    fn flagged_fields_are_returned() {
        let t = TransportSnapshot {
            flags: transport_flags::HAS_TEMPO
                | transport_flags::HAS_TIME_SIG
                | transport_flags::HAS_LOOP
                | transport_flags::HAS_BAR
                | transport_flags::IS_PLAYING
                | transport_flags::IS_LOOPING,
            tempo: 128.5,
            time_sig_numerator: 6,
            time_sig_denominator: 8,
            loop_start_beats: 8.0,
            loop_end_beats: 16.0,
            loop_start_seconds: 4.0,
            loop_end_seconds: 8.0,
            bar_start_beats: 12.0,
            bar_number: 3,
            ..TransportSnapshot::unknown()
        };
        assert_eq!(t.tempo(), Some(128.5));
        assert_eq!(t.time_signature(), Some((6, 8)));
        assert_eq!(t.loop_range_beats(), Some((8.0, 16.0)));
        assert_eq!(t.loop_range_seconds(), Some((4.0, 8.0)));
        assert_eq!(t.bar(), Some((12.0, 3)));
        assert!(t.is_playing());
        assert!(t.is_looping());
        assert!(!t.is_recording());
    }

    #[test]
    fn has_requires_every_bit_of_the_mask() {
        let t = TransportSnapshot {
            flags: transport_flags::HAS_TEMPO,
            ..TransportSnapshot::unknown()
        };
        assert!(t.has(transport_flags::HAS_TEMPO));
        assert!(!t.has(transport_flags::HAS_TEMPO | transport_flags::HAS_BEATS));
        assert!(t.has(0));
    }
}
