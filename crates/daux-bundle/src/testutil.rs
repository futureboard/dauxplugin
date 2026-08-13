//! A scratch directory for the tests, so the crate needs no dev-dependency.

use std::path::{Path, PathBuf};

/// A directory that deletes itself when dropped.
pub(crate) struct TempDir(PathBuf);

impl TempDir {
    /// The directory's path. Valid until this value is dropped.
    pub(crate) fn path(&self) -> &Path {
        &self.0
    }

    /// Writes `contents` to `relative`, creating parent directories as needed.
    pub(crate) fn write(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.0.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("a writable temp directory");
        }
        std::fs::write(&path, contents).expect("a writable temp file");
        path
    }

    /// Creates `relative` as a directory, including parents.
    pub(crate) fn dir(&self, relative: &str) -> PathBuf {
        let path = self.0.join(relative);
        std::fs::create_dir_all(&path).expect("a writable temp directory");
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A fresh scratch directory.
///
/// Named from the process id and a counter rather than a random number, so a directory left
/// behind by a crashed run can be traced back to it.
pub(crate) fn tempdir() -> TempDir {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("daux-bundle-test-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&path).expect("a writable temp directory");
    TempDir(path)
}
