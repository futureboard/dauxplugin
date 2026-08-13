//! Target identifiers: the only vocabulary that appears in bundle paths and in `targets`.
//!
//! See `axt-v1` §3 and `manifest-v1` §3.7.

use std::fmt;
use std::str::FromStr;

use serde::de::{Error as DeError, Unexpected};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{BundleError, BundleErrorKind, BundleResult};
use crate::limits::MAX_TARGET_ID_BYTES;

/// `windows-x86_64` — PE32+, AMD64.
pub const WINDOWS_X86_64: &str = "windows-x86_64";
/// `windows-aarch64` — PE32+, ARM64.
pub const WINDOWS_AARCH64: &str = "windows-aarch64";
/// `linux-x86_64` — ELF64, `EM_X86_64`.
pub const LINUX_X86_64: &str = "linux-x86_64";
/// `linux-aarch64` — ELF64, `EM_AARCH64`.
pub const LINUX_AARCH64: &str = "linux-aarch64";
/// `macos-x86_64` — thin Mach-O, `CPU_TYPE_X86_64`.
pub const MACOS_X86_64: &str = "macos-x86_64";
/// `macos-arm64` — thin Mach-O, `CPU_TYPE_ARM64`.
///
/// Spelled the way Apple's own tools spell it (`axt-v1` §3.1); `macos-aarch64` is accepted
/// on input as an alias and normalised to this form.
pub const MACOS_ARM64: &str = "macos-arm64";
/// `macos-universal` — one fat Mach-O carrying both macOS architectures.
pub const MACOS_UNIVERSAL: &str = "macos-universal";

/// Alias accepted on input for [`MACOS_ARM64`], as spelled by `manifest-v1` §3.7.
const MACOS_AARCH64_ALIAS: &str = "macos-aarch64";

/// The closed v1 registry, in the order `axt-v1` §3 lists it.
const REGISTRY: [&str; 7] = [
    WINDOWS_X86_64,
    WINDOWS_AARCH64,
    LINUX_X86_64,
    LINUX_AARCH64,
    MACOS_X86_64,
    MACOS_ARM64,
    MACOS_UNIVERSAL,
];

const WINDOWS_X86_64_TRIPLES: &[&str] = &[
    "x86_64-pc-windows-msvc",
    "x86_64-pc-windows-gnu",
    "x86_64-pc-windows-gnullvm",
];
const WINDOWS_AARCH64_TRIPLES: &[&str] = &["aarch64-pc-windows-msvc", "aarch64-pc-windows-gnullvm"];
const LINUX_X86_64_TRIPLES: &[&str] = &["x86_64-unknown-linux-gnu", "x86_64-unknown-linux-musl"];
const LINUX_AARCH64_TRIPLES: &[&str] = &["aarch64-unknown-linux-gnu", "aarch64-unknown-linux-musl"];
const MACOS_X86_64_TRIPLES: &[&str] = &["x86_64-apple-darwin"];
const MACOS_ARM64_TRIPLES: &[&str] = &["aarch64-apple-darwin"];
const MACOS_UNIVERSAL_TRIPLES: &[&str] = &["x86_64-apple-darwin", "aarch64-apple-darwin"];
const NO_TRIPLES: &[&str] = &[];

/// One binary slice inside a bundle, e.g. `windows-x86_64`. [any-thread]
///
/// The value is always well-formed `{os}-{arch}`: lower-case ASCII, 1..=32 bytes, exactly
/// two non-empty segments. Well-formedness and *registration* are separate questions —
/// [`TargetId::is_known`] answers the second one:
///
/// * a **malformed** id (`Windows-X86_64`, `a-b-c`, 40 bytes) is rejected outright, because
///   it can never name a directory this format defines (`manifest-v1` §3.7, `DAUX-M013`);
/// * a **well-formed but unregistered** id (a future `linux-riscv64`) parses, so that a v1
///   reader tolerates a bundle built by a newer SDK; it simply has no binary it can load
///   for that entry, and `daux validate` reports it as `axt.target.unknown`.
///
/// `macos-aarch64` is accepted on input and normalised to [`MACOS_ARM64`], so that the two
/// spellings used by `axt-v1` §3.1 and `manifest-v1` §3.7 never denote different targets.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TargetId(String);

impl TargetId {
    /// [main-thread] Parses and canonicalises a target identifier.
    ///
    /// # Errors
    ///
    /// [`BundleErrorKind::InvalidTarget`] when `s` is empty, longer than 32 bytes, not
    /// exactly two `-`-separated segments, or contains anything but lower-case ASCII
    /// letters, digits and `_`.
    pub fn parse(s: &str) -> BundleResult<Self> {
        validate_syntax(s)?;
        if s == MACOS_AARCH64_ALIAS {
            return Ok(Self(MACOS_ARM64.to_owned()));
        }
        Ok(Self(s.to_owned()))
    }

    /// [any-thread] The identifier as written in the manifest.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// [any-thread] The operating-system segment, e.g. `windows`.
    #[must_use]
    pub fn os(&self) -> &str {
        self.0.split_once('-').map_or(self.0.as_str(), |(os, _)| os)
    }

    /// [any-thread] The architecture segment, e.g. `x86_64` or `universal`.
    #[must_use]
    pub fn arch(&self) -> &str {
        self.0.split_once('-').map_or("", |(_, arch)| arch)
    }

    /// [any-thread] Whether this id is in the closed v1 registry of `axt-v1` §3.
    #[must_use]
    pub fn is_known(&self) -> bool {
        REGISTRY.contains(&self.0.as_str())
    }

    /// [any-thread] Whether this target uses the Apple bundle layout.
    ///
    /// `macos-*` targets never appear in a POSIX-layout bundle and `windows-*`/`linux-*`
    /// targets never appear in an Apple-layout one (`axt-v1` §4).
    #[must_use]
    pub fn is_apple(&self) -> bool {
        self.os() == "macos"
    }

    /// [any-thread] Whether this target names a fat Mach-O rather than a thin one.
    #[must_use]
    pub fn is_universal(&self) -> bool {
        self.0 == MACOS_UNIVERSAL
    }

    /// [any-thread] File-name extension of the plug-in binary for this target family,
    /// without the leading dot.
    ///
    /// `"dll"` for `windows-*`, `"so"` for `linux-*` and `""` for `macos-*` and for
    /// unregistered targets, exactly as `axt-v1` §3.2 specifies: the macOS binary lives at
    /// `Contents/MacOS/<BundleName>` with no extension at all.
    #[must_use]
    pub fn dylib_extension(&self) -> &'static str {
        match self.os() {
            "windows" => "dll",
            "linux" => "so",
            _ => "",
        }
    }

    /// [main-thread] Maps a Rust target triple onto a target id.
    ///
    /// Returns `None` for a triple no v1 target covers. The libc flavour is deliberately
    /// not part of the identity: `x86_64-unknown-linux-gnu` and `-musl` both map to
    /// `linux-x86_64` (`axt-v1` §3.1).
    #[must_use]
    pub fn from_rust_triple(triple: &str) -> Option<Self> {
        let id = if WINDOWS_X86_64_TRIPLES.contains(&triple) {
            WINDOWS_X86_64
        } else if WINDOWS_AARCH64_TRIPLES.contains(&triple) {
            WINDOWS_AARCH64
        } else if LINUX_X86_64_TRIPLES.contains(&triple) {
            LINUX_X86_64
        } else if LINUX_AARCH64_TRIPLES.contains(&triple) {
            LINUX_AARCH64
        } else if MACOS_X86_64_TRIPLES.contains(&triple) {
            MACOS_X86_64
        } else if MACOS_ARM64_TRIPLES.contains(&triple) {
            MACOS_ARM64
        } else {
            return None;
        };
        Some(Self(id.to_owned()))
    }

    /// [any-thread] Every Rust target triple that produces a binary for this target.
    ///
    /// `macos-universal` lists both Apple triples because a fat binary is built from both
    /// and merged with `lipo`. An unregistered target has no triples.
    #[must_use]
    pub fn to_rust_triples(&self) -> &'static [&'static str] {
        match self.0.as_str() {
            WINDOWS_X86_64 => WINDOWS_X86_64_TRIPLES,
            WINDOWS_AARCH64 => WINDOWS_AARCH64_TRIPLES,
            LINUX_X86_64 => LINUX_X86_64_TRIPLES,
            LINUX_AARCH64 => LINUX_AARCH64_TRIPLES,
            MACOS_X86_64 => MACOS_X86_64_TRIPLES,
            MACOS_ARM64 => MACOS_ARM64_TRIPLES,
            MACOS_UNIVERSAL => MACOS_UNIVERSAL_TRIPLES,
            _ => NO_TRIPLES,
        }
    }

    /// [main-thread] The target of the **running process**, not of the machine.
    ///
    /// An x86_64 host emulated on an arm64 machine reports `…-x86_64`, because that is the
    /// only slice it can actually load (`axt-v1` §9 rule 2). Platforms outside the v1
    /// registry produce a well-formed but unregistered id rather than a wrong one.
    #[must_use]
    pub fn host() -> Self {
        Self(host_id())
    }

    /// [main-thread] Targets the running process can load, in selection order.
    ///
    /// On macOS `macos-universal` is tried first and the exact architecture second, as
    /// `axt-v1` §9 rule 2 requires; on other platforms the list has exactly one entry.
    #[must_use]
    pub fn host_candidates() -> Vec<Self> {
        let host = Self::host();
        if host.is_apple() {
            vec![Self(MACOS_UNIVERSAL.to_owned()), host]
        } else {
            vec![host]
        }
    }

    /// [any-thread] Every identifier in the closed v1 registry, in specification order.
    #[must_use]
    pub fn registry() -> Vec<Self> {
        REGISTRY.iter().map(|id| Self((*id).to_owned())).collect()
    }
}

fn host_id() -> String {
    // `axt-v1` §3.1 spells the three platforms exactly as `std::env::consts::OS` does, so
    // this is a pass-through rather than a translation table. Anything else falls through to
    // the syntax check below.
    let os = std::env::consts::OS;
    let arch = match (os, std::env::consts::ARCH) {
        // Apple spells it `arm64`; ELF and Rust spell it `aarch64` (`axt-v1` §3.1).
        ("macos", "aarch64") => "arm64",
        (_, arch) => arch,
    };
    let candidate = format!("{os}-{arch}");
    if validate_syntax(&candidate).is_ok() {
        candidate
    } else {
        // `std::env::consts` is a fixed table of lower-case ASCII names, so this is
        // unreachable in practice; producing a syntactically valid placeholder keeps the
        // function total instead of panicking on a hypothetical future platform string.
        "unknown-unknown".to_owned()
    }
}

fn validate_syntax(s: &str) -> BundleResult<()> {
    let invalid = |detail: String| BundleError::new(BundleErrorKind::InvalidTarget, detail);

    if s.is_empty() {
        return Err(invalid("empty target id".to_owned()));
    }
    if s.len() > MAX_TARGET_ID_BYTES {
        return Err(invalid(format!(
            "target id is {} bytes, limit is {MAX_TARGET_ID_BYTES}",
            s.len()
        )));
    }
    let Some((os, arch)) = s.split_once('-') else {
        return Err(invalid(format!("`{s}` is not `{{os}}-{{arch}}`")));
    };
    if os.is_empty() || arch.is_empty() {
        return Err(invalid(format!("`{s}` has an empty segment")));
    }
    if arch.contains('-') {
        return Err(invalid(format!("`{s}` has more than two segments")));
    }
    if !os.starts_with(|c: char| c.is_ascii_lowercase()) {
        return Err(invalid(format!(
            "`{s}` does not start with a lower-case ASCII letter"
        )));
    }
    if !os
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    {
        return Err(invalid(format!("`{s}` has an illegal character in the os")));
    }
    if !arch
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return Err(invalid(format!(
            "`{s}` has an illegal character in the architecture"
        )));
    }
    Ok(())
}

impl fmt::Display for TargetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for TargetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TargetId({:?})", self.0)
    }
}

impl AsRef<str> for TargetId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl FromStr for TargetId {
    type Err = BundleError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl Serialize for TargetId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for TargetId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(|err| {
            DeError::invalid_value(
                Unexpected::Str(&raw),
                &err.detail().unwrap_or("a target id"),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_round_trips() {
        for id in TargetId::registry() {
            assert!(id.is_known(), "{id}");
            let reparsed = TargetId::parse(id.as_str()).expect("registry entry parses");
            assert_eq!(reparsed, id);
            assert_eq!(reparsed.to_string(), id.as_str());
        }
    }

    #[test]
    fn dylib_extension_matches_axt_v1_section_3_2() {
        assert_eq!(
            TargetId::parse(WINDOWS_X86_64).unwrap().dylib_extension(),
            "dll"
        );
        assert_eq!(
            TargetId::parse(WINDOWS_AARCH64).unwrap().dylib_extension(),
            "dll"
        );
        assert_eq!(
            TargetId::parse(LINUX_X86_64).unwrap().dylib_extension(),
            "so"
        );
        assert_eq!(
            TargetId::parse(LINUX_AARCH64).unwrap().dylib_extension(),
            "so"
        );
        assert_eq!(TargetId::parse(MACOS_X86_64).unwrap().dylib_extension(), "");
        assert_eq!(TargetId::parse(MACOS_ARM64).unwrap().dylib_extension(), "");
        assert_eq!(
            TargetId::parse(MACOS_UNIVERSAL).unwrap().dylib_extension(),
            ""
        );
    }

    #[test]
    fn macos_aarch64_is_normalised_to_arm64() {
        let id = TargetId::parse("macos-aarch64").expect("alias parses");
        assert_eq!(id.as_str(), MACOS_ARM64);
        assert!(id.is_known());
        assert_eq!(
            TargetId::from_rust_triple("aarch64-apple-darwin")
                .unwrap()
                .as_str(),
            MACOS_ARM64
        );
    }

    #[test]
    fn triples_map_both_ways() {
        for id in TargetId::registry() {
            for triple in id.to_rust_triples() {
                let back = TargetId::from_rust_triple(triple).expect("triple maps back");
                if id.is_universal() {
                    assert!(back.is_apple() && !back.is_universal());
                } else {
                    assert_eq!(&back, &id, "{triple}");
                }
            }
        }
        assert_eq!(
            TargetId::from_rust_triple("x86_64-unknown-linux-musl")
                .unwrap()
                .as_str(),
            LINUX_X86_64
        );
        assert!(TargetId::from_rust_triple("wasm32-unknown-unknown").is_none());
        assert!(TargetId::from_rust_triple("").is_none());
        assert!(TargetId::from_rust_triple("x86_64-pc-windows-msvc-extra").is_none());
    }

    #[test]
    fn unregistered_but_well_formed_ids_parse() {
        let future = TargetId::parse("linux-riscv64").expect("well-formed id parses");
        assert!(!future.is_known());
        assert!(future.to_rust_triples().is_empty());
        assert_eq!(future.dylib_extension(), "so");
        assert_eq!(future.os(), "linux");
        assert_eq!(future.arch(), "riscv64");
    }

    #[test]
    fn malformed_ids_are_rejected() {
        for bad in [
            "",
            "-",
            "windows-",
            "-x86_64",
            "Windows-x86_64",
            "windows-X86_64",
            "windows-x86_64-msvc",
            "windows_x86_64",
            "9windows-x86_64",
            "windows-x86 64",
            "windows-x86.64",
            "windows-x86/64",
            "wíndows-x86_64",
            "windows-x86_64\u{0}",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-x86_64",
        ] {
            let err = TargetId::parse(bad).expect_err(bad);
            assert_eq!(err.kind(), &BundleErrorKind::InvalidTarget, "{bad}");
            assert_eq!(err.code(), "DAUX-M013");
        }
    }

    #[test]
    fn host_is_well_formed_and_candidates_prefer_universal() {
        let host = TargetId::host();
        assert!(validate_syntax(host.as_str()).is_ok(), "{host}");
        let candidates = TargetId::host_candidates();
        assert!(candidates.contains(&host));
        if host.is_apple() {
            assert_eq!(candidates.len(), 2);
            assert!(candidates[0].is_universal());
        } else {
            assert_eq!(candidates.len(), 1);
        }
    }

    #[test]
    fn apple_and_universal_predicates() {
        assert!(TargetId::parse(MACOS_UNIVERSAL).unwrap().is_universal());
        assert!(TargetId::parse(MACOS_UNIVERSAL).unwrap().is_apple());
        assert!(!TargetId::parse(LINUX_X86_64).unwrap().is_apple());
        assert!(!TargetId::parse(MACOS_ARM64).unwrap().is_universal());
    }

    #[test]
    fn serde_round_trip_and_rejection() {
        let id = TargetId::parse(LINUX_AARCH64).unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"linux-aarch64\"");
        let back: TargetId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);

        assert!(serde_json::from_str::<TargetId>("\"WINDOWS-X86_64\"").is_err());
        assert!(serde_json::from_str::<TargetId>("7").is_err());
        assert!(serde_json::from_str::<TargetId>("null").is_err());
    }

    #[test]
    fn from_str_and_debug() {
        let id: TargetId = "linux-x86_64".parse().unwrap();
        assert_eq!(format!("{id:?}"), "TargetId(\"linux-x86_64\")");
        assert_eq!(id.as_ref(), "linux-x86_64");
        assert!("linux x86".parse::<TargetId>().is_err());
    }
}
