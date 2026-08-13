//! Translating between the DAUx model and VST3's vocabulary.
//!
//! Nothing in this module touches a COM pointer; it is pure data translation, which is what
//! makes it testable without a host. The four translations that matter are:
//!
//! * **Parameter values.** VST3 automation is *normalised* to `0..=1`; DAUx parameters are
//!   *plain* (dB, Hz, an enum index). See [`Curve`].
//! * **Categories and features** → VST3's `|`-separated subcategory string.
//! * **Channel layouts** → VST3 speaker arrangements.
//! * **Transport** → `Vst::ProcessContext`, and back.

use daux_plugin_api::{
    Capabilities, Category, ChannelLayout, ParamFlags, ParamInfo, ParamRange, PluginDescriptor,
    ProcessMode, TimeSignature, Transport, TransportBuilder,
};

use crate::api::{context_state, param_flags, process_mode};

// ---------------------------------------------------------------------------------------
// Parameter curves: the plain ↔ normalised boundary
// ---------------------------------------------------------------------------------------

/// The mapping between a parameter's plain value and VST3's normalised `0..=1` position.
///
/// # Why this type exists at all
///
/// This is the single most dangerous conversion in a VST3 adapter. A host records automation
/// as a normalised number and hands it back years later; if the plug-in reconstructs a
/// different plain value from it than the one the user set, the automation is silently wrong
/// — the parameter still moves, it just moves to the wrong place. A frequency knob that maps
/// `0.5` to 10 010 Hz on a linear curve instead of 632 Hz on a logarithmic one is the classic
/// case, and it is invisible until someone listens.
///
/// So the conversion must go through the parameter's *own* curve, never through a generic
/// `min + n * (max - min)`.
///
/// # Why it is reconstructed rather than read
///
/// [`daux_plugin_api::Param`] exposes `normalized`/`set_normalized`, which use the real
/// curve, but not the [`ParamRange`] behind them — and reaching the live `&dyn Param` from
/// VST3's controller thread while the audio thread is inside `process` would alias a `&mut`.
/// [`Curve::probe`] therefore identifies the curve once, on the main thread, from a handful
/// of exact evaluations, and the result is used from then on. The five [`ParamRange`] shapes
/// are all determined by their endpoints and their midpoint, so the identification is exact
/// rather than approximate — see the tests.
///
/// `[any-thread]` once built.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Curve {
    range: ParamRange,
}

/// How close a probe has to be for a curve family to be accepted.
///
/// Relative rather than absolute, because a range may span 20 Hz or 20 000 Hz.
const PROBE_TOLERANCE: f64 = 1e-9;

impl Curve {
    /// `[any-thread]` A curve from a known [`ParamRange`].
    #[must_use]
    pub const fn from_range(range: ParamRange) -> Self {
        Self { range }
    }

    /// `[any-thread]` The range this curve maps through.
    #[must_use]
    pub const fn range(self) -> ParamRange {
        self.range
    }

    /// `[main-thread]` Identifies a parameter's curve by evaluating its own mapping.
    ///
    /// `denormalize` must be the parameter's real normalised → plain mapping. The probe is
    /// exact for every [`ParamRange`] variant; anything else — a hand-written [`Param`] with
    /// a curve of its own — falls back to [`ParamRange::Linear`] over the same endpoints,
    /// which is the mapping a host would have assumed anyway.
    ///
    /// [`Param`]: daux_plugin_api::Param
    #[must_use]
    pub fn probe(info: &ParamInfo, denormalize: impl Fn(f64) -> f64) -> Self {
        // Quantised parameters are fully described by `step_count`; no probing needed, and
        // probing would be wrong because the mapping is a staircase.
        if info.step_count == 1 && info.min == 0.0 && info.max == 1.0 {
            return Self::from_range(ParamRange::Boolean);
        }
        if info.step_count > 0 {
            let min = info.min.round() as i64;
            let max = min.saturating_add(i64::from(info.step_count));
            return Self::from_range(ParamRange::Stepped { min, max });
        }

        let lo = denormalize(0.0);
        let hi = denormalize(1.0);
        if !lo.is_finite() || !hi.is_finite() || lo == hi {
            return Self::from_range(ParamRange::Linear {
                min: info.min,
                max: info.max,
            });
        }
        let mid = denormalize(0.5);

        for candidate in [
            Some(ParamRange::Linear { min: lo, max: hi }),
            Some(ParamRange::Logarithmic { min: lo, max: hi }),
            skewed_through(lo, hi, mid),
        ]
        .into_iter()
        .flatten()
        {
            if candidate.validate().is_ok() && matches(&candidate, &denormalize) {
                return Self::from_range(candidate);
            }
        }

        Self::from_range(ParamRange::Linear { min: lo, max: hi })
    }

    /// `[any-thread]` Plain value for a normalised position. Never `NaN`.
    #[must_use]
    pub fn to_plain(self, normalized: f64) -> f64 {
        self.range.denormalize(normalized)
    }

    /// `[any-thread]` Normalised position of a plain value. Always within `0..=1`.
    #[must_use]
    pub fn to_normalized(self, plain: f64) -> f64 {
        self.range.normalize(plain)
    }

    /// `[any-thread]` Snaps a normalised position onto the nearest representable one.
    #[must_use]
    pub fn snap(self, normalized: f64) -> f64 {
        self.range.snap_normalized(normalized)
    }
}

/// The skewed range whose midpoint is `mid`, if one exists.
fn skewed_through(lo: f64, hi: f64, mid: f64) -> Option<ParamRange> {
    let t = (mid - lo) / (hi - lo);
    if !(t > 0.0 && t < 1.0) {
        return None;
    }
    // `denormalize(0.5) = lo + 0.5^(1/factor) * (hi - lo)`, so `factor = ln(0.5) / ln(t)`.
    let factor = std::f64::consts::LN_2.copysign(-1.0) / t.ln();
    factor.is_finite().then_some(ParamRange::Skewed {
        min: lo,
        max: hi,
        factor,
    })
}

/// Whether `candidate` reproduces `denormalize` everywhere that matters.
fn matches(candidate: &ParamRange, denormalize: &impl Fn(f64) -> f64) -> bool {
    const POSITIONS: [f64; 9] = [0.0, 0.05, 0.125, 0.25, 0.5, 0.6, 0.75, 0.95, 1.0];
    POSITIONS.iter().all(|&n| {
        let want = denormalize(n);
        let got = candidate.denormalize(n);
        let scale = want.abs().max(got.abs()).max(1.0);
        (want - got).abs() <= PROBE_TOLERANCE * scale
    })
}

/// `[any-thread]` DAUx parameter flags as VST3 `ParameterInfo::flags`.
///
/// `MODULATABLE`, `PER_NOTE` and `REQUIRES_PROCESS` have no VST3 equivalent in the core
/// interfaces and are simply not represented; per-note modulation reaches a VST3 plug-in
/// through note expression instead.
#[must_use]
pub fn parameter_flags(flags: ParamFlags, stepped: bool) -> i32 {
    let mut out = param_flags::NO_FLAGS;
    if flags.is_automatable() && !flags.contains(ParamFlags::IS_METER) {
        out |= param_flags::CAN_AUTOMATE;
    }
    if flags.is_read_only() || flags.contains(ParamFlags::IS_METER) {
        out |= param_flags::IS_READ_ONLY;
    }
    if flags.contains(ParamFlags::HIDDEN) {
        out |= param_flags::IS_HIDDEN;
    }
    if flags.contains(ParamFlags::BYPASS) {
        out |= param_flags::IS_BYPASS;
        // A bypass a host cannot automate is a bypass a user cannot record.
        out |= param_flags::CAN_AUTOMATE;
    }
    if stepped {
        out |= param_flags::IS_LIST;
    }
    out
}

// ---------------------------------------------------------------------------------------
// Descriptor
// ---------------------------------------------------------------------------------------

/// `[main-thread]` The VST3 subcategory string for a descriptor, e.g. `"Fx|Filter|Stereo"`.
///
/// The first element is the one hosts sort by, so it is always the category; the plug-in's
/// own feature tags follow in declaration order. Tags are passed through with `|` stripped,
/// since it is the separator.
#[must_use]
pub fn subcategories(descriptor: &PluginDescriptor) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(descriptor.features.len() + 2);
    parts.push(primary_subcategory(descriptor).to_owned());

    if descriptor.capabilities.contains(Capabilities::STEREO_ONLY) {
        parts.push("Stereo".to_owned());
    }
    for feature in &descriptor.features {
        let cleaned: String = feature.chars().filter(|&c| c != '|').collect();
        let trimmed = cleaned.trim();
        if !trimmed.is_empty() && !parts.iter().any(|p| p.eq_ignore_ascii_case(trimmed)) {
            parts.push(trimmed.to_owned());
        }
    }
    parts.join("|")
}

/// The VST3 subcategory a host sorts by.
fn primary_subcategory(descriptor: &PluginDescriptor) -> &'static str {
    match descriptor.category {
        Category::Instrument => "Instrument",
        // VST3 has no MIDI-effect class. Hosts that see "Instrument" put the plug-in on an
        // instrument track, which is where a note transformer has to live to receive notes.
        Category::MidiEffect => "Instrument",
        Category::Analyzer => "Analyzer",
        Category::Generator => "Generator",
        Category::Utility => "Tools",
        // `Category` is `#[non_exhaustive]`: a category added in a later DAUx release must
        // land on the neutral subcategory rather than fail to compile an adapter that
        // predates it.
        Category::Effect | Category::Unknown | _ => "Fx",
    }
}

// ---------------------------------------------------------------------------------------
// Speaker arrangements
// ---------------------------------------------------------------------------------------

/// Individual speakers, from `vstspeaker.h`.
mod speaker {
    /// Front left.
    pub const L: u64 = 1 << 0;
    /// Front right.
    pub const R: u64 = 1 << 1;
    /// Front centre.
    pub const C: u64 = 1 << 2;
    /// Low-frequency effects.
    pub const LFE: u64 = 1 << 3;
    /// Left surround.
    pub const LS: u64 = 1 << 4;
    /// Right surround.
    pub const RS: u64 = 1 << 5;
    /// Left side.
    pub const SL: u64 = 1 << 9;
    /// Right side.
    pub const SR: u64 = 1 << 10;
    /// Top front left.
    pub const TFL: u64 = 1 << 12;
    /// Top front right.
    pub const TFR: u64 = 1 << 14;
    /// Top rear left.
    pub const TRL: u64 = 1 << 15;
    /// Top rear right.
    pub const TRR: u64 = 1 << 17;
    /// The single speaker of a mono bus.
    pub const M: u64 = 1 << 19;
}

/// `Vst::SpeakerArr::kEmpty`.
pub const SPEAKER_ARR_EMPTY: u64 = 0;

/// `[main-thread]` The VST3 speaker arrangement for a DAUx channel layout.
///
/// Layouts VST3 has no name for — ambisonics, `Discrete`, `Custom` — become the low `n` bits,
/// which hosts read as "`n` unnamed channels". [`crate::compat`] reports that as a warning at
/// build time rather than leaving it to be discovered in a session.
#[must_use]
pub fn speaker_arrangement(layout: ChannelLayout) -> u64 {
    use speaker as s;
    match layout {
        ChannelLayout::Mono => s::M,
        ChannelLayout::Stereo => s::L | s::R,
        ChannelLayout::LRC => s::L | s::R | s::C,
        ChannelLayout::Quad => s::L | s::R | s::LS | s::RS,
        ChannelLayout::Surround2_1 => s::L | s::R | s::LFE,
        ChannelLayout::Surround5_1 => s::L | s::R | s::C | s::LFE | s::LS | s::RS,
        ChannelLayout::Surround7_1 => s::L | s::R | s::C | s::LFE | s::LS | s::RS | s::SL | s::SR,
        ChannelLayout::Atmos7_1_4 => {
            s::L | s::R
                | s::C
                | s::LFE
                | s::LS
                | s::RS
                | s::SL
                | s::SR
                | s::TFL
                | s::TFR
                | s::TRL
                | s::TRR
        }
        other => discrete_arrangement(other.channel_count()),
    }
}

/// An arrangement of `channels` unnamed channels.
///
/// Capped at 64 because a `SpeakerArrangement` is 64 bits wide; a bus wider than that cannot
/// be described in VST3 at all and is reported as empty so the host refuses it cleanly rather
/// than truncating it silently.
#[must_use]
pub fn discrete_arrangement(channels: u16) -> u64 {
    match channels {
        0 => SPEAKER_ARR_EMPTY,
        1..=63 => (1u64 << channels) - 1,
        64 => u64::MAX,
        _ => SPEAKER_ARR_EMPTY,
    }
}

/// `[main-thread]` How many channels an arrangement describes.
#[must_use]
pub fn arrangement_channel_count(arrangement: u64) -> u16 {
    arrangement.count_ones() as u16
}

// ---------------------------------------------------------------------------------------
// Process mode and transport
// ---------------------------------------------------------------------------------------

/// `[any-thread]` VST3's process mode as DAUx's.
///
/// VST3 has no "analysis" mode; an unknown value is read as real-time, which is the strictest
/// contract and therefore the safe guess.
#[must_use]
pub fn process_mode_from_vst3(mode: i32) -> ProcessMode {
    match mode {
        process_mode::PREFETCH => ProcessMode::Prefetch,
        process_mode::OFFLINE => ProcessMode::Offline,
        _ => ProcessMode::Realtime,
    }
}

/// `[audio-thread]` A DAUx transport built from VST3's `ProcessContext`.
///
/// Every VST3 field is guarded by a validity bit, and DAUx's accessors return `Option`
/// precisely so a plug-in cannot read a field the host never set — so the two models line up
/// exactly, one flag at a time. Allocation-free.
#[must_use]
pub fn transport_from_context(ctx: &crate::api::ProcessContext) -> Transport {
    let mut builder = TransportBuilder::new()
        .playing(ctx.state & context_state::PLAYING != 0)
        .recording(ctx.state & context_state::RECORDING != 0)
        .sample_position(ctx.project_time_samples);

    if ctx.state & context_state::TEMPO_VALID != 0 {
        builder = builder.tempo(ctx.tempo);
    }
    if ctx.state & context_state::PROJECT_TIME_MUSIC_VALID != 0 {
        builder = builder.beats(ctx.project_time_music);
        if ctx.sample_rate > 0.0 {
            builder = builder.seconds(ctx.project_time_samples as f64 / ctx.sample_rate);
        }
    }
    if ctx.state & context_state::BAR_POSITION_VALID != 0 {
        // VST3 reports the bar's musical position but not its number; DAUx keeps the number
        // for display only, and `0` is the honest answer when the host did not say.
        builder = builder.bar(0, ctx.bar_position_music);
    }
    if ctx.state & context_state::TIME_SIG_VALID != 0 {
        let numerator = u16::try_from(ctx.time_sig_numerator.max(0)).unwrap_or(4);
        let denominator = u16::try_from(ctx.time_sig_denominator.max(0)).unwrap_or(4);
        if TimeSignature::try_new(numerator, denominator).is_some() {
            builder = builder.time_signature(numerator, denominator);
        }
    }
    if ctx.state & context_state::CYCLE_VALID != 0 {
        builder = builder
            .loop_beats(ctx.cycle_start_music, ctx.cycle_end_music)
            .looping(ctx.state & context_state::CYCLE_ACTIVE != 0);
    }
    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use daux_plugin_api::{BoolParam, EnumParam, FloatParam, IntParam, Param, ParamEnum, ParamId};

    /// The reconstructed range must be the *same shape* as the original. A skew factor is
    /// recovered through two logarithms, so it lands within a few ULPs rather than on the
    /// exact bit pattern; the mapping it produces is still exact to 1e-9, which is what
    /// `assert_probe_is_exact` goes on to check at a thousand points.
    fn assert_same_family(got: ParamRange, want: ParamRange, name: &str) {
        match (got, want) {
            (
                ParamRange::Skewed {
                    min: a_min,
                    max: a_max,
                    factor: a,
                },
                ParamRange::Skewed {
                    min: b_min,
                    max: b_max,
                    factor: b,
                },
            ) => {
                assert_eq!((a_min, a_max), (b_min, b_max), "bounds for `{name}`");
                assert!(
                    (a - b).abs() <= 1e-9 * b.abs(),
                    "factor for `{name}`: {a} vs {b}"
                );
            }
            (a, b) => assert_eq!(a, b, "wrong family for `{name}`"),
        }
    }

    /// Probing must reconstruct the curve *exactly*, because an approximation is the bug
    /// this whole module exists to prevent.
    fn assert_probe_is_exact(param: &dyn Param, expected: ParamRange) {
        let info = param.info();
        let original = param.plain();
        let curve = Curve::probe(&info, |n| {
            param.set_normalized(n);
            param.plain()
        });
        param.set_plain(original);

        assert_same_family(curve.range(), expected, &info.name);
        for i in 0..=1000 {
            let n = f64::from(i) / 1000.0;
            param.set_normalized(n);
            let want = param.plain();
            let got = curve.to_plain(n);
            assert!(
                (want - got).abs() <= 1e-9 * want.abs().max(1.0),
                "`{}` at {n}: plug-in says {want}, reconstruction says {got}",
                info.name
            );
            // …and back again, which is what a host round-trips through automation.
            let back = curve.to_normalized(got);
            assert!(
                (back - curve.snap(n)).abs() <= 1e-9,
                "`{}` at {n}: renormalised to {back}",
                info.name
            );
        }
        param.set_plain(original);
    }

    #[test]
    fn a_linear_curve_is_reconstructed_exactly() {
        let p = FloatParam::new(
            ParamId(1),
            "Gain",
            0.0,
            ParamRange::Linear {
                min: -60.0,
                max: 12.0,
            },
        );
        assert_probe_is_exact(
            &p,
            ParamRange::Linear {
                min: -60.0,
                max: 12.0,
            },
        );
    }

    #[test]
    fn a_logarithmic_curve_is_reconstructed_exactly() {
        let p = FloatParam::new(
            ParamId(2),
            "Cutoff",
            1000.0,
            ParamRange::logarithmic(20.0, 20_000.0),
        );
        assert_probe_is_exact(&p, ParamRange::logarithmic(20.0, 20_000.0));

        // The number that makes the difference visible: half a knob's travel is the
        // geometric mean, not the arithmetic one.
        let curve = Curve::from_range(ParamRange::logarithmic(20.0, 20_000.0));
        assert!((curve.to_plain(0.5) - 632.455_532_033_675_9).abs() < 1e-9);
        assert!(
            (curve.to_plain(0.5) - 10_010.0).abs() > 1000.0,
            "a linear reading of the same automation would be catastrophically wrong"
        );
    }

    #[test]
    fn a_skewed_curve_is_reconstructed_exactly_for_both_directions() {
        for factor in [0.25, 0.5, 2.0, 3.0] {
            let p = FloatParam::new(
                ParamId(3),
                "Skewed",
                0.0,
                ParamRange::skewed(0.0, 100.0, factor),
            );
            let info = p.info();
            let curve = Curve::probe(&info, |n| {
                p.set_normalized(n);
                p.plain()
            });
            let ParamRange::Skewed { factor: got, .. } = curve.range() else {
                panic!("expected a skewed range, got {:?}", curve.range());
            };
            assert!(
                (got - factor).abs() < 1e-9,
                "factor {factor} reconstructed as {got}"
            );
            assert_probe_is_exact(&p, ParamRange::skewed(0.0, 100.0, factor));
        }
    }

    #[test]
    fn stepped_and_boolean_parameters_are_read_from_the_step_count() {
        let b = BoolParam::new(ParamId(4), "Invert", false);
        assert_probe_is_exact(&b, ParamRange::Boolean);

        let i = IntParam::new(ParamId(5), "Voices", 4, 1, 16);
        assert_probe_is_exact(&i, ParamRange::Stepped { min: 1, max: 16 });

        let negative = IntParam::new(ParamId(6), "Transpose", 0, -24, 24);
        assert_probe_is_exact(&negative, ParamRange::Stepped { min: -24, max: 24 });
    }

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Mode {
        Low,
        Band,
        High,
    }

    impl ParamEnum for Mode {
        const VARIANTS: &'static [Self] = &[Mode::Low, Mode::Band, Mode::High];
        fn name(self) -> &'static str {
            match self {
                Mode::Low => "Low",
                Mode::Band => "Band",
                Mode::High => "High",
            }
        }
        fn index(self) -> u32 {
            match self {
                Mode::Low => 0,
                Mode::Band => 1,
                Mode::High => 2,
            }
        }
        fn from_index(i: u32) -> Option<Self> {
            Self::VARIANTS.get(i as usize).copied()
        }
    }

    #[test]
    fn an_enum_parameter_keeps_every_step_addressable() {
        let e = EnumParam::new(ParamId(7), "Mode", Mode::Band);
        let info = e.info();
        let curve = Curve::probe(&info, |n| {
            e.set_normalized(n);
            e.plain()
        });
        assert_eq!(curve.range().step_count(), 2);
        for index in 0..3 {
            let n = f64::from(index) / 2.0;
            assert_eq!(curve.to_plain(n), f64::from(index));
            assert!((curve.to_normalized(f64::from(index)) - n).abs() < 1e-12);
        }
    }

    #[test]
    fn an_unrecognisable_curve_falls_back_to_linear_over_the_same_endpoints() {
        let info = ParamInfo::new(
            ParamId(8),
            "Weird",
            &ParamRange::linear(0.0, 1.0),
            0.0,
            ParamFlags::DEFAULT,
        );
        // A staircase that no `ParamRange` produces.
        let curve = Curve::probe(&info, |n| (n * 7.0).floor() / 7.0);
        assert_eq!(
            curve.range(),
            ParamRange::Linear { min: 0.0, max: 1.0 },
            "the fallback must still be invertible and cover the endpoints"
        );
        assert_eq!(curve.to_plain(0.0), 0.0);
        assert_eq!(curve.to_plain(1.0), 1.0);
    }

    #[test]
    fn a_degenerate_parameter_does_not_produce_a_broken_curve() {
        let info = ParamInfo::new(
            ParamId(9),
            "Constant",
            &ParamRange::linear(0.0, 1.0),
            0.5,
            ParamFlags::DEFAULT,
        );
        let curve = Curve::probe(&info, |_| 5.0);
        assert!(curve.to_plain(0.0).is_finite());
        assert!(curve.to_plain(1.0).is_finite());
        assert!(curve.to_normalized(f64::NAN).is_finite());
    }

    #[test]
    fn parameter_flags_translate_to_what_a_host_may_do() {
        assert_eq!(
            parameter_flags(ParamFlags::AUTOMATABLE, false),
            param_flags::CAN_AUTOMATE
        );
        assert_eq!(
            parameter_flags(ParamFlags::METER_DEFAULT, false),
            param_flags::IS_READ_ONLY,
            "a meter must never be automatable"
        );
        assert_eq!(
            parameter_flags(ParamFlags::AUTOMATABLE.with(ParamFlags::BYPASS), false),
            param_flags::CAN_AUTOMATE | param_flags::IS_BYPASS
        );
        assert_eq!(
            parameter_flags(ParamFlags::AUTOMATABLE.with(ParamFlags::HIDDEN), true),
            param_flags::CAN_AUTOMATE | param_flags::IS_HIDDEN | param_flags::IS_LIST
        );
        // Flags VST3 has no word for are dropped, not invented.
        assert_eq!(parameter_flags(ParamFlags::MODULATABLE, false), 0);
        assert_eq!(parameter_flags(ParamFlags::PER_NOTE, false), 0);
    }

    #[test]
    fn subcategories_lead_with_the_category() {
        let fx = PluginDescriptor::builder("com.example.fx", "Fx")
            .feature("Filter")
            .feature("Distortion")
            .build()
            .unwrap();
        assert_eq!(subcategories(&fx), "Fx|Filter|Distortion");

        let synth = PluginDescriptor::builder("com.example.synth", "Synth")
            .category(Category::Instrument)
            .capabilities(Capabilities::NONE.with_stereo_only())
            .feature("Synth")
            .build()
            .unwrap();
        assert_eq!(synth_subcategory(&synth), "Instrument|Stereo|Synth");

        let arp = PluginDescriptor::builder("com.example.arp", "Arp")
            .category(Category::MidiEffect)
            .build()
            .unwrap();
        assert_eq!(subcategories(&arp), "Instrument");
    }

    fn synth_subcategory(d: &PluginDescriptor) -> String {
        subcategories(d)
    }

    #[test]
    fn subcategories_survive_hostile_feature_tags() {
        let d = PluginDescriptor::builder("com.example.tags", "Tags")
            .feature("A|B")
            .feature("  spaced  ")
            .feature("Fx")
            .build()
            .unwrap();
        // `|` is the separator, so it cannot appear inside a tag; duplicates of the category
        // are dropped rather than repeated.
        assert_eq!(subcategories(&d), "Fx|AB|spaced");
    }

    #[test]
    fn speaker_arrangements_have_the_right_channel_counts() {
        for layout in [
            ChannelLayout::Mono,
            ChannelLayout::Stereo,
            ChannelLayout::LRC,
            ChannelLayout::Quad,
            ChannelLayout::Surround2_1,
            ChannelLayout::Surround5_1,
            ChannelLayout::Surround7_1,
            ChannelLayout::Atmos7_1_4,
            ChannelLayout::Ambisonic1st,
            ChannelLayout::Ambisonic2nd,
            ChannelLayout::Ambisonic3rd,
            ChannelLayout::Discrete(3),
            ChannelLayout::Discrete(32),
        ] {
            let arrangement = speaker_arrangement(layout);
            assert_eq!(
                arrangement_channel_count(arrangement),
                layout.channel_count(),
                "{layout:?} lost or gained a channel"
            );
        }
        // Mono is `kSpeakerM`, not `kSpeakerC`: a host that sees the centre speaker puts the
        // bus in the wrong place in a surround session.
        assert_eq!(speaker_arrangement(ChannelLayout::Mono), 1 << 19);
        assert_eq!(speaker_arrangement(ChannelLayout::Stereo), 0b11);
    }

    #[test]
    fn an_impossible_bus_width_is_refused_rather_than_truncated() {
        assert_eq!(discrete_arrangement(0), SPEAKER_ARR_EMPTY);
        assert_eq!(discrete_arrangement(1), 1);
        assert_eq!(discrete_arrangement(64), u64::MAX);
        assert_eq!(arrangement_channel_count(discrete_arrangement(64)), 64);
        assert_eq!(
            discrete_arrangement(65),
            SPEAKER_ARR_EMPTY,
            "a 65-channel bus cannot be described in 64 bits; saying so is better than lying"
        );
    }

    #[test]
    fn process_modes_default_to_the_strictest_contract() {
        assert_eq!(
            process_mode_from_vst3(process_mode::REALTIME),
            ProcessMode::Realtime
        );
        assert_eq!(
            process_mode_from_vst3(process_mode::OFFLINE),
            ProcessMode::Offline
        );
        assert_eq!(
            process_mode_from_vst3(process_mode::PREFETCH),
            ProcessMode::Prefetch
        );
        assert_eq!(process_mode_from_vst3(999), ProcessMode::Realtime);
        assert_eq!(process_mode_from_vst3(-1), ProcessMode::Realtime);
    }

    fn context() -> crate::api::ProcessContext {
        crate::api::ProcessContext {
            state: 0,
            sample_rate: 48_000.0,
            project_time_samples: 0,
            system_time: 0,
            continous_time_samples: 0,
            project_time_music: 0.0,
            bar_position_music: 0.0,
            cycle_start_music: 0.0,
            cycle_end_music: 0.0,
            tempo: 0.0,
            time_sig_numerator: 0,
            time_sig_denominator: 0,
            chord: crate::api::Chord::default(),
            smpte_offset_subframes: 0,
            frame_rate: crate::api::FrameRate::default(),
            samples_to_next_clock: 0,
        }
    }

    #[test]
    fn a_host_that_sets_no_validity_bits_produces_a_transport_that_knows_nothing() {
        let t = transport_from_context(&context());
        assert!(!t.is_playing());
        assert_eq!(t.tempo(), None, "an unset tempo must not become 0 BPM");
        assert_eq!(t.beats(), None);
        assert_eq!(t.time_signature(), None);
        assert_eq!(t.loop_range_beats(), None);
    }

    #[test]
    fn every_validity_bit_is_honoured_separately() {
        let mut ctx = context();
        ctx.state = context_state::PLAYING
            | context_state::TEMPO_VALID
            | context_state::PROJECT_TIME_MUSIC_VALID
            | context_state::TIME_SIG_VALID
            | context_state::CYCLE_VALID
            | context_state::CYCLE_ACTIVE;
        ctx.tempo = 128.0;
        ctx.project_time_music = 8.5;
        ctx.project_time_samples = 96_000;
        ctx.time_sig_numerator = 7;
        ctx.time_sig_denominator = 8;
        ctx.cycle_start_music = 4.0;
        ctx.cycle_end_music = 20.0;

        let t = transport_from_context(&ctx);
        assert!(t.is_playing());
        assert!(!t.is_recording());
        assert!(t.is_looping());
        assert_eq!(t.tempo(), Some(128.0));
        assert_eq!(t.beats(), Some(8.5));
        assert_eq!(t.seconds(), Some(2.0));
        assert_eq!(t.time_signature(), Some(TimeSignature::new(7, 8)));
        assert_eq!(t.loop_range_beats(), Some((4.0, 20.0)));
        assert_eq!(t.song_pos_samples, 96_000);
    }

    #[test]
    fn a_nonsense_time_signature_is_dropped_rather_than_passed_on() {
        let mut ctx = context();
        ctx.state = context_state::TIME_SIG_VALID;
        ctx.time_sig_numerator = 0;
        ctx.time_sig_denominator = 0;
        assert_eq!(transport_from_context(&ctx).time_signature(), None);

        ctx.time_sig_numerator = -4;
        ctx.time_sig_denominator = 4;
        assert_eq!(transport_from_context(&ctx).time_signature(), None);

        ctx.time_sig_numerator = 4;
        ctx.time_sig_denominator = 4;
        assert_eq!(
            transport_from_context(&ctx).time_signature(),
            Some(TimeSignature::COMMON)
        );
    }
}
