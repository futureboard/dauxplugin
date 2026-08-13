//! Driving one plug-in through the lifecycle of `docs/specifications/abi-v1.md` §7.

use core::fmt;

use daux_audio::{AudioBuses, BusLayout};
use daux_core::{
    DauxError, DauxPlugin, DauxResult, ErrorKind, EventPortLayout, Latency, PluginDescriptor,
    ProcessConfig, ProcessContext, ProcessEvents, ProcessStatus, Tail,
};
use daux_graphics::DauxGraphic;
use daux_host_services::{HostServices, TaskId};
use daux_parameter::Params;
use daux_state::{StateReader, StateWriter};

use crate::editor::take_editor;

/// Builds a refusal without allocating, so an out-of-order call costs nothing even on the
/// audio thread. `[any-thread]`
const fn wrong_state(message: &'static str) -> DauxError {
    ErrorKind::InvalidState.with_static(message)
}

/// Where a [`PluginInstance`] sits in the abi-v1 §7 state machine.
///
/// ```text
/// Created ──init──► Inactive ──activate──► Active ──start_processing──► Processing
///                      ▲                     │                              │
///                      └──── deactivate ─────┘◄──── stop_processing ────────┘
/// ```
///
/// [`Poisoned`](InstanceState::Poisoned) is not a transition a host can request: it is where
/// an adapter puts an instance whose `catch_unwind` caught a panic (abi-v1 §17.3). Nothing
/// leaves it.
///
/// `[any-thread]`
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum InstanceState {
    /// Constructed but not yet initialised. Only [`PluginInstance::init`] and
    /// [`PluginInstance::set_host`] are legal.
    #[default]
    Created,
    /// Initialised, with no DSP resources allocated. This is where state is loaded and where
    /// a new [`ProcessConfig`] is accepted.
    Inactive,
    /// Prepared for a specific [`ProcessConfig`]. Buffers exist; the audio thread is not
    /// running yet.
    Active,
    /// The audio thread is running. [`PluginInstance::process`] is legal only here.
    Processing,
    /// A panic crossed a boundary. Every call is refused with
    /// [`ErrorKind::InvalidState`]; the host may only drop the instance (abi-v1 §17).
    Poisoned,
}

impl InstanceState {
    /// Every state, in lifecycle order. `[any-thread]`
    pub const ALL: [InstanceState; 5] = [
        InstanceState::Created,
        InstanceState::Inactive,
        InstanceState::Active,
        InstanceState::Processing,
        InstanceState::Poisoned,
    ];

    /// Short, stable identifier for logs and errors. `[any-thread]`
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            InstanceState::Created => "created",
            InstanceState::Inactive => "inactive",
            InstanceState::Active => "active",
            InstanceState::Processing => "processing",
            InstanceState::Poisoned => "poisoned",
        }
    }

    /// `true` once [`PluginInstance::init`] has succeeded and the instance is still usable.
    /// `[any-thread]`
    #[must_use]
    pub const fn is_initialised(self) -> bool {
        matches!(
            self,
            InstanceState::Inactive | InstanceState::Active | InstanceState::Processing
        )
    }

    /// `true` only in [`InstanceState::Processing`], the one state where `process` is legal.
    /// `[audio-thread]`
    #[must_use]
    pub const fn is_processing(self) -> bool {
        matches!(self, InstanceState::Processing)
    }

    /// `true` when a panic has been caught for this instance. `[any-thread]`
    #[must_use]
    pub const fn is_poisoned(self) -> bool {
        matches!(self, InstanceState::Poisoned)
    }
}

impl fmt::Display for InstanceState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One plug-in, owned and driven through its lifecycle.
///
/// A format adapter holds a `PluginInstance` and never touches the [`DauxPlugin`] directly.
/// That is what lets `.axt`, VST3 and CLAP share one implementation of the rules in abi-v1
/// §7: **any transition the state machine does not allow is refused with
/// [`ErrorKind::InvalidState`], never obeyed and never left to the plug-in to notice.**
/// Hosts get this wrong regularly — calling `process` after `stop_processing`, activating
/// twice, re-preparing while the audio thread is running — and a plug-in that trusts the host
/// crashes a DAW.
///
/// # Mapping onto [`DauxProcessor`](daux_core::DauxProcessor)
///
/// The ABI's verbs and the processor's are not the same words for the same things, which is
/// the single most common source of confusion in an adapter:
///
/// | `PluginInstance` (abi-v1 §7) | Calls on the processor | Thread |
/// |---|---|---|
/// | [`init`](Self::init) | — | `[main-thread]` |
/// | [`activate`](Self::activate) | `prepare(config)` — allocates here | `[main-thread]` |
/// | [`deactivate`](Self::deactivate) | — resources are released by the next `prepare` or by `Drop` | `[main-thread]` |
/// | [`start_processing`](Self::start_processing) | `activate()` | `[audio-thread]` |
/// | [`stop_processing`](Self::stop_processing) | `deactivate()` | `[audio-thread]` |
/// | [`reset`](Self::reset) | `reset()` | `[audio-thread]` |
/// | [`process`](Self::process) | `process(..)` | `[audio-thread]` |
///
/// # Real-time behaviour
///
/// [`process`](Self::process), [`process_f64`](Self::process_f64),
/// [`start_processing`](Self::start_processing), [`stop_processing`](Self::stop_processing)
/// and [`reset`](Self::reset) allocate nothing, *including on their refusal paths* — every
/// message they can produce is a `&'static str`. A refused `process` also silences the output
/// buffers, so a host that calls out of order gets silence rather than whatever was left in
/// the buffer.
///
/// # Editors
///
/// [`create_editor`](Self::create_editor) is legal in every initialised state, including
/// [`Processing`](InstanceState::Processing), and dropping the returned editor never touches
/// the processor. That is the ninth architectural rule, and it is enforced here by *not*
/// gating editors on the DSP state.
pub struct PluginInstance {
    plugin: Box<dyn DauxPlugin>,
    descriptor: Option<PluginDescriptor>,
    state: InstanceState,
    config: Option<ProcessConfig>,
}

impl fmt::Debug for PluginInstance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PluginInstance")
            .field("state", &self.state)
            .field("id", &self.descriptor.as_ref().map(|d| d.id.as_str()))
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl PluginInstance {
    /// [main-thread] Takes ownership of a plug-in a factory produced.
    ///
    /// The instance starts in [`InstanceState::Created`]: nothing but
    /// [`init`](Self::init) and [`set_host`](Self::set_host) is legal until `init` succeeds.
    ///
    /// The descriptor is unknown to a `Box<dyn DauxPlugin>` — `descriptor()` is an associated
    /// function and cannot be called on a trait object — so [`descriptor`](Self::descriptor)
    /// returns `None`. Use [`with_descriptor`](Self::with_descriptor) or
    /// [`for_plugin`](Self::for_plugin) when the caller knows it.
    #[must_use]
    pub fn new(plugin: Box<dyn DauxPlugin>) -> Self {
        Self {
            plugin,
            descriptor: None,
            state: InstanceState::Created,
            config: None,
        }
    }

    /// [main-thread] As [`new`](Self::new), remembering the descriptor the factory used.
    #[must_use]
    pub fn with_descriptor(plugin: Box<dyn DauxPlugin>, descriptor: PluginDescriptor) -> Self {
        Self {
            plugin,
            descriptor: Some(descriptor),
            state: InstanceState::Created,
            config: None,
        }
    }

    /// [main-thread] Builds an instance from a concrete plug-in, capturing its descriptor.
    ///
    /// The convenient form for tests and for the in-process host: `P` is known, so
    /// `P::descriptor()` is callable and nothing is lost.
    #[must_use]
    pub fn for_plugin<P: DauxPlugin>(plugin: P) -> Self {
        Self::with_descriptor(Box::new(plugin), P::descriptor())
    }

    /// [any-thread] Where this instance is in the lifecycle.
    #[inline]
    #[must_use]
    pub const fn state(&self) -> InstanceState {
        self.state
    }

    /// [any-thread] `true` when a panic has poisoned this instance (abi-v1 §17).
    #[inline]
    #[must_use]
    pub const fn is_poisoned(&self) -> bool {
        self.state.is_poisoned()
    }

    /// [main-thread] The descriptor, when the constructor was given one.
    #[must_use]
    pub const fn descriptor(&self) -> Option<&PluginDescriptor> {
        self.descriptor.as_ref()
    }

    /// [audio-thread] The configuration the current activation was prepared with, or `None`
    /// while inactive.
    #[must_use]
    pub const fn config(&self) -> Option<&ProcessConfig> {
        self.config.as_ref()
    }

    /// [main-thread] The plug-in itself, for the rare adapter that needs something this
    /// wrapper does not expose.
    ///
    /// Reaching past the state machine is how the guarantees above are lost; prefer a method
    /// on `PluginInstance`, and add one here if it is missing.
    #[must_use]
    pub fn plugin(&self) -> &dyn DauxPlugin {
        self.plugin.as_ref()
    }

    /// [main-thread] Mutable access to the plug-in. See [`plugin`](Self::plugin).
    #[must_use]
    pub fn plugin_mut(&mut self) -> &mut dyn DauxPlugin {
        self.plugin.as_mut()
    }

    /// [any-thread] Marks this instance unusable after a panic crossed a boundary.
    ///
    /// Every later call is refused with [`ErrorKind::InvalidState`] and every later `process`
    /// returns [`ProcessStatus::Error`]. Poisoning is one-way and idempotent, and allocates
    /// nothing: an adapter calls it from inside a `catch_unwind` handler where the stack has
    /// already gone wrong once (abi-v1 §17.3).
    pub const fn poison(&mut self) {
        self.state = InstanceState::Poisoned;
    }

    // ---- lifecycle ---------------------------------------------------------------------

    /// [main-thread] Completes construction. `Created` → `Inactive`.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::InvalidState`] if the instance is already initialised or poisoned.
    pub fn init(&mut self) -> DauxResult<()> {
        match self.state {
            InstanceState::Created => {
                self.state = InstanceState::Inactive;
                Ok(())
            }
            InstanceState::Poisoned => Err(POISONED),
            _ => Err(wrong_state(
                "init: the instance has already been initialised (abi-v1 §7)",
            )),
        }
    }

    /// [main-thread] Allocates DSP resources for `config`. `Inactive` → `Active`.
    ///
    /// Calls [`DauxProcessor::prepare`](daux_core::DauxProcessor::prepare), which is the one
    /// place a processor may allocate. The config is validated first, so a plug-in never sees
    /// a NaN sample rate or a zero block size even from a host that would send one.
    ///
    /// A failing `prepare` leaves the instance `Inactive`, not half-activated: the host may
    /// retry with a different configuration.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::InvalidState`] unless the instance is `Inactive`,
    /// [`ErrorKind::InvalidArgument`] for a configuration no plug-in can be sized from, or
    /// whatever `prepare` returned.
    pub fn activate(&mut self, config: &ProcessConfig) -> DauxResult<()> {
        match self.state {
            InstanceState::Inactive => {}
            InstanceState::Active => {
                return Err(wrong_state(
                    "activate: the instance is already active; deactivate first (abi-v1 §7)",
                ));
            }
            InstanceState::Processing => {
                return Err(wrong_state(
                    "activate: cannot re-prepare while processing; stop_processing and \
                     deactivate first (abi-v1 §7)",
                ));
            }
            InstanceState::Created => {
                return Err(wrong_state(
                    "activate: init has not been called (abi-v1 §7)",
                ));
            }
            InstanceState::Poisoned => return Err(POISONED),
        }
        config.validate()?;
        self.plugin.processor().prepare(config)?;
        self.config = Some(*config);
        self.state = InstanceState::Active;
        Ok(())
    }

    /// [main-thread] Releases the activation. `Active` → `Inactive`.
    ///
    /// The processor keeps whatever `prepare` allocated: the next
    /// [`activate`](Self::activate) re-sizes it, and `Drop` releases it. There is no
    /// "unprepare" on [`DauxProcessor`](daux_core::DauxProcessor) by design — a plug-in that
    /// freed on every deactivate would allocate again on every transport start.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::InvalidState`] unless the instance is `Active`. In particular
    /// deactivating while processing is refused: abi-v1 §7 requires `stop_processing` first,
    /// and obeying it here would tear down buffers the audio thread is still inside.
    pub fn deactivate(&mut self) -> DauxResult<()> {
        match self.state {
            InstanceState::Active => {
                self.config = None;
                self.state = InstanceState::Inactive;
                Ok(())
            }
            InstanceState::Processing => Err(wrong_state(
                "deactivate: still processing; call stop_processing first (abi-v1 §7)",
            )),
            InstanceState::Poisoned => Err(POISONED),
            InstanceState::Created | InstanceState::Inactive => Err(wrong_state(
                "deactivate: the instance is not active (abi-v1 §7)",
            )),
        }
    }

    /// [audio-thread] Arms the processor for a run. `Active` → `Processing`.
    ///
    /// Calls [`DauxProcessor::activate`](daux_core::DauxProcessor::activate). A failure
    /// leaves the instance `Active`, so the host may retry or deactivate cleanly.
    ///
    /// Allocates nothing, including when it refuses.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::InvalidState`] unless the instance is `Active`, or whatever the
    /// processor's `activate` returned.
    pub fn start_processing(&mut self) -> DauxResult<()> {
        match self.state {
            InstanceState::Active => {}
            InstanceState::Processing => {
                return Err(wrong_state(
                    "start_processing: already processing (abi-v1 §7)",
                ));
            }
            InstanceState::Poisoned => return Err(POISONED),
            InstanceState::Created | InstanceState::Inactive => {
                return Err(wrong_state(
                    "start_processing: the instance is not active; activate first (abi-v1 §7)",
                ));
            }
        }
        self.plugin.processor().activate()?;
        self.state = InstanceState::Processing;
        Ok(())
    }

    /// [audio-thread] Ends a run. `Processing` → `Active`.
    ///
    /// Calls [`DauxProcessor::deactivate`](daux_core::DauxProcessor::deactivate), which
    /// cannot fail and must not deallocate. Allocates nothing, including when it refuses.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::InvalidState`] unless the instance is `Processing`.
    pub fn stop_processing(&mut self) -> DauxResult<()> {
        match self.state {
            InstanceState::Processing => {
                self.plugin.processor().deactivate();
                self.state = InstanceState::Active;
                Ok(())
            }
            InstanceState::Poisoned => Err(POISONED),
            _ => Err(wrong_state(
                "stop_processing: the instance is not processing (abi-v1 §7)",
            )),
        }
    }

    /// [audio-thread] Clears everything that depends on past audio.
    ///
    /// abi-v1 §7 marks `reset` `[audio-thread, only while not processing]`, so it is legal
    /// only in [`Active`](InstanceState::Active). A host that wants to clear a delay line
    /// mid-run calls `stop_processing`, `reset`, `start_processing` — which is also the only
    /// order in which a processor can assume nothing is reading its buffers.
    ///
    /// Allocates nothing, including when it refuses.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::InvalidState`] unless the instance is `Active`.
    pub fn reset(&mut self) -> DauxResult<()> {
        match self.state {
            InstanceState::Active => {
                self.plugin.processor().reset();
                Ok(())
            }
            InstanceState::Processing => Err(wrong_state(
                "reset: not while processing; stop_processing first (abi-v1 §7)",
            )),
            InstanceState::Poisoned => Err(POISONED),
            InstanceState::Created | InstanceState::Inactive => {
                Err(wrong_state("reset: the instance is not active (abi-v1 §7)"))
            }
        }
    }

    /// [audio-thread] Processes one block of `f32` audio.
    ///
    /// Returns [`ProcessStatus::Error`] — never a `Result`, because building a message would
    /// allocate — when the call is out of order, when the instance is poisoned, or when the
    /// host asked for more frames than [`ProcessConfig::max_block_size`] promised. In every
    /// one of those cases the output buffers are silenced first, so a misbehaving host gets
    /// silence rather than stale samples.
    ///
    /// The block-size check is not pedantry: a processor sized its buffers from
    /// `max_block_size` in `prepare`, and an over-long block is the exact input that makes a
    /// plug-in either overrun a scratch buffer or allocate on the audio thread.
    #[must_use]
    pub fn process<'a>(
        &mut self,
        ctx: &ProcessContext<'a>,
        audio: &mut AudioBuses<'a, f32>,
        events: &mut ProcessEvents<'a>,
    ) -> ProcessStatus {
        if !self.may_process(ctx.frames()) {
            audio.silence_outputs();
            return ProcessStatus::Error;
        }
        self.plugin.processor().process(ctx, audio, events)
    }

    /// [audio-thread] Processes one block of `f64` audio.
    ///
    /// Gated exactly as [`process`](Self::process). A plug-in that did not opt into 64-bit
    /// processing returns [`ProcessStatus::Error`] from the default trait method, which is
    /// indistinguishable from a refusal here — deliberately, since both mean "do not trust
    /// these outputs".
    #[must_use]
    pub fn process_f64<'a>(
        &mut self,
        ctx: &ProcessContext<'a>,
        audio: &mut AudioBuses<'a, f64>,
        events: &mut ProcessEvents<'a>,
    ) -> ProcessStatus {
        if !self.may_process(ctx.frames()) {
            audio.silence_outputs();
            return ProcessStatus::Error;
        }
        self.plugin.processor().process_f64(ctx, audio, events)
    }

    /// [audio-thread] Whether a block of `frames` may be handed to the processor right now.
    #[inline]
    fn may_process(&self, frames: usize) -> bool {
        match (self.state, self.config.as_ref()) {
            (InstanceState::Processing, Some(config)) => frames <= config.max_block_size as usize,
            _ => false,
        }
    }

    // ---- reporting ---------------------------------------------------------------------

    /// [audio-thread] The processor's reported latency.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::InvalidState`] before `init` or after poisoning. Allocates nothing.
    pub fn latency(&mut self) -> DauxResult<Latency> {
        self.require_initialised()?;
        Ok(self.plugin.processor().latency())
    }

    /// [audio-thread] The processor's reported tail.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::InvalidState`] before `init` or after poisoning. Allocates nothing.
    pub fn tail(&mut self) -> DauxResult<Tail> {
        self.require_initialised()?;
        Ok(self.plugin.processor().tail())
    }

    /// [main-thread] The plug-in's current audio bus topology.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::InvalidState`] before `init` or after poisoning.
    pub fn bus_layout(&self) -> DauxResult<BusLayout> {
        self.require_initialised()?;
        Ok(self.plugin.bus_layout())
    }

    /// [main-thread] The plug-in's current event port topology.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::InvalidState`] before `init` or after poisoning.
    pub fn event_ports(&self) -> DauxResult<EventPortLayout> {
        self.require_initialised()?;
        Ok(self.plugin.event_ports())
    }

    /// [main-thread] Whether the plug-in would accept `layout`.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::InvalidState`] before `init` or after poisoning.
    pub fn accepts_bus_layout(&self, layout: &BusLayout) -> DauxResult<bool> {
        self.require_initialised()?;
        Ok(self.plugin.accepts_bus_layout(layout))
    }

    // ---- controller --------------------------------------------------------------------

    /// [main-thread] The plug-in's parameter set.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::InvalidState`] before `init` or after poisoning.
    pub fn params(&mut self) -> DauxResult<&dyn Params> {
        self.require_initialised()?;
        Ok(self.plugin.controller().params())
    }

    /// [main-thread] Hands the controller its host services.
    ///
    /// Legal only before the plug-in is activated: abi-v1 §7 has the host provide services
    /// during construction, and a plug-in that cached a service pointer would be reading a
    /// stale one if the host swapped it mid-run.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::InvalidState`] once the instance is `Active` or `Processing`, or after
    /// poisoning.
    pub fn set_host(&mut self, host: HostServices) -> DauxResult<()> {
        match self.state {
            InstanceState::Created | InstanceState::Inactive => {
                self.plugin.controller().set_host(host);
                Ok(())
            }
            InstanceState::Poisoned => Err(POISONED),
            InstanceState::Active | InstanceState::Processing => Err(wrong_state(
                "set_host: host services must be supplied before activation (abi-v1 §7)",
            )),
        }
    }

    /// [main-thread] Writes the plug-in's state.
    ///
    /// Legal while processing: saving a project does not stop the transport.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::InvalidState`] before `init` or after poisoning, or whatever the
    /// controller returned.
    pub fn save_state(&mut self, w: &mut StateWriter) -> DauxResult<()> {
        self.require_initialised()?;
        self.plugin.controller().save_state(w)
    }

    /// [main-thread] Restores the plug-in's state.
    ///
    /// Legal while processing: hosts load presets during playback, and refusing would make
    /// this wrapper unusable in a real DAW.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::InvalidState`] before `init` or after poisoning, or whatever the
    /// controller returned.
    pub fn load_state(&mut self, r: &StateReader) -> DauxResult<()> {
        self.require_initialised()?;
        self.plugin.controller().load_state(r)
    }

    /// [main-thread] Drains work the audio thread asked for with `request_callback`.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::InvalidState`] before `init` or after poisoning.
    pub fn on_main_thread(&mut self) -> DauxResult<()> {
        self.require_initialised()?;
        self.plugin.controller().on_main_thread();
        Ok(())
    }

    /// [any-thread] Runs a task the host scheduled on its worker pool.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::InvalidState`] before `init` or after poisoning.
    pub fn on_worker(&mut self, task: TaskId) -> DauxResult<()> {
        self.require_initialised()?;
        self.plugin.controller().on_worker(task);
        Ok(())
    }

    // ---- editor ------------------------------------------------------------------------

    /// [main-thread] Creates the plug-in's editor, re-typed into a real editor handle.
    ///
    /// `Ok(None)` means the plug-in is headless — a normal answer, not a failure. Legal in
    /// every initialised state including `Processing`, and repeatable: a user may open and
    /// close an editor a hundred times while audio never stops.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::InvalidState`] before `init` or after poisoning, or
    /// [`ErrorKind::Plugin`] when the plug-in returned something that is not a
    /// `Box<dyn DauxGraphic>` — see [`crate::editor`].
    pub fn create_editor(&mut self) -> DauxResult<Option<Box<dyn DauxGraphic>>> {
        self.require_initialised()?;
        take_editor(self.plugin.as_mut())
    }

    // ---- guards ------------------------------------------------------------------------

    /// Refuses a call that needs a live, initialised instance. Allocates nothing.
    #[inline]
    fn require_initialised(&self) -> DauxResult<()> {
        match self.state {
            s if s.is_initialised() => Ok(()),
            InstanceState::Poisoned => Err(POISONED),
            _ => Err(wrong_state(
                "the instance has not been initialised; call init first (abi-v1 §7)",
            )),
        }
    }
}

/// The refusal a poisoned instance gives to everything (abi-v1 §17.3). A constant, so no
/// refusal path ever allocates.
const POISONED: DauxError =
    wrong_state("the instance is poisoned after a panic and refuses further work (abi-v1 §17)");

impl Drop for PluginInstance {
    /// [main-thread] Leaves the plug-in in a state where its own `Drop` is safe.
    ///
    /// abi-v1 §7 requires a host to stop and deactivate before destroying, and hosts forget.
    /// Rather than let a processor be dropped while it believes the audio thread is inside
    /// it, the missing `stop_processing` is performed here.
    fn drop(&mut self) {
        if self.state == InstanceState::Processing {
            self.plugin.processor().deactivate();
            self.state = InstanceState::Active;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{Spy, assert_no_alloc, block, config, expect_err, spy};
    use daux_audio::SampleFormat;
    use daux_core::ProcessMode;
    use daux_state::{StateVersion, StateWriter};

    /// Every method that can refuse, applied to one instance, so a new state cannot be added
    /// without deciding what each of them does in it.
    fn refuse_everything(instance: &mut PluginInstance, expect: ErrorKind) {
        assert_eq!(instance.init().unwrap_err().kind(), expect, "init");
        assert_eq!(
            instance
                .activate(&config(48_000.0, 128))
                .unwrap_err()
                .kind(),
            expect,
            "activate"
        );
        assert_eq!(
            instance.deactivate().unwrap_err().kind(),
            expect,
            "deactivate"
        );
        assert_eq!(
            instance.start_processing().unwrap_err().kind(),
            expect,
            "start_processing"
        );
        assert_eq!(
            instance.stop_processing().unwrap_err().kind(),
            expect,
            "stop_processing"
        );
        assert_eq!(instance.reset().unwrap_err().kind(), expect, "reset");
        assert_eq!(instance.latency().unwrap_err().kind(), expect, "latency");
        assert_eq!(instance.tail().unwrap_err().kind(), expect, "tail");
        assert_eq!(
            instance.bus_layout().unwrap_err().kind(),
            expect,
            "bus_layout"
        );
        assert_eq!(
            instance.event_ports().unwrap_err().kind(),
            expect,
            "event_ports"
        );
        assert_eq!(expect_err(instance.params()).kind(), expect, "params");
        assert_eq!(
            instance.on_main_thread().unwrap_err().kind(),
            expect,
            "on_main_thread"
        );
        assert_eq!(
            instance.on_worker(TaskId::new(1)).unwrap_err().kind(),
            expect,
            "on_worker"
        );
        assert_eq!(
            expect_err(instance.create_editor()).kind(),
            expect,
            "create_editor"
        );
    }

    #[test]
    fn a_new_instance_is_created_and_knows_nothing_yet() {
        let (instance, counts) = spy();
        assert_eq!(instance.state(), InstanceState::Created);
        assert!(!instance.is_poisoned());
        assert!(instance.config().is_none());
        assert_eq!(
            instance.descriptor().map(|d| d.id.as_str()),
            Some("com.example.spy")
        );
        assert_eq!(counts.prepares(), 0);
    }

    #[test]
    fn a_boxed_plugin_has_no_descriptor_but_still_works() {
        let counts = std::sync::Arc::new(crate::testkit::Counts::default());
        let plugin: Box<dyn DauxPlugin> = Box::new(Spy::new(counts.clone()));
        let mut instance = PluginInstance::new(plugin);
        assert!(instance.descriptor().is_none());
        instance.init().unwrap();
        assert!(instance.bus_layout().is_ok());
    }

    #[test]
    fn nothing_but_init_and_set_host_is_legal_before_init() {
        let (mut instance, counts) = spy();

        assert_eq!(
            instance.activate(&config(48_000.0, 64)).unwrap_err().kind(),
            ErrorKind::InvalidState
        );
        assert_eq!(
            instance.start_processing().unwrap_err().kind(),
            ErrorKind::InvalidState
        );
        assert_eq!(
            instance.reset().unwrap_err().kind(),
            ErrorKind::InvalidState
        );
        assert_eq!(
            expect_err(instance.params()).kind(),
            ErrorKind::InvalidState
        );
        assert_eq!(
            expect_err(instance.create_editor()).kind(),
            ErrorKind::InvalidState
        );
        assert_eq!(counts.prepares(), 0, "prepare must not have been reached");
        assert_eq!(instance.state(), InstanceState::Created);

        // Host services, on the other hand, arrive before init.
        instance.set_host(HostServices::null()).unwrap();
        assert_eq!(counts.hosts(), 1);
        instance.init().unwrap();
        assert_eq!(instance.state(), InstanceState::Inactive);
    }

    #[test]
    fn init_is_not_repeatable() {
        let (mut instance, _) = spy();
        instance.init().unwrap();
        let err = instance.init().unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidState);
        assert!(err.message().contains("already been initialised"));
        assert_eq!(instance.state(), InstanceState::Inactive);
    }

    #[test]
    fn activate_twice_is_rejected_rather_than_re_preparing() {
        let (mut instance, counts) = spy();
        instance.init().unwrap();
        instance.activate(&config(48_000.0, 256)).unwrap();
        assert_eq!(counts.prepares(), 1);

        let err = instance.activate(&config(96_000.0, 512)).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidState);
        assert!(err.message().contains("already active"));
        assert_eq!(counts.prepares(), 1, "the second prepare must not happen");
        // The first configuration is untouched.
        assert_eq!(instance.config().unwrap().sample_rate, 48_000.0);
    }

    #[test]
    fn activate_refuses_a_configuration_no_plug_in_could_be_sized_from() {
        let (mut instance, counts) = spy();
        instance.init().unwrap();

        for bad in [
            config(0.0, 256),
            config(f64::NAN, 256),
            config(-48_000.0, 256),
            config(48_000.0, 0),
        ] {
            let err = instance.activate(&bad).unwrap_err();
            assert_eq!(err.kind(), ErrorKind::InvalidArgument, "{bad:?}");
            assert_eq!(instance.state(), InstanceState::Inactive);
        }
        assert_eq!(
            counts.prepares(),
            0,
            "a bad config never reaches the plug-in"
        );
    }

    #[test]
    fn a_failing_prepare_leaves_the_instance_inactive_and_retryable() {
        let (mut instance, counts) = spy();
        instance.init().unwrap();
        counts.fail_prepare(true);

        let err = instance.activate(&config(48_000.0, 256)).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::OutOfMemory);
        assert_eq!(instance.state(), InstanceState::Inactive);
        assert!(instance.config().is_none());
        // …and the audio thread cannot be started on the back of it.
        assert_eq!(
            instance.start_processing().unwrap_err().kind(),
            ErrorKind::InvalidState
        );

        counts.fail_prepare(false);
        instance.activate(&config(48_000.0, 256)).unwrap();
        assert_eq!(instance.state(), InstanceState::Active);
        assert_eq!(counts.prepares(), 2);
    }

    #[test]
    fn a_failing_activate_leaves_the_instance_active_not_processing() {
        let (mut instance, counts) = spy();
        instance.init().unwrap();
        instance.activate(&config(48_000.0, 64)).unwrap();
        counts.fail_activate(true);

        let err = instance.start_processing().unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Unsupported);
        assert_eq!(instance.state(), InstanceState::Active);

        // The refusal is real: process must still be closed.
        let mut b = block(64);
        assert_eq!(b.run(&mut instance), ProcessStatus::Error);

        counts.fail_activate(false);
        instance.start_processing().unwrap();
        assert_eq!(instance.state(), InstanceState::Processing);
    }

    #[test]
    fn process_before_start_processing_is_refused_and_silences_the_output() {
        let (mut instance, counts) = spy();
        instance.init().unwrap();
        instance.activate(&config(48_000.0, 64)).unwrap();

        let mut b = block(64);
        b.fill_output(0.75);
        assert_eq!(b.run(&mut instance), ProcessStatus::Error);
        assert_eq!(
            counts.processes(),
            0,
            "the plug-in must not have been called"
        );
        assert!(
            b.output_is_silent(),
            "a refused block must not leave stale samples in the output"
        );
    }

    #[test]
    fn process_stops_being_legal_the_moment_processing_stops() {
        let (mut instance, counts) = spy();
        instance.init().unwrap();
        instance.activate(&config(48_000.0, 64)).unwrap();
        instance.start_processing().unwrap();

        let mut b = block(64);
        assert_eq!(b.run(&mut instance), ProcessStatus::Continue);
        assert_eq!(counts.processes(), 1);

        instance.stop_processing().unwrap();
        assert_eq!(b.run(&mut instance), ProcessStatus::Error);
        assert_eq!(counts.processes(), 1);
        assert_eq!(counts.deactivates(), 1);
    }

    #[test]
    fn process_refuses_a_block_longer_than_the_prepared_maximum() {
        let (mut instance, counts) = spy();
        instance.init().unwrap();
        instance.activate(&config(48_000.0, 64)).unwrap();
        instance.start_processing().unwrap();

        // Exactly the maximum is fine…
        let mut ok = block(64);
        assert_eq!(ok.run(&mut instance), ProcessStatus::Continue);

        // …one frame more is a host bug, and the plug-in sized its buffers for 64.
        let mut over = block(65);
        over.fill_output(1.0);
        assert_eq!(over.run(&mut instance), ProcessStatus::Error);
        assert!(over.output_is_silent());
        assert_eq!(
            counts.processes(),
            1,
            "the over-long block never got through"
        );
    }

    #[test]
    fn f64_processing_is_gated_by_the_same_state_machine() {
        let (mut instance, counts) = spy();
        instance.init().unwrap();
        let cfg = config(48_000.0, 32).with_sample_format(SampleFormat::F64);
        instance.activate(&cfg).unwrap();

        let mut b = block(32);
        assert_eq!(b.run_f64(&mut instance), ProcessStatus::Error);
        assert_eq!(counts.f64_processes(), 0);

        instance.start_processing().unwrap();
        assert_eq!(b.run_f64(&mut instance), ProcessStatus::Continue);
        assert_eq!(counts.f64_processes(), 1);

        let mut over = block(33);
        assert_eq!(over.run_f64(&mut instance), ProcessStatus::Error);
        assert_eq!(counts.f64_processes(), 1);
    }

    #[test]
    fn start_processing_twice_is_rejected() {
        let (mut instance, counts) = spy();
        instance.init().unwrap();
        instance.activate(&config(48_000.0, 64)).unwrap();
        instance.start_processing().unwrap();

        let err = instance.start_processing().unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidState);
        assert!(err.message().contains("already processing"));
        assert_eq!(counts.activates(), 1);
        assert_eq!(instance.state(), InstanceState::Processing);
    }

    #[test]
    fn stop_processing_needs_a_run_to_stop() {
        let (mut instance, counts) = spy();
        instance.init().unwrap();
        assert_eq!(
            instance.stop_processing().unwrap_err().kind(),
            ErrorKind::InvalidState
        );
        instance.activate(&config(48_000.0, 64)).unwrap();
        assert_eq!(
            instance.stop_processing().unwrap_err().kind(),
            ErrorKind::InvalidState
        );
        assert_eq!(counts.deactivates(), 0);
    }

    #[test]
    fn deactivating_while_processing_is_refused() {
        let (mut instance, _) = spy();
        instance.init().unwrap();
        instance.activate(&config(48_000.0, 64)).unwrap();
        instance.start_processing().unwrap();

        let err = instance.deactivate().unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidState);
        assert!(err.message().contains("stop_processing"));
        assert_eq!(instance.state(), InstanceState::Processing);
        // The prepared configuration must survive the refused teardown.
        assert!(instance.config().is_some());
    }

    #[test]
    fn deactivating_something_that_is_not_active_is_refused() {
        let (mut instance, _) = spy();
        assert_eq!(
            instance.deactivate().unwrap_err().kind(),
            ErrorKind::InvalidState
        );
        instance.init().unwrap();
        assert_eq!(
            instance.deactivate().unwrap_err().kind(),
            ErrorKind::InvalidState
        );
    }

    #[test]
    fn reset_is_legal_only_between_activate_and_start_processing() {
        let (mut instance, counts) = spy();
        assert_eq!(
            instance.reset().unwrap_err().kind(),
            ErrorKind::InvalidState
        );
        instance.init().unwrap();
        assert_eq!(
            instance.reset().unwrap_err().kind(),
            ErrorKind::InvalidState
        );

        instance.activate(&config(48_000.0, 64)).unwrap();
        instance.reset().unwrap();
        assert_eq!(counts.resets(), 1);

        instance.start_processing().unwrap();
        let err = instance.reset().unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidState);
        assert!(err.message().contains("stop_processing"));
        assert_eq!(
            counts.resets(),
            1,
            "reset must not run under the audio thread"
        );

        // The documented way to clear state mid-session.
        instance.stop_processing().unwrap();
        instance.reset().unwrap();
        instance.start_processing().unwrap();
        assert_eq!(counts.resets(), 2);
    }

    #[test]
    fn the_whole_cycle_runs_and_can_be_repeated_with_a_new_configuration() {
        let (mut instance, counts) = spy();
        instance.init().unwrap();

        instance.activate(&config(48_000.0, 128)).unwrap();
        instance.start_processing().unwrap();
        let mut b = block(128);
        for _ in 0..4 {
            assert_eq!(b.run(&mut instance), ProcessStatus::Continue);
        }
        instance.stop_processing().unwrap();
        instance.deactivate().unwrap();
        assert_eq!(instance.state(), InstanceState::Inactive);
        assert!(instance.config().is_none());

        // A host changing the sample rate re-prepares the same object; the processor must
        // tolerate it without being dropped.
        instance.activate(&config(96_000.0, 64)).unwrap();
        assert_eq!(instance.config().unwrap().sample_rate, 96_000.0);
        instance.start_processing().unwrap();
        let mut small = block(64);
        assert_eq!(small.run(&mut instance), ProcessStatus::Continue);
        // …and the old, larger block size is now out of bounds.
        assert_eq!(b.run(&mut instance), ProcessStatus::Error);
        instance.stop_processing().unwrap();
        instance.deactivate().unwrap();

        assert_eq!(counts.prepares(), 2);
        assert_eq!(counts.activates(), 2);
        assert_eq!(counts.deactivates(), 2);
        assert_eq!(counts.processes(), 5);
        assert_eq!(counts.rates(), vec![48_000.0, 96_000.0]);
    }

    #[test]
    fn the_process_mode_reaches_the_plug_in_unchanged() {
        let (mut instance, _) = spy();
        instance.init().unwrap();
        let cfg = config(44_100.0, 512).with_process_mode(ProcessMode::Offline);
        instance.activate(&cfg).unwrap();
        assert_eq!(
            instance.config().unwrap().process_mode,
            ProcessMode::Offline,
            "an adapter reads the mode back from the instance"
        );
    }

    #[test]
    fn a_poisoned_instance_refuses_everything_for_ever() {
        let (mut instance, counts) = spy();
        instance.init().unwrap();
        instance.activate(&config(48_000.0, 64)).unwrap();
        instance.start_processing().unwrap();

        instance.poison();
        assert!(instance.is_poisoned());
        assert_eq!(instance.state(), InstanceState::Poisoned);

        let mut b = block(64);
        b.fill_output(0.5);
        assert_eq!(b.run(&mut instance), ProcessStatus::Error);
        assert!(b.output_is_silent());
        assert_eq!(b.run_f64(&mut instance), ProcessStatus::Error);
        refuse_everything(&mut instance, ErrorKind::InvalidState);
        assert_eq!(
            instance.set_host(HostServices::null()).unwrap_err().kind(),
            ErrorKind::InvalidState
        );

        // Poisoning twice changes nothing, and the plug-in was never touched again.
        instance.poison();
        assert_eq!(counts.processes(), 0);
        assert_eq!(counts.resets(), 0);
    }

    #[test]
    fn poisoning_reports_the_panic_rather_than_the_transition() {
        let (mut instance, _) = spy();
        instance.poison();
        let err = instance.init().unwrap_err();
        assert_eq!(err.status_code(), daux_core::status::INVALID_STATE);
        assert!(
            err.message().contains("poisoned"),
            "a host should be able to tell a poisoned instance from a mis-sequenced one: {err}"
        );
    }

    #[test]
    fn set_host_is_refused_once_the_plug_in_is_running() {
        let (mut instance, counts) = spy();
        instance.init().unwrap();
        instance.set_host(HostServices::null()).unwrap();
        instance.activate(&config(48_000.0, 64)).unwrap();

        let err = instance.set_host(HostServices::null()).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidState);
        assert_eq!(counts.hosts(), 1);

        instance.start_processing().unwrap();
        assert_eq!(
            instance.set_host(HostServices::null()).unwrap_err().kind(),
            ErrorKind::InvalidState
        );
        assert_eq!(counts.hosts(), 1);
    }

    #[test]
    fn state_and_parameters_stay_reachable_while_audio_runs() {
        let (mut instance, counts) = spy();
        instance.init().unwrap();
        instance.activate(&config(48_000.0, 64)).unwrap();
        instance.start_processing().unwrap();

        // A DAW saves projects and loads presets without stopping the transport.
        let mut w = StateWriter::new(StateVersion(1));
        instance.save_state(&mut w).unwrap();
        let bytes = w.try_finish().expect("the spy writes a valid document");
        let reader = daux_state::StateReader::from_bytes(&bytes).expect("round-trips");
        instance.load_state(&reader).unwrap();
        assert_eq!(counts.saves(), 1);
        assert_eq!(counts.loads(), 1);

        assert!(instance.params().unwrap().param_refs().is_empty());
        instance.on_main_thread().unwrap();
        instance.on_worker(TaskId::new(7)).unwrap();
        assert_eq!(counts.main_thread(), 1);
        assert_eq!(counts.workers(), 1);
    }

    #[test]
    fn an_editor_can_be_opened_and_dropped_while_the_dsp_keeps_running() {
        let (mut instance, counts) = spy();
        instance.init().unwrap();
        instance.activate(&config(48_000.0, 64)).unwrap();
        instance.start_processing().unwrap();

        for _ in 0..3 {
            let editor = instance.create_editor().unwrap();
            assert!(editor.is_some(), "the spy has an editor");
            drop(editor);
            // Rule 9: closing an editor must not touch DSP state.
            let mut b = block(64);
            assert_eq!(b.run(&mut instance), ProcessStatus::Continue);
        }
        assert_eq!(counts.editors(), 3);
        assert_eq!(counts.processes(), 3);
        assert_eq!(instance.state(), InstanceState::Processing);
    }

    #[test]
    fn topology_queries_need_an_initialised_instance() {
        let (mut instance, _) = spy();
        let stereo = BusLayout::stereo_effect();
        assert_eq!(
            instance.bus_layout().unwrap_err().kind(),
            ErrorKind::InvalidState
        );
        assert_eq!(
            instance.event_ports().unwrap_err().kind(),
            ErrorKind::InvalidState
        );
        assert_eq!(
            instance.accepts_bus_layout(&stereo).unwrap_err().kind(),
            ErrorKind::InvalidState
        );
        assert_eq!(
            instance.latency().unwrap_err().kind(),
            ErrorKind::InvalidState
        );
        assert_eq!(instance.tail().unwrap_err().kind(), ErrorKind::InvalidState);

        instance.init().unwrap();
        assert_eq!(instance.bus_layout().unwrap().inputs.len(), 1);
        assert_eq!(instance.event_ports().unwrap(), EventPortLayout::none());
        assert!(instance.accepts_bus_layout(&stereo).unwrap());
        assert_eq!(instance.latency().unwrap(), Latency::Samples(64));
        assert_eq!(instance.tail().unwrap(), Tail::Samples(128));
    }

    #[test]
    fn dropping_a_processing_instance_stops_the_processor_first() {
        let counts = std::sync::Arc::new(crate::testkit::Counts::default());
        {
            let mut instance = PluginInstance::for_plugin(Spy::new(counts.clone()));
            instance.init().unwrap();
            instance.activate(&config(48_000.0, 64)).unwrap();
            instance.start_processing().unwrap();
            assert_eq!(counts.deactivates(), 0);
        }
        assert_eq!(
            counts.deactivates(),
            1,
            "a host that forgot stop_processing must not drop a live processor"
        );
    }

    #[test]
    fn dropping_an_inactive_instance_does_not_invent_a_deactivate() {
        let counts = std::sync::Arc::new(crate::testkit::Counts::default());
        {
            let mut instance = PluginInstance::for_plugin(Spy::new(counts.clone()));
            instance.init().unwrap();
            instance.activate(&config(48_000.0, 64)).unwrap();
            instance.start_processing().unwrap();
            instance.stop_processing().unwrap();
        }
        assert_eq!(counts.deactivates(), 1, "exactly one deactivate, not two");
    }

    /// The refusal paths are reachable from the audio thread, so they must obey its rules.
    /// Every message a refusal can carry is a `&'static str` precisely so this holds.
    #[test]
    fn refusing_a_real_time_call_allocates_nothing() {
        let (mut instance, _) = spy();
        instance.init().unwrap();
        instance.activate(&config(48_000.0, 64)).unwrap();
        let mut b = block(64);
        // Preallocated outside every measured scope: the block, not the refusal, is what
        // would otherwise allocate.
        let mut over = block(4096);

        assert_no_alloc("a refused process", || b.run(&mut instance));
        assert_no_alloc("a refused process_f64", || b.run_f64(&mut instance));
        assert_no_alloc("a refused stop_processing", || {
            assert!(instance.stop_processing().is_err());
        });

        instance.start_processing().unwrap();
        assert_no_alloc("a refused over-long block", || {
            assert_eq!(over.run(&mut instance), ProcessStatus::Error);
        });
        assert_no_alloc("a refused reset", || {
            assert!(instance.reset().is_err());
        });

        let (mut dead, _) = spy();
        dead.poison();
        let mut dead_block = block(64);
        assert_no_alloc("a poisoned instance", || {
            assert_eq!(dead_block.run(&mut dead), ProcessStatus::Error);
            assert!(dead.stop_processing().is_err());
            assert!(dead.reset().is_err());
            assert!(dead.start_processing().is_err());
            assert!(dead.latency().is_err());
            assert!(dead.tail().is_err());
        });
    }

    /// And so must the happy path: the wrapper adds a state check, nothing more.
    #[test]
    fn the_wrapper_adds_no_allocation_to_a_block() {
        let (mut instance, counts) = spy();
        instance.init().unwrap();
        instance.activate(&config(48_000.0, 64)).unwrap();
        instance.start_processing().unwrap();
        let mut b = block(64);

        assert_no_alloc("a processed block", || {
            assert_eq!(b.run(&mut instance), ProcessStatus::Continue);
        });
        assert_no_alloc("stop_processing", || {
            instance.stop_processing().unwrap();
        });
        assert_no_alloc("reset", || instance.reset().unwrap());
        assert_no_alloc("start_processing", || {
            instance.start_processing().unwrap();
        });
        assert_eq!(counts.processes(), 1);
    }

    #[test]
    fn an_instance_moves_to_the_thread_that_will_run_it() {
        const fn assert_send<T: Send>() {}
        assert_send::<PluginInstance>();

        let (mut instance, counts) = spy();
        instance.init().unwrap();
        let handle = std::thread::spawn(move || {
            instance.activate(&config(48_000.0, 64)).unwrap();
            instance.start_processing().unwrap();
            instance.state()
        });
        assert_eq!(handle.join().unwrap(), InstanceState::Processing);
        assert_eq!(counts.prepares(), 1);
    }

    #[test]
    fn every_state_has_a_name_and_the_predicates_agree_with_it() {
        for state in InstanceState::ALL {
            assert!(!state.as_str().is_empty());
            assert_eq!(state.to_string(), state.as_str());
            assert_eq!(
                state.is_initialised(),
                matches!(
                    state,
                    InstanceState::Inactive | InstanceState::Active | InstanceState::Processing
                )
            );
            assert_eq!(state.is_processing(), state == InstanceState::Processing);
            assert_eq!(state.is_poisoned(), state == InstanceState::Poisoned);
        }
        assert_eq!(InstanceState::default(), InstanceState::Created);
        // A poisoned instance is not "initialised" for the purposes of the guards.
        assert!(!InstanceState::Poisoned.is_initialised());
    }
}
