//! The factory: one per module, and the only thing that creates instances.

use core::ffi::c_void;
use core::mem::offset_of;
use std::sync::Arc;

use daux_abi::{
    DAUX_OK, DauxFactoryApiV1, DauxFactoryV1, DauxPluginApiV1, DauxPluginDescriptorV1,
    DauxPluginV1, DauxStrView,
};
use daux_core::PluginDescriptor;

use crate::descriptor::to_plugin_descriptor;
use crate::error::{RuntimeError, RuntimeErrorKind, RuntimeResult};
use crate::host::HostBridge;
use crate::module::AxtModule;
use crate::plugin::LoadedPlugin;
use crate::probe::{RequiredFn, read_table};

/// Non-optional entries of [`DauxFactoryApiV1`]. `get_extension` is `Option` in the ABI and
/// is deliberately absent.
const FACTORY_REQUIRED: &[RequiredFn] = &[
    (offset_of!(DauxFactoryApiV1, plugin_count), "plugin_count"),
    (offset_of!(DauxFactoryApiV1, descriptor), "descriptor"),
    (offset_of!(DauxFactoryApiV1, create_plugin), "create_plugin"),
];

/// Non-optional entries of [`DauxPluginApiV1`]. Every entry of the instance table is
/// mandatory in ABI v1.
const PLUGIN_REQUIRED: &[RequiredFn] = &[
    (offset_of!(DauxPluginApiV1, init), "init"),
    (offset_of!(DauxPluginApiV1, destroy), "destroy"),
    (offset_of!(DauxPluginApiV1, activate), "activate"),
    (offset_of!(DauxPluginApiV1, deactivate), "deactivate"),
    (
        offset_of!(DauxPluginApiV1, start_processing),
        "start_processing",
    ),
    (
        offset_of!(DauxPluginApiV1, stop_processing),
        "stop_processing",
    ),
    (offset_of!(DauxPluginApiV1, reset), "reset"),
    (offset_of!(DauxPluginApiV1, process), "process"),
    (offset_of!(DauxPluginApiV1, get_extension), "get_extension"),
    (
        offset_of!(DauxPluginApiV1, on_main_thread),
        "on_main_thread",
    ),
];

/// The state a factory owns, shared by every instance it created.
///
/// This is the type that makes `abi-v1` §16.1 hold by construction. It holds the module, so
/// the library cannot be unloaded while the factory lives; every
/// [`LoadedPlugin`](crate::LoadedPlugin) holds one of these, so the factory cannot be
/// destroyed while an instance lives; and it holds the [`HostBridge`], so the `DauxHostV1`
/// the module was handed stays valid until `destroy_factory` has returned.
#[derive(Debug)]
pub(crate) struct FactoryInner {
    module: Arc<AxtModule>,
    /// Never read after construction — its whole job is to outlive `destroy_factory`.
    host: HostBridge,
    factory: DauxFactoryV1,
    api: DauxFactoryApiV1,
}

// SAFETY: the raw pointers in `factory`/`api` address memory the module owns for as long as
// the module is loaded (`abi-v1` §2.3), and `module` keeps it loaded. `HostBridge` is
// `Send + Sync`, and no field is thread-affine or mutated after construction, so a factory
// may be moved between threads.
unsafe impl Send for FactoryInner {}
// SAFETY: `Arc<FactoryInner>` is what every instance holds to keep the module loaded, and
// `Arc<T>: Send` requires `T: Sync`, so this bound is what lets a `LoadedPlugin` move to an
// audio thread at all. Sharing is sound: `FactoryInner` is immutable after construction, and
// the two entries reachable through a shared reference — `plugin_count` and `descriptor` —
// are `[any-thread]`, which `abi-v1` §15 defines as safe from any thread *including
// concurrently*. `create_plugin` is `[main-thread]`; Rust cannot express "main thread only",
// so that obligation stays where the ABI puts it, on the host, and is restated on the
// method.
unsafe impl Sync for FactoryInner {}

impl FactoryInner {
    /// The module this factory lives in, and keeps loaded.
    pub(crate) const fn module(&self) -> &Arc<AxtModule> {
        &self.module
    }
}

impl Drop for FactoryInner {
    fn drop(&mut self) {
        // SAFETY: `destroy_factory` comes from the entry header this crate validated at
        // load, and `factory` is exactly the value the matching `create_factory` produced.
        // Every instance holds an `Arc<FactoryInner>`, so reaching this destructor proves
        // the last one is gone — which is the precondition `abi-v1` §4 states. The module
        // is unloaded only after this returns, because `module` is dropped after the body.
        unsafe { (self.module.entry().destroy_factory)(self.factory) }
    }
}

/// A live factory inside a loaded module. [main-thread]
///
/// Cloning is cheap and shares the same factory; the module is unloaded when the last
/// clone, and every instance created from it, is gone.
#[derive(Clone, Debug)]
pub struct LoadedFactory {
    inner: Arc<FactoryInner>,
}

impl LoadedFactory {
    /// Calls `create_factory` and validates what came back. [main-thread]
    ///
    /// `host` is moved in because `abi-v1` §4 requires the `DauxHostV1` to stay valid until
    /// `destroy_factory` returns; the factory therefore owns it and drops it afterwards.
    ///
    /// # Errors
    ///
    /// Whatever status `create_factory` returned, and
    /// [`RuntimeErrorKind::Protocol`] when it reports success but leaves the interface
    /// empty or publishes a table this host may not call into.
    pub fn create(module: Arc<AxtModule>, host: HostBridge) -> RuntimeResult<Self> {
        let mut factory = DauxFactoryV1::null();
        // SAFETY: `create_factory` is the entry this crate validated at load, and the
        // module is kept loaded by `module`. `host.as_raw()` addresses the bridge's boxed
        // interface, which the factory takes ownership of below and keeps alive until
        // `destroy_factory`. `factory` is a host-owned out-parameter.
        let status = unsafe { (module.entry().create_factory)(host.as_raw(), &raw mut factory) };
        if status.0 != DAUX_OK.0 {
            return Err(
                RuntimeError::from_status("create_factory", status.0).with_path(module.path())
            );
        }
        if factory.api.is_null() {
            return Err(RuntimeError::protocol(
                "create_factory reported success but published no table",
            )
            .with_path(module.path()));
        }

        // SAFETY: `factory.api` is non-null and, per `abi-v1` §2.3, addresses an immutable
        // table the module owns for as long as it stays loaded, which `module` guarantees.
        let api = match unsafe { read_table(factory.api, "DauxFactoryApiV1", FACTORY_REQUIRED) } {
            Ok(api) => api,
            Err(e) => {
                // The table is unusable, but the factory object behind it exists and must
                // not be leaked. `destroy_factory` lives on the *entry*, which was validated
                // at load, so calling it is sound even though the factory table is not.
                // SAFETY: as in `FactoryInner::drop`; no instance can exist yet.
                unsafe { (module.entry().destroy_factory)(factory) }
                return Err(e.with_path(module.path()));
            }
        };

        Ok(Self {
            inner: Arc::new(FactoryInner {
                module,
                host,
                factory,
                api,
            }),
        })
    }

    /// The module this factory came from. [main-thread]
    #[inline]
    #[must_use]
    pub fn module(&self) -> &Arc<AxtModule> {
        &self.inner.module
    }

    /// The host services the module was handed. [main-thread]
    #[inline]
    #[must_use]
    pub fn host(&self) -> &HostBridge {
        &self.inner.host
    }

    /// How many plug-ins this module publishes. [any-thread]
    #[must_use]
    pub fn plugin_count(&self) -> usize {
        // SAFETY: `api` is the validated table this factory published and `factory.handle`
        // is its own handle; the module is kept loaded by `inner.module`.
        let count = unsafe { (self.inner.api.plugin_count)(self.inner.factory.handle) };
        count as usize
    }

    /// The descriptor at `index`. [any-thread]
    ///
    /// # Errors
    ///
    /// [`RuntimeErrorKind::InvalidArgument`] for an index the ABI cannot express,
    /// [`RuntimeErrorKind::NotFound`] when the module reports one, and
    /// [`RuntimeErrorKind::Protocol`] when it reports success but writes a descriptor a
    /// host cannot use.
    pub fn descriptor(&self, index: usize) -> RuntimeResult<PluginDescriptor> {
        let index = u32::try_from(index).map_err(|_| {
            RuntimeError::new(
                RuntimeErrorKind::InvalidArgument,
                format!("descriptor index {index} does not fit the ABI's u32"),
            )
        })?;
        // A host-owned, fully zeroed out-buffer with `size` already set, so a module that
        // reports success and writes nothing is caught by the validation below rather than
        // by reading whatever was on the stack.
        let mut raw = DauxPluginDescriptorV1::new();
        // SAFETY: as in `plugin_count`; `raw` is a live host-owned structure and no
        // allocation crosses the boundary (`abi-v1` §16.2).
        let status =
            unsafe { (self.inner.api.descriptor)(self.inner.factory.handle, index, &raw mut raw) };
        if status.0 != DAUX_OK.0 {
            return Err(RuntimeError::from_status("factory::descriptor", status.0));
        }
        if (raw.size as usize) < DauxPluginDescriptorV1::MIN_SIZE_V1_0 {
            return Err(RuntimeError::abi(format!(
                "descriptor {index} declares size {}, below the v1.0 minimum of {}",
                raw.size,
                DauxPluginDescriptorV1::MIN_SIZE_V1_0
            )));
        }
        to_plugin_descriptor(&raw)
    }

    /// Every descriptor the module publishes. [main-thread] — allocates.
    ///
    /// # Errors
    ///
    /// As [`LoadedFactory::descriptor`], for the first one that fails.
    pub fn descriptors(&self) -> RuntimeResult<Vec<PluginDescriptor>> {
        let count = self.plugin_count();
        let mut out = Vec::with_capacity(count);
        for index in 0..count {
            out.push(self.descriptor(index)?);
        }
        Ok(out)
    }

    /// Instantiates the plug-in with the given stable id and runs its `init`.
    /// [main-thread]
    ///
    /// # Errors
    ///
    /// [`RuntimeErrorKind::NotFound`] when no plug-in in the module answers to `id`,
    /// [`RuntimeErrorKind::Protocol`] when the module reports success but publishes no
    /// usable instance table, and whatever status `init` returned.
    pub fn create_plugin(&self, id: &str) -> RuntimeResult<LoadedPlugin> {
        let mut instance = DauxPluginV1::null();
        // SAFETY: as in `plugin_count`. The `DauxStrView` borrows `id`, which outlives the
        // call, and `abi-v1` §2 makes that borrow valid for exactly the call's duration.
        let status = unsafe {
            (self.inner.api.create_plugin)(
                self.inner.factory.handle,
                DauxStrView::from_str(id),
                &raw mut instance,
            )
        };
        if status.0 != DAUX_OK.0 {
            return Err(RuntimeError::from_status("create_plugin", status.0));
        }
        if instance.api.is_null() {
            return Err(RuntimeError::protocol(format!(
                "create_plugin(`{id}`) reported success but published no instance table"
            )));
        }

        // SAFETY: `instance.api` is non-null and addresses an immutable table the module
        // owns while it stays loaded, which `inner.module` guarantees.
        //
        // A table that fails validation leaves the instance behind: `destroy` lives in the
        // very table this host has just judged unusable, so calling it would be exactly the
        // undefined behaviour the validation exists to prevent. Leaking inside a module
        // that is already broken is the lesser failure.
        let api = unsafe { read_table(instance.api, "DauxPluginApiV1", PLUGIN_REQUIRED) }?;

        // SAFETY: `api` has been validated, so `init` is a real entry, and `instance.handle`
        // is the token the same call produced.
        let status = unsafe { (api.init)(instance.handle) };
        if status.0 != DAUX_OK.0 {
            // `abi-v1` §7: the instance exists but is not usable. It was never activated,
            // so `destroy` is the correct and only cleanup.
            // SAFETY: `destroy` comes from the validated table and the instance has not been
            // activated, which is the precondition `abi-v1` §7 states.
            unsafe { (api.destroy)(instance.handle) }
            return Err(RuntimeError::from_status("plugin::init", status.0));
        }

        Ok(LoadedPlugin::new(Arc::clone(&self.inner), instance, api))
    }

    /// The factory-level extension table for `id`, or null. [any-thread]
    ///
    /// `get_extension` is optional at factory level, so a module that does not implement it
    /// at all answers null too.
    #[must_use]
    pub fn extension(&self, id: &str) -> *const c_void {
        let Some(entry) = self.inner.api.get_extension else {
            return core::ptr::null();
        };
        // SAFETY: the entry was published by the module in the table this crate validated,
        // and the `DauxStrView` borrows `id` for exactly the duration of the call.
        unsafe { entry(self.inner.factory.handle, DauxStrView::from_str(id)) }
    }
}
