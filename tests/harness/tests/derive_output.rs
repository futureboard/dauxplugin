//! Compiles and runs what `#[derive(DauxParams)]`, `#[derive(DauxPlugin)]` and
//! `#[derive(DauxState)]` actually generate.
//!
//! `daux-plugin-macros` can test that it *emits* a given token stream, and it does — but a
//! proc-macro crate cannot depend on `daux-plugin` (that would be a dependency cycle), so it
//! can never compile its own output against the real types. A macro that emits
//! `::daux_plugin::__private::StateWriter` when the real path is
//! `::daux_plugin::__private::StateWrite` passes every test in its own crate and fails in
//! every plug-in.
//!
//! `tests/harness` is downstream of the facade, so this is where the two halves meet. Each
//! test below is a failure mode rather than a feature: the happy path is covered incidentally
//! by `examples/gain` compiling at all.

use daux_plugin::__private::{
    BoolParam, EnumParam, FloatParam, IntParam, MeterParam, Param, ParamEnum, ParamId,
    ParamMigration, ParamRange, Params, StateReader, StateVersion, StateWriter,
};
use daux_plugin::{DauxParams, DauxPlugin, DauxState};

// ---------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------

/// A parameter enum, so `EnumParam` has something to select over.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shape {
    /// A sine.
    Sine,
    /// A saw.
    Saw,
}

impl ParamEnum for Shape {
    const VARIANTS: &'static [Self] = &[Self::Sine, Self::Saw];
    fn name(self) -> &'static str {
        match self {
            Self::Sine => "Sine",
            Self::Saw => "Saw",
        }
    }
    fn index(self) -> u32 {
        self as u32
    }
    fn from_index(index: u32) -> Option<Self> {
        Self::VARIANTS.get(index as usize).copied()
    }
}

/// The migration table `#[params(migrations = ..)]` points at.
pub static MIGRATIONS: [ParamMigration; 1] =
    [ParamMigration::rename(ParamId::new(9), ParamId::new(1))];

/// Every parameter type the derive supports, built entirely from attributes.
#[derive(DauxParams)]
#[params(state_schema_version = 2, migrations = MIGRATIONS)]
pub struct AllParams {
    /// Logarithmic float with every optional key set.
    #[param(id = 1, name = "Cutoff", range = 20.0..=20000.0, default = 1000.0, curve = "log",
            unit = "Hz", decimals = 1, group = "Filter", smoothing = "exponential(20.0)",
            flags(automatable, modulatable))]
    pub cutoff: FloatParam,
    /// Integer literals on a *float* parameter: the derive has to widen them.
    #[param(id = 2, name = "Mix", range = 0..=1, default = 1, smoothing = "linear(5.0)")]
    pub mix: FloatParam,
    /// Skewed float.
    #[param(id = 3, name = "Drive", range = 0.0..=1.0, default = 0.5, curve = "skew(0.3)")]
    pub drive: FloatParam,
    /// Integer.
    #[param(id = 4, name = "Voices", range = 1..=16, default = 8, unit = "voices")]
    pub voices: IntParam,
    /// Boolean with labels.
    #[param(id = 5, name = "Invert", default = true, labels("Normal", "Inverted"))]
    pub invert: BoolParam,
    /// Enum.
    #[param(id = 6, name = "Shape", default = Shape::Sine, flags(stepped))]
    pub shape: EnumParam<Shape>,
    /// Meter — read-only, host-written.
    #[param(id = 7, name = "Level", range = -60.0..=6.0, unit = "dB", decimals = 1,
            flags(is_meter, read_only))]
    pub level: MeterParam,
}

/// A parameter id given as a constant rather than a literal.
pub const OTHER_ID: u32 = 4_000_000_000;

/// Ids the macro cannot compare at expansion time, plus a skipped field.
#[derive(DauxParams)]
pub struct MixedIds {
    /// Id hashed from a name.
    #[param(id = "gain")]
    pub gain: FloatParam,
    /// Id from a constant.
    #[param(id = OTHER_ID)]
    pub other: FloatParam,
    /// Not a parameter at all.
    #[param(skip)]
    pub scratch: Vec<f32>,
}

impl MixedIds {
    /// Built by hand: the derive cannot generate `new()` for fields it was not fully told
    /// about.
    #[must_use]
    pub fn new() -> Self {
        Self {
            gain: FloatParam::new(ParamId::from_name("gain"), "Gain", 0.0, ParamRange::UNIT),
            other: FloatParam::new(ParamId::new(OTHER_ID), "Other", 0.0, ParamRange::UNIT),
            scratch: Vec::new(),
        }
    }
}

impl Default for MixedIds {
    fn default() -> Self {
        Self::new()
    }
}

/// A bank with no parameters at all still has to implement the trait.
#[derive(DauxParams)]
pub struct EmptyParams {}

/// The nested half of the state fixture.
#[derive(DauxState, Default)]
pub struct FilterState {
    /// Cutoff.
    #[state]
    pub cutoff: f64,
    /// Mode, under an explicit key.
    #[state(key = "mode")]
    pub mode: String,
}

/// Every storage kind, a group, a version, defaults and a nested child.
#[derive(DauxState, Default)]
#[state(version = 3, group = "dsp")]
pub struct SavedState {
    /// Plain `f64`.
    #[state]
    pub gain: f64,
    /// Narrowed float.
    #[state]
    pub mix: f32,
    /// Checked integer.
    #[state]
    pub voices: u32,
    /// Wide integer.
    #[state]
    pub samples: u64,
    /// Signed integer.
    #[state]
    pub offset: i64,
    /// Boolean.
    #[state]
    pub bypass: bool,
    /// String.
    #[state]
    pub preset: String,
    /// Bytes.
    #[state]
    pub curve: Vec<u8>,
    /// Added in a later version, `Default`-filled when absent.
    #[state(default)]
    pub added_later: f64,
    /// Added later with an explicit fallback.
    #[state(default = 0.5)]
    pub also_added: f32,
    /// Added later, string fallback.
    #[state(default = String::from("Init"))]
    pub label: String,
    /// Added later, integer fallback.
    #[state(default = 4)]
    pub width: u16,
    /// Added later, byte fallback.
    #[state(default = Vec::new())]
    pub table: Vec<u8>,
    /// Added later, boolean fallback.
    #[state(default)]
    pub locked: bool,
    /// Nested group.
    #[state(nested, key = "filter")]
    pub filter: FilterState,
    /// Explicitly not saved.
    #[state(skip)]
    pub scratch: Vec<f32>,
    /// Not annotated, so not saved.
    pub untouched: u8,
}

/// The descriptor derive with every key it accepts.
#[derive(DauxPlugin)]
#[plugin(
    id = "com.example.derives",
    name = "Derives",
    vendor = "Example Audio",
    version = "1.2.3.4",
    description = "Every attribute the plug-in derive accepts.",
    url = "https://example.test",
    support_url = "https://example.test/support",
    copyright = "(c) 2026 Example Audio",
    license = "MIT OR Apache-2.0",
    category = "instrument",
    capabilities(audio_effect, has_gui, midi_input),
    features("utility", "gain"),
    sample_formats(f32, f64),
    state_schema_version = 3,
    min_abi = (1, 0)
)]
pub struct Everything {
    /// Parameters.
    pub params: AllParams,
}

/// The shortest descriptor the derive accepts.
#[derive(DauxPlugin)]
#[plugin(id = "com.example.min", name = "Min")]
pub struct Min;

/// Stands in for `DauxPlugin`, to prove the inherent `descriptor()` wins name resolution.
pub trait HasDescriptor {
    /// The descriptor.
    fn descriptor() -> daux_plugin::__private::PluginDescriptor;
}

impl HasDescriptor for Everything {
    fn descriptor() -> daux_plugin::__private::PluginDescriptor {
        // If the inherent function did not outrank the trait method here, this would be
        // infinite recursion rather than a delegation — which is exactly the shape every
        // `impl DauxPlugin` written against the derive uses.
        Self::descriptor()
    }
}

// ---------------------------------------------------------------------------------------
// The `crate = ..` redirect
// ---------------------------------------------------------------------------------------

/// The escape hatch a workspace crate has to use, since it cannot depend on the facade.
///
/// This is the only place the redirect is *compiled*. `daux-plugin-macros` asserts that it
/// emits `::daux_plugin_api::__private::…`, and `daux-plugin-api` asserts that every one of
/// those names exists — but only generated code that actually resolves proves the two agree.
/// The redirect once resolved for `DauxParams` and failed for `DauxState` and `DauxPlugin`,
/// because `daux-plugin-api::__private` carried the parameter names alone.
mod redirected {
    use daux_plugin::{DauxParams, DauxPlugin, DauxState};
    use daux_plugin_api::{FloatParam, ParamId, Params as _};

    /// Parameters, resolved through `daux-plugin-api` rather than the facade.
    #[derive(DauxParams)]
    #[params(crate = ::daux_plugin_api)]
    pub struct Redirected {
        /// One parameter is enough to make the expansion name the whole set.
        #[param(id = 1, name = "Gain", range = -60.0..=6.0, default = 0.0, unit = "dB")]
        pub gain: FloatParam,
    }

    /// State, resolved the same way — this is the half that used to fail.
    #[derive(DauxState, Default)]
    #[state(crate = ::daux_plugin_api, version = 1)]
    pub struct RedirectedState {
        /// A value.
        #[state]
        pub gain: f64,
    }

    /// A descriptor, resolved the same way.
    #[derive(DauxPlugin)]
    #[plugin(
        crate = ::daux_plugin_api,
        id = "com.example.redirected",
        name = "Redirected",
        category = "effect"
    )]
    pub struct RedirectedPlugin;

    /// Runs the three of them, so the test is not merely a compile check.
    pub fn exercise() {
        let params = Redirected::new();
        assert_eq!(params.param_refs().len(), 1);
        assert!(params.param(ParamId::new(1)).is_some());

        let descriptor = RedirectedPlugin::descriptor();
        assert_eq!(descriptor.id, "com.example.redirected");
        assert_eq!(descriptor.category, daux_plugin_api::Category::Effect);

        let mut writer = daux_plugin_api::StateWriter::new(RedirectedState::STATE_VERSION);
        let state = RedirectedState { gain: -3.0 };
        state.save_state(&mut writer).expect("save");
        let bytes = writer.try_finish().expect("finish");
        let reader = daux_plugin_api::StateReader::from_bytes(&bytes).expect("decode");
        let mut restored = RedirectedState::default();
        restored.load_state(&reader).expect("load");
        assert_eq!(restored.gain, -3.0);
    }
}

// ---------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------

/// The generated `Params` impl describes what the attributes said.
#[test]
fn the_params_derive_produces_the_bank_the_attributes_describe() {
    let params = AllParams::new();

    let refs = params.param_refs();
    assert_eq!(refs.len(), 7, "one entry per non-skipped field");
    // Declaration order, not sorted order: a host lays parameters out in the order it is
    // given, and reordering them silently rearranges every generic editor.
    let ids: Vec<u32> = refs.iter().map(|(id, _)| id.0).collect();
    assert_eq!(ids, [1, 2, 3, 4, 5, 6, 7]);

    assert_eq!(params.state_schema_version(), 2);
    assert_eq!(params.migrations().len(), 1);

    assert_eq!(params.cutoff.plain(), 1000.0);
    assert!(params.invert.value());
    assert_eq!(params.voices.value(), 8);
    assert_eq!(params.shape.value(), Shape::Sine);

    // Every declared key reached the `ParamInfo` a host reads.
    let cutoff = params.param(ParamId::new(1)).expect("parameter 1").info();
    assert_eq!(cutoff.name, "Cutoff");
    assert_eq!(cutoff.unit, "Hz");
    assert_eq!(cutoff.group, "Filter");
    assert_eq!(cutoff.min, 20.0);
    assert_eq!(cutoff.max, 20_000.0);
    assert!(cutoff.flags.is_automatable());
    assert!(
        cutoff
            .flags
            .contains(daux_parameter::ParamFlags::MODULATABLE)
    );

    let level = params.param(ParamId::new(7)).expect("parameter 7").info();
    assert!(level.flags.contains(daux_parameter::ParamFlags::IS_METER));
    assert!(level.flags.is_read_only());
}

/// The generated `param` lookup answers by permanent id and refuses everything else.
///
/// It is the one generated method reachable from the audio thread, so it must be a `match`
/// on the raw id rather than a walk of `param_refs` — which allocates a `Vec`.
#[test]
fn the_generated_param_lookup_is_by_id_and_never_guesses() {
    let params = AllParams::new();
    for id in 1..=7 {
        assert!(params.param(ParamId::new(id)).is_some(), "id {id}");
    }
    for id in [0, 8, 99, u32::MAX] {
        assert!(params.param(ParamId::new(id)).is_none(), "id {id}");
    }

    // And it does not allocate. `param_refs` does — that is why it is `[main-thread]` while
    // this is `[any-thread]`. The counter only reports anything with the counting allocator
    // installed, which this binary does at the bottom of the file.
    let (found, allocations) = daux_rt::AllocGuard::scope(|| params.param(ParamId::new(4)));
    assert!(found.is_some());
    assert!(
        daux_rt::counting_allocator_installed(),
        "without the counting allocator the assertion below is vacuous"
    );
    assert_eq!(
        allocations, 0,
        "the generated `param` allocated; it must be a match on the raw id, because CLAP \
         calls `flush` on the audio thread and reaches parameters through exactly this"
    );
}

/// Ids the macro cannot evaluate — a hashed name, a constant — still reach the right field.
#[test]
fn parameter_ids_from_names_and_constants_resolve_to_their_fields() {
    let mixed = MixedIds::new();
    assert_eq!(mixed.param_refs().len(), 2, "`scratch` is skipped");

    let hashed = ParamId::from_name("gain");
    assert!(mixed.param(hashed).is_some());
    assert!(mixed.param(ParamId::new(OTHER_ID)).is_some());
    assert!(mixed.param(ParamId::new(1)).is_none());

    // A hashed id is stable: it is a permanent id, and a different hash on a later build
    // would orphan every saved automation lane.
    assert_eq!(ParamId::from_name("gain"), hashed);
    assert_ne!(
        ParamId::from_name("Gain"),
        hashed,
        "hashing is case-sensitive"
    );

    assert!(EmptyParams {}.param_refs().is_empty());
    assert!(EmptyParams {}.param(ParamId::new(1)).is_none());
}

/// A state document written by the derive reads back identically.
#[test]
fn the_state_derive_round_trips_every_storage_kind() {
    let state = SavedState {
        gain: -6.0,
        mix: 0.25,
        voices: 8,
        samples: 48_000,
        offset: -3,
        bypass: true,
        preset: String::from("Lead"),
        curve: vec![1, 2, 3],
        filter: FilterState {
            cutoff: 1000.0,
            mode: String::from("lowpass"),
        },
        ..SavedState::default()
    };

    let mut writer = StateWriter::new(SavedState::STATE_VERSION);
    state.save_state(&mut writer).expect("save");
    let bytes = writer.try_finish().expect("finish");

    let reader = StateReader::from_bytes(&bytes).expect("decode");
    assert_eq!(reader.version(), StateVersion(3));
    // The `#[state(group = "dsp")]` prefix and the nested `filter` key are both in the path,
    // joined with `daux_state::format::PATH_SEPARATOR` — which the derive hard-codes as '/'.
    assert_eq!(reader.f64("dsp/gain"), Ok(-6.0));
    assert_eq!(reader.str("dsp/filter/mode"), Ok("lowpass"));

    let mut restored = SavedState::default();
    restored.load_state(&reader).expect("load");
    assert_eq!(restored.gain, -6.0);
    assert_eq!(restored.mix, 0.25);
    assert_eq!(restored.voices, 8);
    assert_eq!(restored.samples, 48_000);
    assert_eq!(restored.offset, -3);
    assert!(restored.bypass);
    assert_eq!(restored.preset, "Lead");
    assert_eq!(restored.curve, vec![1, 2, 3]);
    assert_eq!(restored.filter.cutoff, 1000.0);
    assert_eq!(restored.filter.mode, "lowpass");

    // Skipped and unannotated fields keep their defaults rather than being invented.
    assert!(restored.scratch.is_empty());
    assert_eq!(restored.untouched, 0);
}

/// A blob an older build wrote still loads: that is the whole point of `#[state(default)]`.
#[test]
fn a_state_blob_missing_the_newer_keys_still_loads() {
    let mut old = StateWriter::new(StateVersion(1));
    old.begin_group("dsp");
    old.put_f64("gain", -3.0);
    old.put_f64("mix", 0.5);
    old.put_i64("voices", 4);
    old.put_i64("samples", 1);
    old.put_i64("offset", 0);
    old.put_bool("bypass", false);
    old.put_str("preset", "Old");
    old.put_bytes("curve", &[]);
    old.begin_group("filter");
    old.put_f64("cutoff", 100.0);
    old.put_str("mode", "highpass");
    old.end_group();
    old.end_group();
    let bytes = old.try_finish().expect("finish");

    let reader = StateReader::from_bytes(&bytes).expect("decode");
    let mut migrated = SavedState::default();
    migrated
        .load_state(&reader)
        .expect("a blob from an older schema must still open the user's project");

    assert_eq!(migrated.preset, "Old");
    assert_eq!(migrated.filter.mode, "highpass");
    // The keys that did not exist yet come from their declared fallbacks.
    assert_eq!(migrated.label, "Init");
    assert_eq!(migrated.also_added, 0.5);
    assert_eq!(migrated.width, 4);
    assert_eq!(migrated.added_later, 0.0);
    assert!(!migrated.locked);
}

/// A value that does not fit its field is refused, and the field is left untouched.
///
/// The alternative — a silent wrap — turns a corrupt project file into a plug-in that runs
/// with a plausible-looking wrong value, which is far worse than a refused load.
#[test]
fn a_state_value_that_does_not_fit_its_field_is_refused_not_truncated() {
    let mut hostile = StateWriter::new(StateVersion(3));
    hostile.begin_group("dsp");
    hostile.put_f64("gain", 0.0);
    hostile.put_f64("mix", 0.0);
    hostile.put_i64("voices", 5_000_000_000); // `voices` is a u32.
    hostile.end_group();
    let bytes = hostile.try_finish().expect("finish");

    let reader = StateReader::from_bytes(&bytes).expect("decode");
    let mut victim = SavedState::default();
    let error = victim
        .load_state(&reader)
        .expect_err("5e9 does not fit in a u32");
    assert!(
        format!("{error}").contains("voices"),
        "the error must name the field that failed: {error}"
    );
    assert_eq!(victim.voices, 0, "a refused load must not half-apply");
}

/// A key with no default and no stored value is an error, not a zero.
#[test]
fn a_missing_state_key_without_a_default_is_an_error() {
    let mut truncated = StateWriter::new(StateVersion(3));
    truncated.begin_group("dsp");
    truncated.put_f64("gain", 0.0);
    truncated.end_group();
    let bytes = truncated.try_finish().expect("finish");

    let reader = StateReader::from_bytes(&bytes).expect("decode");
    SavedState::default()
        .load_state(&reader)
        .expect_err("`mix` has no `#[state(default)]`, so its absence is a corrupt document");
}

/// A value that cannot be written is refused at save time rather than silently narrowed.
#[test]
fn a_state_value_with_no_wire_spelling_is_refused_at_save_time() {
    let overflow = SavedState {
        samples: u64::MAX,
        ..SavedState::default()
    };
    let mut writer = StateWriter::new(StateVersion(3));
    overflow
        .save_state(&mut writer)
        .expect_err("u64::MAX has no i64 spelling, and wrapping it would corrupt the preset");
}

/// The descriptor derive produces exactly what the attribute declared.
#[test]
fn the_plugin_derive_produces_the_descriptor_the_attribute_declares() {
    let descriptor = Everything::descriptor();
    assert_eq!(descriptor.id, "com.example.derives");
    assert_eq!(descriptor.name, "Derives");
    assert_eq!(descriptor.vendor, "Example Audio");
    assert_eq!(descriptor.features, ["utility", "gain"]);
    assert_eq!(descriptor.state_schema_version, 3);
    assert_eq!(descriptor.category, daux_core::Category::Instrument);
    assert!(descriptor.capabilities.is_has_gui());
    assert!(descriptor.capabilities.is_audio_effect());
    assert!(descriptor.capabilities.is_midi_input());
    assert!(!descriptor.capabilities.is_instrument(), "not declared");
    descriptor
        .validate()
        .expect("a derived descriptor must satisfy daux-core's own rules");

    // The minimal form fills the rest in rather than refusing.
    let min = Min::descriptor();
    assert_eq!(min.name, "Min");
    assert_eq!(min.id, "com.example.min");
    min.validate().expect("the minimal descriptor is valid too");

    // The inherent `descriptor()` outranks a trait method of the same name.
    assert_eq!(<Everything as HasDescriptor>::descriptor().name, "Derives");
}

/// The `crate = ..` redirect resolves for all three derives.
#[test]
fn the_crate_redirect_resolves_for_every_derive() {
    redirected::exercise();
}

/// Installed so the allocation assertion above measures rather than assumes.
#[global_allocator]
static TEST_ALLOCATOR: daux_rt::CountingAllocator = daux_rt::CountingAllocator;
