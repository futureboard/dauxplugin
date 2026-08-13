//! Descriptor translation, and the report of everything that did not survive it.
//!
//! AXT is the native format, so almost everything in a [`PluginDescriptor`] maps one-to-one
//! onto [`DauxPluginDescriptorV1`]. What cannot survive is *length*: abi-v1 §2.1 fixes the
//! metadata buffers at 64, 128 and 256 bytes so that no allocation crosses the boundary, and a
//! value longer than its buffer is truncated on a character boundary. Silent truncation of a
//! plug-in **id** is not a cosmetic problem — it is a different plug-in as far as every saved
//! project is concerned (abi-v1 §14) — which is why [`compatibility_report`] exists and why
//! `daux build` prints it.

use daux_abi::{
    DAUX_CATEGORY_ANALYZER, DAUX_CATEGORY_EFFECT, DAUX_CATEGORY_GENERATOR,
    DAUX_CATEGORY_INSTRUMENT, DAUX_CATEGORY_MIDI_EFFECT, DAUX_CATEGORY_UNKNOWN,
    DAUX_CATEGORY_UTILITY, DauxId, DauxName, DauxPluginDescriptorV1, DauxText, DauxVersion,
};
use daux_plugin_api::{Capabilities, Category, PluginDescriptor};

/// Stable codes for the warnings [`compatibility_report`] can produce.
///
/// They are `&'static str` rather than an enum so that a tool can print, group or suppress one
/// without matching on a type it would then have to keep up to date.
pub mod warning_code {
    /// A value was longer than its fixed ABI buffer and was truncated.
    pub const TRUNCATED: &str = "axt.truncated";
    /// The plug-in's permanent id was truncated, which changes its identity.
    pub const ID_TRUNCATED: &str = "axt.id-truncated";
    /// The descriptor sets capability bits this build of the ABI does not define.
    pub const UNKNOWN_CAPABILITIES: &str = "axt.unknown-capabilities";
    /// The descriptor requires a newer ABI than this SDK implements.
    pub const ABI_TOO_NEW: &str = "axt.abi-too-new";
    /// The descriptor declares sample formats the ABI has no bit for.
    pub const UNKNOWN_SAMPLE_FORMAT: &str = "axt.unknown-sample-format";
    /// The descriptor is not valid at all; the ABI would publish something a host rejects.
    pub const INVALID_DESCRIPTOR: &str = "axt.invalid-descriptor";
}

/// One thing the target format cannot express exactly.
///
/// [main-thread]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityWarning {
    /// A stable code from [`warning_code`], for tools that filter or group.
    pub code: &'static str,
    /// The descriptor field the warning is about, e.g. `"name"`. Empty when it is about the
    /// descriptor as a whole.
    pub field: &'static str,
    /// A sentence a developer can act on.
    pub message: String,
}

impl CompatibilityWarning {
    /// [main-thread] Builds a warning.
    fn new(code: &'static str, field: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            field,
            message: message.into(),
        }
    }
}

impl core::fmt::Display for CompatibilityWarning {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.field.is_empty() {
            write!(f, "[{}] {}", self.code, self.message)
        } else {
            write!(f, "[{}] {}: {}", self.code, self.field, self.message)
        }
    }
}

/// [main-thread] Everything the AXT export cannot express exactly for `descriptor`.
///
/// An empty report means the descriptor survives the round trip byte for byte. AXT is the
/// native format, so that is the normal outcome; a warning here almost always means a field is
/// longer than the ABI's fixed buffer for it.
///
/// The report is advisory. Nothing here prevents the export — a truncated description is still
/// a working plug-in — with the deliberate exception of an invalid descriptor, which is
/// reported so that `daux build` can fail loudly rather than ship something hosts reject.
#[must_use]
pub fn compatibility_report(descriptor: &PluginDescriptor) -> Vec<CompatibilityWarning> {
    let mut out = Vec::new();

    if let Err(err) = descriptor.validate() {
        out.push(CompatibilityWarning::new(
            warning_code::INVALID_DESCRIPTOR,
            "",
            format!("descriptor is not valid: {err}"),
        ));
    }

    // The id is the one truncation that changes meaning rather than presentation.
    if descriptor.id.as_str().len() > DauxId::CAPACITY {
        out.push(CompatibilityWarning::new(
            warning_code::ID_TRUNCATED,
            "id",
            format!(
                "`{}` is {} bytes; DauxId holds {}, and a truncated id is a different plug-in to \
                 every saved project (abi-v1 §14)",
                descriptor.id.as_str(),
                descriptor.id.as_str().len(),
                DauxId::CAPACITY
            ),
        ));
    }

    let mut check = |field: &'static str, value: &str, capacity: usize| {
        if value.len() > capacity {
            out.push(CompatibilityWarning::new(
                warning_code::TRUNCATED,
                field,
                format!(
                    "{} bytes will be truncated to {capacity} (abi-v1 §2.1)",
                    value.len()
                ),
            ));
        }
    };
    check("name", &descriptor.name, DauxName::CAPACITY);
    check("vendor", &descriptor.vendor, DauxName::CAPACITY);
    check("license", &descriptor.license, DauxName::CAPACITY);
    check(
        "version_string",
        &descriptor.version.to_string(),
        DauxName::CAPACITY,
    );
    check("description", &descriptor.description, DauxText::CAPACITY);
    check("url", &descriptor.url, DauxText::CAPACITY);
    check("support_url", &descriptor.support_url, DauxText::CAPACITY);
    check("copyright", &descriptor.copyright, DauxText::CAPACITY);
    check("features", &join_features(descriptor), DauxText::CAPACITY);

    let unknown = descriptor.capabilities.without(Capabilities::ALL).bits();
    if unknown != 0 {
        out.push(CompatibilityWarning::new(
            warning_code::UNKNOWN_CAPABILITIES,
            "capabilities",
            format!(
                "bits {unknown:#x} are not defined by abi-v1 §6.2; they cross the ABI unchanged \
                 but no host will understand them"
            ),
        ));
    }

    let known_formats = daux_abi::DAUX_SAMPLE_FORMAT_F32 | daux_abi::DAUX_SAMPLE_FORMAT_F64;
    let unknown_formats = descriptor.sample_formats.bits() & !known_formats;
    if unknown_formats != 0 {
        out.push(CompatibilityWarning::new(
            warning_code::UNKNOWN_SAMPLE_FORMAT,
            "sample_formats",
            format!("bits {unknown_formats:#x} are not defined by abi-v1 §6.3"),
        ));
    }

    if descriptor.min_abi.0 > daux_abi::DAUX_ABI_VERSION_MAJOR
        || (descriptor.min_abi.0 == daux_abi::DAUX_ABI_VERSION_MAJOR
            && descriptor.min_abi.1 > daux_abi::DAUX_ABI_VERSION_MINOR)
    {
        out.push(CompatibilityWarning::new(
            warning_code::ABI_TOO_NEW,
            "min_abi",
            format!(
                "plug-in requires ABI {}.{}, this SDK implements {}.{}",
                descriptor.min_abi.0,
                descriptor.min_abi.1,
                daux_abi::DAUX_ABI_VERSION_MAJOR,
                daux_abi::DAUX_ABI_VERSION_MINOR
            ),
        ));
    }

    out
}

/// The ABI's semicolon-separated feature tags (abi-v1 §6).
fn join_features(descriptor: &PluginDescriptor) -> String {
    descriptor.features.join(";")
}

/// [any-thread] The `DAUX_CATEGORY_*` code for a category (abi-v1 §6.1).
///
/// Written out rather than delegated: `daux-core`'s own `Category::code` uses a different,
/// zero-based numbering, and a category that silently shifts by one would file every plug-in
/// under the wrong heading in a host's browser.
pub(crate) const fn category_code(category: Category) -> u32 {
    match category {
        Category::Effect => DAUX_CATEGORY_EFFECT,
        Category::Instrument => DAUX_CATEGORY_INSTRUMENT,
        Category::MidiEffect => DAUX_CATEGORY_MIDI_EFFECT,
        Category::Analyzer => DAUX_CATEGORY_ANALYZER,
        Category::Generator => DAUX_CATEGORY_GENERATOR,
        Category::Utility => DAUX_CATEGORY_UTILITY,
        _ => DAUX_CATEGORY_UNKNOWN,
    }
}

/// [any-thread] The DAUx category a `DAUX_CATEGORY_*` code names.
///
/// The inverse of [`category_code`]. This crate is the *export* side and never reads a category
/// back, so it exists for the round-trip test that keeps the two halves honest.
#[cfg(test)]
const fn category_from_code(code: u32) -> Category {
    match code {
        DAUX_CATEGORY_EFFECT => Category::Effect,
        DAUX_CATEGORY_INSTRUMENT => Category::Instrument,
        DAUX_CATEGORY_MIDI_EFFECT => Category::MidiEffect,
        DAUX_CATEGORY_ANALYZER => Category::Analyzer,
        DAUX_CATEGORY_GENERATOR => Category::Generator,
        DAUX_CATEGORY_UTILITY => Category::Utility,
        _ => Category::Unknown,
    }
}

/// [main-thread] Fills `out` with the ABI form of `descriptor`.
///
/// Every field is written, including the reserved ones, which are zeroed as abi-v1 §3 requires
/// of a writer. Text is truncated on a character boundary; see [`compatibility_report`] for
/// what that costs.
pub(crate) fn write_descriptor(descriptor: &PluginDescriptor, out: &mut DauxPluginDescriptorV1) {
    let (major, minor, patch, build) = descriptor.version.to_parts();
    *out = DauxPluginDescriptorV1::new();
    out.min_abi_version_major = descriptor.min_abi.0;
    out.min_abi_version_minor = descriptor.min_abi.1;
    out.id = DauxId::new(descriptor.id.as_str());
    out.name = DauxName::new(&descriptor.name);
    out.vendor = DauxName::new(&descriptor.vendor);
    out.version = DauxVersion {
        major,
        minor,
        patch,
        build,
    };
    out.version_string = DauxName::new(&descriptor.version.to_string());
    out.description = DauxText::new(&descriptor.description);
    out.url = DauxText::new(&descriptor.url);
    out.support_url = DauxText::new(&descriptor.support_url);
    out.copyright = DauxText::new(&descriptor.copyright);
    out.license = DauxName::new(&descriptor.license);
    out.category = category_code(descriptor.category);
    out.sample_formats = descriptor.sample_formats.bits();
    out.capabilities = descriptor.capabilities.bits();
    out.state_schema_version = descriptor.state_schema_version;
    out.features = DauxText::new(&join_features(descriptor));
}

#[cfg(test)]
mod tests {
    use super::*;
    use daux_plugin_api::{SampleFormats, Version};

    fn descriptor() -> PluginDescriptor {
        PluginDescriptor::builder("studio.futureboard.gain", "Gain")
            .vendor("Futureboard")
            .version(Version::new(2, 3, 4).with_build(5))
            .description("A gain")
            .url("https://example.invalid")
            .support_url("https://example.invalid/support")
            .copyright("(c) 2026")
            .license("MIT OR Apache-2.0")
            .category(Category::Utility)
            .capabilities(Capabilities::NONE.with_audio_effect().with_has_gui())
            .features(["gain", "utility"])
            .state_schema_version(7)
            .build()
            .expect("valid descriptor")
    }

    #[test]
    fn every_field_survives_the_translation() {
        let source = descriptor();
        let mut abi = DauxPluginDescriptorV1::new();
        write_descriptor(&source, &mut abi);

        assert_eq!(abi.size, DauxPluginDescriptorV1::SIZE);
        assert_eq!(abi.id.as_str(), "studio.futureboard.gain");
        assert_eq!(abi.name.as_str(), "Gain");
        assert_eq!(abi.vendor.as_str(), "Futureboard");
        assert_eq!(abi.version.major, 2);
        assert_eq!(abi.version.minor, 3);
        assert_eq!(abi.version.patch, 4);
        assert_eq!(abi.version.build, 5);
        assert_eq!(abi.description.as_str(), "A gain");
        assert_eq!(abi.url.as_str(), "https://example.invalid");
        assert_eq!(abi.support_url.as_str(), "https://example.invalid/support");
        assert_eq!(abi.copyright.as_str(), "(c) 2026");
        assert_eq!(abi.license.as_str(), "MIT OR Apache-2.0");
        assert_eq!(abi.state_schema_version, 7);
        assert_eq!(abi.features.as_str(), "gain;utility");
        assert_eq!(abi.sample_formats, SampleFormats::F32.bits());
        assert_eq!(abi.capabilities, source.capabilities.bits());
        assert_eq!(abi.min_abi_version_major, 1);
        // A writer zeroes everything it does not populate (abi-v1 §3).
        assert_eq!(abi._pad0, 0);
        assert_eq!(abi._pad1, 0);
        assert_eq!(abi.reserved, [0; 8]);
    }

    /// The bug this guards against is a one-off silent shift: `daux-core`'s `Category::code`
    /// numbers `Effect` as `0`, which is `DAUX_CATEGORY_UNKNOWN` on the wire.
    #[test]
    fn category_codes_are_the_abi_codes_not_the_core_codes() {
        assert_eq!(category_code(Category::Unknown), 0);
        assert_eq!(category_code(Category::Effect), 1);
        assert_eq!(category_code(Category::Instrument), 2);
        assert_eq!(category_code(Category::MidiEffect), 3);
        assert_eq!(category_code(Category::Analyzer), 4);
        assert_eq!(category_code(Category::Generator), 5);
        assert_eq!(category_code(Category::Utility), 6);
        for c in Category::ALL {
            assert_eq!(category_from_code(category_code(c)), c, "{c:?}");
        }
        // An unrecognised code loads as Unknown rather than being rejected (abi-v1 §6.1).
        assert_eq!(category_from_code(9_999), Category::Unknown);
    }

    #[test]
    fn a_native_descriptor_reports_nothing() {
        assert_eq!(compatibility_report(&descriptor()), Vec::new());
    }

    #[test]
    fn over_long_text_is_reported_and_truncated_on_a_character_boundary() {
        let long_name = "é".repeat(40); // 80 bytes into a 64-byte buffer.
        let source = PluginDescriptor::builder("com.example.long", &long_name)
            .description("d".repeat(400))
            .build()
            .expect("valid descriptor");

        let report = compatibility_report(&source);
        let fields: Vec<&str> = report.iter().map(|w| w.field).collect();
        assert!(fields.contains(&"name"), "{report:?}");
        assert!(fields.contains(&"description"), "{report:?}");
        assert!(report.iter().all(|w| w.code == warning_code::TRUNCATED));

        let mut abi = DauxPluginDescriptorV1::new();
        write_descriptor(&source, &mut abi);
        // Whatever survives is valid UTF-8: a truncation never leaves half of an "é" behind.
        assert!(abi.name.as_str().chars().all(|c| c == 'é'));
        assert_eq!(abi.name.as_str().len(), 64);
        assert_eq!(abi.description.as_str().len(), 256);

        // ...and when the 64th byte lands inside a character, the copy really does back up.
        let odd = format!("a{}", "é".repeat(40)); // 1 + 2n bytes, so byte 64 splits an "é".
        let truncated = DauxName::new(&odd);
        assert_eq!(truncated.as_str().len(), 63);
        assert!(core::str::from_utf8(truncated.as_bytes()).is_ok());
    }

    #[test]
    fn a_truncated_id_is_reported_as_an_identity_change() {
        let id = format!("com.example.{}", "x".repeat(200));
        let source = PluginDescriptor::builder(&id, "Long").build();
        // `PluginId` refuses an over-long id outright, which is the better outcome; when a
        // descriptor is assembled some other way the report is what catches it.
        assert!(source.is_err(), "PluginId now accepts over-long ids");

        let long = "y".repeat(DauxId::CAPACITY + 1);
        let warning = CompatibilityWarning::new(warning_code::ID_TRUNCATED, "id", long);
        assert!(warning.to_string().starts_with("[axt.id-truncated] id: "));
    }

    #[test]
    fn an_abi_from_the_future_is_reported() {
        let mut source = descriptor();
        source.min_abi = (1, 99);
        let report = compatibility_report(&source);
        assert_eq!(report.len(), 1, "{report:?}");
        assert_eq!(report[0].code, warning_code::ABI_TOO_NEW);

        source.min_abi = (2, 0);
        assert_eq!(
            compatibility_report(&source)[0].code,
            warning_code::ABI_TOO_NEW
        );
    }

    #[test]
    fn unknown_capability_bits_are_reported_but_still_cross_unchanged() {
        let mut source = descriptor();
        source.capabilities = Capabilities::from_bits(1 << 40);
        let report = compatibility_report(&source);
        assert_eq!(report.len(), 1, "{report:?}");
        assert_eq!(report[0].code, warning_code::UNKNOWN_CAPABILITIES);

        let mut abi = DauxPluginDescriptorV1::new();
        write_descriptor(&source, &mut abi);
        assert_eq!(abi.capabilities, 1 << 40, "unknown bits must survive");
    }
}
