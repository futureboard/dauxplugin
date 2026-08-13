//! Manifest against binary: `manifest-v1` §8.1, row by row.
//!
//! A manifest is a file. Files get edited, copied from the wrong build, repackaged by a
//! third party, and hand-written by someone in a hurry. The binary is the plug-in. When they
//! disagree, the binary wins — but the disagreement is still worth reporting, because it
//! means the bundle a user installed is not the bundle its author tested.
//!
//! Two of the nine rows are fatal, and both are about **identity**:
//!
//! * `DAUX-M100` — the manifest's `plugin.id` is not the principal descriptor's id;
//! * `DAUX-M108` — the manifest's `plugin.id` is not exported by the factory at all.
//!
//! An entry that hits either one is never registered. A saved project stores a plug-in id
//! and expects to find the same plug-in behind it; a bundle that answers to an id it does
//! not implement turns "reopen the project" into "silently load something else".
//!
//! The remaining rows are recorded on the entry and change nothing about how it is used.

use daux_bundle::{BundleMetadata, ValidationIssue};
use daux_runtime::daux_core::Version;

use crate::error::{ScanError, ScanErrorKind, ScanResult};
use crate::isolation::ProbeOutcome;

/// Compares what the bundle claims with what the module reports. [main-thread]
///
/// Appends one issue per difference to `issues`, in row order.
///
/// # Errors
///
/// [`ScanErrorKind::Identity`] for the two fatal rows, which means the entry must not be
/// registered at all.
pub(crate) fn cross_check(
    metadata: &BundleMetadata,
    outcome: &ProbeOutcome,
    issues: &mut Vec<ValidationIssue>,
) -> ScanResult<()> {
    let Some(principal) = outcome.descriptors.first() else {
        // A factory that publishes nothing is not a plug-in bundle, whatever the manifest
        // says. Reported as an identity failure because the manifest's id resolves to
        // nothing — row 9 with an empty right-hand side.
        return Err(ScanError::new(
            ScanErrorKind::Identity,
            format!(
                "the manifest declares `{}` but the module's factory publishes no plug-in at \
                 all (manifest-v1 §8.1, DAUX-M108)",
                metadata.id
            ),
        ));
    };

    // Row 1 — fatal.
    if principal.id.as_str() != metadata.id {
        return Err(ScanError::new(
            ScanErrorKind::Identity,
            format!(
                "the manifest declares `{}` but the module's principal plug-in is `{}` \
                 (manifest-v1 §8.1, DAUX-M100)",
                metadata.id,
                principal.id.as_str()
            ),
        ));
    }

    // Row 9 — fatal. Redundant with row 1 for a single-plug-in bundle, and not redundant at
    // all for a bundle that publishes several: the id a project saved has to be one the
    // factory will actually answer to.
    if !outcome
        .descriptors
        .iter()
        .any(|descriptor| descriptor.id.as_str() == metadata.id)
    {
        return Err(ScanError::new(
            ScanErrorKind::Identity,
            format!(
                "the manifest declares `{}`, which the module's factory does not export \
                 (manifest-v1 §8.1, DAUX-M108)",
                metadata.id
            ),
        ));
    }

    // Row 2.
    if principal.name != metadata.name {
        issues.push(disagreement(
            "DAUX-M101",
            "plugin.name",
            &metadata.name,
            &principal.name,
        ));
    }

    // Row 3.
    if principal.vendor != metadata.vendor {
        issues.push(disagreement(
            "DAUX-M102",
            "plugin.vendor",
            &metadata.vendor,
            &principal.vendor,
        ));
    }

    // Row 4 — all four components, so a bundle that ships build 41 of version 1.0.0 next to
    // a manifest that says build 40 is caught.
    match Version::parse(&metadata.version) {
        Ok(declared) if declared.to_parts() == principal.version.to_parts() => {}
        Ok(declared) => issues.push(disagreement(
            "DAUX-M103",
            "plugin.version",
            &declared.to_string(),
            &principal.version.to_string(),
        )),
        Err(error) => issues.push(ValidationIssue::error(
            "DAUX-M103",
            format!(
                "plugin.version `{}` is not a version at all ({error}); the binary reports {} \
                 (manifest-v1 §8.1)",
                metadata.version, principal.version
            ),
        )),
    }

    // Row 5. A manifest that declared no category at all has nothing to disagree with, so
    // `None` is skipped rather than treated as `Unknown` — the two are different claims, and
    // reporting the first as a mismatch would fire on every hand-written minimal manifest.
    if let Some(declared) = metadata.category
        && declared.slug() != principal.category.as_str()
    {
        issues.push(disagreement(
            "DAUX-M104",
            "plugin.category",
            declared.slug(),
            principal.category.as_str(),
        ));
    }

    // Row 6 — the whole bitset, not only the bits this build knows: a capability a newer
    // SDK added is still a capability the manifest is wrong about.
    if metadata.capabilities.bits() != principal.capabilities.bits() {
        issues.push(disagreement(
            "DAUX-M105",
            "capabilities",
            &format!("{:#018x}", metadata.capabilities.bits()),
            &format!("{:#018x}", principal.capabilities.bits()),
        ));
    }

    // Row 7 — this is the one that makes a pre-load filter safe: a manifest that
    // under-reports the ABI would have been let through by `§8.3` and is caught here.
    if metadata.abi_version != outcome.abi_version.0 {
        issues.push(disagreement(
            "DAUX-M106",
            "abiVersion",
            &metadata.abi_version.to_string(),
            &outcome.abi_version.0.to_string(),
        ));
    }

    // Row 8 — a bundle that advertises an editor it does not have makes a host reserve a
    // window that never opens, and one that hides an editor it does have makes the editor
    // unreachable.
    let declares_editor = metadata
        .graphics
        .as_ref()
        .is_some_and(|graphics| graphics.enabled);
    if declares_editor != principal.capabilities.is_has_gui() {
        issues.push(disagreement(
            "DAUX-M107",
            "graphics.enabled",
            &declares_editor.to_string(),
            &principal.capabilities.is_has_gui().to_string(),
        ));
    }

    Ok(())
}

/// One row's finding, with both values side by side — which `manifest-v1` §8.2 requires of
/// `daux validate` and is just as useful in a host's log.
fn disagreement(code: &'static str, field: &str, manifest: &str, binary: &str) -> ValidationIssue {
    ValidationIssue::error(
        code,
        format!("{field}: the manifest says `{manifest}`, the binary says `{binary}`"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{descriptor, metadata, metadata_with};
    use daux_bundle::{ManifestGraphics, Severity};
    use daux_runtime::daux_core::Capabilities;
    use daux_runtime::daux_core::PluginDescriptor;

    fn outcome(descriptors: Vec<PluginDescriptor>) -> ProbeOutcome {
        ProbeOutcome {
            descriptors,
            abi_version: (1, 0),
        }
    }

    fn matching() -> (BundleMetadata, ProbeOutcome) {
        let metadata = metadata("com.example.gain", "Gain", "Example", "1.2.3");
        let outcome = outcome(vec![
            PluginDescriptor::builder("com.example.gain", "Gain")
                .vendor("Example")
                .version(Version::new(1, 2, 3))
                .build()
                .expect("valid"),
        ]);
        (metadata, outcome)
    }

    #[test]
    fn a_bundle_that_agrees_with_itself_produces_nothing() {
        let (metadata, outcome) = matching();
        let mut issues = Vec::new();
        cross_check(&metadata, &outcome, &mut issues).expect("no fatal row");
        assert!(issues.is_empty(), "unexpected issues: {issues:?}");
    }

    /// The rule that protects saved projects: a bundle that answers to an id it does not
    /// implement must never be registered, because a project stores the id and would
    /// silently load a different plug-in.
    #[test]
    fn an_id_the_binary_does_not_implement_is_fatal() {
        let metadata = metadata("com.example.gain", "Gain", "Example", "1.0.0");
        let outcome = outcome(vec![descriptor("com.example.reverb", "Reverb", "Example")]);
        let mut issues = Vec::new();
        let error = cross_check(&metadata, &outcome, &mut issues).expect_err("fatal");
        assert_eq!(error.kind(), ScanErrorKind::Identity);
        assert!(error.message().contains("DAUX-M100"), "{error}");
    }

    /// A multi-plug-in bundle whose manifest names a plug-in the factory does not export at
    /// all is row 9 rather than row 1, and equally fatal.
    #[test]
    fn a_manifest_id_absent_from_a_multi_plug_in_factory_is_fatal() {
        let mut metadata = metadata("com.example.suite.missing", "Suite", "Example", "1.0.0");
        metadata.id = "com.example.suite.missing".to_owned();
        let outcome = outcome(vec![
            descriptor("com.example.suite", "Suite", "Example"),
            descriptor("com.example.suite.eq", "EQ", "Example"),
        ]);
        let mut issues = Vec::new();
        let error = cross_check(&metadata, &outcome, &mut issues).expect_err("fatal");
        assert_eq!(error.kind(), ScanErrorKind::Identity);
    }

    #[test]
    fn a_factory_that_publishes_nothing_is_not_a_plug_in() {
        let metadata = metadata("com.example.gain", "Gain", "Example", "1.0.0");
        let mut issues = Vec::new();
        let error = cross_check(&metadata, &outcome(Vec::new()), &mut issues).expect_err("fatal");
        assert_eq!(error.kind(), ScanErrorKind::Identity);
        assert!(error.message().contains("DAUX-M108"), "{error}");
    }

    /// Everything that is not identity is recorded and survived: the plug-in still works,
    /// and the packaging bug still has to reach someone.
    #[test]
    fn display_text_differences_are_recorded_and_not_fatal() {
        let metadata = metadata("com.example.gain", "Gain", "Example", "1.2.3");
        let outcome = outcome(vec![
            PluginDescriptor::builder("com.example.gain", "Gain Pro")
                .vendor("Example Audio")
                .version(Version::new(1, 2, 4))
                .build()
                .expect("valid"),
        ]);
        let mut issues = Vec::new();
        cross_check(&metadata, &outcome, &mut issues).expect("not fatal");

        let codes: Vec<&str> = issues.iter().map(|issue| issue.code).collect();
        assert_eq!(codes, ["DAUX-M101", "DAUX-M102", "DAUX-M103"]);
        assert!(issues.iter().all(|issue| issue.severity == Severity::Error));
        // Both sides are shown, which is what makes the report actionable.
        assert!(issues[0].message.contains("Gain"), "{:?}", issues[0]);
        assert!(issues[0].message.contains("Gain Pro"), "{:?}", issues[0]);
    }

    /// Row 5. A manifest that files a synth under "effect" puts it in the wrong browser
    /// heading, where a user looking for an instrument will not find it.
    #[test]
    fn a_category_the_binary_disagrees_with_is_reported() {
        let mut metadata = metadata_with(
            "com.example.synth",
            "Synth",
            "Example",
            "1.0.0",
            |manifest| {
                manifest.plugin.category = Some(daux_bundle::Category::Effect);
            },
        );
        metadata.category = Some(daux_bundle::Category::Effect);

        let outcome = outcome(vec![
            PluginDescriptor::builder("com.example.synth", "Synth")
                .vendor("Example")
                .version(Version::new(1, 0, 0))
                .category(daux_runtime::daux_core::Category::Instrument)
                .build()
                .expect("valid"),
        ]);

        let mut issues = Vec::new();
        cross_check(&metadata, &outcome, &mut issues).expect("not fatal");
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].code, "DAUX-M104");
        assert!(issues[0].message.contains("effect"), "{:?}", issues[0]);
        assert!(issues[0].message.contains("instrument"), "{:?}", issues[0]);
    }

    /// A manifest that declared no category has made no claim, so there is nothing to
    /// contradict. Treating the absent value as `unknown` would raise `DAUX-M104` on every
    /// minimal hand-written manifest, and a warning that always fires is a warning nobody
    /// reads.
    #[test]
    fn a_manifest_with_no_category_is_not_a_disagreement() {
        let mut metadata = metadata("com.example.synth", "Synth", "Example", "1.0.0");
        metadata.category = None;

        let outcome = outcome(vec![
            PluginDescriptor::builder("com.example.synth", "Synth")
                .vendor("Example")
                .version(Version::new(1, 0, 0))
                .category(daux_runtime::daux_core::Category::Instrument)
                .build()
                .expect("valid"),
        ]);

        let mut issues = Vec::new();
        cross_check(&metadata, &outcome, &mut issues).expect("not fatal");
        assert!(issues.is_empty(), "{issues:?}");
    }

    /// The build number is part of the version. A manifest copied from the previous build
    /// is the most common packaging mistake there is.
    #[test]
    fn a_version_that_differs_only_in_its_build_number_is_caught() {
        let metadata = metadata("com.example.gain", "Gain", "Example", "1.0.0.40");
        let outcome = outcome(vec![
            PluginDescriptor::builder("com.example.gain", "Gain")
                .vendor("Example")
                .version(Version::new(1, 0, 0).with_build(41))
                .build()
                .expect("valid"),
        ]);
        let mut issues = Vec::new();
        cross_check(&metadata, &outcome, &mut issues).expect("not fatal");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].code, "DAUX-M103");
    }

    #[test]
    fn a_capability_or_abi_disagreement_is_reported_with_both_values() {
        // The manifest promises an editor and a capability set the binary does not have.
        let mut metadata =
            metadata_with("com.example.gain", "Gain", "Example", "1.0.0", |manifest| {
                manifest.graphics = Some(ManifestGraphics::default());
            });
        metadata.capabilities = daux_bundle::ManifestCaps::from_bits(
            Capabilities::AUDIO_EFFECT
                .union(Capabilities::HAS_GUI)
                .bits(),
        );
        metadata.abi_version = 1;

        let mut outcome = outcome(vec![
            PluginDescriptor::builder("com.example.gain", "Gain")
                .vendor("Example")
                .version(Version::new(1, 0, 0))
                .capabilities(Capabilities::AUDIO_EFFECT)
                .build()
                .expect("valid"),
        ]);
        outcome.abi_version = (2, 0);

        let mut issues = Vec::new();
        cross_check(&metadata, &outcome, &mut issues).expect("not fatal");
        let codes: Vec<&str> = issues.iter().map(|issue| issue.code).collect();
        assert_eq!(codes, ["DAUX-M105", "DAUX-M106", "DAUX-M107"]);
        assert!(
            issues[2].message.contains("true") && issues[2].message.contains("false"),
            "the editor row must show both answers: {:?}",
            issues[2]
        );
    }

    /// A bundle whose manifest declares an editor block with `enabled: false` and a binary
    /// that has no editor agree, and must produce nothing.
    #[test]
    fn a_disabled_graphics_block_agrees_with_a_headless_binary() {
        let metadata = metadata_with("com.example.gain", "Gain", "Example", "1.0.0", |manifest| {
            manifest.graphics = Some(ManifestGraphics {
                enabled: false,
                ..ManifestGraphics::default()
            });
        });
        let outcome = outcome(vec![
            PluginDescriptor::builder("com.example.gain", "Gain")
                .vendor("Example")
                .version(Version::new(1, 0, 0))
                .build()
                .expect("valid"),
        ]);
        let mut issues = Vec::new();
        cross_check(&metadata, &outcome, &mut issues).expect("not fatal");
        assert!(issues.is_empty(), "{issues:?}");
    }
}
