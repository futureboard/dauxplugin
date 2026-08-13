//! Everything a host can know about a plug-in without instantiating it.

use daux_audio::{SampleFormat, SampleFormats};

use crate::{Capabilities, Category, DauxError, DauxResult, ErrorKind, PluginId, Version};

/// The static, instance-independent description of one plug-in.
///
/// A descriptor is what a scanner caches, what a browser lists, and what an adapter
/// translates into a VST3 `PClassInfo` or a CLAP `clap_plugin_descriptor`. It must be
/// derivable without creating an instance and must not change over the lifetime of a build.
///
/// # Stability
///
/// [`PluginDescriptor::id`] and the parameter ids reachable from the plug-in are **permanent**
/// (abi-v1 §5). Everything else — name, vendor, description, category, even the capability
/// set — may change between versions. Renaming is free; renumbering silently corrupts saved
/// projects.
///
/// [main-thread]
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct PluginDescriptor {
    /// Permanent reverse-DNS identity, e.g. `com.example.gain`.
    pub id: PluginId,
    /// Human-readable product name shown in a host's browser.
    pub name: String,
    /// Human-readable vendor name.
    pub vendor: String,
    /// Product version, independent of the ABI version.
    pub version: Version,
    /// One or two sentences describing what the plug-in does. May be empty.
    pub description: String,
    /// Product home page. May be empty.
    pub url: String,
    /// Support contact URL. May be empty.
    pub support_url: String,
    /// Copyright line. May be empty.
    pub copyright: String,
    /// SPDX licence expression, or a free-form licence name. May be empty.
    pub license: String,
    /// Primary role, a browser hint only.
    pub category: Category,
    /// What the plug-in can do, as a bitset.
    pub capabilities: Capabilities,
    /// Free-form searchable feature tags, e.g. `"reverb"`, `"stereo"`, `"mastering"`.
    ///
    /// These map onto CLAP's `features` array and onto VST3's subcategory string. They are
    /// hints, never load-bearing.
    pub features: Vec<String>,
    /// Which sample formats [`process`](crate::DauxProcessor::process) accepts. Never empty
    /// in a valid descriptor.
    pub sample_formats: SampleFormats,
    /// Version of this plug-in's own state schema, used to select a migration chain.
    pub state_schema_version: u32,
    /// The oldest DAUx ABI `(major, minor)` this plug-in can be loaded over.
    pub min_abi: (u32, u32),
}

impl PluginDescriptor {
    /// [main-thread] Starts building a descriptor from the two fields that have no sensible
    /// default.
    ///
    /// The id is validated when [`PluginDescriptorBuilder::build`] is called, not here, so
    /// that a builder chain reads without a `?` on every line.
    pub fn builder(id: &str, name: &str) -> PluginDescriptorBuilder {
        PluginDescriptorBuilder::new(id, name)
    }

    /// [main-thread] Checks the invariants a host is entitled to rely on.
    ///
    /// Called by [`PluginDescriptorBuilder::build`], by the AXT/VST3/CLAP adapters before
    /// they publish a descriptor, and by `daux validate`.
    pub fn validate(&self) -> DauxResult<()> {
        PluginId::validate(self.id.as_str())?;
        if self.name.trim().is_empty() {
            return Err(DauxError::new(
                ErrorKind::InvalidArgument,
                "plug-in name must not be empty",
            ));
        }
        if self.sample_formats.is_empty() {
            return Err(DauxError::new(
                ErrorKind::InvalidArgument,
                format!(
                    "plug-in `{}` declares no sample format; at least f32 is required",
                    self.id
                ),
            ));
        }
        if !self.sample_formats.contains(SampleFormat::F32) {
            return Err(DauxError::new(
                ErrorKind::Unsupported,
                format!(
                    "plug-in `{}` does not support f32; every DAUx plug-in must (abi-v1 §8)",
                    self.id
                ),
            ));
        }
        if self.min_abi.0 == 0 {
            return Err(DauxError::new(
                ErrorKind::AbiMismatch,
                format!(
                    "plug-in `{}` declares min ABI major 0; v1 is the first release",
                    self.id
                ),
            ));
        }
        for feature in &self.features {
            if feature.trim().is_empty() {
                return Err(DauxError::new(
                    ErrorKind::InvalidArgument,
                    format!("plug-in `{}` declares an empty feature tag", self.id),
                ));
            }
        }
        Ok(())
    }

    /// [main-thread] `true` when this plug-in can process `format`.
    pub fn supports(&self, format: SampleFormat) -> bool {
        self.sample_formats.contains(format)
    }

    /// [main-thread] `true` when this plug-in can be loaded over ABI `(major, minor)`.
    pub fn loadable_over_abi(&self, major: u32, minor: u32) -> bool {
        major == self.min_abi.0 && minor >= self.min_abi.1
    }
}

/// Builder for [`PluginDescriptor`], returned by [`PluginDescriptor::builder`].
///
/// Every optional field starts empty or at its most conservative value, so a descriptor built
/// from `builder(id, name).build()` is minimal but valid.
#[derive(Clone, Debug)]
pub struct PluginDescriptorBuilder {
    id: String,
    name: String,
    vendor: String,
    version: Version,
    description: String,
    url: String,
    support_url: String,
    copyright: String,
    license: String,
    category: Category,
    capabilities: Capabilities,
    features: Vec<String>,
    sample_formats: SampleFormats,
    state_schema_version: u32,
    min_abi: (u32, u32),
}

impl PluginDescriptorBuilder {
    /// [main-thread] A builder carrying only the permanent id and the display name.
    pub fn new(id: &str, name: &str) -> Self {
        Self {
            id: id.to_owned(),
            name: name.to_owned(),
            vendor: String::new(),
            version: Version::new(0, 1, 0),
            description: String::new(),
            url: String::new(),
            support_url: String::new(),
            copyright: String::new(),
            license: String::new(),
            category: Category::Effect,
            capabilities: Capabilities::NONE,
            features: Vec::new(),
            sample_formats: SampleFormats::F32,
            state_schema_version: 1,
            min_abi: (1, 0),
        }
    }

    /// Sets the vendor name.
    #[must_use]
    pub fn vendor(mut self, vendor: impl Into<String>) -> Self {
        self.vendor = vendor.into();
        self
    }

    /// Sets the product version.
    #[must_use]
    pub fn version(mut self, version: impl Into<Version>) -> Self {
        self.version = version.into();
        self
    }

    /// Sets the long description.
    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// Sets the product home page.
    #[must_use]
    pub fn url(mut self, url: impl Into<String>) -> Self {
        self.url = url.into();
        self
    }

    /// Sets the support contact URL.
    #[must_use]
    pub fn support_url(mut self, url: impl Into<String>) -> Self {
        self.support_url = url.into();
        self
    }

    /// Sets the copyright line.
    #[must_use]
    pub fn copyright(mut self, copyright: impl Into<String>) -> Self {
        self.copyright = copyright.into();
        self
    }

    /// Sets the licence expression.
    #[must_use]
    pub fn license(mut self, license: impl Into<String>) -> Self {
        self.license = license.into();
        self
    }

    /// Sets the browser category.
    #[must_use]
    pub fn category(mut self, category: Category) -> Self {
        self.category = category;
        self
    }

    /// Replaces the capability bitset.
    #[must_use]
    pub fn capabilities(mut self, capabilities: Capabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Adds capability bits to the ones already set.
    #[must_use]
    pub fn with_capabilities(mut self, capabilities: Capabilities) -> Self {
        self.capabilities |= capabilities;
        self
    }

    /// Appends one searchable feature tag.
    #[must_use]
    pub fn feature(mut self, feature: impl Into<String>) -> Self {
        self.features.push(feature.into());
        self
    }

    /// Appends several feature tags.
    #[must_use]
    pub fn features<I, S>(mut self, features: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.features.extend(features.into_iter().map(Into::into));
        self
    }

    /// Declares which sample formats `process` accepts.
    #[must_use]
    pub fn sample_formats(mut self, formats: impl Into<SampleFormats>) -> Self {
        self.sample_formats = formats.into();
        self
    }

    /// Declares the version of this plug-in's own state schema.
    #[must_use]
    pub fn state_schema_version(mut self, version: u32) -> Self {
        self.state_schema_version = version;
        self
    }

    /// Declares the oldest ABI `(major, minor)` this plug-in can be loaded over.
    #[must_use]
    pub fn min_abi(mut self, major: u32, minor: u32) -> Self {
        self.min_abi = (major, minor);
        self
    }

    /// [main-thread] Validates the id and the rest of the descriptor, then produces it.
    pub fn build(self) -> DauxResult<PluginDescriptor> {
        let descriptor = PluginDescriptor {
            id: PluginId::new(self.id)?,
            name: self.name,
            vendor: self.vendor,
            version: self.version,
            description: self.description,
            url: self.url,
            support_url: self.support_url,
            copyright: self.copyright,
            license: self.license,
            category: self.category,
            capabilities: self.capabilities,
            features: self.features,
            sample_formats: self.sample_formats,
            state_schema_version: self.state_schema_version,
            min_abi: self.min_abi,
        };
        descriptor.validate()?;
        Ok(descriptor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal() -> PluginDescriptor {
        PluginDescriptor::builder("com.example.gain", "Gain")
            .build()
            .expect("minimal descriptor is valid")
    }

    #[test]
    fn a_minimal_descriptor_is_valid() {
        let d = minimal();
        assert_eq!(d.id, "com.example.gain");
        assert_eq!(d.name, "Gain");
        assert_eq!(d.category, Category::Effect);
        assert!(d.supports(SampleFormat::F32));
        assert!(!d.supports(SampleFormat::F64));
        assert_eq!(d.min_abi, (1, 0));
        assert_eq!(d.state_schema_version, 1);
        d.validate().unwrap();
    }

    #[test]
    fn the_builder_sets_every_field() {
        let d = PluginDescriptor::builder("com.example.verb", "Verb")
            .vendor("Example Audio")
            .version(Version::new(2, 1, 3))
            .description("A plate reverb.")
            .url("https://example.com/verb")
            .support_url("https://example.com/support")
            .copyright("(c) 2026 Example Audio")
            .license("MIT OR Apache-2.0")
            .category(Category::Effect)
            .capabilities(Capabilities::NONE)
            .with_capabilities(Capabilities::NONE)
            .features(["reverb", "stereo"])
            .feature("mastering")
            .sample_formats(SampleFormats::BOTH)
            .state_schema_version(4)
            .min_abi(1, 2)
            .build()
            .unwrap();

        assert_eq!(d.vendor, "Example Audio");
        assert_eq!(d.version, Version::new(2, 1, 3));
        assert_eq!(d.license, "MIT OR Apache-2.0");
        assert_eq!(d.features, ["reverb", "stereo", "mastering"]);
        assert!(d.supports(SampleFormat::F64));
        assert_eq!(d.state_schema_version, 4);
        assert_eq!(d.min_abi, (1, 2));
    }

    #[test]
    fn an_invalid_id_is_rejected_at_build_time() {
        let err = PluginDescriptor::builder("not a valid id", "X")
            .build()
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidArgument);
    }

    #[test]
    fn an_empty_name_is_rejected() {
        let err = PluginDescriptor::builder("com.example.x", "   ")
            .build()
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidArgument);
        assert!(err.message().contains("name"));
    }

    #[test]
    fn f32_support_is_mandatory() {
        let err = PluginDescriptor::builder("com.example.x", "X")
            .sample_formats(SampleFormats::F64)
            .build()
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Unsupported);

        let err = PluginDescriptor::builder("com.example.x", "X")
            .sample_formats(SampleFormats::NONE)
            .build()
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidArgument);
    }

    #[test]
    fn an_empty_feature_tag_is_rejected() {
        let err = PluginDescriptor::builder("com.example.x", "X")
            .feature("  ")
            .build()
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidArgument);
        assert!(err.message().contains("feature"));
    }

    #[test]
    fn abi_zero_is_rejected() {
        let err = PluginDescriptor::builder("com.example.x", "X")
            .min_abi(0, 1)
            .build()
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::AbiMismatch);
    }

    #[test]
    fn loadability_needs_the_same_major_and_at_least_the_minor() {
        let d = PluginDescriptor::builder("com.example.x", "X")
            .min_abi(1, 2)
            .build()
            .unwrap();
        assert!(d.loadable_over_abi(1, 2));
        assert!(d.loadable_over_abi(1, 7));
        assert!(!d.loadable_over_abi(1, 1));
        assert!(!d.loadable_over_abi(2, 9));
    }
}
