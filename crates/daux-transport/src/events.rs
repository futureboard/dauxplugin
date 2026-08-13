//! The one legal home for `Transport` ⇄ `TransportSnapshot`.
//!
//! `daux-events` owns [`TransportSnapshot`], the flat `Copy` mirror of `DauxTransportV1`. It
//! sits at the very bottom of the dependency graph — the event model is reachable from every
//! adapter and from the audio thread — so it cannot depend on `daux-transport`.
//!
//! Rust's orphan rule then leaves exactly one legal home for the conversion: this crate, the
//! one that owns [`Transport`]. A third crate that depends on both, such as `daux-core`,
//! could not write these impls even though it can see both types. Hence the one internal
//! dependency in `daux-transport`'s manifest; it remains free of *external* dependencies.

use daux_events::TransportSnapshot;

use crate::{TimeSignature, Transport, TransportFlags};

/// Flattens the musical timeline into the `Copy` mirror the event model carries.
///
/// The flag word is copied verbatim — [`TransportFlags`] and [`transport_flags`] are the same
/// bit assignment (abi-v1 §10) — so unknown bits set by a newer host survive the round trip
/// rather than being silently dropped.
impl From<Transport> for TransportSnapshot {
    fn from(t: Transport) -> Self {
        Self {
            flags: t.flags.bits(),
            song_pos_samples: t.song_pos_samples,
            song_pos_beats: t.song_pos_beats,
            song_pos_seconds: t.song_pos_seconds,
            tempo: t.tempo,
            tempo_increment: t.tempo_increment,
            bar_start_beats: t.bar_start_beats,
            bar_number: t.bar_number,
            time_sig_numerator: t.time_signature.numerator,
            time_sig_denominator: t.time_signature.denominator,
            loop_start_beats: t.loop_start_beats,
            loop_end_beats: t.loop_end_beats,
            loop_start_seconds: t.loop_start_seconds,
            loop_end_seconds: t.loop_end_seconds,
        }
    }
}

/// Rebuilds the musical timeline from the flat mirror.
///
/// Lossless in both directions: the two structs carry exactly the same fields, differing only
/// in that `Transport` groups the two time-signature integers into a [`TimeSignature`].
impl From<TransportSnapshot> for Transport {
    fn from(s: TransportSnapshot) -> Self {
        Self {
            flags: TransportFlags::from_bits(s.flags),
            song_pos_samples: s.song_pos_samples,
            song_pos_beats: s.song_pos_beats,
            song_pos_seconds: s.song_pos_seconds,
            tempo: s.tempo,
            tempo_increment: s.tempo_increment,
            bar_start_beats: s.bar_start_beats,
            bar_number: s.bar_number,
            time_signature: TimeSignature::new(s.time_sig_numerator, s.time_sig_denominator),
            loop_start_beats: s.loop_start_beats,
            loop_end_beats: s.loop_end_beats,
            loop_start_seconds: s.loop_start_seconds,
            loop_end_seconds: s.loop_end_seconds,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use daux_events::transport_flags;

    fn populated() -> Transport {
        Transport {
            flags: TransportFlags::ALL,
            song_pos_samples: 96_000,
            song_pos_beats: 4.0,
            song_pos_seconds: 2.0,
            tempo: 120.0,
            tempo_increment: 0.001,
            bar_start_beats: 4.0,
            bar_number: 1,
            time_signature: TimeSignature::new(7, 8),
            loop_start_beats: 0.0,
            loop_end_beats: 16.0,
            loop_start_seconds: 0.0,
            loop_end_seconds: 8.0,
        }
    }

    #[test]
    fn a_populated_transport_round_trips() {
        let t = populated();
        let round_tripped = Transport::from(TransportSnapshot::from(t));
        assert_eq!(round_tripped, t);
    }

    #[test]
    fn an_empty_transport_round_trips() {
        let t = Transport::EMPTY;
        assert_eq!(Transport::from(TransportSnapshot::from(t)), t);
        assert_eq!(TransportSnapshot::from(t).flags, 0);
    }

    #[test]
    fn an_unknown_snapshot_round_trips() {
        let s = TransportSnapshot::unknown();
        assert_eq!(TransportSnapshot::from(Transport::from(s)), s);
    }

    #[test]
    fn the_two_flag_words_are_the_same_bit_assignment() {
        assert_eq!(TransportFlags::HAS_TEMPO.bits(), transport_flags::HAS_TEMPO);
        assert_eq!(TransportFlags::HAS_BEATS.bits(), transport_flags::HAS_BEATS);
        assert_eq!(
            TransportFlags::HAS_SECONDS.bits(),
            transport_flags::HAS_SECONDS
        );
        assert_eq!(
            TransportFlags::HAS_TIME_SIG.bits(),
            transport_flags::HAS_TIME_SIG
        );
        assert_eq!(TransportFlags::HAS_LOOP.bits(), transport_flags::HAS_LOOP);
        assert_eq!(TransportFlags::HAS_BAR.bits(), transport_flags::HAS_BAR);
        assert_eq!(
            TransportFlags::IS_PLAYING.bits(),
            transport_flags::IS_PLAYING
        );
        assert_eq!(
            TransportFlags::IS_RECORDING.bits(),
            transport_flags::IS_RECORDING
        );
        assert_eq!(
            TransportFlags::IS_LOOPING.bits(),
            transport_flags::IS_LOOPING
        );
        assert_eq!(
            TransportFlags::IS_PREROLL.bits(),
            transport_flags::IS_PREROLL
        );
    }

    #[test]
    fn bits_from_a_newer_host_survive_the_conversion() {
        let mut t = populated();
        t.flags = TransportFlags::from_bits(TransportFlags::ALL.bits() | (1 << 24));
        let s = TransportSnapshot::from(t);
        assert_eq!(s.flags & (1 << 24), 1 << 24);
        assert_eq!(Transport::from(s).flags.bits(), t.flags.bits());
    }

    #[test]
    fn the_accessors_agree_after_conversion() {
        let t = populated();
        let s = TransportSnapshot::from(t);
        assert_eq!(s.is_playing(), t.is_playing());
        assert_eq!(s.is_recording(), t.is_recording());
        assert_eq!(s.is_looping(), t.is_looping());
        assert_eq!(s.tempo(), t.tempo());
        assert_eq!(s.beats(), t.beats());
    }
}
