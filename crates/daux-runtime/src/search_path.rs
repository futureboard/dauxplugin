//! Where the dynamic linker looks for a plug-in's bundled dependencies.
//!
//! A bundle may ship its own libraries in `Library/{target}/`. Making the loader find them
//! is the one part of loading that is genuinely platform-specific, and it is also the part
//! where the obvious answer is wrong:
//!
//! > **Never mutate `PATH` or `LD_LIBRARY_PATH`.**
//!
//! Those are process-global mutable state. A host has dozens of plug-ins from different
//! vendors in one process; whichever one edited the variable last decides which
//! `libfftw3.so` every other one resolves to, and the corruption shows up as a crash in
//! an innocent plug-in hours later.
//!
//! The correct mechanisms are per-load and additive:
//!
//! * **Windows** — [`AddDllDirectory`] registers one directory and returns a cookie; the
//!   library is then opened with `LOAD_LIBRARY_SEARCH_USER_DIRS`, which is the only search
//!   mode that consults those directories. The cookie is removed when the module is
//!   dropped, so the addition lives exactly as long as the module does.
//! * **Unix** — nothing to do at load time. The plug-in binary carries a `$ORIGIN`-relative
//!   `RUNPATH` baked in at link time, so `dlopen` resolves siblings without the host
//!   touching any environment variable. `daux build` is responsible for the rpath; this
//!   crate deliberately does nothing here rather than reaching for `LD_LIBRARY_PATH`.
//!
//! [`AddDllDirectory`]: https://learn.microsoft.com/windows/win32/api/libloaderapi/nf-libloaderapi-adddlldirectory

use std::path::Path;

/// A directory registered with the platform's dynamic loader for the lifetime of one
/// module. [main-thread]
///
/// Dropping it undoes the registration. On platforms where the plug-in's own rpath does
/// the work, this is an empty value and [`DependencyDirectory::add`] always returns `None`.
#[derive(Debug)]
pub(crate) struct DependencyDirectory {
    #[cfg(windows)]
    cookie: *mut core::ffi::c_void,
}

#[cfg(windows)]
// SAFETY: the value is a `DLL_DIRECTORY_COOKIE`, an opaque process-wide token that is never
// dereferenced by this crate. `AddDllDirectory`/`RemoveDllDirectory` are documented as safe
// to call from any thread, so moving the cookie between threads cannot introduce a data
// race; the only operation performed on it is the single `RemoveDllDirectory` in `Drop`.
unsafe impl Send for DependencyDirectory {}

#[cfg(windows)]
// SAFETY: as for `Send` — `&DependencyDirectory` exposes no operation at all, so sharing it
// is trivially race-free.
unsafe impl Sync for DependencyDirectory {}

impl DependencyDirectory {
    /// Registers `dir` with the dynamic loader, if this platform needs it. [main-thread]
    ///
    /// Returns `None` when the platform resolves dependencies through the plug-in's own
    /// rpath, and also when the operating system refuses the directory — a refusal is not
    /// fatal, because the library may well have no bundled dependencies at all, and
    /// failing the whole load over it would be worse than letting `LoadLibraryExW` report
    /// the real missing dependency a moment later.
    #[cfg(windows)]
    pub(crate) fn add(dir: &Path) -> Option<Self> {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::System::LibraryLoader::AddDllDirectory;

        let mut wide: Vec<u16> = dir.as_os_str().encode_wide().collect();
        // An interior NUL would silently truncate the path the OS sees.
        if wide.is_empty() || wide.contains(&0) {
            return None;
        }
        wide.push(0);

        // SAFETY: `wide` is a NUL-terminated UTF-16 sequence that lives for the whole call,
        // which is exactly what `AddDllDirectory`'s `PCWSTR` parameter requires. The call
        // borrows the buffer only for its duration and returns an opaque cookie; it does
        // not retain the pointer.
        let cookie = unsafe { AddDllDirectory(wide.as_ptr()) };
        if cookie.is_null() {
            return None;
        }
        Some(Self { cookie })
    }

    /// Registers `dir` with the dynamic loader, if this platform needs it. [main-thread]
    ///
    /// Always `None` outside Windows: the plug-in binary's `$ORIGIN` rpath already covers
    /// its own `Library/{target}` directory, and mutating `LD_LIBRARY_PATH` would corrupt
    /// every other plug-in in the process.
    #[cfg(not(windows))]
    pub(crate) fn add(dir: &Path) -> Option<Self> {
        let _ = dir;
        None
    }
}

#[cfg(windows)]
impl Drop for DependencyDirectory {
    fn drop(&mut self) {
        use windows_sys::Win32::System::LibraryLoader::RemoveDllDirectory;

        // SAFETY: `cookie` came from a successful `AddDllDirectory` in `add` and has not
        // been removed before — `DependencyDirectory` is neither `Copy` nor `Clone`, so
        // this runs exactly once per successful registration. A failure is not actionable
        // and must not unwind out of a destructor, so the result is dropped.
        let _ = unsafe { RemoveDllDirectory(self.cookie) };
    }
}
