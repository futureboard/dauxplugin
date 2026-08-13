//! A self-deleting directory to build fixtures in.
//!
//! The bundle and scanner suites need real directories: a bundle is a directory tree by
//! definition (`axt-v1` §1), and a scanner walks the filesystem. Nothing here is clever —
//! it exists so that a test can make a tree, hand its path to code under test, and be sure
//! the tree is gone afterwards even when an assertion fails.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Distinguishes trees created within one process; the process id separates processes and
/// the nanosecond clock separates successive runs of the same test binary.
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A uniquely named directory under the system temporary directory, removed on drop.
///
/// The name embeds a caller-supplied label so a leftover directory from a killed test run
/// says which test made it.
///
/// ```
/// let tree = daux_tests::TempTree::new("example");
/// tree.write("Content/readme.txt", b"hello");
/// assert!(tree.path().join("Content/readme.txt").is_file());
/// ```
///
/// [main-thread]
#[derive(Debug)]
pub struct TempTree {
    path: PathBuf,
}

impl TempTree {
    /// [main-thread] Creates an empty tree labelled `label`.
    ///
    /// # Panics
    ///
    /// If the directory cannot be created. A test that cannot make a temporary directory
    /// has nothing useful left to assert.
    #[must_use]
    pub fn new(label: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "daux-tests-{label}-{}-{nanos:x}-{unique}",
            std::process::id(),
        ));
        // A previous run that was killed between naming and cleanup would collide.
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path)
            .unwrap_or_else(|e| panic!("cannot create temporary tree at {}: {e}", path.display()));
        Self { path }
    }

    /// [main-thread] The tree's root.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// [main-thread] Creates `relative` as a directory, including parents, and returns it.
    ///
    /// # Panics
    ///
    /// If the directory cannot be created.
    pub fn dir(&self, relative: impl AsRef<Path>) -> PathBuf {
        let path = self.path.join(relative);
        std::fs::create_dir_all(&path)
            .unwrap_or_else(|e| panic!("cannot create {}: {e}", path.display()));
        path
    }

    /// [main-thread] Writes `contents` to `relative`, creating parent directories.
    ///
    /// # Panics
    ///
    /// If the file cannot be written.
    pub fn write(&self, relative: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> PathBuf {
        let path = self.path.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .unwrap_or_else(|e| panic!("cannot create {}: {e}", parent.display()));
        }
        std::fs::write(&path, contents)
            .unwrap_or_else(|e| panic!("cannot write {}: {e}", path.display()));
        path
    }

    /// [main-thread] Joins `relative` onto the tree's root without touching the filesystem.
    #[must_use]
    pub fn join(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.path.join(relative)
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        // Best effort: a Windows virus scanner holding a handle open must not turn a
        // passing test into a panic-in-drop, which would abort the whole test binary.
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
