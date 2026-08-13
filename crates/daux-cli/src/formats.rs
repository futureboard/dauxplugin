//! The export formats, and the one vocabulary the CLI prints their reports in.
//!
//! Each adapter reports what it cannot express in its own terms — VST3 grades a loss as a
//! note or a warning, CLAP as info, lossy or unsupported, AXT names the descriptor field it
//! had to truncate. A build log that mixed the three would be unreadable, so they are
//! normalised here onto [`daux_bundle::Severity`], which is what every other finding in this
//! CLI is already printed with. Nothing is dropped: the adapter's own code and wording
//! survive verbatim, and matching on `code` in a script keeps working.

use daux_bundle::Severity;
use daux_runtime::daux_core::PluginDescriptor;

/// One export format. [main-thread]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Format {
    /// The native DAUx Audio Extension.
    Axt,
    /// The VST3 compatibility export.
    Vst3,
    /// The CLAP compatibility export.
    Clap,
}

impl Format {
    /// Every format, in the order a build reports them.
    pub const ALL: [Self; 3] = [Self::Axt, Self::Vst3, Self::Clap];

    /// [main-thread] Parses a format name, case-insensitively.
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "axt" => Some(Self::Axt),
            "vst3" => Some(Self::Vst3),
            "clap" => Some(Self::Clap),
            _ => None,
        }
    }

    /// [main-thread] The lower-case name this format is written as.
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Axt => "axt",
            Self::Vst3 => "vst3",
            Self::Clap => "clap",
        }
    }

    /// [main-thread] Every format's name, comma-separated, for a diagnostic.
    pub fn names() -> String {
        Self::ALL.map(Self::slug).join(", ")
    }
}

impl std::fmt::Display for Format {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.slug())
    }
}

/// One thing an export format cannot carry, in the CLI's own vocabulary. [main-thread]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormatWarning {
    /// Which export the finding is about.
    pub format: Format,
    /// How badly it hurts, normalised from the adapter's own grading.
    pub severity: Severity,
    /// The adapter's stable code, e.g. `"vst3.midi2"`. Safe to match on.
    pub code: &'static str,
    /// What is lost, in the adapter's own words.
    pub message: String,
    /// What the author can do about it, when the adapter suggested anything.
    pub advice: Option<String>,
}

impl std::fmt::Display for FormatWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: [{}] {}",
            self.severity.as_str(),
            self.code,
            self.message
        )
    }
}

/// [main-thread] Everything `format` cannot express about `descriptor`.
///
/// An empty result means the plug-in maps onto that format without loss.
pub fn compatibility_report(format: Format, descriptor: &PluginDescriptor) -> Vec<FormatWarning> {
    match format {
        Format::Axt => daux_format_axt::compatibility_report(descriptor)
            .into_iter()
            .map(|warning| FormatWarning {
                format,
                // The native format loses nothing by design, so anything it reports is a
                // truncation — except a descriptor it refuses outright, which is a build
                // error rather than a note about a shortened string.
                severity: if warning.code == daux_format_axt::warning_code::INVALID_DESCRIPTOR {
                    Severity::Error
                } else {
                    Severity::Warning
                },
                code: warning.code,
                message: if warning.field.is_empty() {
                    warning.message
                } else {
                    format!("{} ({})", warning.message, warning.field)
                },
                advice: None,
            })
            .collect(),
        Format::Vst3 => daux_format_vst3::compatibility_report(descriptor)
            .into_iter()
            .map(|warning| FormatWarning {
                format,
                severity: match warning.level {
                    daux_format_vst3::WarningLevel::Note => Severity::Info,
                    _ => Severity::Warning,
                },
                code: warning.code,
                message: warning.message,
                advice: warning.advice,
            })
            .collect(),
        Format::Clap => daux_format_clap::compatibility_report(descriptor)
            .into_iter()
            .map(|warning| FormatWarning {
                format,
                severity: match warning.severity {
                    daux_format_clap::WarningSeverity::Info => Severity::Info,
                    // "Lossy" and "unsupported" both mean a host will observe something the
                    // plug-in did not intend, which is what a warning is for.
                    _ => Severity::Warning,
                },
                code: warning.code,
                message: warning.message,
                advice: None,
            })
            .collect(),
    }
}

/// [main-thread] A compatibility finding as JSON.
pub fn warning_json(warning: &FormatWarning) -> serde_json::Value {
    serde_json::json!({
        "format": warning.format.slug(),
        "severity": warning.severity.as_str(),
        "code": warning.code,
        "message": warning.message,
        "advice": warning.advice,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use daux_runtime::daux_core::{Capabilities, Category};

    fn descriptor(capabilities: Capabilities) -> PluginDescriptor {
        PluginDescriptor::builder("com.example.gain", "Gain")
            .vendor("Example Audio")
            .category(Category::Effect)
            .capabilities(capabilities)
            .build()
            .expect("a valid identity")
    }

    #[test]
    fn format_names_round_trip_and_nonsense_is_refused() {
        for format in Format::ALL {
            assert_eq!(Format::parse(format.slug()), Some(format));
            assert_eq!(Format::parse(&format.slug().to_uppercase()), Some(format));
            assert_eq!(
                Format::parse(&format!("  {}  ", format.slug())),
                Some(format)
            );
        }
        assert_eq!(Format::parse("au"), None);
        assert_eq!(Format::parse(""), None);
        assert_eq!(Format::parse("axt3"), None);
    }

    /// The native format is lossless for a plain descriptor: anything it reported would be a
    /// bug in the adapter, and printing noise on every build would train developers to
    /// ignore the report.
    #[test]
    fn a_plain_plug_in_maps_onto_the_native_format_without_loss() {
        let report = compatibility_report(Format::Axt, &descriptor(Capabilities::NONE));
        assert!(report.is_empty(), "{report:#?}");
    }

    /// The point of the report: a capability the compatibility formats cannot carry must
    /// reach the developer at build time rather than a user at load time.
    #[test]
    fn a_capability_vst3_cannot_express_is_reported_against_vst3_only() {
        let midi2 = Capabilities::NONE.with_midi2().with_midi_input();
        let vst3 = compatibility_report(Format::Vst3, &descriptor(midi2));
        assert!(
            vst3.iter().any(|w| w.code.contains("midi2")),
            "MIDI 2.0 is not expressible in VST3: {vst3:#?}"
        );
        assert!(vst3.iter().all(|w| w.format == Format::Vst3));

        // And the native format, which speaks MIDI 2.0 natively, says nothing about it.
        let axt = compatibility_report(Format::Axt, &descriptor(midi2));
        assert!(!axt.iter().any(|w| w.code.contains("midi2")), "{axt:#?}");
    }

    /// Every adapter grades its findings differently; the CLI prints one scale. A finding
    /// that arrived as "unsupported" must not be shown as "info".
    #[test]
    fn adapter_gradings_are_normalised_without_losing_the_code() {
        let hostile = Capabilities::NONE
            .with_midi2()
            .with_shared_texture_gui()
            .with_has_gui()
            .with_note_expression();
        for format in Format::ALL {
            for warning in compatibility_report(format, &descriptor(hostile)) {
                assert_eq!(warning.format, format);
                assert!(!warning.code.is_empty(), "a finding must be matchable");
                assert!(!warning.message.is_empty());
                assert!(
                    matches!(
                        warning.severity,
                        Severity::Info | Severity::Warning | Severity::Error
                    ),
                    "{warning:?}"
                );
                let json = warning_json(&warning);
                assert_eq!(json["code"], warning.code);
                assert_eq!(json["format"], format.slug());
            }
        }
    }

    /// The report has to be stable between two runs of the same build, or a diff of two
    /// build logs is useless.
    #[test]
    fn the_report_is_deterministic() {
        let d = descriptor(Capabilities::NONE.with_midi2().with_has_gui());
        for format in Format::ALL {
            let first = compatibility_report(format, &d);
            let second = compatibility_report(format, &d);
            assert_eq!(first, second);
        }
    }
}
