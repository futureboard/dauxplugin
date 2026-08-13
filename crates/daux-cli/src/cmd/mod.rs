//! The subcommands, one module each, plus the few things more than one of them needs.

pub mod build;
pub mod bundle;
pub mod inspect;
pub mod new;
pub mod run;
pub mod scan;
pub mod test;
pub mod validate;

use std::path::Path;

use anyhow::{Context as _, anyhow};
use daux_bundle::{Bundle, Severity, ValidationIssue};
use daux_runtime::daux_core::PluginDescriptor;

use crate::out::Out;

/// [main-thread] Opens a bundle, or explains why it could not be.
///
/// The two mistakes a user actually makes — pointing at a directory that is not a bundle,
/// and pointing at something that is not there at all — get their own sentences, because
/// "invalid bundle" tells them nothing about which one it was.
///
/// # Errors
///
/// Whatever [`Bundle::open`] refuses, with the path attached.
pub fn open_bundle(path: &Path) -> anyhow::Result<Bundle> {
    if !path.exists() {
        return Err(anyhow!("`{}` does not exist", path.display()));
    }
    Bundle::open(path)
        .map_err(|error| anyhow!("{error}"))
        .with_context(|| format!("`{}` is not a bundle this SDK can read", path.display()))
}

/// [main-thread] Prints a list of findings and returns how many of each severity there were.
pub fn print_issues(out: &Out, issues: &[ValidationIssue]) -> crate::out::IssueCounts {
    for issue in issues {
        out.issue(issue);
    }
    crate::out::IssueCounts::of(issues)
}

/// [main-thread] Whether any finding is an error.
pub fn has_errors(issues: &[ValidationIssue]) -> bool {
    issues.iter().any(|issue| issue.severity == Severity::Error)
}

/// [main-thread] A plug-in descriptor as JSON, for `--json` output.
pub fn descriptor_json(descriptor: &PluginDescriptor) -> serde_json::Value {
    let capabilities: Vec<&str> = descriptor
        .capabilities
        .iter()
        .map(|(name, _)| name)
        .collect();
    serde_json::json!({
        "id": descriptor.id.as_str(),
        "name": descriptor.name,
        "vendor": descriptor.vendor,
        "version": descriptor.version.to_string(),
        "description": descriptor.description,
        "category": descriptor.category.as_str(),
        "capabilities": capabilities,
        "features": descriptor.features,
        "stateSchemaVersion": descriptor.state_schema_version,
        "minAbi": format!("{}.{}", descriptor.min_abi.0, descriptor.min_abi.1),
    })
}

/// [main-thread] Prints one descriptor as prose.
pub fn print_descriptor(out: &Out, index: usize, descriptor: &PluginDescriptor) {
    out.line(format!(
        "  [{index}] {}  {} {}",
        descriptor.id.as_str(),
        descriptor.name,
        descriptor.version
    ));
    out.line(format!("       vendor        {}", descriptor.vendor));
    out.line(format!(
        "       category      {}",
        descriptor.category.as_str()
    ));
    let capabilities = capability_list(descriptor);
    out.line(format!("       capabilities  {capabilities}"));
    out.line(format!(
        "       state schema  {}",
        descriptor.state_schema_version
    ));
    out.line(format!(
        "       min abi       {}.{}",
        descriptor.min_abi.0, descriptor.min_abi.1
    ));
}

/// [main-thread] The declared capability names, comma-separated, or `none`.
pub fn capability_list(descriptor: &PluginDescriptor) -> String {
    let names: Vec<&str> = descriptor
        .capabilities
        .iter()
        .map(|(name, _)| name)
        .collect();
    if names.is_empty() {
        "none".to_owned()
    } else {
        names.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_that_is_not_there_says_so_rather_than_blaming_the_bundle_format() {
        let error =
            open_bundle(Path::new("no-such-directory-anywhere.axt")).expect_err("nothing to open");
        assert!(error.to_string().contains("does not exist"), "{error}");
    }

    /// A directory that exists but is not a bundle gets the other sentence, and the cause
    /// chain keeps the underlying reason.
    #[test]
    fn something_that_exists_but_is_not_a_bundle_is_reported_as_such() {
        let error = open_bundle(Path::new(".")).expect_err("the current directory is not one");
        assert!(error.to_string().contains("not a bundle"), "{error}");
        assert!(error.chain().count() > 1, "the reason must survive");
    }

    #[test]
    fn errors_are_distinguished_from_warnings() {
        assert!(has_errors(&[ValidationIssue::error("x", "y")]));
        assert!(!has_errors(&[
            ValidationIssue::warning("x", "y"),
            ValidationIssue::info("x", "y"),
        ]));
        assert!(!has_errors(&[]));
    }

    #[test]
    fn a_descriptor_renders_the_same_facts_in_both_output_modes() {
        use daux_runtime::daux_core::{Capabilities, Category};

        let descriptor = PluginDescriptor::builder("com.example.gain", "Gain")
            .vendor("Example Audio")
            .category(Category::Effect)
            .capabilities(Capabilities::NONE.with_audio_effect().with_has_gui())
            .build()
            .expect("a valid identity");

        let json = descriptor_json(&descriptor);
        assert_eq!(json["id"], "com.example.gain");
        assert_eq!(json["category"], "effect");
        let capabilities = json["capabilities"]
            .as_array()
            .expect("an array")
            .iter()
            .filter_map(|value| value.as_str())
            .collect::<Vec<_>>();
        assert!(capabilities.contains(&"AUDIO_EFFECT") || capabilities.contains(&"audioEffect"));
        assert_eq!(capabilities.len(), 2);

        let prose = capability_list(&descriptor);
        for name in &capabilities {
            assert!(prose.contains(name), "`{prose}` must mention `{name}`");
        }
        assert_eq!(
            capability_list(
                &PluginDescriptor::builder("com.example.plain", "Plain")
                    .build()
                    .expect("valid")
            ),
            "none"
        );
    }
}
