//! The derives, resolved the way a plug-in author's crate resolves them.
//!
//! `daux-plugin-macros` has thorough unit tests, but they assert on *tokens*: they check that
//! the expansion says `:: daux_plugin :: __private :: Params`, not that such a path exists or
//! that the code behind it does the right thing. Only a crate whose single dependency is
//! `daux-plugin` can check that, because only there does `::daux_plugin` resolve exactly as it
//! does in a real plug-in.
//!
//! So this file is the contract between the macros and [`daux_plugin::__private`]: every name
//! the expansions emit is exercised through the facade, and the results are asserted. A name
//! missing from `__private` is a compile error here rather than in a user's crate, where the
//! error would point at their struct and say nothing about the cause.

#![cfg(feature = "derive")]

use daux_plugin::prelude::*;

/// The allocation tripwire, installed for this test binary only. Without it
/// [`AllocGuard`](daux_plugin::AllocGuard) counts zero and
/// `the_generated_lookup_allocates_nothing` would pass vacuously.
#[global_allocator]
static COUNTING: daux_plugin::CountingAllocator = daux_plugin::CountingAllocator;

// ------------------------------------------------------------------- DauxParams ---

/// One parameter of every kind the derive can build, so that each constructor path through
/// `__private` is taken.
#[derive(DauxParams)]
struct EveryKind {
    #[param(id = 1, name = "Gain", range = -60.0..=12.0, default = 0.0, unit = "dB",
            group = "Output", decimals = 1, smoothing = "exponential(20.0)",
            flags(automatable, modulatable))]
    gain: FloatParam,
    #[param(id = 2, name = "Cutoff", range = 20.0..=20000.0, default = 1000.0,
            curve = "log", unit = "Hz")]
    cutoff: FloatParam,
    #[param(id = 3, name = "Voices", range = 1..=16, default = 8)]
    voices: IntParam,
    #[param(id = 4, name = "Invert", default = true, labels("Normal", "Inverted"))]
    invert: BoolParam,
    #[param(id = 5, name = "Shape", default = Shape::Sine)]
    shape: EnumParam<Shape>,
    #[param(id = 6, name = "Level", range = -60.0..=6.0, unit = "dB", flags(is_meter, read_only))]
    level: MeterParam,
}

/// A `ParamEnum` for the `EnumParam` field above.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Shape {
    Sine,
    Square,
}

impl ParamEnum for Shape {
    const VARIANTS: &'static [Self] = &[Self::Sine, Self::Square];

    fn name(self) -> &'static str {
        match self {
            Self::Sine => "Sine",
            Self::Square => "Square",
        }
    }

    fn index(self) -> u32 {
        self as u32
    }

    fn from_index(index: u32) -> Option<Self> {
        Self::VARIANTS.get(index as usize).copied()
    }
}

#[test]
fn the_generated_constructor_builds_every_parameter_from_its_attribute() {
    let params = EveryKind::new();

    let gain = params.gain.info();
    assert_eq!(gain.id, ParamId::new(1));
    assert_eq!(gain.name, "Gain");
    assert_eq!(gain.unit, "dB");
    assert_eq!(gain.group, "Output");
    assert_eq!(gain.min, -60.0);
    assert_eq!(gain.max, 12.0);
    assert_eq!(gain.default, 0.0);
    assert!(gain.flags.contains(ParamFlags::AUTOMATABLE));
    assert!(gain.flags.contains(ParamFlags::MODULATABLE));

    // `curve = "log"` must reach the range, not merely the info block: a logarithmic knob at
    // half travel sits far below the arithmetic mean.
    params.cutoff.set_normalized(0.5);
    let midpoint = params.cutoff.plain();
    assert!(
        (midpoint - 632.45).abs() < 1.0,
        "a logarithmic 20..=20000 range is ~632 Hz at half travel, got {midpoint}"
    );

    assert_eq!(params.voices.info().min, 1.0);
    assert_eq!(params.voices.info().max, 16.0);
    assert_eq!(params.voices.info().default, 8.0);

    // `labels(..)` reaches the formatter, which is the only place it is observable.
    let mut text = String::new();
    params.invert.to_text(1.0, &mut text);
    assert_eq!(text, "Inverted");

    assert_eq!(params.shape.value(), Shape::Sine);
    assert!(params.level.info().flags.contains(ParamFlags::IS_METER));
}

#[test]
fn param_refs_keeps_declaration_order_because_hosts_show_it_to_users() {
    let params = EveryKind::new();
    let ids: Vec<u32> = params
        .param_refs()
        .into_iter()
        .map(|(id, _)| id.get())
        .collect();
    assert_eq!(ids, vec![1, 2, 3, 4, 5, 6]);
}

#[test]
fn the_generated_lookup_answers_only_to_the_declared_ids() {
    let params = EveryKind::new();
    for id in 1..=6 {
        let param = params
            .param(ParamId::new(id))
            .unwrap_or_else(|| panic!("id {id} is declared"));
        assert_eq!(param.info().id.get(), id);
    }
    assert!(params.param(ParamId::new(0)).is_none());
    assert!(params.param(ParamId::new(7)).is_none());
    assert!(params.param(ParamId::new(u32::MAX)).is_none());
}

/// The lookup is reachable from `process`, so it must not allocate — abi-v1 §15 and
/// `CLAUDE.md` rule 1. The derive lowers it to a `match` on the raw id precisely for this.
#[test]
fn the_generated_lookup_allocates_nothing() {
    assert!(
        daux_plugin::counting_allocator_installed(),
        "without the tripwire this test cannot fail, so it must not be allowed to pass"
    );
    let params = EveryKind::new();
    let (found, allocations) = daux_plugin::AllocGuard::scope(|| {
        let mut found = 0_usize;
        for id in 0..=8_u32 {
            if params.param(ParamId::new(id)).is_some() {
                found += 1;
            }
        }
        found
    });
    assert_eq!(found, 6);
    assert_eq!(
        allocations, 0,
        "Params::param is callable from the audio thread and must not allocate"
    );
}

/// Migrations and the schema version are declared on the container and reach the trait's
/// defaulted methods. A saved project is read through them, so a dropped `migrations = ..`
/// silently loses a user's automation.
static RENAMES: [ParamMigration; 1] = [ParamMigration::rename(ParamId::new(99), ParamId::new(1))];

#[derive(DauxParams)]
#[params(state_schema_version = 4, migrations = RENAMES)]
struct Versioned {
    #[param(id = 1, name = "Gain", range = 0.0..=1.0, default = 1.0)]
    gain: FloatParam,
}

#[test]
fn the_container_attribute_reaches_the_trait_methods() {
    let params = Versioned::new();
    assert_eq!(params.state_schema_version(), 4);
    assert_eq!(params.migrations().len(), 1);
    // …and the migration is the one declared, i.e. the old id maps onto the new one.
    assert_eq!(
        daux_plugin::migrate_param_id(params.migrations(), ParamId::new(99)),
        Some(ParamId::new(1))
    );
}

/// Ids written as names are hashed at compile time. The hash is what ends up in a saved
/// project, so it has to be stable and the lookup has to agree with `param_refs`.
#[derive(DauxParams)]
struct NamedIds {
    #[param(id = "gain")]
    gain: FloatParam,
    #[param(id = "mix")]
    mix: FloatParam,
}

#[test]
fn hashed_ids_agree_between_the_reference_list_and_the_lookup() {
    let params = NamedIds {
        gain: FloatParam::new(
            ParamId::from_name("gain"),
            "Gain",
            0.0,
            ParamRange::Linear { min: 0.0, max: 1.0 },
        ),
        mix: FloatParam::new(
            ParamId::from_name("mix"),
            "Mix",
            1.0,
            ParamRange::Linear { min: 0.0, max: 1.0 },
        ),
    };

    let refs = params.param_refs();
    assert_eq!(refs.len(), 2);
    assert_ne!(
        refs[0].0, refs[1].0,
        "two names must not hash to one id, or every saved project is ambiguous"
    );
    for (id, param) in refs {
        let looked_up = params.param(id).expect("param_refs lists reachable ids");
        assert_eq!(looked_up.info().name, param.info().name);
    }
    assert_eq!(params.param_refs()[0].0, ParamId::from_name("gain"));
    assert!(params.param(ParamId::from_name("nothing")).is_none());
}

// ------------------------------------------------------------------- DauxPlugin ---

/// Every key the descriptor derive accepts, so that each `__private` name it emits — the
/// descriptor, the version, the category, the capabilities and the sample formats — is used.
#[derive(DauxPlugin)]
#[plugin(
    id = "com.example.derived",
    name = "Derived",
    vendor = "DAUxPlug tests",
    version = "2.3.4.5",
    description = "A descriptor written entirely in an attribute.",
    url = "https://example.com",
    support_url = "https://example.com/support",
    copyright = "© Example",
    license = "MIT OR Apache-2.0",
    category = "instrument",
    capabilities(instrument, midi_input, has_gui),
    features("synth", "polyphonic"),
    sample_formats(f32, f64),
    state_schema_version = 7,
    min_abi = (1, 0)
)]
struct Derived;

#[test]
fn the_descriptor_derive_transcribes_every_key() {
    let d = Derived::descriptor();
    assert_eq!(d.id.as_str(), "com.example.derived");
    assert_eq!(d.name, "Derived");
    assert_eq!(d.vendor, "DAUxPlug tests");
    assert_eq!(d.version, Version::new(2, 3, 4).with_build(5));
    assert_eq!(
        d.description,
        "A descriptor written entirely in an attribute."
    );
    assert_eq!(d.url, "https://example.com");
    assert_eq!(d.support_url, "https://example.com/support");
    assert_eq!(d.copyright, "© Example");
    assert_eq!(d.license, "MIT OR Apache-2.0");
    assert_eq!(d.category, Category::Instrument);
    assert!(d.capabilities.contains(Capabilities::INSTRUMENT));
    assert!(d.capabilities.contains(Capabilities::MIDI_INPUT));
    assert!(d.capabilities.contains(Capabilities::HAS_GUI));
    assert!(!d.capabilities.contains(Capabilities::AUDIO_EFFECT));
    assert_eq!(
        d.features,
        vec!["synth".to_owned(), "polyphonic".to_owned()]
    );
    assert!(d.sample_formats.contains(SampleFormat::F32));
    assert!(d.sample_formats.contains(SampleFormat::F64));
    assert_eq!(d.state_schema_version, 7);
    assert_eq!(d.min_abi, (1, 0));

    // The descriptor the derive built must be one the runtime accepts, or the compile-time
    // checks and the run-time checks have drifted apart.
    assert!(d.validate().is_ok());
}

/// The minimum a descriptor can declare. The derive must fill the rest with the same defaults
/// the builder uses, not with empty-but-invalid values.
#[derive(DauxPlugin)]
#[plugin(id = "com.example.minimal", name = "Minimal")]
struct Minimal;

#[test]
fn a_minimal_descriptor_is_still_valid() {
    let d = Minimal::descriptor();
    assert_eq!(d.id.as_str(), "com.example.minimal");
    assert_eq!(
        d,
        PluginDescriptor::builder("com.example.minimal", "Minimal")
            .build()
            .expect("valid"),
        "an attribute with only `id` and `name` must produce exactly what the same two \
         arguments produce through the builder — the derive fills nothing in of its own"
    );
    assert!(
        d.validate().is_ok(),
        "a two-key descriptor must still be usable"
    );
}

// -------------------------------------------------------------------- DauxState ---

/// One field of every storage kind, plus a nested struct and a defaulted field.
#[derive(DauxState, Debug, PartialEq)]
#[state(version = 3, group = "dsp")]
struct Saved {
    #[state]
    gain: f64,
    #[state]
    ratio: f32,
    #[state]
    voices: u8,
    #[state]
    enabled: bool,
    #[state(key = "preset-name")]
    preset: String,
    #[state]
    blob: Vec<u8>,
    #[state(default = 0.5)]
    added_in_v3: f64,
    #[state(nested)]
    filter: Nested,
    #[state(skip)]
    scratch: Vec<f32>,
}

/// The nested half of `Saved`, stored as its own group.
#[derive(DauxState, Debug, PartialEq)]
struct Nested {
    #[state]
    cutoff: f64,
}

impl Default for Saved {
    fn default() -> Self {
        Self {
            gain: -6.0,
            ratio: 0.25,
            voices: 8,
            enabled: true,
            preset: "Init".to_owned(),
            blob: vec![1, 2, 3],
            added_in_v3: 0.75,
            filter: Nested { cutoff: 1_000.0 },
            scratch: vec![0.0; 4],
        }
    }
}

#[test]
fn state_round_trips_through_the_facades_writer_and_reader() {
    let original = Saved::default();
    let mut writer = StateWriter::new(Saved::STATE_VERSION);
    original.save_state(&mut writer).expect("save");
    let bytes = writer.finish();

    let reader = StateReader::from_bytes(&bytes).expect("the blob we just wrote parses");
    assert_eq!(reader.version(), StateVersion(3));

    let mut restored = Saved {
        gain: 0.0,
        ratio: 0.0,
        voices: 0,
        enabled: false,
        preset: String::new(),
        blob: Vec::new(),
        added_in_v3: 0.0,
        filter: Nested { cutoff: 0.0 },
        // A skipped field is not restored, so it keeps whatever the constructor put there.
        scratch: vec![9.0],
    };
    restored.load_state(&reader).expect("load");

    assert_eq!(restored.gain, -6.0);
    assert_eq!(restored.ratio, 0.25);
    assert_eq!(restored.voices, 8);
    assert!(restored.enabled);
    assert_eq!(restored.preset, "Init");
    assert_eq!(restored.blob, vec![1, 2, 3]);
    assert_eq!(restored.added_in_v3, 0.75);
    assert_eq!(restored.filter.cutoff, 1_000.0);
    assert_eq!(
        restored.scratch,
        vec![9.0],
        "`skip` means the field is not part of the state, in both directions"
    );
}

/// The point of `#[state(group = ..)]` and `#[state(nested)]`: keys are paths, so two structs
/// can both own a field called `cutoff` without colliding.
#[test]
fn groups_and_nesting_produce_the_documented_key_paths() {
    let mut writer = StateWriter::new(Saved::STATE_VERSION);
    Saved::default().save_state(&mut writer).expect("save");
    let bytes = writer.finish();
    let reader = StateReader::from_bytes(&bytes).expect("parses");

    assert_eq!(reader.f64("dsp/gain").expect("grouped"), -6.0);
    assert_eq!(reader.f64("dsp/filter/cutoff").expect("nested"), 1_000.0);
    assert_eq!(reader.str("dsp/preset-name").expect("renamed key"), "Init");
    // …and the ungrouped spellings are genuinely absent.
    assert!(reader.opt_f64("gain").is_none());
    assert!(reader.opt_f64("cutoff").is_none());
}

/// A key that was never written is an error unless the field said `default`. Silently reading
/// zero is how a plug-in update turns a user's mix into silence.
#[test]
fn a_missing_key_is_an_error_and_a_defaulted_one_is_not() {
    // A blob written by an older build: no `added_in_v3`, and no `enabled` either.
    let mut writer = StateWriter::new(StateVersion(3));
    writer.begin_group("dsp");
    writer.put_f64("gain", -12.0);
    writer.put_f64("ratio", 0.5);
    writer.put_i64("voices", 4);
    writer.put_str("preset-name", "Old");
    writer.put_bytes("blob", &[7]);
    writer.begin_group("filter");
    writer.put_f64("cutoff", 500.0);
    writer.end_group();
    writer.end_group();
    let bytes = writer.finish();
    let reader = StateReader::from_bytes(&bytes).expect("parses");

    let mut restored = Saved::default();
    let error = restored
        .load_state(&reader)
        .expect_err("`enabled` has no default and was never written");
    assert!(
        format!("{error}").contains("enabled"),
        "the error must name the key, got: {error}"
    );

    // With `enabled` present, the same blob loads and the defaulted field takes its default.
    let mut writer = StateWriter::new(StateVersion(3));
    writer.begin_group("dsp");
    writer.put_f64("gain", -12.0);
    writer.put_f64("ratio", 0.5);
    writer.put_i64("voices", 4);
    writer.put_bool("enabled", false);
    writer.put_str("preset-name", "Old");
    writer.put_bytes("blob", &[7]);
    writer.begin_group("filter");
    writer.put_f64("cutoff", 500.0);
    writer.end_group();
    writer.end_group();
    let bytes = writer.finish();
    let reader = StateReader::from_bytes(&bytes).expect("parses");

    let mut restored = Saved::default();
    restored
        .load_state(&reader)
        .expect("an older blob still loads");
    assert_eq!(restored.gain, -12.0);
    assert_eq!(restored.voices, 4);
    assert!(!restored.enabled);
    assert_eq!(
        restored.added_in_v3, 0.5,
        "a field added later takes its declared default, not zero"
    );
}

/// A hostile or corrupt blob must produce a `StateError`, never a wrapped number. `voices` is
/// a `u8`; a blob claiming 1_000 voices cannot be honoured.
#[test]
fn an_integer_that_does_not_fit_the_field_is_refused_rather_than_truncated() {
    let mut writer = StateWriter::new(StateVersion(3));
    writer.begin_group("dsp");
    writer.put_f64("gain", -12.0);
    writer.put_f64("ratio", 0.5);
    writer.put_i64("voices", 1_000);
    writer.put_bool("enabled", true);
    writer.put_str("preset-name", "Hostile");
    writer.put_bytes("blob", &[]);
    writer.begin_group("filter");
    writer.put_f64("cutoff", 500.0);
    writer.end_group();
    writer.end_group();
    let bytes = writer.finish();
    let reader = StateReader::from_bytes(&bytes).expect("parses");

    let mut restored = Saved::default();
    let error = restored
        .load_state(&reader)
        .expect_err("1000 does not fit in a u8");
    assert!(
        format!("{error}").contains("voices"),
        "the error must name the field, got: {error}"
    );
    assert_eq!(
        restored.voices, 8,
        "a refused load must not leave a truncated value behind in that field"
    );
}

/// A truncated blob never reaches the derive at all, but the derive's error type has to be the
/// one the reader produces, or a plug-in cannot use `?` across the two.
#[test]
fn a_truncated_blob_is_rejected_by_the_reader_the_derive_uses() {
    let mut writer = StateWriter::new(Saved::STATE_VERSION);
    Saved::default().save_state(&mut writer).expect("save");
    let bytes = writer.finish();

    for cut in [0, 1, 4, 8, bytes.len() / 2, bytes.len() - 1] {
        let error = StateReader::from_bytes(&bytes[..cut]);
        assert!(
            error.is_err(),
            "a blob truncated to {cut} bytes must not parse"
        );
    }
}
