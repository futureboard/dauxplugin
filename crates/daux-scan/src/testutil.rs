//! Fixtures shared by this crate's tests.
//!
//! Everything here builds *real* artefacts with the real [`daux_bundle`] writer, because a
//! scanner tested against a hand-written directory tree would only prove that the test and
//! the scanner agree with each other.
//!
//! What the fixtures cannot provide is a loadable module: producing one means compiling a
//! `cdylib` that exports `daux_plugin_entry_v1`, which is a build-script's job and not a
//! unit test's. So the bundles here carry a *stand-in* binary — a file with the right name
//! in the right directory that the operating system will refuse to map — which is exactly
//! what a truncated download looks like, and exercises the failure path that matters most.

use std::path::{Path, PathBuf};

use daux_bundle::{Bundle, BundleBuilder, BundleMetadata, Manifest, TargetId};
use daux_runtime::daux_core::{PluginDescriptor, Version};

/// A clean, empty directory under the system temporary directory. [main-thread]
pub(crate) fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("daux-scan-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory");
    dir
}

/// A minimal valid descriptor. [main-thread]
pub(crate) fn descriptor(id: &str, name: &str, vendor: &str) -> PluginDescriptor {
    PluginDescriptor::builder(id, name)
        .vendor(vendor)
        .version(Version::new(1, 0, 0))
        .build()
        .expect("the fixture must itself be valid")
}

/// The layout-independent metadata a manifest with this identity produces. [main-thread]
pub(crate) fn metadata(id: &str, name: &str, vendor: &str, version: &str) -> BundleMetadata {
    metadata_with(id, name, vendor, version, |_| {})
}

/// As [`metadata`], with the manifest adjusted before it is flattened. [main-thread]
pub(crate) fn metadata_with(
    id: &str,
    name: &str,
    vendor: &str,
    version: &str,
    adjust: impl FnOnce(&mut Manifest),
) -> BundleMetadata {
    let mut manifest = Manifest::new(id, name, vendor, version).expect("a valid identity");
    adjust(&mut manifest);
    BundleMetadata::from_manifest(&manifest)
}

/// The file name a plug-in binary has for `target`. [main-thread]
///
/// Apple bundles carry an extensionless binary named after the bundle (`axt-v1` §3.2);
/// everything else uses the platform's dynamic-library extension.
fn binary_name(target: &TargetId, plugin_name: &str) -> String {
    let extension = target.dylib_extension();
    if extension.is_empty() {
        plugin_name.to_owned()
    } else {
        format!("{}.{extension}", plugin_name.to_lowercase())
    }
}

/// Writes a bundle carrying a stand-in binary for this machine. [main-thread]
///
/// Returns the bundle's root directory.
pub(crate) fn write_bundle(out_dir: &Path, id: &str, name: &str, version: &str) -> PathBuf {
    write_bundle_with(out_dir, id, name, version, |_| {})
}

/// As [`write_bundle`], with the manifest rewritten after the bundle is assembled.
/// [main-thread]
///
/// The rewrite goes through [`Manifest`] rather than a string replacement so the file stays
/// a manifest the reader will accept — the point of these fixtures is to test the scanner,
/// not the JSON writer.
pub(crate) fn write_bundle_with(
    out_dir: &Path,
    id: &str,
    name: &str,
    version: &str,
    adjust: impl FnOnce(&mut Manifest),
) -> PathBuf {
    let target = TargetId::host();
    let staging = out_dir.join(format!(".staging-{name}"));
    std::fs::create_dir_all(&staging).expect("a staging directory");
    let stand_in = staging.join(binary_name(&target, name));
    std::fs::write(&stand_in, b"this is not a dynamic library").expect("a stand-in binary");

    let root = BundleBuilder::new(id, name, "Example", version)
        .expect("a valid identity")
        .binary(target, &stand_in)
        .write(out_dir)
        .expect("the bundle writes");
    let _ = std::fs::remove_dir_all(&staging);

    let manifest_path = root.join("manifest.json");
    let bytes = std::fs::read(&manifest_path).expect("the manifest that was just written");
    let mut manifest = Manifest::from_json_bytes(&bytes).expect("and it parses");
    adjust(&mut manifest);
    std::fs::write(
        &manifest_path,
        manifest
            .to_json()
            .expect("the adjusted manifest serialises"),
    )
    .expect("rewrite");

    root
}

/// A bundle whose only binary is for a platform this process can never be. [main-thread]
pub(crate) fn bundle_without_binary(out_dir: &Path, id: &str, name: &str) -> (Bundle, PathBuf) {
    let foreign = TargetId::parse("aix-power64").expect("syntactically valid, never the host");
    let staging = out_dir.join(".staging-foreign");
    std::fs::create_dir_all(&staging).expect("a staging directory");
    let stand_in = staging.join("libgain.so");
    std::fs::write(&stand_in, b"for another machine entirely").expect("a stand-in binary");

    let root = BundleBuilder::new(id, name, "Example", "1.0.0")
        .expect("a valid identity")
        .binary(foreign, &stand_in)
        .write(out_dir)
        .expect("the bundle writes");
    let _ = std::fs::remove_dir_all(&staging);

    let bundle = Bundle::open(&root).expect("a well-formed bundle");
    (bundle, root)
}

/// A bundle whose binary is in the right place and is not a library. [main-thread]
pub(crate) fn bundle_with_fake_binary(out_dir: &Path, id: &str, name: &str) -> (Bundle, PathBuf) {
    let root = write_bundle(out_dir, id, name, "1.0.0");
    let bundle = Bundle::open(&root).expect("a well-formed bundle");
    (bundle, root)
}
