//! Synthetic `.axt` bundles — the well-formed ones and the deliberately hostile ones.
//!
//! A bundle is a directory (`axt-v1` §1), so a bundle test needs real files. [`Synthetic`]
//! describes one declaratively and can put it on disk two ways:
//!
//! * [`Synthetic::write`] goes through [`daux_bundle::BundleBuilder`], which is the path a
//!   real `daux build` takes. Use it whenever the test is about the round trip.
//! * [`Synthetic::write_tree`] lays the directory out by hand. Use it when the shape under
//!   test is one the builder would refuse to produce — a declared target with no binary, a
//!   manifest that is not JSON, two metadata files at once.
//!
//! The hostile shapes have their own constructors ([`corrupt`], [`oversized_metadata`],
//! [`ambiguous_layout`], [`missing_binary`]) because a test reads better when its fixture
//! is named after the failure it is provoking.

use std::path::{Path, PathBuf};

use daux_bundle::{
    BundleBuilder, BundleLayout, BundleResult, Manifest, ManifestCaps, TargetId, limits,
};

/// The bytes every fixture uses where a real bundle would carry a dynamic library.
///
/// Deliberately not a loadable image: a bundle test must never be one `LoadLibrary` away
/// from executing something, and the scanner tests want a binary that fails to load.
pub const FAKE_BINARY: &[u8] = b"DAUx test harness: not a real dynamic library\n";

/// A bundle described declaratively, written on demand.
#[derive(Clone, Debug)]
pub struct Synthetic {
    /// Display name; also the `<BundleName>.axt` directory name.
    pub name: String,
    /// Permanent reverse-DNS id.
    pub id: String,
    /// Vendor name.
    pub vendor: String,
    /// `major.minor.patch` version string.
    pub version: String,
    /// Long description.
    pub description: String,
    /// Targets the bundle claims to ship.
    pub targets: Vec<TargetId>,
    /// Layout to force, or `None` to let the builder derive one from the first target.
    pub layout: Option<BundleLayout>,
    /// Coarse capability bits.
    pub capabilities: ManifestCaps,
    /// ABI major version the binaries claim to be built against.
    pub abi_version: u32,
    /// Resources, as `(logical path, bytes)`.
    pub resources: Vec<(String, Vec<u8>)>,
    /// When `false`, [`Synthetic::write_tree`] declares the targets but ships no binary.
    pub with_binaries: bool,
}

impl Synthetic {
    /// [main-thread] A well-formed single-target bundle for this machine.
    #[must_use]
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            id: format!("studio.futureboard.tests.{}", name.to_ascii_lowercase()),
            vendor: "Futureboard Studio".to_owned(),
            version: "1.0.0".to_owned(),
            description: format!("The {name} test fixture."),
            targets: vec![TargetId::host()],
            layout: None,
            capabilities: ManifestCaps::empty(),
            abi_version: daux_abi::DAUX_ABI_VERSION_MAJOR,
            resources: Vec::new(),
            with_binaries: true,
        }
    }

    /// Sets the permanent plug-in id.
    #[must_use]
    pub fn id(mut self, id: &str) -> Self {
        self.id = id.to_owned();
        self
    }

    /// Sets the vendor.
    #[must_use]
    pub fn vendor(mut self, vendor: &str) -> Self {
        self.vendor = vendor.to_owned();
        self
    }

    /// Sets the version string.
    #[must_use]
    pub fn version(mut self, version: &str) -> Self {
        self.version = version.to_owned();
        self
    }

    /// Replaces the target list.
    #[must_use]
    pub fn targets(mut self, targets: &[&str]) -> Self {
        self.targets = targets
            .iter()
            .map(|t| TargetId::parse(t).expect("a well-formed target id"))
            .collect();
        self
    }

    /// Forces a layout instead of deriving one.
    #[must_use]
    pub const fn layout(mut self, layout: BundleLayout) -> Self {
        self.layout = Some(layout);
        self
    }

    /// Sets the coarse capability bits.
    #[must_use]
    pub const fn capabilities(mut self, capabilities: ManifestCaps) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Sets the ABI major version the manifest declares.
    #[must_use]
    pub const fn abi_version(mut self, major: u32) -> Self {
        self.abi_version = major;
        self
    }

    /// Adds a resource at `logical`, e.g. `"fonts/Inter.txt"`.
    #[must_use]
    pub fn resource(mut self, logical: &str, bytes: &[u8]) -> Self {
        self.resources.push((logical.to_owned(), bytes.to_vec()));
        self
    }

    /// Declares the targets but ships no binary for them. [`Synthetic::write_tree`] only.
    #[must_use]
    pub const fn without_binaries(mut self) -> Self {
        self.with_binaries = false;
        self
    }

    /// [main-thread] The `manifest.json` this fixture describes.
    ///
    /// # Errors
    ///
    /// Whatever [`Manifest::to_json`] reports for an identity this fixture cannot express.
    pub fn manifest(&self) -> BundleResult<Manifest> {
        let mut manifest = Manifest::new(&self.id, &self.name, &self.vendor, &self.version)?;
        manifest.plugin.description = self.description.clone();
        manifest.targets = self.targets.clone();
        manifest.capabilities = self.capabilities;
        manifest.abi_version = self.abi_version;
        Ok(manifest)
    }

    /// [main-thread] Writes the bundle through [`BundleBuilder`] and returns its root.
    ///
    /// This is the production path: staging directory, atomic move, manifest generated
    /// from one source of truth.
    ///
    /// # Errors
    ///
    /// Whatever [`BundleBuilder::write`] reports.
    ///
    /// # Panics
    ///
    /// If the temporary staging tree for the fake binaries cannot be created.
    pub fn write(&self, out_dir: &Path) -> BundleResult<PathBuf> {
        let sources = crate::TempTree::new("bundle-sources");
        let mut builder = BundleBuilder::new(&self.id, &self.name, &self.vendor, &self.version)?
            .description(self.description.clone())
            .capabilities(self.capabilities)
            .abi_version(self.abi_version, 0);
        if let Some(layout) = self.layout {
            builder = builder.layout(layout);
        }
        for target in &self.targets {
            let file = sources.write(
                format!(
                    "{}/{}",
                    target.as_str(),
                    binary_file_name(&self.name, target)
                ),
                FAKE_BINARY,
            );
            builder = builder.binary(target.clone(), &file);
        }
        if !self.resources.is_empty() {
            for (logical, bytes) in &self.resources {
                sources.write(format!("resources/{logical}"), bytes);
            }
            builder = builder.resource_dir(&sources.join("resources"));
        }
        builder.write(out_dir)
    }

    /// [main-thread] Lays the bundle out by hand and returns its root.
    ///
    /// Unlike [`Synthetic::write`] this writes exactly what it is told to, which is how a
    /// fixture with a declared-but-absent binary gets made.
    ///
    /// # Panics
    ///
    /// If any file cannot be written, or if the identity is one `Manifest` refuses.
    pub fn write_tree(&self, out_dir: &Path) -> PathBuf {
        let layout = self.layout.unwrap_or_else(|| {
            self.targets
                .first()
                .map_or(BundleLayout::Posix, BundleLayout::preferred_for)
        });
        let root = out_dir.join(format!("{}.axt", self.name));
        let manifest = self.manifest().expect("a well-formed identity");
        let json = manifest.to_json().expect("a serialisable manifest");
        write_file(&root.join(layout.manifest_path()), json.as_bytes());

        if layout == BundleLayout::Apple {
            write_file(
                &root.join(layout.metadata_path()),
                info_plist(&manifest).as_bytes(),
            );
        }

        if self.with_binaries {
            for target in &self.targets {
                let dir = root.join(layout.binary_dir(target));
                write_file(&dir.join(binary_file_name(&self.name, target)), FAKE_BINARY);
            }
        }

        for (logical, bytes) in &self.resources {
            write_file(
                &root.join(layout.resource_dir("Resources")).join(logical),
                bytes,
            );
        }
        root
    }
}

/// [main-thread] A bundle whose `manifest.json` is not JSON at all.
///
/// The scanner must report it and carry on rather than abort the walk.
///
/// # Panics
///
/// If the fixture cannot be written.
pub fn corrupt(out_dir: &Path, name: &str) -> PathBuf {
    let root = out_dir.join(format!("{name}.axt"));
    write_file(
        &root.join("manifest.json"),
        b"{ \"format\": \"DAUx Audio Extension\", this is not json",
    );
    write_file(
        &root.join(format!("Content/{}/{name}.dll", TargetId::host())),
        FAKE_BINARY,
    );
    root
}

/// [main-thread] A bundle whose metadata is larger than
/// [`MAX_METADATA_BYTES`](daux_bundle::MAX_METADATA_BYTES).
///
/// The size must be refused from the directory entry, before a byte is read.
///
/// # Panics
///
/// If the fixture cannot be written.
pub fn oversized_metadata(out_dir: &Path, name: &str) -> PathBuf {
    let root = out_dir.join(format!("{name}.axt"));
    let mut json = String::with_capacity(limits::MAX_METADATA_BYTES as usize + 4_096);
    json.push_str("{\"format\":\"DAUx Audio Extension\",\"formatVersion\":1,\"padding\":\"");
    while json.len() < limits::MAX_METADATA_BYTES as usize + 1_024 {
        json.push('p');
    }
    json.push_str("\"}");
    write_file(&root.join("manifest.json"), json.as_bytes());
    root
}

/// [main-thread] A bundle carrying both `manifest.json` and `Contents/Info.plist`.
///
/// `axt-v1` §4 makes this ambiguous rather than "pick one", because the two files can
/// disagree and a reader that guesses is a reader two hosts will disagree with.
///
/// # Panics
///
/// If the fixture cannot be written.
pub fn ambiguous_layout(out_dir: &Path, name: &str) -> PathBuf {
    let root = out_dir.join(format!("{name}.axt"));
    let manifest = Manifest::new(
        &format!("studio.futureboard.tests.{}", name.to_ascii_lowercase()),
        name,
        "Futureboard Studio",
        "1.0.0",
    )
    .expect("a well-formed identity");
    write_file(
        &root.join("manifest.json"),
        manifest.to_json().expect("serialisable").as_bytes(),
    );
    write_file(
        &root.join("Contents/Info.plist"),
        info_plist(&manifest).as_bytes(),
    );
    root
}

/// [main-thread] A well-formed bundle that declares a target and ships no binary for it.
///
/// # Panics
///
/// If the fixture cannot be written.
pub fn missing_binary(out_dir: &Path, name: &str) -> PathBuf {
    Synthetic::new(name).without_binaries().write_tree(out_dir)
}

/// [main-thread] A bundle directory holding two candidate binaries for one target.
///
/// `manifest-v1` §4.3: a host must not have to guess which library to open.
///
/// # Panics
///
/// If the fixture cannot be written.
pub fn ambiguous_binary(out_dir: &Path, name: &str) -> PathBuf {
    let root = Synthetic::new(name).write_tree(out_dir);
    let target = TargetId::host();
    let dir = root.join(BundleLayout::Posix.binary_dir(&target));
    write_file(
        &dir.join(format!("Another.{}", target.dylib_extension())),
        FAKE_BINARY,
    );
    root
}

/// The file name a fixture's binary gets for `target`.
fn binary_file_name(name: &str, target: &TargetId) -> String {
    if target.is_apple() {
        name.to_owned()
    } else {
        format!("{name}.{}", target.dylib_extension())
    }
}

/// The minimal `Info.plist` an Apple-layout fixture needs.
fn info_plist(manifest: &Manifest) -> String {
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
	</dict>
</dict>
</plist>
"#,
        id = manifest.plugin.id,
        name = manifest.plugin.name,
        vendor = manifest.plugin.vendor,
        version = manifest.plugin.version,
        abi = manifest.abi_version,
    )
}

/// Writes `bytes` to `path`, creating parents. Panics: a fixture that cannot be written
/// leaves nothing to test.
fn write_file(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .unwrap_or_else(|e| panic!("cannot create {}: {e}", parent.display()));
    }
    std::fs::write(path, bytes).unwrap_or_else(|e| panic!("cannot write {}: {e}", path.display()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use daux_bundle::Bundle;

    #[test]
    fn the_builder_path_and_the_hand_written_path_agree() {
        let out = crate::TempTree::new("bundles-agree");
        let fixture = Synthetic::new("Agree");

        let built = fixture.write(&out.dir("built")).expect("writes");
        let handmade = fixture.write_tree(&out.dir("handmade"));

        let a = Bundle::open(&built).expect("opens");
        let b = Bundle::open(&handmade).expect("opens");
        assert_eq!(a.metadata().id, b.metadata().id);
        assert_eq!(a.metadata().targets, b.metadata().targets);
        assert_eq!(a.layout(), b.layout());
        assert!(a.binary_path(&TargetId::host()).is_ok());
        assert!(b.binary_path(&TargetId::host()).is_ok());
    }

    #[test]
    fn the_hostile_fixtures_really_are_hostile() {
        let out = crate::TempTree::new("bundles-hostile");

        assert!(Bundle::open(&corrupt(out.path(), "Corrupt")).is_err());
        assert!(Bundle::open(&oversized_metadata(out.path(), "Huge")).is_err());
        assert!(Bundle::open(&ambiguous_layout(out.path(), "Both")).is_err());

        // A missing binary is not an *open* failure — the manifest is fine — so it must
        // show up as a validation issue instead.
        let bundle = Bundle::open(&missing_binary(out.path(), "Hollow")).expect("opens");
        assert!(
            bundle.binary_path(&TargetId::host()).is_err(),
            "the fixture must ship no binary"
        );
    }
}
