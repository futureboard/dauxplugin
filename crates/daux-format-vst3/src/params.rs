//! The parameter mirror: VST3's controller-side view of a DAUx parameter set.
//!
//! # Why a mirror rather than the parameters themselves
//!
//! VST3 calls `IEditController` on the UI thread and `IAudioProcessor::process` on the audio
//! thread, and — for a plug-in that keeps both halves in one object, as this adapter does —
//! it calls them *concurrently*. Reaching the live `&dyn Param` from the controller side
//! means going through
//! [`DauxPlugin::controller`](daux_plugin_api::DauxPlugin::controller), which takes `&mut
//! self`, so the UI thread would be holding an exclusive borrow of the same object the audio
//! thread is inside. That is undefined behaviour in Rust, whatever C++ gets away with.
//!
//! The mirror removes the problem rather than papering over it: everything the controller
//! half needs is captured once, on the main thread, during `initialize`, and every later
//! controller call reads or writes an atomic. The audio thread never takes the parameters
//! either — VST3 automation is translated into DAUx `ParamValue` **events** with plain
//! values and handed to `process`, which is how the DAUx event model expects parameter
//! changes to arrive anyway.
//!
//! # What the mirror stores, and what it costs
//!
//! | Captured at `initialize` | Used for |
//! |---|---|
//! | [`ParamInfo`] | `getParameterInfo`, names, units, step counts |
//! | [`Curve`] | `normalizedParamToPlain` and the reverse — see [`crate::mapping::Curve`] |
//! | the text of every step of a discrete parameter | `getParamStringByValue` for enums, booleans and switches |
//! | the current value | the controller's normalised value |
//!
//! The one thing it cannot capture is a **custom formatter on a continuous parameter**: a
//! `with_formatter` closure is a function of a value, and there is no way to tabulate it. A
//! continuous parameter is therefore rendered the way `daux-parameter`'s *default* formatter
//! renders it — `"-6.00 dB"` — which is byte-identical for every parameter that has not
//! installed one. This is the only translation loss in the module and it is deliberate; the
//! alternative is a data race.

use daux_plugin_api::{AtomicF64, ParamFlags, ParamId, ParamInfo, Params, text};

use crate::mapping::Curve;

/// Largest discrete parameter whose step texts are captured.
///
/// A 128-step MIDI-note selector is worth tabulating; a 100 000-step one is a continuous
/// parameter wearing a hat, and tabulating it would allocate megabytes during `initialize`.
const MAX_TABULATED_STEPS: u32 = 512;

/// Fraction digits used when rendering a continuous parameter, matching
/// [`daux_plugin_api::FloatParam`]'s default.
const DEFAULT_DECIMALS: u8 = 2;

/// One parameter, as the VST3 controller half sees it.
#[derive(Debug)]
pub struct ParamEntry {
    /// The permanent DAUx id. **This is also the VST3 `ParamID`** — see
    /// [`ParamEntry::vst3_id`].
    pub id: ParamId,
    /// Everything static the host asks about: name, unit, bounds, step count, flags.
    pub info: ParamInfo,
    /// The plain ↔ normalised mapping.
    pub curve: Curve,
    /// The default, normalised, which is what `ParameterInfo` carries.
    pub default_normalized: f64,
    /// The text of every step, for a discrete parameter small enough to tabulate.
    pub step_texts: Option<Vec<String>>,
    /// The controller's current value, normalised. Written by the UI thread
    /// (`setParamNormalized`) and by the audio thread (automation), read by both.
    pub value: AtomicF64,
}

impl ParamEntry {
    /// `[any-thread]` The VST3 parameter id.
    ///
    /// Identical to the DAUx id, deliberately and permanently. Some adapters hash their
    /// parameter names into a `ParamID`; DAUx ids are already stable `u32`s (abi-v1 §14), so
    /// hashing them would only add a way for two versions of a plug-in to disagree about
    /// which parameter a saved automation lane refers to. Renaming a parameter is free;
    /// renumbering corrupts every project that used it.
    #[inline]
    #[must_use]
    pub const fn vst3_id(&self) -> u32 {
        self.id.get()
    }

    /// `[any-thread]` The current normalised value.
    #[inline]
    #[must_use]
    pub fn normalized(&self) -> f64 {
        self.value.get()
    }

    /// `[any-thread]` Stores a normalised value, clamped and snapped to the curve.
    ///
    /// Allocation-free and wait-free: this is called from the audio thread while applying
    /// automation, and from the UI thread while the user drags a knob.
    #[inline]
    pub fn set_normalized(&self, normalized: f64) {
        self.value.set(self.curve.snap(normalized));
    }

    /// `[any-thread]` The current plain value.
    #[inline]
    #[must_use]
    pub fn plain(&self) -> f64 {
        self.curve.to_plain(self.normalized())
    }

    /// `[main-thread]` `true` when the host must not write this parameter.
    #[must_use]
    pub fn is_read_only(&self) -> bool {
        self.info.flags.is_read_only() || self.info.flags.contains(ParamFlags::IS_METER)
    }

    /// `[main-thread]` Renders a plain value as text, into `out`.
    ///
    /// Exact for discrete parameters, whose steps were tabulated from the plug-in's own
    /// formatter. For continuous ones see the module documentation.
    pub fn format(&self, plain: f64, out: &mut String) {
        if let Some(texts) = &self.step_texts {
            let index = self.step_index(plain);
            if let Some(text) = texts.get(index) {
                out.clear();
                out.push_str(text);
                return;
            }
        }
        text::format_value(plain, DEFAULT_DECIMALS, &self.info.unit, out);
    }

    /// `[main-thread]` Parses user-entered text into a plain value.
    ///
    /// Discrete parameters match their captured step text first, case-insensitively and
    /// ignoring surrounding whitespace, so typing `"lowpass"` into a filter-mode field
    /// works. Everything falls back to `daux-parameter`'s lenient number parser, which
    /// accepts `"  -6.0 dB "`, `"6dB"`, `"1e3"` and `"-inf"`.
    #[must_use]
    pub fn parse(&self, entered: &str) -> Option<f64> {
        let trimmed = entered.trim();
        if let Some(texts) = &self.step_texts {
            for (index, text) in texts.iter().enumerate() {
                if text.trim().eq_ignore_ascii_case(trimmed) {
                    return Some(self.curve.to_plain(self.step_normalized(index)));
                }
            }
        }
        let value = text::parse_value(trimmed)?;
        Some(self.curve.range().clamp(value))
    }

    /// Which tabulated step a plain value falls on.
    fn step_index(&self, plain: f64) -> usize {
        let steps = self.info.step_count;
        if steps == 0 {
            return 0;
        }
        let normalized = self.curve.to_normalized(plain);
        (normalized * f64::from(steps)).round().max(0.0) as usize
    }

    /// The normalised position of a tabulated step.
    fn step_normalized(&self, index: usize) -> f64 {
        let steps = self.info.step_count;
        if steps == 0 {
            return 0.0;
        }
        index as f64 / f64::from(steps)
    }
}

/// Every parameter of one plug-in, in the order the plug-in declared them.
///
/// Built once during `IPluginBase::initialize` and immutable afterwards apart from the
/// per-entry atomics. VST3 addresses parameters by *index* in `getParameterInfo` and by *id*
/// everywhere else, so both lookups are here and both are `O(log n)` or better.
#[derive(Debug, Default)]
pub struct ParamTable {
    entries: Vec<ParamEntry>,
    /// `(vst3_id, index)` sorted by id, for `find`.
    by_id: Vec<(u32, u32)>,
    /// Parameters dropped because another one already claimed their id.
    duplicates: usize,
}

impl ParamTable {
    /// `[main-thread]` Captures a plug-in's parameters.
    ///
    /// Probing a curve means asking the parameter what plain value a normalised position
    /// maps to, which temporarily *sets* it; the original value is restored before the entry
    /// is stored. That is safe because [`Param::set_plain`](daux_plugin_api::Param::set_plain)
    /// is contractually a plain atomic store with no side effects, and it happens once, on
    /// the main thread, before the plug-in has been activated.
    ///
    /// Duplicate ids are dropped rather than allowed to shadow each other: VST3 addresses
    /// parameters by id, so two parameters sharing one would make automation ambiguous.
    #[must_use]
    pub fn build(params: &dyn Params) -> Self {
        let refs = params.param_refs();
        let mut entries: Vec<ParamEntry> = Vec::with_capacity(refs.len());
        let mut by_id: Vec<(u32, u32)> = Vec::with_capacity(refs.len());
        let mut duplicates = 0;

        for (id, param) in refs {
            if by_id.iter().any(|&(existing, _)| existing == id.get()) {
                duplicates += 1;
                continue;
            }
            let info = param.info();
            let saved = param.plain();
            let curve = Curve::probe(&info, |normalized| {
                param.set_normalized(normalized);
                param.plain()
            });
            let step_texts = capture_step_texts(param, &info, &curve);
            param.set_plain(saved);

            let index = u32::try_from(entries.len()).unwrap_or(u32::MAX);
            by_id.push((id.get(), index));
            entries.push(ParamEntry {
                id,
                default_normalized: curve.to_normalized(info.default),
                value: AtomicF64::new(curve.to_normalized(saved)),
                curve,
                step_texts,
                info,
            });
        }

        by_id.sort_unstable_by_key(|&(id, _)| id);
        Self {
            entries,
            by_id,
            duplicates,
        }
    }

    /// `[any-thread]` How many parameters the host will see.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `[any-thread]` `true` when the plug-in has no parameters.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// `[main-thread]` How many parameters were dropped for sharing an id.
    #[inline]
    #[must_use]
    pub const fn duplicates(&self) -> usize {
        self.duplicates
    }

    /// `[any-thread]` The parameter at a VST3 index.
    #[inline]
    #[must_use]
    pub fn at(&self, index: usize) -> Option<&ParamEntry> {
        self.entries.get(index)
    }

    /// `[audio-thread]` The parameter with a VST3 id.
    ///
    /// A binary search over a sorted array: no allocation, no hashing, bounded time — which
    /// matters because automation lookup happens per parameter per block.
    #[inline]
    #[must_use]
    pub fn find(&self, vst3_id: u32) -> Option<&ParamEntry> {
        let position = self
            .by_id
            .binary_search_by_key(&vst3_id, |&(id, _)| id)
            .ok()?;
        let (_, index) = self.by_id[position];
        self.entries.get(index as usize)
    }

    /// `[any-thread]` Every parameter, in declaration order.
    #[inline]
    #[must_use]
    pub fn entries(&self) -> &[ParamEntry] {
        &self.entries
    }

    /// `[main-thread]` Copies the plug-in's current values into the mirror.
    ///
    /// Called after anything that can change parameters behind the controller's back —
    /// loading state, most importantly — so that the host's next `getParamNormalized` sees
    /// what the plug-in actually holds.
    pub fn refresh_from(&self, params: &dyn Params) {
        for entry in &self.entries {
            if let Some(param) = params.param(entry.id) {
                entry.value.set(param.normalized());
            }
        }
    }

    /// `[main-thread]` Copies the mirror into the plug-in's parameters.
    ///
    /// Used when the host restores *controller* state, which VST3 delivers to the controller
    /// half only; without this the DSP would keep running with the previous values.
    pub fn apply_to(&self, params: &dyn Params) {
        for entry in &self.entries {
            if entry.is_read_only() {
                continue;
            }
            if let Some(param) = params.param(entry.id) {
                param.set_normalized(entry.normalized());
            }
        }
    }
}

/// Captures the text of every step of a small discrete parameter.
fn capture_step_texts(
    param: &dyn daux_plugin_api::Param,
    info: &ParamInfo,
    curve: &Curve,
) -> Option<Vec<String>> {
    let steps = info.step_count;
    if steps == 0 || steps > MAX_TABULATED_STEPS {
        return None;
    }
    let mut texts = Vec::with_capacity(steps as usize + 1);
    let mut scratch = String::with_capacity(32);
    for step in 0..=steps {
        let normalized = f64::from(step) / f64::from(steps);
        param.to_text(curve.to_plain(normalized), &mut scratch);
        texts.push(scratch.clone());
    }
    Some(texts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use daux_plugin_api::{
        BoolParam, EnumParam, FloatParam, IntParam, MeterParam, Param, ParamEnum, ParamRange,
    };

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Shape {
        Sine,
        Saw,
        Square,
    }

    impl ParamEnum for Shape {
        const VARIANTS: &'static [Self] = &[Shape::Sine, Shape::Saw, Shape::Square];
        fn name(self) -> &'static str {
            match self {
                Shape::Sine => "Sine",
                Shape::Saw => "Saw",
                Shape::Square => "Square",
            }
        }
        fn index(self) -> u32 {
            match self {
                Shape::Sine => 0,
                Shape::Saw => 1,
                Shape::Square => 2,
            }
        }
        fn from_index(i: u32) -> Option<Self> {
            Self::VARIANTS.get(i as usize).copied()
        }
    }

    struct Bank {
        gain: FloatParam,
        cutoff: FloatParam,
        voices: IntParam,
        invert: BoolParam,
        shape: EnumParam<Shape>,
        meter: MeterParam,
    }

    impl Default for Bank {
        fn default() -> Self {
            Self {
                gain: FloatParam::new(ParamId(1), "Gain", -6.0, ParamRange::linear(-60.0, 12.0))
                    .with_unit("dB"),
                cutoff: FloatParam::new(
                    ParamId(2),
                    "Cutoff",
                    1_000.0,
                    ParamRange::logarithmic(20.0, 20_000.0),
                )
                .with_unit("Hz"),
                voices: IntParam::new(ParamId(3), "Voices", 8, 1, 16),
                invert: BoolParam::new(ParamId(4), "Invert", false),
                shape: EnumParam::new(ParamId(5), "Shape", Shape::Saw),
                meter: MeterParam::new(ParamId(6), "Output", ParamRange::linear(-60.0, 0.0)),
            }
        }
    }

    impl Params for Bank {
        fn param_refs(&self) -> Vec<(ParamId, &dyn Param)> {
            vec![
                (ParamId(1), &self.gain),
                (ParamId(2), &self.cutoff),
                (ParamId(3), &self.voices),
                (ParamId(4), &self.invert),
                (ParamId(5), &self.shape),
                (ParamId(6), &self.meter),
            ]
        }
    }

    #[test]
    fn building_the_mirror_leaves_every_parameter_where_it_found_it() {
        let bank = Bank::default();
        let before: Vec<f64> = bank.param_refs().iter().map(|(_, p)| p.plain()).collect();
        let table = ParamTable::build(&bank);
        let after: Vec<f64> = bank.param_refs().iter().map(|(_, p)| p.plain()).collect();
        assert_eq!(
            before, after,
            "probing curves must not disturb the plug-in's values"
        );
        assert_eq!(table.len(), 6);
        assert_eq!(table.duplicates(), 0);
    }

    #[test]
    fn the_mirror_starts_holding_what_the_plug_in_holds() {
        let bank = Bank::default();
        let table = ParamTable::build(&bank);
        for (id, param) in bank.param_refs() {
            let entry = table.find(id.get()).expect("every parameter is mirrored");
            assert!(
                (entry.normalized() - param.normalized()).abs() < 1e-12,
                "`{}` starts at the wrong value",
                entry.info.name
            );
            assert!(
                (entry.plain() - param.plain()).abs() <= 1e-9 * param.plain().abs().max(1.0),
                "`{}` round-trips to the wrong plain value",
                entry.info.name
            );
        }
    }

    #[test]
    fn ids_are_the_daux_ids_verbatim_and_never_renumbered() {
        let table = ParamTable::build(&Bank::default());
        for (index, entry) in table.entries().iter().enumerate() {
            assert_eq!(entry.vst3_id(), entry.id.get());
            assert_eq!(
                table.at(index).map(ParamEntry::vst3_id),
                Some(entry.vst3_id())
            );
            assert!(std::ptr::eq(
                table.find(entry.vst3_id()).unwrap(),
                table.at(index).unwrap()
            ));
        }
        assert_eq!(table.at(0).unwrap().vst3_id(), 1);
        assert_eq!(table.at(5).unwrap().vst3_id(), 6);
        assert!(table.find(999).is_none());
    }

    #[test]
    fn a_logarithmic_parameter_converts_through_its_own_curve() {
        let table = ParamTable::build(&Bank::default());
        let cutoff = table.find(2).unwrap();
        // The geometric mean, not the arithmetic one.
        assert!((cutoff.curve.to_plain(0.5) - 632.455_532_033_675_9).abs() < 1e-9);
        assert!((cutoff.curve.to_normalized(632.455_532_033_675_9) - 0.5).abs() < 1e-12);
        // …and a full round-trip of an automation value is lossless.
        for i in 0..=100 {
            let n = f64::from(i) / 100.0;
            let back = cutoff.curve.to_normalized(cutoff.curve.to_plain(n));
            assert!((back - n).abs() < 1e-9, "automation drifted at {n}");
        }
    }

    #[test]
    fn discrete_parameters_keep_the_plug_ins_own_words() {
        let table = ParamTable::build(&Bank::default());
        let mut out = String::new();

        let shape = table.find(5).unwrap();
        shape.format(0.0, &mut out);
        assert_eq!(out, "Sine");
        shape.format(1.0, &mut out);
        assert_eq!(out, "Saw");
        shape.format(2.0, &mut out);
        assert_eq!(out, "Square");
        assert_eq!(shape.parse("square"), Some(2.0));
        assert_eq!(shape.parse("  Sine "), Some(0.0));

        let invert = table.find(4).unwrap();
        invert.format(0.0, &mut out);
        assert_eq!(out, "Off");
        invert.format(1.0, &mut out);
        assert_eq!(out, "On");
        assert_eq!(invert.parse("on"), Some(1.0));

        let voices = table.find(3).unwrap();
        voices.format(8.0, &mut out);
        assert_eq!(out, "8");
        assert_eq!(voices.parse("12"), Some(12.0));
    }

    #[test]
    fn continuous_parameters_render_like_the_default_formatter() {
        let bank = Bank::default();
        let table = ParamTable::build(&bank);
        let gain = table.find(1).unwrap();
        let mut mirrored = String::new();
        let mut live = String::new();
        for plain in [-60.0, -6.0, 0.0, 11.5, 12.0] {
            gain.format(plain, &mut mirrored);
            bank.gain.to_text(plain, &mut live);
            assert_eq!(
                mirrored, live,
                "the mirror must match the plug-in at {plain}"
            );
        }
        assert_eq!(gain.parse("-6 dB"), Some(-6.0));
        assert_eq!(gain.parse("nonsense"), None);
        // Out-of-range entry is clamped rather than accepted.
        assert_eq!(gain.parse("100"), Some(12.0));
    }

    #[test]
    fn a_meter_is_read_only_and_never_automatable() {
        let table = ParamTable::build(&Bank::default());
        let meter = table.find(6).unwrap();
        assert!(meter.is_read_only());
        assert!(!table.find(1).unwrap().is_read_only());
    }

    #[test]
    fn setting_a_value_snaps_it_onto_the_parameters_grid() {
        let table = ParamTable::build(&Bank::default());
        let voices = table.find(3).unwrap();
        // 16 values, so each step is 1/15 of the travel.
        voices.set_normalized(0.5);
        assert_eq!(voices.plain(), 9.0);
        voices.set_normalized(-3.0);
        assert_eq!(voices.plain(), 1.0);
        voices.set_normalized(f64::NAN);
        assert!(voices.plain().is_finite());
        voices.set_normalized(17.0);
        assert_eq!(voices.plain(), 16.0);
    }

    #[test]
    fn the_mirror_and_the_plug_in_can_be_pushed_at_each_other() {
        let bank = Bank::default();
        let table = ParamTable::build(&bank);

        // Plug-in → mirror, which is what happens after loading state.
        bank.gain.set_plain(3.0);
        bank.cutoff.set_plain(440.0);
        table.refresh_from(&bank);
        assert!((table.find(1).unwrap().plain() - 3.0).abs() < 1e-9);
        assert!((table.find(2).unwrap().plain() - 440.0).abs() < 1e-6);

        // Mirror → plug-in, which is what happens after the controller's own setState.
        table.find(1).unwrap().set_normalized(1.0);
        table.find(6).unwrap().set_normalized(1.0);
        table.apply_to(&bank);
        assert_eq!(bank.gain.plain(), 12.0);
        assert_eq!(
            bank.meter.plain(),
            -60.0,
            "a read-only meter must not be written by the host's state"
        );
    }

    #[test]
    fn duplicate_ids_are_dropped_rather_than_allowed_to_shadow_each_other() {
        struct Broken {
            a: FloatParam,
            b: FloatParam,
        }
        impl Params for Broken {
            fn param_refs(&self) -> Vec<(ParamId, &dyn Param)> {
                vec![(ParamId(7), &self.a), (ParamId(7), &self.b)]
            }
        }
        let broken = Broken {
            a: FloatParam::new(ParamId(7), "First", 0.0, ParamRange::UNIT),
            b: FloatParam::new(ParamId(7), "Second", 0.0, ParamRange::UNIT),
        };
        let table = ParamTable::build(&broken);
        assert_eq!(table.len(), 1);
        assert_eq!(table.duplicates(), 1);
        assert_eq!(table.at(0).unwrap().info.name, "First");
    }

    #[test]
    fn a_plug_in_with_no_parameters_produces_an_empty_table() {
        struct None_;
        impl Params for None_ {
            fn param_refs(&self) -> Vec<(ParamId, &dyn Param)> {
                Vec::new()
            }
        }
        let table = ParamTable::build(&None_);
        assert!(table.is_empty());
        assert_eq!(table.len(), 0);
        assert!(table.at(0).is_none());
        assert!(table.find(0).is_none());
    }

    #[test]
    fn a_huge_discrete_parameter_is_not_tabulated() {
        struct Huge(IntParam);
        impl Params for Huge {
            fn param_refs(&self) -> Vec<(ParamId, &dyn Param)> {
                vec![(ParamId(9), &self.0)]
            }
        }
        let huge = Huge(IntParam::new(ParamId(9), "Samples", 0, 0, 100_000));
        let table = ParamTable::build(&huge);
        let entry = table.at(0).unwrap();
        assert!(
            entry.step_texts.is_none(),
            "capturing 100 001 strings during initialize is not acceptable"
        );
        // It still formats and parses, just through the generic path.
        let mut out = String::new();
        entry.format(1234.0, &mut out);
        assert_eq!(out, "1234.00");
        assert_eq!(entry.parse("1234"), Some(1234.0));
    }
}
