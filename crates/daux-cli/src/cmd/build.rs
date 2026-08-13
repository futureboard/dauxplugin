//! `daux build` — compile the plug-in, package it, and say what each format loses.
//!
//! Three steps, in this order, because each one depends on the last being true:
//!
//! 1. read `[package.metadata.daux]`, the single source of truth (`manifest-v1` §2);
//! 2. `cargo build`, and take the artefact cargo says it produced rather than a guess;
//! 3. package, then open what was packaged and ask each format adapter what it cannot
//!    carry.
//!
//! Step 3 is the reason the command exists rather than a shell alias for `cargo build`. A
//! capability VST3 or CLAP cannot express is not a runtime error — the plug-in loads, and
//! quietly does less than it says. `compatibility_report` turns that into a line in a build
//! log, which is the only place a developer will see it.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail};
use daux_bundle::{Bundle, Severity, TargetId, ValidationIssue};
use daux_runtime::daux_core::PluginDescriptor;
use daux_scan::Scanner;

use crate::cargo_build::{BuildRequest, build_cdylib, profile_dir};
use crate::cli::BuildArgs;
use crate::cmd::{has_errors, print_issues};
use crate::exit::Exit;
use crate::formats::{Format, FormatWarning, compatibility_report, warning_json};
use crate::meta::{self, CrateMetadata};
use crate::out::{IssueCounts, Out, issues_json};
use crate::pack::{PackRequest, write_axt, write_clap, write_vst3};

/// One packaged artefact. [main-thread]
struct Artefact {
    format: Format,
    path: PathBuf,
}

/// [main-thread] Runs `daux build`.
///
/// # Errors
///
/// When the metadata cannot be read, when the crate is not a `cdylib`, when cargo fails, or
/// when packaging fails. A plug-in that builds and packages but loses a capability in a
/// compatibility format is not an error: it is the report, and it is only fatal under
/// `--strict`.
pub fn run(args: &BuildArgs, out: &Out) -> anyhow::Result<Exit> {
    let manifest_path = args
        .manifest_path
        .clone()
        .unwrap_or_else(|| PathBuf::from("Cargo.toml"));
    let metadata = meta::read(&manifest_path)?;

    for warning in &metadata.warnings {
        out.issue(warning);
    }
    if !metadata.is_cdylib() {
        bail!(
            "DAUX-M204: `{}` does not build a `cdylib`; a plug-in is a dynamic library, so \
             its `[lib]` section needs `crate-type = [\"cdylib\"]`",
            metadata.package_name
        );
    }

    let formats = requested_formats(args, &metadata)?;
    let target = build_target(args.target.as_deref())?;
    let profile = profile_dir(!args.debug);

    out.line(format!(
        "building {} {} for {target} ({profile})",
        metadata.manifest.plugin.name, metadata.manifest.plugin.version
    ));

    let binary = build_cdylib(&BuildRequest {
        manifest_path: &manifest_path,
        package: &metadata.package_name,
        release: !args.debug,
        target: args.target.as_deref(),
        dylib_extension: target.dylib_extension(),
    })?;

    let base = args.out_dir.clone().unwrap_or_else(|| {
        daux_out_dir(&binary, args.target.is_some(), profile, &metadata.crate_dir)
    });

    let mut artefacts = Vec::new();
    let mut issues = Vec::new();
    for format in &formats {
        match package(*format, &metadata, &target, &binary, &base) {
            Ok(artefact) => artefacts.push(artefact),
            Err(error) => issues.push(ValidationIssue::error(
                "axt.package.failed",
                format!("the {format} export was not written: {error}"),
            )),
        }
    }

    let axt = artefacts
        .iter()
        .find(|artefact| artefact.format == Format::Axt)
        .map(|artefact| artefact.path.clone());
    if let Some(root) = &axt
        && let Ok(bundle) = Bundle::open(root)
    {
        issues.extend(bundle.validate());
    }

    let descriptors = if args.no_probe {
        Vec::new()
    } else {
        axt.as_deref()
            .map(|root| descriptors_of(root, &target, out))
            .unwrap_or_default()
    };
    let warnings = compatibility(&formats, &descriptors);

    report(args, out, &metadata, &artefacts, &issues, &warnings)
}

/// Which formats this build packages.
fn requested_formats(args: &BuildArgs, metadata: &CrateMetadata) -> anyhow::Result<Vec<Format>> {
    if args.formats.is_empty() {
        return Ok(metadata.formats.clone());
    }
    args.formats
        .iter()
        .map(|name| {
            Format::parse(name).ok_or_else(|| {
                anyhow!(
                    "`{name}` is not a format; expected one of {}",
                    Format::names()
                )
            })
        })
        .collect()
}

/// The DAUx target the build produces a binary for.
fn build_target(triple: Option<&str>) -> anyhow::Result<TargetId> {
    match triple {
        None => Ok(TargetId::host()),
        Some(triple) => TargetId::from_rust_triple(triple).ok_or_else(|| {
            anyhow!(
                "`{triple}` is not a target this SDK packages for; the registry is {}",
                TargetId::registry()
                    .iter()
                    .map(TargetId::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }),
    }
}

/// `target/daux/{profile}/`, derived from where cargo actually put the artefact.
///
/// Guessing `./target` is wrong in a workspace, wrong under `CARGO_TARGET_DIR`, and wrong
/// again when `--target` adds a triple directory. The artefact's own path knows all three;
/// `crate_dir` is only the fallback for an artefact path with nothing to walk up to.
fn daux_out_dir(binary: &Path, cross: bool, profile: &str, crate_dir: &Path) -> PathBuf {
    let profile_dir = binary.parent();
    let target_dir = if cross {
        // `<target>/<triple>/<profile>/lib.so`
        profile_dir.and_then(Path::parent).and_then(Path::parent)
    } else {
        // `<target>/<profile>/lib.so`
        profile_dir.and_then(Path::parent)
    };
    target_dir.map_or_else(
        || crate_dir.join("target").join("daux").join(profile),
        |dir| dir.join("daux").join(profile),
    )
}

/// Packages one format.
fn package(
    format: Format,
    metadata: &CrateMetadata,
    target: &TargetId,
    binary: &Path,
    base: &Path,
) -> anyhow::Result<Artefact> {
    let out_dir = base.join(format.slug());
    let libraries: Vec<(TargetId, PathBuf)> = metadata
        .library_files()
        .into_iter()
        .map(|file| (target.clone(), file))
        .collect();
    let path = match format {
        Format::Axt => write_axt(&PackRequest {
            manifest: &metadata.manifest,
            bundle_name: &metadata.bundle_name,
            binaries: &[(target.clone(), binary.to_path_buf())],
            libraries: &libraries,
            resources: metadata.resources.as_deref(),
            out_dir: &out_dir,
        })?,
        Format::Vst3 => write_vst3(&out_dir, &metadata.bundle_name, target, binary)?,
        Format::Clap => write_clap(&out_dir, &metadata.bundle_name, target, binary)?,
    };
    Ok(Artefact { format, path })
}

/// Opens the packaged bundle and asks its factory what it publishes.
///
/// A failure is a warning rather than an error: a cross-compiled bundle cannot be opened
/// here at all, and the build itself succeeded.
fn descriptors_of(root: &Path, target: &TargetId, out: &Out) -> Vec<PluginDescriptor> {
    if *target != TargetId::host() {
        out.note(format!(
            "built for {target}; the binary was not opened, so no compatibility report \
             could be produced"
        ));
        return Vec::new();
    }
    let mut scanner = Scanner::new();
    scanner.clear_search_paths();
    scanner.set_probe(true);
    match scanner.inspect(root) {
        Ok(entry) => entry.descriptors,
        Err(error) => {
            out.warn(format!(
                "the plug-in was packaged but could not be opened ({error}); \
                 no compatibility report was produced"
            ));
            Vec::new()
        }
    }
}

/// Every format's report, for every plug-in the binary publishes.
fn compatibility(formats: &[Format], descriptors: &[PluginDescriptor]) -> Vec<FormatWarning> {
    let mut warnings = Vec::new();
    for descriptor in descriptors {
        for format in formats {
            warnings.extend(compatibility_report(*format, descriptor));
        }
    }
    warnings
}

/// Prints the outcome and decides the exit code.
fn report(
    args: &BuildArgs,
    out: &Out,
    metadata: &CrateMetadata,
    artefacts: &[Artefact],
    issues: &[ValidationIssue],
    warnings: &[FormatWarning],
) -> anyhow::Result<Exit> {
    let counts = IssueCounts::of(issues);
    let compat_errors = warnings
        .iter()
        .filter(|warning| warning.severity == Severity::Error)
        .count();
    let compat_warnings = warnings
        .iter()
        .filter(|warning| warning.severity == Severity::Warning)
        .count();
    let failed = has_errors(issues)
        || compat_errors > 0
        || (args.strict && (counts.warnings > 0 || compat_warnings > 0));

    if out.is_json() {
        out.emit(&serde_json::json!({
            "ok": !failed,
            "id": metadata.manifest.plugin.id,
            "name": metadata.manifest.plugin.name,
            "version": metadata.manifest.plugin.version,
            "artefacts": artefacts
                .iter()
                .map(|artefact| serde_json::json!({
                    "format": artefact.format.slug(),
                    "path": artefact.path.display().to_string(),
                }))
                .collect::<Vec<_>>(),
            "issues": issues_json(issues),
            "metadataWarnings": issues_json(&metadata.warnings),
            "compatibility": warnings.iter().map(warning_json).collect::<Vec<_>>(),
        }))?;
        return Ok(Exit::from_issues(failed));
    }

    out.blank();
    for artefact in artefacts {
        out.line(format!(
            "{:<6}{}",
            artefact.format.slug(),
            artefact.path.display()
        ));
    }
    if artefacts.is_empty() {
        out.line("nothing was packaged");
    }

    if !issues.is_empty() {
        out.blank();
        out.heading("bundle");
        print_issues(out, issues);
    }

    if warnings.is_empty() {
        out.blank();
        out.line("every requested format carries this plug-in without loss");
    } else {
        out.blank();
        out.heading("compatibility");
        for warning in warnings {
            out.warn(format!("  {warning}"));
            if let Some(advice) = &warning.advice {
                out.warn(format!("           {advice}"));
            }
        }
    }
    if failed && !has_errors(issues) && compat_errors == 0 {
        out.note("failing on warnings because `--strict` was given");
    }
    Ok(Exit::from_issues(failed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;
    use daux_runtime::daux_core::{Capabilities, Category};

    fn args(argv: &[&str]) -> BuildArgs {
        let cli = crate::cli::Cli::try_parse_from(argv).expect("parses");
        match cli.command {
            crate::cli::Command::Build(args) => args,
            other => panic!("expected `build`, got {other:?}"),
        }
    }

    fn metadata(formats: &[Format]) -> CrateMetadata {
        let document: toml::Table = toml::from_str(
            "[package]\nname = \"gain\"\nversion = \"1.0.0\"\n\
             [lib]\ncrate-type = [\"cdylib\"]\n\
             [package.metadata.daux]\nid = \"com.example.gain\"\nvendor = \"E\"\n",
        )
        .expect("valid TOML");
        let mut metadata =
            meta::read_document(&document, Path::new("no-such-crate-directory")).expect("valid");
        metadata.formats = formats.to_vec();
        metadata
    }

    /// The metadata decides which formats are packaged; the flag overrides it. Getting this
    /// backwards would make `--formats` silently additive.
    #[test]
    fn the_format_flag_replaces_the_manifests_list_rather_than_adding_to_it() {
        let metadata = metadata(&[Format::Axt, Format::Vst3]);
        assert_eq!(
            requested_formats(&args(&["daux", "build"]), &metadata).expect("parses"),
            [Format::Axt, Format::Vst3]
        );
        assert_eq!(
            requested_formats(&args(&["daux", "build", "--formats", "clap"]), &metadata)
                .expect("parses"),
            [Format::Clap]
        );
    }

    #[test]
    fn an_unknown_format_flag_is_refused() {
        let error = requested_formats(&args(&["daux", "build", "--formats", "au"]), &metadata(&[]))
            .expect_err("there is no `au`");
        assert!(error.to_string().contains("axt"), "{error}");
    }

    /// A cross-compile triple has to reach a target this SDK can package for, and an
    /// unknown one must name the registry rather than failing at the copy.
    #[test]
    fn the_target_comes_from_the_triple_and_an_unknown_one_is_refused() {
        assert_eq!(
            build_target(None).expect("the host is always known"),
            TargetId::host()
        );
        assert_eq!(
            build_target(Some("x86_64-unknown-linux-gnu"))
                .expect("a registered triple")
                .as_str(),
            "linux-x86_64"
        );
        let error = build_target(Some("mips-unknown-linux-gnu")).expect_err("not in the registry");
        assert!(error.to_string().contains("windows-x86_64"), "{error}");
    }

    /// The output directory is derived from where cargo really wrote the artefact, which is
    /// the only thing that survives a workspace, a `CARGO_TARGET_DIR` and a `--target`.
    #[test]
    fn the_output_directory_follows_the_artefact_not_the_current_directory() {
        let crate_dir = Path::new("/w/gain");
        assert_eq!(
            daux_out_dir(
                Path::new("/w/target/release/gain.so"),
                false,
                "release",
                crate_dir
            ),
            PathBuf::from("/w/target/daux/release")
        );
        assert_eq!(
            daux_out_dir(
                Path::new("/w/target/x86_64-unknown-linux-gnu/debug/gain.so"),
                true,
                "debug",
                crate_dir
            ),
            PathBuf::from("/w/target/daux/debug")
        );
        // A path with nowhere to walk up to falls back to the crate's own target directory.
        assert_eq!(
            daux_out_dir(Path::new("gain.so"), false, "release", crate_dir),
            PathBuf::from("/w/gain/target/daux/release")
        );
    }

    /// Every plug-in a multi-plug-in bundle publishes is reported against every requested
    /// format; reporting only the first would hide the one that actually loses something.
    #[test]
    fn the_compatibility_report_covers_every_plug_in_and_every_format() {
        let plain = PluginDescriptor::builder("com.example.a", "A")
            .category(Category::Effect)
            .build()
            .expect("valid");
        let midi2 = PluginDescriptor::builder("com.example.b", "B")
            .category(Category::Instrument)
            .capabilities(Capabilities::NONE.with_midi2().with_midi_input())
            .build()
            .expect("valid");

        let warnings = compatibility(&[Format::Axt, Format::Vst3], &[plain, midi2]);
        assert!(
            warnings
                .iter()
                .any(|warning| warning.format == Format::Vst3 && warning.code.contains("midi2")),
            "{warnings:#?}"
        );
        assert!(
            warnings
                .iter()
                .all(|warning| warning.format != Format::Clap),
            "a format that was not requested must not be reported on"
        );
    }

    #[test]
    fn nothing_to_report_is_the_normal_case_for_a_plain_plug_in() {
        let plain = PluginDescriptor::builder("com.example.a", "A")
            .category(Category::Effect)
            .build()
            .expect("valid");
        assert!(compatibility(&[Format::Axt], &[plain]).is_empty());
        // And with no descriptors — a cross-compile — there is nothing to say either.
        assert!(compatibility(&[Format::Axt, Format::Vst3, Format::Clap], &[]).is_empty());
    }
}
