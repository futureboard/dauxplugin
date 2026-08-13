//! Translating between VST3 events and the DAUx event model.
//!
//! # What maps
//!
//! | VST3 | DAUx |
//! |---|---|
//! | `NoteOnEvent` / `NoteOffEvent` | [`DauxEvent::NoteOn`] / [`DauxEvent::NoteOff`] |
//! | `PolyPressureEvent` | [`DauxEvent::NoteExpression`] with [`NoteExpression::Pressure`] |
//! | `NoteExpressionValueEvent` | [`DauxEvent::NoteExpression`] |
//! | `DataEvent` with `kMidiSysEx` | [`DauxEvent::SysEx`] |
//! | `IParameterChanges` | [`DauxEvent::ParamValue`], with a **plain** value |
//!
//! # What does not
//!
//! VST3 has no event for a plain MIDI 1.0 message: continuous controllers reach a plug-in as
//! *parameters*, through `IMidiMapping`, and a plug-in's own controller output goes through
//! `LegacyMIDICCOutEvent`. Neither is implemented here, so `DauxEvent::Midi1` and
//! `DauxEvent::Midi2` are dropped in both directions and [`crate::compat`] says so at build
//! time. MIDI 2.0 has no VST3 representation at all.
//!
//! `DauxEvent::NoteEnd` is also dropped: VST3 hosts infer voice lifetime rather than being
//! told, so there is nothing to send it to.
//!
//! # Units
//!
//! VST3 normalises everything to `0..=1`, including per-note tuning, which it defines as
//! `0..=1` spanning ±120 semitones. DAUx keeps tuning in **cents**, so the two are converted
//! rather than passed through — the one place in this module where a number changes meaning.

use daux_plugin_api::{
    DauxEvent, EventFlags, EventHeader, NoteEvent, NoteExpression, NoteExpressionEvent, SysExEvent,
};

use crate::api::{self, event_flags, event_type, note_expression_type};

/// Semitones either side of centre that VST3's normalised tuning covers.
const TUNING_SEMITONE_SPAN: f64 = 120.0;
/// Cents per semitone.
const CENTS_PER_SEMITONE: f64 = 100.0;

/// The full ±span in cents, so the two conversions cannot drift apart.
const TUNING_CENTS_SPAN: f64 = TUNING_SEMITONE_SPAN * CENTS_PER_SEMITONE;

/// `[audio-thread]` VST3's normalised tuning as DAUx cents.
///
/// `NaN` becomes no detune rather than propagating: a `NaN` reaching an oscillator's
/// frequency silences a voice for the rest of the session.
#[must_use]
pub fn tuning_to_cents(normalized: f64) -> f64 {
    if normalized.is_nan() {
        return 0.0;
    }
    (normalized.clamp(0.0, 1.0) * 2.0 - 1.0) * TUNING_CENTS_SPAN
}

/// `[audio-thread]` DAUx cents as VST3's normalised tuning, clamped to `0..=1`.
#[must_use]
pub fn cents_to_tuning(cents: f64) -> f64 {
    if cents.is_nan() {
        return 0.5;
    }
    (((cents / TUNING_CENTS_SPAN) + 1.0) * 0.5).clamp(0.0, 1.0)
}

/// VST3's note-expression id as DAUx's, or `None` for one DAUx has no name for.
#[must_use]
fn expression_from_vst3(type_id: u32) -> Option<NoteExpression> {
    Some(match type_id {
        note_expression_type::VOLUME => NoteExpression::Volume,
        note_expression_type::PAN => NoteExpression::Pan,
        note_expression_type::TUNING => NoteExpression::Tuning,
        note_expression_type::VIBRATO => NoteExpression::Vibrato,
        note_expression_type::EXPRESSION => NoteExpression::Expression,
        note_expression_type::BRIGHTNESS => NoteExpression::Brightness,
        note_expression_type::PRESSURE => NoteExpression::Pressure,
        _ => return None,
    })
}

/// DAUx's note-expression id as VST3's.
#[must_use]
fn expression_to_vst3(expression: NoteExpression) -> u32 {
    match expression {
        NoteExpression::Volume => note_expression_type::VOLUME,
        NoteExpression::Pan => note_expression_type::PAN,
        NoteExpression::Tuning => note_expression_type::TUNING,
        NoteExpression::Vibrato => note_expression_type::VIBRATO,
        NoteExpression::Expression => note_expression_type::EXPRESSION,
        NoteExpression::Brightness => note_expression_type::BRIGHTNESS,
        NoteExpression::Pressure => note_expression_type::PRESSURE,
    }
}

/// The DAUx header for a VST3 event.
fn header(event: &api::Event, port: u16) -> EventHeader {
    let flags = if event.flags & event_flags::IS_LIVE != 0 {
        EventFlags::IS_LIVE
    } else {
        EventFlags::NONE
    };
    EventHeader::new(u32::try_from(event.sample_offset).unwrap_or(0), port, flags)
}

/// `[audio-thread]` A VST3 event as a DAUx one, or `None` when there is no equivalent.
///
/// `port` is the DAUx event port index the host's `busIndex` maps to; the caller has already
/// bounds-checked it against the plug-in's event port layout.
///
/// Allocation-free. The returned event borrows `event` — and, for SysEx, the payload the
/// host pointed at — for exactly the lifetime the caller chooses, which must not outlive the
/// `process` call (abi-v1 §16.3).
///
/// # Safety
///
/// For a `DataEvent`, `event.payload.data.bytes` must be null or point to `size` readable
/// bytes that stay valid for `'a`. Every other event type is read from `event` alone.
#[must_use]
pub unsafe fn to_daux<'a>(event: &'a api::Event, port: u16) -> Option<DauxEvent<'a>> {
    let header = header(event, port);
    match event.event_type {
        event_type::NOTE_ON => {
            // SAFETY: `event_type` selects the union arm, which is exactly what VST3
            // guarantees; `NoteOnEvent` is plain data with no invalid bit patterns.
            let n = unsafe { event.payload.note_on };
            Some(DauxEvent::NoteOn(NoteEvent {
                header,
                note_id: n.note_id,
                channel: n.channel,
                key: n.pitch,
                velocity: f64::from(n.velocity),
                tuning: f64::from(n.tuning),
            }))
        }
        event_type::NOTE_OFF => {
            // SAFETY: as above, for the note-off arm.
            let n = unsafe { event.payload.note_off };
            Some(DauxEvent::NoteOff(NoteEvent {
                header,
                note_id: n.note_id,
                channel: n.channel,
                key: n.pitch,
                velocity: f64::from(n.velocity),
                tuning: f64::from(n.tuning),
            }))
        }
        event_type::POLY_PRESSURE => {
            // SAFETY: as above, for the poly-pressure arm.
            let p = unsafe { event.payload.poly_pressure };
            Some(DauxEvent::NoteExpression(NoteExpressionEvent {
                header,
                expression: NoteExpression::Pressure,
                note_id: p.note_id,
                channel: p.channel,
                key: p.pitch,
                value: f64::from(p.pressure),
            }))
        }
        event_type::NOTE_EXPRESSION_VALUE => {
            // SAFETY: as above, for the note-expression arm.
            let e = unsafe { event.payload.note_expression_value };
            let expression = expression_from_vst3(e.type_id)?;
            let value = if expression == NoteExpression::Tuning {
                tuning_to_cents(e.value)
            } else {
                e.value
            };
            Some(DauxEvent::NoteExpression(NoteExpressionEvent {
                header,
                expression,
                note_id: e.note_id,
                // VST3 addresses note expression by voice id alone.
                channel: -1,
                key: -1,
                value,
            }))
        }
        event_type::DATA => {
            // SAFETY: as above, for the data arm.
            let d = unsafe { event.payload.data };
            if d.data_type != api::K_MIDI_SYSEX || d.bytes.is_null() || d.size == 0 {
                return None;
            }
            let len = usize::try_from(d.size).ok()?;
            // SAFETY: the caller promises `size` readable bytes at `bytes` for `'a`. A
            // `u8` slice needs no alignment beyond one byte.
            let bytes = unsafe { core::slice::from_raw_parts(d.bytes, len) };
            Some(DauxEvent::SysEx(SysExEvent { header, bytes }))
        }
        _ => None,
    }
}

/// `[audio-thread]` A DAUx event as a VST3 one, or `None` when VST3 has no equivalent.
///
/// Allocation-free. SysEx output is *not* produced: VST3's `DataEvent` borrows a pointer the
/// host reads after `process` returns, and DAUx's output events borrow the plug-in's own
/// block-scoped arena, so handing the pointer over would be a use-after-free. Dropping it is
/// the honest answer, and [`crate::compat`] reports it.
#[must_use]
pub fn from_daux(event: &DauxEvent<'_>, bus_index: i32) -> Option<api::Event> {
    let head = event.header();
    let mut out = api::Event {
        bus_index,
        sample_offset: i32::try_from(head.time).unwrap_or(i32::MAX),
        ppq_position: 0.0,
        flags: if head.flags.is_live() {
            event_flags::IS_LIVE
        } else {
            0
        },
        event_type: event_type::NOTE_ON,
        payload: api::EventPayload::default(),
    };

    match event {
        DauxEvent::NoteOn(n) => {
            out.event_type = event_type::NOTE_ON;
            out.payload = api::EventPayload {
                note_on: api::NoteOnEvent {
                    channel: n.channel.max(0),
                    pitch: n.key.max(0),
                    tuning: n.tuning as f32,
                    velocity: n.velocity as f32,
                    length: 0,
                    note_id: n.note_id,
                },
            };
            Some(out)
        }
        DauxEvent::NoteOff(n) | DauxEvent::NoteChoke(n) => {
            out.event_type = event_type::NOTE_OFF;
            out.payload = api::EventPayload {
                note_off: api::NoteOffEvent {
                    channel: n.channel.max(0),
                    pitch: n.key.max(0),
                    velocity: n.velocity as f32,
                    note_id: n.note_id,
                    tuning: n.tuning as f32,
                },
            };
            Some(out)
        }
        DauxEvent::NoteExpression(e) => {
            out.event_type = event_type::NOTE_EXPRESSION_VALUE;
            let value = if e.expression == NoteExpression::Tuning {
                cents_to_tuning(e.value)
            } else {
                e.value.clamp(0.0, 1.0)
            };
            out.payload = api::EventPayload {
                note_expression_value: api::NoteExpressionValueEvent {
                    type_id: expression_to_vst3(e.expression),
                    note_id: e.note_id,
                    value,
                },
            };
            Some(out)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use daux_plugin_api::EventHeader as DauxHeader;

    fn note_on(offset: i32) -> api::Event {
        api::Event {
            sample_offset: offset,
            event_type: event_type::NOTE_ON,
            flags: event_flags::IS_LIVE,
            payload: api::EventPayload {
                note_on: api::NoteOnEvent {
                    channel: 3,
                    pitch: 64,
                    tuning: 12.5,
                    velocity: 0.75,
                    length: 0,
                    note_id: 42,
                },
            },
            ..api::Event::default()
        }
    }

    #[test]
    fn a_note_on_survives_the_round_trip() {
        let vst = note_on(128);
        // SAFETY: the event carries no payload pointer.
        let daux = unsafe { to_daux(&vst, 0) }.expect("note on maps");
        let DauxEvent::NoteOn(note) = daux else {
            panic!("expected a note on, got {daux:?}");
        };
        assert_eq!(note.header.time, 128);
        assert_eq!(note.header.port_index, 0);
        assert!(note.header.flags.is_live());
        assert_eq!(note.channel, 3);
        assert_eq!(note.key, 64);
        assert_eq!(note.note_id, 42);
        assert!((note.velocity - 0.75).abs() < 1e-6);
        assert!((note.tuning - 12.5).abs() < 1e-4);

        let back = from_daux(&daux, 0).expect("and back again");
        assert_eq!(back.event_type, event_type::NOTE_ON);
        assert_eq!(back.sample_offset, 128);
        assert_eq!(back.flags, event_flags::IS_LIVE);
        // SAFETY: `event_type` says the note-on arm is live.
        let n = unsafe { back.payload.note_on };
        assert_eq!((n.channel, n.pitch, n.note_id), (3, 64, 42));
        assert!((n.velocity - 0.75).abs() < 1e-6);
    }

    #[test]
    fn a_note_off_keeps_its_release_velocity_and_voice_id() {
        let vst = api::Event {
            sample_offset: 7,
            event_type: event_type::NOTE_OFF,
            payload: api::EventPayload {
                note_off: api::NoteOffEvent {
                    channel: 0,
                    pitch: 60,
                    velocity: 0.25,
                    note_id: 9,
                    tuning: 0.0,
                },
            },
            ..api::Event::default()
        };
        // SAFETY: no payload pointer.
        let daux = unsafe { to_daux(&vst, 1) }.expect("note off maps");
        let DauxEvent::NoteOff(note) = daux else {
            panic!("expected a note off");
        };
        assert_eq!(note.header.port_index, 1);
        assert_eq!(note.note_id, 9);
        assert!((note.velocity - 0.25).abs() < 1e-6);
    }

    #[test]
    fn poly_pressure_becomes_per_note_pressure() {
        let vst = api::Event {
            event_type: event_type::POLY_PRESSURE,
            payload: api::EventPayload {
                poly_pressure: api::PolyPressureEvent {
                    channel: 2,
                    pitch: 48,
                    pressure: 0.5,
                    note_id: -1,
                },
            },
            ..api::Event::default()
        };
        // SAFETY: no payload pointer.
        let daux = unsafe { to_daux(&vst, 0) }.expect("poly pressure maps");
        let DauxEvent::NoteExpression(e) = daux else {
            panic!("expected a note expression");
        };
        assert_eq!(e.expression, NoteExpression::Pressure);
        assert_eq!((e.channel, e.key), (2, 48));
        assert!((e.value - 0.5).abs() < 1e-6);
    }

    #[test]
    fn tuning_is_converted_between_normalised_semitones_and_cents() {
        // Centre is no detune, the ends are the full ±120 semitones.
        assert!((tuning_to_cents(0.5)).abs() < 1e-9);
        assert!((tuning_to_cents(1.0) - 12_000.0).abs() < 1e-9);
        assert!((tuning_to_cents(0.0) + 12_000.0).abs() < 1e-9);
        // One semitone up.
        let one_semitone = 0.5 + 1.0 / 240.0;
        assert!((tuning_to_cents(one_semitone) - 100.0).abs() < 1e-9);

        for cents in [-12_000.0, -100.0, 0.0, 50.0, 12_000.0] {
            let round_trip = tuning_to_cents(cents_to_tuning(cents));
            assert!(
                (round_trip - cents).abs() < 1e-6,
                "{cents} cents came back as {round_trip}"
            );
        }
        // Out-of-range and NaN clamp rather than escape.
        assert!(cents_to_tuning(1e9).is_finite());
        assert_eq!(cents_to_tuning(f64::NAN), 0.5);
        assert!(tuning_to_cents(f64::NAN).is_finite());
    }

    #[test]
    fn note_expression_maps_every_dimension_both_ways() {
        for (id, expression) in [
            (note_expression_type::VOLUME, NoteExpression::Volume),
            (note_expression_type::PAN, NoteExpression::Pan),
            (note_expression_type::VIBRATO, NoteExpression::Vibrato),
            (note_expression_type::EXPRESSION, NoteExpression::Expression),
            (note_expression_type::BRIGHTNESS, NoteExpression::Brightness),
            (note_expression_type::PRESSURE, NoteExpression::Pressure),
        ] {
            let vst = api::Event {
                event_type: event_type::NOTE_EXPRESSION_VALUE,
                payload: api::EventPayload {
                    note_expression_value: api::NoteExpressionValueEvent {
                        type_id: id,
                        note_id: 5,
                        value: 0.25,
                    },
                },
                ..api::Event::default()
            };
            // SAFETY: no payload pointer.
            let daux = unsafe { to_daux(&vst, 0) }.expect("expression maps");
            let DauxEvent::NoteExpression(e) = daux else {
                panic!("expected a note expression");
            };
            assert_eq!(e.expression, expression);
            assert!((e.value - 0.25).abs() < 1e-9);

            let back = from_daux(&daux, 0).expect("and back");
            // SAFETY: `event_type` says the expression arm is live.
            let payload = unsafe { back.payload.note_expression_value };
            assert_eq!(payload.type_id, id);
            assert!((payload.value - 0.25).abs() < 1e-9);
        }
    }

    #[test]
    fn an_expression_dimension_daux_has_no_name_for_is_dropped() {
        let vst = api::Event {
            event_type: event_type::NOTE_EXPRESSION_VALUE,
            payload: api::EventPayload {
                note_expression_value: api::NoteExpressionValueEvent {
                    type_id: 12_345,
                    note_id: 0,
                    value: 0.5,
                },
            },
            ..api::Event::default()
        };
        // SAFETY: no payload pointer.
        assert!(unsafe { to_daux(&vst, 0) }.is_none());
    }

    #[test]
    fn sysex_borrows_the_hosts_bytes() {
        let payload = [0xF0u8, 0x7E, 0x00, 0x06, 0x01, 0xF7];
        let vst = api::Event {
            event_type: event_type::DATA,
            payload: api::EventPayload {
                data: api::DataEvent {
                    size: payload.len() as u32,
                    data_type: api::K_MIDI_SYSEX,
                    bytes: payload.as_ptr(),
                },
            },
            ..api::Event::default()
        };
        // SAFETY: `payload` is a live array of exactly `size` bytes that outlives `daux`.
        let daux = unsafe { to_daux(&vst, 0) }.expect("sysex maps");
        let DauxEvent::SysEx(e) = daux else {
            panic!("expected sysex");
        };
        assert_eq!(e.bytes, &payload);
        // …and it is not sent back out, because the pointer would dangle.
        assert!(from_daux(&daux, 0).is_none());
    }

    #[test]
    fn a_hostile_data_event_is_dropped_rather_than_dereferenced() {
        let null_bytes = api::Event {
            event_type: event_type::DATA,
            payload: api::EventPayload {
                data: api::DataEvent {
                    size: 16,
                    data_type: api::K_MIDI_SYSEX,
                    bytes: core::ptr::null(),
                },
            },
            ..api::Event::default()
        };
        // SAFETY: the null pointer is checked for before any slice is built.
        assert!(unsafe { to_daux(&null_bytes, 0) }.is_none());

        let wrong_type = api::Event {
            event_type: event_type::DATA,
            payload: api::EventPayload {
                data: api::DataEvent {
                    size: 4,
                    data_type: 999,
                    bytes: [1u8, 2, 3, 4].as_ptr(),
                },
            },
            ..api::Event::default()
        };
        // SAFETY: the payload is live; the unknown data type is rejected first anyway.
        assert!(unsafe { to_daux(&wrong_type, 0) }.is_none());
    }

    #[test]
    fn a_negative_sample_offset_does_not_wrap_into_the_far_future() {
        let mut vst = note_on(-1);
        vst.sample_offset = -1;
        // SAFETY: no payload pointer.
        let daux = unsafe { to_daux(&vst, 0) }.expect("still maps");
        assert_eq!(
            daux.time(),
            0,
            "a negative offset must clamp to the block start"
        );
    }

    #[test]
    fn events_vst3_cannot_carry_are_dropped_in_both_directions() {
        let unknown = api::Event {
            event_type: event_type::CHORD,
            ..api::Event::default()
        };
        // SAFETY: no payload pointer is read for an unhandled type.
        assert!(unsafe { to_daux(&unknown, 0) }.is_none());

        let midi = DauxEvent::Midi1(daux_plugin_api::Midi1Event {
            header: DauxHeader::at(0),
            message: daux_plugin_api::Midi1Message::note_on(0, 60, 100),
        });
        assert!(
            from_daux(&midi, 0).is_none(),
            "VST3 has no generic MIDI event; inventing one would send nonsense"
        );

        let end = DauxEvent::NoteEnd(NoteEvent::default());
        assert!(from_daux(&end, 0).is_none());
    }

    #[test]
    fn a_choke_leaves_as_a_note_off_rather_than_being_lost() {
        let choke = DauxEvent::NoteChoke(NoteEvent {
            header: DauxHeader::at(3),
            note_id: 1,
            channel: 0,
            key: 60,
            velocity: 0.0,
            tuning: 0.0,
        });
        let out = from_daux(&choke, 0).expect("a choke is at least a note off");
        assert_eq!(out.event_type, event_type::NOTE_OFF);
        assert_eq!(out.sample_offset, 3);
    }

    #[test]
    fn wildcard_channels_and_keys_do_not_become_negative_vst3_fields() {
        let wildcard = DauxEvent::NoteOff(NoteEvent {
            header: DauxHeader::at(0),
            note_id: 11,
            channel: -1,
            key: -1,
            velocity: 0.5,
            tuning: 0.0,
        });
        let out = from_daux(&wildcard, 0).expect("maps");
        // SAFETY: `event_type` says the note-off arm is live.
        let n = unsafe { out.payload.note_off };
        assert_eq!((n.channel, n.pitch), (0, 0));
        assert_eq!(n.note_id, 11, "the voice id is what identifies the note");
    }
}
