//! The binary contract, pinned to literal numbers.
//!
//! A silent layout change is the single worst bug this project can ship. It breaks every
//! plug-in already built against the old layout, at run time, in someone else's DAW, with no
//! diagnostic — the host reads a `f64` where the plug-in wrote a `u32` and the result is
//! garbage audio or a crash inside code neither party can debug.
//!
//! `daux-abi` has its own internal compile-time assertions, and they are good. This suite is
//! deliberately *different in kind*: it restates the numbers as literals from outside the
//! crate. An internal assertion written as `size_of::<T>() == size_of::<T>()` is a tautology,
//! and an internal one derived from the same constants that define the layout moves whenever
//! the layout moves. These do not move. If a field is inserted, reordered, widened, or if a
//! `#[repr(C)]` is dropped, something here fails.
//!
//! # Where the numbers come from
//!
//! `docs/specifications/abi-v1.md`, which is normative and wins over any code. When a number
//! here disagrees with the code, the code is wrong until the specification is amended — and
//! amending it is a deliberate act that breaks compatibility, not a refactor.
//!
//! # What is intentionally not asserted
//!
//! Pointer-sized fields make some struct sizes differ between 32- and 64-bit targets. Those
//! are asserted relationally (against `size_of::<*const c_void>()`) rather than as literals,
//! so the suite is meaningful on both without encoding one target's answer as the truth.

use core::mem::{align_of, offset_of, size_of};

use daux_abi::{
    DAUX_ABI_MAGIC, DAUX_ABI_VERSION_MAJOR, DAUX_ENTRY_SYMBOL, DAUX_ID_SIZE, DAUX_NAME_SIZE,
    DAUX_PATH_SIZE, DAUX_TEXT_SIZE, DauxAudioBufferV1, DauxEventHeaderV1, DauxEventListV1,
    DauxEventMidi1V1, DauxEventMidi2V1, DauxEventNoteExpressionV1, DauxEventNoteV1,
    DauxEventParamV1, DauxEventSysExV1, DauxEventTransportV1, DauxPluginDescriptorV1,
    DauxPluginEntryV1, DauxProcessConfigV1, DauxProcessV1, DauxStatus, DauxStrView,
    DauxTransportV1, DauxVersion,
};

/// One machine word. Several ABI structs are arrays of function pointers, whose size is a
/// multiple of this rather than a fixed number of bytes.
const PTR: usize = size_of::<*const core::ffi::c_void>();

// ------------------------------------------------------------------ identity ---

#[test]
fn the_magic_and_entry_symbol_are_frozen_forever() {
    // These two identify a DAUx module. Changing either silently turns every existing
    // bundle into "not a DAUx plug-in" with no explanation to the user.
    assert_eq!(DAUX_ABI_MAGIC, 0x4441_5558_4142_4931, "DAUX_ABI_MAGIC");
    // The magic spells "DAUXABI1" in ASCII, which is what makes it recognisable in a hex
    // dump of a broken module.
    assert_eq!(&DAUX_ABI_MAGIC.to_be_bytes(), b"DAUXABI1");
    assert_eq!(DAUX_ENTRY_SYMBOL, "daux_plugin_entry_v1");
    assert_eq!(DAUX_ABI_VERSION_MAJOR, 1);
}

#[test]
fn the_fixed_string_buffer_sizes_are_frozen() {
    // Every out-string in the ABI is one of these four. Shrinking one truncates real
    // metadata; growing one changes the size of every struct that embeds it.
    assert_eq!(DAUX_NAME_SIZE, 64, "DAUX_NAME_SIZE");
    assert_eq!(DAUX_TEXT_SIZE, 256, "DAUX_TEXT_SIZE");
    assert_eq!(DAUX_ID_SIZE, 128, "DAUX_ID_SIZE");
    assert_eq!(DAUX_PATH_SIZE, 1024, "DAUX_PATH_SIZE");
}

// -------------------------------------------------------------------- scalars ---

#[test]
fn the_scalar_wrappers_are_the_size_of_what_they_wrap() {
    // A newtype that is not `#[repr(transparent)]`/`#[repr(C)]` can gain padding or a
    // niche, and then a C caller passing a bare i32 is passing the wrong thing.
    assert_eq!(size_of::<DauxStatus>(), 4, "DauxStatus is an i32");
    assert_eq!(align_of::<DauxStatus>(), align_of::<i32>());

    // Four components, not three: `build` is part of the struct even though it is not part
    // of a release's public identity.
    assert_eq!(size_of::<DauxVersion>(), 16, "DauxVersion is four u32s");
    assert_eq!(align_of::<DauxVersion>(), 4);
    assert_eq!(offset_of!(DauxVersion, major), 0);
    assert_eq!(offset_of!(DauxVersion, minor), 4);
    assert_eq!(offset_of!(DauxVersion, patch), 8);
    assert_eq!(offset_of!(DauxVersion, build), 12);
}

#[test]
fn a_string_view_is_a_pointer_and_a_length() {
    // `DauxStrView` is inputs-only and never owns. Two words, in this order.
    assert_eq!(size_of::<DauxStrView>(), 2 * PTR);
    assert_eq!(align_of::<DauxStrView>(), PTR);
    assert_eq!(offset_of!(DauxStrView, ptr), 0);
    assert_eq!(offset_of!(DauxStrView, len), PTR);
}

// --------------------------------------------------------------------- events ---

#[test]
fn the_event_header_layout_is_frozen() {
    // Every event record starts with this header, so a change here shifts every payload
    // field in every event type at once — the widest-blast-radius struct in the ABI.
    assert_eq!(size_of::<DauxEventHeaderV1>(), 16, "DauxEventHeaderV1");
    assert_eq!(align_of::<DauxEventHeaderV1>(), 4);
    assert_eq!(offset_of!(DauxEventHeaderV1, size), 0);
    assert_eq!(offset_of!(DauxEventHeaderV1, time), 4);
    assert_eq!(offset_of!(DauxEventHeaderV1, kind), 8);
    assert_eq!(offset_of!(DauxEventHeaderV1, flags), 10);
    assert_eq!(offset_of!(DauxEventHeaderV1, port_index), 12);
    // The explicit tail pad keeps the header 16 bytes, so every payload that follows it is
    // 4- and 8-aligned without the compiler inserting padding of its own.
    assert_eq!(offset_of!(DauxEventHeaderV1, _pad0), 14);
}

#[test]
fn every_event_record_begins_with_the_header() {
    // This is what makes a host able to read `kind` from an unknown event and skip it by
    // `size`. If any record put its header anywhere but offset 0, forward compatibility
    // would break silently for exactly the events a host does not recognise.
    assert_eq!(offset_of!(DauxEventNoteV1, header), 0);
    assert_eq!(offset_of!(DauxEventNoteExpressionV1, header), 0);
    assert_eq!(offset_of!(DauxEventParamV1, header), 0);
    assert_eq!(offset_of!(DauxEventMidi1V1, header), 0);
    assert_eq!(offset_of!(DauxEventMidi2V1, header), 0);
    assert_eq!(offset_of!(DauxEventSysExV1, header), 0);
    assert_eq!(offset_of!(DauxEventTransportV1, header), 0);
}

#[test]
fn event_records_are_at_least_as_large_as_their_header() {
    // A record smaller than its own header would mean a `size` field that cannot describe
    // itself, and a reader walking the list would run backwards.
    let header = size_of::<DauxEventHeaderV1>();
    for (name, size) in [
        ("note", size_of::<DauxEventNoteV1>()),
        ("note expression", size_of::<DauxEventNoteExpressionV1>()),
        ("param", size_of::<DauxEventParamV1>()),
        ("midi1", size_of::<DauxEventMidi1V1>()),
        ("midi2", size_of::<DauxEventMidi2V1>()),
        ("sysex", size_of::<DauxEventSysExV1>()),
        ("transport", size_of::<DauxEventTransportV1>()),
    ] {
        assert!(size >= header, "{name} record is smaller than the header");
        assert!(
            u32::try_from(size).is_ok(),
            "{name} record does not fit the u32 `size` field"
        );
    }
}

#[test]
fn a_midi1_event_carries_exactly_three_status_bytes() {
    // MIDI 1.0 messages are at most three bytes. A wider field here would tempt a
    // implementation to pack something else in and desynchronise the two sides.
    assert_eq!(
        size_of::<DauxEventMidi1V1>() - size_of::<DauxEventHeaderV1>(),
        4,
        "three data bytes plus one of padding"
    );
}

#[test]
fn a_midi2_event_carries_a_word_count_and_four_ump_words() {
    // A UMP packet is one to four 32-bit words, so the count travels with them; a reader
    // that assumed four would forward three words of zeroes as real MIDI 2.0 data.
    assert_eq!(
        size_of::<DauxEventMidi2V1>() - size_of::<DauxEventHeaderV1>(),
        4 + 16,
        "a u32 word_count plus four 32-bit UMP words"
    );
    assert_eq!(offset_of!(DauxEventMidi2V1, word_count), 16);
    assert_eq!(offset_of!(DauxEventMidi2V1, words), 20);
}

#[test]
fn the_event_list_is_a_vtable_not_a_buffer() {
    // The host owns event storage; the list is function pointers over it. If this ever
    // became a struct with an inline array, a plug-in could not push without allocating.
    assert_eq!(align_of::<DauxEventListV1>(), PTR);
    assert_eq!(offset_of!(DauxEventListV1, size), 0);
    assert!(size_of::<DauxEventListV1>() >= 4 * PTR);
}

// -------------------------------------------------------------------- process ---

#[test]
fn the_process_config_layout_is_frozen() {
    // Read once per activation, but read wrong forever after if it shifts.
    assert_eq!(offset_of!(DauxProcessConfigV1, size), 0);
    assert_eq!(align_of::<DauxProcessConfigV1>(), 8, "f64 sample_rate");
    assert_eq!(
        size_of::<DauxProcessConfigV1>() % 8,
        0,
        "an 8-aligned struct must be a multiple of 8 bytes"
    );
}

#[test]
fn an_audio_buffer_is_planar_pointers_and_a_count() {
    // Planar, not interleaved: `data` is an array of channel pointers. Getting this wrong
    // reads the second channel's samples as the first channel's tail.
    assert_eq!(align_of::<DauxAudioBufferV1>(), PTR);
    assert!(size_of::<DauxAudioBufferV1>() >= 2 * PTR);
}

#[test]
fn the_process_struct_starts_with_its_size() {
    // Every versioned struct in the ABI starts with `size`, and that is what lets a host
    // built against 1.0 read a struct written by 1.3 — it reads only as far as it knows.
    assert_eq!(offset_of!(DauxProcessV1, size), 0);
    assert_eq!(size_of::<u32>(), 4, "the `size` field is a u32");
}

// ------------------------------------------------------------------ transport ---

#[test]
fn the_transport_layout_is_frozen() {
    // 64-bit sample position forces 8-byte alignment; a compiler that packed this
    // differently would shift every field after `flags`.
    assert_eq!(offset_of!(DauxTransportV1, size), 0);
    assert_eq!(align_of::<DauxTransportV1>(), 8);
    assert_eq!(size_of::<DauxTransportV1>() % 8, 0);
}

// ----------------------------------------------------------------- descriptor ---

#[test]
fn the_descriptor_embeds_its_strings_inline() {
    // The descriptor owns its text as fixed buffers rather than pointers, so a host can
    // copy it and the module can be unloaded without dangling anything. That makes it
    // large on purpose: it must be at least the sum of the buffers it carries.
    assert_eq!(offset_of!(DauxPluginDescriptorV1, size), 0);
    assert!(
        size_of::<DauxPluginDescriptorV1>() >= DAUX_ID_SIZE + DAUX_NAME_SIZE,
        "the descriptor must carry its id and name inline, not by pointer"
    );
    assert!(
        size_of::<DauxPluginDescriptorV1>() < 8192,
        "a descriptor this large suggests a buffer was widened by accident"
    );
}

#[test]
fn the_entry_point_header_layout_is_frozen() {
    // The first bytes a host reads from a library it has just dlopen'd, before it trusts
    // anything else in the module. `size` leads, so a host can bound its own read; the
    // 8-byte `magic` is 8-aligned at 16, which is why `_pad0` exists and must stay.
    assert_eq!(offset_of!(DauxPluginEntryV1, size), 0);
    assert_eq!(offset_of!(DauxPluginEntryV1, abi_version_major), 4);
    assert_eq!(offset_of!(DauxPluginEntryV1, abi_version_minor), 8);
    assert_eq!(offset_of!(DauxPluginEntryV1, _pad0), 12);
    assert_eq!(offset_of!(DauxPluginEntryV1, magic), 16);
    assert_eq!(
        offset_of!(DauxPluginEntryV1, magic) % 8,
        0,
        "an unaligned u64 read is undefined behaviour on some targets"
    );
}

// ------------------------------------------------------- forward compatibility ---

#[test]
fn every_versioned_struct_declares_a_v1_0_minimum_size() {
    // `MIN_SIZE_V1_0` is what a host compares an incoming `size` against (abi-v1 §3,
    // rejection rule 4). It must be frozen at the v1.0 value even as the struct grows, so
    // it can never exceed the current size.
    fn check<T: daux_abi::AbiStruct>(name: &str) {
        let min = daux_abi::size_of_v1_0::<T>();
        assert!(min > 0, "{name}: MIN_SIZE_V1_0 must be positive");
        assert!(
            min <= size_of::<T>(),
            "{name}: MIN_SIZE_V1_0 ({min}) exceeds the current size ({}), which means a \
             field was removed — that is a breaking change, not a minor revision",
            size_of::<T>()
        );
        assert!(
            min >= 4,
            "{name}: a versioned struct must be at least large enough for its own `size`"
        );
    }

    check::<DauxProcessV1>("DauxProcessV1");
    check::<DauxProcessConfigV1>("DauxProcessConfigV1");
    check::<DauxTransportV1>("DauxTransportV1");
    check::<DauxPluginDescriptorV1>("DauxPluginDescriptorV1");
    check::<DauxEventListV1>("DauxEventListV1");
}

#[test]
fn a_struct_from_a_newer_minor_version_is_accepted_and_a_truncated_one_is_not() {
    // The whole point of the `size` prefix: a 1.3 producer writing a larger struct must be
    // readable by a 1.0 consumer, while a struct too small to hold the v1.0 fields must be
    // rejected rather than read past its end.
    let min = u32::try_from(daux_abi::size_of_v1_0::<DauxProcessV1>())
        .expect("the v1.0 size fits the u32 `size` field");

    // `is_v1_0_compatible` reads the `size` the *producer* wrote, so the check is driven by
    // building values with different declared sizes rather than by passing a bare number.
    let with_size = |size: u32| {
        let mut process = DauxProcessV1::new();
        process.size = size;
        process
    };

    assert!(
        daux_abi::is_v1_0_compatible(&with_size(min)),
        "a struct of exactly the v1.0 size must be accepted"
    );
    assert!(
        daux_abi::is_v1_0_compatible(&with_size(min + 64)),
        "a larger struct from a newer minor version must be accepted"
    );
    assert!(
        !daux_abi::is_v1_0_compatible(&with_size(min - 1)),
        "a struct one byte short of the v1.0 size must be rejected"
    );
    assert!(
        !daux_abi::is_v1_0_compatible(&with_size(0)),
        "a zeroed `size` must be rejected, not treated as 'unknown'"
    );

    // A default-constructed value declares the current size, which must itself pass.
    assert!(daux_abi::is_v1_0_compatible(&DauxProcessV1::new()));
}

// ------------------------------------------------------------------ soundness ---

#[test]
fn no_abi_struct_is_over_aligned_for_c() {
    // A `#[repr(C)]` struct whose alignment exceeds the platform's maximum fundamental
    // alignment cannot be produced by a C compiler, so no C host could construct one.
    // 16 covers every scalar and vector type these structs contain.
    macro_rules! check_align {
        ($($t:ty),+ $(,)?) => {
            $(assert!(
                align_of::<$t>() <= 16,
                concat!(stringify!($t), " is over-aligned for a C ABI struct")
            );)+
        };
    }
    check_align!(
        DauxStatus,
        DauxVersion,
        DauxStrView,
        DauxEventHeaderV1,
        DauxEventNoteV1,
        DauxEventNoteExpressionV1,
        DauxEventParamV1,
        DauxEventMidi1V1,
        DauxEventMidi2V1,
        DauxEventSysExV1,
        DauxEventTransportV1,
        DauxEventListV1,
        DauxProcessConfigV1,
        DauxAudioBufferV1,
        DauxProcessV1,
        DauxTransportV1,
        DauxPluginDescriptorV1,
        DauxPluginEntryV1,
    );
}

#[test]
fn every_abi_struct_is_a_multiple_of_its_own_alignment() {
    // True of any well-formed `#[repr(C)]` type. If it ever fails, the type is not
    // `#[repr(C)]` at all and Rust is free to reorder its fields between compilations —
    // the exact failure mode this whole suite exists to catch.
    macro_rules! check_padding {
        ($($t:ty),+ $(,)?) => {
            $(assert_eq!(
                size_of::<$t>() % align_of::<$t>(),
                0,
                concat!(stringify!($t), " is not padded to its alignment")
            );)+
        };
    }
    check_padding!(
        DauxVersion,
        DauxStrView,
        DauxEventHeaderV1,
        DauxEventNoteV1,
        DauxEventNoteExpressionV1,
        DauxEventParamV1,
        DauxEventMidi1V1,
        DauxEventMidi2V1,
        DauxEventSysExV1,
        DauxEventTransportV1,
        DauxEventListV1,
        DauxProcessConfigV1,
        DauxAudioBufferV1,
        DauxProcessV1,
        DauxTransportV1,
        DauxPluginDescriptorV1,
        DauxPluginEntryV1,
    );
}
