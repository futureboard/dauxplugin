//! `daux inspect` — what is inside a bundle.
//!
//! Two sources of truth meet here. The manifest describes the bundle *before* any code
//! runs; the factory's descriptors describe the plug-in once the binary is open. `manifest-v1`
//! §8.2 is explicit about what a tool must do when they disagree: show both, and label
//! which is which. Silently preferring either one hides the packaging bug that produced the
//! difference.

use daux_bundle::{Bundle, BundleMetadata, TargetId};
use daux_runtime::daux_core::PluginDescriptor;
use daux_scan::{ScanEntry, Scanner};

use crate::cli::InspectArgs;
use crate::cmd::{descriptor_json, open_bundle, print_descriptor, print_issues};
use crate::exit::Exit;
use crate::out::{IssueCounts, Out, issues_json};

/// One place the manifest and the binary disagree. [main-thread]
#[derive(Clone, Debug, PartialEq, Eq)]
struct Difference {
    field: &'static str,
    manifest: String,
    binary: String,
}

/// [main-thread] Runs `daux inspect`.
///
/// # Errors
///
/// When the bundle cannot be opened.
pub fn run(args: &InspectArgs, out: &Out) -> anyhow::Result<Exit> {
    let bundle = open_bundle(&args.bundle)?;
    let entry = if args.no_probe {
        None
    } else {
        probe(&args.bundle, out)
    };

    let metadata = bundle.metadata();
    let descriptors: &[PluginDescriptor] = entry
        .as_ref()
        .map_or(&[], |entry| entry.descriptors.as_slice());
    let issues = entry
        .as_ref()
        .map_or_else(|| bundle.validate(), |entry| entry.issues.clone());
    let differences = descriptors
        .first()
        .map(|descriptor| differences(metadata, descriptor))
        .unwrap_or_default();

    if out.is_json() {
        return emit_json(args, out, &bundle, descriptors, &issues, &differences);
    }

    out.heading(args.bundle.display().to_string());
    out.field("layout", bundle.layout().as_str());
    out.field("id", &metadata.id);
    out.field("name", &metadata.name);
    out.field("vendor", &metadata.vendor);
    out.field("version", &metadata.version);
    if !metadata.description.is_empty() {
        out.field("description", &metadata.description);
    }
    out.field("format version", metadata.format_version);
    out.field(
        "abi version",
        format!("{}.{}", metadata.abi_version, metadata.abi_version_minor),
    );
    out.field("targets", target_summary(&bundle));
    out.field("capabilities", manifest_capability_list(metadata));
    out.opt_field("graphics", graphics_summary(metadata));
    out.field(
        "resources",
        if bundle.resources().root().is_dir() {
            format!("{} (present)", metadata.resource_dir_name)
        } else {
            format!("{} (absent)", metadata.resource_dir_name)
        },
    );

    out.blank();
    if descriptors.is_empty() {
        out.heading(if args.no_probe {
            "plug-ins: not enumerated (`--no-probe`); the manifest describes only the principal one"
        } else {
            "plug-ins: the binary could not be opened; every value above comes from the manifest"
        });
    } else {
        out.heading("plug-ins (from the binary, which is authoritative once loaded)");
        for (index, descriptor) in descriptors.iter().enumerate() {
            print_descriptor(out, index, descriptor);
        }
    }

    if !differences.is_empty() {
        out.blank();
        out.heading("manifest and binary disagree");
        for difference in &differences {
            out.line(format!(
                "  {:<14}manifest: {}   binary: {}",
                difference.field, difference.manifest, difference.binary
            ));
        }
    }

    let counts = IssueCounts::of(&issues);
    if !issues.is_empty() {
        out.blank();
        out.heading("findings");
        print_issues(out, &issues);
        out.blank();
        out.line(counts.summary());
    }

    Ok(Exit::from_issues(counts.errors > 0))
}

/// The JSON document, which carries both sources for exactly the same reason the prose does.
fn emit_json(
    args: &InspectArgs,
    out: &Out,
    bundle: &Bundle,
    descriptors: &[PluginDescriptor],
    issues: &[daux_bundle::ValidationIssue],
    differences: &[Difference],
) -> anyhow::Result<Exit> {
    let metadata = bundle.metadata();
    let counts = IssueCounts::of(issues);
    let targets: Vec<serde_json::Value> = metadata
        .targets
        .iter()
        .map(|target| {
            serde_json::json!({
                "target": target.as_str(),
                "binary": bundle
                    .binary_path(target)
                    .ok()
                    .map(|path| path.display().to_string()),
            })
        })
        .collect();

    out.emit(&serde_json::json!({
        "ok": counts.errors == 0,
        "bundle": args.bundle.display().to_string(),
        "layout": bundle.layout().as_str(),
        "manifest": {
            "id": metadata.id,
            "name": metadata.name,
            "vendor": metadata.vendor,
            "version": metadata.version,
            "description": metadata.description,
            "formatVersion": metadata.format_version,
            "abiVersion": metadata.abi_version,
            "abiVersionMinor": metadata.abi_version_minor,
            "targets": targets,
            "capabilities": metadata.capabilities.enabled_names().collect::<Vec<_>>(),
            "hasEditor": metadata.has_editor(),
        },
        "probed": !descriptors.is_empty(),
        "plugins": descriptors.iter().map(descriptor_json).collect::<Vec<_>>(),
        "differences": differences
            .iter()
            .map(|difference| serde_json::json!({
                "field": difference.field,
                "manifest": difference.manifest,
                "binary": difference.binary,
            }))
            .collect::<Vec<_>>(),
        "issues": issues_json(issues),
    }))?;
    Ok(Exit::from_issues(counts.errors > 0))
}

/// Opens the binary, or explains once why it could not and carries on.
fn probe(path: &std::path::Path, out: &Out) -> Option<ScanEntry> {
    let mut scanner = Scanner::new();
    scanner.clear_search_paths();
    scanner.set_probe(true);
    match scanner.inspect(path) {
        Ok(entry) => Some(entry),
        Err(error) => {
            out.warn(format!(
                "the binary was not opened ({error}); everything below comes from the \
                 manifest, which is user-writable"
            ));
            None
        }
    }
}

/// The rows of `manifest-v1` §8.1 that a human can act on, compared side by side.
fn differences(metadata: &BundleMetadata, descriptor: &PluginDescriptor) -> Vec<Difference> {
    let mut differences = Vec::new();
    let mut compare = |field: &'static str, manifest: &str, binary: &str| {
        if manifest != binary {
            differences.push(Difference {
                field,
                manifest: manifest.to_owned(),
                binary: binary.to_owned(),
            });
        }
    };
    compare("id", &metadata.id, descriptor.id.as_str());
    compare("name", &metadata.name, &descriptor.name);
    compare("vendor", &metadata.vendor, &descriptor.vendor);
    compare(
        "version",
        &metadata.version,
        &descriptor.version.to_string(),
    );

    if metadata.capabilities.bits() != descriptor.capabilities.bits() {
        differences.push(Difference {
            field: "capabilities",
            manifest: format!("0x{:x}", metadata.capabilities.bits()),
            binary: format!("0x{:x}", descriptor.capabilities.bits()),
        });
    }
    differences
}

/// `windows-x86_64 (binary present), linux-x86_64 (no binary)`.
fn target_summary(bundle: &Bundle) -> String {
    let host = TargetId::host();
    let parts: Vec<String> = bundle
        .metadata()
        .targets
        .iter()
        .map(|target| {
            let state = if bundle.binary_path(target).is_ok() {
                "binary present"
            } else {
                "no binary"
            };
            let here = if *target == host {
                ", this machine"
            } else {
                ""
            };
            format!("{target} ({state}{here})")
        })
        .collect();
    if parts.is_empty() {
        "none declared".to_owned()
    } else {
        parts.join(", ")
    }
}

/// The manifest's capability names, comma-separated.
fn manifest_capability_list(metadata: &BundleMetadata) -> String {
    let names: Vec<&str> = metadata.capabilities.enabled_names().collect();
    if names.is_empty() {
        "none declared".to_owned()
    } else {
        names.join(", ")
    }
}

/// `gpui / wgpu, 1100x700 logical, resizable`.
fn graphics_summary(metadata: &BundleMetadata) -> Option<String> {
    let graphics = metadata.graphics.as_ref()?;
    let framework = graphics.framework.map_or_else(
        || "?".to_owned(),
        |framework| format!("{framework:?}").to_lowercase(),
    );
    let renderer = graphics.renderer.map_or_else(
        || "?".to_owned(),
        |renderer| format!("{renderer:?}").to_lowercase(),
    );
    Some(format!(
        "{framework} / {renderer}, {}x{} logical, {}",
        graphics.width,
        graphics.height,
        if graphics.resizable {
            "resizable"
        } else {
            "fixed size"
        }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use daux_bundle::{Manifest, ManifestCaps};
    use daux_runtime::daux_core::{Capabilities, Version};

    fn metadata(id: &str, name: &str, version: &str, caps: ManifestCaps) -> BundleMetadata {
        let mut manifest =
            Manifest::new(id, name, "Example Audio", version).expect("a valid identity");
        manifest.capabilities = caps;
        manifest.targets = vec![TargetId::host()];
        BundleMetadata::from_manifest(&manifest)
    }

    fn descriptor(id: &str, name: &str, version: Version, caps: Capabilities) -> PluginDescriptor {
        PluginDescriptor::builder(id, name)
            .vendor("Example Audio")
            .version(version)
            .capabilities(caps)
            .build()
            .expect("a valid identity")
    }

    /// A bundle that agrees with itself produces no "these disagree" section, which is what
    /// makes the section worth reading when it does appear.
    #[test]
    fn a_consistent_bundle_reports_no_differences() {
        let found = differences(
            &metadata("com.example.gain", "Gain", "1.2.3", ManifestCaps::empty()),
            &descriptor(
                "com.example.gain",
                "Gain",
                Version::new(1, 2, 3),
                Capabilities::NONE,
            ),
        );
        assert!(found.is_empty(), "{found:#?}");
    }

    /// `manifest-v1` §8.2: every difference is shown with both values labelled. A tool that
    /// printed only one of them would make a stale manifest invisible.
    #[test]
    fn every_disagreement_is_reported_with_both_values() {
        let found = differences(
            &metadata("com.example.gain", "Gain", "1.2.3", ManifestCaps::empty()),
            &descriptor(
                "com.example.other",
                "Renamed",
                Version::new(2, 0, 0),
                Capabilities::NONE.with_audio_effect(),
            ),
        );
        let fields: Vec<&str> = found.iter().map(|difference| difference.field).collect();
        assert_eq!(fields, ["id", "name", "version", "capabilities"]);

        let id = &found[0];
        assert_eq!(id.manifest, "com.example.gain");
        assert_eq!(id.binary, "com.example.other");
        let capabilities = found.last().expect("the capability row");
        assert_ne!(capabilities.manifest, capabilities.binary);
    }

    /// The four-component version is what DAUx orders by, so `1.2.3` and `1.2.3.4` are
    /// different versions and must be reported as a difference rather than smoothed over.
    #[test]
    fn a_build_number_counts_as_a_version_difference() {
        let found = differences(
            &metadata("com.example.gain", "Gain", "1.2.3", ManifestCaps::empty()),
            &descriptor(
                "com.example.gain",
                "Gain",
                Version::new(1, 2, 3).with_build(4),
                Capabilities::NONE,
            ),
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].field, "version");
    }

    #[test]
    fn a_bundle_with_no_declared_capability_says_so_rather_than_printing_nothing() {
        let bare = metadata("com.example.gain", "Gain", "1.0.0", ManifestCaps::empty());
        assert_eq!(manifest_capability_list(&bare), "none declared");

        let with_caps = metadata(
            "com.example.gain",
            "Gain",
            "1.0.0",
            ManifestCaps::empty().with(daux_abi::DAUX_CAP_AUDIO_EFFECT),
        );
        assert_eq!(manifest_capability_list(&with_caps), "audioEffect");
    }

    #[test]
    fn a_headless_bundle_has_no_graphics_line() {
        let bare = metadata("com.example.gain", "Gain", "1.0.0", ManifestCaps::empty());
        assert_eq!(graphics_summary(&bare), None);
    }
}
