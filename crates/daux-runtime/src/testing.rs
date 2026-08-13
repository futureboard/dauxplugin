//! Hand-built ABI structures for the crate's own tests.
//!
//! There is no example plug-in to load yet, and even once there is, the interesting cases
//! are the ones a conforming plug-in never produces: a header that lies about its size, a
//! function table with a null entry, a factory that reports success and writes nothing.
//! Those are unreachable through a real module, so the tests build the bytes directly and
//! point the loader at them.

use core::ffi::c_void;

use daux_abi::{
    DAUX_ABI_MAGIC, DAUX_ABI_VERSION_MAJOR, DAUX_ABI_VERSION_MINOR, DAUX_OK, DauxFactoryApiV1,
    DauxFactoryHandle, DauxFactoryV1, DauxHostV1, DauxName, DauxPluginDescriptorV1,
    DauxPluginEntryV1, DauxPluginV1, DauxStatus, DauxStrView, DauxVersion,
};

/// A byte buffer aligned like a real ABI structure, so tests exercise the layout a module
/// actually produces rather than an artificially misaligned one.
#[repr(C, align(16))]
pub(crate) struct Aligned<const N: usize> {
    bytes: [u8; N],
}

impl<const N: usize> Aligned<N> {
    /// A zeroed buffer.
    pub(crate) const fn new() -> Self {
        Self { bytes: [0; N] }
    }

    /// Overwrites the `size` word every growable ABI structure carries at offset 0.
    pub(crate) fn set_declared_size(&mut self, size: u32) {
        self.bytes[..4].copy_from_slice(&size.to_ne_bytes());
    }

    /// Zeroes the pointer-wide slot at `offset`, i.e. nulls one function-table entry.
    pub(crate) fn zero_slot(&mut self, offset: usize) {
        self.bytes[offset..offset + size_of::<usize>()].fill(0);
    }

    /// Writes arbitrary bytes at `offset`, used to fake a newer revision's tail.
    pub(crate) fn write_at(&mut self, offset: usize, bytes: &[u8]) {
        self.bytes[offset..offset + bytes.len()].copy_from_slice(bytes);
    }
}

/// Copies `value` into `buffer` and returns a pointer to it, the way a module's `static`
/// looks from the host's side.
pub(crate) fn plant<T: Copy, const N: usize>(buffer: &mut Aligned<N>, value: &T) -> *const T {
    assert!(
        size_of::<T>() <= N,
        "test buffer is too small for {T:?}",
        T = std::any::type_name::<T>()
    );
    assert!(align_of::<T>() <= 16, "test buffer is under-aligned");
    // SAFETY: `value` is a live, fully initialised `T` and `buffer` owns at least
    // `size_of::<T>()` bytes, checked above. The regions cannot overlap: `value` is a
    // caller-owned local and `buffer` is a distinct `&mut`. The buffer is 16-byte aligned
    // and `T`'s alignment is at most that, so the destination is suitably aligned.
    unsafe {
        core::ptr::copy_nonoverlapping(
            (value as *const T).cast::<u8>(),
            buffer.bytes.as_mut_ptr(),
            size_of::<T>(),
        );
    }
    buffer.bytes.as_ptr().cast::<T>()
}

unsafe extern "C" fn stub_create_factory(
    _host: *const DauxHostV1,
    _out: *mut DauxFactoryV1,
) -> DauxStatus {
    DAUX_OK
}

unsafe extern "C" fn stub_destroy_factory(_factory: DauxFactoryV1) {}

/// A header that satisfies every rejection rule of `abi-v1` §3.
pub(crate) fn entry_header() -> DauxPluginEntryV1 {
    DauxPluginEntryV1 {
        size: DauxPluginEntryV1::SIZE,
        abi_version_major: DAUX_ABI_VERSION_MAJOR,
        abi_version_minor: DAUX_ABI_VERSION_MINOR,
        _pad0: 0,
        magic: DAUX_ABI_MAGIC,
        sdk_name: DauxName::new("daux-test-sdk"),
        sdk_version: DauxVersion::new(0, 1, 0, 0),
        create_factory: stub_create_factory,
        destroy_factory: stub_destroy_factory,
        reserved: [0; 8],
    }
}

unsafe extern "C" fn stub_plugin_count(_f: DauxFactoryHandle) -> u32 {
    0
}

unsafe extern "C" fn stub_descriptor(
    _f: DauxFactoryHandle,
    _index: u32,
    _out: *mut DauxPluginDescriptorV1,
) -> DauxStatus {
    DAUX_OK
}

unsafe extern "C" fn stub_create_plugin(
    _f: DauxFactoryHandle,
    _id: DauxStrView,
    _out: *mut DauxPluginV1,
) -> DauxStatus {
    DAUX_OK
}

unsafe extern "C" fn stub_factory_extension(
    _f: DauxFactoryHandle,
    _id: DauxStrView,
) -> *const c_void {
    core::ptr::null()
}

/// A factory table with every non-optional entry filled in.
pub(crate) fn factory_api() -> DauxFactoryApiV1 {
    DauxFactoryApiV1 {
        size: DauxFactoryApiV1::SIZE,
        _pad0: 0,
        plugin_count: stub_plugin_count,
        descriptor: stub_descriptor,
        create_plugin: stub_create_plugin,
        get_extension: Some(stub_factory_extension),
        reserved: [0; 6],
    }
}
