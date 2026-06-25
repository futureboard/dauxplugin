//! # daux-plugin
//!
//! Safe, ergonomic Rust SDK for writing **DAUx** plugins. You implement the
//! [`DauxPlugin`] trait and invoke [`daux_export_plugin!`] once; the macro
//! generates the C ABI entry point, factory and vtable and bridges every call
//! to your type. All `unsafe` FFI lives in this crate, not your plugin.
//!
//! ```ignore
//! use daux_plugin::*;
//!
//! struct MyGain { gain_db: f64 }
//!
//! impl DauxPlugin for MyGain {
//!     fn descriptor() -> PluginDescriptor { /* ... */ }
//!     fn parameters() -> Vec<ParamInfo> { /* ... */ }
//!     fn new() -> Self { MyGain { gain_db: 0.0 } }
//!     fn process(&mut self, _ctx: &ProcessContext, b: &mut Buffers) { /* ... */ }
//!     fn get_param(&self, _id: u32) -> f64 { self.gain_db }
//!     fn set_param(&mut self, _id: u32, v: f64) { self.gain_db = v; }
//! }
//!
//! daux_export_plugin!(MyGain);
//! ```

pub mod buffer;
pub mod export;
pub mod types;

pub use buffer::{Buffers, ProcessContext};
pub use types::{Caps, Category, ParamFlags, ParamInfo, PluginDescriptor};

/// Re-export of the raw bindings for advanced use and for the macro.
pub use daux_plugin_sys as sys;

/// The trait a Rust plugin implements. The wrapper bridges it to the C ABI.
///
/// Threading: `process` runs on the realtime audio thread and must not block or
/// allocate; everything else runs on a non-realtime thread and never
/// concurrently with `process`.
pub trait DauxPlugin: Sized + 'static {
    /// Static, instantiation-free descriptor (read by DAUxScan).
    fn descriptor() -> PluginDescriptor;

    /// Static parameter list. Order defines the index used by the host's
    /// `get_parameter_info(index)`.
    fn parameters() -> Vec<ParamInfo>;

    /// Construct a fresh instance.
    fn new() -> Self;

    /// Called before processing and whenever sample rate / block size change.
    fn prepare(&mut self, _sample_rate: f64, _max_block: u32) {}
    fn activate(&mut self) {}
    fn deactivate(&mut self) {}
    fn reset(&mut self) {}

    /// Realtime audio processing.
    fn process(&mut self, ctx: &ProcessContext, buffers: &mut Buffers);

    /// Return a parameter's current value in plain units.
    fn get_param(&self, id: u32) -> f64;
    /// Set a parameter's value (plain units). Implementations should clamp.
    fn set_param(&mut self, id: u32, value: f64);

    /// Serialize plugin state. Default: empty (no state).
    fn get_state(&self) -> Vec<u8> {
        Vec::new()
    }
    /// Restore plugin state. Return `false` on malformed data. Default: ok.
    fn set_state(&mut self, _data: &[u8]) -> bool {
        true
    }
}

/// Sync wrapper for ABI POD structs stored in `static`s.
///
/// The raw `daux_*` structs contain function pointers / `*mut c_void` and so
/// are not `Sync`, which a `static` requires. They are only ever read after
/// one-time initialization and the pointers they hold are valid for the life
/// of the module, so wrapping them as `Sync` is sound. `#[doc(hidden)]`: this
/// is a macro implementation detail.
#[doc(hidden)]
pub struct AbiStatic<T>(pub T);

// SAFETY: see the type docs - immutable after init, pointers are 'static.
// `OnceLock<T>: Sync` requires `T: Send + Sync`, so we assert both.
unsafe impl<T> Sync for AbiStatic<T> {}
unsafe impl<T> Send for AbiStatic<T> {}

/// Generate the C ABI surface (`daux_plugin_entry`, factory, vtable) for a type
/// implementing [`DauxPlugin`]. Invoke exactly once per cdylib.
#[macro_export]
macro_rules! daux_export_plugin {
    ($ty:ty) => {
        // Wrapped in an anonymous const so the generated items don't leak into
        // the user's module namespace (except the required #[no_mangle] entry).
        const _: () = {
            use ::std::sync::OnceLock;
            use $crate::sys;

            type __DauxPluginType = $ty;

            type AbiStatic<T> = $crate::AbiStatic<T>;

            static DESCRIPTOR: OnceLock<AbiStatic<sys::daux_plugin_descriptor>> = OnceLock::new();
            static VTABLE: OnceLock<AbiStatic<sys::daux_plugin_vtable>> = OnceLock::new();
            static FACTORY: OnceLock<AbiStatic<sys::daux_plugin_factory>> = OnceLock::new();

            extern "C" fn get_descriptor() -> *const sys::daux_plugin_descriptor {
                &DESCRIPTOR
                    .get_or_init(|| AbiStatic($crate::export::build_descriptor::<__DauxPluginType>()))
                    .0 as *const _
            }

            unsafe extern "C" fn create(
                host: *const sys::daux_host_callbacks,
                out: *mut sys::daux_plugin_instance,
            ) -> sys::daux_result {
                let vt = &VTABLE
                    .get_or_init(|| AbiStatic($crate::export::build_vtable::<__DauxPluginType>()))
                    .0 as *const _;
                $crate::export::create_instance::<__DauxPluginType>(host, out, vt)
            }

            unsafe extern "C" fn destroy(h: sys::daux_plugin_handle) -> sys::daux_result {
                $crate::export::destroy_instance::<__DauxPluginType>(h)
            }

            /// The single exported entry point resolved by the host loader.
            #[no_mangle]
            pub unsafe extern "C" fn daux_plugin_entry(
                host_abi_version: u32,
            ) -> *const sys::daux_plugin_factory {
                if sys::daux_version_major(host_abi_version) != sys::DAUX_ABI_VERSION_MAJOR {
                    return ::core::ptr::null();
                }
                &FACTORY
                    .get_or_init(|| {
                        AbiStatic(sys::daux_plugin_factory {
                            struct_size: ::core::mem::size_of::<sys::daux_plugin_factory>() as u32,
                            abi_version: sys::DAUX_ABI_VERSION,
                            get_descriptor: Some(get_descriptor),
                            create: Some(create),
                            destroy: Some(destroy),
                            reserved: [::core::ptr::null_mut(); 6],
                        })
                    })
                    .0 as *const _
            }
        };
    };
}
