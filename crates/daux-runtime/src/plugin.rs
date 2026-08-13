//! A live plug-in instance, and the lifecycle rules it must be driven by.

use core::ffi::c_void;
use std::sync::Arc;

use daux_abi::{
    DAUX_OK, DAUX_TAIL_INFINITE, DauxGuiApiV1, DauxLatencyApiV1, DauxParamsApiV1, DauxPluginApiV1,
    DauxPluginV1, DauxProcessConfigV1, DauxStateApiV1, DauxStatus, DauxStrView, DauxTailApiV1, ext,
};
use daux_core::{ProcessConfig, ProcessStatus, Tail, status};

use crate::block::HostBlock;
use crate::error::{RuntimeError, RuntimeErrorKind, RuntimeResult};
use crate::ext::{GUI_REQUIRED, GuiExt, PARAMS_REQUIRED, ParamsExt, STATE_REQUIRED, StateExt};
use crate::factory::FactoryInner;
use crate::probe::{RequiredFn, read_table};

/// Where an instance is in the lifecycle of `abi-v1` §7. [any-thread]
///
/// ```text
/// created ──init──> inactive ──activate──> active ──start_processing──> processing
///                      ^                      |                              |
///                      └──── deactivate ──────┘<──── stop_processing ────────┘
/// inactive ──destroy──> gone
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PluginState {
    /// `init` has run; no DSP resources are allocated.
    #[default]
    Inactive,
    /// `activate` has run; `process` may not be called yet.
    Active,
    /// `start_processing` has run; `process` is legal.
    Processing,
}

impl core::fmt::Display for PluginState {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Inactive => "inactive",
            Self::Active => "active",
            Self::Processing => "processing",
        })
    }
}

/// Non-optional entries of `daux.latency/1` and `daux.tail/1`.
const GET_ONLY_LATENCY: &[RequiredFn] = &[(core::mem::offset_of!(DauxLatencyApiV1, get), "get")];
const GET_ONLY_TAIL: &[RequiredFn] = &[(core::mem::offset_of!(DauxTailApiV1, get), "get")];

/// One instantiated plug-in. [main-thread] except where noted.
///
/// The instance holds a strong reference to its factory, which holds the module, so neither
/// can be destroyed or unloaded while this value exists — `abi-v1` §16.1 by construction.
///
/// Dropping the instance walks it back to `inactive` first: an instance destroyed while
/// processing is undefined behaviour on the plug-in's side, and a host that forgets is more
/// likely than a plug-in that survives it.
#[derive(Debug)]
pub struct LoadedPlugin {
    factory: Arc<FactoryInner>,
    plugin: DauxPluginV1,
    api: DauxPluginApiV1,
    config: Option<ProcessConfig>,
    state: PluginState,
    poisoned: bool,
}

// SAFETY: the pointers an instance holds address module-owned memory that stays valid while
// `factory` keeps the module loaded. `abi-v1` §15 states that calls for one instance are
// never concurrent and may move between threads between blocks, so a `LoadedPlugin` may be
// handed from the main thread to an audio thread — which is what `Send` expresses. It is
// deliberately **not** `Sync`: `&LoadedPlugin` shared across threads would allow exactly the
// concurrent calls §15 forbids.
unsafe impl Send for LoadedPlugin {}

impl LoadedPlugin {
    pub(crate) const fn new(
        factory: Arc<FactoryInner>,
        plugin: DauxPluginV1,
        api: DauxPluginApiV1,
    ) -> Self {
        Self {
            factory,
            plugin,
            api,
            config: None,
            state: PluginState::Inactive,
            poisoned: false,
        }
    }

    /// Where the instance is in its lifecycle. [any-thread]
    #[inline]
    #[must_use]
    pub const fn lifecycle(&self) -> PluginState {
        self.state
    }

    /// The module this instance came from, which it keeps loaded. [any-thread]
    #[inline]
    #[must_use]
    pub fn module(&self) -> &Arc<crate::AxtModule> {
        self.factory.module()
    }

    /// The configuration the instance was activated with, if it is active. [any-thread]
    #[inline]
    #[must_use]
    pub const fn config(&self) -> Option<&ProcessConfig> {
        self.config.as_ref()
    }

    /// `true` when the instance reported `DAUX_ERR_PANIC` and refuses further work.
    /// [any-thread]
    ///
    /// `abi-v1` §17: a poisoned instance is unloadable-but-safe. A host must stop calling
    /// it and offer the user a reload, never abort the process.
    #[inline]
    #[must_use]
    pub const fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    /// Allocates DSP resources for `config`. [main-thread]
    ///
    /// # Errors
    ///
    /// [`RuntimeErrorKind::InvalidArgument`] for a configuration the model rejects,
    /// [`RuntimeErrorKind::InvalidState`] unless the instance is inactive, and whatever
    /// status the module returned.
    pub fn activate(&mut self, config: &ProcessConfig) -> RuntimeResult<()> {
        self.usable()?;
        self.expect(PluginState::Inactive, "activate")?;
        config
            .validate()
            .map_err(|e| RuntimeError::new(RuntimeErrorKind::InvalidArgument, e.to_string()))?;

        let mut raw = DauxProcessConfigV1::new();
        raw.sample_format = config.sample_format.as_bits();
        raw.process_mode = config.process_mode.code();
        raw.min_block_size = config.min_block_size;
        raw.max_block_size = config.max_block_size;
        raw.sample_rate = config.sample_rate;

        // SAFETY: `api` is the validated instance table and `plugin.handle` its handle;
        // `factory` keeps the module loaded. `raw` is a host-owned value alive for the call.
        let status = unsafe { (self.api.activate)(self.plugin.handle, &raw const raw) };
        self.record("activate", status)?;
        self.config = Some(*config);
        self.state = PluginState::Active;
        Ok(())
    }

    /// Releases DSP resources. [main-thread]
    ///
    /// # Errors
    ///
    /// [`RuntimeErrorKind::InvalidState`] unless the instance is active — an instance that
    /// is still processing must be stopped first.
    pub fn deactivate(&mut self) -> RuntimeResult<()> {
        self.usable()?;
        self.expect(PluginState::Active, "deactivate")?;
        // SAFETY: as in `activate`.
        unsafe { (self.api.deactivate)(self.plugin.handle) }
        self.config = None;
        self.state = PluginState::Inactive;
        Ok(())
    }

    /// Announces the first `process` of a run. [audio-thread]
    ///
    /// # Errors
    ///
    /// [`RuntimeErrorKind::InvalidState`] unless the instance is active, and whatever
    /// status the module returned.
    pub fn start_processing(&mut self) -> RuntimeResult<()> {
        self.usable()?;
        self.expect(PluginState::Active, "start_processing")?;
        // SAFETY: as in `activate`.
        let status = unsafe { (self.api.start_processing)(self.plugin.handle) };
        self.record("start_processing", status)?;
        self.state = PluginState::Processing;
        Ok(())
    }

    /// Announces the last `process` of a run. [audio-thread]
    ///
    /// # Errors
    ///
    /// [`RuntimeErrorKind::InvalidState`] unless the instance is processing.
    pub fn stop_processing(&mut self) -> RuntimeResult<()> {
        self.usable()?;
        self.expect(PluginState::Processing, "stop_processing")?;
        // SAFETY: as in `activate`.
        unsafe { (self.api.stop_processing)(self.plugin.handle) }
        self.state = PluginState::Active;
        Ok(())
    }

    /// Clears delay lines, filters and voices. [audio-thread, only while not processing]
    ///
    /// # Errors
    ///
    /// [`RuntimeErrorKind::InvalidState`] while the instance is processing, which is the
    /// one state `abi-v1` §7 forbids this in.
    pub fn reset(&mut self) -> RuntimeResult<()> {
        self.usable()?;
        if self.state == PluginState::Processing {
            return Err(RuntimeError::new(
                RuntimeErrorKind::InvalidState,
                "`reset` is only legal while the instance is not processing (abi-v1 §7)",
            ));
        }
        // SAFETY: as in `activate`.
        unsafe { (self.api.reset)(self.plugin.handle) }
        Ok(())
    }

    /// Runs one block. [audio-thread]
    ///
    /// Returns [`ProcessStatus::Error`] — never a `Result` — because a real-time failure
    /// path must not build a message. The block is refused without calling the plug-in when
    /// the instance is not processing, when it is poisoned, or when the block would hand
    /// the plug-in a pointer it will read past.
    ///
    /// Output events are cleared before the call and sorted by time after it, because
    /// `abi-v1` §9 lets a plug-in push them in any order and makes sorting the host's job.
    /// Nothing here allocates.
    pub fn process(&mut self, block: &mut HostBlock<'_>) -> ProcessStatus {
        if self.poisoned || self.state != PluginState::Processing {
            return ProcessStatus::Error;
        }
        if let Some(config) = &self.config
            && block.frames() > config.max_block_size
        {
            return ProcessStatus::Error;
        }
        if block.check().is_err() {
            return ProcessStatus::Error;
        }

        block.output_events_mut().clear();
        let handle = self.plugin.handle;
        let process = self.api.process;
        // SAFETY: `process` is the validated entry of the instance table and `handle` its
        // handle; `factory` keeps the module loaded. `with_raw` builds the `DauxProcessV1`
        // on its own stack frame, so every pointer inside it — audio, events, transport —
        // is valid for exactly the duration of the call, which is the lifetime `abi-v1`
        // §16.3 gives them. `block.check()` proved every channel is bound and long enough.
        let code = block.with_raw(|raw| unsafe { process(handle, raw) });
        block.output_events_mut().sort_by_time();
        ProcessStatus::from_code(code)
    }

    /// Drains work the plug-in queued with `request_callback`. [main-thread]
    ///
    /// # Errors
    ///
    /// [`RuntimeErrorKind::Poisoned`] once the instance has reported a panic.
    pub fn on_main_thread(&mut self) -> RuntimeResult<()> {
        self.usable()?;
        // SAFETY: as in `activate`.
        unsafe { (self.api.on_main_thread)(self.plugin.handle) }
        Ok(())
    }

    /// Latency in samples, or `0` when the plug-in publishes no `daux.latency/1`.
    /// [main-thread]
    ///
    /// A plug-in that does not implement the extension has no latency to declare, so `0` is
    /// the answer rather than an error.
    #[must_use]
    pub fn latency(&self) -> u32 {
        self.table::<DauxLatencyApiV1>(ext::LATENCY, GET_ONLY_LATENCY)
            .map_or(0, |api| {
                // SAFETY: `api` is a validated copy of the table the instance published and
                // `plugin.handle` is its handle.
                unsafe { (api.get)(self.plugin.handle) }
            })
    }

    /// Tail length, or [`Tail::None`] when the plug-in publishes no `daux.tail/1`.
    /// [any-thread]
    #[must_use]
    pub fn tail(&self) -> Tail {
        self.table::<DauxTailApiV1>(ext::TAIL, GET_ONLY_TAIL)
            .map_or(Tail::None, |api| {
                // SAFETY: as in `latency`.
                let samples = unsafe { (api.get)(self.plugin.handle) };
                if samples == DAUX_TAIL_INFINITE {
                    Tail::Infinite
                } else {
                    Tail::from_samples(samples)
                }
            })
    }

    /// The plug-in's parameter model, when it publishes `daux.params/1`. [main-thread]
    #[must_use]
    pub fn params(&self) -> Option<ParamsExt<'_>> {
        self.table::<DauxParamsApiV1>(ext::PARAMS, PARAMS_REQUIRED)
            .map(|api| ParamsExt::new(self.plugin.handle, api))
    }

    /// The plug-in's save/load, when it publishes `daux.state/1`. [main-thread]
    #[must_use]
    pub fn state(&self) -> Option<StateExt<'_>> {
        self.table::<DauxStateApiV1>(ext::STATE, STATE_REQUIRED)
            .map(|api| StateExt::new(self.plugin.handle, api))
    }

    /// The plug-in's editor, when it publishes `daux.gui/1`. [main-thread]
    ///
    /// The editor's lifetime is independent of the processor's: opening and closing it must
    /// never touch DSP state, and it can outlive many activations or none at all.
    #[must_use]
    pub fn gui(&self) -> Option<GuiExt<'_>> {
        self.table::<DauxGuiApiV1>(ext::GUI, GUI_REQUIRED)
            .map(|api| GuiExt::new(self.plugin.handle, api))
    }

    /// The raw extension table for `id`, or null. [any-thread]
    ///
    /// For extensions this crate does not model. Anything reached through the returned
    /// pointer is entirely the caller's responsibility, including validating its `size`.
    #[must_use]
    pub fn extension(&self, id: &str) -> *const c_void {
        // SAFETY: `get_extension` is a validated entry of the instance table, `plugin.handle`
        // is its handle, and the `DauxStrView` borrows `id` for the duration of the call.
        // `abi-v1` §7 makes the entry legal from any thread once `init` has run, which it
        // has: `create_plugin` does not return an instance otherwise.
        unsafe { (self.api.get_extension)(self.plugin.handle, DauxStrView::from_str(id)) }
    }

    /// Looks an extension table up and validates it before anything is called through it.
    fn table<T: daux_abi::AbiStruct>(&self, id: &str, required: &[RequiredFn]) -> Option<T> {
        if self.poisoned {
            return None;
        }
        let raw = self.extension(id);
        if raw.is_null() {
            return None;
        }
        // SAFETY: the module returned this pointer for `id`, so `abi-v1` §11 says it
        // addresses a `#[repr(C)]` table the module owns and keeps valid while the instance
        // lives; `factory` keeps the module loaded. `read_table` reads nothing beyond the
        // `size` word until that word says the bytes are there, so a module that publishes
        // an undersized or half-null table is refused rather than called into.
        unsafe { read_table(raw.cast::<T>(), id, required) }.ok()
    }

    /// Refuses everything once the instance has reported a panic (`abi-v1` §17).
    fn usable(&self) -> RuntimeResult<()> {
        if self.poisoned {
            return Err(RuntimeError::new(
                RuntimeErrorKind::Poisoned,
                "the instance panicked across the ABI boundary and refuses further work \
                 (abi-v1 §17)",
            ));
        }
        Ok(())
    }

    /// Refuses a lifecycle transition the ABI does not allow.
    fn expect(&self, wanted: PluginState, what: &str) -> RuntimeResult<()> {
        if self.state == wanted {
            return Ok(());
        }
        Err(RuntimeError::new(
            RuntimeErrorKind::InvalidState,
            format!(
                "`{what}` needs the instance to be {wanted}, but it is {} (abi-v1 §7)",
                self.state
            ),
        ))
    }

    /// Converts a status, poisoning the instance when the module reports a caught panic.
    fn record(&mut self, what: &str, status: DauxStatus) -> RuntimeResult<()> {
        if status.0 == DAUX_OK.0 {
            return Ok(());
        }
        if status.0 == status::PANIC {
            self.poisoned = true;
        }
        Err(RuntimeError::from_status(what, status.0))
    }
}

impl Drop for LoadedPlugin {
    fn drop(&mut self) {
        // `abi-v1` §7: `destroy` must be preceded by `deactivate` if the instance was
        // activated, and `deactivate` cannot happen while processing. A host that drops an
        // instance mid-run is making a mistake, but taking the module down with it would
        // turn that mistake into a crash in someone else's plug-in.
        if self.state == PluginState::Processing {
            // SAFETY: the validated table's entry, on this instance's own handle, in the
            // one state `stop_processing` is legal in.
            unsafe { (self.api.stop_processing)(self.plugin.handle) }
            self.state = PluginState::Active;
        }
        if self.state == PluginState::Active {
            // SAFETY: as above, for `deactivate`.
            unsafe { (self.api.deactivate)(self.plugin.handle) }
            self.state = PluginState::Inactive;
        }
        // SAFETY: the instance is inactive, which is `destroy`'s precondition. `factory` is
        // dropped after this body runs, so the factory outlives the instance it created
        // (`abi-v1` §5) and the module outlives them both.
        unsafe { (self.api.destroy)(self.plugin.handle) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_states_display_the_way_the_specification_names_them() {
        assert_eq!(PluginState::Inactive.to_string(), "inactive");
        assert_eq!(PluginState::Active.to_string(), "active");
        assert_eq!(PluginState::Processing.to_string(), "processing");
        assert_eq!(PluginState::default(), PluginState::Inactive);
    }

    #[test]
    fn an_instance_can_be_handed_to_the_audio_thread_but_not_shared() {
        const fn assert_send<T: Send>() {}
        assert_send::<LoadedPlugin>();
        // `LoadedPlugin: Sync` must NOT hold: `abi-v1` §15 forbids concurrent calls for one
        // instance, and a shared reference would permit them. This is asserted by the
        // absence of `unsafe impl Sync`; a future change that adds one has to justify it
        // against §15.
    }
}
