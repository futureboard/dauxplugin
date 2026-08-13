//! The factory function table (abi-v1 §5).
//!
//! One [`FactoryState`] lives behind the [`DauxFactoryHandle`] the host was given. It is shared
//! (`&self`, never `&mut`) because `plugin_count` and `descriptor` are `[any-thread]`, so its
//! poison flag has to be atomic.

use std::sync::atomic::{AtomicBool, Ordering};

use daux_abi::{
    DAUX_ERR_ABI_MISMATCH, DAUX_ERR_NOT_FOUND, DAUX_OK, DauxFactoryApiV1, DauxFactoryHandle,
    DauxPluginDescriptorV1, DauxPluginV1, DauxStatus, DauxStrView,
};
use daux_plugin_api::DauxFactory;

use crate::compat::write_descriptor;
use crate::host::HostBridge;
use crate::instance;
use crate::panic::{Refusal, catch_reporting, status_of_error};

/// Everything behind a `DauxFactoryHandle`.
pub(crate) struct FactoryState {
    factory: Box<dyn DauxFactory>,
    host: HostBridge,
    /// Set when a panic escaped one of this factory's entries (abi-v1 §17.3).
    poisoned: AtomicBool,
}

impl FactoryState {
    /// [main-thread] Wraps the plug-in module's factory.
    pub(crate) fn new(factory: Box<dyn DauxFactory>, host: HostBridge) -> Self {
        Self {
            factory,
            host,
            poisoned: AtomicBool::new(false),
        }
    }

    /// [any-thread] `true` once a panic has been caught in this factory.
    fn is_poisoned(&self) -> bool {
        self.poisoned.load(Ordering::Acquire)
    }

    /// [any-thread] Marks the factory unusable. One-way and idempotent.
    fn poison(&self) {
        self.poisoned.store(true, Ordering::Release);
    }
}

/// [any-thread] Runs `body` with the factory behind `handle`, honouring abi-v1 §17.
///
/// Refuses a null handle, refuses everything once poisoned, and poisons on the way out of a
/// caught panic so that the *next* call is refused rather than re-entering broken plug-in code.
///
/// # Safety
///
/// `handle` is null or a [`DauxFactoryHandle`] this module produced in `create_factory` and has
/// not yet destroyed.
unsafe fn with_factory<R: Refusal>(
    handle: DauxFactoryHandle,
    body: impl FnOnce(&FactoryState) -> R,
) -> R {
    if handle.is_null() {
        return R::INVALID_ARG;
    }
    // SAFETY: the caller guarantees the handle is one of ours and still live, so it points at a
    // `FactoryState` this module allocated with `Box::new`. Only a shared reference is taken,
    // which is what makes the `[any-thread]` entries legal.
    let state = unsafe { &*handle.as_ptr().cast::<FactoryState>() };
    if state.is_poisoned() {
        return R::POISONED;
    }
    match catch_reporting(|| body(state)) {
        Ok(value) => value,
        Err(()) => {
            state.poison();
            R::PANICKED
        }
    }
}

/// [any-thread] Number of plug-ins in this module (abi-v1 §5).
///
/// # Safety
///
/// See [`with_factory`].
unsafe extern "C" fn plugin_count(f: DauxFactoryHandle) -> u32 {
    // SAFETY: forwarded verbatim from this function's own contract.
    unsafe {
        with_factory(f, |state| {
            u32::try_from(state.factory.plugin_count()).unwrap_or(u32::MAX)
        })
    }
}

/// [any-thread] Fills `out` with the descriptor at `index` (abi-v1 §5).
///
/// `out.size` is honoured as the caller's declaration of how large its structure is: a value
/// below the v1.0 minimum means the caller could not hold what we are about to write, and is
/// refused with `DAUX_ERR_ABI_MISMATCH` rather than overrun. Zero means "not declared", which
/// is what a host that memset the structure produces, and is treated as v1.0.
///
/// # Safety
///
/// `out` is null or points at a writable, aligned [`DauxPluginDescriptorV1`] of at least
/// `out.size` bytes (or of v1.0 size when `out.size` is zero). See also [`with_factory`].
unsafe extern "C" fn descriptor(
    f: DauxFactoryHandle,
    index: u32,
    out: *mut DauxPluginDescriptorV1,
) -> DauxStatus {
    let body = |state: &FactoryState| {
        if out.is_null() {
            return DauxStatus::INVALID_ARG;
        }
        // SAFETY: `size` is the first field of the structure and is present in every revision,
        // so it can be read before the rest is trusted (abi-v1 §3); this function's contract
        // guarantees the pointer is aligned.
        let declared = unsafe { (&raw const (*out).size).read() };
        if declared != 0 && (declared as usize) < DauxPluginDescriptorV1::MIN_SIZE_V1_0 {
            return DAUX_ERR_ABI_MISMATCH;
        }
        let Some(source) = state.factory.descriptor(index as usize) else {
            return DAUX_ERR_NOT_FOUND;
        };
        // SAFETY: `out` is non-null and, per this function's contract, points at a writable,
        // aligned descriptor large enough for the v1.0 revision — which the size check above
        // just established. Nothing is read out of it, so its contents may be garbage.
        let out = unsafe { &mut *out };
        write_descriptor(&source, out);
        DAUX_OK
    };
    // SAFETY: forwarded verbatim from this function's own contract.
    unsafe { with_factory(f, body) }
}

/// [main-thread] Instantiates the plug-in with the given permanent id (abi-v1 §5).
///
/// # Safety
///
/// `id` points at `id.len` readable bytes for the duration of the call; `out` is null or points
/// at a writable, aligned [`DauxPluginV1`], written only on success. See also [`with_factory`].
unsafe extern "C" fn create_plugin(
    f: DauxFactoryHandle,
    id: DauxStrView,
    out: *mut DauxPluginV1,
) -> DauxStatus {
    let body = |state: &FactoryState| {
        if out.is_null() {
            return DauxStatus::INVALID_ARG;
        }
        // SAFETY: this function's contract guarantees the view is readable for the call; the
        // borrow ends before `create` returns, well inside that window.
        let Some(id) = (unsafe { id.as_str() }) else {
            return DauxStatus::INVALID_ARG;
        };
        let plugin = match state.factory.create(id) {
            Ok(plugin) => plugin,
            Err(err) => return status_of_error(&err),
        };
        let descriptor = state
            .factory
            .descriptors()
            .into_iter()
            .find(|d| d.id == *id);
        let interface = instance::create(plugin, descriptor, state.host.clone());
        // SAFETY: `out` is non-null and, per this function's contract, writable and aligned.
        // Ownership of the instance passes to the host until it calls `DauxPluginApiV1::destroy`.
        unsafe { out.write(interface) };
        DAUX_OK
    };
    // SAFETY: forwarded verbatim from this function's own contract.
    unsafe { with_factory(f, body) }
}

/// The factory table every `.axt` this crate builds hands out.
///
/// A `static`, so its address is valid for as long as the module is loaded, which is exactly
/// what abi-v1 §2.3 requires of a function table. `get_extension` is null: this crate defines no
/// factory-level extension, and a null entry is how the ABI spells "not supported".
pub(crate) static FACTORY_API: DauxFactoryApiV1 = DauxFactoryApiV1 {
    size: DauxFactoryApiV1::SIZE,
    _pad0: 0,
    plugin_count,
    descriptor,
    create_plugin,
    get_extension: None,
    reserved: [0; 6],
};
