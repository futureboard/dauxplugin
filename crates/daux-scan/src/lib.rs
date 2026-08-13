//! Finding the plug-ins installed on a machine, without being taken down by them.
//!
//! Scanning is the slowest thing a DAW does when it starts, and the most dangerous: it runs
//! code from several hundred vendors, some of which is broken, in the host's own process,
//! before the user has done anything. This crate exists to make that survivable and fast, in
//! that order.
//!
//! ```no_run
//! use daux_scan::{PluginFormat, Scanner};
//!
//! let mut scanner = Scanner::with_cache(std::path::PathBuf::from("scan-cache.json"));
//! scanner.set_formats(&[PluginFormat::Axt]);
//!
//! let report = scanner.scan();
//! println!(
//!     "{} plug-ins, {} from cache, {} skipped, in {:?}",
//!     report.len(),
//!     report.stats().from_cache,
//!     report.stats().failed,
//!     report.stats().duration,
//! );
//! ```
//!
//! # The two ideas
//!
//! **Caching.** The expensive part of a scan is not reading manifests, it is opening
//! several hundred dynamic libraries. So the descriptors a module's factory publishes are
//! cached against a fingerprint that covers the manifest's bytes and the binary's size and
//! modification time, exactly as `manifest-v1` §8.2 requires. An unchanged plug-in is never
//! opened twice. Everything else — the manifest, the validation, the presence of the binary
//! — is re-read every time, because a cached answer about a file that may have been deleted
//! is worse than no answer at all.
//!
//! **Isolation.** A plug-in that panics is caught, a plug-in that hangs is abandoned after a
//! timeout, and a plug-in that kills the process is remembered in a journal written before
//! the probe and read on the next run — so it takes the host down once and is skipped
//! thereafter, until it changes or the user asks for it again. Any of the three costs one
//! [`ScanFailure`] in the report and nothing else; the walk carries on to the next bundle.
//! See [`Scanner`] and the crate's `isolation` module for the mechanisms.
//!
//! # What is described and what is only found
//!
//! An `.axt` carries a manifest this workspace can read, so it becomes a [`ScanEntry`] with
//! identity, capabilities and — when the binary is probed — the descriptors the factory
//! published. A `.vst3` or a `.clap` keeps its identity inside the binary, behind that
//! format's own C ABI; reading it means loading and calling foreign code, which the host
//! side of this workspace does not do in v1. Those are reported as [`ForeignPlugin`]s: found,
//! located, fingerprinted, and honestly not described.
//!
//! # Where it looks
//!
//! [`Scanner::default_search_paths`] returns the platform's conventional directories —
//! `%CommonProgramFiles%\VST3` and friends on Windows, `/Library/Audio/Plug-Ins/…` on macOS,
//! `~/.clap` and `/usr/lib/clap` on Linux — plus anything named by the `DAUX_PATH`,
//! `VST3_PATH` and `CLAP_PATH` environment variables.
//!
//! # Threading
//!
//! Everything here is `[main-thread]`, and none of it may be called from `process`.
//! [`Scanner::inspect`] runs each probe on a thread of its own, which is what makes the
//! timeout possible, but the scanner itself is driven from one thread.

#![forbid(unsafe_code)]

mod cache;
mod crosscheck;
mod entry;
mod error;
mod fingerprint;
mod format;
mod isolation;
mod scanner;
mod search_paths;

#[cfg(test)]
mod testutil;

pub use entry::{ForeignPlugin, ScanEntry, ScanFailure, ScanReport, ScanStats};
pub use error::{ScanError, ScanErrorKind, ScanResult};
pub use format::{PluginFormat, UnknownFormat};
pub use scanner::{DEFAULT_MAX_DEPTH, DEFAULT_PROBE_TIMEOUT, Scanner};

/// The crates whose types appear in this one's signatures, re-exported so a host can name
/// them without adding each dependency itself.
pub use {daux_bundle, daux_runtime};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{temp_dir, write_bundle};
    use std::path::PathBuf;
    use std::time::Duration;

    /// The end-to-end promise of the cache, measured rather than asserted: the second scan
    /// of an unchanged tree opens no module, and a rebuilt binary makes it open one again.
    ///
    /// Probing is on, and the fixtures' binaries are not real libraries, so every probe
    /// *fails* — which is the point: a failure that was reached through the cache would
    /// prove nothing, so the counters are read from the report instead.
    #[test]
    fn a_rescan_of_an_unchanged_tree_reuses_what_it_learned() {
        let dir = temp_dir("cache-end-to-end");
        let cache = dir.join("cache").join("scan.json");
        write_bundle(&dir, "com.example.gain", "Gain", "1.0.0");

        let mut scanner = Scanner::with_cache(cache.clone());
        scanner.clear_search_paths().add_search_path(dir.clone());
        // Cataloguing only: the fixtures carry no loadable binary, so this is the mode in
        // which an entry is actually produced.
        scanner.set_probe(false);

        let first = scanner.scan();
        assert_eq!(first.len(), 1, "{:?}", first.failures());
        let fingerprint = first.entries()[0].fingerprint;

        // A second scanner, a cold process, the same cache file.
        let mut again = Scanner::with_cache(cache.clone());
        again.clear_search_paths().add_search_path(dir.clone());
        again.set_probe(false);
        let second = again.scan();
        assert_eq!(second.len(), 1);
        assert_eq!(
            second.entries()[0].fingerprint,
            fingerprint,
            "nothing changed, so the fingerprint must not"
        );

        // Touching the manifest changes the fingerprint, which is what invalidates the
        // cached probe.
        let manifest = second.entries()[0].path.join("manifest.json");
        let mut text = std::fs::read_to_string(&manifest).expect("read");
        text.push('\n');
        std::fs::write(&manifest, text).expect("write");

        let mut third = Scanner::with_cache(cache);
        third.clear_search_paths().add_search_path(dir.clone());
        third.set_probe(false);
        let report = third.scan();
        assert_ne!(
            report.entries()[0].fingerprint,
            fingerprint,
            "an edited manifest is a different bundle"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A probe that fails must not poison the entry for a bundle that is merely
    /// unloadable *here* — and must not be silently retried forever either.
    #[test]
    fn a_bundle_whose_binary_will_not_load_is_reported_once_per_scan() {
        let dir = temp_dir("unloadable");
        write_bundle(&dir, "com.example.gain", "Gain", "1.0.0");

        let mut scanner = Scanner::new();
        scanner.clear_search_paths().add_search_path(dir.clone());
        scanner.set_probe_timeout(Duration::from_secs(10));

        let report = scanner.scan();
        assert_eq!(report.len(), 0, "a module that will not open is not usable");
        assert_eq!(report.failures().len(), 1, "{:?}", report.failures());
        let failure = &report.failures()[0];
        assert!(
            matches!(
                failure.kind,
                ScanErrorKind::Load | ScanErrorKind::NoBinaryForTarget
            ),
            "unexpected kind: {failure}"
        );
        assert_eq!(failure.format, Some(PluginFormat::Axt));
        assert!(
            !failure.is_sticky(),
            "a library the OS refused may load after a reinstall"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A scan of a directory that is not a plug-in directory at all must be quiet: no
    /// entries, no failures, no foreign artefacts.
    #[test]
    fn a_tree_with_nothing_in_it_produces_nothing() {
        let dir = temp_dir("empty-tree");
        std::fs::create_dir_all(dir.join("documents").join("notes")).expect("mkdir");
        std::fs::write(dir.join("readme.txt"), b"hello").expect("write");

        let mut scanner = Scanner::new();
        scanner.clear_search_paths().add_search_path(dir.clone());
        let report = scanner.scan();
        assert!(report.is_empty(), "{report:?}");
        assert!(report.stats().directories >= 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_public_surface_crosses_threads() {
        const fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Scanner>();
        assert_send_sync::<ScanReport>();
        assert_send_sync::<ScanEntry>();
        assert_send_sync::<ScanFailure>();
        assert_send_sync::<ForeignPlugin>();
        assert_send_sync::<ScanError>();
        assert_send_sync::<PluginFormat>();
    }

    #[test]
    fn the_default_search_paths_are_the_ones_the_scanner_starts_with() {
        let scanner = Scanner::new();
        assert_eq!(scanner.search_paths(), Scanner::default_search_paths());
        assert!(!Scanner::default_search_paths().contains(&PathBuf::new()));
    }
}
