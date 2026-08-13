//! The dynamic library, and the ABI negotiation that decides whether it may be called.

use core::mem::offset_of;
use std::path::{Path, PathBuf};

use daux_abi::{
    DAUX_ABI_MAGIC, DAUX_ABI_VERSION_MAJOR, DAUX_ENTRY_SYMBOL, DAUX_ENTRY_SYMBOL_CSTR,
    DauxPluginEntryFn, DauxPluginEntryV1, DauxVersion,
};
use daux_bundle::{Bundle, TargetId};
use libloading::Library;

use crate::error::{RuntimeError, RuntimeErrorKind, RuntimeResult};
use crate::probe::{RequiredFn, read_table};
use crate::search_path::DependencyDirectory;

/// The entries of [`DauxPluginEntryV1`] that have no null representation.
const ENTRY_REQUIRED: &[RequiredFn] = &[
    (
        offset_of!(DauxPluginEntryV1, create_factory),
        "create_factory",
    ),
    (
        offset_of!(DauxPluginEntryV1, destroy_factory),
        "destroy_factory",
    ),
];

/// A loaded `.axt` module: one dynamic library plus its validated entry header.
/// [main-thread]
///
/// # Why this type is always behind an `Arc`
///
/// Unloading a library while a factory or an instance from it still exists turns every
/// function pointer the host holds into an address in unmapped memory. There is no
/// diagnostic for that — the process dies with an access violation somewhere unrelated.
///
/// So [`LoadedFactory`](crate::LoadedFactory) takes `Arc<AxtModule>` and every
/// [`LoadedPlugin`](crate::LoadedPlugin) holds a strong reference to its factory, which
/// holds the module. The library is therefore dropped **after** the last instance and the
/// factory, by construction rather than by discipline: there is no ordering a caller can
/// choose that unloads it early.
///
/// # What `load` guarantees
///
/// A module that returns from [`AxtModule::load`] has satisfied every rejection rule of
/// `abi-v1` §3: it exports `daux_plugin_entry_v1`, the symbol returned a non-null pointer,
/// the magic matches, the major version is 1, the declared `size` covers the whole v1.0
/// header, and both non-optional entries are non-null. Nothing has been *called* through
/// the header yet; `create_factory` runs when a [`LoadedFactory`](crate::LoadedFactory) is
/// built.
#[derive(Debug)]
pub struct AxtModule {
    /// Dropped first, which is what actually unloads the image. Never read: holding it for
    /// exactly as long as the `Arc` lives *is* the job.
    #[allow(
        dead_code,
        reason = "held so that `Drop` unmaps the image only after every derived object is gone"
    )]
    image: ModuleImage,
    /// A host-owned copy of the module's static header. Copying it means later reads never
    /// touch module memory again and never depend on the module's alignment.
    entry: DauxPluginEntryV1,
    path: PathBuf,
    /// Registration of the bundle's `Library/{target}` directory, undone on drop.
    dependency_dir: Option<DependencyDirectory>,
}

/// Where a module's code lives.
#[derive(Debug)]
enum ModuleImage {
    /// A dynamic library this process mapped. Dropping it unmaps the image, which is why
    /// every derived object holds an `Arc<AxtModule>`.
    Dynamic(
        #[allow(
            dead_code,
            reason = "the handle is never read; dropping it at the right moment is its purpose"
        )]
        Library,
    ),
    /// Tables compiled into the host binary itself. Only this crate's own tests use it:
    /// there is no example plug-in yet, and the interesting failures are the ones a
    /// conforming module never produces. Nothing to unload, so the `Arc` chain is the only
    /// thing the variant changes — which is exactly what the tests exercise.
    #[cfg(test)]
    Static,
}

impl AxtModule {
    /// Opens the binary a bundle ships for `target`. [main-thread]
    ///
    /// The bundle's `Library/{target}` directory, when it exists, is registered with the
    /// dynamic loader for the lifetime of this module — never by editing `PATH` or
    /// `LD_LIBRARY_PATH`, which are process-global and shared with every other plug-in in
    /// the host. The mechanism is per-load and additive: `AddDllDirectory` plus
    /// `LOAD_LIBRARY_SEARCH_USER_DIRS` on Windows, and the plug-in binary's own `$ORIGIN`
    /// rpath elsewhere.
    ///
    /// # Errors
    ///
    /// [`RuntimeErrorKind::NotFound`] when the bundle ships nothing for `target` — the
    /// normal outcome for a cross-platform bundle on a platform it does not build for.
    /// [`RuntimeErrorKind::Library`] when the operating system refuses the image,
    /// [`RuntimeErrorKind::MissingEntry`] when the entry symbol is absent, and
    /// [`RuntimeErrorKind::AbiMismatch`] for a header this host may not call into.
    pub fn load(bundle: &Bundle, target: &TargetId) -> RuntimeResult<Self> {
        let binary = bundle.binary_path(target)?;
        let dependencies = bundle.library_dir(target);
        Self::open(&binary, dependencies.as_deref())
    }

    /// Opens a bare dynamic library. [main-thread]
    ///
    /// The path a host normally takes is [`AxtModule::load`]; this exists for tooling that
    /// already knows the binary, and for tests. `dependency_dir`, when given, is registered
    /// with the dynamic loader exactly as [`AxtModule::load`] does it.
    ///
    /// # Errors
    ///
    /// As [`AxtModule::load`], minus the bundle failures.
    pub fn open(binary: &Path, dependency_dir: Option<&Path>) -> RuntimeResult<Self> {
        // `LoadLibraryExW` only consults the alternate search order for an absolute path,
        // and `AddDllDirectory` rejects a relative one outright.
        let binary = std::path::absolute(binary).map_err(|e| {
            RuntimeError::new(
                RuntimeErrorKind::Bundle,
                format!("cannot resolve the plug-in binary path: {e}"),
            )
            .with_path(binary)
        })?;

        let registered = match dependency_dir {
            Some(dir) => std::path::absolute(dir)
                .ok()
                .and_then(|dir| DependencyDirectory::add(&dir)),
            None => None,
        };

        let library = open_library(&binary)?;
        let entry = read_entry_symbol(&library, &binary)?;

        Ok(Self {
            image: ModuleImage::Dynamic(library),
            entry,
            path: binary,
            dependency_dir: registered,
        })
    }

    /// Wraps an entry header whose tables live in this binary. [main-thread]
    ///
    /// Test-only: it is how the crate's own tests get a module to drive without a `.axt` on
    /// disk. Everything downstream — the factory, the instances, the `Arc` chain — is the
    /// production code path unchanged.
    #[cfg(test)]
    pub(crate) fn from_static_entry(entry: DauxPluginEntryV1) -> Self {
        Self {
            image: ModuleImage::Static,
            entry,
            path: PathBuf::from("<static>"),
            dependency_dir: None,
        }
    }

    /// The module's validated static header. [main-thread]
    ///
    /// This is the host's own copy, not a borrow of module memory, so it stays readable and
    /// correctly aligned whatever the module did with its `static`.
    #[inline]
    #[must_use]
    pub const fn entry(&self) -> &DauxPluginEntryV1 {
        &self.entry
    }

    /// The `(major, minor)` ABI version the module was built against. [main-thread]
    ///
    /// `major` is always 1 — a header that said otherwise was refused at load. `minor` may
    /// be **higher** than this host implements, and that is not an error: a reader accepts
    /// both directions and validates individual fields with `size` instead (`abi-v1` §3).
    #[inline]
    #[must_use]
    pub const fn abi_version(&self) -> (u32, u32) {
        (self.entry.abi_version_major, self.entry.abi_version_minor)
    }

    /// The binary this module was loaded from. [main-thread]
    #[inline]
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The SDK that produced the binary. Diagnostics only. [main-thread]
    #[inline]
    #[must_use]
    pub fn sdk_name(&self) -> &str {
        self.entry.sdk_name.as_str()
    }

    /// The version of the SDK that produced the binary. Diagnostics only. [main-thread]
    #[inline]
    #[must_use]
    pub const fn sdk_version(&self) -> DauxVersion {
        self.entry.sdk_version
    }

    /// Whether a bundled-dependency directory was registered with the loader for this
    /// module. [main-thread]
    ///
    /// Always `false` where the platform resolves dependencies through the plug-in's own
    /// `$ORIGIN` rpath instead, which is every platform but Windows.
    #[inline]
    #[must_use]
    pub const fn has_dependency_directory(&self) -> bool {
        self.dependency_dir.is_some()
    }
}

/// Opens `binary`, using the additive per-load search order where the platform has one.
fn open_library(binary: &Path) -> RuntimeResult<Library> {
    #[cfg(windows)]
    {
        use libloading::os::windows as win;

        // `DLL_LOAD_DIR` finds siblings of the plug-in itself, `USER_DIRS` finds the
        // directories registered with `AddDllDirectory`, and `DEFAULT_DIRS` keeps the
        // application directory and System32 reachable. Passing any of these switches
        // `LoadLibraryExW` off the legacy search order, which is what keeps `PATH` out of
        // the picture entirely.
        let flags = win::LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR
            | win::LOAD_LIBRARY_SEARCH_USER_DIRS
            | win::LOAD_LIBRARY_SEARCH_DEFAULT_DIRS;

        // SAFETY: opening a library runs the image's static initialisers, which is why
        // `libloading` marks this `unsafe`; there is no way to load a plug-in without it.
        // The path is absolute and owned by the caller for the duration of the call, and
        // the returned handle owns the module from here on.
        let library = unsafe { win::Library::load_with_flags(binary, flags) }
            .map_err(|e| RuntimeError::from(e).with_path(binary))?;
        Ok(Library::from(library))
    }
    #[cfg(not(windows))]
    {
        // SAFETY: as above — `dlopen` runs the image's initialisers. The path is absolute
        // and lives for the duration of the call. Dependency resolution relies on the
        // `$ORIGIN` rpath the plug-in binary carries; no environment variable is touched.
        let library =
            unsafe { Library::new(binary) }.map_err(|e| RuntimeError::from(e).with_path(binary))?;
        Ok(library)
    }
}

/// Resolves `daux_plugin_entry_v1`, calls it, and validates what it returned.
fn read_entry_symbol(library: &Library, binary: &Path) -> RuntimeResult<DauxPluginEntryV1> {
    // SAFETY: the symbol is looked up by its documented name and immediately copied out as
    // a plain function pointer, so the `Symbol` borrow of `library` ends here. The signature
    // is the one `abi-v1` §4 fixes for this symbol; a binary that exports it with a
    // different one is not a DAUx module, and no amount of checking on this side could
    // detect that.
    let entry_fn: DauxPluginEntryFn = unsafe {
        library
            .get::<DauxPluginEntryFn>(DAUX_ENTRY_SYMBOL_CSTR)
            .map(|symbol| *symbol)
    }
    .map_err(|e| {
        RuntimeError::new(
            RuntimeErrorKind::MissingEntry,
            format!("`{DAUX_ENTRY_SYMBOL}` is not exported: {e}"),
        )
        .with_path(binary)
    })?;

    // SAFETY: `entry_fn` is a live symbol in a module this call keeps loaded. `abi-v1` §4
    // requires the function to be callable before any other DAUx symbol, to take no
    // arguments and to return a pointer to `'static` storage; it may return null, which the
    // validation below treats as rejection rule 1.
    let raw = unsafe { entry_fn() };

    // SAFETY: `raw` is whatever the module returned. `read_entry` handles null itself and
    // reads nothing beyond the `size` word until that word says the bytes exist. `abi-v1`
    // §4 guarantees the storage is `'static`, so it outlives this call.
    unsafe { read_entry(raw) }.map_err(|e| e.with_path(binary))
}

/// Applies the rejection rules of `abi-v1` §3 to a raw entry pointer. [main-thread]
///
/// Split out of [`AxtModule::open`] so that the rules can be exercised against hand-built
/// headers without a real module on disk.
///
/// # Errors
///
/// [`RuntimeErrorKind::MissingEntry`] for a null pointer (rule 1),
/// [`RuntimeErrorKind::AbiMismatch`] for bad magic (rule 2), a major version this host does
/// not implement (rule 3) or an undersized header (rule 4), and
/// [`RuntimeErrorKind::Protocol`] when `create_factory` or `destroy_factory` is null.
///
/// # Safety
///
/// `ptr` must be null or point to at least four readable bytes with `'static` lifetime, and,
/// when the `size` word it holds is at least `DauxPluginEntryV1::MIN_SIZE_V1_0`, to that
/// many readable bytes. Reads are unaligned, so no alignment is assumed.
pub(crate) unsafe fn read_entry(ptr: *const DauxPluginEntryV1) -> RuntimeResult<DauxPluginEntryV1> {
    // Rejection rule 1. Reported as a missing entry rather than a protocol error: from a
    // host's point of view "the symbol gave me nothing" and "there is no symbol" are the
    // same unloadable binary.
    if ptr.is_null() {
        return Err(RuntimeError::new(
            RuntimeErrorKind::MissingEntry,
            format!("`{DAUX_ENTRY_SYMBOL}` returned null (abi-v1 §3, rejection rule 1)"),
        ));
    }

    // Rejection rule 4 first, because nothing else may be read until `size` says the bytes
    // are there — and rules 2 and 3 read fields at offsets 4 and 16.
    // SAFETY: forwarded verbatim from this function's own contract.
    let entry = unsafe { read_table(ptr, DAUX_ENTRY_SYMBOL, ENTRY_REQUIRED) }?;

    // Rejection rule 2.
    if entry.magic != DAUX_ABI_MAGIC {
        return Err(RuntimeError::abi(format!(
            "magic is {:#018x}, expected {DAUX_ABI_MAGIC:#018x} (abi-v1 §3, rejection rule 2)",
            entry.magic
        )));
    }

    // Rejection rule 3. A module built against a newer *major* generation exports a
    // different symbol, so reaching this branch means the header is inconsistent with the
    // symbol it was found under; either way this host must not call into it.
    if entry.abi_version_major != DAUX_ABI_VERSION_MAJOR {
        return Err(RuntimeError::abi(format!(
            "ABI major version {} is not {DAUX_ABI_VERSION_MAJOR} (abi-v1 §3, rejection rule 3)",
            entry.abi_version_major
        )));
    }

    // The minor version is deliberately not checked: a host MUST accept a lower *or* higher
    // minor revision and validate individual fields through `size` instead.
    Ok(entry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Aligned, entry_header, plant};

    #[test]
    fn a_conforming_header_is_accepted() {
        let header = entry_header();
        let mut buffer = Aligned::<512>::new();
        let ptr = plant(&mut buffer, &header);
        // SAFETY: `plant` copied a complete, well-formed header into a buffer that outlives
        // the call, so the pointer addresses `size_of::<DauxPluginEntryV1>()` readable bytes.
        let read = unsafe { read_entry(ptr) }.expect("a conforming header loads");
        assert_eq!(read.magic, DAUX_ABI_MAGIC);
        assert_eq!(read.abi_version_major, 1);
        assert_eq!(read.sdk_name.as_str(), "daux-test-sdk");
    }

    /// Rejection rule 1.
    #[test]
    fn a_null_entry_pointer_is_refused() {
        // SAFETY: null is explicitly admitted by `read_entry`'s contract.
        let err = unsafe { read_entry(core::ptr::null()) }.unwrap_err();
        assert_eq!(err.kind(), RuntimeErrorKind::MissingEntry);
        assert!(err.message().contains("rule 1"), "{err}");
    }

    /// Rejection rule 2 — including the near-miss a byte-swapped magic produces.
    #[test]
    fn bad_magic_is_refused() {
        for magic in [0, DAUX_ABI_MAGIC.swap_bytes(), DAUX_ABI_MAGIC ^ 1, u64::MAX] {
            let mut header = entry_header();
            header.magic = magic;
            let mut buffer = Aligned::<512>::new();
            let ptr = plant(&mut buffer, &header);
            // SAFETY: a complete header lives in `buffer`; only `magic` is wrong.
            let err = unsafe { read_entry(ptr) }.unwrap_err();
            assert_eq!(
                err.kind(),
                RuntimeErrorKind::AbiMismatch,
                "magic {magic:#x} must be refused"
            );
            assert!(err.message().contains("rule 2"), "{err}");
        }
    }

    /// Rejection rule 3: a module built against a newer generation must be refused
    /// cleanly, never called into optimistically.
    #[test]
    fn a_foreign_major_version_is_refused() {
        for major in [0, 2, 3, u32::MAX] {
            let mut header = entry_header();
            header.abi_version_major = major;
            let mut buffer = Aligned::<512>::new();
            let ptr = plant(&mut buffer, &header);
            // SAFETY: a complete header lives in `buffer`; only the major version is wrong.
            let err = unsafe { read_entry(ptr) }.unwrap_err();
            assert_eq!(
                err.kind(),
                RuntimeErrorKind::AbiMismatch,
                "major {major} must be refused"
            );
            assert!(err.message().contains("rule 3"), "{err}");
        }
    }

    /// Rejection rule 4.
    #[test]
    fn an_undersized_header_is_refused() {
        for declared in [0, 4, 16, DauxPluginEntryV1::SIZE - 1] {
            let header = entry_header();
            let mut buffer = Aligned::<512>::new();
            let ptr = plant(&mut buffer, &header);
            buffer.set_declared_size(declared);
            // SAFETY: the buffer holds a full header; only the declared size lies, and
            // `read_entry` must stop after reading that one word.
            let err = unsafe { read_entry(ptr) }.unwrap_err();
            assert_eq!(
                err.kind(),
                RuntimeErrorKind::AbiMismatch,
                "size {declared} must be refused"
            );
        }
    }

    /// `abi-v1` §3: a host MUST accept a plug-in with a lower *or* higher minor version.
    /// Refusing a newer minor would break every module built after the next tail extension.
    #[test]
    fn any_minor_version_is_accepted_in_both_directions() {
        for minor in [0, 1, 7, u32::MAX] {
            let mut header = entry_header();
            header.abi_version_minor = minor;
            let mut buffer = Aligned::<512>::new();
            let ptr = plant(&mut buffer, &header);
            // SAFETY: a complete, well-formed header lives in `buffer`.
            let read = unsafe { read_entry(ptr) }.expect("minor versions never reject");
            assert_eq!(read.abi_version_minor, minor);
        }
    }

    /// A newer module declares a bigger header and appends fields the host does not know.
    /// The unknown tail must be ignored, not refused.
    #[test]
    fn a_newer_header_with_a_larger_size_is_accepted() {
        let header = entry_header();
        let mut buffer = Aligned::<512>::new();
        let ptr = plant(&mut buffer, &header);
        buffer.set_declared_size(DauxPluginEntryV1::SIZE + 128);
        buffer.write_at(DauxPluginEntryV1::SIZE as usize, &[0xAB; 64]);
        // SAFETY: the 512-byte buffer covers the inflated size the header declares, so
        // every byte `read_entry` may look at is memory this test owns.
        let read = unsafe { read_entry(ptr) }.expect("forward compatible");
        assert_eq!(read.abi_version_major, 1);
    }

    /// `create_factory`/`destroy_factory` have no null representation in Rust, so a module
    /// that leaves one zeroed must be caught before the header is materialised.
    #[test]
    fn a_null_factory_entry_is_refused() {
        for (offset, name) in ENTRY_REQUIRED.iter().copied() {
            let header = entry_header();
            let mut buffer = Aligned::<512>::new();
            let ptr = plant(&mut buffer, &header);
            buffer.zero_slot(offset);
            // SAFETY: the buffer holds a full header with one function-pointer slot zeroed.
            let err = unsafe { read_entry(ptr) }.unwrap_err();
            assert_eq!(err.kind(), RuntimeErrorKind::Protocol);
            assert!(err.message().contains(name), "{err} should name `{name}`");
        }
    }

    /// The order of the checks is load-bearing: a header that is both undersized *and*
    /// has bad magic must report the size, because reading `magic` at offset 16 of a
    /// 4-byte structure is exactly the read the rule exists to prevent.
    #[test]
    fn size_is_checked_before_any_other_field() {
        let mut header = entry_header();
        header.magic = 0;
        header.abi_version_major = 9;
        let mut buffer = Aligned::<512>::new();
        let ptr = plant(&mut buffer, &header);
        buffer.set_declared_size(4);
        // SAFETY: the buffer holds a full header; the declared size is what lies.
        let err = unsafe { read_entry(ptr) }.unwrap_err();
        assert_eq!(err.kind(), RuntimeErrorKind::AbiMismatch);
        assert!(
            err.message().contains("v1.0 minimum"),
            "the size rule must fire first, got: {err}"
        );
    }

    #[test]
    fn loading_a_file_that_is_not_a_library_fails_without_panicking() {
        let path = std::env::temp_dir().join("daux-runtime-not-a-library.bin");
        std::fs::write(&path, b"this is not a dynamic library").expect("temp file");
        let err = AxtModule::open(&path, None).expect_err("a text file is not a module");
        assert!(
            matches!(
                err.kind(),
                RuntimeErrorKind::Library | RuntimeErrorKind::MissingEntry
            ),
            "unexpected kind: {err}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn loading_a_missing_file_reports_the_path() {
        let path = std::env::temp_dir().join("daux-runtime-definitely-absent.dll");
        let _ = std::fs::remove_file(&path);
        let err = AxtModule::open(&path, None).expect_err("no such file");
        assert_eq!(err.kind(), RuntimeErrorKind::Library);
        assert_eq!(err.path(), Some(path.as_path()));
    }
}
