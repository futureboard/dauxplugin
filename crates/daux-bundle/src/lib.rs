//! The `.axt` bundle: its layout, its metadata, and safe access to what is inside it.
//!
//! A DAUx Audio Extension is a directory, not an archive. This crate is the only thing that
//! knows its shape — `docs/specifications/axt-v1.md` and `manifest-v1.md` are normative and
//! win over anything here.
//!
//! ```text
//! Gain.axt/
//!   manifest.json                 identity, targets, capabilities
//!   Content/windows-x86_64/       the plug-in binary, one directory per target
//!   Library/windows-x86_64/       bundled dependencies
//!   Resources/                    fonts, images, presets
//! ```
//!
//! Apple's tooling only understands its own arrangement, so [`BundleLayout::Apple`] exists
//! too; [`Bundle`] and [`BundleMetadata`] present both identically, and nothing above this
//! crate has to care which it is looking at.
//!
//! # Hostile input is the normal case
//!
//! A bundle is usually something the user downloaded. Every entry point here treats its input
//! as adversarial:
//!
//! - metadata over 4 MiB is refused from the directory entry, before it is read;
//! - parsing is bounded in depth, string length, and element count ([`limits`]);
//! - every resource lookup is confined to the bundle — `..`, absolute paths, drive letters,
//!   UNC prefixes, Windows device names and symlink escapes are all rejected;
//! - nothing panics on malformed data, and no allocation is sized by an untrusted number.
//!
//! # Reading and writing
//!
//! [`Bundle::open`] reads metadata and loads no code. [`BundleBuilder`] writes a bundle
//! through a staging directory, so an interrupted build never leaves a half-written `.axt`
//! for a scanner to find.

mod builder;
mod bundle;
mod error;
mod json_scan;
mod layout;
pub mod limits;
mod manifest;
mod metadata;
pub mod path_rules;
mod read;
mod target;
mod xml_scan;

#[cfg(test)]
mod testutil;

pub use builder::BundleBuilder;
pub use bundle::{Bundle, ResourceDir, Severity, ValidationIssue};
pub use error::{BundleError, BundleErrorKind, BundleResult};
pub use layout::BundleLayout;
pub use limits::{
    DEFAULT_MAX_RESOURCE_BYTES, FORMAT_SENTINEL, FORMAT_VERSION, MAX_METADATA_BYTES,
    MAX_STRING_BYTES, MAX_TARGETS,
};
pub use manifest::{
    CAPABILITY_KEYS, Category, GraphicsFramework, GraphicsPresentation, GraphicsRenderer,
    Manifest, ManifestCaps, ManifestGenerator, ManifestGraphics, ManifestPlugin,
    ManifestPluginRef, ManifestResources, validate_plugin_id, validate_version,
};
pub use metadata::BundleMetadata;
pub use target::TargetId;
