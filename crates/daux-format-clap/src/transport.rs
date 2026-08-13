//! `clap_event_transport` ↔ [`Transport`].
//!
//! The two models say almost the same thing, with three differences that matter:
//!
//! * CLAP stores beats and seconds as 64-bit fixed point (`clap_beattime`,
//!   `clap_sectime`); DAUx stores them as `f64`.
//! * CLAP has no sample position on the song timeline. DAUx does, and plug-ins use it, so
//!   it is derived from the seconds timeline when the host provides one and left at zero
//!   when it does not — never invented.
//! * CLAP has no separate "bar fields are valid" flag: `bar_start` and `bar_number` are
//!   meaningful exactly when the beats timeline is, and the loop fields exactly when the
//!   loop is active.
//!
//! `[audio-thread]` — every function here is arithmetic on `Copy` values.

use daux_plugin_api::{TimeSignature, Transport, TransportFlags};

use crate::abi::{
    CLAP_BEATTIME_FACTOR, CLAP_EVENT_TRANSPORT, CLAP_SECTIME_FACTOR,
    CLAP_TRANSPORT_HAS_BEATS_TIMELINE, CLAP_TRANSPORT_HAS_SECONDS_TIMELINE,
    CLAP_TRANSPORT_HAS_TEMPO, CLAP_TRANSPORT_HAS_TIME_SIGNATURE, CLAP_TRANSPORT_IS_LOOP_ACTIVE,
    CLAP_TRANSPORT_IS_PLAYING, CLAP_TRANSPORT_IS_RECORDING, CLAP_TRANSPORT_IS_WITHIN_PRE_ROLL,
    ClapEventHeader, ClapEventTransport,
};

/// `[audio-thread]` Fixed-point beats to `f64` quarter-note beats.
#[must_use]
pub const fn beats_to_f64(raw: i64) -> f64 {
    raw as f64 / CLAP_BEATTIME_FACTOR as f64
}

/// `[audio-thread]` `f64` quarter-note beats to fixed-point beats, saturating.
#[must_use]
pub fn beats_from_f64(v: f64) -> i64 {
    saturating_fixed(v, CLAP_BEATTIME_FACTOR)
}

/// `[audio-thread]` Fixed-point seconds to `f64` seconds.
#[must_use]
pub const fn seconds_to_f64(raw: i64) -> f64 {
    raw as f64 / CLAP_SECTIME_FACTOR as f64
}

/// `[audio-thread]` `f64` seconds to fixed-point seconds, saturating.
#[must_use]
pub fn seconds_from_f64(v: f64) -> i64 {
    saturating_fixed(v, CLAP_SECTIME_FACTOR)
}

/// Scales and rounds to `i64`, turning NaN into `0` and overflow into a saturation rather
/// than the undefined result a bare `as` cast used to give. `[audio-thread]`
fn saturating_fixed(v: f64, factor: i64) -> i64 {
    if v.is_nan() {
        return 0;
    }
    // `f64 as i64` saturates in Rust, so the only case left to handle is NaN.
    (v * factor as f64) as i64
}

/// `[audio-thread]` Reads a CLAP transport into the DAUx one.
///
/// `sample_rate` is used only to derive [`Transport::song_pos_samples`] from the seconds
/// timeline. A host that provides no seconds timeline leaves the sample position at `0`;
/// guessing it from the beats timeline would need a tempo map the plug-in does not have.
#[must_use]
pub fn transport_from_clap(t: &ClapEventTransport, sample_rate: f64) -> Transport {
    let mut flags = TransportFlags::NONE;
    let mut set = |clap_bit: u32, daux_bit: TransportFlags| {
        if t.flags & clap_bit != 0 {
            flags = flags.union(daux_bit);
        }
    };
    set(CLAP_TRANSPORT_HAS_TEMPO, TransportFlags::HAS_TEMPO);
    set(
        CLAP_TRANSPORT_HAS_SECONDS_TIMELINE,
        TransportFlags::HAS_SECONDS,
    );
    set(
        CLAP_TRANSPORT_HAS_TIME_SIGNATURE,
        TransportFlags::HAS_TIME_SIG,
    );
    set(CLAP_TRANSPORT_IS_PLAYING, TransportFlags::IS_PLAYING);
    set(CLAP_TRANSPORT_IS_RECORDING, TransportFlags::IS_RECORDING);
    set(
        CLAP_TRANSPORT_IS_WITHIN_PRE_ROLL,
        TransportFlags::IS_PREROLL,
    );
    // CLAP's beats timeline covers the bar fields too, and its loop fields are meaningful
    // exactly while the loop is armed.
    set(
        CLAP_TRANSPORT_HAS_BEATS_TIMELINE,
        TransportFlags::HAS_BEATS.union(TransportFlags::HAS_BAR),
    );
    set(
        CLAP_TRANSPORT_IS_LOOP_ACTIVE,
        TransportFlags::IS_LOOPING.union(TransportFlags::HAS_LOOP),
    );

    let song_pos_seconds = seconds_to_f64(t.song_pos_seconds);
    let song_pos_samples = if flags.contains(TransportFlags::HAS_SECONDS)
        && sample_rate.is_finite()
        && sample_rate > 0.0
    {
        (song_pos_seconds * sample_rate) as i64
    } else {
        0
    };

    Transport {
        flags,
        song_pos_samples,
        song_pos_beats: beats_to_f64(t.song_pos_beats),
        song_pos_seconds,
        tempo: t.tempo,
        tempo_increment: t.tempo_inc,
        bar_start_beats: beats_to_f64(t.bar_start),
        bar_number: t.bar_number,
        time_signature: TimeSignature {
            numerator: t.tsig_num,
            denominator: t.tsig_denom,
        },
        loop_start_beats: beats_to_f64(t.loop_start_beats),
        loop_end_beats: beats_to_f64(t.loop_end_beats),
        loop_start_seconds: seconds_to_f64(t.loop_start_seconds),
        loop_end_seconds: seconds_to_f64(t.loop_end_seconds),
    }
}

/// `[audio-thread]` Writes a DAUx transport back into a CLAP event.
///
/// Used when a plug-in emits a transport event on its output list. Fields whose `HAS_*`
/// flag is clear are written as zero, so a host cannot read a value the plug-in never
/// promised.
#[must_use]
pub fn transport_to_clap(t: &Transport, time: u32) -> ClapEventTransport {
    let mut flags = 0u32;
    let mut set = |daux_bit: TransportFlags, clap_bit: u32| {
        if t.flags.contains(daux_bit) {
            flags |= clap_bit;
        }
    };
    set(TransportFlags::HAS_TEMPO, CLAP_TRANSPORT_HAS_TEMPO);
    set(TransportFlags::HAS_BEATS, CLAP_TRANSPORT_HAS_BEATS_TIMELINE);
    set(
        TransportFlags::HAS_SECONDS,
        CLAP_TRANSPORT_HAS_SECONDS_TIMELINE,
    );
    set(
        TransportFlags::HAS_TIME_SIG,
        CLAP_TRANSPORT_HAS_TIME_SIGNATURE,
    );
    set(TransportFlags::IS_PLAYING, CLAP_TRANSPORT_IS_PLAYING);
    set(TransportFlags::IS_RECORDING, CLAP_TRANSPORT_IS_RECORDING);
    set(TransportFlags::IS_LOOPING, CLAP_TRANSPORT_IS_LOOP_ACTIVE);
    set(
        TransportFlags::IS_PREROLL,
        CLAP_TRANSPORT_IS_WITHIN_PRE_ROLL,
    );

    let has = |bit: TransportFlags| t.flags.contains(bit);
    ClapEventTransport {
        header: ClapEventHeader {
            size: size_of::<ClapEventTransport>() as u32,
            time,
            space_id: crate::abi::CLAP_CORE_EVENT_SPACE_ID,
            type_: CLAP_EVENT_TRANSPORT,
            flags: 0,
        },
        flags,
        song_pos_beats: if has(TransportFlags::HAS_BEATS) {
            beats_from_f64(t.song_pos_beats)
        } else {
            0
        },
        song_pos_seconds: if has(TransportFlags::HAS_SECONDS) {
            seconds_from_f64(t.song_pos_seconds)
        } else {
            0
        },
        tempo: if has(TransportFlags::HAS_TEMPO) {
            t.tempo
        } else {
            0.0
        },
        tempo_inc: if has(TransportFlags::HAS_TEMPO) {
            t.tempo_increment
        } else {
            0.0
        },
        loop_start_beats: if has(TransportFlags::HAS_LOOP) {
            beats_from_f64(t.loop_start_beats)
        } else {
            0
        },
        loop_end_beats: if has(TransportFlags::HAS_LOOP) {
            beats_from_f64(t.loop_end_beats)
        } else {
            0
        },
        loop_start_seconds: if has(TransportFlags::HAS_LOOP) {
            seconds_from_f64(t.loop_start_seconds)
        } else {
            0
        },
        loop_end_seconds: if has(TransportFlags::HAS_LOOP) {
            seconds_from_f64(t.loop_end_seconds)
        } else {
            0
        },
        bar_start: if has(TransportFlags::HAS_BAR) {
            beats_from_f64(t.bar_start_beats)
        } else {
            0
        },
        bar_number: if has(TransportFlags::HAS_BAR) {
            t.bar_number
        } else {
            0
        },
        tsig_num: if has(TransportFlags::HAS_TIME_SIG) {
            t.time_signature.numerator
        } else {
            0
        },
        tsig_denom: if has(TransportFlags::HAS_TIME_SIG) {
            t.time_signature.denominator
        } else {
            0
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use daux_plugin_api::TransportBuilder;

    fn playing_clap() -> ClapEventTransport {
        ClapEventTransport {
            header: ClapEventHeader {
                size: size_of::<ClapEventTransport>() as u32,
                time: 0,
                space_id: crate::abi::CLAP_CORE_EVENT_SPACE_ID,
                type_: CLAP_EVENT_TRANSPORT,
                flags: 0,
            },
            flags: CLAP_TRANSPORT_HAS_TEMPO
                | CLAP_TRANSPORT_HAS_BEATS_TIMELINE
                | CLAP_TRANSPORT_HAS_SECONDS_TIMELINE
                | CLAP_TRANSPORT_HAS_TIME_SIGNATURE
                | CLAP_TRANSPORT_IS_PLAYING
                | CLAP_TRANSPORT_IS_LOOP_ACTIVE,
            song_pos_beats: 8 * CLAP_BEATTIME_FACTOR,
            song_pos_seconds: 4 * CLAP_SECTIME_FACTOR,
            tempo: 120.0,
            tempo_inc: 0.0,
            loop_start_beats: 4 * CLAP_BEATTIME_FACTOR,
            loop_end_beats: 12 * CLAP_BEATTIME_FACTOR,
            loop_start_seconds: 2 * CLAP_SECTIME_FACTOR,
            loop_end_seconds: 6 * CLAP_SECTIME_FACTOR,
            bar_start: 8 * CLAP_BEATTIME_FACTOR,
            bar_number: 3,
            tsig_num: 7,
            tsig_denom: 8,
        }
    }

    #[test]
    fn a_full_clap_transport_becomes_a_full_daux_one() {
        let t = transport_from_clap(&playing_clap(), 48_000.0);
        assert!(t.is_playing());
        assert!(!t.is_recording());
        assert!(t.is_looping());
        assert_eq!(t.tempo(), Some(120.0));
        assert_eq!(t.beats(), Some(8.0));
        assert_eq!(t.seconds(), Some(4.0));
        assert_eq!(
            t.time_signature(),
            Some(TimeSignature {
                numerator: 7,
                denominator: 8
            })
        );
        assert_eq!(t.loop_range_beats(), Some((4.0, 12.0)));
        assert_eq!(t.bar_number, 3);
        // Derived from the seconds timeline, never invented.
        assert_eq!(t.song_pos_samples, 4 * 48_000);
    }

    #[test]
    fn a_host_that_promises_nothing_makes_every_accessor_none() {
        let mut c = playing_clap();
        c.flags = 0;
        let t = transport_from_clap(&c, 48_000.0);
        assert_eq!(t.tempo(), None);
        assert_eq!(t.beats(), None);
        assert_eq!(t.seconds(), None);
        assert_eq!(t.time_signature(), None);
        assert_eq!(t.loop_range_beats(), None);
        assert!(!t.is_playing());
        assert_eq!(
            t.song_pos_samples, 0,
            "with no seconds timeline the sample position must not be guessed"
        );
    }

    #[test]
    fn the_bar_fields_ride_on_the_beats_timeline() {
        let mut c = playing_clap();
        c.flags = CLAP_TRANSPORT_HAS_BEATS_TIMELINE;
        let t = transport_from_clap(&c, 48_000.0);
        assert!(t.flags.contains(TransportFlags::HAS_BAR));
        assert!(t.flags.contains(TransportFlags::HAS_BEATS));

        c.flags = CLAP_TRANSPORT_HAS_SECONDS_TIMELINE;
        let t = transport_from_clap(&c, 48_000.0);
        assert!(!t.flags.contains(TransportFlags::HAS_BAR));
    }

    #[test]
    fn an_unusable_sample_rate_does_not_produce_a_nonsense_position() {
        for rate in [0.0, -48_000.0, f64::NAN, f64::INFINITY] {
            let t = transport_from_clap(&playing_clap(), rate);
            assert_eq!(t.song_pos_samples, 0, "rate {rate}");
        }
    }

    #[test]
    fn a_daux_transport_round_trips_through_clap() {
        let original = TransportBuilder::new()
            .playing(true)
            .recording(true)
            .looping(true)
            .tempo_ramp(128.0, 0.001)
            .beats(16.5)
            .seconds(7.75)
            .time_signature(5, 4)
            .bar(4, 16.0)
            .loop_beats(8.0, 24.0)
            .loop_seconds(3.0, 11.0)
            .build();

        let encoded = transport_to_clap(&original, 64);
        assert_eq!(encoded.header.time, 64);
        assert_eq!(encoded.header.type_, CLAP_EVENT_TRANSPORT);
        assert_eq!(
            encoded.header.size as usize,
            size_of::<ClapEventTransport>()
        );

        let back = transport_from_clap(&encoded, 48_000.0);
        assert_eq!(back.tempo(), Some(128.0));
        assert!((back.tempo_increment - 0.001).abs() < 1e-12);
        assert_eq!(back.beats(), Some(16.5));
        assert_eq!(back.seconds(), Some(7.75));
        assert_eq!(back.loop_range_beats(), Some((8.0, 24.0)));
        assert_eq!(
            back.time_signature(),
            Some(TimeSignature {
                numerator: 5,
                denominator: 4
            })
        );
        assert_eq!(back.bar_number, 4);
        assert!(back.is_playing() && back.is_recording() && back.is_looping());
    }

    #[test]
    fn fields_without_their_flag_are_written_as_zero() {
        let bare = TransportBuilder::new().playing(true).build();
        let encoded = transport_to_clap(&bare, 0);
        assert_eq!(encoded.tempo, 0.0);
        assert_eq!(encoded.song_pos_beats, 0);
        assert_eq!(encoded.song_pos_seconds, 0);
        assert_eq!(encoded.tsig_num, 0);
        assert_eq!(encoded.bar_number, 0);
        assert_eq!(encoded.flags, CLAP_TRANSPORT_IS_PLAYING);
    }

    #[test]
    fn fixed_point_conversion_survives_hostile_values() {
        assert_eq!(beats_from_f64(f64::NAN), 0);
        assert_eq!(seconds_from_f64(f64::NAN), 0);
        assert_eq!(beats_from_f64(f64::INFINITY), i64::MAX);
        assert_eq!(beats_from_f64(f64::NEG_INFINITY), i64::MIN);
        assert_eq!(beats_from_f64(1e300), i64::MAX);
        // Fractions the factor can represent exactly must be exact.
        assert_eq!(beats_to_f64(beats_from_f64(0.25)), 0.25);
        assert_eq!(seconds_to_f64(seconds_from_f64(-1.5)), -1.5);
        assert_eq!(beats_to_f64(0), 0.0);
    }
}
