//! The artefact formats a scan can find on disk.

use std::fmt;
use std::path::Path;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// A plug-in artefact format. [any-thread]
///
/// The three are not equally knowable from outside. An `.axt` carries a manifest this
/// workspace can read without executing anything, so a scan produces a full
/// [`ScanEntry`](crate::ScanEntry) for one. A `.vst3` or a `.clap` carries its identity
/// only inside the binary, behind that format's own C ABI, and reading it means loading and
/// calling foreign code — which the host side of this workspace does not do in v1. Those are
/// therefore *found* and reported as [`ForeignPlugin`](crate::ForeignPlugin)s, not described.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginFormat {
    /// DAUx Audio Extension — always a directory, always carries `manifest.json` or
    /// `Contents/Info.plist`.
    Axt,
    /// Steinberg VST3 — a directory bundle on macOS and Linux, and either a bare `.vst3`
    /// DLL or a directory bundle on Windows.
    Vst3,
    /// CLAP — a bare shared library on Windows and Linux, a directory bundle on macOS.
    Clap,
}

impl PluginFormat {
    /// Every format a scan looks for, in the order a report lists them. [any-thread]
    pub const ALL: [Self; 3] = [Self::Axt, Self::Vst3, Self::Clap];

    /// The stable lower-case name used by `daux scan --format` and in the cache file.
    /// [any-thread]
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Axt => "axt",
            Self::Vst3 => "vst3",
            Self::Clap => "clap",
        }
    }

    /// The file-name extension, without the leading dot. [any-thread]
    #[must_use]
    pub const fn extension(self) -> &'static str {
        // Identical to `as_str` today, and deliberately a separate function: the name a user
        // types and the extension on disk are different concepts that happen to coincide.
        self.as_str()
    }

    /// The format an extension names, ASCII case-insensitively. [any-thread]
    #[must_use]
    pub fn from_extension(extension: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|format| extension.eq_ignore_ascii_case(format.extension()))
    }

    /// The format `path` names by its extension, if any. [any-thread]
    ///
    /// This is a name test, not a content test: it says what the path claims to be, and a
    /// scan still has to open it to find out whether the claim is true.
    #[must_use]
    pub fn from_path(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?;
        Self::from_extension(extension)
    }

    /// Whether an artefact of this format is always a directory. [any-thread]
    ///
    /// Only `.axt` is: `axt-v1` §1 makes a bundle a directory and never an archive. The
    /// other two are directories on some platforms and plain files on others, so a scan
    /// must accept either.
    #[must_use]
    pub const fn is_always_a_directory(self) -> bool {
        matches!(self, Self::Axt)
    }

    /// Whether this workspace can describe the artefact without executing its code.
    /// [any-thread]
    #[must_use]
    pub const fn has_readable_metadata(self) -> bool {
        matches!(self, Self::Axt)
    }
}

impl fmt::Display for PluginFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Returned by [`PluginFormat::from_str`] for a name no format answers to. [any-thread]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnknownFormat;

impl fmt::Display for UnknownFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("expected one of `axt`, `vst3` or `clap`")
    }
}

impl std::error::Error for UnknownFormat {}

impl FromStr for PluginFormat {
    type Err = UnknownFormat;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_extension(s).ok_or(UnknownFormat)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn names_and_extensions_round_trip_case_insensitively() {
        for format in PluginFormat::ALL {
            assert_eq!(PluginFormat::from_extension(format.as_str()), Some(format));
            assert_eq!(
                PluginFormat::from_extension(&format.as_str().to_uppercase()),
                Some(format),
                "Windows preserves the case a user typed; `Gain.AXT` is the same bundle"
            );
            assert_eq!(format.to_string(), format.as_str());
            assert_eq!(format.as_str().parse::<PluginFormat>(), Ok(format));
        }
        assert_eq!(PluginFormat::from_extension("dll"), None);
        assert_eq!(PluginFormat::from_extension(""), None);
        assert_eq!(PluginFormat::from_extension("axt3"), None);
        assert_eq!("vst".parse::<PluginFormat>(), Err(UnknownFormat));
    }

    #[test]
    fn a_path_is_classified_by_its_extension_only() {
        assert_eq!(
            PluginFormat::from_path(&PathBuf::from("C:/plugins/Gain.axt")),
            Some(PluginFormat::Axt)
        );
        assert_eq!(
            PluginFormat::from_path(&PathBuf::from("/usr/lib/clap/reverb.clap")),
            Some(PluginFormat::Clap)
        );
        // A directory whose *name* contains a format is not that format.
        assert_eq!(
            PluginFormat::from_path(&PathBuf::from("/usr/lib/vst3")),
            None
        );
        assert_eq!(PluginFormat::from_path(&PathBuf::from("Gain")), None);
        // A dotfile has no extension, whatever it looks like.
        assert_eq!(PluginFormat::from_path(&PathBuf::from(".axt")), None);
    }

    /// The cache file stores the format by name, so the encoding is part of the on-disk
    /// format and must not drift.
    #[test]
    fn the_serialised_form_is_the_lower_case_name() {
        for format in PluginFormat::ALL {
            let json = serde_json::to_string(&format).expect("a plain enum serialises");
            assert_eq!(json, format!("\"{}\"", format.as_str()));
            let back: PluginFormat = serde_json::from_str(&json).expect("and parses back");
            assert_eq!(back, format);
        }
        assert!(serde_json::from_str::<PluginFormat>("\"AXT\"").is_err());
    }

    #[test]
    fn only_axt_is_describable_without_running_code() {
        assert!(PluginFormat::Axt.has_readable_metadata());
        assert!(PluginFormat::Axt.is_always_a_directory());
        for format in [PluginFormat::Vst3, PluginFormat::Clap] {
            assert!(!format.has_readable_metadata());
            assert!(!format.is_always_a_directory());
        }
    }
}
