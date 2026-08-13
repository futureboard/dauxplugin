//! Every number two crates had to agree on without being able to see each other.
//!
//! The zero-dependency rule (`CLAUDE.md` rule 4) means `daux-core`, `daux-state` and
//! `daux-parameter` cannot depend on `daux-abi`, and a proc-macro crate cannot depend on any
//! of them. So the same constants are transcribed in two or three places, from
//! `docs/specifications/abi-v1.md`, by hand.
//!
//! That is a deliberate trade — the alternative is a dependency edge that would drag the ABI
//! into every leaf crate — but it has a specific failure mode: a transcription that is
//! internally consistent and externally wrong. Such a mistake round-trips perfectly inside
//! its own crate and every unit test passes, while every value that crosses the ABI is
//! silently shifted. `Category::code` shipped with exactly that bug: `Effect` encoded as 0
//! where the spec says 1, so `from_code(DAUX_CATEGORY_INSTRUMENT)` returned `MidiEffect` and
//! every plug-in would have been filed under its neighbour's heading in every host browser.
//!
//! `tests/harness` is the only crate that can see both sides at once, so this is where the
//! transcriptions are checked against each other.

use daux_core::{Capabilities, Category, ProcessMode, ProcessStatus, Tail};

/// `abi-v1` §6.1. Both directions, all seven values, against the ABI's own constants.
#[test]
fn category_codes_agree_with_daux_abi() {
    let table = [
        (Category::Unknown, daux_abi::DAUX_CATEGORY_UNKNOWN),
        (Category::Effect, daux_abi::DAUX_CATEGORY_EFFECT),
        (Category::Instrument, daux_abi::DAUX_CATEGORY_INSTRUMENT),
        (Category::MidiEffect, daux_abi::DAUX_CATEGORY_MIDI_EFFECT),
        (Category::Analyzer, daux_abi::DAUX_CATEGORY_ANALYZER),
        (Category::Generator, daux_abi::DAUX_CATEGORY_GENERATOR),
        (Category::Utility, daux_abi::DAUX_CATEGORY_UTILITY),
    ];

    for (category, code) in table {
        assert_eq!(
            category.code(),
            code,
            "`Category::{category:?}` encodes as {} but the ABI constant is {code}",
            category.code()
        );
        assert_eq!(
            Category::from_code(code),
            category,
            "decoding {code} must give `Category::{category:?}`"
        );
    }

    // Every category is covered, so a new variant cannot slip through untested.
    assert_eq!(Category::ALL.len(), table.len());
    for category in Category::ALL {
        assert!(
            table.iter().any(|(known, _)| *known == category),
            "`Category::{category:?}` is not in this test's table"
        );
    }
}

/// `abi-v1` §11.5. Both sentinels, and the largest value that is *not* one.
#[test]
fn tail_sentinels_agree_with_daux_abi() {
    assert_eq!(Tail::INFINITE_SAMPLES, daux_abi::DAUX_TAIL_INFINITE);
    assert_eq!(Tail::UNKNOWN_SAMPLES, daux_abi::DAUX_TAIL_UNKNOWN);
    assert_eq!(Tail::Infinite.samples(), daux_abi::DAUX_TAIL_INFINITE);
    assert_eq!(Tail::Unknown.samples(), daux_abi::DAUX_TAIL_UNKNOWN);
    assert_ne!(daux_abi::DAUX_TAIL_INFINITE, daux_abi::DAUX_TAIL_UNKNOWN);

    // A finite tail one sample shorter than the lower sentinel must survive the round trip
    // as a finite tail. This is the boundary a fencepost error lands on.
    let longest_finite = daux_abi::DAUX_TAIL_UNKNOWN - 1;
    assert_eq!(
        Tail::from_samples(longest_finite),
        Tail::Samples(longest_finite)
    );
    assert!(Tail::from_samples(longest_finite).is_bounded());
    assert!(!Tail::from_samples(daux_abi::DAUX_TAIL_UNKNOWN).is_bounded());
}

/// `abi-v1` §8.4. The status a plug-in returns from `process`.
#[test]
fn process_status_codes_agree_with_daux_abi() {
    let table = [
        (ProcessStatus::Error, daux_abi::DAUX_PROCESS_ERROR),
        (ProcessStatus::Continue, daux_abi::DAUX_PROCESS_CONTINUE),
        (
            ProcessStatus::ContinueIfNotQuiet,
            daux_abi::DAUX_PROCESS_CONTINUE_IF_LOUD,
        ),
        (ProcessStatus::Tail, daux_abi::DAUX_PROCESS_TAIL),
        (ProcessStatus::Sleep, daux_abi::DAUX_PROCESS_SLEEP),
    ];
    for (status, code) in table {
        assert_eq!(status.code(), code, "`ProcessStatus::{status:?}`");
        assert_eq!(ProcessStatus::from_code(code), status, "decoding {code}");
    }

    // An unrecognised code — a plug-in built against a later ABI minor — must be read
    // conservatively as `Continue`. Falsely sleeping cuts audio off; falsely continuing only
    // wastes CPU, so this asymmetry is deliberate and worth pinning.
    assert_eq!(ProcessStatus::from_code(9999), ProcessStatus::Continue);
    assert_eq!(ProcessStatus::from_code(-1), ProcessStatus::Continue);
}

/// `abi-v1` §7.2.
#[test]
fn process_mode_codes_agree_with_daux_abi() {
    for (mode, code) in [
        (ProcessMode::Realtime, daux_abi::DAUX_PROCESS_MODE_REALTIME),
        (ProcessMode::Offline, daux_abi::DAUX_PROCESS_MODE_OFFLINE),
        (ProcessMode::Prefetch, daux_abi::DAUX_PROCESS_MODE_PREFETCH),
        (ProcessMode::Analysis, daux_abi::DAUX_PROCESS_MODE_ANALYSIS),
    ] {
        assert_eq!(mode.code(), code, "`ProcessMode::{mode:?}`");
    }
}

/// `abi-v1` §6.2. Every capability bit, by name, against the ABI constant it mirrors.
///
/// A shifted bit here is worse than a shifted category: a plug-in that says
/// `SAMPLE_ACCURATE_AUTO` and is read as `DYNAMIC_BUSES` invites the host to renegotiate
/// buses on a plug-in that cannot.
#[test]
fn capability_bits_agree_with_daux_abi() {
    let table: [(Capabilities, u64); 20] = [
        (Capabilities::AUDIO_EFFECT, daux_abi::DAUX_CAP_AUDIO_EFFECT),
        (Capabilities::INSTRUMENT, daux_abi::DAUX_CAP_INSTRUMENT),
        (Capabilities::MIDI_EFFECT, daux_abi::DAUX_CAP_MIDI_EFFECT),
        (Capabilities::ANALYZER, daux_abi::DAUX_CAP_ANALYZER),
        (Capabilities::MIDI_INPUT, daux_abi::DAUX_CAP_MIDI_INPUT),
        (Capabilities::MIDI_OUTPUT, daux_abi::DAUX_CAP_MIDI_OUTPUT),
        (Capabilities::MIDI2, daux_abi::DAUX_CAP_MIDI2),
        (Capabilities::SIDECHAIN, daux_abi::DAUX_CAP_SIDECHAIN),
        (
            Capabilities::DYNAMIC_BUSES,
            daux_abi::DAUX_CAP_DYNAMIC_BUSES,
        ),
        (
            Capabilities::SAMPLE_ACCURATE_AUTO,
            daux_abi::DAUX_CAP_SAMPLE_ACCURATE_AUTO,
        ),
        (
            Capabilities::NOTE_EXPRESSION,
            daux_abi::DAUX_CAP_NOTE_EXPRESSION,
        ),
        (Capabilities::HAS_GUI, daux_abi::DAUX_CAP_HAS_GUI),
        (Capabilities::REQUIRES_GUI, daux_abi::DAUX_CAP_REQUIRES_GUI),
        (
            Capabilities::SHARED_TEXTURE_GUI,
            daux_abi::DAUX_CAP_SHARED_TEXTURE_GUI,
        ),
        (
            Capabilities::OFFLINE_RENDER,
            daux_abi::DAUX_CAP_OFFLINE_RENDER,
        ),
        (
            Capabilities::HARD_REALTIME,
            daux_abi::DAUX_CAP_HARD_REALTIME,
        ),
        (Capabilities::SANDBOX_SAFE, daux_abi::DAUX_CAP_SANDBOX_SAFE),
        (Capabilities::STEREO_ONLY, daux_abi::DAUX_CAP_STEREO_ONLY),
        (
            Capabilities::LATENCY_DYNAMIC,
            daux_abi::DAUX_CAP_LATENCY_DYNAMIC,
        ),
        (
            Capabilities::TAIL_INFINITE,
            daux_abi::DAUX_CAP_TAIL_INFINITE,
        ),
    ];

    let mut seen = 0u64;
    for (capability, bit) in table {
        assert_eq!(
            capability.bits(),
            bit,
            "a capability bit disagrees with the ABI constant it mirrors"
        );
        assert_eq!(
            bit.count_ones(),
            1,
            "every DAUX_CAP_* constant is exactly one bit"
        );
        assert_eq!(seen & bit, 0, "two capabilities claim bit {bit:#x}");
        seen |= bit;
    }

    assert_eq!(
        Capabilities::ALL.bits(),
        seen,
        "`Capabilities::ALL` and this test's table describe different sets, so a bit was \
         added to one and not the other"
    );
    // A bit no build of this SDK knows must survive a round trip rather than be dropped:
    // a host must not silently strip a capability a newer plug-in declared.
    let future = Capabilities::from_bits(seen | (1 << 40));
    assert_eq!(future.unknown_bits(), 1 << 40);
    assert_eq!(future.bits(), seen | (1 << 40));
}

/// `abi-v1` §6.3.
#[test]
fn sample_format_bits_agree_with_daux_abi() {
    use daux_audio::SampleFormats;
    assert_eq!(SampleFormats::F32.bits(), daux_abi::DAUX_SAMPLE_FORMAT_F32);
    assert_eq!(SampleFormats::F64.bits(), daux_abi::DAUX_SAMPLE_FORMAT_F64);
    assert_eq!(
        (SampleFormats::F32 | SampleFormats::F64).bits(),
        daux_abi::DAUX_SAMPLE_FORMAT_F32 | daux_abi::DAUX_SAMPLE_FORMAT_F64
    );
}

/// The plug-in id grammar is transcribed into `daux-plugin-macros`, which cannot depend on
/// `daux-core`, so `#[derive(DauxPlugin)]` re-implements `PluginId::validate` from scratch.
///
/// A compile-time check cannot be run from here without `trybuild`, so this covers the half
/// that matters most in practice: `examples/gain` uses the derive with a real id, and its
/// descriptor comes out with an id `daux-core` also considers valid. If the two grammars
/// drift apart in the accepting direction, this fails; the rejecting direction is covered by
/// `daux-plugin-macros`' own unit tests over the same table.
#[test]
fn the_derives_id_grammar_accepts_what_daux_core_accepts() {
    let descriptor = <daux_example_gain::Gain as daux_core::DauxPlugin>::descriptor();
    assert!(
        daux_core::PluginId::is_valid(descriptor.id.as_str()),
        "`{}` came out of `#[derive(DauxPlugin)]` but `daux-core` rejects it, so the two \
         transcriptions of the id grammar have drifted",
        descriptor.id.as_str()
    );
    descriptor
        .validate()
        .expect("a descriptor the derive produced must satisfy daux-core's own rules");

    // And the ids `daux-core` refuses stay refused, so the check above is not vacuous.
    for hostile in [
        "",
        "gain",
        "Com.Example.Gain",
        "com..gain",
        ".com.gain",
        "com.gain.",
        "com.exa mple.gain",
    ] {
        assert!(
            !daux_core::PluginId::is_valid(hostile),
            "`{hostile}` must not be a valid plug-in id"
        );
    }
}

/// `#[derive(DauxState)]` builds nested load paths by joining with a hard-coded `'/'`,
/// because a proc-macro crate cannot name `daux_state::format::PATH_SEPARATOR`.
///
/// Changing that constant without changing `crates/daux-plugin-macros/src/state.rs` would
/// make every nested field silently fail to load — the reader would look up `a/b` while the
/// writer stored `a.b`, and `load_state` would quietly keep the defaults.
#[test]
fn the_state_path_separator_is_the_one_the_derive_hard_codes() {
    assert_eq!(
        daux_state::format::PATH_SEPARATOR,
        '/',
        "`daux-plugin-macros`' `state.rs` joins nested paths with a literal '/'; changing \
         this constant requires changing `load_field` and `expand`'s `root_prefix` too"
    );
}
