//! Turning a module's `DauxPluginDescriptorV1` into the model's [`PluginDescriptor`].
//!
//! The descriptor is the first structured thing a module produces, and it is the cheapest
//! place to find out that the module is not one this host can run: a descriptor that fails
//! [`PluginDescriptor::validate`] is refused here rather than after an instance exists.

use daux_abi::{
    DAUX_CATEGORY_ANALYZER, DAUX_CATEGORY_EFFECT, DAUX_CATEGORY_GENERATOR,
    DAUX_CATEGORY_INSTRUMENT, DAUX_CATEGORY_MIDI_EFFECT, DAUX_CATEGORY_UTILITY,
    DauxPluginDescriptorV1,
};
use daux_audio::SampleFormats;
use daux_core::{Capabilities, Category, PluginDescriptor, Version};

use crate::error::{RuntimeError, RuntimeResult};

/// Maps a `DAUX_CATEGORY_*` code onto the model's category. [any-thread]
///
/// Deliberately **not** `daux_core::Category::from_code`. That function numbers the
/// categories from zero starting at `Effect`, while `abi-v1` §6.1 — which is normative —
/// reserves `0` for `UNKNOWN` and starts `EFFECT` at `1`. Reading a module's descriptor
/// through the wrong table files every plug-in in a host's browser under its neighbour's
/// category, so this crate maps against the `daux-abi` constants, which are transcribed
/// from the specification.
fn category_from_abi(code: u32) -> Category {
    match code {
        DAUX_CATEGORY_EFFECT => Category::Effect,
        DAUX_CATEGORY_INSTRUMENT => Category::Instrument,
        DAUX_CATEGORY_MIDI_EFFECT => Category::MidiEffect,
        DAUX_CATEGORY_ANALYZER => Category::Analyzer,
        DAUX_CATEGORY_GENERATOR => Category::Generator,
        DAUX_CATEGORY_UTILITY => Category::Utility,
        // `DAUX_CATEGORY_UNKNOWN` and anything a newer ABI adds. A host must never refuse a
        // plug-in over a category it does not recognise.
        _ => Category::Unknown,
    }
}

/// Converts a descriptor a module filled in. [main-thread]
///
/// # Errors
///
/// [`RuntimeErrorKind::Protocol`](crate::RuntimeErrorKind::Protocol) when the descriptor
/// breaks an invariant a host is entitled to rely on: a malformed plug-in id, an empty
/// name, no `f32` support, or a declared minimum ABI major of zero.
pub(crate) fn to_plugin_descriptor(
    raw: &DauxPluginDescriptorV1,
) -> RuntimeResult<PluginDescriptor> {
    let id = raw.id.as_str();
    // `features` is one semicolon-separated field in the ABI (`abi-v1` §6). Empty tags are
    // dropped rather than rejected: `"eq;dynamics;"` is a trailing separator, not a defect.
    let features = raw
        .features
        .as_str()
        .split(';')
        .map(str::trim)
        .filter(|tag| !tag.is_empty());

    PluginDescriptor::builder(id, raw.name.as_str())
        .vendor(raw.vendor.as_str())
        .version(Version::from_parts((
            raw.version.major,
            raw.version.minor,
            raw.version.patch,
            raw.version.build,
        )))
        .description(raw.description.as_str())
        .url(raw.url.as_str())
        .support_url(raw.support_url.as_str())
        .copyright(raw.copyright.as_str())
        .license(raw.license.as_str())
        .category(category_from_abi(raw.category))
        .capabilities(Capabilities::from_bits(raw.capabilities))
        .features(features)
        .sample_formats(SampleFormats::from_bits_truncate(raw.sample_formats))
        .state_schema_version(raw.state_schema_version)
        .min_abi(raw.min_abi_version_major, raw.min_abi_version_minor)
        .build()
        .map_err(|e| {
            RuntimeError::protocol(format!(
                "the module's descriptor for `{id}` is not usable: {e}"
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use daux_abi::{
        DAUX_CAP_AUDIO_EFFECT, DAUX_CAP_HAS_GUI, DAUX_CATEGORY_INSTRUMENT, DAUX_SAMPLE_FORMAT_F32,
        DAUX_SAMPLE_FORMAT_F64, DauxId, DauxName, DauxText, DauxVersion,
    };
    use daux_audio::SampleFormat;
    use daux_core::ErrorKind;

    fn raw() -> DauxPluginDescriptorV1 {
        let mut d = DauxPluginDescriptorV1::new();
        d.id = DauxId::new("com.example.gain");
        d.name = DauxName::new("Gain");
        d.vendor = DauxName::new("Example Audio");
        d.version = DauxVersion::new(2, 1, 3, 44);
        d.description = DauxText::new("A gain.");
        d.url = DauxText::new("https://example.com");
        d.category = DAUX_CATEGORY_INSTRUMENT;
        d.capabilities = DAUX_CAP_AUDIO_EFFECT | DAUX_CAP_HAS_GUI;
        d.sample_formats = DAUX_SAMPLE_FORMAT_F32 | DAUX_SAMPLE_FORMAT_F64;
        d.state_schema_version = 3;
        d.features = DauxText::new("eq;dynamics;mastering");
        d
    }

    #[test]
    fn a_conforming_descriptor_converts_field_for_field() {
        let converted = to_plugin_descriptor(&raw()).expect("valid");
        assert_eq!(converted.id, "com.example.gain");
        assert_eq!(converted.name, "Gain");
        assert_eq!(converted.vendor, "Example Audio");
        assert_eq!(converted.version, Version::new(2, 1, 3).with_build(44));
        assert_eq!(converted.category, Category::Instrument);
        assert!(converted.capabilities.contains(Capabilities::HAS_GUI));
        assert!(converted.supports(SampleFormat::F64));
        assert_eq!(converted.state_schema_version, 3);
        assert_eq!(converted.features, ["eq", "dynamics", "mastering"]);
        assert_eq!(converted.min_abi, (1, 0));
    }

    /// A trailing or doubled separator is sloppiness, not corruption; an empty tag would
    /// make the descriptor invalid, so it is dropped instead.
    #[test]
    fn empty_feature_tags_are_dropped_rather_than_rejected() {
        let mut d = raw();
        d.features = DauxText::new(";;eq;  ; dynamics ;");
        let converted = to_plugin_descriptor(&d).expect("valid");
        assert_eq!(converted.features, ["eq", "dynamics"]);

        d.features = DauxText::new("");
        assert!(to_plugin_descriptor(&d).expect("valid").features.is_empty());
    }

    /// Every DAUx plug-in must support `f32` (`abi-v1` §8); a descriptor that does not is
    /// refused at the descriptor, before any instance exists.
    #[test]
    fn a_descriptor_without_f32_is_refused() {
        let mut d = raw();
        d.sample_formats = DAUX_SAMPLE_FORMAT_F64;
        let err = to_plugin_descriptor(&d).unwrap_err();
        assert_eq!(err.kind(), crate::RuntimeErrorKind::Protocol);

        d.sample_formats = 0;
        assert!(to_plugin_descriptor(&d).is_err());

        // Unknown format bits from a newer ABI are masked off, not treated as f32.
        d.sample_formats = 1 << 20;
        assert!(to_plugin_descriptor(&d).is_err());
    }

    #[test]
    fn a_malformed_or_empty_identity_is_refused() {
        let mut d = raw();
        d.id = DauxId::new("not a valid id");
        assert!(to_plugin_descriptor(&d).is_err());

        let mut d = raw();
        d.id = DauxId::empty();
        assert!(to_plugin_descriptor(&d).is_err());

        let mut d = raw();
        d.name = DauxName::new("   ");
        let err = to_plugin_descriptor(&d).unwrap_err();
        assert!(err.message().contains("name"), "{err}");
    }

    /// `min_abi_version_major` of zero is nonsense — v1 is the first release — and the
    /// model rejects it, so the runtime must surface that rather than swallow it.
    #[test]
    fn a_zero_minimum_abi_major_is_refused() {
        let mut d = raw();
        d.min_abi_version_major = 0;
        let err = to_plugin_descriptor(&d).unwrap_err();
        assert_eq!(err.kind(), crate::RuntimeErrorKind::Protocol);
        assert_eq!(daux_core::DauxError::from(err).kind(), ErrorKind::Plugin);
    }

    /// The numbers here are transcribed from `abi-v1` §6.1, not from any Rust constant, so
    /// a renumbering anywhere in the workspace fails this test rather than silently filing
    /// every plug-in under its neighbour's category.
    #[test]
    fn categories_map_onto_the_numbers_the_specification_assigns() {
        assert_eq!(category_from_abi(0), Category::Unknown);
        assert_eq!(category_from_abi(1), Category::Effect);
        assert_eq!(category_from_abi(2), Category::Instrument);
        assert_eq!(category_from_abi(3), Category::MidiEffect);
        assert_eq!(category_from_abi(4), Category::Analyzer);
        assert_eq!(category_from_abi(5), Category::Generator);
        assert_eq!(category_from_abi(6), Category::Utility);
        assert_eq!(category_from_abi(7), Category::Unknown);
        assert_eq!(category_from_abi(u32::MAX), Category::Unknown);

        // And the `daux-abi` constants agree with those literals.
        assert_eq!(DAUX_CATEGORY_EFFECT, 1);
        assert_eq!(DAUX_CATEGORY_INSTRUMENT, 2);
        assert_eq!(DAUX_CATEGORY_MIDI_EFFECT, 3);
        assert_eq!(DAUX_CATEGORY_ANALYZER, 4);
        assert_eq!(DAUX_CATEGORY_GENERATOR, 5);
        assert_eq!(DAUX_CATEGORY_UTILITY, 6);
    }

    /// A category or capability bit from a newer ABI must degrade, not fail: the
    /// descriptor is still perfectly usable.
    #[test]
    fn unknown_categories_and_capability_bits_degrade_gracefully() {
        let mut d = raw();
        d.category = 9_999;
        d.capabilities = u64::MAX;
        let converted = to_plugin_descriptor(&d).expect("still usable");
        assert_eq!(converted.category, Category::Unknown);
        assert_ne!(converted.capabilities.unknown_bits(), 0);
    }

    /// A module built against a newer minor revision reports a higher `min_abi` minor.
    /// That is not a conversion failure; whether this host can drive it is a separate
    /// question, answered by `PluginDescriptor::loadable_over_abi`.
    #[test]
    fn a_newer_minimum_minor_converts_and_reports_itself() {
        let mut d = raw();
        d.min_abi_version_minor = 4;
        let converted = to_plugin_descriptor(&d).expect("valid");
        assert_eq!(converted.min_abi, (1, 4));
        assert!(!converted.loadable_over_abi(1, 0));
        assert!(converted.loadable_over_abi(1, 4));
    }

    /// Fixed buffers are NUL-padded and may be filled to the brim; reading one must stop
    /// at the padding and never run past the array.
    #[test]
    fn a_full_fixed_buffer_reads_back_without_its_padding() {
        let mut d = raw();
        let long = "n".repeat(200);
        d.name = DauxName::new(&long);
        let converted = to_plugin_descriptor(&d).expect("valid");
        assert_eq!(converted.name.len(), daux_abi::DAUX_NAME_SIZE);
    }
}
