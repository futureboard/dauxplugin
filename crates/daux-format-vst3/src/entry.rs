//! The exported symbols of a VST3 binary, and the macro that emits them.
//!
//! A VST3 module exports three functions on Windows and Linux and two on macOS. This module
//! holds their bodies; [`crate::export_entry`] emits the `#[unsafe(no_mangle)]` shims that
//! call them, so that a plug-in crate never writes an `extern "system" fn` of its own.
//!
//! Every shim is total: it cannot panic (the bodies are wrapped), it cannot return an
//! uninitialised pointer, and calling `ExitDll` while instances are still alive does not free
//! anything — objects own themselves through their reference counts, exactly as COM requires.

use core::ffi::c_void;

use daux_plugin_api::DauxFactory;

use crate::factory::Vst3Factory;

/// `[main-thread]` The body of the exported `GetPluginFactory`.
///
/// Returns a fresh factory carrying one reference the host owns, or null if constructing it
/// panicked — which a host reads as "this module exports nothing" and moves on from, rather
/// than crashing its scan.
#[must_use]
pub fn get_plugin_factory<F: DauxFactory + Default>() -> *mut c_void {
    std::panic::catch_unwind(|| Vst3Factory::create(Box::new(F::default())))
        .unwrap_or(core::ptr::null_mut())
}

/// `[main-thread]` The body of the exported `InitDll` / `ModuleEntry` / `bundleEntry`.
///
/// There is nothing to initialise: a DAUx module has no global state, which is what lets
/// hundreds of instances coexist. Answering `true` is what a host needs to hear.
#[must_use]
pub const fn init_module() -> bool {
    true
}

/// `[main-thread]` The body of the exported `ExitDll` / `ModuleExit` / `bundleExit`.
///
/// Also nothing to do, and deliberately so: any object the host still holds owns itself
/// through its reference count, so tearing something down here would free memory the host is
/// about to use.
#[must_use]
pub const fn exit_module() -> bool {
    true
}

/// Emits the exported entry points of a VST3 binary for a [`DauxFactory`] type.
///
/// The type must implement [`DauxFactory`] and [`Default`] — the VST3 ABI creates the factory
/// with no arguments, so everything a plug-in module needs it must be able to build for
/// itself.
///
/// ```ignore
/// use daux_plugin_api::SingleFactory;
///
/// daux_format_vst3::export_entry!(SingleFactory<MyPlugin>);
/// ```
///
/// Expands to `GetPluginFactory` plus the platform's module hooks: `InitDll`/`ExitDll` on
/// Windows, `ModuleEntry`/`ModuleExit` on Linux, `bundleEntry`/`bundleExit` on macOS. The
/// names are dictated by the format and are therefore `#[unsafe(no_mangle)]`; a binary may
/// contain exactly one expansion of this macro.
#[macro_export]
macro_rules! export_entry {
    ($factory:ty) => {
        /// The VST3 entry point: hands the host this module's factory.
        ///
        /// # Safety
        ///
        /// Called by the host through the module's export table. The returned pointer carries
        /// one reference the host owns and must release.
        #[unsafe(no_mangle)]
        pub extern "system" fn GetPluginFactory() -> *mut ::core::ffi::c_void {
            $crate::entry::get_plugin_factory::<$factory>()
        }

        /// Windows module initialisation.
        #[cfg(target_os = "windows")]
        #[unsafe(no_mangle)]
        pub extern "system" fn InitDll() -> bool {
            $crate::entry::init_module()
        }

        /// Windows module teardown.
        #[cfg(target_os = "windows")]
        #[unsafe(no_mangle)]
        pub extern "system" fn ExitDll() -> bool {
            $crate::entry::exit_module()
        }

        /// macOS bundle initialisation.
        #[cfg(target_os = "macos")]
        #[unsafe(no_mangle)]
        #[allow(non_snake_case)]
        pub extern "system" fn bundleEntry(_bundle: *mut ::core::ffi::c_void) -> bool {
            $crate::entry::init_module()
        }

        /// macOS bundle teardown.
        #[cfg(target_os = "macos")]
        #[unsafe(no_mangle)]
        #[allow(non_snake_case)]
        pub extern "system" fn bundleExit() -> bool {
            $crate::entry::exit_module()
        }

        /// Linux module initialisation.
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        #[unsafe(no_mangle)]
        #[allow(non_snake_case)]
        pub extern "system" fn ModuleEntry(_handle: *mut ::core::ffi::c_void) -> bool {
            $crate::entry::init_module()
        }

        /// Linux module teardown.
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        #[unsafe(no_mangle)]
        #[allow(non_snake_case)]
        pub extern "system" fn ModuleExit() -> bool {
            $crate::entry::exit_module()
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_module_hooks_have_nothing_to_do_and_say_so() {
        assert!(init_module());
        assert!(exit_module());
        // Idempotent, because hosts call them more than once.
        assert!(init_module());
        assert!(exit_module());
    }
}
