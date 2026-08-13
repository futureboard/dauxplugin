//! The `manifest.json` model (`manifest-v1` §3) and its hostile-input parser.

use std::collections::BTreeMap;
use std::fmt;

use serde::de::{Error as DeError, MapAccess, Unexpected, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{BundleError, BundleErrorKind, BundleResult};
use crate::json_scan;
use crate::limits::{
    FORMAT_SENTINEL, FORMAT_VERSION, MAX_CAPABILITY_KEYS, MAX_DEPENDENCIES,
    MAX_DIRECTORY_NAME_BYTES, MAX_FEATURE_BYTES, MAX_FEATURES, MAX_ID_BYTES, MAX_NAME_BYTES,
    MAX_PLUGIN_ENTRIES, MAX_RESOURCE_ENTRIES, MAX_TARGETS, MAX_TEXT_BYTES,
};
use crate::path_rules;
use crate::target::TargetId;

/// Largest logical editor dimension the manifest may declare (`manifest-v1` §3.9).
const MAX_LOGICAL_PIXELS: u32 = 16_384;

// ---------------------------------------------------------------------------------------
// Category
// ---------------------------------------------------------------------------------------

/// What kind of plug-in this is (`manifest-v1` §3.6). [any-thread]
///
/// The manifest carries a cached copy; `DauxPluginDescriptorV1::category` in the binary is
/// authoritative once the module is loaded (`manifest-v1` §7.1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Category {
    /// Category is unknown or does not fit the list.
    #[default]
    Unknown,
    /// Processes audio it is given.
    Effect,
    /// Generates audio from notes.
    Instrument,
    /// Transforms events without producing audio.
    MidiEffect,
    /// Measures without altering the signal.
    Analyzer,
    /// Produces audio without input.
    Generator,
    /// Routing, metering and other tooling.
    Utility,
}

impl Category {
    /// Every category, in the order `manifest-v1` §3.6 lists them.
    pub const ALL: [Self; 7] = [
        Self::Unknown,
        Self::Effect,
        Self::Instrument,
        Self::MidiEffect,
        Self::Analyzer,
        Self::Generator,
        Self::Utility,
    ];

    /// [any-thread] The slug this category is written as.
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Effect => "effect",
            Self::Instrument => "instrument",
            Self::MidiEffect => "midi-effect",
            Self::Analyzer => "analyzer",
            Self::Generator => "generator",
            Self::Utility => "utility",
        }
    }

    /// [main-thread] Parses a category slug.
    ///
    /// Both spellings of the compound slug are accepted: `manifest-v1` §3.6 writes
    /// `midi-effect` and `axt-v1` §7.1 writes `midiEffect`. Anything else returns `None`;
    /// a reader must never silently map a typo to `unknown`, because that hides a build
    /// mistake until it reaches a user's machine.
    #[must_use]
    pub fn parse(slug: &str) -> Option<Self> {
        match slug {
            "unknown" => Some(Self::Unknown),
            "effect" => Some(Self::Effect),
            "instrument" => Some(Self::Instrument),
            "midi-effect" | "midiEffect" => Some(Self::MidiEffect),
            "analyzer" => Some(Self::Analyzer),
            "generator" => Some(Self::Generator),
            "utility" => Some(Self::Utility),
            _ => None,
        }
    }

    /// [any-thread] The `DAUX_CATEGORY_*` constant of `abi-v1` §6.1.
    #[must_use]
    pub const fn as_abi_constant(self) -> u32 {
        match self {
            Self::Unknown => daux_abi::DAUX_CATEGORY_UNKNOWN,
            Self::Effect => daux_abi::DAUX_CATEGORY_EFFECT,
            Self::Instrument => daux_abi::DAUX_CATEGORY_INSTRUMENT,
            Self::MidiEffect => daux_abi::DAUX_CATEGORY_MIDI_EFFECT,
            Self::Analyzer => daux_abi::DAUX_CATEGORY_ANALYZER,
            Self::Generator => daux_abi::DAUX_CATEGORY_GENERATOR,
            Self::Utility => daux_abi::DAUX_CATEGORY_UTILITY,
        }
    }

    /// [any-thread] Maps a `DAUX_CATEGORY_*` constant back, or `None` when the binary
    /// reports a category this build does not know.
    #[must_use]
    pub const fn from_abi_constant(value: u32) -> Option<Self> {
        match value {
            daux_abi::DAUX_CATEGORY_UNKNOWN => Some(Self::Unknown),
            daux_abi::DAUX_CATEGORY_EFFECT => Some(Self::Effect),
            daux_abi::DAUX_CATEGORY_INSTRUMENT => Some(Self::Instrument),
            daux_abi::DAUX_CATEGORY_MIDI_EFFECT => Some(Self::MidiEffect),
            daux_abi::DAUX_CATEGORY_ANALYZER => Some(Self::Analyzer),
            daux_abi::DAUX_CATEGORY_GENERATOR => Some(Self::Generator),
            daux_abi::DAUX_CATEGORY_UTILITY => Some(Self::Utility),
            _ => None,
        }
    }
}

impl fmt::Display for Category {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.slug())
    }
}

impl Serialize for Category {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.slug())
    }
}

impl<'de> Deserialize<'de> for Category {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw)
            .ok_or_else(|| DeError::invalid_value(Unexpected::Str(&raw), &"a category slug"))
    }
}

// ---------------------------------------------------------------------------------------
// Capabilities
// ---------------------------------------------------------------------------------------

/// The manifest capability names and the `DAUX_CAP_*` bit each one maps to, in the order
/// `manifest-v1` §3.8 lists them. Writers emit enabled entries in this order so that two
/// builds of the same source produce byte-identical output (`manifest-v1` §3.13).
pub const CAPABILITY_KEYS: [(&str, u64); 20] = [
    ("audioEffect", daux_abi::DAUX_CAP_AUDIO_EFFECT),
    ("instrument", daux_abi::DAUX_CAP_INSTRUMENT),
    ("midiEffect", daux_abi::DAUX_CAP_MIDI_EFFECT),
    ("analyzer", daux_abi::DAUX_CAP_ANALYZER),
    ("midiInput", daux_abi::DAUX_CAP_MIDI_INPUT),
    ("midiOutput", daux_abi::DAUX_CAP_MIDI_OUTPUT),
    ("midi2", daux_abi::DAUX_CAP_MIDI2),
    ("sidechain", daux_abi::DAUX_CAP_SIDECHAIN),
    ("dynamicBuses", daux_abi::DAUX_CAP_DYNAMIC_BUSES),
    (
        "sampleAccurateAutomation",
        daux_abi::DAUX_CAP_SAMPLE_ACCURATE_AUTO,
    ),
    ("noteExpression", daux_abi::DAUX_CAP_NOTE_EXPRESSION),
    ("hasGui", daux_abi::DAUX_CAP_HAS_GUI),
    ("requiresGui", daux_abi::DAUX_CAP_REQUIRES_GUI),
    ("sharedTextureGui", daux_abi::DAUX_CAP_SHARED_TEXTURE_GUI),
    ("offlineRender", daux_abi::DAUX_CAP_OFFLINE_RENDER),
    ("hardRealtime", daux_abi::DAUX_CAP_HARD_REALTIME),
    ("sandboxSafe", daux_abi::DAUX_CAP_SANDBOX_SAFE),
    ("stereoOnly", daux_abi::DAUX_CAP_STEREO_ONLY),
    ("latencyDynamic", daux_abi::DAUX_CAP_LATENCY_DYNAMIC),
    ("tailInfinite", daux_abi::DAUX_CAP_TAIL_INFINITE),
];

/// The coarse capability bits a scanner can filter on without opening a library.
/// [any-thread]
///
/// Stored as the same `u64` bitset the ABI uses, so comparing a manifest against
/// `DauxPluginDescriptorV1::capabilities` (`manifest-v1` §8.1 row 6) is one integer
/// comparison. Unknown names in the document are ignored rather than rejected: a v2 SDK
/// will add bits, and a v1 host cannot act on a capability it has never heard of.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ManifestCaps(u64);

impl ManifestCaps {
    /// [any-thread] No capability declared.
    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    /// [any-thread] Wraps a raw `DAUX_CAP_*` bitset.
    #[must_use]
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    /// [any-thread] The raw `DAUX_CAP_*` bitset.
    #[must_use]
    pub const fn bits(self) -> u64 {
        self.0
    }

    /// [any-thread] Whether every bit of `bit` is set.
    #[must_use]
    pub const fn contains(self, bit: u64) -> bool {
        self.0 & bit == bit
    }

    /// [any-thread] Returns a copy with `bit` set.
    #[must_use]
    pub const fn with(self, bit: u64) -> Self {
        Self(self.0 | bit)
    }

    /// [any-thread] Sets or clears `bit` in place.
    pub const fn set(&mut self, bit: u64, enabled: bool) {
        if enabled {
            self.0 |= bit;
        } else {
            self.0 &= !bit;
        }
    }

    /// [any-thread] Whether the bundle advertises an editor.
    #[must_use]
    pub const fn has_gui(self) -> bool {
        self.contains(daux_abi::DAUX_CAP_HAS_GUI)
    }

    /// [main-thread] Looks a capability up by its manifest name.
    ///
    /// Returns `None` for a name this build does not know.
    #[must_use]
    pub fn get(self, name: &str) -> Option<bool> {
        CAPABILITY_KEYS
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, bit)| self.contains(*bit))
    }

    /// [main-thread] Sets a capability by its manifest name.
    ///
    /// Returns `false` — leaving `self` untouched — when the name is not one this build
    /// knows.
    pub fn set_named(&mut self, name: &str, enabled: bool) -> bool {
        match CAPABILITY_KEYS.iter().find(|(key, _)| *key == name) {
            Some((_, bit)) => {
                self.set(*bit, enabled);
                true
            }
            None => false,
        }
    }

    /// [main-thread] The names of the enabled capabilities, in specification order.
    pub fn enabled_names(self) -> impl Iterator<Item = &'static str> {
        CAPABILITY_KEYS
            .into_iter()
            .filter(move |(_, bit)| self.contains(*bit))
            .map(|(name, _)| name)
    }
}

impl Serialize for ManifestCaps {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let enabled: Vec<&'static str> = self.enabled_names().collect();
        let mut map = serializer.serialize_map(Some(enabled.len()))?;
        for name in enabled {
            map.serialize_entry(name, &true)?;
        }
        map.end()
    }
}

struct CapsVisitor;

impl<'de> Visitor<'de> for CapsVisitor {
    type Value = ManifestCaps;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an object of capability booleans")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut caps = ManifestCaps::empty();
        let mut seen = 0usize;
        while let Some(key) = map.next_key::<String>()? {
            seen += 1;
            if seen > MAX_CAPABILITY_KEYS {
                return Err(DeError::custom(format!(
                    "more than {MAX_CAPABILITY_KEYS} capability keys"
                )));
            }
            // `0`, `1`, `"true"` and `null` are errors, not falsy values
            // (`manifest-v1` §3.8).
            let value: bool = map.next_value()?;
            caps.set_named(&key, value);
        }
        Ok(caps)
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        // `axt-v1` §8.1 spells `DAUxCapabilities` as an array of the enabled names, while
        // `manifest-v1` §6.2 spells it as a dictionary of booleans. Accepting both keeps
        // the two documents from denoting different bundles; writers emit the dictionary.
        let mut caps = ManifestCaps::empty();
        let mut seen = 0usize;
        while let Some(name) = seq.next_element::<String>()? {
            seen += 1;
            if seen > MAX_CAPABILITY_KEYS {
                return Err(DeError::custom(format!(
                    "more than {MAX_CAPABILITY_KEYS} capability entries"
                )));
            }
            caps.set_named(&name, true);
        }
        Ok(caps)
    }
}

impl<'de> Deserialize<'de> for ManifestCaps {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(CapsVisitor)
    }
}

// ---------------------------------------------------------------------------------------
// Graphics
// ---------------------------------------------------------------------------------------

/// UI framework an editor is built with (`manifest-v1` §3.9). [any-thread]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GraphicsFramework {
    /// `egui`.
    Egui,
    /// `gpui`.
    Gpui,
    /// A framework outside the SDK's own backends.
    Custom,
}

/// Renderer an editor draws with (`manifest-v1` §3.9). [any-thread]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GraphicsRenderer {
    /// `wgpu`.
    Wgpu,
    /// `opengl`.
    #[serde(rename = "opengl")]
    OpenGl,
    /// CPU rasterisation.
    Software,
}

/// How an editor surface reaches the host (`manifest-v1` §3.9). [any-thread]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GraphicsPresentation {
    /// The plug-in owns a native window.
    NativeWindow,
    /// The plug-in draws into a surface the host embeds.
    #[default]
    EmbeddedSurface,
    /// The plug-in hands the host a shared GPU texture.
    SharedTexture,
    /// The editor is an entirely separate window the host does not embed.
    ExternalWindow,
}

const fn default_true() -> bool {
    true
}
const fn default_width() -> u32 {
    800
}
const fn default_height() -> u32 {
    600
}

/// The pre-load editor hint (`manifest-v1` §3.9). [any-thread]
///
/// Everything here lets a host size a placeholder window or skip a bundle in a headless
/// render farm *before* loading code. The plug-in's `GraphicDescriptor`, returned after
/// load, is authoritative for the actual editor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestGraphics {
    /// `false` means the bundle declares no editor.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Required when [`ManifestGraphics::enabled`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub framework: Option<GraphicsFramework>,
    /// Required when [`ManifestGraphics::enabled`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renderer: Option<GraphicsRenderer>,
    /// How the surface reaches the host.
    #[serde(default)]
    pub presentation: GraphicsPresentation,
    /// Whether the host may resize the editor.
    #[serde(default)]
    pub resizable: bool,
    /// Preferred logical width, 1..=16384.
    #[serde(default = "default_width")]
    pub width: u32,
    /// Preferred logical height, 1..=16384.
    #[serde(default = "default_height")]
    pub height: u32,
    /// Smallest logical width the editor accepts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_width: Option<u32>,
    /// Smallest logical height the editor accepts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_height: Option<u32>,
    /// Largest logical width the editor accepts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_width: Option<u32>,
    /// Largest logical height the editor accepts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_height: Option<u32>,
}

impl Default for ManifestGraphics {
    fn default() -> Self {
        Self {
            enabled: true,
            framework: None,
            renderer: None,
            presentation: GraphicsPresentation::default(),
            resizable: false,
            width: default_width(),
            height: default_height(),
            min_width: None,
            min_height: None,
            max_width: None,
            max_height: None,
        }
    }
}

impl ManifestGraphics {
    fn check(&self) -> BundleResult<()> {
        if self.enabled && self.framework.is_none() {
            return Err(missing("graphics.framework"));
        }
        if self.enabled && self.renderer.is_none() {
            return Err(missing("graphics.renderer"));
        }
        check_pixels("graphics.width", self.width)?;
        check_pixels("graphics.height", self.height)?;
        for (field, value) in [
            ("graphics.minWidth", self.min_width),
            ("graphics.minHeight", self.min_height),
            ("graphics.maxWidth", self.max_width),
            ("graphics.maxHeight", self.max_height),
        ] {
            if let Some(value) = value {
                check_pixels(field, value)?;
            }
        }
        if self.min_width.is_some_and(|min| min > self.width) {
            return Err(invalid("graphics.minWidth exceeds graphics.width"));
        }
        if self.min_height.is_some_and(|min| min > self.height) {
            return Err(invalid("graphics.minHeight exceeds graphics.height"));
        }
        if self.max_width.is_some_and(|max| max < self.width) {
            return Err(invalid("graphics.maxWidth is below graphics.width"));
        }
        if self.max_height.is_some_and(|max| max < self.height) {
            return Err(invalid("graphics.maxHeight is below graphics.height"));
        }
        Ok(())
    }
}

fn check_pixels(field: &str, value: u32) -> BundleResult<()> {
    if value == 0 || value > MAX_LOGICAL_PIXELS {
        return Err(invalid(format!(
            "{field} is {value}, outside 1..={MAX_LOGICAL_PIXELS}"
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Resources, dependencies, generator, plug-in entries
// ---------------------------------------------------------------------------------------

fn default_resource_dir() -> String {
    "Resources".to_owned()
}
fn default_library_dir() -> String {
    "Library".to_owned()
}

/// Where resources and bundled dependencies live, and which of them the plug-in needs.
/// [any-thread]
///
/// `dir` and `libraryDir` come from `manifest-v1` §3.11; `required` and `optional` come
/// from `axt-v1` §7.5. Both sets are supported because both documents are normative and a
/// bundle in the wild may carry either.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestResources {
    /// Resource directory at the bundle root. Single segment; defaults to `Resources`.
    #[serde(default = "default_resource_dir")]
    pub dir: String,
    /// Dependency directory at the bundle root. Single segment; defaults to `Library`.
    #[serde(default = "default_library_dir")]
    pub library_dir: String,
    /// Logical paths that MUST exist; `daux validate` reports a missing one as an error.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required: Vec<String>,
    /// Logical paths that MAY exist.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub optional: Vec<String>,
}

impl Default for ManifestResources {
    fn default() -> Self {
        Self {
            dir: default_resource_dir(),
            library_dir: default_library_dir(),
            required: Vec::new(),
            optional: Vec::new(),
        }
    }
}

impl ManifestResources {
    fn check(&self) -> BundleResult<()> {
        check_directory_name("resources.dir", &self.dir)?;
        check_directory_name("resources.libraryDir", &self.library_dir)?;
        if self.required.len() + self.optional.len() > MAX_RESOURCE_ENTRIES {
            return Err(limit(format!(
                "more than {MAX_RESOURCE_ENTRIES} declared resources"
            )));
        }
        for logical in self.required.iter().chain(&self.optional) {
            path_rules::validate_logical(logical)?;
        }
        Ok(())
    }
}

fn check_directory_name(field: &str, value: &str) -> BundleResult<()> {
    if value.is_empty() || value.len() > MAX_DIRECTORY_NAME_BYTES {
        return Err(invalid(format!(
            "{field} must be 1..={MAX_DIRECTORY_NAME_BYTES} bytes"
        )));
    }
    if !value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Err(invalid(format!(
            "{field} may only contain `[A-Za-z0-9._-]`"
        )));
    }
    if value.starts_with('.') {
        return Err(invalid(format!("{field} must not start with `.`")));
    }
    path_rules::validate_component(value).map_err(|err| {
        invalid(format!(
            "{field}: {}",
            err.detail().unwrap_or("illegal directory name")
        ))
    })
}

/// Provenance of the tool that generated the manifest (`manifest-v1` §3.12). [any-thread]
///
/// Informational only. A reader MUST NOT let any value here change its behaviour.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestGenerator {
    /// Tool name, e.g. `"daux"`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Tool version.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub version: String,
    /// Free text. JSON has no comments, so this is where the "do not edit" notice lives.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

/// One entry of the optional `plugins` array (`axt-v1` §7.6). [any-thread]
///
/// The factory's descriptor enumeration is authoritative; this list only lets a scanner
/// show the secondary plug-ins of a multi-plug-in bundle without loading code.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestPluginRef {
    /// Reverse-DNS id of this plug-in.
    pub id: String,
    /// Display name.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Category slug.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<Category>,
}

// ---------------------------------------------------------------------------------------
// Plugin block
// ---------------------------------------------------------------------------------------

/// The `plugin` object of `manifest.json` (`manifest-v1` §3.2). [any-thread]
///
/// Every byte limit here is chosen so that the value survives a lossless round trip
/// through the fixed ABI buffers of `abi-v1` §2.1: an over-long value is rejected, never
/// truncated.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestPlugin {
    /// Permanent reverse-DNS identity (`manifest-v1` §3.4).
    pub id: String,
    /// Display name.
    pub name: String,
    /// Publisher display name.
    pub vendor: String,
    /// `major.minor.patch[.build]`, decimal, no `v` prefix.
    pub version: String,
    /// One-line description; the key may be absent and then reads as `""`.
    #[serde(default)]
    pub description: String,
    /// Human-facing version text, e.g. `"1.0.0-beta.2"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_string: Option<String>,
    /// Category slug; absent means [`Category::Unknown`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<Category>,
    /// Product page.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
    /// Support page.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub support_url: String,
    /// Copyright notice.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub copyright: String,
    /// SPDX identifier where applicable.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub license: String,
    /// Free-form lower-case search tags.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,
}

impl ManifestPlugin {
    fn check(&self) -> BundleResult<()> {
        validate_plugin_id(&self.id)?;
        check_text("plugin.name", &self.name, MAX_NAME_BYTES, false)?;
        check_text("plugin.vendor", &self.vendor, MAX_NAME_BYTES, false)?;
        validate_version(&self.version)?;
        check_text(
            "plugin.description",
            &self.description,
            MAX_TEXT_BYTES,
            true,
        )?;
        if let Some(version_string) = &self.version_string {
            check_text("plugin.versionString", version_string, MAX_NAME_BYTES, true)?;
        }
        check_text("plugin.url", &self.url, MAX_TEXT_BYTES, true)?;
        check_text("plugin.supportUrl", &self.support_url, MAX_TEXT_BYTES, true)?;
        check_text("plugin.copyright", &self.copyright, MAX_TEXT_BYTES, true)?;
        check_text("plugin.license", &self.license, MAX_NAME_BYTES, true)?;
        check_features(&self.features)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------
// Manifest
// ---------------------------------------------------------------------------------------

/// `manifest.json` as written on disk (`manifest-v1` §3). [any-thread]
///
/// Unknown top-level keys are captured in [`Manifest::unknown`] rather than discarded, so
/// that a tool which rewrites a manifest preserves what a newer SDK put there
/// (`manifest-v1` §9.1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    /// MUST be `"DAUx Audio Extension"`; a cheap sanity gate against an unrelated
    /// `manifest.json`.
    pub format: String,
    /// Bundle format version. `1` for this document.
    pub format_version: u32,
    /// ABI **major** version the binaries were built against.
    pub abi_version: u32,
    /// Lowest ABI **minor** version the binaries need.
    #[serde(default)]
    pub abi_version_minor: u32,
    /// The principal plug-in of this bundle.
    pub plugin: ManifestPlugin,
    /// Target identifiers this bundle ships a binary for.
    pub targets: Vec<TargetId>,
    /// Coarse capability bits for pre-load filtering.
    #[serde(default)]
    pub capabilities: ManifestCaps,
    /// Editor hint; absence means "no editor".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graphics: Option<ManifestGraphics>,
    /// Resource and dependency directory names and declared resource paths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<ManifestResources>,
    /// Bare file names expected in the dependency directory.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,
    /// Secondary plug-ins of a multi-plug-in bundle (`axt-v1` §7.6).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugins: Vec<ManifestPluginRef>,
    /// Scanner hint; `DauxPluginDescriptorV1::state_schema_version` is authoritative.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_schema_version: Option<u32>,
    /// Which tool wrote this file. Informational.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generator: Option<ManifestGenerator>,
    /// Top-level keys this build does not recognise, preserved verbatim.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, serde_json::Value>,
}

impl Manifest {
    /// [main-thread] A minimal, valid manifest for one plug-in.
    ///
    /// # Errors
    ///
    /// [`BundleErrorKind::InvalidId`] or [`BundleErrorKind::InvalidVersion`] when the
    /// identity is not well-formed.
    pub fn new(id: &str, name: &str, vendor: &str, version: &str) -> BundleResult<Self> {
        let manifest = Self {
            format: FORMAT_SENTINEL.to_owned(),
            format_version: FORMAT_VERSION,
            abi_version: daux_abi::DAUX_ABI_VERSION_MAJOR,
            abi_version_minor: 0,
            plugin: ManifestPlugin {
                id: id.to_owned(),
                name: name.to_owned(),
                vendor: vendor.to_owned(),
                version: version.to_owned(),
                ..ManifestPlugin::default()
            },
            targets: Vec::new(),
            capabilities: ManifestCaps::empty(),
            graphics: None,
            resources: None,
            dependencies: Vec::new(),
            plugins: Vec::new(),
            state_schema_version: None,
            generator: None,
            unknown: BTreeMap::new(),
        };
        manifest.plugin.check()?;
        Ok(manifest)
    }

    /// [main-thread] Parses and validates `manifest.json` from raw bytes.
    ///
    /// The order of operations is normative (`manifest-v1` §9.3): encoding, then the
    /// structural limits, then the stable prologue — so that a v2 bundle produces
    /// *"EQUZX 2.0.0 uses AXT format 2; this host supports 1"* rather than
    /// "unparseable file" — and only then the typed parse and the field rules.
    ///
    /// # Errors
    ///
    /// Any [`BundleErrorKind`] the metadata rules define. Never panics, whatever the
    /// bytes are.
    pub fn from_json_bytes(bytes: &[u8]) -> BundleResult<Self> {
        let text = crate::read::decode_utf8(bytes)?;
        json_scan::prescan(text)?;

        let prologue: Prologue = serde_json::from_str(text).map_err(|err| {
            BundleError::new(
                BundleErrorKind::Parse,
                format!("manifest is not a JSON object: {err}"),
            )
        })?;
        prologue.check()?;

        let manifest: Self = serde_json::from_str(text).map_err(map_serde_error)?;
        manifest.check()?;
        Ok(manifest)
    }

    /// [main-thread] Serialises the manifest deterministically (`manifest-v1` §3.13).
    ///
    /// UTF-8 without a BOM, two-space indent, LF line endings, exactly one trailing LF,
    /// keys in specification order. Two calls with equal input produce equal bytes.
    ///
    /// # Errors
    ///
    /// [`BundleErrorKind::InvalidField`] if the value cannot be represented as JSON, which
    /// in practice means a non-string map key inside [`Manifest::unknown`].
    pub fn to_json(&self) -> BundleResult<String> {
        let mut text = serde_json::to_string_pretty(self)
            .map_err(|err| invalid(format!("cannot serialise manifest: {err}")))?;
        text.push('\n');
        Ok(text)
    }

    /// [main-thread] Applies every field rule of `manifest-v1` §3.
    ///
    /// # Errors
    ///
    /// The first violation found, as a [`BundleError`] carrying the offending field in its
    /// detail.
    pub fn check(&self) -> BundleResult<()> {
        if self.format != FORMAT_SENTINEL {
            return Err(BundleError::new(
                BundleErrorKind::WrongFormat,
                format!(
                    "`format` is `{}`, expected `{FORMAT_SENTINEL}`",
                    self.format
                ),
            ));
        }
        if self.format_version != FORMAT_VERSION {
            return Err(BundleError::bare(
                BundleErrorKind::UnsupportedFormatVersion {
                    found: self.format_version,
                    supported: FORMAT_VERSION,
                },
            ));
        }
        self.plugin.check()?;
        check_targets(&self.targets)?;
        check_dependencies(&self.dependencies)?;
        if let Some(graphics) = &self.graphics {
            graphics.check()?;
        }
        if let Some(resources) = &self.resources {
            resources.check()?;
        }
        if self.plugins.len() > MAX_PLUGIN_ENTRIES {
            return Err(limit(format!(
                "more than {MAX_PLUGIN_ENTRIES} entries in `plugins`"
            )));
        }
        for entry in &self.plugins {
            validate_plugin_id(&entry.id)?;
            check_text("plugins[].name", &entry.name, MAX_NAME_BYTES, true)?;
        }
        Ok(())
    }

    /// [main-thread] The resource directory name, honouring `resources.dir`.
    #[must_use]
    pub fn resource_dir_name(&self) -> &str {
        self.resources
            .as_ref()
            .map_or("Resources", |resources| resources.dir.as_str())
    }

    /// [main-thread] The dependency directory name, honouring `resources.libraryDir`.
    #[must_use]
    pub fn library_dir_name(&self) -> &str {
        self.resources
            .as_ref()
            .map_or("Library", |resources| resources.library_dir.as_str())
    }
}

/// The six keys that are frozen for all time across every future `formatVersion`
/// (`manifest-v1` §3.3), read leniently so that a document this build refuses can still be
/// described in the diagnostic.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Prologue {
    #[serde(default)]
    format: Option<serde_json::Value>,
    #[serde(default)]
    format_version: Option<serde_json::Value>,
    #[serde(default)]
    plugin: Option<ProloguePlugin>,
}

#[derive(Debug, Deserialize)]
struct ProloguePlugin {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    version: Option<String>,
}

impl Prologue {
    fn describe(&self) -> String {
        let plugin = self.plugin.as_ref();
        let name = plugin
            .and_then(|p| p.name.as_deref())
            .unwrap_or("<unnamed>");
        let version = plugin
            .and_then(|p| p.version.as_deref())
            .unwrap_or("<no version>");
        let id = plugin.and_then(|p| p.id.as_deref()).unwrap_or("<no id>");
        format!("{name} {version} ({id})")
    }

    fn check(&self) -> BundleResult<()> {
        match self.format.as_ref().and_then(serde_json::Value::as_str) {
            Some(FORMAT_SENTINEL) => {}
            Some(other) => {
                return Err(BundleError::new(
                    BundleErrorKind::WrongFormat,
                    format!("`format` is `{other}`, expected `{FORMAT_SENTINEL}`"),
                ));
            }
            None => {
                return Err(BundleError::new(
                    BundleErrorKind::WrongFormat,
                    "`format` is missing or is not a string",
                ));
            }
        }
        let Some(raw) = self.format_version.as_ref() else {
            return Err(missing("formatVersion"));
        };
        // `1.0`, `1e0`, `"1"` and `0x1` are all errors: `formatVersion` is an exact
        // integer and MUST NOT be routed through `f64` (`manifest-v1` §10.3).
        let Some(found) = raw.as_u64().and_then(|v| u32::try_from(v).ok()) else {
            return Err(invalid(format!(
                "`formatVersion` must be a non-negative integer, found `{raw}`"
            )));
        };
        if found != FORMAT_VERSION {
            return Err(BundleError::new(
                BundleErrorKind::UnsupportedFormatVersion {
                    found,
                    supported: FORMAT_VERSION,
                },
                self.describe(),
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------
// Shared field rules
// ---------------------------------------------------------------------------------------

fn invalid(detail: impl Into<String>) -> BundleError {
    BundleError::new(BundleErrorKind::InvalidField, detail)
}

fn limit(detail: impl Into<String>) -> BundleError {
    BundleError::new(BundleErrorKind::LimitExceeded, detail)
}

fn missing(field: &str) -> BundleError {
    BundleError::new(
        BundleErrorKind::MissingField,
        format!("`{field}` is missing"),
    )
}

fn map_serde_error(err: serde_json::Error) -> BundleError {
    let message = err.to_string();
    let kind = if message.starts_with("missing field") {
        BundleErrorKind::MissingField
    } else {
        BundleErrorKind::InvalidField
    };
    BundleError::new(kind, message)
}

/// Rejects C0/C1 control characters and the two Unicode line separators in text that
/// reaches a fixed ABI buffer (`manifest-v1` §3.2).
pub(crate) fn check_text(
    field: &str,
    value: &str,
    max_bytes: usize,
    allow_empty: bool,
) -> BundleResult<()> {
    if !allow_empty && value.is_empty() {
        return Err(invalid(format!("`{field}` is empty")));
    }
    if value.len() > max_bytes {
        return Err(invalid(format!(
            "`{field}` is {} bytes, limit is {max_bytes}",
            value.len()
        )));
    }
    for ch in value.chars() {
        let code = ch as u32;
        if code <= 0x1F || (0x7F..=0x9F).contains(&code) || code == 0x2028 || code == 0x2029 {
            return Err(invalid(format!(
                "`{field}` contains the control character U+{code:04X}"
            )));
        }
    }
    Ok(())
}

/// [main-thread] Validates a plug-in id against the grammar of `manifest-v1` §3.4.
///
/// # Errors
///
/// [`BundleErrorKind::InvalidId`] when the id is empty, over 127 bytes, not lower-case
/// ASCII, has no `.`, has an empty label, or contains no letter at all.
pub fn validate_plugin_id(id: &str) -> BundleResult<()> {
    let bad = |detail: String| BundleError::new(BundleErrorKind::InvalidId, detail);

    if id.is_empty() {
        return Err(bad("plug-in id is empty".to_owned()));
    }
    if id.len() > MAX_ID_BYTES {
        return Err(bad(format!(
            "plug-in id is {} bytes, limit is {MAX_ID_BYTES}",
            id.len()
        )));
    }
    if !id.contains('.') {
        return Err(bad(format!("`{id}` is not reverse-DNS (no `.`)")));
    }
    let mut has_letter = false;
    for label in id.split('.') {
        if label.is_empty() {
            return Err(bad(format!("`{id}` has an empty label")));
        }
        let first = label.as_bytes()[0];
        if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
            return Err(bad(format!(
                "label `{label}` must start with a lower-case ASCII letter or digit"
            )));
        }
        for byte in label.bytes() {
            if byte.is_ascii_lowercase() {
                has_letter = true;
            } else if !(byte.is_ascii_digit() || byte == b'-' || byte == b'_') {
                return Err(bad(format!(
                    "`{id}` contains `{}`, which is not `[a-z0-9-_.]`",
                    char::from(byte)
                )));
            }
        }
    }
    if !has_letter {
        return Err(bad(format!("`{id}` contains no ASCII letter")));
    }
    Ok(())
}

/// [main-thread] Parses `major.minor.patch[.build]` into its four components.
///
/// # Errors
///
/// [`BundleErrorKind::InvalidVersion`] for a missing component, a non-decimal component, a
/// leading zero, or a component that overflows `u32`. Parsing is checked; a value never
/// wraps.
pub fn validate_version(version: &str) -> BundleResult<[u32; 4]> {
    let bad = |detail: String| BundleError::new(BundleErrorKind::InvalidVersion, detail);

    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() < 3 || parts.len() > 4 {
        return Err(bad(format!(
            "`{version}` is not `major.minor.patch[.build]`"
        )));
    }
    let mut out = [0u32; 4];
    for (index, part) in parts.iter().enumerate() {
        if part.is_empty() {
            return Err(bad(format!("`{version}` has an empty component")));
        }
        if !part.bytes().all(|b| b.is_ascii_digit()) {
            return Err(bad(format!(
                "`{version}` has a non-decimal component `{part}`"
            )));
        }
        if part.len() > 1 && part.starts_with('0') {
            return Err(bad(format!("`{version}` has a leading zero in `{part}`")));
        }
        out[index] = part
            .parse::<u32>()
            .map_err(|_| bad(format!("`{version}` overflows a u32 in `{part}`")))?;
    }
    Ok(out)
}

fn check_features(features: &[String]) -> BundleResult<()> {
    if features.len() > MAX_FEATURES {
        return Err(limit(format!(
            "more than {MAX_FEATURES} entries in `plugin.features`"
        )));
    }
    let mut joined = 0usize;
    for tag in features {
        if tag.is_empty() || tag.len() > MAX_FEATURE_BYTES {
            return Err(invalid(format!(
                "feature tag `{tag}` must be 1..={MAX_FEATURE_BYTES} bytes"
            )));
        }
        let mut bytes = tag.bytes();
        let first = bytes.next().unwrap_or(b'-');
        if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
            return Err(invalid(format!(
                "feature tag `{tag}` must match `[a-z0-9][a-z0-9-]*`"
            )));
        }
        if !bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
            return Err(invalid(format!(
                "feature tag `{tag}` must match `[a-z0-9][a-z0-9-]*`"
            )));
        }
        joined += tag.len() + 1;
    }
    if joined > MAX_TEXT_BYTES + 1 {
        return Err(limit(
            "`plugin.features` does not fit the ABI features buffer".to_owned(),
        ));
    }
    Ok(())
}

fn check_targets(targets: &[TargetId]) -> BundleResult<()> {
    if targets.is_empty() {
        return Err(invalid("`targets` is empty"));
    }
    if targets.len() > MAX_TARGETS {
        return Err(limit(format!(
            "more than {MAX_TARGETS} entries in `targets`"
        )));
    }
    for (index, target) in targets.iter().enumerate() {
        if targets[..index].contains(target) {
            return Err(BundleError::new(
                BundleErrorKind::InvalidTarget,
                format!("`{target}` appears twice in `targets`"),
            ));
        }
    }
    Ok(())
}

fn check_dependencies(dependencies: &[String]) -> BundleResult<()> {
    if dependencies.len() > MAX_DEPENDENCIES {
        return Err(limit(format!(
            "more than {MAX_DEPENDENCIES} entries in `dependencies`"
        )));
    }
    for (index, name) in dependencies.iter().enumerate() {
        path_rules::validate_component(name)?;
        if dependencies[..index].contains(name) {
            return Err(invalid(format!("`{name}` appears twice in `dependencies`")));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> Manifest {
        let mut m = Manifest::new("com.example.gain", "Gain", "Example Audio", "1.2.3")
            .expect("a well-formed identity");
        m.targets = vec![TargetId::parse("windows-x86_64").expect("a known target")];
        m
    }

    #[test]
    fn a_manifest_round_trips_through_json() {
        let m = valid();
        let json = m.to_json().unwrap();
        let back = Manifest::from_json_bytes(json.as_bytes()).unwrap();
        assert_eq!(back.plugin.id, m.plugin.id);
        assert_eq!(back.targets, m.targets);
        assert_eq!(back.format, FORMAT_SENTINEL);
        assert_eq!(back.format_version, FORMAT_VERSION);
    }

    #[test]
    fn serialisation_is_deterministic_and_lf_terminated() {
        let m = valid();
        assert_eq!(m.to_json().unwrap(), m.to_json().unwrap());
        let json = m.to_json().unwrap();
        assert!(json.ends_with('\n'), "exactly one trailing LF");
        assert!(!json.ends_with("\n\n"));
        assert!(!json.contains('\r'), "LF line endings only");
        assert!(!json.starts_with('\u{feff}'), "no BOM");
    }

    #[test]
    fn unknown_top_level_keys_survive_a_round_trip() {
        let json = r#"{
  "format": "DAUx Audio Extension",
  "formatVersion": 1,
  "abiVersion": 1,
  "plugin": {"id": "com.example.gain", "name": "Gain", "vendor": "E", "version": "1.0.0"},
  "targets": ["windows-x86_64"],
  "somethingFromTheFuture": {"nested": true}
}"#;
        let m = Manifest::from_json_bytes(json.as_bytes()).unwrap();
        assert!(m.unknown.contains_key("somethingFromTheFuture"));
        let back = Manifest::from_json_bytes(m.to_json().unwrap().as_bytes()).unwrap();
        assert_eq!(back.unknown, m.unknown);
    }

    #[test]
    fn the_format_sentinel_is_enforced() {
        let json = r#"{
  "format": "Some Other Thing",
  "formatVersion": 1,
  "abiVersion": 1,
  "plugin": {"id": "com.example.gain", "name": "Gain", "vendor": "E", "version": "1.0.0"},
  "targets": ["windows-x86_64"]
}"#;
        let err = Manifest::from_json_bytes(json.as_bytes()).unwrap_err();
        assert_eq!(*err.kind(), BundleErrorKind::WrongFormat);
    }

    #[test]
    fn malformed_json_is_an_error_not_a_panic() {
        for bad in [
            b"".as_slice(),
            b"not json",
            b"{",
            b"[]",
            b"null",
            b"{\"format\":",
            &[0xff, 0xfe, 0x00],
        ] {
            assert!(
                Manifest::from_json_bytes(bad).is_err(),
                "{bad:?} must be refused"
            );
        }
    }

    #[test]
    fn a_utf8_bom_is_tolerated() {
        let m = valid();
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(m.to_json().unwrap().as_bytes());
        assert!(Manifest::from_json_bytes(&bytes).is_ok());
    }

    #[test]
    fn plugin_ids_must_be_reverse_dns() {
        for good in ["com.example.gain", "org.a.b.c", "io.x.y2"] {
            validate_plugin_id(good).unwrap_or_else(|e| panic!("`{good}`: {e}"));
        }
        for bad in [
            "",
            "nodots",
            "com..gain",
            ".com.gain",
            "com.gain.",
            "com gain",
        ] {
            assert!(validate_plugin_id(bad).is_err(), "`{bad}` must be refused");
        }
    }

    #[test]
    fn versions_are_three_or_four_numbers() {
        assert_eq!(validate_version("1.2.3").unwrap(), [1, 2, 3, 0]);
        assert_eq!(validate_version("1.2.3.4").unwrap(), [1, 2, 3, 4]);
        for bad in ["1", "1.2", "v1.2.3", "1.2.3-beta", "1.2.x", ""] {
            assert!(validate_version(bad).is_err(), "`{bad}` must be refused");
        }
    }

    #[test]
    fn directory_names_default_when_unset() {
        let m = valid();
        assert_eq!(m.resource_dir_name(), "Resources");
        assert_eq!(m.library_dir_name(), "Library");

        let mut custom = valid();
        custom.resources = Some(ManifestResources {
            dir: "Assets".to_owned(),
            library_dir: "Deps".to_owned(),
            ..ManifestResources::default()
        });
        assert_eq!(custom.resource_dir_name(), "Assets");
        assert_eq!(custom.library_dir_name(), "Deps");
    }

    #[test]
    fn capability_names_map_to_their_bits_both_ways() {
        let mut caps = ManifestCaps::empty();
        for (name, _) in CAPABILITY_KEYS {
            assert_eq!(caps.get(name), Some(false), "{name}");
            assert!(caps.set_named(name, true), "{name}");
            assert_eq!(caps.get(name), Some(true), "{name}");
        }
        assert_eq!(caps.enabled_names().count(), CAPABILITY_KEYS.len());
        assert_eq!(caps.get("no-such-capability"), None);
        assert!(!caps.set_named("no-such-capability", true));
    }

    #[test]
    fn an_invalid_manifest_is_caught_by_check() {
        let mut m = valid();
        m.plugin.id = "not reverse dns".to_owned();
        assert!(m.check().is_err());

        let mut m = valid();
        m.format = "wrong".to_owned();
        assert!(m.check().is_err());
    }
}
