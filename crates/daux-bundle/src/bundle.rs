//! Opening a bundle and reading what is inside it, safely.

use std::path::{Path, PathBuf};

use crate::{
    BundleError, BundleErrorKind, BundleLayout, BundleMetadata, BundleResult,
    DEFAULT_MAX_RESOURCE_BYTES, TargetId, path_rules,
};

/// How serious a validation finding is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// The bundle will not load, or will load and misbehave.
    Error,
    /// The bundle will load, but something is wrong or will break later.
    Warning,
    /// Worth knowing, nothing is wrong.
    Info,
}

impl Severity {
    /// [any-thread] The stable lower-case name used in CLI output.
    pub const fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
        }
    }
}

impl core::fmt::Display for Severity {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One finding from [`Bundle::validate`].
///
/// `code` is stable and machine-readable; `message` is for a human and may change.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ValidationIssue {
    /// How serious it is.
    pub severity: Severity,
    /// A stable identifier, e.g. `"missing-binary"`.
    pub code: &'static str,
    /// A human-readable explanation.
    pub message: String,
}

impl ValidationIssue {
    /// [main-thread] An error-severity finding.
    pub fn error(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            code,
            message: message.into(),
        }
    }

    /// [main-thread] A warning-severity finding.
    pub fn warning(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            code,
            message: message.into(),
        }
    }

    /// [main-thread] An informational finding.
    pub fn info(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Info,
            code,
            message: message.into(),
        }
    }
}

impl core::fmt::Display for ValidationIssue {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}: [{}] {}", self.severity, self.code, self.message)
    }
}

/// An opened `.axt` bundle.
///
/// Opening reads and validates the metadata but loads no code and touches no binary. A
/// scanner can open thousands of these; only [`Bundle::binary_path`] leads anywhere near a
/// `dlopen`.
#[derive(Clone, Debug)]
pub struct Bundle {
    path: PathBuf,
    layout: BundleLayout,
    metadata: BundleMetadata,
}

impl Bundle {
    /// [main-thread] Opens the bundle at `path`.
    ///
    /// # Errors
    ///
    /// [`BundleErrorKind::NotADirectory`] when `path` is a file,
    /// [`BundleErrorKind::NotAxtExtension`] when it is not named `*.axt`,
    /// [`BundleErrorKind::AmbiguousLayout`] or [`BundleErrorKind::NotABundle`] from layout
    /// detection, and whatever metadata parsing reports.
    pub fn open(path: &Path) -> BundleResult<Self> {
        let meta = std::fs::metadata(path).map_err(|e| BundleError::io(path, &e))?;
        if !meta.is_dir() {
            return Err(BundleError::new(
                BundleErrorKind::NotADirectory,
                "a bundle is a directory, not a file",
            )
            .with_path(path));
        }
        if !path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("axt"))
        {
            return Err(BundleError::new(
                BundleErrorKind::NotAxtExtension,
                "a bundle directory must be named `*.axt`",
            )
            .with_path(path));
        }

        let layout = BundleLayout::detect(path)?;
        let metadata = BundleMetadata::read(path, layout)?;
        Ok(Self {
            path: path.to_path_buf(),
            layout,
            metadata,
        })
    }

    /// [main-thread] The bundle's root directory.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// [main-thread] Which on-disk shape this bundle uses.
    pub const fn layout(&self) -> BundleLayout {
        self.layout
    }

    /// [main-thread] The layout-independent metadata.
    pub const fn metadata(&self) -> &BundleMetadata {
        &self.metadata
    }

    /// [main-thread] The dynamic library to load for `target`.
    ///
    /// The file name is not assumed: the binary directory is scanned for exactly one file
    /// with the platform's dynamic-library extension, so a bundle whose binary was renamed
    /// still loads and one with two candidates is rejected rather than picked from at random.
    ///
    /// # Errors
    ///
    /// [`BundleErrorKind::NoBinaryForTarget`] when the directory is missing or holds no
    /// candidate, and [`BundleErrorKind::AmbiguousBinary`] when it holds more than one.
    pub fn binary_path(&self, target: &TargetId) -> BundleResult<PathBuf> {
        let dir = self.path.join(self.layout.binary_dir(target));
        let extension = target.dylib_extension();

        let entries = std::fs::read_dir(&dir).map_err(|e| {
            BundleError::new(
                BundleErrorKind::NoBinaryForTarget,
                format!("no binary directory for target `{target}`: {e}"),
            )
            .with_path(&dir)
        })?;

        let mut found: Vec<PathBuf> = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| BundleError::io(&dir, &e))?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            // An Apple bundle's `Contents/MacOS` binary conventionally has no extension at
            // all; every other platform requires the right one.
            let matches = match path.extension() {
                Some(e) => e.eq_ignore_ascii_case(extension),
                None => self.layout == BundleLayout::Apple,
            };
            if matches {
                found.push(path);
            }
        }
        found.sort();

        match found.len() {
            0 => Err(BundleError::new(
                BundleErrorKind::NoBinaryForTarget,
                format!("no `*.{extension}` in the binary directory for target `{target}`"),
            )
            .with_path(dir)),
            1 => Ok(found.remove(0)),
            n => Err(BundleError::new(
                BundleErrorKind::AmbiguousBinary,
                format!("{n} candidate binaries for target `{target}`; expected exactly one"),
            )
            .with_path(dir)),
        }
    }

    /// [main-thread] The directory holding bundled dependencies for `target`, if it exists.
    ///
    /// A loader adds this to the dynamic linker's search path before opening the binary.
    pub fn library_dir(&self, target: &TargetId) -> Option<PathBuf> {
        let dir = self
            .path
            .join(self.layout.library_dir(target, &self.metadata.library_dir_name));
        dir.is_dir().then_some(dir)
    }

    /// [main-thread] Confined access to the bundle's resources.
    pub fn resources(&self) -> ResourceDir {
        ResourceDir {
            root: self
                .path
                .join(self.layout.resource_dir(&self.metadata.resource_dir_name)),
            max_bytes: DEFAULT_MAX_RESOURCE_BYTES,
        }
    }

    /// [main-thread] Every problem this bundle has, worst first.
    ///
    /// Unlike [`open`](Self::open), this never stops at the first failure: `daux validate`
    /// exists to report everything at once. An empty result means the bundle is sound.
    pub fn validate(&self) -> Vec<ValidationIssue> {
        let mut issues = Vec::new();

        if self.metadata.targets.is_empty() {
            issues.push(ValidationIssue::error(
                "no-targets",
                "the manifest declares no targets, so nothing can ever load this bundle",
            ));
        }

        for target in &self.metadata.targets {
            match self.binary_path(target) {
                Ok(_) => {}
                Err(e) => issues.push(ValidationIssue::error(
                    "missing-binary",
                    format!("target `{target}` is declared but has no loadable binary: {e}"),
                )),
            }
            if !target.is_known() {
                issues.push(ValidationIssue::warning(
                    "unknown-target",
                    format!("target `{target}` is not one this build recognises"),
                ));
            }
        }

        if !self
            .metadata
            .targets
            .iter()
            .any(|t| t == &TargetId::host())
        {
            issues.push(ValidationIssue::info(
                "not-loadable-here",
                format!(
                    "no binary for this machine (`{}`); the bundle is still valid",
                    TargetId::host()
                ),
            ));
        }

        if self.metadata.vendor.trim().is_empty() {
            issues.push(ValidationIssue::warning(
                "no-vendor",
                "the manifest declares no vendor",
            ));
        }

        let resources = self.resources();
        for logical in self.required_resources() {
            if !resources.exists(&logical) {
                issues.push(ValidationIssue::error(
                    "missing-resource",
                    format!("`{logical}` is declared required but is not in the bundle"),
                ));
            }
        }

        if self.metadata.has_editor() && !resources.root().is_dir() {
            issues.push(ValidationIssue::warning(
                "no-resource-dir",
                "the plug-in declares an editor but the bundle has no resource directory",
            ));
        }

        issues.sort_by(|a, b| a.severity.cmp(&b.severity).then(a.code.cmp(b.code)));
        issues
    }

    /// The resource paths the manifest marks required, read back from the manifest file.
    ///
    /// Read on demand rather than kept in [`BundleMetadata`]: only validation needs it, and
    /// a scanner caching metadata for thousands of bundles should not carry it.
    fn required_resources(&self) -> Vec<String> {
        let path = self.path.join(self.layout.manifest_path());
        let Ok(bytes) = std::fs::read(&path) else {
            return Vec::new();
        };
        let Ok(manifest) = crate::Manifest::from_json_bytes(&bytes) else {
            return Vec::new();
        };
        manifest
            .resources
            .map(|r| r.required)
            .unwrap_or_default()
    }
}

/// Confined read access to a bundle's resource directory.
///
/// Every lookup goes through [`path_rules`], which rejects `..`, absolute paths, drive
/// letters, UNC prefixes, Windows device names and symlink escapes. A plug-in id or a
/// resource name in a manifest is attacker-controlled in exactly the way a downloaded plug-in
/// is attacker-controlled, so "it came from our own manifest" is not a reason to trust it.
#[derive(Clone, Debug)]
pub struct ResourceDir {
    root: PathBuf,
    max_bytes: u64,
}

impl ResourceDir {
    /// [main-thread] The resource directory itself. May not exist.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// [main-thread] Sets the largest single resource this will read.
    ///
    /// Defaults to [`DEFAULT_MAX_RESOURCE_BYTES`].
    #[must_use]
    pub const fn with_max_bytes(mut self, max_bytes: u64) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    /// [main-thread] Turns a logical path such as `"fonts/Inter.ttf"` into a real one.
    ///
    /// # Errors
    ///
    /// [`BundleErrorKind::PathEscape`] when the path would leave the bundle by any route,
    /// including through a symlink, and [`BundleErrorKind::NotFound`] when it does not exist.
    pub fn resolve(&self, logical: &str) -> BundleResult<PathBuf> {
        path_rules::resolve_within(&self.root, logical)
    }

    /// [main-thread] `true` when `logical` names an existing file inside the bundle.
    ///
    /// A path that escapes the bundle is reported as absent rather than as an error: callers
    /// asking "is it there?" should not have to distinguish "no" from "no, and hostile".
    pub fn exists(&self, logical: &str) -> bool {
        self.resolve(logical).is_ok_and(|p| p.is_file())
    }

    /// [main-thread] Reads a resource.
    ///
    /// # Errors
    ///
    /// Whatever [`resolve`](Self::resolve) reports, plus [`BundleErrorKind::TooLarge`] for a
    /// file over the configured cap and [`BundleErrorKind::NotRegularFile`] for a directory
    /// or device.
    pub fn read(&self, logical: &str) -> BundleResult<Vec<u8>> {
        let path = self.resolve(logical)?;
        let meta = std::fs::metadata(&path).map_err(|e| BundleError::io(&path, &e))?;
        if !meta.is_file() {
            return Err(BundleError::new(
                BundleErrorKind::NotRegularFile,
                "the resource is not a regular file",
            )
            .with_path(path));
        }
        if meta.len() > self.max_bytes {
            return Err(BundleError::new(
                BundleErrorKind::TooLarge,
                format!(
                    "the resource is {} bytes, over the {}-byte limit",
                    meta.len(),
                    self.max_bytes
                ),
            )
            .with_path(path));
        }
        std::fs::read(&path).map_err(|e| BundleError::io(&path, &e))
    }

    /// [main-thread] Reads a resource as UTF-8 text.
    ///
    /// # Errors
    ///
    /// Whatever [`read`](Self::read) reports, plus [`BundleErrorKind::Encoding`] when the
    /// bytes are not valid UTF-8.
    pub fn read_to_string(&self, logical: &str) -> BundleResult<String> {
        let bytes = self.read(logical)?;
        String::from_utf8(bytes).map_err(|e| {
            BundleError::new(
                BundleErrorKind::Encoding,
                format!("the resource is not valid UTF-8: {e}"),
            )
            .with_path(self.root.join(logical))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Manifest, testutil::tempdir};

    /// Builds a minimal loadable Posix bundle and returns its root.
    fn gain_bundle(targets: &[&str]) -> (crate::testutil::TempDir, PathBuf) {
        let dir = tempdir();
        let root = dir.dir("Gain.axt");

        let mut m = Manifest::new("com.example.gain", "Gain", "Example Audio", "1.0.0")
            .expect("a well-formed identity");
        m.targets = targets
            .iter()
            .map(|t| TargetId::parse(t).expect("a known target"))
            .collect();
        std::fs::write(root.join("manifest.json"), m.to_json().unwrap()).unwrap();

        for t in targets {
            let target = TargetId::parse(t).unwrap();
            let bin_dir = root.join(format!("Content/{t}"));
            std::fs::create_dir_all(&bin_dir).unwrap();
            std::fs::write(
                bin_dir.join(format!("Gain.{}", target.dylib_extension())),
                b"not really a binary",
            )
            .unwrap();
        }
        (dir, root)
    }

    #[test]
    fn opening_reads_metadata_without_touching_a_binary() {
        let (_dir, root) = gain_bundle(&["windows-x86_64"]);
        let bundle = Bundle::open(&root).unwrap();

        assert_eq!(bundle.layout(), BundleLayout::Posix);
        assert_eq!(bundle.metadata().id, "com.example.gain");
        assert_eq!(bundle.path(), root);
    }

    #[test]
    fn a_directory_without_the_axt_extension_is_refused() {
        let dir = tempdir();
        let root = dir.dir("Gain");
        std::fs::write(root.join("manifest.json"), "{}").unwrap();

        let err = Bundle::open(&root).unwrap_err();
        assert_eq!(*err.kind(), BundleErrorKind::NotAxtExtension);
    }

    #[test]
    fn a_file_is_not_a_bundle() {
        let dir = tempdir();
        let file = dir.write("Gain.axt", "not a directory");
        let err = Bundle::open(&file).unwrap_err();
        assert_eq!(*err.kind(), BundleErrorKind::NotADirectory);
    }

    #[test]
    fn the_binary_is_found_by_extension_not_by_name() {
        let (_dir, root) = gain_bundle(&["windows-x86_64"]);
        let bundle = Bundle::open(&root).unwrap();
        let target = TargetId::parse("windows-x86_64").unwrap();

        let binary = bundle.binary_path(&target).unwrap();
        assert_eq!(binary.extension().unwrap(), "dll");

        // Renaming it changes nothing: the extension is what identifies it.
        std::fs::rename(&binary, binary.with_file_name("Renamed.dll")).unwrap();
        assert!(bundle.binary_path(&target).is_ok());
    }

    #[test]
    fn two_candidate_binaries_are_refused_rather_than_guessed_at() {
        let (_dir, root) = gain_bundle(&["windows-x86_64"]);
        let bundle = Bundle::open(&root).unwrap();
        let target = TargetId::parse("windows-x86_64").unwrap();
        std::fs::write(root.join("Content/windows-x86_64/Other.dll"), b"x").unwrap();

        let err = bundle.binary_path(&target).unwrap_err();
        assert_eq!(*err.kind(), BundleErrorKind::AmbiguousBinary);
    }

    #[test]
    fn an_undeclared_target_has_no_binary() {
        let (_dir, root) = gain_bundle(&["windows-x86_64"]);
        let bundle = Bundle::open(&root).unwrap();
        let err = bundle
            .binary_path(&TargetId::parse("linux-x86_64").unwrap())
            .unwrap_err();
        assert_eq!(*err.kind(), BundleErrorKind::NoBinaryForTarget);
    }

    #[test]
    fn resources_are_confined_to_the_bundle() {
        let (_dir, root) = gain_bundle(&["windows-x86_64"]);
        std::fs::create_dir_all(root.join("Resources/fonts")).unwrap();
        std::fs::write(root.join("Resources/fonts/Inter.txt"), "hello").unwrap();
        let bundle = Bundle::open(&root).unwrap();
        let resources = bundle.resources();

        assert_eq!(resources.read_to_string("fonts/Inter.txt").unwrap(), "hello");
        assert!(resources.exists("fonts/Inter.txt"));
        assert!(!resources.exists("fonts/Missing.txt"));

        for hostile in [
            "../manifest.json",
            "../../etc/passwd",
            "/etc/passwd",
            "fonts/../../manifest.json",
        ] {
            let err = resources.read(hostile).unwrap_err();
            assert_eq!(
                *err.kind(),
                BundleErrorKind::PathEscape,
                "`{hostile}` must not escape"
            );
            assert!(!resources.exists(hostile), "`{hostile}` must read as absent");
        }
    }

    #[test]
    fn an_oversized_resource_is_refused() {
        let (_dir, root) = gain_bundle(&["windows-x86_64"]);
        std::fs::create_dir_all(root.join("Resources")).unwrap();
        std::fs::write(root.join("Resources/big.bin"), vec![0u8; 1024]).unwrap();
        let bundle = Bundle::open(&root).unwrap();

        let resources = bundle.resources().with_max_bytes(512);
        let err = resources.read("big.bin").unwrap_err();
        assert_eq!(*err.kind(), BundleErrorKind::TooLarge);

        assert!(bundle.resources().read("big.bin").is_ok());
    }

    #[test]
    fn non_utf8_resources_report_an_encoding_error() {
        let (_dir, root) = gain_bundle(&["windows-x86_64"]);
        std::fs::create_dir_all(root.join("Resources")).unwrap();
        std::fs::write(root.join("Resources/raw.bin"), [0xff, 0xfe, 0x00]).unwrap();
        let bundle = Bundle::open(&root).unwrap();

        assert!(bundle.resources().read("raw.bin").is_ok());
        let err = bundle.resources().read_to_string("raw.bin").unwrap_err();
        assert_eq!(*err.kind(), BundleErrorKind::Encoding);
    }

    #[test]
    fn a_sound_bundle_validates_with_no_errors() {
        let (_dir, root) = gain_bundle(&[TargetId::host().as_str()]);
        let bundle = Bundle::open(&root).unwrap();

        let issues = bundle.validate();
        assert!(
            !issues.iter().any(|i| i.severity == Severity::Error),
            "{issues:#?}"
        );
    }

    #[test]
    fn a_declared_target_with_no_binary_is_an_error() {
        let (_dir, root) = gain_bundle(&["windows-x86_64"]);
        std::fs::remove_dir_all(root.join("Content/windows-x86_64")).unwrap();
        let bundle = Bundle::open(&root).unwrap();

        let issues = bundle.validate();
        assert!(
            issues
                .iter()
                .any(|i| i.code == "missing-binary" && i.severity == Severity::Error),
            "{issues:#?}"
        );
        // Errors sort first.
        assert_eq!(issues[0].severity, Severity::Error);
    }

    #[test]
    fn a_missing_required_resource_is_an_error() {
        let (_dir, root) = gain_bundle(&["windows-x86_64"]);
        let mut m = Manifest::new("com.example.gain", "Gain", "Example Audio", "1.0.0").unwrap();
        m.targets = vec![TargetId::parse("windows-x86_64").unwrap()];
        m.resources = Some(crate::ManifestResources {
            required: vec!["fonts/Inter.ttf".to_owned()],
            ..crate::ManifestResources::default()
        });
        std::fs::write(root.join("manifest.json"), m.to_json().unwrap()).unwrap();
        let bundle = Bundle::open(&root).unwrap();

        let issues = bundle.validate();
        assert!(
            issues.iter().any(|i| i.code == "missing-resource"),
            "{issues:#?}"
        );
    }

    #[test]
    fn a_bundle_for_another_machine_is_informational_not_an_error() {
        let other = if TargetId::host().as_str() == "windows-x86_64" {
            "linux-x86_64"
        } else {
            "windows-x86_64"
        };
        let (_dir, root) = gain_bundle(&[other]);
        let bundle = Bundle::open(&root).unwrap();

        let issues = bundle.validate();
        let not_here = issues
            .iter()
            .find(|i| i.code == "not-loadable-here")
            .expect("the finding is reported");
        assert_eq!(not_here.severity, Severity::Info);
    }

    #[test]
    fn issues_render_with_their_code() {
        let issue = ValidationIssue::error("missing-binary", "nothing there");
        assert_eq!(issue.to_string(), "error: [missing-binary] nothing there");
        assert_eq!(
            ValidationIssue::warning("x", "y").severity,
            Severity::Warning
        );
        assert_eq!(ValidationIssue::info("x", "y").severity, Severity::Info);
    }
}
