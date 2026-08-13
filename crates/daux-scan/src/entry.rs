//! What a scan produces: what was found, what was skipped, and what went wrong.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use daux_bundle::{BundleMetadata, ValidationIssue};
use daux_runtime::daux_core::PluginDescriptor;

use crate::error::{ScanError, ScanErrorKind};
use crate::format::PluginFormat;

/// One catalogued DAUx plug-in bundle. [main-thread]
///
/// This is what a host stores in its plug-in database and shows in a browser. It is
/// produced without instantiating anything: [`descriptors`](ScanEntry::descriptors) come
/// from the factory's enumeration, which `abi-v1` §5 requires to be lightweight, and
/// [`metadata`](ScanEntry::metadata) comes from the manifest, which loads no code at all.
///
/// # Which side is authoritative
///
/// `axt-v1` §8.3 is unambiguous: the manifest is authoritative for what a scanner needs
/// *before* executing code — layout, targets, where the binary is — and the binary
/// descriptor is authoritative for everything else. When [`probed`](ScanEntry::probed) is
/// `true`, a host must show the descriptor's values, not the manifest's; the differences
/// are recorded in [`issues`](ScanEntry::issues) rather than silently reconciled.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct ScanEntry {
    /// The bundle's root directory.
    pub path: PathBuf,
    /// Which format this artefact is. Always [`PluginFormat::Axt`] in v1: it is the only
    /// format this workspace can describe without executing foreign code.
    pub format: PluginFormat,
    /// Identity, targets and capabilities, read from `manifest.json` or `Info.plist`.
    pub metadata: BundleMetadata,
    /// Every plug-in the module's factory publishes, or empty when the binary was not
    /// probed — see [`probed`](ScanEntry::probed).
    pub descriptors: Vec<PluginDescriptor>,
    /// When this entry was produced. Carried through the cache, so a cached entry keeps
    /// the time of the scan that really read the bundle.
    pub scanned_at: SystemTime,
    /// Covers the metadata bytes and the binary's size and modification time
    /// (`manifest-v1` §8.2). Changing either one produces a different value.
    pub fingerprint: u64,
    /// Whether the dynamic library was opened and its factory enumerated.
    ///
    /// `false` means [`descriptors`](ScanEntry::descriptors) is empty and every value here
    /// comes from the manifest, which is user-writable and may be wrong.
    pub probed: bool,
    /// Everything wrong with this bundle, from `Bundle::validate` and from the
    /// manifest ↔ binary cross-check of `manifest-v1` §8.1.
    ///
    /// A non-empty list does not make an entry unusable: only the two *fatal* rows of
    /// §8.1 do, and an entry that hits one of those is never produced at all.
    pub issues: Vec<ValidationIssue>,
}

impl ScanEntry {
    /// The plug-in id a saved project references. [main-thread]
    ///
    /// From the descriptor when the binary was probed, and from the manifest otherwise —
    /// the order `axt-v1` §8.3 requires.
    #[must_use]
    pub fn id(&self) -> &str {
        self.descriptors
            .first()
            .map_or(self.metadata.id.as_str(), |d| d.id.as_str())
    }

    /// The display name. [main-thread]
    #[must_use]
    pub fn name(&self) -> &str {
        self.descriptors
            .first()
            .map_or(self.metadata.name.as_str(), |d| d.name.as_str())
    }

    /// The vendor name. [main-thread]
    #[must_use]
    pub fn vendor(&self) -> &str {
        self.descriptors
            .first()
            .map_or(self.metadata.vendor.as_str(), |d| d.vendor.as_str())
    }

    /// How many plug-ins this bundle publishes, as far as the scan could tell.
    /// [main-thread]
    ///
    /// One for an unprobed bundle: the manifest describes a principal plug-in, and
    /// `axt-v1` §7.6 makes the factory's enumeration the only authority on the rest.
    #[must_use]
    pub fn plugin_count(&self) -> usize {
        self.descriptors.len().max(1)
    }

    /// Whether anything in [`issues`](ScanEntry::issues) is an error. [main-thread]
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.issues
            .iter()
            .any(|issue| issue.severity == daux_bundle::Severity::Error)
    }
}

/// One artefact that could not be catalogued. [main-thread]
///
/// A failure is a report, not an interruption. Four hundred plug-ins with one bad apple
/// produce three hundred and ninety-nine entries and one of these.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ScanFailure {
    /// The artefact the failure is about.
    pub path: PathBuf,
    /// The format its name claimed, when it claimed one.
    pub format: Option<PluginFormat>,
    /// Why it failed.
    pub kind: ScanErrorKind,
    /// The detail, for a log or a tooltip.
    pub message: String,
}

impl ScanFailure {
    /// Records a failure against `path`. [main-thread]
    #[must_use]
    pub(crate) fn new(
        path: impl Into<PathBuf>,
        format: Option<PluginFormat>,
        error: &ScanError,
    ) -> Self {
        Self {
            path: error.path().map_or_else(|| path.into(), Path::to_path_buf),
            format,
            kind: error.kind(),
            message: error.message().to_owned(),
        }
    }

    /// Whether this artefact will be skipped again next time unless something changes.
    /// [main-thread]
    #[must_use]
    pub const fn is_sticky(&self) -> bool {
        self.kind.is_sticky()
    }
}

impl core::fmt::Display for ScanFailure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{}: {} ({})",
            self.kind,
            self.message,
            self.path.display()
        )
    }
}

/// A VST3 or CLAP artefact that was found but not described. [main-thread]
///
/// Describing one means loading it through that format's own C ABI and calling into it,
/// which the host side of this workspace deliberately does not do in v1. Reporting the
/// location is still worth doing: a host has to know a file is there before it can decide
/// to hand it to a VST3 or CLAP host implementation.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ForeignPlugin {
    /// The artefact — a directory bundle or a bare shared library, depending on the format
    /// and the platform.
    pub path: PathBuf,
    /// Which format it claims to be, by extension.
    pub format: PluginFormat,
    /// Path, size and modification time, so a host can tell it was replaced.
    pub fingerprint: u64,
    /// When it was found.
    pub scanned_at: SystemTime,
}

/// What one scan cost. [main-thread]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ScanStats {
    /// Artefacts of any format that were looked at.
    pub examined: usize,
    /// Entries served from the cache without re-reading or re-loading the bundle.
    pub from_cache: usize,
    /// Bundles whose binary was opened and enumerated during this scan.
    pub probed: usize,
    /// Artefacts that produced a [`ScanFailure`].
    pub failed: usize,
    /// Bundles skipped because they took the scanner down last time.
    pub quarantined: usize,
    /// Directories walked during discovery.
    pub directories: usize,
    /// How long the whole scan took.
    pub duration: Duration,
}

/// Everything one scan found. [main-thread]
///
/// A report never means "the scan failed" — there is no such outcome. It means "here is
/// what is installed, here is what is broken, and here is what I could not describe".
#[derive(Clone, Debug, Default)]
pub struct ScanReport {
    entries: Vec<ScanEntry>,
    failures: Vec<ScanFailure>,
    foreign: Vec<ForeignPlugin>,
    stats: ScanStats,
}

impl ScanReport {
    /// An empty report. [main-thread]
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The DAUx bundles that were catalogued, in discovery order. [main-thread]
    #[inline]
    #[must_use]
    pub fn entries(&self) -> &[ScanEntry] {
        &self.entries
    }

    /// The artefacts that could not be catalogued. [main-thread]
    #[inline]
    #[must_use]
    pub fn failures(&self) -> &[ScanFailure] {
        &self.failures
    }

    /// The VST3 and CLAP artefacts that were found but not described. [main-thread]
    #[inline]
    #[must_use]
    pub fn foreign(&self) -> &[ForeignPlugin] {
        &self.foreign
    }

    /// What the scan cost. [main-thread]
    #[inline]
    #[must_use]
    pub const fn stats(&self) -> &ScanStats {
        &self.stats
    }

    /// How many DAUx bundles were catalogued. [main-thread]
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing at all was found — no entries, no failures, no foreign artefacts.
    /// [main-thread]
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.failures.is_empty() && self.foreign.is_empty()
    }

    /// The entry publishing `id`, if one does. [main-thread]
    ///
    /// Matches every descriptor the bundle publishes, not only the principal one, because a
    /// saved project references a plug-in id and a multi-plug-in bundle publishes several.
    #[must_use]
    pub fn find(&self, id: &str) -> Option<&ScanEntry> {
        self.entries.iter().find(|entry| {
            entry.metadata.id == id || entry.descriptors.iter().any(|d| d.id.as_str() == id)
        })
    }

    /// The entry at `path`, if one was catalogued. [main-thread]
    #[must_use]
    pub fn at(&self, path: &Path) -> Option<&ScanEntry> {
        self.entries.iter().find(|entry| entry.path == path)
    }

    /// Takes the entries out, leaving the report empty. [main-thread]
    #[must_use]
    pub fn into_entries(self) -> Vec<ScanEntry> {
        self.entries
    }

    pub(crate) fn push_entry(&mut self, entry: ScanEntry) {
        self.entries.push(entry);
    }

    pub(crate) fn push_failure(&mut self, failure: ScanFailure) {
        self.stats.failed += 1;
        if failure.kind == ScanErrorKind::Quarantined {
            self.stats.quarantined += 1;
        }
        self.failures.push(failure);
    }

    pub(crate) fn push_foreign(&mut self, foreign: ForeignPlugin) {
        self.foreign.push(foreign);
    }

    pub(crate) const fn stats_mut(&mut self) -> &mut ScanStats {
        &mut self.stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{descriptor, metadata};

    fn entry(path: &str, id: &str, probed: bool) -> ScanEntry {
        ScanEntry {
            path: PathBuf::from(path),
            format: PluginFormat::Axt,
            metadata: metadata(id, "Manifest Name", "Manifest Vendor", "1.0.0"),
            descriptors: if probed {
                vec![descriptor(id, "Binary Name", "Binary Vendor")]
            } else {
                Vec::new()
            },
            scanned_at: SystemTime::UNIX_EPOCH,
            fingerprint: 7,
            probed,
            issues: Vec::new(),
        }
    }

    /// `axt-v1` §8.3: once the module is loaded, the descriptor wins. A host that showed
    /// the manifest's name next to a loaded plug-in would be showing a value the plug-in
    /// itself disagrees with.
    #[test]
    fn a_probed_entry_reports_the_binarys_values_and_an_unprobed_one_the_manifests() {
        let probed = entry("/p/Gain.axt", "com.example.gain", true);
        assert_eq!(probed.name(), "Binary Name");
        assert_eq!(probed.vendor(), "Binary Vendor");
        assert_eq!(probed.id(), "com.example.gain");

        let unprobed = entry("/p/Gain.axt", "com.example.gain", false);
        assert_eq!(unprobed.name(), "Manifest Name");
        assert_eq!(unprobed.vendor(), "Manifest Vendor");
        assert_eq!(
            unprobed.plugin_count(),
            1,
            "a manifest describes exactly one principal plug-in"
        );
    }

    #[test]
    fn a_report_finds_entries_by_any_id_the_bundle_publishes() {
        let mut report = ScanReport::new();
        let mut multi = entry("/p/Suite.axt", "com.example.suite", true);
        multi
            .descriptors
            .push(descriptor("com.example.suite.eq", "EQ", "Example"));
        report.push_entry(multi);
        report.push_entry(entry("/p/Gain.axt", "com.example.gain", true));

        assert_eq!(report.len(), 2);
        assert!(!report.is_empty());
        assert_eq!(
            report.find("com.example.suite.eq").map(|e| e.path.clone()),
            Some(PathBuf::from("/p/Suite.axt")),
            "a saved project may reference any plug-in of a multi-plug-in bundle"
        );
        assert!(report.find("com.example.nothing").is_none());
        assert_eq!(
            report.at(Path::new("/p/Gain.axt")).map(ScanEntry::id),
            Some("com.example.gain")
        );
    }

    #[test]
    fn failures_are_counted_and_quarantine_is_counted_twice_over() {
        let mut report = ScanReport::new();
        report.push_failure(ScanFailure::new(
            "/p/Bad.axt",
            Some(PluginFormat::Axt),
            &ScanError::new(ScanErrorKind::Metadata, "unparseable"),
        ));
        report.push_failure(ScanFailure::new(
            "/p/Crasher.axt",
            Some(PluginFormat::Axt),
            &ScanError::new(ScanErrorKind::Quarantined, "took the scanner down"),
        ));

        assert_eq!(report.stats().failed, 2);
        assert_eq!(report.stats().quarantined, 1);
        assert!(report.failures()[0].message.contains("unparseable"));
        assert!(!report.failures()[0].is_sticky());
        assert!(report.failures()[1].is_sticky());
        assert!(!report.is_empty(), "failures are findings too");
    }

    /// The path on the error is more precise than the one the caller knew, so it must win.
    #[test]
    fn a_failure_keeps_the_most_precise_path() {
        let error = ScanError::new(ScanErrorKind::Metadata, "duplicate key")
            .with_path("/p/Gain.axt/manifest.json");
        let failure = ScanFailure::new("/p/Gain.axt", Some(PluginFormat::Axt), &error);
        assert_eq!(failure.path, PathBuf::from("/p/Gain.axt/manifest.json"));
        assert!(failure.to_string().contains("manifest.json"));
    }

    #[test]
    fn an_entry_reports_whether_anything_is_actually_broken() {
        let mut entry = entry("/p/Gain.axt", "com.example.gain", true);
        assert!(!entry.has_errors());
        entry
            .issues
            .push(ValidationIssue::warning("unknown-target", "aix-power64"));
        assert!(!entry.has_errors(), "a warning is not an error");
        entry
            .issues
            .push(ValidationIssue::error("DAUX-M101", "name disagrees"));
        assert!(entry.has_errors());
    }
}
