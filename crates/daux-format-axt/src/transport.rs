//! Transport translation (abi-v1 §10).
//!
//! The bit values of `DAUX_TRANSPORT_*`, `daux_transport::TransportFlags` and
//! `daux_events::transport_flags` are the same numbers by construction, so the flag word
//! crosses unchanged — including bits this build does not know, which a plug-in built against a
//! later minor revision may.
//!
//! What must *not* be lost is the rule that a field is meaningless unless its `HAS_*` flag is
//! set: a host that leaves `tempo` at zero and a host that means 0 BPM are indistinguishable
//! without it, so the flags are copied first and everything else is copied verbatim behind
//! them.

use daux_abi::DauxTransportV1;
use daux_plugin_api::{TimeSignature, Transport, TransportFlags, TransportSnapshot};

/// [audio-thread] The DAUx transport for one block, from the host's flat record.
///
/// # Safety of the caller
///
/// `abi` must have been validated with [`is_usable`] first: a record whose `size` does not
/// cover the v1.0 fields has not been fully written by the host.
pub(crate) fn from_abi(abi: &DauxTransportV1) -> Transport {
    Transport {
        flags: TransportFlags::from_bits(abi.flags),
        song_pos_samples: abi.song_pos_samples,
        song_pos_beats: abi.song_pos_beats,
        song_pos_seconds: abi.song_pos_seconds,
        tempo: abi.tempo,
        tempo_increment: abi.tempo_increment,
        bar_start_beats: abi.bar_start_beats,
        bar_number: abi.bar_number,
        time_signature: TimeSignature::new(abi.time_sig_numerator, abi.time_sig_denominator),
        loop_start_beats: abi.loop_start_beats,
        loop_end_beats: abi.loop_end_beats,
        loop_start_seconds: abi.loop_start_seconds,
        loop_end_seconds: abi.loop_end_seconds,
    }
}

/// [audio-thread] The flat `Copy` snapshot the event model carries, from the host's record.
pub(crate) fn snapshot_from_abi(abi: &DauxTransportV1) -> TransportSnapshot {
    TransportSnapshot {
        flags: abi.flags,
        song_pos_samples: abi.song_pos_samples,
        song_pos_beats: abi.song_pos_beats,
        song_pos_seconds: abi.song_pos_seconds,
        tempo: abi.tempo,
        tempo_increment: abi.tempo_increment,
        bar_start_beats: abi.bar_start_beats,
        bar_number: abi.bar_number,
        time_sig_numerator: abi.time_sig_numerator,
        time_sig_denominator: abi.time_sig_denominator,
        loop_start_beats: abi.loop_start_beats,
        loop_end_beats: abi.loop_end_beats,
        loop_start_seconds: abi.loop_start_seconds,
        loop_end_seconds: abi.loop_end_seconds,
    }
}

/// [audio-thread] The host's flat record, from a snapshot a plug-in is sending back.
pub(crate) fn snapshot_to_abi(snapshot: &TransportSnapshot) -> DauxTransportV1 {
    let mut abi = DauxTransportV1::new();
    abi.flags = snapshot.flags;
    abi.song_pos_samples = snapshot.song_pos_samples;
    abi.song_pos_beats = snapshot.song_pos_beats;
    abi.song_pos_seconds = snapshot.song_pos_seconds;
    abi.tempo = snapshot.tempo;
    abi.tempo_increment = snapshot.tempo_increment;
    abi.bar_start_beats = snapshot.bar_start_beats;
    abi.bar_number = snapshot.bar_number;
    abi.time_sig_numerator = snapshot.time_sig_numerator;
    abi.time_sig_denominator = snapshot.time_sig_denominator;
    abi.loop_start_beats = snapshot.loop_start_beats;
    abi.loop_end_beats = snapshot.loop_end_beats;
    abi.loop_start_seconds = snapshot.loop_start_seconds;
    abi.loop_end_seconds = snapshot.loop_end_seconds;
    abi
}

/// [audio-thread] `true` when the host wrote every v1.0 field of the record (abi-v1 §3).
pub(crate) fn is_usable(abi: &DauxTransportV1) -> bool {
    abi.is_v1_0_compatible()
}

#[cfg(test)]
mod tests {
    use super::*;
    use daux_abi::{
        DAUX_TRANSPORT_HAS_BEATS, DAUX_TRANSPORT_HAS_TEMPO, DAUX_TRANSPORT_HAS_TIME_SIG,
        DAUX_TRANSPORT_IS_PLAYING,
    };

    fn filled() -> DauxTransportV1 {
        let mut t = DauxTransportV1::new();
        t.flags = DAUX_TRANSPORT_HAS_TEMPO
            | DAUX_TRANSPORT_HAS_BEATS
            | DAUX_TRANSPORT_HAS_TIME_SIG
            | DAUX_TRANSPORT_IS_PLAYING;
        t.song_pos_samples = 48_000;
        t.song_pos_beats = 4.5;
        t.song_pos_seconds = 1.0;
        t.tempo = 128.0;
        t.tempo_increment = 0.001;
        t.bar_start_beats = 4.0;
        t.bar_number = 2;
        t.time_sig_numerator = 7;
        t.time_sig_denominator = 8;
        t.loop_start_beats = 8.0;
        t.loop_end_beats = 16.0;
        t.loop_start_seconds = 2.0;
        t.loop_end_seconds = 4.0;
        t
    }

    #[test]
    fn flags_gate_what_a_plugin_can_read() {
        let transport = from_abi(&filled());
        assert!(transport.is_playing());
        assert!(!transport.is_recording());
        assert_eq!(transport.tempo(), Some(128.0));
        assert_eq!(transport.beats(), Some(4.5));
        // HAS_SECONDS and HAS_LOOP were not set, so those fields must stay unreadable even
        // though the host happened to write values into them.
        assert_eq!(transport.seconds(), None);
        assert_eq!(transport.loop_range_beats(), None);
        assert_eq!(
            transport.time_signature(),
            Some(TimeSignature::new(7, 8)),
            "HAS_TIME_SIG was set"
        );
    }

    #[test]
    fn an_unflagged_transport_promises_nothing() {
        let transport = from_abi(&DauxTransportV1::new());
        assert!(!transport.is_playing());
        assert_eq!(transport.tempo(), None);
        assert_eq!(transport.beats(), None);
        assert_eq!(transport.bar_number(), None);
    }

    #[test]
    fn a_snapshot_round_trips_through_the_abi_record() {
        let original = snapshot_from_abi(&filled());
        let there_and_back = snapshot_from_abi(&snapshot_to_abi(&original));
        assert_eq!(there_and_back, original);
        assert_eq!(snapshot_to_abi(&original).size, DauxTransportV1::SIZE);
        assert_eq!(snapshot_to_abi(&original).reserved, [0; 6]);
    }

    #[test]
    fn unknown_flag_bits_survive_the_crossing() {
        let mut abi = filled();
        abi.flags |= 1 << 20;
        assert_eq!(from_abi(&abi).flags.bits() & (1 << 20), 1 << 20);
        assert_eq!(snapshot_from_abi(&abi).flags & (1 << 20), 1 << 20);
    }

    #[test]
    fn a_short_record_is_not_usable() {
        let mut abi = filled();
        assert!(is_usable(&abi));
        abi.size = 8;
        assert!(!is_usable(&abi));
    }
}
