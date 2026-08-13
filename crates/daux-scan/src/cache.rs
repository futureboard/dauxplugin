//! The scan cache: what makes the second start-up fast.
//!
//! Scanning is the slowest thing a DAW does when it launches, and almost all of that cost is
//! one operation: opening several hundred dynamic libraries, running their static
//! initialisers, creating a factory in each and enumerating it. Reading a manifest is a
//! few kilobytes off a warm page cache; `dlopen` is page faults, relocations, dependency
//! resolution and whatever the vendor decided to do in a constructor.
//!
//! So this cache stores exactly that one expensive result — the descriptors a module's
//! factory published — keyed on a fingerprint that changes whenever the bundle does. It
//! deliberately does **not** cache the manifest or the validation issues:
//!
//! * re-reading a manifest is cheap, and re-parsing it means a cached entry and a fresh one
//!   are byte-identical rather than nearly so;
//! * validation touches the filesystem, so a cached verdict could claim a binary exists
//!   long after someone deleted it.
//!
//! A cache is an optimisation, never an authority. Nothing here is trusted: a record that
//! does not deserialise, does not validate, or does not match the fingerprint is a miss, and
//! a miss costs a scan, not a failure.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use daux_runtime::daux_core::daux_audio::SampleFormats;
use daux_runtime::daux_core::{Capabilities, Category, PluginDescriptor, Version};
use serde::{Deserialize, Serialize};

use crate::error::{ScanError, ScanErrorKind, ScanResult};
use crate::format::PluginFormat;

/// On-disk format version. A file written by another version is discarded, not migrated:
/// the cost of being wrong is a slow start-up, and the cost of a migration bug is a host
/// that believes a plug-in has parameters it does not.
const CACHE_VERSION: u32 = 1;

/// Largest cache file this build will read.
///
/// A cache lives in a user-writable directory, so its size is untrusted input. Refusing to
/// read a suspiciously large one costs one slow scan.
const MAX_CACHE_BYTES: u64 = 32 * 1024 * 1024;

/// Largest number of records this build will keep.
///
/// Two hundred thousand plug-ins is far past any real installation and still bounds the
/// memory a tampered file can make the host allocate.
const MAX_RECORDS: usize = 200_000;

/// What a cache hit gives back. [main-thread]
#[derive(Clone, Debug)]
pub(crate) struct CachedProbe {
    /// The descriptors the module's factory published when it was last opened.
    pub(crate) descriptors: Vec<PluginDescriptor>,
    /// Whether the binary was actually opened for that scan.
    pub(crate) probed: bool,
    /// When the bundle was really read.
    pub(crate) scanned_at: SystemTime,
}

/// A serialisable mirror of [`PluginDescriptor`].
///
/// `daux-core` has no dependencies at all — that is one of the workspace's hard rules — so
/// it cannot derive `serde` traits, and this crate cannot ask it to. The mirror is the
/// price, and it is worth paying: the alternative is a `serde` dependency in the crate every
/// plug-in links against.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct CachedDescriptor {
    id: String,
    name: String,
    #[serde(default)]
    vendor: String,
    /// `[major, minor, patch, build]`.
    #[serde(default)]
    version: [u32; 4],
    #[serde(default)]
    description: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    support_url: String,
    #[serde(default)]
    copyright: String,
    #[serde(default)]
    license: String,
    /// `DAUX_CATEGORY_*`, which is total in both directions.
    #[serde(default)]
    category: u32,
    #[serde(default)]
    capabilities: u64,
    #[serde(default)]
    features: Vec<String>,
    #[serde(default)]
    sample_formats: u32,
    #[serde(default)]
    state_schema_version: u32,
    #[serde(default)]
    min_abi: [u32; 2],
}

impl CachedDescriptor {
    fn from_descriptor(descriptor: &PluginDescriptor) -> Self {
        let (major, minor, patch, build) = descriptor.version.to_parts();
        Self {
            id: descriptor.id.as_str().to_owned(),
            name: descriptor.name.clone(),
            vendor: descriptor.vendor.clone(),
            version: [major, minor, patch, build],
            description: descriptor.description.clone(),
            url: descriptor.url.clone(),
            support_url: descriptor.support_url.clone(),
            copyright: descriptor.copyright.clone(),
            license: descriptor.license.clone(),
            category: descriptor.category.code(),
            capabilities: descriptor.capabilities.bits(),
            features: descriptor.features.clone(),
            sample_formats: descriptor.sample_formats.bits(),
            state_schema_version: descriptor.state_schema_version,
            min_abi: [descriptor.min_abi.0, descriptor.min_abi.1],
        }
    }

    /// Rebuilds the descriptor, or fails if the record does not describe a usable plug-in.
    ///
    /// The builder validates, so a hand-edited cache cannot inject a descriptor the rest of
    /// the workspace would reject — an empty name, an id that is not reverse-DNS, a plug-in
    /// that claims not to support `f32`.
    fn to_descriptor(&self) -> ScanResult<PluginDescriptor> {
        let [major, minor, patch, build] = self.version;
        PluginDescriptor::builder(&self.id, &self.name)
            .vendor(self.vendor.clone())
            .version(Version::from_parts((major, minor, patch, build)))
            .description(self.description.clone())
            .url(self.url.clone())
            .support_url(self.support_url.clone())
            .copyright(self.copyright.clone())
            .license(self.license.clone())
            .category(Category::from_code(self.category))
            .capabilities(Capabilities::from_bits(self.capabilities))
            .features(self.features.clone())
            .sample_formats(SampleFormats::from_bits_truncate(self.sample_formats))
            .state_schema_version(self.state_schema_version)
            .min_abi(self.min_abi[0], self.min_abi[1])
            .build()
            .map_err(|error| {
                ScanError::new(
                    ScanErrorKind::Cache,
                    format!("cached descriptor `{}` is not usable: {error}", self.id),
                )
            })
    }
}

/// One cached artefact.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct CacheRecord {
    path: String,
    fingerprint: u64,
    format: PluginFormat,
    /// Seconds since the Unix epoch. Split from the nanoseconds so the file stays readable
    /// and so a clock that only has second resolution does not produce a lopsided value.
    scanned_at_secs: u64,
    #[serde(default)]
    scanned_at_nanos: u32,
    #[serde(default)]
    probed: bool,
    #[serde(default)]
    descriptors: Vec<CachedDescriptor>,
}

/// The cache file as written.
#[derive(Debug, Serialize, Deserialize)]
struct CacheFile {
    version: u32,
    records: Vec<CacheRecord>,
}

/// The scan cache, in memory and optionally on disk. [main-thread]
///
/// A cache with no path is a perfectly good cache for the lifetime of one process; that is
/// what [`Scanner::new`](crate::Scanner::new) uses, and it still saves the second scan of a
/// session.
#[derive(Debug, Default)]
pub(crate) struct ScanCache {
    path: Option<PathBuf>,
    records: BTreeMap<String, CacheRecord>,
    dirty: bool,
}

impl ScanCache {
    /// An in-memory cache that is never written. [main-thread]
    pub(crate) fn in_memory() -> Self {
        Self::default()
    }

    /// A cache backed by `path`. Nothing is read until [`ScanCache::load`] is called.
    /// [main-thread]
    pub(crate) fn at(path: PathBuf) -> Self {
        Self {
            path: Some(path),
            records: BTreeMap::new(),
            dirty: false,
        }
    }

    /// How many records are held. [main-thread]
    pub(crate) fn len(&self) -> usize {
        self.records.len()
    }

    /// Reads the cache file. [main-thread]
    ///
    /// A file that is absent is not an error — it is the first run. A file that is present
    /// but unreadable, oversized, truncated or from another version *is* reported, because
    /// a user whose cache silently never works deserves to be told why, and the scan itself
    /// carries on regardless.
    ///
    /// # Errors
    ///
    /// [`ScanErrorKind::Cache`] for anything the file itself got wrong, and
    /// [`ScanErrorKind::Io`] when the filesystem refused the read.
    pub(crate) fn load(&mut self) -> ScanResult<()> {
        let Some(path) = self.path.clone() else {
            return Ok(());
        };
        let metadata = match std::fs::metadata(&path) {
            Ok(metadata) => metadata,
            // Nothing there: a cold cache, which is the normal state on first run.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(ScanError::new(ScanErrorKind::Io, error.to_string()).with_path(path));
            }
        };
        if metadata.len() > MAX_CACHE_BYTES {
            return Err(ScanError::new(
                ScanErrorKind::Cache,
                format!(
                    "the cache is {} bytes, above the {MAX_CACHE_BYTES}-byte limit",
                    metadata.len()
                ),
            )
            .with_path(path));
        }

        let bytes = std::fs::read(&path).map_err(|error| {
            ScanError::new(ScanErrorKind::Io, error.to_string()).with_path(&path)
        })?;
        let file: CacheFile = serde_json::from_slice(&bytes).map_err(|error| {
            ScanError::new(ScanErrorKind::Cache, format!("unreadable cache: {error}"))
                .with_path(&path)
        })?;
        if file.version != CACHE_VERSION {
            return Err(ScanError::new(
                ScanErrorKind::Cache,
                format!(
                    "cache format {} is not {CACHE_VERSION}; it will be rebuilt",
                    file.version
                ),
            )
            .with_path(path));
        }
        if file.records.len() > MAX_RECORDS {
            return Err(ScanError::new(
                ScanErrorKind::Cache,
                format!(
                    "the cache holds {} records, above the {MAX_RECORDS} limit",
                    file.records.len()
                ),
            )
            .with_path(path));
        }

        self.records = file
            .records
            .into_iter()
            .map(|record| (record.path.clone(), record))
            .collect();
        self.dirty = false;
        Ok(())
    }

    /// The cached probe for `path`, if it is still valid. [main-thread]
    ///
    /// A record whose fingerprint disagrees is a miss, and so is one whose descriptors no
    /// longer rebuild — a cache is never allowed to make a scan produce something a fresh
    /// scan could not.
    pub(crate) fn lookup(&self, path: &Path, fingerprint: u64) -> Option<CachedProbe> {
        let key = key_for(path)?;
        let record = self.records.get(&key)?;
        if record.fingerprint != fingerprint {
            return None;
        }
        let mut descriptors = Vec::with_capacity(record.descriptors.len());
        for cached in &record.descriptors {
            descriptors.push(cached.to_descriptor().ok()?);
        }
        Some(CachedProbe {
            descriptors,
            probed: record.probed,
            scanned_at: UNIX_EPOCH
                + Duration::new(
                    record.scanned_at_secs,
                    record.scanned_at_nanos.min(999_999_999),
                ),
        })
    }

    /// Records what a scan found. [main-thread]
    ///
    /// A path that is not valid UTF-8 is not cached: the key would have to be lossy, and two
    /// different bundles that differ only in the bytes that were lost would then share an
    /// entry. Such a bundle is simply scanned every time, which is correct but slow.
    pub(crate) fn store(
        &mut self,
        path: &Path,
        fingerprint: u64,
        format: PluginFormat,
        scanned_at: SystemTime,
        probed: bool,
        descriptors: &[PluginDescriptor],
    ) {
        let Some(key) = key_for(path) else {
            return;
        };
        if self.records.len() >= MAX_RECORDS && !self.records.contains_key(&key) {
            return;
        }
        let since_epoch = scanned_at
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO);
        self.records.insert(
            key.clone(),
            CacheRecord {
                path: key,
                fingerprint,
                format,
                scanned_at_secs: since_epoch.as_secs(),
                scanned_at_nanos: since_epoch.subsec_nanos(),
                probed,
                descriptors: descriptors
                    .iter()
                    .map(CachedDescriptor::from_descriptor)
                    .collect(),
            },
        );
        self.dirty = true;
    }

    /// Drops records for artefacts this scan did not see. [main-thread]
    ///
    /// Two conditions, and both matter:
    ///
    /// * only after a *complete* scan of every search path — pruning after a scan of one
    ///   directory would throw away every plug-in outside it;
    /// * only for the formats that scan actually looked for, because `daux scan --format
    ///   clap` did not look for a single `.axt` and must not conclude that they are all gone.
    pub(crate) fn retain_seen(&mut self, seen: &BTreeSet<String>, formats: &[PluginFormat]) {
        let before = self.records.len();
        self.records
            .retain(|key, record| seen.contains(key) || !formats.contains(&record.format));
        if self.records.len() != before {
            self.dirty = true;
        }
    }

    /// Writes the cache, if it has a path and anything changed. [main-thread]
    ///
    /// The file is written beside its destination and renamed over it, so a host killed
    /// mid-write leaves the previous cache intact rather than a truncated one that every
    /// future start-up would have to discard.
    ///
    /// # Errors
    ///
    /// [`ScanErrorKind::Io`] for anything the filesystem refused, and
    /// [`ScanErrorKind::Cache`] if the records cannot be serialised.
    pub(crate) fn save(&mut self) -> ScanResult<()> {
        let Some(path) = self.path.clone() else {
            return Ok(());
        };
        if !self.dirty {
            return Ok(());
        }

        let file = CacheFile {
            version: CACHE_VERSION,
            records: self.records.values().cloned().collect(),
        };
        let json = serde_json::to_vec_pretty(&file).map_err(|error| {
            ScanError::new(ScanErrorKind::Cache, format!("cannot serialise: {error}"))
                .with_path(&path)
        })?;

        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|error| {
                ScanError::new(ScanErrorKind::Io, error.to_string()).with_path(parent)
            })?;
        }

        let staging = staging_path(&path);
        std::fs::write(&staging, &json).map_err(|error| {
            ScanError::new(ScanErrorKind::Io, error.to_string()).with_path(&staging)
        })?;
        // Windows refuses to rename onto an existing file, so the old cache goes first. The
        // window between the two is the reason for the staging file: a crash inside it
        // costs one cold start, never a corrupt cache.
        let _ = std::fs::remove_file(&path);
        std::fs::rename(&staging, &path).map_err(|error| {
            let _ = std::fs::remove_file(&staging);
            ScanError::new(ScanErrorKind::Io, error.to_string()).with_path(&path)
        })?;

        self.dirty = false;
        Ok(())
    }
}

/// The key a path is stored under, or `None` when it cannot be one losslessly.
fn key_for(path: &Path) -> Option<String> {
    path.to_str().map(str::to_owned)
}

/// The staging file a save is written to before it is renamed into place.
fn staging_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".staging");
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{descriptor, temp_dir};

    fn full_descriptor() -> PluginDescriptor {
        PluginDescriptor::builder("com.example.gain", "Gain")
            .vendor("Example")
            .version(Version::new(2, 3, 4).with_build(567))
            .description("A gain")
            .url("https://example.com")
            .support_url("https://example.com/support")
            .copyright("© 2026 Example")
            .license("MIT OR Apache-2.0")
            .category(Category::Effect)
            .capabilities(Capabilities::HAS_GUI.union(Capabilities::AUDIO_EFFECT))
            .features(["gain", "utility"])
            .sample_formats(SampleFormats::BOTH)
            .state_schema_version(3)
            .min_abi(1, 2)
            .build()
            .expect("a valid descriptor")
    }

    /// Every field a host shows in its browser has to survive the round trip, or a cached
    /// plug-in and a freshly scanned one would look different to the user.
    #[test]
    fn a_descriptor_survives_the_cache_unchanged() {
        let original = full_descriptor();
        let cached = CachedDescriptor::from_descriptor(&original);
        let json = serde_json::to_string(&cached).expect("serialises");
        let parsed: CachedDescriptor = serde_json::from_str(&json).expect("parses");
        let restored = parsed.to_descriptor().expect("rebuilds");
        assert_eq!(restored, original);
    }

    #[test]
    fn an_unchanged_bundle_is_a_hit_and_a_changed_one_is_a_miss() {
        let mut cache = ScanCache::in_memory();
        let path = Path::new("/plugins/Gain.axt");
        let now = SystemTime::now();
        cache.store(
            path,
            0xdead_beef,
            PluginFormat::Axt,
            now,
            true,
            &[full_descriptor()],
        );

        let hit = cache
            .lookup(path, 0xdead_beef)
            .expect("an unchanged bundle");
        assert!(hit.probed);
        assert_eq!(hit.descriptors.len(), 1);
        assert_eq!(hit.descriptors[0].name, "Gain");

        assert!(
            cache.lookup(path, 0xdead_beee).is_none(),
            "one bit of the fingerprint is one changed bundle"
        );
        assert!(
            cache
                .lookup(Path::new("/plugins/Other.axt"), 0xdead_beef)
                .is_none()
        );
    }

    /// The timestamp is what a user interface shows as "last scanned", so a cache hit must
    /// report when the bundle was really read, not when the cache was consulted.
    #[test]
    fn a_hit_reports_the_time_of_the_scan_that_read_the_bundle() {
        let mut cache = ScanCache::in_memory();
        let then = UNIX_EPOCH + Duration::new(1_700_000_000, 123_456_789);
        cache.store(
            Path::new("/p/Gain.axt"),
            1,
            PluginFormat::Axt,
            then,
            true,
            &[],
        );
        let hit = cache.lookup(Path::new("/p/Gain.axt"), 1).expect("a hit");
        assert_eq!(hit.scanned_at, then);
    }

    #[test]
    fn the_cache_round_trips_through_a_real_file() {
        let dir = temp_dir("cache-roundtrip");
        let file = dir.join("scan-cache.json");

        let mut cache = ScanCache::at(file.clone());
        cache
            .load()
            .expect("an absent cache is a cold cache, not an error");
        assert_eq!(cache.len(), 0);
        assert!(!cache.dirty);

        cache.store(
            Path::new("/plugins/Gain.axt"),
            42,
            PluginFormat::Axt,
            UNIX_EPOCH + Duration::from_secs(1_000),
            true,
            &[full_descriptor()],
        );
        assert!(cache.dirty);
        cache.save().expect("writes");
        assert!(!cache.dirty, "a saved cache is clean");
        assert!(file.is_file());
        assert!(
            !staging_path(&file).exists(),
            "the staging file must not be left behind"
        );

        let mut reopened = ScanCache::at(file);
        reopened.load().expect("reads");
        assert_eq!(reopened.len(), 1);
        let hit = reopened
            .lookup(Path::new("/plugins/Gain.axt"), 42)
            .expect("the record survived the file");
        assert_eq!(hit.descriptors[0].vendor, "Example");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The cache lives in a user-writable directory, so every one of these is input a host
    /// will eventually meet. None of them may be fatal, and none may be trusted.
    #[test]
    fn a_hostile_or_broken_cache_file_is_refused_rather_than_believed() {
        let dir = temp_dir("cache-hostile");

        let truncated = dir.join("truncated.json");
        std::fs::write(&truncated, b"{\"version\":1,\"records\":[").expect("write");
        let mut cache = ScanCache::at(truncated);
        let error = cache.load().expect_err("truncated JSON is not a cache");
        assert_eq!(error.kind(), ScanErrorKind::Cache);
        assert_eq!(cache.len(), 0);

        let wrong_version = dir.join("v99.json");
        std::fs::write(&wrong_version, b"{\"version\":99,\"records\":[]}").expect("write");
        let mut cache = ScanCache::at(wrong_version);
        let error = cache.load().expect_err("a future cache is not read");
        assert_eq!(error.kind(), ScanErrorKind::Cache);
        assert!(error.message().contains("rebuilt"), "{error}");

        // A record that deserialises but describes an impossible plug-in must not become a
        // descriptor the rest of the workspace would have rejected.
        let tampered = dir.join("tampered.json");
        std::fs::write(
            &tampered,
            br#"{"version":1,"records":[{"path":"/p/X.axt","fingerprint":1,"format":"axt",
                 "scanned_at_secs":0,"probed":true,
                 "descriptors":[{"id":"not a reverse dns id","name":""}]}]}"#,
        )
        .expect("write");
        let mut cache = ScanCache::at(tampered);
        cache.load().expect("the file itself is well formed");
        assert_eq!(cache.len(), 1);
        assert!(
            cache.lookup(Path::new("/p/X.axt"), 1).is_none(),
            "a record that cannot rebuild a valid descriptor is a miss"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_oversized_cache_is_refused_before_it_is_parsed() {
        let dir = temp_dir("cache-oversized");
        let path = dir.join("huge.json");
        // Sparse or not, the length is what the guard reads, and it is read before the file.
        let file = std::fs::File::create(&path).expect("create");
        file.set_len(MAX_CACHE_BYTES + 1).expect("grow");
        drop(file);

        let mut cache = ScanCache::at(path);
        let error = cache.load().expect_err("refused");
        assert_eq!(error.kind(), ScanErrorKind::Cache);
        assert!(error.message().contains("limit"), "{error}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pruning_forgets_only_what_the_scan_did_not_see() {
        let mut cache = ScanCache::in_memory();
        for path in ["/p/A.axt", "/p/B.axt", "/p/C.axt"] {
            cache.store(
                Path::new(path),
                1,
                PluginFormat::Axt,
                SystemTime::UNIX_EPOCH,
                false,
                &[descriptor("com.example.a", "A", "Example")],
            );
        }
        assert_eq!(cache.len(), 3);

        let seen: BTreeSet<String> = ["/p/A.axt".to_owned(), "/p/C.axt".to_owned()]
            .into_iter()
            .collect();
        cache.retain_seen(&seen, &[PluginFormat::Axt]);
        assert_eq!(cache.len(), 2);
        assert!(cache.lookup(Path::new("/p/B.axt"), 1).is_none());
        assert!(cache.lookup(Path::new("/p/A.axt"), 1).is_some());
    }

    /// A scan that filtered `.axt` out did not look for one, so it has learned nothing
    /// about whether they are still installed. Pruning on that would throw away the whole
    /// cache and make the next full scan reload every plug-in on the machine.
    #[test]
    fn a_scan_of_other_formats_does_not_forget_what_it_did_not_look_for() {
        let mut cache = ScanCache::in_memory();
        cache.store(
            Path::new("/p/A.axt"),
            1,
            PluginFormat::Axt,
            SystemTime::UNIX_EPOCH,
            true,
            &[full_descriptor()],
        );

        cache.retain_seen(&BTreeSet::new(), &[PluginFormat::Clap]);
        assert_eq!(cache.len(), 1, "a CLAP-only scan says nothing about AXT");

        cache.retain_seen(&BTreeSet::new(), &[PluginFormat::Axt]);
        assert_eq!(
            cache.len(),
            0,
            "an AXT scan that found none means there are none"
        );
    }

    #[test]
    fn an_in_memory_cache_never_touches_the_filesystem() {
        let mut cache = ScanCache::in_memory();
        assert!(cache.path.is_none());
        cache.store(
            Path::new("/p/A.axt"),
            1,
            PluginFormat::Axt,
            SystemTime::UNIX_EPOCH,
            false,
            &[],
        );
        assert!(cache.dirty);
        cache.save().expect("saving a pathless cache is a no-op");
        assert!(cache.dirty, "nothing was written, so nothing became clean");
    }
}
