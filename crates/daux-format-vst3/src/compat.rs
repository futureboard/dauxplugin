//! What VST3 cannot say about a DAUx plug-in.
//!
//! A format adapter is a translation, and translations lose things. Rather than lose them
//! silently, [`compatibility_report`] enumerates every promise in a [`PluginDescriptor`] that
//! the VST3 export cannot carry, so `daux build` can print them next to the artefact it just
//! produced and the author finds out at build time rather than from a user.
//!
//! A warning is not an error. Every plug-in in this list still loads and runs; the report
//! says what a VST3 host will *not* know about it.

use daux_plugin_api::{Capabilities, Category, PluginDescriptor};

/// How badly a mapping is lost.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum WarningLevel {
    /// The plug-in behaves the same; only a hint or a label is missing.
    Note,
    /// A behaviour the plug-in declared will not happen in a VST3 host.
    Warning,
}

impl WarningLevel {
    /// `[main-thread]` A short, stable word for a build log.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            WarningLevel::Note => "note",
            WarningLevel::Warning => "warning",
        }
    }
}

/// One thing the VST3 export cannot express.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct CompatibilityWarning {
    /// How badly it is lost.
    pub level: WarningLevel,
    /// Stable machine-readable code, e.g. `"vst3.midi2"`. Safe to match on in tooling.
    pub code: &'static str,
    /// What is lost, in one sentence.
    pub message: String,
    /// What the author can do instead, when there is anything to do.
    pub advice: Option<String>,
}

impl CompatibilityWarning {
    /// `[main-thread]` Builds a warning.
    #[must_use]
    pub fn new(level: WarningLevel, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            level,
            code,
            message: message.into(),
            advice: None,
        }
    }

    /// `[main-thread]` Attaches a suggestion.
    #[must_use]
    pub fn with_advice(mut self, advice: impl Into<String>) -> Self {
        self.advice = Some(advice.into());
        self
    }
}

impl core::fmt::Display for CompatibilityWarning {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{} [{}]: {}",
            self.level.as_str(),
            self.code,
            self.message
        )?;
        if let Some(advice) = &self.advice {
            write!(f, " — {advice}")?;
        }
        Ok(())
    }
}

/// Longest string VST3 keeps in `PClassInfo::name`, minus the terminator.
const MAX_NAME: usize = 63;
/// Longest string VST3 keeps in `PClassInfo2::vendor` / `version`, minus the terminator.
const MAX_VENDOR: usize = 63;
/// Longest `PClassInfo2::subCategories`, minus the terminator.
const MAX_SUBCATEGORIES: usize = 127;
/// Longest `PFactoryInfo::url`, minus the terminator.
const MAX_URL: usize = 255;
/// Longest `PFactoryInfo::email`, minus the terminator.
const MAX_EMAIL: usize = 127;

/// `[main-thread]` Everything the VST3 export of `descriptor` cannot express.
///
/// Empty means the translation is lossless. The order is stable: capability losses first,
/// then category mapping, then string truncation, so a build log reads the same every time.
#[must_use]
pub fn compatibility_report(descriptor: &PluginDescriptor) -> Vec<CompatibilityWarning> {
    let caps = descriptor.capabilities;
    let mut out = Vec::new();

    if caps.is_midi2() {
        out.push(
            CompatibilityWarning::new(
                WarningLevel::Warning,
                "vst3.midi2",
                "VST3 has no MIDI 2.0 / UMP event; the plug-in will only ever receive MIDI 1.0",
            )
            .with_advice(
                "handle DauxEvent::Midi1 as well as Midi2, or export to CLAP or .axt for MIDI 2.0",
            ),
        );
    }
    if caps.is_shared_texture_gui() {
        out.push(
            CompatibilityWarning::new(
                WarningLevel::Warning,
                "vst3.shared-texture-gui",
                "VST3 has no shared-texture presentation; the editor is embedded as a native \
                 child window instead",
            )
            .with_advice("make sure the editor also supports PresentationMode::EmbeddedSurface"),
        );
    }
    if caps.is_requires_gui() {
        out.push(
            CompatibilityWarning::new(
                WarningLevel::Warning,
                "vst3.requires-gui",
                "VST3 cannot say that a plug-in is unusable without its editor; hosts will \
                 instantiate it headless and render with it",
            )
            .with_advice("produce correct audio without an editor, even if it is only silence"),
        );
    }
    if caps.is_hard_realtime() {
        out.push(
            CompatibilityWarning::new(
                WarningLevel::Warning,
                "vst3.hard-realtime",
                "VST3 cannot forbid offline rendering; a host may bounce this plug-in faster \
                 than real time",
            )
            .with_advice("detect ProcessMode::Offline in `process` and degrade explicitly"),
        );
    }
    if caps.is_dynamic_buses() {
        out.push(
            CompatibilityWarning::new(
                WarningLevel::Warning,
                "vst3.dynamic-buses",
                "VST3 fixes the bus topology at initialize; hosts negotiate speaker \
                 arrangements but never add or remove buses",
            )
            .with_advice(
                "declare every bus up front and mark the optional ones BusFlags::OPTIONAL",
            ),
        );
    }
    if caps.is_sandbox_safe() {
        out.push(CompatibilityWarning::new(
            WarningLevel::Note,
            "vst3.sandbox-safe",
            "VST3 has no sandbox-safety declaration; the host decides on its own whether to \
             isolate the plug-in",
        ));
    }
    if caps.is_midi_input() {
        out.push(
            CompatibilityWarning::new(
                WarningLevel::Warning,
                "vst3.midi-cc-input",
                "VST3 delivers MIDI controllers as parameters through `IMidiMapping`, which \
                 this adapter does not implement; notes and per-note expression arrive, \
                 controllers do not",
            )
            .with_advice("drive continuous control from parameters rather than from raw CCs"),
        );
    }
    if caps.is_midi_output() {
        out.push(
            CompatibilityWarning::new(
                WarningLevel::Warning,
                "vst3.midi-cc-output",
                "VST3 has no generic MIDI output event; notes and note expression are sent, \
                 and controllers, program changes and SysEx are dropped",
            )
            .with_advice("export to CLAP or .axt when full MIDI output matters"),
        );
    }
    if caps.contains(Capabilities::NOTE_EXPRESSION) && !caps.is_instrument() {
        out.push(CompatibilityWarning::new(
            WarningLevel::Note,
            "vst3.note-expression-on-effect",
            "VST3 note expression is only routed to plug-ins the host treats as instruments; \
             an effect will not receive it",
        ));
    }

    match descriptor.category {
        Category::MidiEffect => out.push(
            CompatibilityWarning::new(
                WarningLevel::Warning,
                "vst3.midi-effect-category",
                "VST3 has no MIDI-effect class; the plug-in is exported as an instrument with \
                 the \"Instrument\" subcategory and no audio output",
            )
            .with_advice("expect hosts to place it on an instrument track"),
        ),
        Category::Generator => out.push(CompatibilityWarning::new(
            WarningLevel::Note,
            "vst3.generator-category",
            "VST3 has no generator class; the plug-in is exported with the \"Generator\" \
             subcategory of an audio effect",
        )),
        Category::Unknown => out.push(CompatibilityWarning::new(
            WarningLevel::Note,
            "vst3.unknown-category",
            "the plug-in declares no category; it is exported as a plain \"Fx\"",
        )),
        // `Category` is `#[non_exhaustive]`; anything added later maps to "Fx" without a
        // warning, because there is nothing yet to warn about.
        _ => {}
    }

    truncation(
        &mut out,
        "vst3.name-truncated",
        "name",
        &descriptor.name,
        MAX_NAME,
    );
    truncation(
        &mut out,
        "vst3.vendor-truncated",
        "vendor",
        &descriptor.vendor,
        MAX_VENDOR,
    );
    truncation(
        &mut out,
        "vst3.url-truncated",
        "url",
        &descriptor.url,
        MAX_URL,
    );
    truncation(
        &mut out,
        "vst3.support-url-truncated",
        "support URL",
        &descriptor.support_url,
        MAX_EMAIL,
    );

    let subcategories = crate::mapping::subcategories(descriptor);
    if subcategories.len() > MAX_SUBCATEGORIES {
        out.push(
            CompatibilityWarning::new(
                WarningLevel::Note,
                "vst3.subcategories-truncated",
                format!(
                    "the subcategory list is {} bytes and VST3 keeps {MAX_SUBCATEGORIES}; the \
                     tail is dropped",
                    subcategories.len()
                ),
            )
            .with_advice("shorten the `features` list; hosts only index the first few"),
        );
    }

    out
}

/// Adds a truncation warning when `value` will not fit in `max` bytes.
fn truncation(
    out: &mut Vec<CompatibilityWarning>,
    code: &'static str,
    field: &str,
    value: &str,
    max: usize,
) {
    if value.len() > max {
        out.push(CompatibilityWarning::new(
            WarningLevel::Note,
            code,
            format!(
                "the {field} is {} bytes and VST3 keeps {max}; hosts will show it truncated",
                value.len()
            ),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use daux_plugin_api::Version;

    fn plain() -> PluginDescriptor {
        PluginDescriptor::builder("com.example.plain", "Plain")
            .vendor("Example")
            .version(Version::new(1, 0, 0))
            .capabilities(Capabilities::NONE.with_audio_effect())
            .build()
            .expect("a minimal descriptor is valid")
    }

    fn codes(warnings: &[CompatibilityWarning]) -> Vec<&'static str> {
        warnings.iter().map(|w| w.code).collect()
    }

    #[test]
    fn a_plain_audio_effect_translates_losslessly() {
        assert!(compatibility_report(&plain()).is_empty());
    }

    #[test]
    fn every_capability_vst3_cannot_express_is_reported() {
        let d = PluginDescriptor::builder("com.example.everything", "Everything")
            .capabilities(
                Capabilities::NONE
                    .with_instrument()
                    .with_midi2()
                    .with_shared_texture_gui()
                    .with_requires_gui()
                    .with_hard_realtime()
                    .with_dynamic_buses()
                    .with_sandbox_safe(),
            )
            .build()
            .unwrap();
        let report = compatibility_report(&d);
        let codes = codes(&report);
        for expected in [
            "vst3.midi2",
            "vst3.shared-texture-gui",
            "vst3.requires-gui",
            "vst3.hard-realtime",
            "vst3.dynamic-buses",
            "vst3.sandbox-safe",
        ] {
            assert!(codes.contains(&expected), "missing {expected} in {codes:?}");
        }
        // The ones VST3 *can* express must not appear.
        assert!(!codes.contains(&"vst3.tail-infinite"));
        assert!(!codes.contains(&"vst3.latency-dynamic"));
    }

    #[test]
    fn midi_controller_traffic_is_reported_in_both_directions() {
        let d = PluginDescriptor::builder("com.example.midi", "Midi")
            .capabilities(
                Capabilities::NONE
                    .with_instrument()
                    .with_midi_input()
                    .with_midi_output(),
            )
            .build()
            .unwrap();
        let codes = codes(&compatibility_report(&d));
        assert!(codes.contains(&"vst3.midi-cc-input"));
        assert!(codes.contains(&"vst3.midi-cc-output"));
    }

    #[test]
    fn capabilities_vst3_can_express_produce_no_warning() {
        let d = PluginDescriptor::builder("com.example.expressible", "Expressible")
            .capabilities(
                Capabilities::NONE
                    .with_audio_effect()
                    .with_sidechain()
                    .with_has_gui()
                    .with_offline_render()
                    .with_latency_dynamic()
                    .with_tail_infinite()
                    .with_sample_accurate_auto()
                    .with_stereo_only(),
            )
            .build()
            .unwrap();
        assert_eq!(compatibility_report(&d), Vec::new());
    }

    #[test]
    fn the_midi_effect_category_is_a_warning_not_a_silent_remap() {
        let d = PluginDescriptor::builder("com.example.arp", "Arp")
            .category(Category::MidiEffect)
            .capabilities(Capabilities::NONE.with_midi_effect())
            .build()
            .unwrap();
        let report = compatibility_report(&d);
        assert!(codes(&report).contains(&"vst3.midi-effect-category"));
        assert_eq!(report[0].level, WarningLevel::Warning);
    }

    #[test]
    fn over_long_strings_are_reported_before_a_host_silently_cuts_them() {
        let long_name = "N".repeat(200);
        let long_url = format!("https://example.com/{}", "p".repeat(300));
        let d = PluginDescriptor::builder("com.example.long", &long_name)
            .vendor("V".repeat(100))
            .url(long_url)
            .build()
            .unwrap();
        let codes = codes(&compatibility_report(&d));
        assert!(codes.contains(&"vst3.name-truncated"));
        assert!(codes.contains(&"vst3.vendor-truncated"));
        assert!(codes.contains(&"vst3.url-truncated"));
    }

    #[test]
    fn a_long_feature_list_is_reported_as_a_truncated_subcategory_string() {
        let features: Vec<String> = (0..40).map(|i| format!("feature{i}")).collect();
        let d = PluginDescriptor::builder("com.example.tags", "Tags")
            .features(features)
            .build()
            .unwrap();
        assert!(codes(&compatibility_report(&d)).contains(&"vst3.subcategories-truncated"));
    }

    #[test]
    fn warnings_render_for_a_build_log() {
        let w = CompatibilityWarning::new(WarningLevel::Warning, "vst3.midi2", "no UMP")
            .with_advice("use MIDI 1.0");
        assert_eq!(w.to_string(), "warning [vst3.midi2]: no UMP — use MIDI 1.0");
        let n = CompatibilityWarning::new(WarningLevel::Note, "vst3.x", "y");
        assert_eq!(n.to_string(), "note [vst3.x]: y");
    }
}
