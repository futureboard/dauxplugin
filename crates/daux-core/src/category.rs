//! What kind of plug-in this is.

use core::fmt;
use core::str::FromStr;

use crate::{DauxError, DauxResult, ErrorKind};

/// The primary role of a plug-in, mirroring `DAUX_CATEGORY_*` in
/// `docs/specifications/abi-v1.md` §6.
///
/// A category is a hint for how a host files a plug-in in its browser, not a constraint on
/// what it may do; the authoritative statement of what a plug-in can do is its
/// [`Capabilities`](crate::Capabilities) bitset and its bus layout. Hosts must treat an
/// unrecognised numeric category as [`Category::Unknown`] rather than rejecting the plug-in,
/// so that a plug-in built against a later ABI minor version still loads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Category {
    /// Audio in, audio out: a filter, a compressor, a reverb.
    Effect,
    /// Events in, audio out: a synthesiser or sampler.
    Instrument,
    /// Events in, events out, no audio: an arpeggiator or chord generator.
    MidiEffect,
    /// Reads audio and reports on it without changing it: a meter or spectrogram.
    Analyzer,
    /// Produces audio from nothing: a tone or noise generator, a test signal.
    Generator,
    /// Routing, gain staging, metering helpers and other infrastructure.
    Utility,
    /// The host did not recognise the category the plug-in declared.
    #[default]
    Unknown,
}

impl Category {
    /// Every category a host can expect to see, in ABI order.
    pub const ALL: [Category; 7] = [
        Category::Effect,
        Category::Instrument,
        Category::MidiEffect,
        Category::Analyzer,
        Category::Generator,
        Category::Utility,
        Category::Unknown,
    ];

    /// [any-thread] The ABI code for this category, `DAUX_CATEGORY_*` (abi-v1 §6.1).
    ///
    /// `Unknown` is `0`, not a sentinel: the spec puts it at the bottom of the range so that
    /// a zeroed descriptor means "uncategorised" rather than accidentally meaning "effect".
    pub const fn code(self) -> u32 {
        match self {
            Category::Unknown => 0,
            Category::Effect => 1,
            Category::Instrument => 2,
            Category::MidiEffect => 3,
            Category::Analyzer => 4,
            Category::Generator => 5,
            Category::Utility => 6,
        }
    }

    /// [any-thread] Decodes an ABI category code.
    ///
    /// An unknown code becomes [`Category::Unknown`]; this never fails, because refusing to
    /// load a plug-in over a category we do not recognise would break forward compatibility.
    pub const fn from_code(code: u32) -> Self {
        match code {
            1 => Category::Effect,
            2 => Category::Instrument,
            3 => Category::MidiEffect,
            4 => Category::Analyzer,
            5 => Category::Generator,
            6 => Category::Utility,
            // `0` is `DAUX_CATEGORY_UNKNOWN`; anything higher is a category from a later
            // ABI minor version, and both mean the same thing to this build.
            _ => Category::Unknown,
        }
    }

    /// [any-thread] The stable lower-case name used in manifests and on the CLI.
    pub const fn as_str(self) -> &'static str {
        match self {
            Category::Effect => "effect",
            Category::Instrument => "instrument",
            Category::MidiEffect => "midi-effect",
            Category::Analyzer => "analyzer",
            Category::Generator => "generator",
            Category::Utility => "utility",
            Category::Unknown => "unknown",
        }
    }

    /// [any-thread] `true` when a plug-in of this kind is expected to produce audio.
    ///
    /// A [`Category::MidiEffect`] is the only category with no audio output at all; an
    /// [`Category::Analyzer`] normally passes its input through unchanged.
    pub const fn produces_audio(self) -> bool {
        !matches!(self, Category::MidiEffect)
    }

    /// [any-thread] `true` when a plug-in of this kind is normally driven by note events.
    pub const fn is_note_driven(self) -> bool {
        matches!(self, Category::Instrument | Category::MidiEffect)
    }
}

impl fmt::Display for Category {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Category {
    type Err = DauxError;

    /// Parses the manifest spelling, accepting a few common aliases.
    ///
    /// Unlike [`Category::from_code`] this *does* fail on an unrecognised name: a manifest is
    /// authored by a human and a typo there should be reported, not silently downgraded.
    fn from_str(s: &str) -> DauxResult<Self> {
        let normalised = s.trim().to_ascii_lowercase().replace(['_', ' '], "-");
        Ok(match normalised.as_str() {
            "effect" | "audio-effect" | "fx" => Category::Effect,
            "instrument" | "synth" | "synthesizer" | "synthesiser" => Category::Instrument,
            "midi-effect" | "note-effect" | "event-effect" => Category::MidiEffect,
            "analyzer" | "analyser" => Category::Analyzer,
            "generator" | "tone-generator" => Category::Generator,
            "utility" | "tool" => Category::Utility,
            "unknown" | "" => Category::Unknown,
            other => {
                return Err(DauxError::new(
                    ErrorKind::InvalidArgument,
                    format!("unknown plug-in category `{other}`"),
                ));
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_round_trip() {
        for c in Category::ALL {
            assert_eq!(Category::from_code(c.code()), c, "{c}");
        }
    }

    /// Pins every code to the number the specification fixes.
    ///
    /// A round-trip test alone cannot catch this: a self-consistent but wrong numbering
    /// round-trips perfectly and still files every plug-in under the wrong heading in every
    /// host. `daux-core` cannot depend on `daux-abi`, so the constants are restated here and
    /// this test is what keeps the two honest.
    #[test]
    fn codes_match_abi_v1_section_6_1() {
        assert_eq!(Category::Unknown.code(), 0, "DAUX_CATEGORY_UNKNOWN");
        assert_eq!(Category::Effect.code(), 1, "DAUX_CATEGORY_EFFECT");
        assert_eq!(Category::Instrument.code(), 2, "DAUX_CATEGORY_INSTRUMENT");
        assert_eq!(Category::MidiEffect.code(), 3, "DAUX_CATEGORY_MIDI_EFFECT");
        assert_eq!(Category::Analyzer.code(), 4, "DAUX_CATEGORY_ANALYZER");
        assert_eq!(Category::Generator.code(), 5, "DAUX_CATEGORY_GENERATOR");
        assert_eq!(Category::Utility.code(), 6, "DAUX_CATEGORY_UTILITY");
    }

    #[test]
    fn an_unknown_code_degrades_instead_of_failing() {
        assert_eq!(Category::from_code(9999), Category::Unknown);
        // The first code past the ones v1.0 defines: a category from a later minor version.
        assert_eq!(Category::from_code(7), Category::Unknown);
    }

    #[test]
    fn names_round_trip() {
        for c in Category::ALL {
            assert_eq!(c.as_str().parse::<Category>().unwrap(), c, "{c}");
        }
    }

    #[test]
    fn parsing_is_forgiving_about_spelling_but_not_about_typos() {
        assert_eq!("Synth".parse::<Category>().unwrap(), Category::Instrument);
        assert_eq!(
            "MIDI_Effect".parse::<Category>().unwrap(),
            Category::MidiEffect
        );
        assert_eq!(
            " analyser ".parse::<Category>().unwrap(),
            Category::Analyzer
        );
        let err = "efect".parse::<Category>().unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidArgument);
        assert!(err.message().contains("efect"));
    }

    #[test]
    fn the_default_is_unknown() {
        assert_eq!(Category::default(), Category::Unknown);
        assert_eq!("".parse::<Category>().unwrap(), Category::Unknown);
    }

    #[test]
    fn role_hints_match_the_categories() {
        assert!(Category::Instrument.is_note_driven());
        assert!(Category::MidiEffect.is_note_driven());
        assert!(!Category::Effect.is_note_driven());
        assert!(!Category::MidiEffect.produces_audio());
        assert!(Category::Effect.produces_audio());
    }
}
