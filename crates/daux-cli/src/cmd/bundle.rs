//! `daux bundle` — an `.axt` around a dynamic library that already exists.
//!
//! `daux build` is the everyday command; this is the one for a build that happened
//! somewhere else — a cross-compile on another machine, a CI job that keeps compilation and
//! packaging in separate steps, a bisect over a binary that is no longer reproducible from
//! source.
//!
//! The identity still comes from one place. `--manifest-path` reads the crate's
//! `[package.metadata.daux]`, exactly as `daux build` does; the individual flags exist for
//! the case where there is no crate to point at, and then all four identity values must be
//! given rather than invented.

use std::path::PathBuf;

use anyhow::{anyhow, bail};
use daux_bundle::{Bundle, CAPABILITY_KEYS, Category, Manifest, TargetId};

use crate::cli::BundleArgs;
use crate::cmd::{has_errors, print_issues};
use crate::exit::Exit;
use crate::meta;
use crate::out::{IssueCounts, Out, issues_json};
use crate::pack::{PackRequest, write_axt};

/// [main-thread] Runs `daux bundle`.
///
/// # Errors
///
/// When the binary is missing, when the identity is incomplete, or when the bundle cannot
/// be written.
pub fn run(args: &BundleArgs, out: &Out) -> anyhow::Result<Exit> {
    if !args.binary.is_file() {
        bail!("`{}` is not a file", args.binary.display());
    }
    let target = match &args.target {
        Some(raw) => {
            TargetId::parse(raw).map_err(|error| anyhow!("`{raw}` is not a target id: {error}"))?
        }
        None => TargetId::host(),
    };

    let (mut manifest, mut bundle_name, mut resources) = identity(args)?;
    apply_overrides(args, &mut manifest, &mut bundle_name, &mut resources)?;

    let out_dir = args.out_dir.clone().unwrap_or_else(|| {
        args.binary
            .parent()
            .map_or_else(|| PathBuf::from("."), std::path::Path::to_path_buf)
    });

    let libraries: Vec<(TargetId, PathBuf)> = args
        .libraries
        .iter()
        .map(|path| (target.clone(), path.clone()))
        .collect();

    let root = write_axt(&PackRequest {
        manifest: &manifest,
        bundle_name: &bundle_name,
        binaries: &[(target.clone(), args.binary.clone())],
        libraries: &libraries,
        resources: resources.as_deref(),
        out_dir: &out_dir,
    })?;

    // What was just written is checked immediately: a bundle nobody validated is a bundle
    // whose first reader is a user's DAW.
    let issues = Bundle::open(&root)
        .map(|bundle| bundle.validate())
        .unwrap_or_default();
    let counts = IssueCounts::of(&issues);
    let failed = has_errors(&issues);

    if out.is_json() {
        out.emit(&serde_json::json!({
            "ok": !failed,
            "bundle": root.display().to_string(),
            "id": manifest.plugin.id,
            "name": manifest.plugin.name,
            "version": manifest.plugin.version,
            "target": target.as_str(),
            "issues": issues_json(&issues),
        }))?;
        return Ok(Exit::from_issues(failed));
    }

    out.heading(format!("wrote {}", root.display()));
    out.field("id", &manifest.plugin.id);
    out.field("name", &manifest.plugin.name);
    out.field("version", &manifest.plugin.version);
    out.field("target", target.as_str());
    if !issues.is_empty() {
        out.blank();
        print_issues(out, &issues);
        out.blank();
        out.line(counts.summary());
    }
    Ok(Exit::from_issues(failed))
}

/// The manifest, the bundle directory name and the resource directory, from whichever
/// source the caller chose.
type Identity = (Manifest, String, Option<PathBuf>);

fn identity(args: &BundleArgs) -> anyhow::Result<Identity> {
    if let Some(path) = &args.manifest_path {
        let metadata = meta::read(path)?;
        return Ok((metadata.manifest, metadata.bundle_name, metadata.resources));
    }

    let missing: Vec<&str> = [
        ("--id", args.id.is_none()),
        ("--name", args.name.is_none()),
        ("--vendor", args.vendor.is_none()),
        ("--plugin-version", args.plugin_version.is_none()),
    ]
    .into_iter()
    .filter_map(|(flag, absent)| absent.then_some(flag))
    .collect();
    if !missing.is_empty() {
        bail!(
            "no identity: pass `--manifest-path <Cargo.toml>` to read \
             `[package.metadata.daux]`, or give {}",
            missing.join(", ")
        );
    }

    let id = args.id.clone().unwrap_or_default();
    let name = args.name.clone().unwrap_or_default();
    let vendor = args.vendor.clone().unwrap_or_default();
    let raw_version = args.plugin_version.clone().unwrap_or_default();
    let (version, dropped) = meta::normalise_version(&raw_version)?;

    let mut manifest = Manifest::new(&id, &name, &vendor, &version)
        .map_err(|error| anyhow!("the identity is not usable: {error}"))?;
    manifest.plugin.version_string = Some(raw_version.clone());
    if dropped {
        manifest.plugin.version_string = Some(raw_version);
    }
    manifest.targets = vec![TargetId::host()];

    let bundle_name = meta::sanitise_bundle_name(&name, &id);
    Ok((manifest, bundle_name, None))
}

/// Applies the flags that override whatever the identity source said.
fn apply_overrides(
    args: &BundleArgs,
    manifest: &mut Manifest,
    bundle_name: &mut String,
    resources: &mut Option<PathBuf>,
) -> anyhow::Result<()> {
    if let Some(name) = &args.name {
        manifest.plugin.name = name.clone();
        *bundle_name = meta::sanitise_bundle_name(name, &manifest.plugin.id);
    }
    if let Some(vendor) = &args.vendor {
        manifest.plugin.vendor = vendor.clone();
    }
    if let Some(description) = &args.description {
        manifest.plugin.description = description.clone();
    }
    if let Some(slug) = &args.category {
        let category = Category::parse(slug).ok_or_else(|| {
            anyhow!(
                "`{slug}` is not a category; expected one of {}",
                Category::ALL.map(Category::slug).join(", ")
            )
        })?;
        manifest.plugin.category = Some(category);
    }
    for name in &args.caps {
        if !manifest.capabilities.set_named(name, true) {
            bail!(
                "`{name}` is not a capability; expected one of {}",
                CAPABILITY_KEYS.map(|(key, _)| key).join(", ")
            );
        }
    }
    if let Some(dir) = &args.resources {
        *resources = Some(dir.clone());
    }
    manifest
        .check()
        .map_err(|error| anyhow!("the resulting manifest is not valid: {error}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;

    fn args(argv: &[&str]) -> BundleArgs {
        let cli = crate::cli::Cli::try_parse_from(argv).expect("parses");
        match cli.command {
            crate::cli::Command::Bundle(args) => args,
            other => panic!("expected `bundle`, got {other:?}"),
        }
    }

    /// An identity cannot be invented. Naming the flags that are missing is the difference
    /// between a usable error and a shrug.
    #[test]
    fn an_incomplete_identity_names_every_missing_flag() {
        let error = identity(&args(&[
            "daux",
            "bundle",
            "--binary",
            "x.dll",
            "--id",
            "com.example.gain",
        ]))
        .expect_err("three values are still missing");
        let text = error.to_string();
        assert!(text.contains("--name"), "{text}");
        assert!(text.contains("--vendor"), "{text}");
        assert!(text.contains("--plugin-version"), "{text}");
        assert!(
            !text.contains("--id"),
            "the one that was given must not be listed: {text}"
        );
        assert!(
            text.contains("--manifest-path"),
            "the better route must be offered: {text}"
        );
    }

    #[test]
    fn a_complete_identity_produces_a_manifest_and_a_bundle_name() {
        let (manifest, bundle_name, resources) = identity(&args(&[
            "daux",
            "bundle",
            "--binary",
            "x.dll",
            "--id",
            "com.example.gain",
            "--name",
            "My Gain",
            "--vendor",
            "Example Audio",
            "--plugin-version",
            "1.2.3",
        ]))
        .expect("everything was given");
        assert_eq!(manifest.plugin.id, "com.example.gain");
        assert_eq!(bundle_name, "My Gain");
        assert!(resources.is_none());
    }

    /// A malformed id is refused where it is cheapest to fix, and never written into a
    /// bundle that saved projects would then reference.
    #[test]
    fn a_malformed_identity_is_refused() {
        let error = identity(&args(&[
            "daux",
            "bundle",
            "--binary",
            "x.dll",
            "--id",
            "NotReverseDns",
            "--name",
            "Gain",
            "--vendor",
            "E",
            "--plugin-version",
            "1.0.0",
        ]))
        .expect_err("an id must be reverse-DNS");
        assert!(error.to_string().contains("not usable"), "{error}");
    }

    #[test]
    fn a_pre_release_version_is_normalised_and_kept_as_display_text() {
        let (manifest, _, _) = identity(&args(&[
            "daux",
            "bundle",
            "--binary",
            "x.dll",
            "--id",
            "com.example.gain",
            "--name",
            "Gain",
            "--vendor",
            "E",
            "--plugin-version",
            "1.0.0-rc.2",
        ]))
        .expect("a pre-release is a normal thing to package");
        assert_eq!(manifest.plugin.version, "1.0.0");
        assert_eq!(
            manifest.plugin.version_string.as_deref(),
            Some("1.0.0-rc.2")
        );
    }

    /// A typo'd capability must not silently produce a bundle that declares nothing.
    #[test]
    fn an_unknown_capability_is_refused_with_the_list_of_real_ones() {
        let mut manifest = Manifest::new("com.example.gain", "Gain", "E", "1.0.0").expect("valid");
        manifest.targets = vec![TargetId::host()];
        let mut bundle_name = "Gain".to_owned();
        let mut resources = None;

        let error = apply_overrides(
            &args(&["daux", "bundle", "--binary", "x.dll", "--cap", "audioEfect"]),
            &mut manifest,
            &mut bundle_name,
            &mut resources,
        )
        .expect_err("a typo must be visible");
        assert!(error.to_string().contains("audioEffect"), "{error}");
    }

    #[test]
    fn overrides_reach_the_manifest_and_the_bundle_name() {
        let mut manifest = Manifest::new("com.example.gain", "Gain", "E", "1.0.0").expect("valid");
        manifest.targets = vec![TargetId::host()];
        let mut bundle_name = "Gain".to_owned();
        let mut resources = None;

        apply_overrides(
            &args(&[
                "daux",
                "bundle",
                "--binary",
                "x.dll",
                "--name",
                "Studio Gain",
                "--vendor",
                "Acme",
                "--category",
                "effect",
                "--cap",
                "audioEffect",
                "--cap",
                "sidechain",
            ]),
            &mut manifest,
            &mut bundle_name,
            &mut resources,
        )
        .expect("every override is legal");

        assert_eq!(manifest.plugin.name, "Studio Gain");
        assert_eq!(manifest.plugin.vendor, "Acme");
        assert_eq!(manifest.plugin.category, Some(Category::Effect));
        assert_eq!(manifest.capabilities.get("audioEffect"), Some(true));
        assert_eq!(manifest.capabilities.get("sidechain"), Some(true));
        assert_eq!(bundle_name, "Studio Gain");
    }

    #[test]
    fn an_unknown_category_is_refused() {
        let mut manifest = Manifest::new("com.example.gain", "Gain", "E", "1.0.0").expect("valid");
        manifest.targets = vec![TargetId::host()];
        let mut bundle_name = "Gain".to_owned();
        let mut resources = None;
        let error = apply_overrides(
            &args(&[
                "daux",
                "bundle",
                "--binary",
                "x.dll",
                "--category",
                "reverb",
            ]),
            &mut manifest,
            &mut bundle_name,
            &mut resources,
        )
        .expect_err("`reverb` is not a category");
        assert!(error.to_string().contains("effect"), "{error}");
    }
}
