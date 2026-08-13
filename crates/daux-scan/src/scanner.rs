//! Walking the disk, and deciding what to open.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use daux_bundle::{Bundle, BundleLayout, TargetId, ValidationIssue};
use daux_runtime::daux_abi::DAUX_ABI_VERSION_MAJOR;

use crate::cache::ScanCache;
use crate::crosscheck::cross_check;
use crate::entry::{ForeignPlugin, ScanEntry, ScanFailure, ScanReport};
use crate::error::{ScanError, ScanErrorKind, ScanResult};
use crate::fingerprint;
use crate::format::PluginFormat;
use crate::isolation::{ProbeOutcome, Quarantine, probe_isolated};
use crate::search_paths::{Platform, process_env, search_paths_for};

/// How long one bundle may take to load and enumerate before it is assumed hung.
///
/// Generous on purpose: a large sample library reads its index at load time, and a cold
/// disk is slow. The cost of being too patient is a slow scan of one plug-in; the cost of
/// being too impatient is a working plug-in the user cannot use.
pub const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// How deep below a search path a bundle may be.
///
/// Vendors nest — `CLAP/Acme/Suite/Reverb.clap` is ordinary — but nobody nests eight deep,
/// and an unbounded walk of a search path someone pointed at `/` is a start-up that never
/// finishes.
pub const DEFAULT_MAX_DEPTH: usize = 6;

/// How the scanner opens a module and enumerates it.
///
/// A function pointer rather than a direct call, so that the parts of a scan that cannot be
/// exercised without a compiled plug-in — the cache actually saving a load, a hang turning
/// into a quarantine, an identity mismatch rejecting a bundle — can be tested against a
/// stand-in. Production always uses [`probe_isolated`].
type Prober = fn(&Bundle, &TargetId, Duration) -> ScanResult<ProbeOutcome>;

/// Finds plug-ins. [main-thread]
///
/// ```no_run
/// use daux_scan::Scanner;
///
/// let mut scanner = Scanner::with_cache("~/.cache/daux/scan.json".into());
/// let report = scanner.scan();
/// for entry in report.entries() {
///     println!("{} — {} ({} plug-ins)", entry.name(), entry.vendor(), entry.plugin_count());
/// }
/// for failure in report.failures() {
///     eprintln!("skipped {failure}");
/// }
/// ```
///
/// # What a scan costs, and what the cache saves
///
/// Discovery reads directories and manifests: cheap, and done every time, because a cached
/// answer about a file that may have been deleted is worse than no answer. Probing opens
/// the dynamic library and enumerates its factory: expensive, done once per version of a
/// bundle, and cached on a fingerprint that covers the manifest's bytes and the binary's
/// size and modification time.
///
/// # What a scan survives
///
/// Everything. A bundle with a corrupt manifest, a binary for another architecture, a
/// module that panics, a module that hangs, and a module that killed the last scan are each
/// recorded as a [`ScanFailure`](crate::ScanFailure) and the walk continues. There is no
/// input that makes [`Scanner::scan`] return fewer plug-ins than it found.
#[derive(Debug)]
pub struct Scanner {
    search_paths: Vec<PathBuf>,
    formats: Vec<PluginFormat>,
    cache: ScanCache,
    quarantine: Quarantine,
    probe: bool,
    probe_timeout: Duration,
    max_depth: usize,
    target: TargetId,
    cache_loaded: bool,
    /// How a module is opened. Always [`probe_isolated`] outside this crate's own tests.
    prober: Prober,
    /// Probes served from the cache during the scan in progress.
    hits: usize,
    /// Probes that really opened a module during the scan in progress.
    misses: usize,
}

impl Default for Scanner {
    fn default() -> Self {
        Self::new()
    }
}

impl Scanner {
    /// A scanner over the platform's usual directories, with an in-memory cache.
    /// [main-thread]
    ///
    /// The cache lives for as long as the scanner does, which already saves a rescan inside
    /// one session. Crash isolation across restarts needs somewhere to write, so it is
    /// available only from [`Scanner::with_cache`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            search_paths: Self::default_search_paths(),
            formats: PluginFormat::ALL.to_vec(),
            cache: ScanCache::in_memory(),
            quarantine: Quarantine::in_memory(),
            probe: true,
            probe_timeout: DEFAULT_PROBE_TIMEOUT,
            max_depth: DEFAULT_MAX_DEPTH,
            target: TargetId::host(),
            cache_loaded: true,
            prober: probe_isolated,
            hits: 0,
            misses: 0,
        }
    }

    /// A scanner whose cache, crash journal and quarantine live beside `path`.
    /// [main-thread]
    ///
    /// Three files are used: `path` itself for the cache, `path.inflight` for the bundle
    /// currently being probed, and `path.quarantine` for the ones that never came back.
    /// Nothing is read until the first scan.
    #[must_use]
    pub fn with_cache(path: PathBuf) -> Self {
        Self {
            quarantine: Quarantine::beside(&path),
            cache: ScanCache::at(path),
            cache_loaded: false,
            ..Self::new()
        }
    }

    /// Adds a directory to search. [main-thread]
    pub fn add_search_path(&mut self, path: PathBuf) -> &mut Self {
        if !self.search_paths.contains(&path) {
            self.search_paths.push(path);
        }
        self
    }

    /// Removes every search path, including the platform defaults. [main-thread]
    pub fn clear_search_paths(&mut self) -> &mut Self {
        self.search_paths.clear();
        self
    }

    /// The directories this scanner will search, in order. [main-thread]
    #[must_use]
    pub fn search_paths(&self) -> &[PathBuf] {
        &self.search_paths
    }

    /// Where plug-ins live on this platform. [main-thread]
    ///
    /// See the [module documentation](crate) for the table, and note that the list is not
    /// filtered against the filesystem: a directory that does not exist yet is where the
    /// user's first plug-in will be installed.
    #[must_use]
    pub fn default_search_paths() -> Vec<PathBuf> {
        search_paths_for(Platform::current(), &process_env)
    }

    /// Restricts the scan to these formats. An empty list finds nothing. [main-thread]
    pub fn set_formats(&mut self, formats: &[PluginFormat]) -> &mut Self {
        self.formats = formats.to_vec();
        self
    }

    /// The formats this scanner looks for. [main-thread]
    #[must_use]
    pub fn formats(&self) -> &[PluginFormat] {
        &self.formats
    }

    /// Whether to open each bundle's binary and enumerate its factory. [main-thread]
    ///
    /// On by default. Turning it off makes a scan a pure catalogue — `axt-v1` §9 rule 1's
    /// "discovery MUST NOT load code" — at the price of knowing nothing the manifest does
    /// not say, and the manifest is user-writable.
    pub fn set_probe(&mut self, probe: bool) -> &mut Self {
        self.probe = probe;
        self
    }

    /// How long one bundle may take to load before it is abandoned. [main-thread]
    pub fn set_probe_timeout(&mut self, timeout: Duration) -> &mut Self {
        self.probe_timeout = timeout;
        self
    }

    /// How deep below a search path to look. [main-thread]
    pub fn set_max_depth(&mut self, depth: usize) -> &mut Self {
        self.max_depth = depth;
        self
    }

    /// Which target's binary to load. Defaults to the running process's. [main-thread]
    pub fn set_target(&mut self, target: TargetId) -> &mut Self {
        self.target = target;
        self
    }

    /// Lets every quarantined plug-in be tried again. [main-thread]
    ///
    /// What a "rescan all plug-ins, including the ones that crashed" button calls.
    pub fn clear_quarantine(&mut self) -> &mut Self {
        self.quarantine.clear();
        self
    }

    /// Lets one quarantined plug-in be tried again. [main-thread]
    pub fn forget(&mut self, path: &Path) -> &mut Self {
        self.quarantine.forget(path);
        self
    }

    /// The bundles that took the scanner down since the last scan. [main-thread]
    ///
    /// Populated when the scanner is built, from the crash journal, so a host can tell the
    /// user *before* scanning which plug-in it is about to skip.
    #[must_use]
    pub fn crashed_last_time(&self) -> &[PathBuf] {
        self.quarantine.crashed()
    }

    /// How many probe results are cached. [main-thread]
    #[must_use]
    pub fn cached_count(&self) -> usize {
        self.cache.len()
    }

    /// Scans every search path. [main-thread]
    ///
    /// Cache records for bundles that no longer exist are dropped, and the cache is written
    /// back if anything changed. A failure to read or write it is reported in the report,
    /// never raised: a scan without a cache is slow, not wrong.
    pub fn scan(&mut self) -> ScanReport {
        let started = Instant::now();
        let mut report = self.begin(&started);

        let mut seen = BTreeSet::new();
        let mut visited = BTreeSet::new();
        let roots = self.search_paths.clone();
        for root in &roots {
            self.walk(root, 0, &mut report, &mut seen, &mut visited);
        }

        // Safe only here: `seen` covers every search path, so anything missing from it is
        // really gone rather than merely outside the directory that was scanned.
        let formats = self.formats.clone();
        self.cache.retain_seen(&seen, &formats);
        self.finish(&mut report, started);
        report
    }

    /// Scans one directory tree. [main-thread]
    ///
    /// Unlike [`Scanner::scan`] this never prunes the cache: the bundles outside `root` were
    /// not looked for, and forgetting them would make the next full scan reload everything.
    pub fn scan_path(&mut self, root: &Path) -> ScanReport {
        let started = Instant::now();
        let mut report = self.begin(&started);

        let mut seen = BTreeSet::new();
        let mut visited = BTreeSet::new();
        self.walk(root, 0, &mut report, &mut seen, &mut visited);

        self.finish(&mut report, started);
        report
    }

    /// Reads one bundle without loading any code. [main-thread]
    ///
    /// The cataloguing half of a scan, on its own: metadata, validation and a fingerprint,
    /// and nothing that could run a line of the plug-in's code. No cache is consulted and
    /// none is written, which is what makes it an associated function.
    ///
    /// # Errors
    ///
    /// [`ScanErrorKind::NotABundle`] when there is no bundle there, and
    /// [`ScanErrorKind::Metadata`] for metadata that will not parse.
    pub fn scan_one(path: &Path) -> ScanResult<ScanEntry> {
        let bundle = Bundle::open(path)?;
        let metadata_bytes = read_metadata_bytes(path, bundle.layout());
        let binary = bundle.binary_path(&TargetId::host()).ok();
        Ok(ScanEntry {
            path: path.to_path_buf(),
            format: PluginFormat::Axt,
            fingerprint: fingerprint::of_bundle(path, &metadata_bytes, binary.as_deref()),
            issues: bundle.validate(),
            metadata: bundle.metadata().clone(),
            descriptors: Vec::new(),
            scanned_at: SystemTime::now(),
            probed: false,
        })
    }

    /// Reads one bundle, with the cache, the quarantine and — unless probing is off — its
    /// binary. [main-thread]
    ///
    /// This is what the walk calls for every `.axt` it finds, and what a host calls when the
    /// user drops a bundle onto it.
    ///
    /// # Errors
    ///
    /// Whatever [`Scanner::scan_one`] reports, plus [`ScanErrorKind::Quarantined`] for a
    /// bundle that crashed the last scan, [`ScanErrorKind::Timeout`] for one that hangs,
    /// [`ScanErrorKind::Panicked`] for one that panics, [`ScanErrorKind::Load`] for a module
    /// this host may not call into, and [`ScanErrorKind::Identity`] for a bundle whose
    /// manifest and binary disagree about who it is.
    pub fn inspect(&mut self, path: &Path) -> ScanResult<ScanEntry> {
        // A cache that will not load is not this bundle's problem; the scan that owns the
        // report has already recorded it, and inspecting one bundle without a cache simply
        // costs one probe.
        let _ = self.ensure_cache();
        let bundle = Bundle::open(path).map_err(|error| ScanError::from(error).or_path(path))?;
        let metadata_bytes = read_metadata_bytes(path, bundle.layout());
        let binary = bundle.binary_path(&self.target).ok();
        let fingerprint = fingerprint::of_bundle(path, &metadata_bytes, binary.as_deref());
        let mut issues = bundle.validate();

        if self.quarantine.is_quarantined(path, fingerprint) {
            return Err(ScanError::new(
                ScanErrorKind::Quarantined,
                "this bundle did not survive its last scan; it is skipped until it changes \
                 or the user asks for it",
            )
            .with_path(path));
        }

        let mut descriptors = Vec::new();
        let mut probed = false;
        let mut scanned_at = SystemTime::now();

        if self.probe {
            // `manifest-v1` §8.3: the manifest's `abiVersion` exists so a host can refuse a
            // module *before* paying for the `dlopen` that would refuse it anyway.
            if bundle.metadata().abi_version == DAUX_ABI_VERSION_MAJOR {
                match self.probe_bundle(&bundle, path, fingerprint) {
                    Ok((outcome, at)) => {
                        cross_check(bundle.metadata(), &outcome, &mut issues)
                            .map_err(|error| error.or_path(path))?;
                        descriptors = outcome.descriptors;
                        probed = true;
                        scanned_at = at;
                    }
                    Err(error) => return Err(error.or_path(path)),
                }
            } else {
                issues.push(ValidationIssue::warning(
                    "axt.abi.newer",
                    format!(
                        "the bundle declares ABI major version {}; this host implements \
                         {DAUX_ABI_VERSION_MAJOR}, so its binary was not opened \
                         (manifest-v1 §8.3)",
                        bundle.metadata().abi_version
                    ),
                ));
            }
        }

        Ok(ScanEntry {
            path: path.to_path_buf(),
            format: PluginFormat::Axt,
            metadata: bundle.metadata().clone(),
            descriptors,
            scanned_at,
            fingerprint,
            probed,
            issues,
        })
    }

    /// Replaces the function that opens modules. Tests only. [main-thread]
    #[cfg(test)]
    pub(crate) fn set_prober(&mut self, prober: Prober) -> &mut Self {
        self.prober = prober;
        self
    }

    /// Writes the cache, if this scanner has one. [main-thread]
    ///
    /// [`Scanner::scan`] does this already; a host that drives [`Scanner::inspect`] itself
    /// calls it when it is done.
    ///
    /// # Errors
    ///
    /// [`ScanErrorKind::Io`] or [`ScanErrorKind::Cache`].
    pub fn save_cache(&mut self) -> ScanResult<()> {
        self.cache.save()
    }

    /// The cached probe for a bundle, or a real one, journalled so a crash is survivable.
    fn probe_bundle(
        &mut self,
        bundle: &Bundle,
        path: &Path,
        fingerprint: u64,
    ) -> ScanResult<(ProbeOutcome, SystemTime)> {
        if let Some(hit) = self.cache.lookup(path, fingerprint)
            && hit.probed
        {
            self.hits += 1;
            return Ok((
                ProbeOutcome {
                    descriptors: hit.descriptors,
                    // The cache stores what the factory published, not the module header. A
                    // cached entry therefore cannot re-check row 7 of `manifest-v1` §8.1,
                    // so it reports what the manifest claims and the row passes — the check
                    // already ran, and produced its issue, on the scan that really loaded it.
                    abi_version: (bundle.metadata().abi_version, 0),
                },
                hit.scanned_at,
            ));
        }

        let timeout = self.probe_timeout;
        let target = self.target.clone();
        let prober = self.prober;
        let outcome = self
            .quarantine
            .guard(path, fingerprint, || prober(bundle, &target, timeout));

        match outcome {
            Ok(outcome) => {
                self.misses += 1;
                let now = SystemTime::now();
                self.cache.store(
                    path,
                    fingerprint,
                    PluginFormat::Axt,
                    now,
                    true,
                    &outcome.descriptors,
                );
                Ok((outcome, now))
            }
            Err(error) => {
                // A hang leaves a thread inside the module forever. Trying again next
                // start-up would cost another timeout for the same certain failure, so it
                // is treated exactly like a crash.
                if error.kind() == ScanErrorKind::Timeout {
                    self.quarantine.quarantine(path, fingerprint);
                }
                Err(error)
            }
        }
    }

    /// Reads the cache file the first time it is needed. [main-thread]
    ///
    /// Returns the failure rather than raising it: a cache that will not load costs a slow
    /// scan, and a scan that refused to run because of it would cost the user their
    /// plug-ins.
    fn ensure_cache(&mut self) -> Option<ScanError> {
        if self.cache_loaded {
            return None;
        }
        self.cache_loaded = true;
        self.cache.load().err()
    }

    /// Starts a scan: resets the per-scan counters and loads the cache.
    fn begin(&mut self, _started: &Instant) -> ScanReport {
        let mut report = ScanReport::new();
        self.hits = 0;
        self.misses = 0;
        if let Some(error) = self.ensure_cache() {
            let path = error.path().map_or_else(PathBuf::new, Path::to_path_buf);
            report.push_failure(ScanFailure::new(path, None, &error));
        }
        report
    }

    /// Ends a scan: publishes the counters and writes the cache back.
    fn finish(&mut self, report: &mut ScanReport, started: Instant) {
        if let Err(error) = self.cache.save() {
            let path = error.path().map_or_else(PathBuf::new, Path::to_path_buf);
            report.push_failure(ScanFailure::new(path, None, &error));
        }
        let stats = report.stats_mut();
        stats.from_cache = self.hits;
        stats.probed = self.misses;
        stats.duration = started.elapsed();
    }

    /// One directory, and everything below it.
    fn walk(
        &mut self,
        directory: &Path,
        depth: usize,
        report: &mut ScanReport,
        seen: &mut BTreeSet<String>,
        visited: &mut BTreeSet<PathBuf>,
    ) {
        // Symlinked directories are followed, because that is how plug-ins are installed on
        // Unix, and a loop is broken by remembering the real path rather than by refusing
        // to follow.
        let real = std::fs::canonicalize(directory).unwrap_or_else(|_| directory.to_path_buf());
        if !visited.insert(real) {
            return;
        }

        let entries = match std::fs::read_dir(directory) {
            Ok(entries) => entries,
            // A search path that does not exist is the normal case for a user who has no
            // plug-ins of that format, and is not worth a report.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => {
                report.push_failure(ScanFailure::new(
                    directory,
                    None,
                    &ScanError::new(ScanErrorKind::Io, error.to_string()).with_path(directory),
                ));
                return;
            }
        };
        report.stats_mut().directories += 1;

        // Sorted, so that two machines with the same plug-ins produce the same report and a
        // diff of two scans means something.
        let mut children: Vec<PathBuf> = Vec::new();
        for entry in entries {
            match entry {
                Ok(entry) => children.push(entry.path()),
                Err(error) => report.push_failure(ScanFailure::new(
                    directory,
                    None,
                    &ScanError::new(ScanErrorKind::Io, error.to_string()).with_path(directory),
                )),
            }
        }
        children.sort();

        for child in children {
            match PluginFormat::from_path(&child) {
                Some(format) => {
                    if self.formats.contains(&format) {
                        self.artefact(&child, format, report, seen);
                    }
                    // Never descend into an artefact: a `.vst3` bundle contains directories
                    // that look like nothing in particular, and an `.axt` contains a
                    // `Resources` tree that may contain anything at all.
                }
                None => {
                    if depth < self.max_depth && child.is_dir() {
                        self.walk(&child, depth + 1, report, seen, visited);
                    }
                }
            }
        }
    }

    /// One artefact, of a format this scan is interested in.
    fn artefact(
        &mut self,
        path: &Path,
        format: PluginFormat,
        report: &mut ScanReport,
        seen: &mut BTreeSet<String>,
    ) {
        report.stats_mut().examined += 1;
        if let Some(key) = path.to_str() {
            seen.insert(key.to_owned());
        }

        if !format.has_readable_metadata() {
            report.push_foreign(ForeignPlugin {
                path: path.to_path_buf(),
                format,
                fingerprint: fingerprint::of_file(path),
                scanned_at: SystemTime::now(),
            });
            return;
        }

        match self.inspect(path) {
            Ok(entry) => report.push_entry(entry),
            Err(error) => report.push_failure(ScanFailure::new(path, Some(format), &error)),
        }
    }
}

/// The bytes of whichever metadata file this layout keeps its identity in.
///
/// Empty when neither is readable, which cannot happen for a bundle that opened — and if it
/// somehow does, an empty fingerprint input simply means the entry is rescanned every time.
fn read_metadata_bytes(root: &Path, layout: BundleLayout) -> Vec<u8> {
    for relative in [layout.manifest_path(), layout.metadata_path()] {
        let candidate = root.join(relative);
        if let Ok(bytes) = std::fs::read(&candidate) {
            return bytes;
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{temp_dir, write_bundle};
    use daux_runtime::daux_core::{PluginDescriptor, Version};
    use std::cell::Cell;

    /// A scanner that would otherwise walk the developer's own plug-in folders.
    fn scanner_for(root: &Path) -> Scanner {
        let mut scanner = Scanner::new();
        scanner
            .clear_search_paths()
            .add_search_path(root.to_path_buf());
        scanner.set_probe(false);
        scanner
    }

    thread_local! {
        /// How many times a stand-in prober has been called on this thread. Thread-local so
        /// that tests running in parallel cannot see each other's counts.
        static PROBE_CALLS: Cell<usize> = const { Cell::new(0) };
    }

    fn probe_calls() -> usize {
        PROBE_CALLS.with(Cell::get)
    }

    fn reset_probe_calls() {
        PROBE_CALLS.with(|calls| calls.set(0));
    }

    /// A module that agrees with its own manifest, which is what a correctly built bundle
    /// looks like from the scanner's side.
    fn honest_module(
        bundle: &Bundle,
        _target: &TargetId,
        _timeout: Duration,
    ) -> ScanResult<ProbeOutcome> {
        PROBE_CALLS.with(|calls| calls.set(calls.get() + 1));
        let metadata = bundle.metadata();
        let descriptor = PluginDescriptor::builder(&metadata.id, &metadata.name)
            .vendor(metadata.vendor.clone())
            .version(Version::parse(&metadata.version).unwrap_or_default())
            .build()
            .expect("the fixture's identity is valid");
        Ok(ProbeOutcome {
            descriptors: vec![descriptor],
            abi_version: (metadata.abi_version, 0),
        })
    }

    /// A module that answers to an id its manifest does not declare.
    fn impostor_module(
        _bundle: &Bundle,
        _target: &TargetId,
        _timeout: Duration,
    ) -> ScanResult<ProbeOutcome> {
        PROBE_CALLS.with(|calls| calls.set(calls.get() + 1));
        Ok(ProbeOutcome {
            descriptors: vec![crate::testutil::descriptor(
                "com.somebody.else",
                "Something Else",
                "Someone",
            )],
            abi_version: (1, 0),
        })
    }

    /// A module that never comes back.
    fn hanging_module(
        _bundle: &Bundle,
        _target: &TargetId,
        timeout: Duration,
    ) -> ScanResult<ProbeOutcome> {
        PROBE_CALLS.with(|calls| calls.set(calls.get() + 1));
        Err(ScanError::new(
            ScanErrorKind::Timeout,
            format!("no answer within {} ms", timeout.as_millis()),
        ))
    }

    /// A module that panics — but only the one whose id says so, so a scan of several can
    /// be shown to survive it.
    fn one_rotten_module(
        bundle: &Bundle,
        target: &TargetId,
        timeout: Duration,
    ) -> ScanResult<ProbeOutcome> {
        if bundle.metadata().id.contains("rotten") {
            PROBE_CALLS.with(|calls| calls.set(calls.get() + 1));
            return Err(ScanError::new(
                ScanErrorKind::Panicked,
                "the module panicked in its factory",
            ));
        }
        honest_module(bundle, target, timeout)
    }

    /// A module whose display name does not match the manifest's — a packaging mistake,
    /// not a reason to refuse the plug-in.
    fn renamed_module(
        bundle: &Bundle,
        _target: &TargetId,
        _timeout: Duration,
    ) -> ScanResult<ProbeOutcome> {
        PROBE_CALLS.with(|calls| calls.set(calls.get() + 1));
        let metadata = bundle.metadata();
        let descriptor = PluginDescriptor::builder(&metadata.id, "A Different Name")
            .vendor(metadata.vendor.clone())
            .version(Version::parse(&metadata.version).unwrap_or_default())
            .build()
            .expect("valid");
        Ok(ProbeOutcome {
            descriptors: vec![descriptor],
            abi_version: (metadata.abi_version, 0),
        })
    }

    fn probing_scanner(root: &Path, cache: &Path, prober: Prober) -> Scanner {
        let mut scanner = Scanner::with_cache(cache.to_path_buf());
        scanner
            .clear_search_paths()
            .add_search_path(root.to_path_buf());
        scanner.set_prober(prober);
        scanner
    }

    /// The reason the cache exists, measured: a rescan of an unchanged tree opens nothing.
    #[test]
    fn an_unchanged_bundle_is_never_opened_twice() {
        let dir = temp_dir("cache-skips-load");
        let cache = dir.join("cache").join("scan.json");
        write_bundle(&dir, "com.example.a", "A", "1.0.0");
        write_bundle(&dir, "com.example.b", "B", "1.0.0");
        reset_probe_calls();

        let first = probing_scanner(&dir, &cache, honest_module).scan();
        assert_eq!(first.len(), 2, "{:?}", first.failures());
        assert_eq!(probe_calls(), 2, "a cold cache has to open both");
        assert_eq!(first.stats().probed, 2);
        assert_eq!(first.stats().from_cache, 0);

        // A second process, the same cache file.
        reset_probe_calls();
        let second = probing_scanner(&dir, &cache, honest_module).scan();
        assert_eq!(second.len(), 2);
        assert_eq!(
            probe_calls(),
            0,
            "nothing changed, so nothing may be loaded again"
        );
        assert_eq!(second.stats().from_cache, 2);
        // And the cached entries are indistinguishable from the fresh ones.
        assert_eq!(second.entries()[0].descriptors.len(), 1);
        assert_eq!(second.entries()[0].name(), "A");
        assert!(second.entries()[0].probed);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The other half of the same requirement: a plug-in the developer just rebuilt must
    /// *not* be served from the cache, or every build would appear not to have happened.
    #[test]
    fn a_rebuilt_binary_is_opened_again_and_its_neighbour_is_not() {
        let dir = temp_dir("cache-invalidation");
        let cache = dir.join("scan.json");
        let a = write_bundle(&dir, "com.example.a", "A", "1.0.0");
        write_bundle(&dir, "com.example.b", "B", "1.0.0");

        reset_probe_calls();
        assert_eq!(probing_scanner(&dir, &cache, honest_module).scan().len(), 2);
        assert_eq!(probe_calls(), 2);

        // Rewrite only A's binary, leaving its manifest untouched — the case
        // `manifest-v1` §8.2 requires the fingerprint to catch.
        let binaries = a.join("Content").join(TargetId::host().as_str());
        let binary = std::fs::read_dir(&binaries)
            .expect("a binary directory")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| path.is_file())
            .expect("a binary");
        std::fs::write(
            &binary,
            b"a different build entirely, of a different length",
        )
        .expect("rewrite");

        reset_probe_calls();
        let report = probing_scanner(&dir, &cache, honest_module).scan();
        assert_eq!(report.len(), 2);
        assert_eq!(probe_calls(), 1, "exactly the one that changed");
        assert_eq!(report.stats().from_cache, 1);
        assert_eq!(report.stats().probed, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A hang cannot be resolved, only bounded — and then it must not be paid for twice.
    #[test]
    fn a_hanging_plug_in_is_quarantined_and_skipped_by_the_next_scan() {
        let dir = temp_dir("quarantine-hang");
        let cache = dir.join("scan.json");
        write_bundle(&dir, "com.example.hangs", "Hangs", "1.0.0");
        write_bundle(&dir, "com.example.fine", "Fine", "1.0.0");

        reset_probe_calls();
        let mut scanner = probing_scanner(&dir, &cache, hanging_module);
        scanner.set_probe_timeout(Duration::from_millis(50));
        let first = scanner.scan();
        assert_eq!(first.len(), 0);
        assert_eq!(first.failures().len(), 2);
        assert!(
            first
                .failures()
                .iter()
                .all(|f| f.kind == ScanErrorKind::Timeout)
        );

        // Next scan: both are quarantined, so the prober is never reached — which is the
        // whole point, because the timeout is a fixed cost per hung plug-in per start-up.
        reset_probe_calls();
        let second = probing_scanner(&dir, &cache, honest_module).scan();
        assert_eq!(probe_calls(), 0, "a quarantined bundle must not be opened");
        assert_eq!(second.len(), 0);
        assert_eq!(second.stats().quarantined, 2);
        assert!(
            second.failures().iter().all(|f| f.is_sticky()),
            "{:?}",
            second.failures()
        );

        // And the user can always ask for them back.
        reset_probe_calls();
        let mut scanner = probing_scanner(&dir, &cache, honest_module);
        scanner.clear_quarantine();
        let third = scanner.scan();
        assert_eq!(third.len(), 2, "{:?}", third.failures());
        assert_eq!(probe_calls(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The requirement in one test: the plug-in that fails is the only one that suffers.
    #[test]
    fn a_panicking_plug_in_costs_only_itself() {
        let dir = temp_dir("panic-isolation");
        let cache = dir.join("scan.json");
        write_bundle(&dir, "com.example.first", "First", "1.0.0");
        write_bundle(&dir, "com.example.rotten", "Rotten", "1.0.0");
        write_bundle(&dir, "com.example.last", "Last", "1.0.0");

        reset_probe_calls();
        let report = probing_scanner(&dir, &cache, one_rotten_module).scan();
        let mut names: Vec<&str> = report.entries().iter().map(ScanEntry::name).collect();
        names.sort_unstable();
        assert_eq!(names, ["First", "Last"]);
        assert_eq!(report.failures().len(), 1);
        assert_eq!(report.failures()[0].kind, ScanErrorKind::Panicked);
        assert!(report.failures()[0].path.ends_with("Rotten.axt"));

        // A panic is not cached as a success, so the next scan tries it again — a plug-in
        // fixed by a reinstall must not stay broken because of a remembered verdict.
        reset_probe_calls();
        let second = probing_scanner(&dir, &cache, honest_module).scan();
        assert_eq!(second.len(), 3);
        assert_eq!(probe_calls(), 1, "only the one that never succeeded");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `manifest-v1` §8.1: a bundle whose manifest claims an id the binary does not
    /// implement is not registered, because saved projects are keyed on that id.
    #[test]
    fn a_bundle_that_lies_about_its_identity_is_refused() {
        let dir = temp_dir("identity");
        let cache = dir.join("scan.json");
        write_bundle(&dir, "com.example.gain", "Gain", "1.0.0");

        let report = probing_scanner(&dir, &cache, impostor_module).scan();
        assert_eq!(report.len(), 0, "identity confusion is fatal");
        assert_eq!(report.failures().len(), 1);
        assert_eq!(report.failures()[0].kind, ScanErrorKind::Identity);
        assert!(report.failures()[0].message.contains("DAUX-M100"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Everything that is not identity is recorded on the entry and changes nothing: the
    /// plug-in works, and the packaging bug still reaches someone.
    #[test]
    fn a_non_fatal_disagreement_is_recorded_on_the_entry() {
        let dir = temp_dir("crosscheck-entry");
        let cache = dir.join("scan.json");
        write_bundle(&dir, "com.example.gain", "Gain", "1.0.0");

        let report = probing_scanner(&dir, &cache, renamed_module).scan();
        assert_eq!(report.len(), 1, "{:?}", report.failures());
        let entry = &report.entries()[0];
        assert!(entry.has_errors());
        assert!(
            entry.issues.iter().any(|issue| issue.code == "DAUX-M101"),
            "{:?}",
            entry.issues
        );
        assert_eq!(
            entry.name(),
            "A Different Name",
            "axt-v1 §8.3: once loaded, the binary wins"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_bundle_is_found_described_and_fingerprinted() {
        let dir = temp_dir("scan-basic");
        write_bundle(&dir, "com.example.gain", "Gain", "1.2.3");

        let report = scanner_for(&dir).scan();
        assert_eq!(report.len(), 1, "failures: {:?}", report.failures());
        let entry = &report.entries()[0];
        assert_eq!(entry.id(), "com.example.gain");
        assert_eq!(entry.name(), "Gain");
        assert_eq!(entry.format, PluginFormat::Axt);
        assert!(!entry.probed);
        assert_ne!(entry.fingerprint, 0);
        assert_eq!(report.stats().examined, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The requirement, stated as a test: one bad plug-in must not cost the others.
    #[test]
    fn one_broken_bundle_does_not_stop_the_others_from_being_found() {
        let dir = temp_dir("scan-resilience");
        write_bundle(&dir, "com.example.a", "A", "1.0.0");
        write_bundle(&dir, "com.example.b", "B", "1.0.0");
        write_bundle(&dir, "com.example.c", "C", "1.0.0");

        // A manifest that will not parse, in the middle of the alphabet.
        let broken = dir.join("Broken.axt");
        std::fs::create_dir_all(&broken).expect("mkdir");
        std::fs::write(broken.join("manifest.json"), b"{ this is not json").expect("write");

        // And a directory that claims to be a bundle but is empty.
        let empty = dir.join("Empty.axt");
        std::fs::create_dir_all(&empty).expect("mkdir");

        let report = scanner_for(&dir).scan();
        let mut ids: Vec<&str> = report.entries().iter().map(ScanEntry::id).collect();
        ids.sort_unstable();
        assert_eq!(ids, ["com.example.a", "com.example.b", "com.example.c"]);

        assert_eq!(report.failures().len(), 2, "{:?}", report.failures());
        let kinds: BTreeSet<ScanErrorKind> = report.failures().iter().map(|f| f.kind).collect();
        assert!(kinds.contains(&ScanErrorKind::Metadata));
        assert!(kinds.contains(&ScanErrorKind::NotABundle));
        assert_eq!(report.stats().failed, 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nested_directories_are_walked_and_bundles_are_not_descended_into() {
        let dir = temp_dir("scan-nested");
        let vendor = dir.join("Acme").join("Suite");
        std::fs::create_dir_all(&vendor).expect("mkdir");
        write_bundle(&vendor, "com.acme.reverb", "Reverb", "1.0.0");

        // A decoy inside the bundle's own resource tree: a scan that descended into
        // bundles would find it and report the same plug-in twice.
        let inner = vendor
            .join("Reverb.axt")
            .join("Resources")
            .join("Inner.axt");
        std::fs::create_dir_all(&inner).expect("mkdir");
        std::fs::write(inner.join("manifest.json"), b"{}").expect("write");

        let report = scanner_for(&dir).scan();
        assert_eq!(report.len(), 1, "failures: {:?}", report.failures());
        assert!(report.failures().is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_depth_limit_stops_an_unbounded_walk() {
        let dir = temp_dir("scan-depth");
        let mut deep = dir.clone();
        for level in 0..8 {
            deep = deep.join(format!("level{level}"));
        }
        std::fs::create_dir_all(&deep).expect("mkdir");
        write_bundle(&deep, "com.example.deep", "Deep", "1.0.0");

        let mut scanner = scanner_for(&dir);
        scanner.set_max_depth(2);
        assert_eq!(scanner.scan().len(), 0, "too deep to be found");

        let mut scanner = scanner_for(&dir);
        scanner.set_max_depth(16);
        assert_eq!(scanner.scan().len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn vst3_and_clap_artefacts_are_reported_but_not_opened() {
        let dir = temp_dir("scan-foreign");
        write_bundle(&dir, "com.example.gain", "Gain", "1.0.0");
        std::fs::write(dir.join("Reverb.clap"), b"not really a library").expect("write");
        std::fs::create_dir_all(dir.join("Comp.vst3")).expect("mkdir");

        let report = scanner_for(&dir).scan();
        assert_eq!(report.len(), 1);
        assert_eq!(report.foreign().len(), 2, "{:?}", report.foreign());
        let formats: BTreeSet<PluginFormat> = report.foreign().iter().map(|f| f.format).collect();
        assert!(formats.contains(&PluginFormat::Clap));
        assert!(formats.contains(&PluginFormat::Vst3));
        assert!(report.foreign().iter().all(|f| f.fingerprint != 0));

        // And a format filter really filters.
        let mut scanner = scanner_for(&dir);
        scanner.set_formats(&[PluginFormat::Axt]);
        let filtered = scanner.scan();
        assert_eq!(filtered.len(), 1);
        assert!(filtered.foreign().is_empty());

        let mut scanner = scanner_for(&dir);
        scanner.set_formats(&[PluginFormat::Clap]);
        let only_clap = scanner.scan();
        assert_eq!(only_clap.len(), 0);
        assert_eq!(only_clap.foreign().len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_one_reads_a_bundle_without_a_scanner_and_refuses_what_is_not_one() {
        let dir = temp_dir("scan-one");
        let path = write_bundle(&dir, "com.example.gain", "Gain", "4.5.6");

        let entry = Scanner::scan_one(&path).expect("a well-formed bundle");
        assert_eq!(entry.metadata.version, "4.5.6");
        assert!(!entry.probed);
        assert!(entry.descriptors.is_empty());

        let missing = Scanner::scan_one(&dir.join("Nothing.axt")).expect_err("not there");
        assert!(matches!(
            missing.kind(),
            ScanErrorKind::NotFound | ScanErrorKind::Io | ScanErrorKind::NotABundle
        ));

        let not_a_bundle = Scanner::scan_one(&dir).expect_err("a plain directory");
        assert_eq!(not_a_bundle.kind(), ScanErrorKind::NotABundle);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A bundle that ships a binary for another platform is a normal thing to own, and a
    /// scan must not report it as damage.
    #[test]
    fn a_bundle_for_another_platform_is_reported_as_such_when_probing() {
        let dir = temp_dir("scan-foreign-target");
        let path = write_bundle(&dir, "com.example.gain", "Gain", "1.0.0");

        let mut scanner = Scanner::new();
        scanner.clear_search_paths();
        scanner.set_target(
            TargetId::parse("aix-power64").expect("syntactically valid but never the host"),
        );
        let error = scanner.inspect(&path).expect_err("nothing to load");
        assert_eq!(error.kind(), ScanErrorKind::NoBinaryForTarget);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `manifest-v1` §8.3: a bundle built for a newer generation of the ABI must be
    /// recognised without paying for a `dlopen` that would fail at `abi-v1` §3 anyway.
    #[test]
    fn a_bundle_from_a_future_abi_is_catalogued_but_never_opened() {
        let dir = temp_dir("scan-future-abi");
        let path = crate::testutil::write_bundle_with(
            &dir,
            "com.example.future",
            "Future",
            "1.0.0",
            |manifest| manifest.abi_version = DAUX_ABI_VERSION_MAJOR + 1,
        );

        let mut scanner = Scanner::new();
        scanner.clear_search_paths();
        let entry = scanner
            .inspect(&path)
            .expect("a future bundle is still catalogued");
        assert!(!entry.probed, "its binary must not have been opened");
        assert!(
            entry
                .issues
                .iter()
                .any(|issue| issue.code == "axt.abi.newer"),
            "the user has to be told why it does not load: {:?}",
            entry.issues
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The scan is deterministic: two runs over the same tree produce the same order, so a
    /// host can diff two reports and mean something by the result.
    #[test]
    fn discovery_order_is_stable() {
        let dir = temp_dir("scan-order");
        for name in ["Zeta", "Alpha", "Mu"] {
            write_bundle(
                &dir,
                &format!("com.example.{}", name.to_lowercase()),
                name,
                "1.0.0",
            );
        }

        let first: Vec<PathBuf> = scanner_for(&dir)
            .scan()
            .entries()
            .iter()
            .map(|entry| entry.path.clone())
            .collect();
        let second: Vec<PathBuf> = scanner_for(&dir)
            .scan()
            .entries()
            .iter()
            .map(|entry| entry.path.clone())
            .collect();
        assert_eq!(first, second);
        assert_eq!(first.len(), 3);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_search_path_that_does_not_exist_is_not_a_failure() {
        let mut scanner = Scanner::new();
        scanner
            .clear_search_paths()
            .add_search_path(PathBuf::from("/definitely/not/here"));
        let report = scanner.scan();
        assert!(report.is_empty(), "{:?}", report.failures());
    }

    #[test]
    fn search_paths_are_deduplicated_and_the_defaults_can_be_replaced() {
        let mut scanner = Scanner::new();
        assert!(
            !scanner.search_paths().is_empty(),
            "a new scanner knows where plug-ins live"
        );
        scanner.clear_search_paths();
        assert!(scanner.search_paths().is_empty());

        scanner.add_search_path(PathBuf::from("/opt/plugins"));
        scanner.add_search_path(PathBuf::from("/opt/plugins"));
        assert_eq!(scanner.search_paths().len(), 1);
    }
}
