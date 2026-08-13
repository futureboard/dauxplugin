//! Writing a bundle out.

use std::path::{Path, PathBuf};

use crate::{
    BundleError, BundleErrorKind, BundleLayout, BundleResult, Manifest, ManifestCaps,
    ManifestGraphics, TargetId, path_rules,
};

/// Assembles an `.axt` bundle on disk.
///
/// Nothing is written until [`write`](BundleBuilder::write) is called, and `write` builds the
/// bundle in a staging directory and moves it into place at the end. A build interrupted
/// half-way therefore leaves no partial `.axt` for a scanner to find and cache as broken.
#[derive(Clone, Debug)]
pub struct BundleBuilder {
    manifest: Manifest,
    layout: Option<BundleLayout>,
    binaries: Vec<(TargetId, PathBuf)>,
    libraries: Vec<(TargetId, PathBuf)>,
    resource_dir: Option<PathBuf>,
}

impl BundleBuilder {
    /// [main-thread] Starts a bundle for one plug-in.
    ///
    /// The identity is validated immediately: an invalid id is a mistake worth reporting
    /// before any files are copied.
    ///
    /// # Errors
    ///
    /// [`BundleErrorKind::InvalidId`] or [`BundleErrorKind::InvalidVersion`].
    pub fn new(id: &str, name: &str, vendor: &str, version: &str) -> BundleResult<Self> {
        Ok(Self {
            manifest: Manifest::new(id, name, vendor, version)?,
            layout: None,
            binaries: Vec::new(),
            libraries: Vec::new(),
            resource_dir: None,
        })
    }

    /// Forces a layout instead of deriving one from the targets.
    #[must_use]
    pub const fn layout(mut self, layout: BundleLayout) -> Self {
        self.layout = Some(layout);
        self
    }

    /// Adds the plug-in binary for `target`.
    #[must_use]
    pub fn binary(mut self, target: TargetId, from: &Path) -> Self {
        self.binaries.push((target, from.to_path_buf()));
        self
    }

    /// Adds a bundled dependency for `target`.
    #[must_use]
    pub fn library(mut self, target: TargetId, from: &Path) -> Self {
        self.libraries.push((target, from.to_path_buf()));
        self
    }

    /// Copies `from` in as the bundle's resource directory.
    #[must_use]
    pub fn resource_dir(mut self, from: &Path) -> Self {
        self.resource_dir = Some(from.to_path_buf());
        self
    }

    /// Sets the coarse capability bits.
    #[must_use]
    pub const fn capabilities(mut self, capabilities: ManifestCaps) -> Self {
        self.manifest.capabilities = capabilities;
        self
    }

    /// Declares an editor.
    #[must_use]
    pub fn graphics(mut self, graphics: ManifestGraphics) -> Self {
        self.manifest.graphics = Some(graphics);
        self
    }

    /// Sets the plug-in's long description.
    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.manifest.plugin.description = description.into();
        self
    }

    /// Sets the ABI version the binaries were built against.
    #[must_use]
    pub const fn abi_version(mut self, major: u32, minor: u32) -> Self {
        self.manifest.abi_version = major;
        self.manifest.abi_version_minor = minor;
        self
    }

    /// [main-thread] The layout this build will use.
    ///
    /// Explicit if one was set, otherwise derived from the first target — which means a
    /// macOS-only bundle is laid out Apple-style and everything else portably.
    pub fn effective_layout(&self) -> BundleLayout {
        self.layout.unwrap_or_else(|| {
            self.binaries
                .first()
                .map_or(BundleLayout::Posix, |(t, _)| BundleLayout::preferred_for(t))
        })
    }

    /// [main-thread] Writes the bundle into `out_dir` and returns its root.
    ///
    /// The bundle is named `{plugin name}.axt`. An existing bundle of the same name is
    /// replaced only once the new one is complete.
    ///
    /// # Errors
    ///
    /// [`BundleErrorKind::MissingField`] when no binary was added,
    /// [`BundleErrorKind::InvalidBundleName`] when the plug-in name cannot be a directory
    /// name, and [`BundleErrorKind::Io`] for anything the filesystem refuses.
    pub fn write(mut self, out_dir: &Path) -> BundleResult<PathBuf> {
        if self.binaries.is_empty() {
            return Err(BundleError::new(
                BundleErrorKind::MissingField,
                "a bundle needs at least one binary",
            ));
        }

        let layout = self.effective_layout();
        let dir_name = bundle_dir_name(&self.manifest.plugin.name)?;

        self.manifest.targets = self
            .binaries
            .iter()
            .map(|(t, _)| t.clone())
            .collect::<Vec<_>>();
        self.manifest.targets.dedup();
        self.manifest.check()?;
        let manifest_json = self.manifest.to_json()?;

        let final_root = out_dir.join(&dir_name);
        // Staged beside the destination, not in the system temp directory: a cross-volume
        // rename is not atomic, and on Windows it is not even a rename.
        let staging = out_dir.join(format!(".{dir_name}.staging"));
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging).map_err(|e| BundleError::io(&staging, &e))?;

        let result = self.populate(&staging, layout, &manifest_json);
        if let Err(e) = result {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e);
        }

        let _ = std::fs::remove_dir_all(&final_root);
        std::fs::rename(&staging, &final_root).map_err(|e| {
            let _ = std::fs::remove_dir_all(&staging);
            BundleError::io(&final_root, &e)
        })?;
        Ok(final_root)
    }

    /// Fills a staging directory with everything the bundle needs.
    fn populate(
        &self,
        staging: &Path,
        layout: BundleLayout,
        manifest_json: &str,
    ) -> BundleResult<()> {
        let manifest_path = staging.join(layout.manifest_path());
        create_parent(&manifest_path)?;
        std::fs::write(&manifest_path, manifest_json)
            .map_err(|e| BundleError::io(&manifest_path, &e))?;

        if layout == BundleLayout::Apple {
            let plist_path = staging.join(layout.metadata_path());
            create_parent(&plist_path)?;
            std::fs::write(&plist_path, self.info_plist())
                .map_err(|e| BundleError::io(&plist_path, &e))?;
        }

        for (target, from) in &self.binaries {
            let dir = staging.join(layout.binary_dir(target));
            std::fs::create_dir_all(&dir).map_err(|e| BundleError::io(&dir, &e))?;
            copy_into(from, &dir)?;
        }

        for (target, from) in &self.libraries {
            let dir = staging.join(layout.library_dir(target, self.manifest.library_dir_name()));
            std::fs::create_dir_all(&dir).map_err(|e| BundleError::io(&dir, &e))?;
            copy_into(from, &dir)?;
        }

        if let Some(from) = &self.resource_dir {
            let to = staging.join(layout.resource_dir(self.manifest.resource_dir_name()));
            copy_tree(from, &to)?;
        }
        Ok(())
    }

    /// The minimal `Info.plist` an Apple-layout bundle needs.
    ///
    /// Deliberately hand-written rather than serialised: it is a fixed handful of keys, and
    /// the real metadata lives in the `manifest.json` beside it.
    fn info_plist(&self) -> String {
        let plugin = &self.manifest.plugin;
        let escape = |s: &str| {
            s.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
        };
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleIdentifier</key><string>{id}</string>
	<key>CFBundleName</key><string>{name}</string>
	<key>CFBundlePackageType</key><string>BNDL</string>
	<key>CFBundleShortVersionString</key><string>{version}</string>
	<key>CFBundleExecutable</key><string>{name}</string>
	<key>DAUxPlugin</key>
	<dict>
		<key>id</key><string>{id}</string>
		<key>name</key><string>{name}</string>
		<key>vendor</key><string>{vendor}</string>
		<key>version</key><string>{version}</string>
		<key>abiVersion</key><integer>{abi}</integer>
		<key>abiVersionMinor</key><integer>{abi_minor}</integer>
		<key>capabilities</key><integer>{caps}</integer>
	</dict>
</dict>
</plist>
"#,
            id = escape(&plugin.id),
            name = escape(&plugin.name),
            vendor = escape(&plugin.vendor),
            version = escape(&plugin.version),
            abi = self.manifest.abi_version,
            abi_minor = self.manifest.abi_version_minor,
            caps = self.manifest.capabilities.bits(),
        )
    }
}

/// Turns a plug-in name into a bundle directory name.
fn bundle_dir_name(name: &str) -> BundleResult<String> {
    let trimmed = name.trim();
    path_rules::validate_component(trimmed).map_err(|e| {
        BundleError::new(
            BundleErrorKind::InvalidBundleName,
            format!("`{trimmed}` cannot be a bundle directory name: {e}"),
        )
    })?;
    Ok(format!("{trimmed}.axt"))
}

/// Creates the parent directory of `path`.
fn create_parent(path: &Path) -> BundleResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| BundleError::io(parent, &e))?;
    }
    Ok(())
}

/// Copies one file into a directory, keeping its file name.
fn copy_into(from: &Path, dir: &Path) -> BundleResult<()> {
    let name = from.file_name().ok_or_else(|| {
        BundleError::new(BundleErrorKind::InvalidField, "the source has no file name")
            .with_path(from)
    })?;
    let to = dir.join(name);
    std::fs::copy(from, &to).map_err(|e| BundleError::io(from, &e))?;
    Ok(())
}

/// Copies a directory tree, following no symlinks.
///
/// A symlink inside a source tree would be copied as whatever it points at, which is how a
/// build ends up shipping something from outside the source directory. Skipping them is the
/// conservative choice; a resource that must be a link is a packaging decision, not a
/// default.
fn copy_tree(from: &Path, to: &Path) -> BundleResult<()> {
    std::fs::create_dir_all(to).map_err(|e| BundleError::io(to, &e))?;
    let entries = std::fs::read_dir(from).map_err(|e| BundleError::io(from, &e))?;
    for entry in entries {
        let entry = entry.map_err(|e| BundleError::io(from, &e))?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|e| BundleError::io(&path, &e))?;
        if file_type.is_symlink() {
            continue;
        }
        let target = to.join(entry.file_name());
        if file_type.is_dir() {
            copy_tree(&path, &target)?;
        } else {
            std::fs::copy(&path, &target).map_err(|e| BundleError::io(&path, &e))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Bundle, testutil::tempdir};

    fn fake_binary(dir: &crate::testutil::TempDir, name: &str) -> PathBuf {
        dir.write(name, "not really a binary")
    }

    #[test]
    fn a_written_bundle_opens_again() {
        let src = tempdir();
        let out = tempdir();
        let target = TargetId::parse("windows-x86_64").unwrap();
        let binary = fake_binary(&src, "Gain.dll");

        let root = BundleBuilder::new("com.example.gain", "Gain", "Example Audio", "1.0.0")
            .unwrap()
            .binary(target.clone(), &binary)
            .description("A gain plug-in.")
            .write(out.path())
            .unwrap();

        assert_eq!(root.file_name().unwrap(), "Gain.axt");
        let bundle = Bundle::open(&root).unwrap();
        assert_eq!(bundle.metadata().id, "com.example.gain");
        assert_eq!(bundle.metadata().description, "A gain plug-in.");
        assert!(bundle.metadata().supports(&target));
        assert!(bundle.binary_path(&target).is_ok());
    }

    #[test]
    fn the_targets_come_from_the_binaries_that_were_added() {
        let src = tempdir();
        let out = tempdir();
        let win = TargetId::parse("windows-x86_64").unwrap();
        let linux = TargetId::parse("linux-x86_64").unwrap();

        let root = BundleBuilder::new("com.example.multi", "Multi", "Example", "1.0.0")
            .unwrap()
            .binary(win.clone(), &fake_binary(&src, "Multi.dll"))
            .binary(linux.clone(), &fake_binary(&src, "libMulti.so"))
            .write(out.path())
            .unwrap();

        let bundle = Bundle::open(&root).unwrap();
        assert!(bundle.metadata().supports(&win));
        assert!(bundle.metadata().supports(&linux));
        assert!(bundle.binary_path(&win).is_ok());
        assert!(bundle.binary_path(&linux).is_ok());
    }

    #[test]
    fn a_bundle_with_no_binary_is_refused() {
        let out = tempdir();
        let err = BundleBuilder::new("com.example.empty", "Empty", "Example", "1.0.0")
            .unwrap()
            .write(out.path())
            .unwrap_err();
        assert_eq!(*err.kind(), BundleErrorKind::MissingField);
        // Nothing was left behind.
        assert!(!out.path().join("Empty.axt").exists());
    }

    #[test]
    fn a_name_that_cannot_be_a_directory_is_refused() {
        let src = tempdir();
        let out = tempdir();
        for hostile in ["../escape", "CON", "with/slash"] {
            let err = BundleBuilder::new("com.example.x", hostile, "Example", "1.0.0")
                .unwrap()
                .binary(
                    TargetId::parse("windows-x86_64").unwrap(),
                    &fake_binary(&src, "X.dll"),
                )
                .write(out.path())
                .unwrap_err();
            assert_eq!(
                *err.kind(),
                BundleErrorKind::InvalidBundleName,
                "`{hostile}` must be refused"
            );
        }
    }

    #[test]
    fn resources_are_copied_and_readable() {
        let src = tempdir();
        let out = tempdir();
        src.write("res/fonts/Inter.txt", "hello");
        let binary = fake_binary(&src, "Gain.dll");

        let root = BundleBuilder::new("com.example.gain", "Gain", "Example", "1.0.0")
            .unwrap()
            .binary(TargetId::parse("windows-x86_64").unwrap(), &binary)
            .resource_dir(&src.path().join("res"))
            .write(out.path())
            .unwrap();

        let bundle = Bundle::open(&root).unwrap();
        assert_eq!(
            bundle
                .resources()
                .read_to_string("fonts/Inter.txt")
                .unwrap(),
            "hello"
        );
    }

    #[test]
    fn the_apple_layout_writes_both_metadata_files() {
        let src = tempdir();
        let out = tempdir();
        let target = TargetId::parse("macos-universal").unwrap();

        let root = BundleBuilder::new("com.example.gain", "Gain", "Example", "1.0.0")
            .unwrap()
            .binary(target.clone(), &fake_binary(&src, "Gain"))
            .write(out.path())
            .unwrap();

        assert!(root.join("Contents/Info.plist").is_file());
        assert!(root.join("Contents/Resources/manifest.json").is_file());
        assert!(root.join("Contents/MacOS/Gain").is_file());

        let bundle = Bundle::open(&root).unwrap();
        assert_eq!(bundle.layout(), BundleLayout::Apple);
        // The nested manifest wins, so targets survive the round trip.
        assert!(bundle.metadata().supports(&target));
        assert!(bundle.binary_path(&target).is_ok());
    }

    #[test]
    fn the_layout_is_derived_from_the_first_target_unless_forced() {
        let src = tempdir();
        let binary = fake_binary(&src, "X.dll");
        let apple = BundleBuilder::new("com.example.x", "X", "E", "1.0.0")
            .unwrap()
            .binary(TargetId::parse("macos-arm64").unwrap(), &binary);
        assert_eq!(apple.effective_layout(), BundleLayout::Apple);
        assert_eq!(
            apple.layout(BundleLayout::Posix).effective_layout(),
            BundleLayout::Posix
        );

        let posix = BundleBuilder::new("com.example.x", "X", "E", "1.0.0")
            .unwrap()
            .binary(TargetId::parse("linux-x86_64").unwrap(), &binary);
        assert_eq!(posix.effective_layout(), BundleLayout::Posix);
    }

    #[test]
    fn writing_twice_replaces_the_previous_bundle() {
        let src = tempdir();
        let out = tempdir();
        let target = TargetId::parse("windows-x86_64").unwrap();
        let binary = fake_binary(&src, "Gain.dll");

        let build = |version: &str| {
            BundleBuilder::new("com.example.gain", "Gain", "Example", version)
                .unwrap()
                .binary(target.clone(), &binary)
                .write(out.path())
                .unwrap()
        };

        let first = build("1.0.0");
        let second = build("2.0.0");
        assert_eq!(first, second);
        assert_eq!(Bundle::open(&second).unwrap().metadata().version, "2.0.0");
        // No staging directory survives a successful build.
        assert!(!out.path().join(".Gain.axt.staging").exists());
    }

    #[test]
    fn symlinked_resources_are_skipped_rather_than_followed() {
        let src = tempdir();
        let out = tempdir();
        src.write("res/real.txt", "inside");
        src.write("secret.txt", "outside");

        // Only meaningful where the platform lets an unprivileged process make a symlink;
        // where it does not, the copy simply has nothing extra to skip.
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(
            src.path().join("secret.txt"),
            src.path().join("res/leak.txt"),
        )
        .is_ok();
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_file(
            src.path().join("secret.txt"),
            src.path().join("res/leak.txt"),
        )
        .is_ok();

        let root = BundleBuilder::new("com.example.gain", "Gain", "Example", "1.0.0")
            .unwrap()
            .binary(
                TargetId::parse("windows-x86_64").unwrap(),
                &fake_binary(&src, "Gain.dll"),
            )
            .resource_dir(&src.path().join("res"))
            .write(out.path())
            .unwrap();

        let bundle = Bundle::open(&root).unwrap();
        assert_eq!(
            bundle.resources().read_to_string("real.txt").unwrap(),
            "inside"
        );
        if linked {
            assert!(
                !bundle.resources().exists("leak.txt"),
                "a symlink out of the source tree must not be copied in"
            );
        }
    }
}
