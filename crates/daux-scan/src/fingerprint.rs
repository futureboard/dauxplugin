//! The value a cache entry is keyed on.
//!
//! A scan cache is only useful if it is *safe* to trust, and it is only safe to trust if
//! every change a developer or an installer can make invalidates it. `manifest-v1` §8.2 says
//! it directly: a fingerprint MUST cover the metadata file's bytes *and* the binary's size
//! and modification time.
//!
//! Bytes rather than a timestamp for the metadata because that file is small, is read
//! anyway, and is the one a human edits — and because a copy that preserves timestamps
//! (`cp -p`, an installer, a restored backup) would otherwise slip past. Size and
//! modification time for the binary because hashing tens of megabytes for every plug-in at
//! every start-up is exactly the cost a cache exists to avoid.
//!
//! The hash is FNV-1a, written out here rather than taken from [`std::hash`]:
//! `DefaultHasher`'s algorithm is explicitly unspecified and may change between Rust
//! releases, which would silently invalidate every user's cache on a toolchain upgrade.

use std::path::Path;
use std::time::UNIX_EPOCH;

const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// A stable, non-cryptographic hash. [any-thread]
///
/// Not a security boundary: it detects accidents — an edited manifest, a replaced binary,
/// a bundle moved to another directory — and nothing else. A hostile bundle can trivially
/// produce a collision, which is why nothing in this crate skips *validation* on the
/// strength of a fingerprint; a cache hit skips re-reading and re-loading, never re-checking
/// what was read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Fnv1a(u64);

impl Fnv1a {
    /// An empty hash. [any-thread]
    pub(crate) const fn new() -> Self {
        Self(FNV_OFFSET_BASIS)
    }

    /// Folds `bytes` in. [any-thread]
    pub(crate) const fn write(&mut self, bytes: &[u8]) {
        let mut index = 0;
        while index < bytes.len() {
            self.0 ^= bytes[index] as u64;
            self.0 = self.0.wrapping_mul(FNV_PRIME);
            index += 1;
        }
    }

    /// Folds a number in, little-endian. [any-thread]
    pub(crate) const fn write_u64(&mut self, value: u64) {
        self.write(&value.to_le_bytes());
    }

    /// Folds a length-prefixed field in. [any-thread]
    ///
    /// The prefix is what keeps `("ab", "c")` and `("a", "bc")` apart, so two different
    /// bundles cannot share a fingerprint just because their fields concatenate the same
    /// way.
    pub(crate) const fn write_field(&mut self, bytes: &[u8]) {
        self.write_u64(bytes.len() as u64);
        self.write(bytes);
    }

    /// The hash so far. [any-thread]
    pub(crate) const fn finish(self) -> u64 {
        self.0
    }
}

/// What a scan knows about a file without reading it. [main-thread]
///
/// `None` for a file that does not exist or whose metadata the filesystem refuses; that is
/// itself a fingerprint input, so a plug-in whose binary disappears is not a cache hit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FileStamp {
    /// Size in bytes.
    size: u64,
    /// Modification time in nanoseconds since the Unix epoch, or `0` when the filesystem
    /// has none.
    modified_nanos: u128,
    /// Whether the file was there at all.
    present: bool,
}

impl FileStamp {
    /// Stats `path`, treating every failure as "not there". [main-thread]
    pub(crate) fn of(path: &Path) -> Self {
        let Ok(metadata) = std::fs::metadata(path) else {
            return Self::default();
        };
        let modified_nanos = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |since| since.as_nanos());
        Self {
            size: metadata.len(),
            modified_nanos,
            present: true,
        }
    }

    /// Folds the stamp into a hash. [main-thread]
    fn hash_into(self, hash: &mut Fnv1a) {
        hash.write_u64(u64::from(self.present));
        hash.write_u64(self.size);
        // `u128` covers every timestamp a filesystem can produce; folding both halves keeps
        // sub-second resolution, which is what distinguishes two builds a moment apart.
        hash.write_u64(self.modified_nanos as u64);
        hash.write_u64((self.modified_nanos >> 64) as u64);
    }
}

/// The fingerprint of one bundle. [main-thread]
///
/// * `root` — where the bundle is. A bundle copied to a second directory is a second
///   entry, because a host addresses plug-ins by path.
/// * `metadata_bytes` — the bytes of `manifest.json` or `Info.plist`, verbatim.
/// * `binary` — the plug-in binary for the target this host would load, when there is one.
#[must_use]
pub(crate) fn of_bundle(root: &Path, metadata_bytes: &[u8], binary: Option<&Path>) -> u64 {
    let mut hash = Fnv1a::new();
    hash.write_field(root.to_string_lossy().as_bytes());
    hash.write_field(metadata_bytes);
    match binary {
        Some(path) => {
            hash.write_field(path.to_string_lossy().as_bytes());
            FileStamp::of(path).hash_into(&mut hash);
        }
        None => {
            hash.write_field(b"");
            FileStamp::default().hash_into(&mut hash);
        }
    }
    hash.finish()
}

/// The fingerprint of a VST3 or CLAP artefact. [main-thread]
///
/// There is no metadata to read, so this is path, size and modification time only — enough
/// to notice that the file was replaced, which is all a scan can honestly claim about a
/// format it does not open.
#[must_use]
pub(crate) fn of_file(path: &Path) -> u64 {
    let mut hash = Fnv1a::new();
    hash.write_field(path.to_string_lossy().as_bytes());
    FileStamp::of(path).hash_into(&mut hash);
    hash.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("daux-scan-fp-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a temp directory");
        dir
    }

    /// The property the whole cache rests on: equal inputs, equal fingerprint — including
    /// across processes, which is why the algorithm is written out rather than borrowed
    /// from `std`.
    #[test]
    fn the_hash_is_the_published_fnv_1a_and_is_stable() {
        // The FNV-1a 64-bit test vectors from the reference implementation.
        let mut empty = Fnv1a::new();
        empty.write(b"");
        assert_eq!(empty.finish(), 0xcbf2_9ce4_8422_2325);

        let mut a = Fnv1a::new();
        a.write(b"a");
        assert_eq!(a.finish(), 0xaf63_dc4c_8601_ec8c);

        let mut foobar = Fnv1a::new();
        foobar.write(b"foobar");
        assert_eq!(foobar.finish(), 0x8594_4171_f739_67e8);
    }

    /// Concatenation must not be able to hide a difference: `("ab","c")` and `("a","bc")`
    /// are different bundles and must have different fingerprints.
    #[test]
    fn fields_are_length_prefixed_so_they_cannot_run_together() {
        let mut first = Fnv1a::new();
        first.write_field(b"ab");
        first.write_field(b"c");

        let mut second = Fnv1a::new();
        second.write_field(b"a");
        second.write_field(b"bc");

        assert_ne!(first.finish(), second.finish());
    }

    #[test]
    fn a_changed_manifest_changes_the_fingerprint() {
        let root = PathBuf::from("/plugins/Gain.axt");
        let before = of_bundle(&root, b"{\"format\":\"DAUx Audio Extension\"}", None);
        let after = of_bundle(&root, b"{\"format\":\"DAUx Audio Extension\" }", None);
        assert_ne!(
            before, after,
            "one byte of whitespace is still an edited manifest"
        );
    }

    #[test]
    fn the_same_bundle_in_two_directories_has_two_fingerprints() {
        let bytes = b"{}";
        let here = of_bundle(Path::new("/a/Gain.axt"), bytes, None);
        let there = of_bundle(Path::new("/b/Gain.axt"), bytes, None);
        assert_ne!(here, there);
    }

    /// `manifest-v1` §8.2: the binary's size and modification time are part of the
    /// fingerprint, so a rebuild that leaves the manifest alone still invalidates the
    /// cache. This is the case a developer hits every single build.
    #[test]
    fn rebuilding_only_the_binary_invalidates_the_entry() {
        let dir = temp_dir("binary");
        let binary = dir.join("gain.dll");
        std::fs::write(&binary, b"MZ\x00\x00").expect("write");

        let before = of_bundle(&dir, b"{}", Some(&binary));

        // A different size is a different binary, whatever the clock says.
        std::fs::write(&binary, b"MZ\x00\x00\x00\x00").expect("rewrite");
        let after = of_bundle(&dir, b"{}", Some(&binary));
        assert_ne!(before, after);

        // And an absent binary is not the same as a present one.
        std::fs::remove_file(&binary).expect("remove");
        let gone = of_bundle(&dir, b"{}", Some(&binary));
        assert_ne!(after, gone);
        assert_ne!(
            gone,
            of_bundle(&dir, b"{}", None),
            "`no binary declared` and `binary is missing` are different states"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unchanged_bundle_fingerprints_identically_twice() {
        let dir = temp_dir("stable");
        let binary = dir.join("gain.dll");
        std::fs::write(&binary, b"MZ").expect("write");
        let first = of_bundle(&dir, b"{\"a\":1}", Some(&binary));
        let second = of_bundle(&dir, b"{\"a\":1}", Some(&binary));
        assert_eq!(first, second, "nothing changed, so nothing may change");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_file_stamps_as_absent_rather_than_failing() {
        let stamp = FileStamp::of(Path::new("/definitely/not/here.dll"));
        assert!(!stamp.present);
        assert_eq!(stamp.size, 0);
    }

    #[test]
    fn a_replaced_foreign_binary_is_noticed() {
        let dir = temp_dir("foreign");
        let clap = dir.join("reverb.clap");
        std::fs::write(&clap, b"one").expect("write");
        let before = of_file(&clap);
        std::fs::write(&clap, b"two but longer").expect("rewrite");
        assert_ne!(before, of_file(&clap));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
