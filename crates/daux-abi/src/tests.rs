//! Unit tests for the ABI transcription.
//!
//! The layout tests are the important ones: this crate is a binary contract, so a change
//! in field order, width or padding is a compatibility break even when it compiles.

use core::mem::{align_of, offset_of, size_of};

use crate::*;

/// Borrows the raw bytes of any value, used to prove that constructors zero everything
/// they are supposed to zero, padding included.
fn raw_bytes<T>(value: &T) -> &[u8] {
    // SAFETY: `value` is a live, aligned `T` we hold a shared reference to, so the region
    // `[value, value + size_of::<T>())` is readable for the lifetime of that borrow. Every
    // type this helper is used with is `#[repr(C)]` and was produced by a constructor that
    // initialises all bytes (it starts from `mem::zeroed`), so no padding byte is
    // uninitialised. `u8` has no alignment or validity requirement, and the returned slice
    // borrows `value`, so nothing can mutate it while the slice is alive.
    unsafe { core::slice::from_raw_parts((value as *const T).cast::<u8>(), size_of::<T>()) }
}

// ---------------------------------------------------------------------------------------
// Layout — compile-time assertions
// ---------------------------------------------------------------------------------------

/// The null-pointer optimisation is what makes an optional entry in a function table a
/// plain null pointer on the wire. Without it every `Option<fn>` field would be two words
/// and every table would be laid out differently from the C structure it mirrors.
const _: () = assert!(size_of::<Option<unsafe extern "C" fn()>>() == size_of::<usize>());
const _: () = assert!(
    size_of::<Option<unsafe extern "C" fn(DauxPluginHandle, u32) -> DauxStatus>>()
        == size_of::<usize>()
);

const _: () = assert!(size_of::<DauxStatus>() == size_of::<i32>());
const _: () = assert!(size_of::<DauxFactoryHandle>() == size_of::<*mut core::ffi::c_void>());
const _: () = assert!(size_of::<DauxName>() == DAUX_NAME_SIZE);
const _: () = assert!(size_of::<DauxText>() == DAUX_TEXT_SIZE);
const _: () = assert!(size_of::<DauxId>() == DAUX_ID_SIZE);
const _: () = assert!(size_of::<DauxPath>() == DAUX_PATH_SIZE);
const _: () = assert!(align_of::<DauxName>() == 1);

#[cfg(target_pointer_width = "64")]
mod const_layout_64 {
    use super::*;

    const _: () = assert!(size_of::<DauxPluginEntryV1>() == 184);
    const _: () = assert!(size_of::<DauxFactoryApiV1>() == 88);
    const _: () = assert!(size_of::<DauxPluginDescriptorV1>() == 1784);
    const _: () = assert!(size_of::<DauxPluginApiV1>() == 136);
    const _: () = assert!(size_of::<DauxProcessConfigV1>() == 80);
    const _: () = assert!(size_of::<DauxProcessV1>() == 112);
    const _: () = assert!(size_of::<DauxAudioBufferV1>() == 32);
    const _: () = assert!(size_of::<DauxEventHeaderV1>() == 16);
    const _: () = assert!(size_of::<DauxEventNoteV1>() == 48);
    const _: () = assert!(size_of::<DauxEventNoteExpressionV1>() == 40);
    const _: () = assert!(size_of::<DauxEventParamV1>() == 48);
    const _: () = assert!(size_of::<DauxEventMidi1V1>() == 20);
    const _: () = assert!(size_of::<DauxEventMidi2V1>() == 36);
    const _: () = assert!(size_of::<DauxEventSysExV1>() == 32);
    const _: () = assert!(size_of::<DauxEventTransportV1>() == 160);
    const _: () = assert!(size_of::<DauxEventListV1>() == 72);
    const _: () = assert!(size_of::<DauxTransportV1>() == 144);
    const _: () = assert!(size_of::<DauxAudioPortInfoV1>() == 120);
    const _: () = assert!(size_of::<DauxAudioPortsApiV1>() == 64);
    const _: () = assert!(size_of::<DauxParamInfoV1>() == 272);
    const _: () = assert!(size_of::<DauxParamsApiV1>() == 88);
    const _: () = assert!(size_of::<DauxStreamV1>() == 64);
    const _: () = assert!(size_of::<DauxStateApiV1>() == 56);
    const _: () = assert!(size_of::<DauxWindowV1>() == 24);
    const _: () = assert!(size_of::<DauxGuiApiV1>() == 144);
    const _: () = assert!(size_of::<DauxLatencyApiV1>() == 32);
    const _: () = assert!(size_of::<DauxTailApiV1>() == 32);
    const _: () = assert!(size_of::<DauxRenderApiV1>() == 40);
    const _: () = assert!(size_of::<DauxHostLogApiV1>() == 32);
    const _: () = assert!(size_of::<DauxHostParamsApiV1>() == 72);
    const _: () = assert!(size_of::<DauxHostWorkerApiV1>() == 32);
    const _: () = assert!(size_of::<DauxHostGuiApiV1>() == 72);
    const _: () = assert!(size_of::<DauxHostApiV1>() == 272);
    const _: () = assert!(size_of::<DauxSharedTextureV1>() == 104);
}

// ---------------------------------------------------------------------------------------
// Layout — field offsets
// ---------------------------------------------------------------------------------------

#[test]
fn entry_field_offsets_match_the_specification() {
    assert_eq!(offset_of!(DauxPluginEntryV1, size), 0);
    assert_eq!(offset_of!(DauxPluginEntryV1, abi_version_major), 4);
    assert_eq!(offset_of!(DauxPluginEntryV1, abi_version_minor), 8);
    assert_eq!(offset_of!(DauxPluginEntryV1, _pad0), 12);
    assert_eq!(offset_of!(DauxPluginEntryV1, magic), 16);
    assert_eq!(offset_of!(DauxPluginEntryV1, sdk_name), 24);
    assert_eq!(offset_of!(DauxPluginEntryV1, sdk_version), 88);
    assert_eq!(offset_of!(DauxPluginEntryV1, create_factory), 104);
    assert_eq!(offset_of!(DauxPluginEntryV1, destroy_factory), 112);
    assert_eq!(offset_of!(DauxPluginEntryV1, reserved), 120);
}

#[test]
fn descriptor_field_offsets_match_the_specification() {
    assert_eq!(offset_of!(DauxPluginDescriptorV1, id), 16);
    assert_eq!(offset_of!(DauxPluginDescriptorV1, name), 144);
    assert_eq!(offset_of!(DauxPluginDescriptorV1, vendor), 208);
    assert_eq!(offset_of!(DauxPluginDescriptorV1, version), 272);
    assert_eq!(offset_of!(DauxPluginDescriptorV1, version_string), 288);
    assert_eq!(offset_of!(DauxPluginDescriptorV1, description), 352);
    assert_eq!(offset_of!(DauxPluginDescriptorV1, url), 608);
    assert_eq!(offset_of!(DauxPluginDescriptorV1, support_url), 864);
    assert_eq!(offset_of!(DauxPluginDescriptorV1, copyright), 1120);
    assert_eq!(offset_of!(DauxPluginDescriptorV1, license), 1376);
    assert_eq!(offset_of!(DauxPluginDescriptorV1, category), 1440);
    assert_eq!(offset_of!(DauxPluginDescriptorV1, sample_formats), 1444);
    assert_eq!(offset_of!(DauxPluginDescriptorV1, capabilities), 1448);
    assert_eq!(
        offset_of!(DauxPluginDescriptorV1, state_schema_version),
        1456
    );
    assert_eq!(offset_of!(DauxPluginDescriptorV1, features), 1464);
}

#[test]
fn process_field_offsets_match_the_specification() {
    assert_eq!(offset_of!(DauxProcessV1, size), 0);
    assert_eq!(offset_of!(DauxProcessV1, frame_count), 4);
    assert_eq!(offset_of!(DauxProcessV1, steady_time), 8);
    assert_eq!(offset_of!(DauxProcessV1, transport), 16);
    assert_eq!(offset_of!(DauxProcessV1, audio_input_count), 24);
    assert_eq!(offset_of!(DauxProcessV1, audio_output_count), 28);
    assert_eq!(offset_of!(DauxProcessV1, audio_inputs), 32);
    assert_eq!(offset_of!(DauxProcessV1, audio_outputs), 40);
    assert_eq!(offset_of!(DauxProcessV1, in_events), 48);
    assert_eq!(offset_of!(DauxProcessV1, out_events), 56);
    assert_eq!(offset_of!(DauxProcessV1, reserved), 64);

    assert_eq!(offset_of!(DauxProcessConfigV1, sample_rate), 24);
    assert_eq!(offset_of!(DauxAudioBufferV1, data32), 8);
    assert_eq!(offset_of!(DauxAudioBufferV1, data64), 16);
    assert_eq!(offset_of!(DauxAudioBufferV1, constant_mask), 24);
}

#[test]
fn event_field_offsets_match_the_specification() {
    assert_eq!(offset_of!(DauxEventHeaderV1, kind), 8);
    assert_eq!(offset_of!(DauxEventHeaderV1, flags), 10);
    assert_eq!(offset_of!(DauxEventHeaderV1, port_index), 12);

    // The v1.0 note layout carries four bytes of implicit padding between `_pad0` and
    // `velocity`; that is what the specification's field list produces and hosts depend
    // on it, so it is pinned here rather than "fixed".
    assert_eq!(offset_of!(DauxEventNoteV1, note_id), 16);
    assert_eq!(offset_of!(DauxEventNoteV1, channel), 20);
    assert_eq!(offset_of!(DauxEventNoteV1, key), 22);
    assert_eq!(offset_of!(DauxEventNoteV1, _pad0), 24);
    assert_eq!(offset_of!(DauxEventNoteV1, velocity), 32);
    assert_eq!(offset_of!(DauxEventNoteV1, tuning), 40);

    assert_eq!(offset_of!(DauxEventNoteExpressionV1, value), 32);
    assert_eq!(offset_of!(DauxEventParamV1, value), 32);
    assert_eq!(offset_of!(DauxEventParamV1, cookie), 40);
    assert_eq!(offset_of!(DauxEventMidi1V1, data), 16);
    assert_eq!(offset_of!(DauxEventMidi2V1, words), 20);
    assert_eq!(offset_of!(DauxEventSysExV1, bytes), 24);
    assert_eq!(offset_of!(DauxEventTransportV1, transport), 16);
}

#[test]
fn transport_and_extension_offsets_match_the_specification() {
    assert_eq!(offset_of!(DauxTransportV1, song_pos_samples), 8);
    assert_eq!(offset_of!(DauxTransportV1, bar_number), 56);
    assert_eq!(offset_of!(DauxTransportV1, time_sig_numerator), 60);
    assert_eq!(offset_of!(DauxTransportV1, time_sig_denominator), 62);
    assert_eq!(offset_of!(DauxTransportV1, loop_start_beats), 64);
    assert_eq!(offset_of!(DauxTransportV1, reserved), 96);

    assert_eq!(offset_of!(DauxAudioPortInfoV1, name), 8);
    assert_eq!(offset_of!(DauxAudioPortInfoV1, channel_count), 72);

    assert_eq!(offset_of!(DauxParamInfoV1, name), 16);
    assert_eq!(offset_of!(DauxParamInfoV1, group), 80);
    assert_eq!(offset_of!(DauxParamInfoV1, unit), 144);
    assert_eq!(offset_of!(DauxParamInfoV1, min_value), 208);
    assert_eq!(offset_of!(DauxParamInfoV1, cookie), 232);

    assert_eq!(offset_of!(DauxHostApiV1, name), 16);
    assert_eq!(offset_of!(DauxHostApiV1, vendor), 80);
    assert_eq!(offset_of!(DauxHostApiV1, version), 144);
    assert_eq!(offset_of!(DauxHostApiV1, get_extension), 160);

    // Like the note event, the shared texture carries implicit padding before `fence`.
    assert_eq!(offset_of!(DauxSharedTextureV1, _pad0), 32);
    assert_eq!(offset_of!(DauxSharedTextureV1, fence), 40);
    assert_eq!(offset_of!(DauxSharedTextureV1, fence_kind), 48);
}

#[test]
fn min_sizes_equal_the_current_layout() {
    assert_eq!(
        DauxPluginEntryV1::MIN_SIZE_V1_0,
        size_of::<DauxPluginEntryV1>()
    );
    assert_eq!(DauxProcessV1::MIN_SIZE_V1_0, size_of::<DauxProcessV1>());
    assert_eq!(DauxTransportV1::MIN_SIZE_V1_0, size_of::<DauxTransportV1>());
    assert_eq!(
        size_of_v1_0::<DauxParamInfoV1>(),
        size_of::<DauxParamInfoV1>()
    );
    assert_eq!(DauxProcessV1::SIZE as usize, size_of::<DauxProcessV1>());
}

// ---------------------------------------------------------------------------------------
// Version negotiation
// ---------------------------------------------------------------------------------------

#[test]
fn magic_round_trips_through_its_ascii_spelling() {
    assert_eq!(DAUX_ABI_MAGIC.to_be_bytes(), *b"DAUXABI1");
    assert_eq!(u64::from_be_bytes(*b"DAUXABI1"), DAUX_ABI_MAGIC);
    assert_eq!(DAUX_ABI_MAGIC, 0x4441_5558_4142_4931);
}

#[test]
fn entry_header_validation_applies_every_rejection_rule() {
    let min = DauxPluginEntryV1::MIN_SIZE_V1_0;
    let good = DauxPluginEntryV1::SIZE;

    assert!(check_entry_header(DAUX_ABI_MAGIC, 1, good, min).is_ok());
    // Rule 2: wrong magic.
    assert_eq!(check_entry_header(0, 1, good, min), DAUX_ERR_ABI_MISMATCH);
    assert_eq!(
        check_entry_header(DAUX_ABI_MAGIC.swap_bytes(), 1, good, min),
        DAUX_ERR_ABI_MISMATCH
    );
    // Rule 3: wrong major version, in either direction.
    assert_eq!(
        check_entry_header(DAUX_ABI_MAGIC, 0, good, min),
        DAUX_ERR_ABI_MISMATCH
    );
    assert_eq!(
        check_entry_header(DAUX_ABI_MAGIC, 2, good, min),
        DAUX_ERR_ABI_MISMATCH
    );
    // Rule 4: truncated structure, including the boundary case.
    assert_eq!(
        check_entry_header(DAUX_ABI_MAGIC, 1, 0, min),
        DAUX_ERR_ABI_MISMATCH
    );
    assert_eq!(
        check_entry_header(DAUX_ABI_MAGIC, 1, good - 1, min),
        DAUX_ERR_ABI_MISMATCH
    );
    // A larger structure from a newer minor revision is accepted.
    assert!(check_entry_header(DAUX_ABI_MAGIC, 1, good + 64, min).is_ok());
}

#[test]
fn versions_order_lexicographically() {
    assert!(DauxVersion::new(1, 0, 0, 0) > DauxVersion::new(0, 99, 99, 99));
    assert!(DauxVersion::new(1, 2, 3, 4) > DauxVersion::new(1, 2, 3, 3));
    assert_eq!(DauxVersion::ZERO, DauxVersion::default());
}

// ---------------------------------------------------------------------------------------
// Size-based forward compatibility
// ---------------------------------------------------------------------------------------

#[test]
fn field_presence_uses_the_specified_inequality() {
    // "a field at offset O of width W is present iff size >= O + W".
    assert!(has_field(8, 0, 8));
    assert!(!has_field(7, 0, 8));
    assert!(has_field(8, 4, 4));
    assert!(!has_field(8, 4, 5));
    // A zero-width probe at the very end is present; one past the end is not.
    assert!(has_field(8, 8, 0));
    assert!(!has_field(8, 9, 0));
    // Empty structures and absurd offsets must not wrap into a false positive.
    assert!(has_field(0, 0, 0));
    assert!(!has_field(u32::MAX, usize::MAX, 1));
    assert!(!has_field(u32::MAX, 1, usize::MAX));
}

#[test]
fn field_present_reads_the_declared_size_not_the_compiled_one() {
    let mut process = DauxProcessV1::new();
    assert!(process.is_v1_0_compatible());
    assert!(process.field_present(offset_of!(DauxProcessV1, out_events), 8));

    // A producer built against a hypothetical older revision that stopped after
    // `audio_outputs`.
    process.size = 48;
    assert!(!process.is_v1_0_compatible());
    assert!(process.field_present(offset_of!(DauxProcessV1, audio_outputs), 8));
    assert!(!process.field_present(offset_of!(DauxProcessV1, in_events), 8));

    assert_eq!(AbiStruct::declared_size(&process), 48);
    assert!(!is_v1_0_compatible(&process));
}

#[test]
fn event_records_validate_against_the_header_size() {
    let mut note = DauxEventNoteV1::new();
    assert_eq!(note.header.size, DauxEventNoteV1::SIZE);
    assert!(note.is_v1_0_compatible());

    // A truncated record: `velocity` was never written.
    note.header.size = offset_of!(DauxEventNoteV1, velocity) as u32;
    assert!(!note.is_v1_0_compatible());
    assert!(!note.field_present(offset_of!(DauxEventNoteV1, velocity), 8));
    assert!(note.field_present(offset_of!(DauxEventNoteV1, note_id), 4));
}

// ---------------------------------------------------------------------------------------
// Constructors
// ---------------------------------------------------------------------------------------

#[test]
fn constructors_set_size_and_zero_everything_else() {
    let transport = DauxTransportV1::new();
    assert_eq!(transport.size, DauxTransportV1::SIZE);
    assert_eq!(transport.flags, 0);
    assert_eq!(transport.reserved, [0; 6]);
    assert!(raw_bytes(&transport)[4..].iter().all(|&b| b == 0));

    let config = DauxProcessConfigV1::new();
    assert_eq!(config.size, DauxProcessConfigV1::SIZE);
    assert!(raw_bytes(&config)[4..].iter().all(|&b| b == 0));

    let port = DauxAudioPortInfoV1::default();
    assert_eq!(port.size, DauxAudioPortInfoV1::SIZE);
    assert!(raw_bytes(&port)[4..].iter().all(|&b| b == 0));

    let param = DauxParamInfoV1::empty();
    assert_eq!(param.size, DauxParamInfoV1::SIZE);
    assert!(param.cookie.is_null());
    assert!(raw_bytes(&param)[4..].iter().all(|&b| b == 0));

    // The shared texture has implicit padding before `fence`; zeroing must cover it.
    let texture = DauxSharedTextureV1::new();
    assert!(raw_bytes(&texture)[4..].iter().all(|&b| b == 0));

    let stream = DauxStreamV1::new();
    assert!(stream.read.is_none() && stream.write.is_none());
    assert!(raw_bytes(&stream)[4..].iter().all(|&b| b == 0));
}

#[test]
fn descriptor_and_process_constructors_seed_their_special_fields() {
    let descriptor = DauxPluginDescriptorV1::new();
    assert_eq!(descriptor.size, DauxPluginDescriptorV1::SIZE);
    assert_eq!(descriptor.min_abi_version_major, DAUX_ABI_VERSION_MAJOR);
    assert_eq!(descriptor.min_abi_version_minor, DAUX_ABI_VERSION_MINOR);
    assert_eq!(descriptor._pad0, 0);
    assert_eq!(descriptor.reserved, [0; 8]);
    assert_eq!(descriptor.id.as_str(), "");
    assert_eq!(descriptor.capabilities, 0);
    // Everything from `_pad0` onwards is zero.
    assert!(raw_bytes(&descriptor)[12..].iter().all(|&b| b == 0));

    // `steady_time` means "unavailable" as -1, not 0, so a default block must say so.
    let process = DauxProcessV1::new();
    assert_eq!(process.steady_time, -1);
    assert!(process.transport.is_null());
    assert!(process.in_events.is_null());
    assert_eq!(process.reserved, [0; 6]);
}

#[test]
fn event_constructors_stamp_kind_and_size() {
    assert_eq!(DauxEventNoteV1::new().header.kind, DAUX_EVENT_NOTE_ON);
    assert_eq!(DauxEventNoteV1::new().header.size, DauxEventNoteV1::SIZE);
    assert_eq!(
        DauxEventNoteExpressionV1::new().header.kind,
        DAUX_EVENT_NOTE_EXPRESSION
    );
    assert_eq!(DauxEventParamV1::new().header.kind, DAUX_EVENT_PARAM_VALUE);
    assert_eq!(DauxEventMidi1V1::new().header.kind, DAUX_EVENT_MIDI1);
    assert_eq!(DauxEventMidi2V1::new().header.kind, DAUX_EVENT_MIDI2);
    assert_eq!(DauxEventSysExV1::new().header.kind, DAUX_EVENT_SYSEX);
    assert_eq!(
        DauxEventTransportV1::new().header.kind,
        DAUX_EVENT_TRANSPORT
    );
    assert_eq!(
        DauxEventTransportV1::new().header.size,
        DauxEventTransportV1::SIZE
    );
    // The embedded transport is zeroed, `size` included: the producer fills it in.
    assert_eq!(DauxEventTransportV1::new().transport.size, 0);

    let header = DauxEventHeaderV1::with(DAUX_EVENT_NOTE_OFF, 48, 7);
    assert_eq!(
        (header.kind, header.size, header.time),
        (DAUX_EVENT_NOTE_OFF, 48, 7)
    );
    assert!(!header.has_flags(DAUX_EVENT_FLAG_IS_LIVE));

    let live = DauxEventHeaderV1 {
        flags: DAUX_EVENT_FLAG_IS_LIVE | DAUX_EVENT_FLAG_DONT_RECORD,
        ..DauxEventHeaderV1::new()
    };
    assert!(live.has_flags(DAUX_EVENT_FLAG_IS_LIVE));
    assert!(live.has_flags(DAUX_EVENT_FLAG_IS_LIVE | DAUX_EVENT_FLAG_DONT_RECORD));
}

#[test]
fn handles_and_interfaces_default_to_null() {
    assert!(DauxFactoryHandle::null().is_null());
    assert!(DauxPluginHandle::default().is_null());
    assert!(DauxHostHandle::from_ptr(core::ptr::null_mut()).is_null());

    let factory = DauxFactoryV1::null();
    assert!(factory.is_null());
    // SAFETY: `api` is null, which is the one case `api()` handles without dereferencing.
    assert!(unsafe { factory.api() }.is_none());

    let mut api = DauxLatencyApiV1 {
        size: DauxLatencyApiV1::SIZE,
        _pad0: 0,
        get: latency_get,
        reserved: [0; 2],
    };
    let plugin = DauxPluginV1::new(DauxPluginHandle::null(), core::ptr::null());
    assert!(plugin.is_null());
    api.size = 0;
    assert!(!api.is_v1_0_compatible());
}

unsafe extern "C" fn latency_get(_: DauxPluginHandle) -> u32 {
    0
}

// ---------------------------------------------------------------------------------------
// Audio buffers
// ---------------------------------------------------------------------------------------

#[test]
fn constant_mask_is_read_bit_by_bit_and_never_overruns() {
    let mut buffer = DauxAudioBufferV1::new();
    assert!(!buffer.is_channel_constant(0));

    buffer.constant_mask = 0b1010;
    assert!(!buffer.is_channel_constant(0));
    assert!(buffer.is_channel_constant(1));
    assert!(!buffer.is_channel_constant(2));
    assert!(buffer.is_channel_constant(3));

    buffer.constant_mask = 1 << 63;
    assert!(buffer.is_channel_constant(63));
    // Channels the 64-bit mask cannot address are reported as non-constant.
    assert!(!buffer.is_channel_constant(64));
    assert!(!buffer.is_channel_constant(u32::MAX));

    buffer.constant_mask = u64::MAX;
    assert!(buffer.is_channel_constant(63));
    assert!(!buffer.is_channel_constant(64));
}

// ---------------------------------------------------------------------------------------
// Status codes
// ---------------------------------------------------------------------------------------

#[test]
fn status_codes_are_distinct_and_classified_correctly() {
    let all = [
        DAUX_OK,
        DAUX_ERR_UNKNOWN,
        DAUX_ERR_INVALID_ARG,
        DAUX_ERR_UNSUPPORTED,
        DAUX_ERR_OUT_OF_MEMORY,
        DAUX_ERR_INVALID_STATE,
        DAUX_ERR_WRONG_THREAD,
        DAUX_ERR_NOT_REALTIME,
        DAUX_ERR_ABI_MISMATCH,
        DAUX_ERR_VERSION,
        DAUX_ERR_NOT_FOUND,
        DAUX_ERR_IO,
        DAUX_ERR_GRAPHICS,
        DAUX_ERR_HOST,
        DAUX_ERR_PLUGIN,
        DAUX_ERR_PANIC,
        DAUX_ERR_INTERNAL,
    ];
    for (i, status) in all.iter().enumerate() {
        assert_eq!(status.as_i32(), -(i as i32));
        assert_eq!(status.is_ok(), i == 0);
        assert_eq!(status.is_err(), i != 0);
        assert_ne!(status.name(), "DAUX_ERR_UNKNOWN_CODE");
        for other in &all[i + 1..] {
            assert_ne!(status, other);
            assert_ne!(status.name(), other.name());
        }
    }
}

#[test]
fn status_conversions_round_trip() {
    assert_eq!(DauxStatus::from(-4), DAUX_ERR_OUT_OF_MEMORY);
    assert_eq!(i32::from(DAUX_ERR_OUT_OF_MEMORY), -4);
    assert_eq!(DauxStatus::from_raw(0), DAUX_OK);
    assert_eq!(DAUX_OK.into_result(), Ok(()));
    assert_eq!(DAUX_ERR_IO.into_result(), Err(DAUX_ERR_IO));
    // Codes outside the documented set stay usable rather than panicking.
    assert_eq!(DauxStatus(-9999).name(), "DAUX_ERR_UNKNOWN_CODE");
    assert!(DauxStatus(-9999).is_err());
    assert!(!DauxStatus(1).is_ok());
    assert!(!DauxStatus(1).is_err());
}

#[test]
fn daux_bool_follows_the_producer_consumer_rule() {
    assert_eq!(daux_bool(true), DAUX_TRUE);
    assert_eq!(daux_bool(false), DAUX_FALSE);
    assert!(daux_bool_is_true(DAUX_TRUE));
    assert!(!daux_bool_is_true(DAUX_FALSE));
    // Consumers must treat *any* non-zero value as true.
    assert!(daux_bool_is_true(2));
    assert!(daux_bool_is_true(u32::MAX));
}

// ---------------------------------------------------------------------------------------
// Fixed text buffers
// ---------------------------------------------------------------------------------------

#[test]
fn fixed_buffers_start_empty() {
    let name = DauxName::empty();
    assert_eq!(name.len(), 0);
    assert!(name.is_empty());
    assert!(!name.is_full());
    assert_eq!(name.as_str(), "");
    assert_eq!(name.as_bytes(), b"");
    assert_eq!(DauxText::default().as_str(), "");
    assert_eq!(DauxId::default().as_str(), "");
    assert_eq!(DauxPath::default().as_str(), "");
}

#[test]
fn fixed_buffers_round_trip_ascii_and_unicode() {
    let name = DauxName::new("Gain");
    assert_eq!(name.as_str(), "Gain");
    assert_eq!(name.len(), 4);
    assert!(!name.is_empty());
    assert_eq!(name.as_bytes(), b"Gain");
    // Everything past the value is NUL padding.
    assert!(name.as_raw()[4..].iter().all(|&b| b == 0));

    let text = DauxText::new("Größe – 音量 🎛");
    assert_eq!(text.as_str(), "Größe – 音量 🎛");

    let id = DauxId::new("studio.futureboard.equzx");
    assert_eq!(id.as_str(), "studio.futureboard.equzx");
}

#[test]
fn fixed_buffers_truncate_on_a_char_boundary() {
    // 63 ASCII bytes plus a two-byte character does not fit: the character is dropped
    // whole rather than split.
    let mut s = "a".repeat(63);
    s.push('é');
    assert_eq!(s.len(), 65);
    let name = DauxName::new(&s);
    assert_eq!(name.len(), 63);
    assert_eq!(name.as_str(), "a".repeat(63));

    // 62 ASCII bytes plus the same character fills the buffer exactly.
    let mut s = "a".repeat(62);
    s.push('é');
    assert_eq!(s.len(), 64);
    let name = DauxName::new(&s);
    assert_eq!(name.len(), 64);
    assert!(name.is_full());
    assert_eq!(name.as_str(), s);

    // A four-byte character straddling the end is dropped whole.
    let mut s = "a".repeat(61);
    s.push('🎛');
    assert_eq!(s.len(), 65);
    let name = DauxName::new(&s);
    assert_eq!(name.len(), 61);

    // A value made only of oversized characters truncates to a shorter valid prefix.
    let name = DauxName::new(&"🎛".repeat(20));
    assert_eq!(name.len(), 64);
    assert_eq!(name.as_str().chars().count(), 16);
}

#[test]
fn fixed_buffers_stop_at_the_first_nul() {
    let mut name = DauxName::empty();
    name.0[..5].copy_from_slice(b"ab\0cd");
    assert_eq!(name.len(), 2);
    assert_eq!(name.as_str(), "ab");
    assert_eq!(name.as_bytes(), b"ab");

    // A completely full buffer has no terminator at all.
    let full = DauxName([b'x'; DAUX_NAME_SIZE]);
    assert_eq!(full.len(), DAUX_NAME_SIZE);
    assert!(full.is_full());
    assert_eq!(full.as_str().len(), DAUX_NAME_SIZE);
}

#[test]
fn fixed_buffers_tolerate_invalid_utf8_without_panicking() {
    // Entirely invalid.
    let name = DauxName([0xFF; DAUX_NAME_SIZE]);
    assert_eq!(name.as_str(), "");
    assert_eq!(name.len(), DAUX_NAME_SIZE);

    // Valid prefix, then a bad byte: the longest valid prefix survives.
    let mut name = DauxName::empty();
    name.0[..4].copy_from_slice(b"ok\xFF!");
    assert_eq!(name.as_str(), "ok");

    // A multi-byte character truncated mid-codepoint by a hostile writer.
    let mut name = DauxName::empty();
    name.0[..3].copy_from_slice(&[b'a', 0xE9, 0x9F]);
    assert_eq!(name.as_str(), "a");
}

#[test]
fn fixed_buffers_can_be_rewritten_and_cleared() {
    let mut name = DauxName::new("a long-ish previous value");
    name.set("short");
    assert_eq!(name.as_str(), "short");
    // The tail of the old value must not survive the overwrite.
    assert!(name.as_raw()[5..].iter().all(|&b| b == 0));

    name.set("");
    assert!(name.is_empty());

    let mut path = DauxPath::new("C:/plugins/Gain.axt");
    assert_eq!(path.as_str(), "C:/plugins/Gain.axt");
    path.clear();
    assert!(path.is_empty());
    assert_eq!(path.as_str(), "");
}

#[test]
fn fixed_buffer_debug_shows_the_text_not_the_padding() {
    // `Debug` output is bounded by the value, not by the 1 KiB capacity.
    let rendered = format!("{:?}", DauxPath::new("/tmp/x"));
    assert_eq!(rendered, "DauxPath(\"/tmp/x\")");
}

// ---------------------------------------------------------------------------------------
// Borrowed string views
// ---------------------------------------------------------------------------------------

#[test]
fn str_view_borrows_utf8() {
    let view = DauxStrView::from_str("daux.params/1");
    assert_eq!(view.len(), 13);
    assert!(!view.is_empty());
    // SAFETY: the view was built from a `'static` string literal that outlives the borrow.
    assert_eq!(unsafe { view.as_str() }, Some("daux.params/1"));
}

#[test]
fn empty_str_view_is_null_but_readable() {
    let view = DauxStrView::empty();
    assert!(view.is_empty());
    assert!(view.ptr.is_null());
    // SAFETY: `len == 0`, so no memory is read regardless of the null pointer.
    assert_eq!(unsafe { view.as_str() }, Some(""));
    // SAFETY: as above.
    assert_eq!(unsafe { view.as_bytes() }, Some(&[][..]));

    // A zero-length view over real memory is equally fine.
    let view = DauxStrView::from_str("");
    assert!(view.is_empty());
    // SAFETY: `len == 0`; the pointer is a valid dangling-but-aligned string pointer.
    assert_eq!(unsafe { view.as_str() }, Some(""));
}

#[test]
fn malformed_str_views_are_rejected_instead_of_dereferenced() {
    // Null pointer with a non-zero length: a producer bug, not a crash.
    let view = DauxStrView {
        ptr: core::ptr::null(),
        len: 7,
    };
    // SAFETY: the function checks for null before reading anything.
    assert_eq!(unsafe { view.as_str() }, None);
    // SAFETY: as above.
    assert_eq!(unsafe { view.as_bytes() }, None);

    // Valid memory holding invalid UTF-8.
    let bytes: [u8; 3] = [b'a', 0xFF, b'b'];
    let view = DauxStrView {
        ptr: bytes.as_ptr(),
        len: bytes.len(),
    };
    // SAFETY: `bytes` outlives the borrow and is neither moved nor mutated meanwhile.
    assert_eq!(unsafe { view.as_str() }, None);
    // SAFETY: as above.
    assert_eq!(unsafe { view.as_bytes() }, Some(&bytes[..]));
}

#[test]
fn str_views_are_const_constructible() {
    const VIEW: DauxStrView = DauxStrView::from_str(ext::PARAMS);
    assert_eq!(VIEW.len(), ext::PARAMS.len());
    // SAFETY: the view borrows a `'static` string constant.
    assert_eq!(unsafe { VIEW.as_str() }, Some(ext::PARAMS));
}

// ---------------------------------------------------------------------------------------
// Extensions
// ---------------------------------------------------------------------------------------

#[test]
fn extension_ids_are_unique_nul_free_and_correctly_prefixed() {
    assert_eq!(ext::ALL_IDS.len(), 16);
    for (i, id) in ext::ALL_IDS.iter().enumerate() {
        assert!(!id.is_empty());
        assert!(!id.contains('\0'), "{id} must be NUL-free");
        assert!(id.is_ascii());
        // Ids embed their version: a new version is a new id.
        assert!(id.contains('/'), "{id} must carry a version suffix");
        assert!(
            id.starts_with("daux.") || id.starts_with("com."),
            "{id} must use a reverse-DNS prefix"
        );
        for other in &ext::ALL_IDS[i + 1..] {
            assert_ne!(id, other);
        }
    }
    assert_eq!(ext::AUDIO_PORTS, "daux.audio-ports/1");
    assert_eq!(ext::HOST_TIMER, "daux.host.timer/1");
    assert_eq!(ext::SHARED_TEXTURE, "com.futureboard.daux.shared-texture/1");
}

#[test]
fn extension_id_matching_is_exact() {
    let view = DauxStrView::from_str(ext::GUI);
    // SAFETY: the view borrows a `'static` string constant for the call's duration.
    assert!(unsafe { ext::id_matches(view, ext::GUI) });
    // SAFETY: as above.
    assert!(!unsafe { ext::id_matches(view, ext::PARAMS) });

    // A prefix must not match: `daux.gui/1` is not `daux.gui/10`.
    let view = DauxStrView::from_str("daux.gui/10");
    // SAFETY: as above.
    assert!(!unsafe { ext::id_matches(view, ext::GUI) });

    // A malformed view matches nothing rather than faulting.
    let view = DauxStrView {
        ptr: core::ptr::null(),
        len: 4,
    };
    // SAFETY: the implementation checks for null before reading.
    assert!(!unsafe { ext::id_matches(view, ext::GUI) });

    let view = DauxStrView::empty();
    // SAFETY: `len == 0`, nothing is read.
    assert!(!unsafe { ext::id_matches(view, ext::GUI) });
}

#[test]
fn extension_table_casts_reject_null() {
    let table = DauxLatencyApiV1 {
        size: DauxLatencyApiV1::SIZE,
        _pad0: 0,
        get: latency_get,
        reserved: [0; 2],
    };
    let ptr: *const core::ffi::c_void = (&raw const table).cast();
    // SAFETY: `ptr` points at `table`, which is alive for the whole test and is exactly
    // the type named here.
    let borrowed = unsafe { extension_table::<DauxLatencyApiV1>(ptr) };
    assert_eq!(borrowed.map(|t| t.size), Some(DauxLatencyApiV1::SIZE));

    // SAFETY: a null pointer is handled without dereferencing.
    let none = unsafe { extension_table::<DauxLatencyApiV1>(core::ptr::null()) };
    assert!(none.is_none());
}

// ---------------------------------------------------------------------------------------
// Constant sets
// ---------------------------------------------------------------------------------------

#[test]
fn capability_bits_are_distinct_single_bits() {
    let caps = [
        DAUX_CAP_AUDIO_EFFECT,
        DAUX_CAP_INSTRUMENT,
        DAUX_CAP_MIDI_EFFECT,
        DAUX_CAP_ANALYZER,
        DAUX_CAP_MIDI_INPUT,
        DAUX_CAP_MIDI_OUTPUT,
        DAUX_CAP_MIDI2,
        DAUX_CAP_SIDECHAIN,
        DAUX_CAP_DYNAMIC_BUSES,
        DAUX_CAP_SAMPLE_ACCURATE_AUTO,
        DAUX_CAP_NOTE_EXPRESSION,
        DAUX_CAP_HAS_GUI,
        DAUX_CAP_REQUIRES_GUI,
        DAUX_CAP_SHARED_TEXTURE_GUI,
        DAUX_CAP_OFFLINE_RENDER,
        DAUX_CAP_HARD_REALTIME,
        DAUX_CAP_SANDBOX_SAFE,
        DAUX_CAP_STEREO_ONLY,
        DAUX_CAP_LATENCY_DYNAMIC,
        DAUX_CAP_TAIL_INFINITE,
    ];
    let mut seen = 0u64;
    for (i, cap) in caps.iter().enumerate() {
        assert_eq!(cap.count_ones(), 1);
        assert_eq!(*cap, 1 << i);
        assert_eq!(seen & cap, 0);
        seen |= cap;
    }
    assert_eq!(seen.count_ones(), 20);
}

#[test]
fn event_kinds_and_flags_match_the_specification() {
    assert_eq!(DAUX_EVENT_NOTE_ON, 1);
    assert_eq!(DAUX_EVENT_SYSEX, 13);
    assert_eq!(DAUX_EVENT_CUSTOM, 0x7000);
    const { assert!(DAUX_EVENT_SYSEX < DAUX_EVENT_CUSTOM) };
    assert_eq!(DAUX_EVENT_FLAG_IS_LIVE, 1);
    assert_eq!(DAUX_EVENT_FLAG_DONT_RECORD, 2);

    assert_eq!(DAUX_NOTE_EXPR_VOLUME, 0);
    assert_eq!(DAUX_NOTE_EXPR_PRESSURE, 6);
}

#[test]
fn process_and_transport_constants_match_the_specification() {
    assert_eq!(DAUX_PROCESS_ERROR, 0);
    assert_eq!(DAUX_PROCESS_CONTINUE, 1);
    assert_eq!(DAUX_PROCESS_CONTINUE_IF_LOUD, 2);
    assert_eq!(DAUX_PROCESS_TAIL, 3);
    assert_eq!(DAUX_PROCESS_SLEEP, 4);

    assert_eq!(DAUX_PROCESS_MODE_REALTIME, 0);
    assert_eq!(DAUX_PROCESS_MODE_ANALYSIS, 3);

    assert_eq!(DAUX_TRANSPORT_HAS_TEMPO, 1);
    assert_eq!(DAUX_TRANSPORT_IS_PREROLL, 1 << 9);
    assert_eq!(DAUX_TAIL_INFINITE, u32::MAX);
    // Adjacent on purpose (`abi-v1` §11.5): both sentinels sit at the top of the range so
    // that every value a plug-in could plausibly mean as a real tail stays a real tail.
    assert_eq!(DAUX_TAIL_UNKNOWN, u32::MAX - 1);
    assert_ne!(DAUX_TAIL_UNKNOWN, DAUX_TAIL_INFINITE);
    assert_eq!(DAUX_SAMPLE_FORMAT_F32 | DAUX_SAMPLE_FORMAT_F64, 0b11);
}

#[test]
fn transport_flags_gate_field_access() {
    let mut transport = DauxTransportV1::new();
    assert!(!transport.has_flags(DAUX_TRANSPORT_HAS_TEMPO));
    assert!(!transport.has_flags(DAUX_TRANSPORT_IS_PLAYING));

    transport.flags = DAUX_TRANSPORT_HAS_TEMPO | DAUX_TRANSPORT_IS_PLAYING;
    transport.tempo = 128.0;
    assert!(transport.has_flags(DAUX_TRANSPORT_HAS_TEMPO));
    assert!(transport.has_flags(DAUX_TRANSPORT_HAS_TEMPO | DAUX_TRANSPORT_IS_PLAYING));
    // A composite probe fails unless *every* bit is present.
    assert!(!transport.has_flags(DAUX_TRANSPORT_HAS_TEMPO | DAUX_TRANSPORT_HAS_BEATS));
    // Probing for nothing is vacuously true.
    assert!(transport.has_flags(0));
}

#[test]
fn entry_symbol_names_agree() {
    assert_eq!(DAUX_ENTRY_SYMBOL, "daux_plugin_entry_v1");
    assert_eq!(DAUX_ENTRY_SYMBOL_CSTR.last(), Some(&0));
    assert_eq!(
        &DAUX_ENTRY_SYMBOL_CSTR[..DAUX_ENTRY_SYMBOL.len()],
        DAUX_ENTRY_SYMBOL.as_bytes()
    );
    assert_eq!(DAUX_ENTRY_SYMBOL_CSTR.len(), DAUX_ENTRY_SYMBOL.len() + 1);
    assert_eq!(size_of::<DauxPluginEntryFn>(), size_of::<usize>());
}
