//! Surviving the plug-in that does not want to be scanned.
//!
//! A scanner runs code it did not write, from four hundred vendors, on a user's machine, at
//! start-up. Some of it is broken. The requirement is not that nothing goes wrong — it is
//! that one bad plug-in never stops the other three hundred and ninety-nine from being
//! found. There are three ways a probe can go wrong and each needs a different mechanism:
//!
//! | Failure | Mechanism | Cost |
//! | --- | --- | --- |
//! | The module panics across the ABI | [`std::panic::catch_unwind`] on the probe thread | none |
//! | The module hangs | the probe runs on its own thread and the scan stops waiting | one leaked thread |
//! | The module kills the process | a journal written before the probe and read on the next run | one crash, once |
//!
//! The third is the one that matters and the only one that cannot be handled in-process:
//! a segmentation fault or an `abort()` in a static initialiser takes the host down with it,
//! and no amount of Rust can catch that. What *can* be done is to make it happen only once.
//! Before each probe the bundle's path and fingerprint go into a journal that is flushed to
//! disk; after the probe the journal is removed. A journal that is still there at the start
//! of the next scan names the bundle that never came back, and that bundle is quarantined —
//! skipped, reported, and not tried again until it changes or the user asks.
//!
//! Fingerprinting the quarantine rather than only the path is what stops it becoming a
//! permanent blacklist: the vendor ships a fix, the fingerprint changes, and the plug-in is
//! tried again without the user having to know this file exists.
//!
//! A hang leaks a thread on purpose. Killing a thread that is stuck inside `dlopen` while
//! holding the loader lock does not make the process healthier — it makes the next `dlopen`
//! deadlock too. Leaking it is the smaller loss.

use std::io::Write;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use daux_bundle::{Bundle, TargetId};
use daux_runtime::daux_core::PluginDescriptor;
use daux_runtime::daux_core::daux_rt::{ThreadClass, set_current_thread_class};
use daux_runtime::daux_host_services::HostServices;
use daux_runtime::{AxtModule, HostBridge, LoadedFactory};

use crate::error::{ScanError, ScanErrorKind, ScanResult};

/// Largest journal or quarantine file this build will read. Both hold one short line per
/// bundle, so anything past this is not a file this crate wrote.
const MAX_JOURNAL_BYTES: u64 = 4 * 1024 * 1024;

/// What opening a module's factory told the scan. [main-thread]
#[derive(Clone, Debug)]
pub(crate) struct ProbeOutcome {
    /// Every plug-in the factory publishes, in publication order.
    pub(crate) descriptors: Vec<PluginDescriptor>,
    /// The `(major, minor)` ABI version the module was built against.
    pub(crate) abi_version: (u32, u32),
}

/// The persistent record of plug-ins that took the scanner down. [main-thread]
///
/// A [`Quarantine`] with no path still isolates panics and hangs — those need no file — but
/// cannot survive a crash, because there is nowhere to write the journal. That is the shape
/// [`Scanner::new`](crate::Scanner::new) uses; a host that wants crash isolation across
/// restarts gives the scanner a cache path.
#[derive(Debug, Default)]
pub(crate) struct Quarantine {
    /// Where the in-flight probe is recorded, and beside it the quarantine list.
    journal: Option<PathBuf>,
    quarantine_file: Option<PathBuf>,
    /// `(path, fingerprint)` of every bundle currently quarantined.
    entries: Vec<(String, u64)>,
    /// Bundles this scan found named in a leftover journal — they crashed last time.
    crashed: Vec<PathBuf>,
}

impl Quarantine {
    /// An isolation policy with no persistence. [main-thread]
    pub(crate) fn in_memory() -> Self {
        Self::default()
    }

    /// Opens the journal beside `cache_path`, promoting anything left in it. [main-thread]
    ///
    /// A leftover journal means the last scan started a probe and never finished it, which
    /// can only happen if the process died. Those bundles move into the quarantine list
    /// immediately, before anything else is touched.
    pub(crate) fn beside(cache_path: &Path) -> Self {
        let journal = sibling(cache_path, ".inflight");
        let quarantine_file = sibling(cache_path, ".quarantine");
        let mut this = Self {
            entries: read_entries(&quarantine_file),
            journal: Some(journal.clone()),
            quarantine_file: Some(quarantine_file),
            crashed: Vec::new(),
        };

        for (path, fingerprint) in read_entries(&journal) {
            this.crashed.push(PathBuf::from(&path));
            if !this.entries.iter().any(|(known, _)| *known == path) {
                this.entries.push((path, fingerprint));
            }
        }
        if !this.crashed.is_empty() {
            this.write_quarantine();
        }
        // The journal has been consumed; leaving it would quarantine the same bundle twice.
        let _ = std::fs::remove_file(&journal);
        this
    }

    /// The bundles that took the scanner down since the last scan. [main-thread]
    pub(crate) fn crashed(&self) -> &[PathBuf] {
        &self.crashed
    }

    /// Whether `path` is quarantined at this fingerprint. [main-thread]
    ///
    /// A bundle whose fingerprint has changed is *not* quarantined: the vendor may have
    /// shipped the fix, and a blacklist a user cannot escape without deleting a file they
    /// have never heard of is worse than one crash.
    pub(crate) fn is_quarantined(&self, path: &Path, fingerprint: u64) -> bool {
        let key = path.to_string_lossy();
        self.entries
            .iter()
            .any(|(known, known_fingerprint)| *known == key && *known_fingerprint == fingerprint)
    }

    /// Drops `path` from the quarantine, so the next scan tries it again. [main-thread]
    pub(crate) fn forget(&mut self, path: &Path) {
        let key = path.to_string_lossy();
        let before = self.entries.len();
        self.entries.retain(|(known, _)| *known != key);
        if self.entries.len() != before {
            self.write_quarantine();
        }
    }

    /// Empties the quarantine. [main-thread]
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.crashed.clear();
        self.write_quarantine();
    }

    /// Records that a probe of `path` is about to start. [main-thread]
    ///
    /// The write is flushed and `fsync`ed: a journal still sitting in the page cache when
    /// the process dies is a journal that never existed.
    fn begin(&self, path: &Path, fingerprint: u64) {
        let Some(journal) = &self.journal else {
            return;
        };
        if let Ok(mut file) = std::fs::File::create(journal) {
            let _ = writeln!(file, "{fingerprint:016x}\t{}", path.to_string_lossy());
            let _ = file.flush();
            let _ = file.sync_all();
        }
    }

    /// Records that the probe finished — however it finished. [main-thread]
    fn end(&self) {
        if let Some(journal) = &self.journal {
            let _ = std::fs::remove_file(journal);
        }
    }

    /// Adds `path` to the quarantine list without waiting for a crash. [main-thread]
    ///
    /// Used for a hang: the thread is still stuck in the module, so trying it again next
    /// start-up would cost another timeout for the same certain failure.
    pub(crate) fn quarantine(&mut self, path: &Path, fingerprint: u64) {
        let key = path.to_string_lossy().into_owned();
        if !self.entries.iter().any(|(known, _)| *known == key) {
            self.entries.push((key, fingerprint));
            self.write_quarantine();
        }
    }

    fn write_quarantine(&self) {
        let Some(file) = &self.quarantine_file else {
            return;
        };
        let mut text = String::new();
        for (path, fingerprint) in &self.entries {
            text.push_str(&format!("{fingerprint:016x}\t{path}\n"));
        }
        if text.is_empty() {
            let _ = std::fs::remove_file(file);
        } else {
            if let Some(parent) = file.parent()
                && !parent.as_os_str().is_empty()
            {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(file, text);
        }
    }

    /// Runs `probe` with the journal open around it. [main-thread]
    pub(crate) fn guard<T>(&self, path: &Path, fingerprint: u64, probe: impl FnOnce() -> T) -> T {
        self.begin(path, fingerprint);
        let outcome = probe();
        self.end();
        outcome
    }
}

/// `path` with `suffix` appended to its file name.
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

/// Reads a `fingerprint\tpath` file, ignoring anything malformed.
fn read_entries(path: &Path) -> Vec<(String, u64)> {
    let Ok(metadata) = std::fs::metadata(path) else {
        return Vec::new();
    };
    if metadata.len() > MAX_JOURNAL_BYTES {
        return Vec::new();
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            let (fingerprint, path) = line.split_once('\t')?;
            let fingerprint = u64::from_str_radix(fingerprint.trim(), 16).ok()?;
            (!path.is_empty()).then(|| (path.to_owned(), fingerprint))
        })
        .collect()
}

/// Opens the module for `bundle` and enumerates its factory, on a thread of its own.
/// [main-thread]
///
/// # Errors
///
/// [`ScanErrorKind::Panicked`] when the module panicked across the boundary,
/// [`ScanErrorKind::Timeout`] when it did not answer within `timeout`, and whatever the
/// runtime reported otherwise.
pub(crate) fn probe_isolated(
    bundle: &Bundle,
    target: &TargetId,
    timeout: Duration,
) -> ScanResult<ProbeOutcome> {
    let (sender, receiver) = mpsc::channel();
    let owned_bundle = bundle.clone();
    let owned_target = target.clone();
    let path = bundle.path().to_path_buf();

    let spawned = std::thread::Builder::new()
        .name("daux-scan-probe".to_owned())
        .spawn(move || {
            set_current_thread_class(ThreadClass::Scanner);
            // A module that panics across the ABI boundary is a broken module, not a broken
            // scanner. `abi-v1` §17 makes it the plug-in's job to catch its own panics; this
            // is the second net, for the ones that do not.
            let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
                enumerate(&owned_bundle, &owned_target)
            }))
            .unwrap_or_else(|_| {
                Err(ScanError::new(
                    ScanErrorKind::Panicked,
                    "the module panicked while its factory was being enumerated",
                ))
            });
            // The receiver is gone if the scan already gave up on this bundle, which is
            // exactly the timeout case; there is nobody left to tell.
            let _ = sender.send(outcome);
        });

    match spawned {
        Ok(_handle) => match receiver.recv_timeout(timeout) {
            Ok(outcome) => outcome.map_err(|error| error.or_path(&path)),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(ScanError::new(
                ScanErrorKind::Timeout,
                format!(
                    "the module did not finish loading within {} ms; its thread is abandoned \
                     rather than killed, because a thread stuck inside the dynamic loader \
                     still holds the loader lock",
                    timeout.as_millis()
                ),
            )
            .with_path(&path)),
            // The thread died without sending, which after `catch_unwind` means the runtime
            // itself aborted the thread. Nothing came back, so nothing is known.
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(ScanError::new(
                ScanErrorKind::Panicked,
                "the probe thread ended without an answer",
            )
            .with_path(&path)),
        },
        // No thread available: a host under memory pressure still deserves a scan, so the
        // probe runs inline. Panics are still caught; a hang is not survivable this way,
        // which is why the thread is the preferred path.
        Err(_) => std::panic::catch_unwind(AssertUnwindSafe(|| enumerate(bundle, target)))
            .unwrap_or_else(|_| {
                Err(ScanError::new(
                    ScanErrorKind::Panicked,
                    "the module panicked while its factory was being enumerated",
                ))
            })
            .map_err(|error| error.or_path(&path)),
    }
}

/// The part that actually runs foreign code.
fn enumerate(bundle: &Bundle, target: &TargetId) -> ScanResult<ProbeOutcome> {
    let module = Arc::new(AxtModule::load(bundle, target)?);
    let abi_version = module.abi_version();
    // A scanner offers a plug-in nothing: it is not going to answer a callback, and a
    // module that needs a real host to be enumerated is a module that violates `abi-v1` §5.
    let factory = LoadedFactory::create(module, HostBridge::new(HostServices::null()))?;
    let descriptors = factory.descriptors()?;
    Ok(ProbeOutcome {
        descriptors,
        abi_version,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::temp_dir;

    #[test]
    fn a_quarantine_without_a_file_still_works_and_forgets_nothing() {
        let mut quarantine = Quarantine::in_memory();
        assert!(quarantine.crashed().is_empty());
        assert_eq!(quarantine.entries.len(), 0);
        quarantine.quarantine(Path::new("/p/Crasher.axt"), 7);
        assert!(quarantine.is_quarantined(Path::new("/p/Crasher.axt"), 7));
        // The journal is a no-op, so guarding must not fail or panic.
        let value = quarantine.guard(Path::new("/p/Crasher.axt"), 7, || 42);
        assert_eq!(value, 42);
    }

    /// The whole point of the journal: a probe that never returned names the bundle that
    /// killed the process, and that bundle is skipped next time.
    #[test]
    fn a_leftover_journal_becomes_a_quarantine_on_the_next_scan() {
        let dir = temp_dir("quarantine-journal");
        let cache = dir.join("scan-cache.json");

        // Simulate a process that died inside a probe: the journal is written and never
        // removed.
        {
            let quarantine = Quarantine::beside(&cache);
            quarantine.begin(Path::new("/p/Crasher.axt"), 0x1234);
            assert!(sibling(&cache, ".inflight").is_file());
        }

        let next = Quarantine::beside(&cache);
        assert_eq!(next.crashed(), [PathBuf::from("/p/Crasher.axt")]);
        assert!(next.is_quarantined(Path::new("/p/Crasher.axt"), 0x1234));
        assert!(
            !sibling(&cache, ".inflight").exists(),
            "the journal is consumed, or the same crash would be reported forever"
        );

        // And it survives into the scan after that, without being reported as a new crash.
        let third = Quarantine::beside(&cache);
        assert!(
            third.crashed().is_empty(),
            "the crash was already accounted for"
        );
        assert!(third.is_quarantined(Path::new("/p/Crasher.axt"), 0x1234));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A quarantine keyed only on the path would be a blacklist no user could escape. The
    /// fingerprint is what lets a fixed plug-in come back on its own.
    #[test]
    fn a_changed_bundle_leaves_the_quarantine_by_itself() {
        let dir = temp_dir("quarantine-fingerprint");
        let cache = dir.join("scan-cache.json");
        let mut quarantine = Quarantine::beside(&cache);
        quarantine.quarantine(Path::new("/p/Crasher.axt"), 0x1111);

        assert!(quarantine.is_quarantined(Path::new("/p/Crasher.axt"), 0x1111));
        assert!(
            !quarantine.is_quarantined(Path::new("/p/Crasher.axt"), 0x2222),
            "a new build of the same plug-in must be given a chance"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_probe_that_finishes_leaves_no_journal_behind() {
        let dir = temp_dir("quarantine-clean");
        let cache = dir.join("scan-cache.json");
        let quarantine = Quarantine::beside(&cache);

        let answer = quarantine.guard(Path::new("/p/Fine.axt"), 9, || "done");
        assert_eq!(answer, "done");
        assert!(!sibling(&cache, ".inflight").exists());

        let next = Quarantine::beside(&cache);
        assert!(
            next.crashed().is_empty(),
            "a plug-in that loaded must never be quarantined"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Even a probe that panics must clear the journal — a panic is survivable, so
    /// treating it as a crash would quarantine a plug-in that merely misbehaved.
    #[test]
    fn a_probe_that_fails_still_clears_the_journal() {
        let dir = temp_dir("quarantine-failure");
        let cache = dir.join("scan-cache.json");
        let quarantine = Quarantine::beside(&cache);

        let outcome: Result<(), &str> = quarantine.guard(Path::new("/p/Bad.axt"), 3, || Err("no"));
        assert!(outcome.is_err());
        assert!(!sibling(&cache, ".inflight").exists());
        assert!(Quarantine::beside(&cache).crashed().is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn forgetting_and_clearing_bring_a_plug_in_back() {
        let dir = temp_dir("quarantine-forget");
        let cache = dir.join("scan-cache.json");
        let mut quarantine = Quarantine::beside(&cache);
        quarantine.quarantine(Path::new("/p/A.axt"), 1);
        quarantine.quarantine(Path::new("/p/B.axt"), 2);
        assert_eq!(quarantine.entries.len(), 2);

        quarantine.forget(Path::new("/p/A.axt"));
        assert_eq!(quarantine.entries.len(), 1);
        assert!(!quarantine.is_quarantined(Path::new("/p/A.axt"), 1));
        // The change is on disk, not only in memory.
        assert_eq!(Quarantine::beside(&cache).entries.len(), 1);

        quarantine.clear();
        assert_eq!(quarantine.entries.len(), 0);
        assert!(
            !sibling(&cache, ".quarantine").exists(),
            "an empty quarantine leaves no file"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The quarantine file is in a user-writable directory, so it is untrusted input.
    #[test]
    fn a_corrupt_quarantine_file_is_ignored_rather_than_believed() {
        let dir = temp_dir("quarantine-corrupt");
        let cache = dir.join("scan-cache.json");
        std::fs::write(
            sibling(&cache, ".quarantine"),
            "not a fingerprint\t/p/A.axt\n\n\tempty\nzzzz\t/p/B.axt\n00000000000000ff\t/p/C.axt\n",
        )
        .expect("write");

        let quarantine = Quarantine::beside(&cache);
        assert_eq!(
            quarantine.entries.len(),
            1,
            "only the well-formed line survives"
        );
        assert!(quarantine.is_quarantined(Path::new("/p/C.axt"), 0xff));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A hang is the one failure the scanner cannot resolve, so it must at least be
    /// bounded: the call returns, with a timeout, in about the time it was given.
    #[test]
    fn a_probe_of_a_bundle_with_no_binary_fails_fast_rather_than_timing_out() {
        use crate::testutil::bundle_without_binary;

        let dir = temp_dir("probe-no-binary");
        let (bundle, _path) = bundle_without_binary(&dir, "com.example.gain", "Gain");
        let started = std::time::Instant::now();
        let error = probe_isolated(&bundle, &TargetId::host(), Duration::from_secs(30))
            .expect_err("there is nothing to load");
        assert_eq!(error.kind(), ScanErrorKind::NoBinaryForTarget);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "a missing binary must be reported immediately, not waited out"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file that is not a dynamic library is what a truncated download looks like. The
    /// operating system refuses it, and the scan has to survive the refusal.
    #[test]
    fn a_binary_that_is_not_a_library_is_reported_and_survived() {
        use crate::testutil::bundle_with_fake_binary;

        let dir = temp_dir("probe-not-a-library");
        let (bundle, _path) = bundle_with_fake_binary(&dir, "com.example.gain", "Gain");
        let error = probe_isolated(&bundle, &TargetId::host(), Duration::from_secs(30))
            .expect_err("a text file is not a module");
        assert!(
            matches!(
                error.kind(),
                ScanErrorKind::Load | ScanErrorKind::NoBinaryForTarget
            ),
            "unexpected kind: {error}"
        );
        assert!(error.path().is_some(), "a failure must name the bundle");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
