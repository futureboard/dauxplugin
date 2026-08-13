//! Native AXT export: the reference implementation of the DAUx C ABI.
//!
//! This crate turns a [`DauxFactory`](daux_plugin_api::DauxFactory) into a `.axt` module: it
//! emits `daux_plugin_entry_v1`, builds the `DauxFactoryApiV1` and `DauxPluginApiV1` function
//! tables, translates the format-neutral object model of `daux-core` into the flat `#[repr(C)]`
//! structures of `daux-abi`, and provides the standard extensions of
//! `docs/specifications/abi-v1.md` §11.
//!
//! AXT is the **native** format, so this adapter is also the yardstick the VST3 and CLAP
//! adapters are measured against: where the spec and a convenience disagree, the spec wins.
//!
//! # Exporting a plug-in
//!
//! ```
//! use daux_plugin_api::prelude::*;
//! use daux_plugin_api::SingleFactory;
//!
//! #[derive(Default)]
//! struct Bypass;
//!
//! impl DauxProcessor for Bypass {
//!     fn prepare(&mut self, config: &ProcessConfig) -> DauxResult<()> { config.validate() }
//!     fn process<'a>(
//!         &mut self,
//!         _ctx: &ProcessContext<'a>,
//!         audio: &mut AudioBuses<'a, f32>,
//!         _events: &mut ProcessEvents<'a>,
//!     ) -> ProcessStatus {
//!         audio.silence_outputs();
//!         ProcessStatus::ContinueIfNotQuiet
//!     }
//! }
//! impl Params for Bypass {
//!     fn param_refs(&self) -> Vec<(ParamId, &dyn Param)> { Vec::new() }
//! }
//! impl DauxController for Bypass {
//!     fn params(&self) -> &dyn Params { self }
//!     fn save_state(&self, _w: &mut StateWriter) -> DauxResult<()> { Ok(()) }
//!     fn load_state(&mut self, _r: &StateReader) -> DauxResult<()> { Ok(()) }
//! }
//! impl DauxPlugin for Bypass {
//!     fn descriptor() -> PluginDescriptor {
//!         PluginDescriptor::builder("com.example.bypass", "Bypass").build().unwrap()
//!     }
//!     fn bus_layout(&self) -> BusLayout { BusLayout::stereo_effect() }
//!     fn processor(&mut self) -> &mut dyn DauxProcessor { self }
//!     fn controller(&mut self) -> &mut dyn DauxController { self }
//! }
//!
//! daux_format_axt::export_entry!(SingleFactory<Bypass>);
//!
//! // The host's first four steps, which is all a `.axt` module has to get right to load.
//! unsafe extern "C" {
//!     fn daux_plugin_entry_v1() -> *const daux_abi::DauxPluginEntryV1;
//! }
//! # fn main() {
//! // SAFETY: the symbol is the one `export_entry!` just defined; it returns a `'static`.
//! let entry = unsafe { &*daux_plugin_entry_v1() };
//! assert!(entry.check().is_ok());
//!
//! let mut factory = daux_abi::DauxFactoryV1::null();
//! // SAFETY: a null host is allowed, and `factory` is a writable local.
//! let status = unsafe { (entry.create_factory)(core::ptr::null(), &raw mut factory) };
//! assert!(status.is_ok());
//! // SAFETY: the table came from `create_factory`.
//! let api = unsafe { &*factory.api };
//! // SAFETY: the handle is the one just created.
//! assert_eq!(unsafe { (api.plugin_count)(factory.handle) }, 1);
//! // SAFETY: no instance was created, so the factory may be destroyed.
//! unsafe { (entry.destroy_factory)(factory) };
//! # }
//! ```
//!
//! The macro emits exactly one exported symbol, `daux_plugin_entry_v1`, whose returned
//! [`DauxPluginEntryV1`](daux_abi::DauxPluginEntryV1) is a `static`: non-null, immortal and
//! identical across calls, as abi-v1 §4 requires. The factory type must be [`Default`],
//! because the ABI's `create_factory` takes no arguments beyond the host interface.
//!
//! # The rules this adapter obeys
//!
//! * **Nothing unwinds across the boundary** (abi-v1 §17). Every exported function wraps its
//!   whole body in [`catch_unwind`](std::panic::catch_unwind); a caught panic becomes
//!   `DAUX_ERR_PANIC` (or `DAUX_PROCESS_ERROR` from `process`) and **poisons** the object, which
//!   then refuses every later call with `DAUX_ERR_INVALID_STATE` rather than re-entering
//!   plug-in code that has already broken its own invariants.
//! * **No allocation crosses the boundary** (abi-v1 §16.2). Metadata is written into
//!   caller-owned fixed buffers; state travels through the host-owned
//!   [`DauxStreamV1`](daux_abi::DauxStreamV1).
//! * **Parameter values are plain**, never normalised (abi-v1 §11.2), so changing a curve in a
//!   later version cannot corrupt automation written by an earlier one.
//! * **`process` allocates nothing.** The per-block bus views live in
//!   [`FixedVec`](daux_plugin_api::daux_rt::FixedVec)s sized once in `activate`; the event
//!   adapters are stack values that borrow the host's lists.
//! * **No format type leaks into `daux-core`.** Everything in this crate translates in one
//!   direction or the other and stops at the crate boundary.
//! * **No global mutable state.** The only `static`s are the immutable function tables; every
//!   piece of state lives behind the handle the host was given, so hundreds of instances
//!   coexist in one process.
//!
//! # Threading
//!
//! Thread annotations follow abi-v1 §15. Calls for one instance are never concurrent with each
//! other; calls for different instances may be. The two entries abi-v1 marks `[any-thread]` on
//! a *plug-in instance* — `DauxPluginApiV1::get_extension` and `DauxTailApiV1::get` — are
//! served from the same instance state as everything else, so a host must not call them
//! concurrently with another call on the *same* instance. Factory-level entries really are
//! `[any-thread]`: the factory is shared behind `&self` and its poison flag is atomic.

#![deny(unsafe_op_in_unsafe_fn)]

mod audio;
mod compat;
mod entry;
mod events;
mod ext;
mod factory;
mod host;
mod instance;
mod panic;
mod stream;
mod transport;

#[cfg(test)]
mod tests;

pub use crate::compat::{CompatibilityWarning, compatibility_report, warning_code};
pub use crate::entry::{SDK_NAME, SDK_VERSION, entry_symbol};

/// The allocation tripwire, installed only while compiling this crate's tests, so that "a
/// block allocates nothing" is a checked assertion rather than a comment. Production builds
/// are untouched — a `#[global_allocator]` is a whole-program decision and this crate never
/// makes it for anyone else.
#[cfg(test)]
#[global_allocator]
static TEST_ALLOCATOR: daux_plugin_api::daux_rt::CountingAllocator =
    daux_plugin_api::daux_rt::CountingAllocator;

/// Emits the `daux_plugin_entry_v1` export for `$factory`.
///
/// `$factory` must implement [`DauxFactory`](daux_plugin_api::DauxFactory) and [`Default`];
/// [`SingleFactory`](daux_plugin_api::SingleFactory) and a wrapper around
/// [`PluginRegistry`](daux_plugin_api::PluginRegistry) both qualify.
///
/// Invoke it exactly once per dynamic library. A second invocation is a duplicate-symbol link
/// error, which is the correct outcome: a module has one entry point.
///
/// [main-thread]
#[macro_export]
macro_rules! export_entry {
    ($factory:ty) => {
        const _: () = {
            /// The ABI's `create_factory` (abi-v1 §4), instantiated for this module's factory.
            unsafe extern "C" fn daux_axt_create_factory(
                host: *const $crate::__private::DauxHostV1,
                out_factory: *mut $crate::__private::DauxFactoryV1,
            ) -> $crate::__private::DauxStatus {
                // SAFETY: `host` and `out_factory` come straight from the host's call and are
                // passed on unchanged; the helper documents and checks the same contract the
                // ABI states (null-tolerant `host`, writable `out_factory`).
                unsafe {
                    $crate::__private::create_factory(host, out_factory, || {
                        ::std::boxed::Box::new(<$factory as ::core::default::Default>::default())
                    })
                }
            }

            /// The ABI's `destroy_factory` (abi-v1 §4).
            unsafe extern "C" fn daux_axt_destroy_factory(
                factory: $crate::__private::DauxFactoryV1,
            ) {
                // SAFETY: the interface pair is the one `daux_axt_create_factory` produced, as
                // the ABI requires; the helper re-checks the handle before touching it.
                unsafe { $crate::__private::destroy_factory(factory) }
            }

            /// The module header. `static`, so the pointer below is immortal and stable.
            static ENTRY: $crate::__private::DauxPluginEntryV1 =
                $crate::__private::entry_v1(daux_axt_create_factory, daux_axt_destroy_factory);

            #[unsafe(no_mangle)]
            pub extern "C" fn daux_plugin_entry_v1() -> *const $crate::__private::DauxPluginEntryV1
            {
                &raw const ENTRY
            }
        };
    };
}

/// Implementation detail of [`export_entry!`]. Not a public API and not covered by semantic
/// versioning.
#[doc(hidden)]
pub mod __private {
    pub use crate::entry::{create_factory, destroy_factory, entry_v1};
    pub use daux_abi::{DauxFactoryV1, DauxHostV1, DauxPluginEntryV1, DauxStatus};
}
