//! The two on-disk shapes an `.axt` bundle can take.

use core::fmt;
use core::str::FromStr;
use std::path::Path;

use crate::{BundleError, BundleErrorKind, BundleResult, TargetId};

/// How a bundle's directories are arranged.
///
/// Two layouts exist because Apple's frameworks and code-signing tools only understand one
/// shape, and everything else is better served by a flatter one. A bundle is one or the
/// other, never both: a directory containing both `manifest.json` and `Contents/Info.plist`
/// is rejected as [`BundleErrorKind::AmbiguousLayout`] rather than guessed at.
///
/// ```text
/// Posix                          Apple
/// Gain.axt/                      Gain.axt/
///   manifest.json                  Contents/
///   Content/                         Info.plist
///     windows-x86_64/                MacOS/
///       Gain.dll                       Gain
///   Library/                         Frameworks/
///     windows-x86_64/                Resources/
///       shared.dll                     manifest.json
///   Resources/
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum BundleLayout {
    /// The portable layout: `manifest.json` at the root, binaries under `Content/{target}/`.
    #[default]
    Posix,
    /// The Apple layout: `Contents/{Info.plist,MacOS,Frameworks,Resources}`.
    Apple,
}

impl BundleLayout {
    /// [any-thread] The layout Apple's tooling expects for `target`.
    ///
    /// macOS bundles must be [`BundleLayout::Apple`] for `codesign` and `spctl` to accept
    /// them; everything else uses [`BundleLayout::Posix`].
    pub fn preferred_for(target: &TargetId) -> Self {
        if target.is_apple() {
            BundleLayout::Apple
        } else {
            BundleLayout::Posix
        }
    }

    /// [any-thread] The layout for the machine this code is running on.
    pub fn host() -> Self {
        Self::preferred_for(&TargetId::host())
    }

    /// [any-thread] The stable lower-case name used on the CLI and in diagnostics.
    pub const fn as_str(self) -> &'static str {
        match self {
            BundleLayout::Posix => "posix",
            BundleLayout::Apple => "apple",
        }
    }

    /// [any-thread] The metadata file that identifies a bundle of this layout, relative to
    /// the bundle root.
    pub const fn metadata_path(self) -> &'static str {
        match self {
            BundleLayout::Posix => "manifest.json",
            BundleLayout::Apple => "Contents/Info.plist",
        }
    }

    /// [any-thread] Where a full `manifest.json` lives, relative to the bundle root.
    ///
    /// An Apple bundle carries both: `Info.plist` for the platform's tooling and a complete
    /// `manifest.json` in `Contents/Resources/` for everything DAUx needs that a plist cannot
    /// express (`axt-v1` §5).
    pub const fn manifest_path(self) -> &'static str {
        match self {
            BundleLayout::Posix => "manifest.json",
            BundleLayout::Apple => "Contents/Resources/manifest.json",
        }
    }

    /// [any-thread] The directory holding the binary for `target`, relative to the root.
    ///
    /// The Apple layout has one binary directory rather than one per target, because a macOS
    /// binary is normally a universal one covering every architecture at once.
    pub fn binary_dir(self, target: &TargetId) -> String {
        match self {
            BundleLayout::Posix => format!("Content/{}", target.as_str()),
            BundleLayout::Apple => "Contents/MacOS".to_owned(),
        }
    }

    /// [any-thread] The directory holding bundled dependencies for `target`.
    ///
    /// `library_dir_name` comes from the manifest's `resources.libraryDir`, and is ignored by
    /// the Apple layout, whose directory name is fixed by the platform.
    pub fn library_dir(self, target: &TargetId, library_dir_name: &str) -> String {
        match self {
            BundleLayout::Posix => format!("{library_dir_name}/{}", target.as_str()),
            BundleLayout::Apple => "Contents/Frameworks".to_owned(),
        }
    }

    /// [any-thread] The resource directory, relative to the root.
    ///
    /// `resource_dir_name` comes from the manifest's `resources.dir`, and is ignored by the
    /// Apple layout for the same reason as above.
    pub fn resource_dir(self, resource_dir_name: &str) -> String {
        match self {
            BundleLayout::Posix => resource_dir_name.to_owned(),
            BundleLayout::Apple => "Contents/Resources".to_owned(),
        }
    }

    /// [main-thread] Detects the layout of the bundle rooted at `root`.
    ///
    /// # Errors
    ///
    /// [`BundleErrorKind::AmbiguousLayout`] when both metadata files are present, and
    /// [`BundleErrorKind::NotABundle`] when neither is. Guessing in either case would mean
    /// loading a binary chosen by a file that is not the one describing it.
    pub fn detect(root: &Path) -> BundleResult<Self> {
        let posix = root.join("manifest.json").is_file();
        let apple = root.join("Contents").join("Info.plist").is_file();
        match (posix, apple) {
            (true, false) => Ok(BundleLayout::Posix),
            (false, true) => Ok(BundleLayout::Apple),
            (true, true) => Err(BundleError::new(
                BundleErrorKind::AmbiguousLayout,
                "the directory has both manifest.json and Contents/Info.plist",
            )
            .with_path(root)),
            (false, false) => Err(BundleError::new(
                BundleErrorKind::NotABundle,
                "the directory has neither manifest.json nor Contents/Info.plist",
            )
            .with_path(root)),
        }
    }
}

impl fmt::Display for BundleLayout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for BundleLayout {
    type Err = BundleError;

    fn from_str(s: &str) -> BundleResult<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "posix" | "portable" => Ok(BundleLayout::Posix),
            "apple" | "macos" | "darwin" => Ok(BundleLayout::Apple),
            other => Err(BundleError::new(
                BundleErrorKind::InvalidField,
                format!("unknown bundle layout `{other}`"),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::tempdir;

    fn target(s: &str) -> TargetId {
        TargetId::parse(s).expect("a known target id")
    }

    #[test]
    fn names_round_trip() {
        for l in [BundleLayout::Posix, BundleLayout::Apple] {
            assert_eq!(l.as_str().parse::<BundleLayout>().unwrap(), l);
        }
        assert_eq!(
            "MacOS".parse::<BundleLayout>().unwrap(),
            BundleLayout::Apple
        );
        assert!("sideways".parse::<BundleLayout>().is_err());
    }

    #[test]
    fn apple_targets_prefer_the_apple_layout() {
        assert_eq!(
            BundleLayout::preferred_for(&target("macos-universal")),
            BundleLayout::Apple
        );
        assert_eq!(
            BundleLayout::preferred_for(&target("windows-x86_64")),
            BundleLayout::Posix
        );
        assert_eq!(
            BundleLayout::preferred_for(&target("linux-aarch64")),
            BundleLayout::Posix
        );
    }

    #[test]
    fn the_posix_layout_puts_a_directory_per_target() {
        let l = BundleLayout::Posix;
        assert_eq!(
            l.binary_dir(&target("windows-x86_64")),
            "Content/windows-x86_64"
        );
        assert_eq!(
            l.library_dir(&target("linux-x86_64"), "Library"),
            "Library/linux-x86_64"
        );
        assert_eq!(l.resource_dir("Resources"), "Resources");
        assert_eq!(l.metadata_path(), "manifest.json");
        assert_eq!(l.manifest_path(), "manifest.json");
    }

    #[test]
    fn the_apple_layout_ignores_target_and_manifest_directory_names() {
        let l = BundleLayout::Apple;
        assert_eq!(l.binary_dir(&target("macos-universal")), "Contents/MacOS");
        assert_eq!(
            l.library_dir(&target("macos-arm64"), "Whatever"),
            "Contents/Frameworks"
        );
        assert_eq!(l.resource_dir("Whatever"), "Contents/Resources");
        assert_eq!(l.metadata_path(), "Contents/Info.plist");
        assert_eq!(l.manifest_path(), "Contents/Resources/manifest.json");
    }

    #[test]
    fn detection_reads_the_filesystem_and_refuses_to_guess() {
        let dir = tempdir();
        let root = dir.path();

        // Neither file: not a bundle at all.
        let err = BundleLayout::detect(root).unwrap_err();
        assert_eq!(*err.kind(), BundleErrorKind::NotABundle);

        std::fs::write(root.join("manifest.json"), "{}").unwrap();
        assert_eq!(BundleLayout::detect(root).unwrap(), BundleLayout::Posix);

        std::fs::create_dir_all(root.join("Contents")).unwrap();
        std::fs::write(root.join("Contents").join("Info.plist"), "<plist/>").unwrap();
        let err = BundleLayout::detect(root).unwrap_err();
        assert_eq!(*err.kind(), BundleErrorKind::AmbiguousLayout);

        std::fs::remove_file(root.join("manifest.json")).unwrap();
        assert_eq!(BundleLayout::detect(root).unwrap(), BundleLayout::Apple);
    }
}
