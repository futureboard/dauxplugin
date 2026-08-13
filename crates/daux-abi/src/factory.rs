//! The factory function table (`abi-v1` §5).

use core::ffi::c_void;

use crate::compat::impl_abi_struct;
use crate::descriptor::DauxPluginDescriptorV1;
use crate::handle::{DauxFactoryHandle, DauxPluginV1};
use crate::status::DauxStatus;
use crate::string::DauxStrView;

/// Function table of a factory.
///
/// A plug-in instance is destroyed through
/// [`DauxPluginApiV1::destroy`](crate::DauxPluginApiV1), not through the factory. The
/// factory MUST outlive every instance it created.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DauxFactoryApiV1 {
    /// `size_of::<DauxFactoryApiV1>()` as written by the producer.
    pub size: u32,
    /// Reserved for alignment; MUST be zero.
    pub _pad0: u32,

    /// Number of plug-ins in this binary. [any-thread]
    pub plugin_count: unsafe extern "C" fn(f: DauxFactoryHandle) -> u32,

    /// Fills `out` with the descriptor at `index`. Lightweight: it MUST NOT instantiate
    /// DSP, load resources, or touch the GPU. [any-thread]
    pub descriptor: unsafe extern "C" fn(
        f: DauxFactoryHandle,
        index: u32,
        out: *mut DauxPluginDescriptorV1,
    ) -> DauxStatus,

    /// Instantiates the plug-in with the given stable id. [main-thread]
    pub create_plugin: unsafe extern "C" fn(
        f: DauxFactoryHandle,
        id: DauxStrView,
        out: *mut DauxPluginV1,
    ) -> DauxStatus,

    /// Factory-level extension lookup; null when unsupported. [any-thread]
    pub get_extension:
        Option<unsafe extern "C" fn(f: DauxFactoryHandle, id: DauxStrView) -> *const c_void>,

    /// Reserved for future minor revisions; MUST be all zero.
    pub reserved: [usize; 6],
}

impl_abi_struct!(DauxFactoryApiV1);
