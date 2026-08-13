//! One view of a bundle's identity, whichever file it came from.

use std::path::Path;

use crate::{
    BundleError, BundleErrorKind, BundleLayout, BundleResult, Category, MAX_METADATA_BYTES,
    Manifest, ManifestCaps, ManifestGraphics, TargetId, limits::FORMAT_VERSION,
};

/// The reverse-DNS key DAUx metadata lives under in an `Info.plist`.
const PLIST_KEY: &str = "DAUxPlugin";

/// The magic that starts a binary plist.
const BPLIST_MAGIC: &[u8] = b"bplist00";

/// What a host knows about a bundle before it loads any code.
///
/// Produced from `manifest.json` or from `Contents/Info.plist`, and identical either way —
/// nothing above this type needs to know which layout it is looking at. That is the whole
/// point: a scanner, a validator and the CLI all read this and stay layout-agnostic.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct BundleMetadata {
    /// Permanent reverse-DNS plug-in id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Vendor name.
    pub vendor: String,
    /// Product version as written, e.g. `"1.2.3"`.
    pub version: String,
    /// Long description. May be empty.
    pub description: String,
    /// What kind of plug-in the manifest says this is, when it says at all.
    ///
    /// `None` means the manifest declared no category — which is different from declaring
    /// `Category::Unknown`, and the difference matters: `manifest-v1` §8.1 row 5
    /// (`DAUX-M104`) compares this against the category the binary's descriptor reports, and
    /// a manifest that said nothing has nothing to disagree with.
    pub category: Option<Category>,
    /// Bundle format version.
    pub format_version: u32,
    /// ABI major version the binaries were built against.
    pub abi_version: u32,
    /// Lowest ABI minor version the binaries need.
    pub abi_version_minor: u32,
    /// Targets this bundle ships a binary for.
    pub targets: Vec<TargetId>,
    /// Coarse capability bits, for filtering before loading anything.
    pub capabilities: ManifestCaps,
    /// Editor hint. `None` means the plug-in is headless.
    pub graphics: Option<ManifestGraphics>,
    /// The resource directory's name at the bundle root.
    pub resource_dir_name: String,
    /// The dependency directory's name at the bundle root.
    pub library_dir_name: String,
}

impl BundleMetadata {
    /// [main-thread] Flattens a parsed manifest into the layout-independent view.
    pub fn from_manifest(manifest: &Manifest) -> Self {
        Self {
            id: manifest.plugin.id.clone(),
            name: manifest.plugin.name.clone(),
            vendor: manifest.plugin.vendor.clone(),
            version: manifest.plugin.version.clone(),
            description: manifest.plugin.description.clone(),
            category: manifest.plugin.category,
            format_version: manifest.format_version,
            abi_version: manifest.abi_version,
            abi_version_minor: manifest.abi_version_minor,
            targets: manifest.targets.clone(),
            capabilities: manifest.capabilities,
            graphics: manifest.graphics.clone(),
            resource_dir_name: manifest.resource_dir_name().to_owned(),
            library_dir_name: manifest.library_dir_name().to_owned(),
        }
    }

    /// [main-thread] Reads the metadata of the bundle rooted at `root`.
    ///
    /// For [`BundleLayout::Posix`] this is `manifest.json`. For [`BundleLayout::Apple`] it is
    /// `Contents/Resources/manifest.json` when present — a plist cannot express targets or
    /// capabilities faithfully — falling back to `Contents/Info.plist` when it is not.
    ///
    /// # Errors
    ///
    /// [`BundleErrorKind::TooLarge`] before reading anything oversized, plus whatever parsing
    /// reports. Hostile input is expected here: this is the first thing a scanner touches on
    /// a directory it did not create.
    pub fn read(root: &Path, layout: BundleLayout) -> BundleResult<Self> {
        let manifest_path = root.join(layout.manifest_path());
        if manifest_path.is_file() {
            let bytes = read_bounded(&manifest_path)?;
            let manifest =
                Manifest::from_json_bytes(&bytes).map_err(|e| e.or_path(&manifest_path))?;
            return Ok(Self::from_manifest(&manifest));
        }
        match layout {
            BundleLayout::Apple => Self::read_plist(&root.join(layout.metadata_path())),
            BundleLayout::Posix => Err(BundleError::new(
                BundleErrorKind::NotABundle,
                "manifest.json is missing",
            )
            .with_path(manifest_path)),
        }
    }

    /// [main-thread] Reads an `Info.plist` that carries a `DAUxPlugin` dictionary.
    ///
    /// A plist alone is a degraded source: Apple's own keys give identity but nothing about
    /// targets, ABI or capabilities, so those come from the nested `DAUxPlugin` dictionary
    /// and default conservatively when it is absent.
    ///
    /// # Errors
    ///
    /// [`BundleErrorKind::Parse`] for a malformed plist and
    /// [`BundleErrorKind::MissingField`] when it carries no usable identity.
    pub fn read_plist(path: &Path) -> BundleResult<Self> {
        let bytes = read_bounded(path)?;
        // A binary plist is length-prefixed and the `plist` crate bounds it itself. An XML
        // one is a text format with no inherent bounds, so it goes through the same
        // depth-and-size prescan `manifest.json` does before a real parser ever sees it.
        if !bytes.starts_with(BPLIST_MAGIC) {
            let text = crate::read::decode_utf8(&bytes).map_err(|e| e.or_path(path))?;
            crate::xml_scan::prescan(text).map_err(|e| e.or_path(path))?;
        }

        let value: plist::Value = plist::from_bytes(&bytes)
            .map_err(|e| BundleError::new(BundleErrorKind::Parse, e.to_string()).with_path(path))?;
        let root = value.as_dictionary().ok_or_else(|| {
            BundleError::new(BundleErrorKind::Parse, "the plist root is not a dictionary")
                .with_path(path)
        })?;

        let daux = root.get(PLIST_KEY).and_then(plist::Value::as_dictionary);
        let string = |dict: Option<&plist::Dictionary>, key: &str| -> Option<String> {
            dict?.get(key)?.as_string().map(str::to_owned)
        };
        let integer = |dict: Option<&plist::Dictionary>, key: &str| -> Option<u32> {
            u32::try_from(dict?.get(key)?.as_unsigned_integer()?).ok()
        };

        // Identity: prefer the DAUx keys, fall back to Apple's own.
        let id = string(daux, "id")
            .or_else(|| string(Some(root), "CFBundleIdentifier"))
            .ok_or_else(|| {
                BundleError::new(
                    BundleErrorKind::MissingField,
                    "neither DAUxPlugin.id nor CFBundleIdentifier is present",
                )
                .with_path(path)
            })?;
        let name = string(daux, "name")
            .or_else(|| string(Some(root), "CFBundleName"))
            .unwrap_or_else(|| id.clone());
        let version = string(daux, "version")
            .or_else(|| string(Some(root), "CFBundleShortVersionString"))
            .unwrap_or_else(|| "0.0.0".to_owned());

        let targets = daux
            .and_then(|d| d.get("targets"))
            .and_then(plist::Value::as_array)
            .map(|array| {
                array
                    .iter()
                    .filter_map(plist::Value::as_string)
                    .filter_map(|s| TargetId::parse(s).ok())
                    .collect::<Vec<_>>()
            })
            // With nothing declared, an Apple bundle ships a universal macOS binary; that is
            // the only thing an `Info.plist` can be describing.
            .filter(|t: &Vec<TargetId>| !t.is_empty())
            .unwrap_or_else(|| vec![TargetId::parse(crate::target::MACOS_UNIVERSAL)
                .expect("a built-in target id parses")]);

        let capabilities = daux
            .and_then(|d| d.get("capabilities"))
            .and_then(plist::Value::as_unsigned_integer)
            .map_or_else(ManifestCaps::empty, ManifestCaps::from_bits);

        Ok(Self {
            id,
            name,
            vendor: string(daux, "vendor").unwrap_or_default(),
            version,
            description: string(daux, "description").unwrap_or_default(),
            // `manifest-v1` §6.2 spells it `DAUxCategory` at the plist root; the nested
            // `DAUxPlugin` dictionary carries a lower-case `category` alongside the other
            // DAUx keys. An unrecognised slug becomes `None` rather than `Unknown`: a
            // category this build cannot name is not the same as a bundle that declared none,
            // and cross-checking against a guess would raise a false `DAUX-M104`.
            category: string(daux, "category")
                .or_else(|| string(Some(root), "DAUxCategory"))
                .as_deref()
                .and_then(Category::parse),
            format_version: integer(daux, "formatVersion").unwrap_or(FORMAT_VERSION),
            abi_version: integer(daux, "abiVersion").unwrap_or(daux_abi::DAUX_ABI_VERSION_MAJOR),
            abi_version_minor: integer(daux, "abiVersionMinor").unwrap_or(0),
            targets,
            capabilities,
            graphics: None,
            resource_dir_name: "Resources".to_owned(),
            library_dir_name: "Library".to_owned(),
        })
    }

    /// [main-thread] `true` when this bundle ships a binary for `target`.
    pub fn supports(&self, target: &TargetId) -> bool {
        self.targets.iter().any(|t| t == target)
    }

    /// [main-thread] `true` when a host speaking ABI `(major, minor)` can load these binaries.
    pub fn loadable_over_abi(&self, major: u32, minor: u32) -> bool {
        major == self.abi_version && minor >= self.abi_version_minor
    }

    /// [main-thread] `true` when the plug-in declares an editor.
    pub fn has_editor(&self) -> bool {
        self.graphics.is_some()
    }
}

/// Reads a metadata file, refusing to allocate for an oversized one.
///
/// The size is checked from the directory entry *before* the read, so a hostile 4 GiB
/// `manifest.json` costs a `stat` rather than 4 GiB of memory.
fn read_bounded(path: &Path) -> BundleResult<Vec<u8>> {
    let meta = std::fs::metadata(path).map_err(|e| BundleError::io(path, &e))?;
    if !meta.is_file() {
        return Err(
            BundleError::new(BundleErrorKind::NotRegularFile, "metadata is not a file")
                .with_path(path),
        );
    }
    if meta.len() > MAX_METADATA_BYTES {
        return Err(BundleError::new(
            BundleErrorKind::TooLarge,
            format!(
                "metadata is {} bytes, over the {MAX_METADATA_BYTES}-byte limit",
                meta.len()
            ),
        )
        .with_path(path));
    }
    std::fs::read(path).map_err(|e| BundleError::io(path, &e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::tempdir;

    fn manifest_json() -> String {
        let mut m = Manifest::new("com.example.gain", "Gain", "Example Audio", "1.2.3")
            .expect("a well-formed identity");
        m.targets = vec![TargetId::parse("windows-x86_64").unwrap()];
        m.plugin.description = "A gain plug-in.".to_owned();
        m.to_json().expect("serialisable")
    }

    #[test]
    fn a_posix_bundle_reads_its_manifest() {
        let dir = tempdir();
        dir.write("manifest.json", &manifest_json());

        let meta = BundleMetadata::read(dir.path(), BundleLayout::Posix).unwrap();
        assert_eq!(meta.id, "com.example.gain");
        assert_eq!(meta.name, "Gain");
        assert_eq!(meta.vendor, "Example Audio");
        assert_eq!(meta.version, "1.2.3");
        assert_eq!(meta.description, "A gain plug-in.");
        assert!(meta.supports(&TargetId::parse("windows-x86_64").unwrap()));
        assert!(!meta.supports(&TargetId::parse("linux-x86_64").unwrap()));
        assert!(!meta.has_editor());
        assert_eq!(meta.resource_dir_name, "Resources");
    }

    #[test]
    fn a_posix_bundle_without_a_manifest_is_not_a_bundle() {
        let dir = tempdir();
        let err = BundleMetadata::read(dir.path(), BundleLayout::Posix).unwrap_err();
        assert_eq!(*err.kind(), BundleErrorKind::NotABundle);
    }

    #[test]
    fn an_apple_bundle_prefers_its_nested_manifest_over_the_plist() {
        let dir = tempdir();
        dir.write("Contents/Resources/manifest.json", &manifest_json());
        dir.write(
            "Contents/Info.plist",
            r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>com.example.stale</string>
</dict></plist>"#,
        );

        let meta = BundleMetadata::read(dir.path(), BundleLayout::Apple).unwrap();
        // The manifest wins: the plist cannot express targets or capabilities.
        assert_eq!(meta.id, "com.example.gain");
        assert_eq!(meta.targets.len(), 1);
    }

    #[test]
    fn an_apple_bundle_falls_back_to_the_plist() {
        let dir = tempdir();
        dir.write(
            "Contents/Info.plist",
            r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>com.example.verb</string>
  <key>CFBundleName</key><string>Verb</string>
  <key>CFBundleShortVersionString</key><string>2.0.1</string>
  <key>DAUxPlugin</key><dict>
    <key>vendor</key><string>Example Audio</string>
    <key>abiVersion</key><integer>1</integer>
  </dict>
</dict></plist>"#,
        );

        let meta = BundleMetadata::read(dir.path(), BundleLayout::Apple).unwrap();
        assert_eq!(meta.id, "com.example.verb");
        assert_eq!(meta.name, "Verb");
        assert_eq!(meta.version, "2.0.1");
        assert_eq!(meta.vendor, "Example Audio");
        assert_eq!(meta.abi_version, 1);
        // Nothing declared: an Info.plist can only be describing a macOS binary.
        assert_eq!(meta.targets.len(), 1);
        assert!(meta.targets[0].is_apple());
    }

    #[test]
    fn the_daux_keys_win_over_apples_own() {
        let dir = tempdir();
        dir.write(
            "Contents/Info.plist",
            r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>com.example.wrapper</string>
  <key>CFBundleName</key><string>Wrapper</string>
  <key>DAUxPlugin</key><dict>
    <key>id</key><string>com.example.real</string>
    <key>name</key><string>Real</string>
    <key>targets</key><array><string>macos-arm64</string></array>
  </dict>
</dict></plist>"#,
        );

        let meta = BundleMetadata::read(dir.path(), BundleLayout::Apple).unwrap();
        assert_eq!(meta.id, "com.example.real");
        assert_eq!(meta.name, "Real");
        assert_eq!(meta.targets[0].as_str(), "macos-arm64");
    }

    #[test]
    fn a_plist_with_no_identity_at_all_is_rejected() {
        let dir = tempdir();
        dir.write(
            "Contents/Info.plist",
            r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>Unrelated</key><string>x</string></dict></plist>"#,
        );
        let err = BundleMetadata::read(dir.path(), BundleLayout::Apple).unwrap_err();
        assert_eq!(*err.kind(), BundleErrorKind::MissingField);
    }

    #[test]
    fn a_malformed_plist_reports_a_parse_error_rather_than_panicking() {
        let dir = tempdir();
        dir.write("Contents/Info.plist", "this is not a plist at all");
        let err = BundleMetadata::read(dir.path(), BundleLayout::Apple).unwrap_err();
        assert_eq!(*err.kind(), BundleErrorKind::Parse);
    }

    #[test]
    fn oversized_metadata_is_refused_before_it_is_read() {
        let dir = tempdir();
        // One byte over the cap is enough; the check is on the directory entry, not content.
        let huge = "x".repeat(usize::try_from(MAX_METADATA_BYTES).unwrap() + 1);
        dir.write("manifest.json", &huge);

        let err = BundleMetadata::read(dir.path(), BundleLayout::Posix).unwrap_err();
        assert_eq!(*err.kind(), BundleErrorKind::TooLarge);
    }

    #[test]
    fn abi_loadability_needs_the_same_major_and_at_least_the_minor() {
        let dir = tempdir();
        dir.write("manifest.json", &manifest_json());
        let mut meta = BundleMetadata::read(dir.path(), BundleLayout::Posix).unwrap();
        meta.abi_version = 1;
        meta.abi_version_minor = 3;

        assert!(meta.loadable_over_abi(1, 3));
        assert!(meta.loadable_over_abi(1, 9));
        assert!(!meta.loadable_over_abi(1, 2));
        assert!(!meta.loadable_over_abi(2, 3));
    }
}
