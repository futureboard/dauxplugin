//! What CLAP cannot say about a DAUx plug-in.
//!
//! `daux build` prints this report so an author finds out at build time — not from a bug
//! report — that a capability they declared does not survive the trip through CLAP.
//!
//! Everything here is `[main-thread]`: it allocates and formats strings.

use core::fmt;

use daux_plugin_api::{Capabilities, PluginDescriptor, SampleFormat};

use crate::abi::CLAP_NAME_SIZE;

/// How badly a mapping loses information.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WarningSeverity {
    /// The plug-in still works; the host simply never learns something.
    #[default]
    Info,
    /// The mapping succeeds but changes meaning — a value is clamped, truncated or widened.
    Lossy,
    /// CLAP has no way to express this at all, and the behaviour will differ.
    Unsupported,
}

impl WarningSeverity {
    /// `[any-thread]` A short, stable word for logs and CLI output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            WarningSeverity::Info => "info",
            WarningSeverity::Lossy => "lossy",
            WarningSeverity::Unsupported => "unsupported",
        }
    }
}

impl fmt::Display for WarningSeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One thing the CLAP export cannot express faithfully.
///
/// `code` is stable and machine-readable; `message` is for humans and may change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityWarning {
    /// Stable identifier, e.g. `"clap.shared-texture-gui"`.
    pub code: &'static str,
    /// How badly the mapping loses information.
    pub severity: WarningSeverity,
    /// Human-readable explanation, including what the host will see instead.
    pub message: String,
}

impl CompatibilityWarning {
    /// `[main-thread]` Builds a warning.
    #[must_use]
    pub fn new(code: &'static str, severity: WarningSeverity, message: impl Into<String>) -> Self {
        Self {
            code,
            severity,
            message: message.into(),
        }
    }
}

impl fmt::Display for CompatibilityWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}: {}", self.severity, self.code, self.message)
    }
}

/// `[main-thread]` Every capability of `d` that the CLAP export cannot carry.
///
/// An empty result means the descriptor maps onto CLAP without loss. The list is stable in
/// order (declaration order below) so a build log diffs cleanly between runs.
#[must_use]
pub fn compatibility_report(d: &PluginDescriptor) -> Vec<CompatibilityWarning> {
    let mut out = Vec::new();
    let caps = d.capabilities;

    // ---- things that stop the export working at all --------------------------------
    if d.id.as_str().as_bytes().contains(&0) {
        out.push(CompatibilityWarning::new(
            "clap.id-nul",
            WarningSeverity::Unsupported,
            "the plug-in id contains a NUL byte and cannot be a C string; CLAP hosts will \
             not see this plug-in",
        ));
    }
    for (field, value) in [
        ("name", d.name.as_str()),
        ("vendor", d.vendor.as_str()),
        ("description", d.description.as_str()),
        ("url", d.url.as_str()),
        ("support_url", d.support_url.as_str()),
    ] {
        if value.as_bytes().contains(&0) {
            out.push(CompatibilityWarning::new(
                "clap.string-nul",
                WarningSeverity::Lossy,
                format!(
                    "`{field}` contains a NUL byte and is truncated at it in the CLAP descriptor"
                ),
            ));
        }
    }
    for feature in &d.features {
        if feature.as_bytes().contains(&0) {
            out.push(CompatibilityWarning::new(
                "clap.feature-nul",
                WarningSeverity::Lossy,
                format!("feature tag `{feature}` contains a NUL byte and is truncated at it"),
            ));
        }
    }

    // ---- capabilities CLAP has no vocabulary for -----------------------------------
    if caps.is_shared_texture_gui() {
        out.push(CompatibilityWarning::new(
            "clap.shared-texture-gui",
            WarningSeverity::Unsupported,
            "CLAP has no shared-texture GUI extension (abi-v1 §13); the editor is exported \
             as an embedded child window instead",
        ));
    }
    if caps.is_requires_gui() {
        out.push(CompatibilityWarning::new(
            "clap.requires-gui",
            WarningSeverity::Unsupported,
            "CLAP cannot advertise that a plug-in is unusable without its editor; headless \
             hosts will instantiate it anyway",
        ));
    }
    if caps.is_sandbox_safe() {
        out.push(CompatibilityWarning::new(
            "clap.sandbox-safe",
            WarningSeverity::Info,
            "CLAP has no sandbox-safety declaration; the bit is dropped",
        ));
    }
    if caps.is_stereo_only() {
        out.push(CompatibilityWarning::new(
            "clap.stereo-only",
            WarningSeverity::Info,
            "CLAP expresses channel constraints through the audio-ports layout rather than a \
             flag; declare a stereo bus layout instead of relying on STEREO_ONLY",
        ));
    }
    if caps.is_dynamic_buses() {
        out.push(CompatibilityWarning::new(
            "clap.dynamic-buses",
            WarningSeverity::Unsupported,
            "renegotiating buses needs `clap.audio-ports-config`, which this adapter does \
             not export; the host sees one fixed layout",
        ));
    }
    if caps.is_note_expression() && !d.capabilities.is_midi_input() {
        out.push(CompatibilityWarning::new(
            "clap.note-expression-without-input",
            WarningSeverity::Info,
            "NOTE_EXPRESSION is declared without MIDI_INPUT, so no CLAP note port is \
             exported and no expression can ever arrive",
        ));
    }

    // ---- mappings that work but lose precision ------------------------------------
    let unknown = caps.unknown_bits();
    if unknown != 0 {
        out.push(CompatibilityWarning::new(
            "clap.unknown-capabilities",
            WarningSeverity::Info,
            format!("capability bits {unknown:#x} are not defined by ABI v1 and are dropped"),
        ));
    }
    if d.supports(SampleFormat::F64) {
        out.push(CompatibilityWarning::new(
            "clap.f64-optional",
            WarningSeverity::Info,
            "64-bit processing is advertised per audio port with \
             CLAP_AUDIO_PORT_SUPPORTS_64BITS; a host is free to ignore it and send f32",
        ));
    }
    if d.name.chars().count() >= CLAP_NAME_SIZE {
        out.push(CompatibilityWarning::new(
            "clap.name-too-long",
            WarningSeverity::Lossy,
            format!(
                "port and parameter name buffers hold {} bytes including the NUL; long names \
                 are truncated on a character boundary",
                CLAP_NAME_SIZE
            ),
        ));
    }

    // ---- things a CLAP host will do differently -------------------------------------
    if caps.is_hard_realtime() && caps.is_offline_render() {
        out.push(CompatibilityWarning::new(
            "clap.render-contradiction",
            WarningSeverity::Lossy,
            "HARD_REALTIME and OFFLINE_RENDER are both declared; the CLAP render extension \
             reports a hard real-time requirement and refuses offline mode",
        ));
    }
    if !caps.intersects(
        Capabilities::AUDIO_EFFECT
            | Capabilities::INSTRUMENT
            | Capabilities::MIDI_EFFECT
            | Capabilities::ANALYZER,
    ) {
        out.push(CompatibilityWarning::new(
            "clap.no-primary-feature",
            WarningSeverity::Info,
            "no primary capability is declared, so the CLAP feature list falls back to \
             \"audio-effect\" and the plug-in may be filed oddly in a host's browser",
        ));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use daux_plugin_api::{Category, SampleFormats};

    fn descriptor(caps: Capabilities) -> PluginDescriptor {
        PluginDescriptor::builder("com.example.thing", "Thing")
            .capabilities(caps)
            .build()
            .expect("valid descriptor")
    }

    #[test]
    fn a_plain_effect_maps_without_loss() {
        let d = descriptor(Capabilities::AUDIO_EFFECT);
        assert_eq!(compatibility_report(&d), Vec::new());
    }

    #[test]
    fn the_capabilities_clap_cannot_say_are_all_reported() {
        let d = descriptor(
            Capabilities::AUDIO_EFFECT
                .with_shared_texture_gui()
                .with_requires_gui()
                .with_sandbox_safe()
                .with_stereo_only()
                .with_dynamic_buses(),
        );
        let codes: Vec<&str> = compatibility_report(&d).iter().map(|w| w.code).collect();
        assert_eq!(
            codes,
            [
                "clap.shared-texture-gui",
                "clap.requires-gui",
                "clap.sandbox-safe",
                "clap.stereo-only",
                "clap.dynamic-buses",
            ]
        );
    }

    #[test]
    fn shared_texture_and_requires_gui_are_hard_unsupported() {
        let d = descriptor(Capabilities::AUDIO_EFFECT.with_shared_texture_gui());
        let w = &compatibility_report(&d)[0];
        assert_eq!(w.severity, WarningSeverity::Unsupported);
        assert!(
            w.to_string()
                .starts_with("[unsupported] clap.shared-texture-gui")
        );
    }

    #[test]
    fn a_descriptor_with_no_primary_role_is_flagged() {
        let d = descriptor(Capabilities::NONE);
        let codes: Vec<&str> = compatibility_report(&d).iter().map(|w| w.code).collect();
        assert!(codes.contains(&"clap.no-primary-feature"));

        // Declaring one silences it.
        let d = descriptor(Capabilities::INSTRUMENT);
        let codes: Vec<&str> = compatibility_report(&d).iter().map(|w| w.code).collect();
        assert!(!codes.contains(&"clap.no-primary-feature"));
    }

    #[test]
    fn contradictory_render_capabilities_are_reported() {
        let d = descriptor(
            Capabilities::AUDIO_EFFECT
                .with_hard_realtime()
                .with_offline_render(),
        );
        let codes: Vec<&str> = compatibility_report(&d).iter().map(|w| w.code).collect();
        assert!(codes.contains(&"clap.render-contradiction"));
    }

    #[test]
    fn a_nul_in_a_string_field_is_reported_rather_than_silently_truncating() {
        let mut d = descriptor(Capabilities::AUDIO_EFFECT);
        d.name = "Bad\0Name".to_owned();
        d.features = vec!["fine".to_owned(), "bro\0ken".to_owned()];
        let report = compatibility_report(&d);
        let codes: Vec<&str> = report.iter().map(|w| w.code).collect();
        assert!(codes.contains(&"clap.string-nul"));
        assert!(codes.contains(&"clap.feature-nul"));
        assert!(report.iter().any(|w| w.message.contains("bro\0ken")));
    }

    #[test]
    fn a_valid_id_never_trips_the_nul_check() {
        // `PluginId::validate` already refuses a NUL, so the `clap.id-nul` branch only fires
        // for a descriptor assembled around the validator. It is kept because the
        // consequence is severe — a host would see a truncated id and load the wrong plug-in
        // — and this pins the common case so the check cannot start firing spuriously.
        let d = descriptor(Capabilities::AUDIO_EFFECT);
        assert!(
            compatibility_report(&d)
                .iter()
                .all(|w| w.code != "clap.id-nul")
        );
    }

    #[test]
    fn f64_support_is_reported_as_advisory_only() {
        let d = PluginDescriptor::builder("com.example.wide", "Wide")
            .capabilities(Capabilities::AUDIO_EFFECT)
            .sample_formats(SampleFormats::BOTH)
            .category(Category::Effect)
            .build()
            .unwrap();
        let report = compatibility_report(&d);
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].code, "clap.f64-optional");
        assert_eq!(report[0].severity, WarningSeverity::Info);
    }

    #[test]
    fn unknown_capability_bits_are_reported_not_dropped_silently() {
        let d = descriptor(Capabilities::from_bits(
            Capabilities::AUDIO_EFFECT.bits() | (1 << 42),
        ));
        let report = compatibility_report(&d);
        assert!(report.iter().any(|w| w.code == "clap.unknown-capabilities"));
        assert!(report.iter().any(|w| w.message.contains("0x40000000000")));
    }
}
