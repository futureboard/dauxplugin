//! The CLI as a user meets it: the real binary, real arguments, real exit codes.
//!
//! The unit tests inside the crate check the rules; this file checks the contract the
//! `CLAUDE.md` commands depend on — that `daux bundle`, `daux inspect`, `daux validate` and
//! `daux scan` work end to end on a bundle the tool itself produced, that `--json` emits
//! exactly one parseable document, and that no bad input produces a panic or exit code 0.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The exit codes `daux` documents.
mod exit {
    /// Nothing wrong.
    pub const OK: i32 = 0;
    /// Ran, and found problems.
    pub const ISSUES: i32 = 1;
    /// The command line itself was wrong.
    pub const USAGE: i32 = 2;
    /// Could not run.
    pub const CANNOT_RUN: i32 = 3;
    /// A bug in `daux` itself.
    pub const INTERNAL: i32 = 70;
}

/// What one invocation produced.
struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Run {
    fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.stdout).unwrap_or_else(|error| {
            panic!(
                "`--json` must write exactly one JSON document; got {error}\n\
                 stdout was:\n{}\nstderr was:\n{}",
                self.stdout, self.stderr
            )
        })
    }
}

/// Runs the real `daux` binary.
fn daux(args: &[&str]) -> Run {
    let output = Command::new(env!("CARGO_BIN_EXE_daux"))
        .args(args)
        .output()
        .expect("the daux binary runs");
    let run = Run {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    };
    assert_ne!(
        run.code,
        exit::INTERNAL,
        "`daux {}` panicked:\n{}",
        args.join(" "),
        run.stderr
    );
    run
}

/// A temporary directory that removes itself.
struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let path = std::env::temp_dir().join(format!("daux-cli-e2e-{label}-{unique}"));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("a temporary directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn str(&self) -> String {
        self.0.display().to_string()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Packages a bundle with `daux bundle` and returns its root.
///
/// The "binary" is a file with the right name and the wrong contents: everything below
/// reads the bundle's metadata, and nothing loads it, which is exactly the split the format
/// exists to make possible.
fn make_bundle(dir: &TempDir, name: &str, id: &str) -> PathBuf {
    let binary = dir.path().join("build").join("libplugin.bin");
    std::fs::create_dir_all(binary.parent().expect("a parent")).expect("a build directory");
    std::fs::write(&binary, b"not really a library").expect("a stand-in binary");

    let out = dir.path().join("out");
    let run = daux(&[
        "bundle",
        "--binary",
        &binary.display().to_string(),
        "--id",
        id,
        "--name",
        name,
        "--vendor",
        "Example Audio",
        "--plugin-version",
        "1.2.3",
        "--description",
        "A bundle made by the tool's own test.",
        "--category",
        "effect",
        "--cap",
        "audioEffect",
        "--out-dir",
        &out.display().to_string(),
    ]);
    assert_eq!(
        run.code,
        exit::OK,
        "stdout:\n{}\nstderr:\n{}",
        run.stdout,
        run.stderr
    );
    out.join(format!("{name}.axt"))
}

/// The three commands `CLAUDE.md` documents, on a bundle this tool produced.
#[test]
fn bundle_inspect_validate_and_scan_work_end_to_end() {
    let dir = TempDir::new("round-trip");
    let root = make_bundle(&dir, "Gain", "com.example.gain");
    assert!(root.is_dir(), "{}", root.display());
    assert!(root.join("manifest.json").is_file());

    let path = root.display().to_string();

    let inspect = daux(&["inspect", "--no-probe", &path]);
    assert_eq!(inspect.code, exit::OK, "{}", inspect.stderr);
    assert!(
        inspect.stdout.contains("com.example.gain"),
        "{}",
        inspect.stdout
    );
    assert!(inspect.stdout.contains("1.2.3"), "{}", inspect.stdout);
    assert!(
        inspect.stdout.contains("audioEffect"),
        "the declared capability must be shown: {}",
        inspect.stdout
    );

    let validate = daux(&["validate", &path]);
    assert_eq!(
        validate.code,
        exit::OK,
        "a bundle this tool just wrote must validate:\n{}\n{}",
        validate.stdout,
        validate.stderr
    );

    let scan = daux(&["scan", "--path", &dir.str(), "--no-probe", "--json"]);
    assert_eq!(scan.code, exit::OK, "{}", scan.stderr);
    let found = scan.json();
    let plugins = found["plugins"].as_array().expect("an array");
    assert_eq!(plugins.len(), 1, "{}", scan.stdout);
    assert_eq!(plugins[0]["id"], "com.example.gain");
    assert_eq!(plugins[0]["probed"], false);
}

/// The metadata a bundle carries has to be the metadata that was asked for — a packaging
/// step that quietly dropped the category or the capability would be invisible.
#[test]
fn the_manifest_carries_every_value_the_command_was_given() {
    let dir = TempDir::new("manifest-values");
    let root = make_bundle(&dir, "Gain", "com.example.gain");

    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("manifest.json")).expect("a manifest"))
            .expect("valid JSON");

    assert_eq!(manifest["format"], "DAUx Audio Extension");
    assert_eq!(manifest["formatVersion"], 1);
    assert_eq!(manifest["plugin"]["id"], "com.example.gain");
    assert_eq!(manifest["plugin"]["name"], "Gain");
    assert_eq!(manifest["plugin"]["vendor"], "Example Audio");
    assert_eq!(manifest["plugin"]["version"], "1.2.3");
    assert_eq!(manifest["plugin"]["category"], "effect");
    assert_eq!(manifest["capabilities"]["audioEffect"], true);
    assert_eq!(
        manifest["targets"]
            .as_array()
            .expect("an array of targets")
            .len(),
        1
    );
}

/// A broken bundle is the command's *answer*, not its failure: exit 1, with the stable code
/// a script can match on.
#[test]
fn validate_reports_a_broken_bundle_with_its_stable_code_and_exits_one() {
    let dir = TempDir::new("broken");
    let root = make_bundle(&dir, "Gain", "com.example.gain");

    // Remove the binary the manifest declares — the commonest packaging mistake there is.
    std::fs::remove_dir_all(root.join("Content")).expect("the binary directory goes away");

    let run = daux(&["validate", "--json", &root.display().to_string()]);
    assert_eq!(run.code, exit::ISSUES, "{}", run.stderr);
    let report = run.json();
    assert_eq!(report["ok"], false);
    let issues = report["issues"].as_array().expect("an array");
    assert!(
        issues
            .iter()
            .any(|issue| issue["code"] == "missing-binary" && issue["severity"] == "error"),
        "{}",
        run.stdout
    );
    assert_eq!(report["counts"]["errors"].as_u64(), Some(1));
}

/// Warnings do not fail a build unless the caller says so.
#[test]
fn deny_warnings_moves_the_line_between_exit_zero_and_exit_one() {
    let dir = TempDir::new("deny-warnings");
    let root = make_bundle(&dir, "Gain", "com.example.gain");

    // A manifest with no vendor is a warning, never an error.
    let manifest_path = root.join("manifest.json");
    let text = std::fs::read_to_string(&manifest_path).expect("a manifest");
    std::fs::write(
        &manifest_path,
        text.replace("\"vendor\": \"Example Audio\"", "\"vendor\": \" \""),
    )
    .expect("a rewritten manifest");

    let path = root.display().to_string();
    let lenient = daux(&["validate", &path]);
    assert_eq!(
        lenient.code,
        exit::OK,
        "{}\n{}",
        lenient.stdout,
        lenient.stderr
    );

    let strict = daux(&["validate", "--deny-warnings", &path]);
    assert_eq!(
        strict.code,
        exit::ISSUES,
        "{}\n{}",
        strict.stdout,
        strict.stderr
    );
}

/// "Could not run" and "ran and found problems" must never share a code, or a CI script
/// cannot tell a typo'd path from a broken plug-in.
#[test]
fn a_path_that_is_not_a_bundle_exits_three_rather_than_one() {
    let dir = TempDir::new("not-a-bundle");
    let file = dir.path().join("plain.txt");
    std::fs::write(&file, b"hello").expect("a file");

    for args in [
        vec!["validate", "no-such-bundle-anywhere.axt"],
        vec!["inspect", "no-such-bundle-anywhere.axt"],
        vec!["validate", &dir.str()],
        vec!["test", "no-such-bundle-anywhere.axt"],
        vec!["run", "no-such-bundle-anywhere.axt"],
        vec!["build", "--manifest-path", "no-such-crate/Cargo.toml"],
        vec!["bundle", "--binary", "no-such-binary.dll"],
    ] {
        let run = daux(&args);
        assert_eq!(
            run.code,
            exit::CANNOT_RUN,
            "`daux {}` must exit {} — stdout:\n{}\nstderr:\n{}",
            args.join(" "),
            exit::CANNOT_RUN,
            run.stdout,
            run.stderr
        );
        assert!(
            !run.stderr.contains("panicked"),
            "`daux {}` printed a panic",
            args.join(" ")
        );
    }
}

/// Even when it cannot run, `--json` produces the one document the caller asked for.
#[test]
fn a_failure_in_json_mode_is_still_json() {
    let run = daux(&["validate", "--json", "no-such-bundle-anywhere.axt"]);
    assert_eq!(run.code, exit::CANNOT_RUN);
    let report = run.json();
    assert_eq!(report["ok"], false);
    assert!(
        report["error"]
            .as_str()
            .is_some_and(|message| message.contains("does not exist")),
        "{}",
        run.stdout
    );
}

/// A wrong command line is clap's exit code, and never a panic or a silent success.
#[test]
fn a_malformed_command_line_exits_two() {
    for args in [
        vec![],
        vec!["frobnicate"],
        vec!["validate"],
        vec!["inspect", "--nonsense", "Gain.axt"],
        vec!["run", "Gain.axt", "--blocks", "not-a-number"],
        vec!["scan", "--max-depth", "-1"],
    ] {
        let run = daux(&args);
        assert_eq!(
            run.code,
            exit::USAGE,
            "`daux {}` must be a usage error:\n{}\n{}",
            args.join(" "),
            run.stdout,
            run.stderr
        );
    }
}

#[test]
fn help_and_version_work_and_document_the_exit_codes() {
    let version = daux(&["--version"]);
    assert_eq!(version.code, exit::OK);
    assert!(version.stdout.contains(env!("CARGO_PKG_VERSION")));

    let help = daux(&["--help"]);
    assert_eq!(help.code, exit::OK);
    for command in [
        "new", "build", "bundle", "validate", "inspect", "scan", "test", "run",
    ] {
        assert!(
            help.stdout.contains(command),
            "`--help` must list `{command}`:\n{}",
            help.stdout
        );
    }
    assert!(
        help.stdout.contains("Exit codes"),
        "the exit codes belong in the long help:\n{}",
        help.stdout
    );
}

/// `daux new` writes a crate `daux build` can read, refuses to clobber, and clobbers when
/// it is told to.
#[test]
fn new_scaffolds_a_crate_and_refuses_to_overwrite_one() {
    let dir = TempDir::new("new");
    let crate_dir = dir.path().join("reverb");
    let crate_str = crate_dir.display().to_string();

    let first = daux(&["new", "Reverb", "--path", &crate_str, "--json"]);
    assert_eq!(first.code, exit::OK, "{}", first.stderr);
    let created = first.json();
    assert_eq!(created["id"], "com.example.reverb");
    assert_eq!(created["kind"], "effect");
    assert!(crate_dir.join("Cargo.toml").is_file());
    assert!(crate_dir.join("src/lib.rs").is_file());

    // The generated crate describes itself in the one place the build reads.
    let cargo = std::fs::read_to_string(crate_dir.join("Cargo.toml")).expect("a Cargo.toml");
    assert!(cargo.contains("[package.metadata.daux]"), "{cargo}");
    assert!(
        cargo.contains("crate-type = [\"cdylib\", \"rlib\"]"),
        "{cargo}"
    );

    // Writing over somebody's work is not a default.
    let again = daux(&["new", "Reverb", "--path", &crate_str]);
    assert_eq!(again.code, exit::CANNOT_RUN, "{}", again.stdout);
    assert!(again.stderr.contains("--force"), "{}", again.stderr);

    let forced = daux(&["new", "Reverb", "--path", &crate_str, "--force"]);
    assert_eq!(forced.code, exit::OK, "{}", forced.stderr);
}

/// An id is permanent. A malformed one has to be refused at the only moment it is cheap to
/// change.
#[test]
fn new_refuses_an_id_that_is_not_reverse_dns() {
    let dir = TempDir::new("new-bad-id");
    let run = daux(&[
        "new",
        "Reverb",
        "--id",
        "NotReverseDns",
        "--path",
        &dir.path().join("reverb").display().to_string(),
    ]);
    assert_eq!(run.code, exit::CANNOT_RUN);
    assert!(run.stderr.contains("NotReverseDns"), "{}", run.stderr);
    assert!(
        !dir.path().join("reverb").exists(),
        "nothing may be written"
    );
}

/// Each of the three templates has to produce a crate this CLI can read back.
#[test]
fn every_template_kind_scaffolds_something_the_cli_can_read() {
    let dir = TempDir::new("new-kinds");
    for kind in ["effect", "instrument", "midi-effect"] {
        let crate_dir = dir.path().join(kind);
        let run = daux(&[
            "new",
            "Thing",
            "--kind",
            kind,
            "--path",
            &crate_dir.display().to_string(),
            "--json",
        ]);
        assert_eq!(run.code, exit::OK, "{kind}: {}", run.stderr);
        assert_eq!(run.json()["kind"], kind);

        let source = std::fs::read_to_string(crate_dir.join("src/lib.rs")).expect("a lib.rs");
        assert!(
            source.contains("export_plugin!"),
            "{kind} must export an entry point"
        );
        assert!(source.contains("impl DauxPlugin for Thing"), "{kind}");
    }
}

/// A scan of a directory with nothing in it is a successful scan of nothing — not an error,
/// and not a crash.
#[test]
fn scanning_an_empty_directory_finds_nothing_and_succeeds() {
    let dir = TempDir::new("scan-empty");
    let run = daux(&["scan", "--path", &dir.str(), "--json"]);
    assert_eq!(run.code, exit::OK, "{}", run.stderr);
    let report = run.json();
    assert_eq!(report["plugins"].as_array().map(Vec::len), Some(0));
    assert_eq!(report["failures"].as_array().map(Vec::len), Some(0));
}

/// A directory full of things that are not plug-ins is the normal state of a user's disk.
/// None of it may take the scan down.
#[test]
fn a_hostile_directory_tree_does_not_stop_a_scan() {
    let dir = TempDir::new("scan-hostile");
    let good = make_bundle(&dir, "Gain", "com.example.gain");
    assert!(good.is_dir());

    // A bundle whose manifest is not JSON.
    let broken = dir.path().join("out").join("Broken.axt");
    std::fs::create_dir_all(&broken).expect("a directory");
    std::fs::write(broken.join("manifest.json"), b"{ this is not json").expect("a file");

    // A bundle with no metadata at all.
    std::fs::create_dir_all(dir.path().join("out").join("Empty.axt")).expect("a directory");

    // A file that merely looks like a bundle.
    std::fs::write(dir.path().join("out").join("Decoy.axt"), b"not a directory").expect("a file");

    let run = daux(&["scan", "--path", &dir.str(), "--no-probe", "--json"]);
    assert_eq!(run.code, exit::OK, "{}", run.stderr);
    let report = run.json();
    assert_eq!(
        report["plugins"].as_array().map(Vec::len),
        Some(1),
        "the good bundle must still be found: {}",
        run.stdout
    );
    assert!(
        report["failures"]
            .as_array()
            .is_some_and(|failures| failures.len() >= 2),
        "the broken ones must be reported: {}",
        run.stdout
    );

    // And `--strict` is the switch that turns those reports into a failing exit code.
    let strict = daux(&["scan", "--path", &dir.str(), "--no-probe", "--strict"]);
    assert_eq!(strict.code, exit::ISSUES, "{}", strict.stdout);
}

/// `daux build` must refuse a crate that is not a plug-in *before* it spends a minute in
/// the compiler.
#[test]
fn build_refuses_a_crate_that_is_not_a_plug_in_without_compiling_it() {
    let dir = TempDir::new("build-refusals");

    let no_table = dir.path().join("no-table");
    std::fs::create_dir_all(&no_table).expect("a directory");
    std::fs::write(
        no_table.join("Cargo.toml"),
        "[package]\nname = \"thing\"\nversion = \"1.0.0\"\nedition = \"2024\"\n",
    )
    .expect("a Cargo.toml");
    let run = daux(&[
        "build",
        "--manifest-path",
        &no_table.join("Cargo.toml").display().to_string(),
    ]);
    assert_eq!(run.code, exit::CANNOT_RUN);
    assert!(run.stderr.contains("DAUX-M200"), "{}", run.stderr);

    let no_cdylib = dir.path().join("no-cdylib");
    std::fs::create_dir_all(&no_cdylib).expect("a directory");
    std::fs::write(
        no_cdylib.join("Cargo.toml"),
        "[package]\nname = \"thing\"\nversion = \"1.0.0\"\nedition = \"2024\"\n\
         [lib]\ncrate-type = [\"rlib\"]\n\
         [package.metadata.daux]\nid = \"com.example.thing\"\nvendor = \"Example\"\n",
    )
    .expect("a Cargo.toml");
    let run = daux(&[
        "build",
        "--manifest-path",
        &no_cdylib.join("Cargo.toml").display().to_string(),
    ]);
    assert_eq!(run.code, exit::CANNOT_RUN);
    assert!(run.stderr.contains("DAUX-M204"), "{}", run.stderr);
}

/// `--quiet` hides the report and keeps the exit code; it must never hide an error.
#[test]
fn quiet_suppresses_prose_but_not_the_verdict() {
    let dir = TempDir::new("quiet");
    let root = make_bundle(&dir, "Gain", "com.example.gain");
    std::fs::remove_dir_all(root.join("Content")).expect("the binary goes away");

    let run = daux(&["validate", "--quiet", &root.display().to_string()]);
    assert_eq!(run.code, exit::ISSUES);
    assert!(run.stdout.trim().is_empty(), "stdout was:\n{}", run.stdout);
    assert!(
        run.stderr.contains("missing-binary"),
        "the error must survive `--quiet`:\n{}",
        run.stderr
    );
}

/// `daux bundle` without an identity has to say what is missing rather than inventing one.
#[test]
fn bundle_without_an_identity_names_what_is_missing() {
    let dir = TempDir::new("bundle-identity");
    let binary = dir.path().join("plugin.bin");
    std::fs::write(&binary, b"x").expect("a file");

    let run = daux(&["bundle", "--binary", &binary.display().to_string()]);
    assert_eq!(run.code, exit::CANNOT_RUN);
    for flag in ["--id", "--name", "--vendor", "--plugin-version"] {
        assert!(
            run.stderr.contains(flag),
            "{flag} must be listed:\n{}",
            run.stderr
        );
    }
}
