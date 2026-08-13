//! Driving `cargo build`, and finding the dynamic library it produced.
//!
//! Guessing the output path from the profile and the crate name is wrong often enough to
//! matter — a renamed `[lib]`, a `--target`, a custom `CARGO_TARGET_DIR`, a workspace whose
//! `target/` is three directories up. So the artefact is not guessed: `cargo` is asked to
//! report what it built, in JSON, and the answer is matched against the manifest path the
//! build was pointed at.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context as _, anyhow, bail};

/// What to build. [main-thread]
#[derive(Clone, Copy, Debug)]
pub struct BuildRequest<'a> {
    /// The plug-in crate's `Cargo.toml`.
    pub manifest_path: &'a Path,
    /// The cargo package name, for `--package`.
    pub package: &'a str,
    /// Build the optimised profile.
    pub release: bool,
    /// Cross-compile for this Rust target triple.
    pub target: Option<&'a str>,
    /// Extension of the dynamic library to look for: `dll`, `so` or `dylib`.
    pub dylib_extension: &'a str,
}

/// [main-thread] The name cargo goes by in this environment.
///
/// `CARGO` is set when `daux` is itself run through cargo, and using it keeps a build inside
/// the same toolchain the user selected with `rustup` rather than whatever `PATH` finds.
fn cargo_binary() -> std::ffi::OsString {
    std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into())
}

/// [main-thread] The directory name cargo writes this profile into.
pub const fn profile_dir(release: bool) -> &'static str {
    if release { "release" } else { "debug" }
}

/// [main-thread] Builds the crate and returns the dynamic library it produced.
///
/// Cargo's own diagnostics stream straight to stderr, so the developer sees compiler errors
/// exactly as `cargo build` would show them.
///
/// # Errors
///
/// When cargo cannot be started, when it exits non-zero, or when it built no `cdylib` for
/// the crate that was asked for.
pub fn build_cdylib(request: &BuildRequest<'_>) -> anyhow::Result<PathBuf> {
    let mut command = Command::new(cargo_binary());
    command
        .arg("build")
        .arg("--manifest-path")
        .arg(request.manifest_path)
        .arg("--package")
        .arg(request.package)
        .arg("--message-format")
        .arg("json-render-diagnostics")
        .stdout(Stdio::piped())
        // Warnings and errors belong on the developer's terminal, not in a parsed stream.
        .stderr(Stdio::inherit());
    if request.release {
        command.arg("--release");
    }
    if let Some(triple) = request.target {
        command.arg("--target").arg(triple);
    }

    let output = command
        .output()
        .with_context(|| format!("cannot run `{}`", cargo_binary().to_string_lossy()))?;
    if !output.status.success() {
        bail!(
            "cargo build failed{}",
            output
                .status
                .code()
                .map_or_else(String::new, |code| format!(" with exit code {code}"))
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    find_cdylib(&stdout, request.manifest_path, request.dylib_extension).ok_or_else(|| {
        anyhow!(
            "cargo built no `cdylib` for `{}`; a plug-in crate needs \
             `crate-type = [\"cdylib\"]` in its `[lib]` section (DAUX-M204)",
            request.package
        )
    })
}

/// Picks the plug-in's dynamic library out of cargo's JSON message stream.
///
/// Split out so the matching rules can be tested against recorded cargo output rather than
/// against a real build: the interesting cases — a workspace that built several crates, a
/// `cdylib` that is not ours, an import library beside the real one — are all one line of
/// JSON each.
fn find_cdylib(stream: &str, manifest_path: &Path, extension: &str) -> Option<PathBuf> {
    let wanted =
        std::fs::canonicalize(manifest_path).unwrap_or_else(|_| manifest_path.to_path_buf());

    let mut fallback = None;
    for line in stream.lines() {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if message.get("reason").and_then(serde_json::Value::as_str) != Some("compiler-artifact") {
            continue;
        }
        let kinds = message
            .get("target")
            .and_then(|target| target.get("kind"))
            .and_then(serde_json::Value::as_array);
        let is_cdylib =
            kinds.is_some_and(|kinds| kinds.iter().any(|kind| kind.as_str() == Some("cdylib")));
        if !is_cdylib {
            continue;
        }

        let candidate = message
            .get("filenames")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .map(PathBuf::from)
            // On Windows a `cdylib` artefact is two files: the DLL and the import library
            // beside it. Only one of them is loadable.
            .find(|path| {
                path.extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case(extension))
            });
        let Some(candidate) = candidate else {
            continue;
        };

        let same_crate = message
            .get("manifest_path")
            .and_then(serde_json::Value::as_str)
            .map(PathBuf::from)
            .map(|path| std::fs::canonicalize(&path).unwrap_or(path))
            .is_some_and(|path| path == wanted);
        if same_crate {
            return Some(candidate);
        }
        // A workspace build can report several `cdylib`s; one that is not the crate we
        // asked for is only worth using when nothing better turns up.
        fallback.get_or_insert(candidate);
    }
    fallback
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(manifest: &str, kinds: &str, filenames: &[&str]) -> String {
        let files: Vec<String> = filenames.iter().map(|name| format!("{:?}", name)).collect();
        format!(
            r#"{{"reason":"compiler-artifact","manifest_path":"{manifest}","target":{{"kind":[{kinds}],"name":"gain"}},"filenames":[{}]}}"#,
            files.join(",")
        )
    }

    #[test]
    fn the_profile_directory_matches_cargos() {
        assert_eq!(profile_dir(true), "release");
        assert_eq!(profile_dir(false), "debug");
    }

    /// The Windows case that a path guess gets wrong: cargo reports the DLL *and* its
    /// import library, and loading the `.lib` would fail with a message about a bad image.
    #[test]
    fn the_import_library_beside_a_dll_is_never_chosen() {
        let stream = artifact(
            "/w/gain/Cargo.toml",
            "\"cdylib\"",
            &[
                "/w/target/release/gain.dll",
                "/w/target/release/gain.dll.lib",
            ],
        );
        let found = find_cdylib(&stream, Path::new("/w/gain/Cargo.toml"), "dll");
        assert_eq!(found, Some(PathBuf::from("/w/target/release/gain.dll")));
    }

    /// A workspace build reports every crate it touched. Picking the wrong `cdylib` would
    /// package one plug-in under another's name.
    #[test]
    fn the_artefact_of_the_requested_crate_wins_over_any_other() {
        let stream = [
            artifact(
                "/w/other/Cargo.toml",
                "\"cdylib\"",
                &["/w/target/release/other.so"],
            ),
            artifact(
                "/w/gain/Cargo.toml",
                "\"cdylib\"",
                &["/w/target/release/gain.so"],
            ),
            artifact(
                "/w/third/Cargo.toml",
                "\"cdylib\"",
                &["/w/target/release/third.so"],
            ),
        ]
        .join("\n");
        assert_eq!(
            find_cdylib(&stream, Path::new("/w/gain/Cargo.toml"), "so"),
            Some(PathBuf::from("/w/target/release/gain.so"))
        );
    }

    #[test]
    fn an_rlib_only_crate_produces_nothing_to_package() {
        let stream = artifact(
            "/w/gain/Cargo.toml",
            "\"lib\"",
            &["/w/target/release/libgain.rlib"],
        );
        assert_eq!(
            find_cdylib(&stream, Path::new("/w/gain/Cargo.toml"), "so"),
            None
        );
    }

    /// A crate that is both `cdylib` and `rlib` — the shape every example in this workspace
    /// uses — still has exactly one loadable artefact.
    #[test]
    fn a_crate_that_is_both_cdylib_and_rlib_yields_the_dynamic_library() {
        let stream = artifact(
            "/w/gain/Cargo.toml",
            "\"cdylib\",\"rlib\"",
            &[
                "/w/target/release/libgain.rlib",
                "/w/target/release/libgain.so",
            ],
        );
        assert_eq!(
            find_cdylib(&stream, Path::new("/w/gain/Cargo.toml"), "so"),
            Some(PathBuf::from("/w/target/release/libgain.so"))
        );
    }

    /// Cargo's stream carries build-script output, compiler messages and a final
    /// `build-finished` line. None of them is JSON this function understands, and none of
    /// them may make it panic or return nonsense.
    #[test]
    fn unrelated_and_malformed_lines_are_ignored() {
        let stream = [
            "not json at all",
            "",
            r#"{"reason":"build-script-executed","package_id":"x"}"#,
            r#"{"reason":"compiler-message","message":{"rendered":"warning: unused"}}"#,
            &artifact(
                "/w/gain/Cargo.toml",
                "\"cdylib\"",
                &["/w/target/debug/gain.dylib"],
            ),
            r#"{"reason":"build-finished","success":true}"#,
            "{",
        ]
        .join("\n");
        assert_eq!(
            find_cdylib(&stream, Path::new("/w/gain/Cargo.toml"), "dylib"),
            Some(PathBuf::from("/w/target/debug/gain.dylib"))
        );
    }

    #[test]
    fn an_empty_stream_yields_nothing_rather_than_a_guess() {
        assert_eq!(
            find_cdylib("", Path::new("/w/gain/Cargo.toml"), "dll"),
            None
        );
    }

    /// An artefact whose extension does not match the target's is not the plug-in: a
    /// cross-compile for Linux must not package a `.dll` left over from a host build.
    #[test]
    fn an_artefact_for_another_platform_is_not_mistaken_for_the_plug_in() {
        let stream = artifact(
            "/w/gain/Cargo.toml",
            "\"cdylib\"",
            &["/w/target/release/gain.dll"],
        );
        assert_eq!(
            find_cdylib(&stream, Path::new("/w/gain/Cargo.toml"), "so"),
            None
        );
    }
}
