//! The harness: a host small enough to put in a unit test.

use std::path::Path;
use std::sync::Arc;

use daux_audio::AudioStorage;
use daux_events::DauxEvent;
use daux_parameter::ParamId;
use daux_runtime::daux_bundle::{Bundle, TargetId};
use daux_runtime::daux_core::{
    DauxPlugin, Latency, PluginDescriptor, ProcessConfig, ProcessStatus, Tail,
};
use daux_runtime::daux_host_services::{HostInfo, HostServices, TaskId};
use daux_runtime::{AxtModule, HostBridge, LoadedFactory};
use daux_transport::Transport;

use crate::error::{HostError, HostErrorKind, HostResult};
use crate::instance::{
    Instance, InstanceId, LoadedInstance, NativeInstance, note_event, param_event,
};
use crate::services::{BundleResources, HarnessHost};

/// An in-process host, for tests and previews. [main-thread]
///
/// ```
/// use daux_host::TestHost;
/// use daux_host::daux_core::ProcessConfig;
///
/// let host = TestHost::new(ProcessConfig::new(44_100.0, 256));
/// assert_eq!(host.config().max_block_size, 256);
/// assert_eq!(host.instance_count(), 0);
/// ```
///
/// # What it is for
///
/// A plug-in is normally only testable inside a DAW, which is a terrible place to find a
/// bug: the feedback loop is minutes long, the failure is a noise rather than an assertion,
/// and half the interesting cases — a host that refuses to resize the editor, a worker queue
/// that is full, a block of one frame — never occur by accident. This harness makes all of
/// them ordinary `cargo test` code.
///
/// # The two ways in
///
/// * [`TestHost::install`] takes a plug-in type compiled into the same binary. No bundle, no
///   ABI, no `dlopen` — the fast path a plug-in author lives in.
/// * [`TestHost::load`] takes an `.axt` on disk and drives it over the C ABI, exactly as a
///   DAW does.
///
/// The same calls drive both. A test that passes one way and fails the other has found a bug
/// in the format adapter rather than in the plug-in, which is the comparison that makes the
/// second path worth having.
///
/// # What the host actually does
///
/// Every `daux.host.*` service is implemented, not stubbed: logging goes into a bounded
/// lock-free queue, gestures and parameter changes are recorded in order, worker requests
/// are queued and can be made to fail, editor and timer requests can be refused, and
/// resources resolve through the bundle's own confinement rules. [`TestHost::host`] hands
/// out the recording so a test can assert on it.
pub struct TestHost {
    config: ProcessConfig,
    services: HostServices,
    recorder: Arc<HarnessHost>,
    instances: Vec<Option<Instance>>,
    transport: Transport,
    steady_time: i64,
    target: TargetId,
}

impl core::fmt::Debug for TestHost {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TestHost")
            .field("config", &self.config)
            .field("instances", &self.instances.iter().flatten().count())
            .field("steady_time", &self.steady_time)
            .finish()
    }
}

impl TestHost {
    /// A harness that will activate its plug-ins with `config`. [main-thread]
    #[must_use]
    pub fn new(config: ProcessConfig) -> Self {
        let recorder = Arc::new(HarnessHost::new());
        let services = HostServices::builder()
            .info(HostInfo::new(
                "DAUx Test Host",
                "Futureboard Studio",
                env!("CARGO_PKG_VERSION"),
            ))
            .log(recorder.clone())
            .params(recorder.clone())
            .latency(recorder.clone())
            .tail(recorder.clone())
            .worker(recorder.clone())
            .gui(recorder.clone())
            .timer(recorder.clone())
            .threads(recorder.clone())
            .rt_host(recorder.clone())
            .build();
        Self {
            config,
            services,
            recorder,
            instances: Vec::new(),
            transport: Transport::EMPTY,
            steady_time: 0,
            target: TargetId::host(),
        }
    }

    /// The configuration every instance is activated with. [main-thread]
    #[inline]
    #[must_use]
    pub const fn config(&self) -> &ProcessConfig {
        &self.config
    }

    /// Everything the plug-ins have asked the host for. [main-thread]
    #[inline]
    #[must_use]
    pub fn host(&self) -> &Arc<HarnessHost> {
        &self.recorder
    }

    /// The services handed to the plug-ins. [main-thread]
    #[inline]
    #[must_use]
    pub const fn services(&self) -> &HostServices {
        &self.services
    }

    /// The transport published to every block. [main-thread]
    #[inline]
    #[must_use]
    pub const fn transport(&self) -> &Transport {
        &self.transport
    }

    /// Replaces the transport published to every block. [main-thread]
    pub const fn set_transport(&mut self, transport: Transport) {
        self.transport = transport;
    }

    /// The monotonic sample counter handed to the next block. [main-thread]
    #[inline]
    #[must_use]
    pub const fn steady_time(&self) -> i64 {
        self.steady_time
    }

    /// Which target's binary [`TestHost::load`] opens. Defaults to this process's.
    /// [main-thread]
    pub fn set_target(&mut self, target: TargetId) {
        self.target = target;
    }

    /// Installs a plug-in compiled into this binary. [main-thread]
    ///
    /// The plug-in is prepared and activated immediately, so [`TestHost::process`] is legal
    /// as soon as this returns.
    ///
    /// # Errors
    ///
    /// Whatever `prepare` or `activate` refused — a sample rate the plug-in cannot run at,
    /// a block size it cannot allocate for.
    pub fn install<P: DauxPlugin + Default>(&mut self) -> HostResult<InstanceId> {
        self.install_plugin(Box::new(P::default()), P::descriptor())
    }

    /// Installs an already-built plug-in with an explicit descriptor. [main-thread]
    ///
    /// For a plug-in that needs constructor arguments, or one whose descriptor a test wants
    /// to bend.
    ///
    /// # Errors
    ///
    /// As [`TestHost::install`].
    pub fn install_plugin(
        &mut self,
        mut plugin: Box<dyn DauxPlugin>,
        descriptor: PluginDescriptor,
    ) -> HostResult<InstanceId> {
        plugin.controller().set_host(self.services.clone());
        let instance = NativeInstance::new(plugin, descriptor, &self.config)?;
        Ok(self.push(Instance::Native(Box::new(instance))))
    }

    /// Loads the principal plug-in of an `.axt` bundle. [main-thread]
    ///
    /// # Errors
    ///
    /// [`HostErrorKind::Load`] when the bundle cannot be opened, ships no binary for this
    /// machine, or is not a DAUx module this host may call into, and whatever `activate`
    /// returned.
    pub fn load(&mut self, bundle: &Path) -> HostResult<InstanceId> {
        let opened = Bundle::open(bundle).map_err(|error| {
            HostError::new(HostErrorKind::Load, error.to_string()).with_path(bundle)
        })?;
        let id = opened.metadata().id.clone();
        self.load_plugin(&opened, &id)
    }

    /// Loads one named plug-in of an already-opened bundle. [main-thread]
    ///
    /// The way into a multi-plug-in bundle, where [`TestHost::load`] would only ever give
    /// the principal one.
    ///
    /// # Errors
    ///
    /// As [`TestHost::load`], plus [`HostErrorKind::Load`] when the module's factory does
    /// not export `id`.
    pub fn load_plugin(&mut self, bundle: &Bundle, id: &str) -> HostResult<InstanceId> {
        let module = Arc::new(AxtModule::load(bundle, &self.target)?);
        let resources: Arc<dyn daux_runtime::daux_host_services::HostResources> =
            Arc::new(BundleResources::new(bundle.resources()));
        let services = HostServices::builder()
            .info(HostInfo::new(
                "DAUx Test Host",
                "Futureboard Studio",
                env!("CARGO_PKG_VERSION"),
            ))
            .log(self.recorder.clone())
            .params(self.recorder.clone())
            .latency(self.recorder.clone())
            .tail(self.recorder.clone())
            .worker(self.recorder.clone())
            .gui(self.recorder.clone())
            .timer(self.recorder.clone())
            .threads(self.recorder.clone())
            .resources(resources)
            .rt_host(self.recorder.clone())
            .build();

        let factory = LoadedFactory::create(module, HostBridge::new(services))?;
        let descriptor = factory
            .descriptors()?
            .into_iter()
            .find(|descriptor| descriptor.id.as_str() == id)
            .ok_or_else(|| {
                HostError::new(
                    HostErrorKind::Load,
                    format!("the module's factory does not export `{id}`"),
                )
                .with_path(bundle.path())
            })?;
        let plugin = factory.create_plugin(id)?;
        let instance = LoadedInstance::new(plugin, descriptor, &self.config)?;
        Ok(self.push(Instance::Loaded(Box::new(instance))))
    }

    /// Destroys an instance. Its id is never reused. [main-thread]
    ///
    /// # Errors
    ///
    /// [`HostErrorKind::NoSuchInstance`] for an id that was already unloaded.
    pub fn unload(&mut self, instance: InstanceId) -> HostResult<()> {
        let slot = self
            .instances
            .get_mut(instance.get() as usize)
            .ok_or_else(|| unknown(instance))?;
        if slot.take().is_none() {
            return Err(unknown(instance));
        }
        Ok(())
    }

    /// How many instances are loaded. [main-thread]
    #[must_use]
    pub fn instance_count(&self) -> usize {
        self.instances.iter().flatten().count()
    }

    /// The plug-in's static description. [main-thread]
    ///
    /// # Errors
    ///
    /// [`HostErrorKind::NoSuchInstance`].
    pub fn descriptor(&self, instance: InstanceId) -> HostResult<&PluginDescriptor> {
        Ok(self.get(instance)?.descriptor())
    }

    /// Whether the instance is compiled in rather than loaded from a bundle. [main-thread]
    ///
    /// # Errors
    ///
    /// [`HostErrorKind::NoSuchInstance`].
    pub fn is_native(&self, instance: InstanceId) -> HostResult<bool> {
        Ok(self.get(instance)?.is_native())
    }

    /// Sets a parameter to a plain value. [main-thread]
    ///
    /// Two things happen, because a host does both: the value is written to the plug-in's
    /// parameter object so that a plug-in reading it directly sees it, **and** a
    /// sample-accurate `ParamValue` event is queued at offset 0 of the next block so that a
    /// plug-in reading automation from its event list sees it too. A plug-in that reads both
    /// gets the same value twice, which is idempotent.
    ///
    /// Failures are recorded rather than raised, because the signature a host drives is
    /// infallible; use [`TestHost::try_set_param`] when the answer matters.
    pub fn set_param(&mut self, instance: InstanceId, id: u32, value: f64) {
        if let Err(error) = self.try_set_param(instance, id, value) {
            self.log_failure("set_param", &error);
        }
    }

    /// Sets a parameter to a plain value, reporting whether it worked. [main-thread]
    ///
    /// # Errors
    ///
    /// [`HostErrorKind::NoSuchInstance`], and [`HostErrorKind::NoSuchParam`] when the
    /// plug-in has no parameter with that id.
    pub fn try_set_param(&mut self, instance: InstanceId, id: u32, value: f64) -> HostResult<()> {
        let param = ParamId(id);
        match self.get_mut(instance)? {
            Instance::Native(native) => {
                if !native.set_param_value(param, value) {
                    return Err(HostError::new(
                        HostErrorKind::NoSuchParam,
                        format!("the plug-in has no parameter {id}"),
                    ));
                }
                let event = param_event(0, param, value);
                if !native.queue(&DauxEvent::ParamValue(event)) {
                    return Err(HostError::new(
                        HostErrorKind::BadBlock,
                        "the block's event list is full",
                    ));
                }
            }
            Instance::Loaded(loaded) => {
                if loaded.param_value(param).is_none() {
                    return Err(HostError::new(
                        HostErrorKind::NoSuchParam,
                        format!("the plug-in has no parameter {id}"),
                    ));
                }
                if !loaded.queue_param(param, value, 0) {
                    return Err(HostError::new(
                        HostErrorKind::BadBlock,
                        "the block's event list is full",
                    ));
                }
            }
        }
        Ok(())
    }

    /// The plug-in's current plain value for a parameter. [main-thread]
    ///
    /// # Errors
    ///
    /// [`HostErrorKind::NoSuchInstance`] or [`HostErrorKind::NoSuchParam`].
    pub fn param_value(&mut self, instance: InstanceId, id: u32) -> HostResult<f64> {
        let param = ParamId(id);
        let value = match self.get_mut(instance)? {
            Instance::Native(native) => native.param_value(param),
            Instance::Loaded(loaded) => loaded.param_value(param),
        };
        value.ok_or_else(|| {
            HostError::new(
                HostErrorKind::NoSuchParam,
                format!("the plug-in has no parameter {id}"),
            )
        })
    }

    /// Queues a note-on for the next block. [main-thread]
    ///
    /// `time` is a sample offset inside the block and `velocity` is `0.0 ..= 1.0`, as
    /// `abi-v1` §9 defines them. Failures are recorded; see [`TestHost::try_send_note_on`].
    pub fn send_note_on(&mut self, instance: InstanceId, time: u32, key: i16, velocity: f64) {
        if let Err(error) = self.try_send_note(instance, true, time, key, velocity) {
            self.log_failure("send_note_on", &error);
        }
    }

    /// Queues a note-off for the next block. [main-thread]
    pub fn send_note_off(&mut self, instance: InstanceId, time: u32, key: i16, velocity: f64) {
        if let Err(error) = self.try_send_note(instance, false, time, key, velocity) {
            self.log_failure("send_note_off", &error);
        }
    }

    /// Queues a note-on, reporting whether it worked. [main-thread]
    ///
    /// # Errors
    ///
    /// [`HostErrorKind::NoSuchInstance`], or [`HostErrorKind::BadBlock`] when the block's
    /// event list is already full.
    pub fn try_send_note_on(
        &mut self,
        instance: InstanceId,
        time: u32,
        key: i16,
        velocity: f64,
    ) -> HostResult<()> {
        self.try_send_note(instance, true, time, key, velocity)
    }

    /// Queues a note-off, reporting whether it worked. [main-thread]
    ///
    /// # Errors
    ///
    /// As [`TestHost::try_send_note_on`].
    pub fn try_send_note_off(
        &mut self,
        instance: InstanceId,
        time: u32,
        key: i16,
        velocity: f64,
    ) -> HostResult<()> {
        self.try_send_note(instance, false, time, key, velocity)
    }

    fn try_send_note(
        &mut self,
        instance: InstanceId,
        on: bool,
        time: u32,
        key: i16,
        velocity: f64,
    ) -> HostResult<()> {
        let queued = match self.get_mut(instance)? {
            Instance::Native(native) => {
                let event = note_event(time, 0, key, velocity);
                let event = if on {
                    DauxEvent::NoteOn(event)
                } else {
                    DauxEvent::NoteOff(event)
                };
                native.queue(&event)
            }
            Instance::Loaded(loaded) => loaded.queue_note(on, time, 0, key, velocity),
        };
        if queued {
            Ok(())
        } else {
            Err(HostError::new(
                HostErrorKind::BadBlock,
                "the block's event list is full",
            ))
        }
    }

    /// Runs one block. [audio-thread]
    ///
    /// `input` and `output` are separate storages: this harness never processes in place,
    /// so a plug-in that writes its output before reading its input is caught here rather
    /// than in a DAW that happens to share the buffer.
    ///
    /// The steady-time counter advances by the block length afterwards, so a sequence of
    /// calls looks to the plug-in like a sequence of blocks rather than the same block
    /// repeated.
    ///
    /// Nothing here allocates, with one exception: the **first** block handed to a loaded
    /// instance, and any later block whose channel counts differ from the last one, rebuilds
    /// that instance's preallocated ABI block. Call it once with the shape a test is going to
    /// use before measuring anything under an allocation tripwire.
    ///
    /// # Errors
    ///
    /// [`HostErrorKind::NoSuchInstance`], and [`HostErrorKind::BadBlock`] for an empty
    /// block, a block longer than the activation allows, or an input whose frame count
    /// disagrees with the output's.
    pub fn process(
        &mut self,
        instance: InstanceId,
        input: &AudioStorage<f32>,
        output: &mut AudioStorage<f32>,
    ) -> HostResult<ProcessStatus> {
        let frames = output.frames();
        self.check_block(input, output)?;

        // Destructured so that the borrow of one instance and the shared borrows of the
        // transport, the configuration and the services are provably disjoint.
        let Self {
            config,
            services,
            instances,
            transport,
            steady_time,
            ..
        } = self;
        let slot = instances
            .get_mut(instance.get() as usize)
            .and_then(Option::as_mut)
            .ok_or_else(|| unknown(instance))?;

        let status = match slot {
            Instance::Native(native) => native.process(
                config,
                services.rt(),
                transport,
                *steady_time,
                input,
                output,
            ),
            Instance::Loaded(loaded) => loaded.process(transport, *steady_time, input, output)?,
        };

        self.steady_time = self.steady_time.saturating_add(frames as i64);
        Ok(status)
    }

    /// Runs one block into a freshly allocated output. [main-thread] — allocates.
    ///
    /// The shape most tests want: silence in, `frames` frames out.
    ///
    /// # Errors
    ///
    /// As [`TestHost::process`].
    pub fn render(
        &mut self,
        instance: InstanceId,
        channels: usize,
        frames: usize,
    ) -> HostResult<AudioStorage<f32>> {
        let input = AudioStorage::<f32>::new(channels, frames);
        let mut output = AudioStorage::<f32>::new(channels, frames);
        self.process(instance, &input, &mut output)?;
        Ok(output)
    }

    /// Serialises the plug-in's state. [main-thread]
    ///
    /// # Errors
    ///
    /// [`HostErrorKind::NoSuchInstance`], [`HostErrorKind::Unsupported`] when a loaded
    /// plug-in publishes no `daux.state/1`, and whatever the plug-in refused with.
    pub fn save_state(&mut self, instance: InstanceId) -> HostResult<Vec<u8>> {
        match self.get_mut(instance)? {
            Instance::Native(native) => native.save_state(),
            Instance::Loaded(loaded) => loaded.save_state(),
        }
    }

    /// Restores the plug-in's state. [main-thread]
    ///
    /// `abi-v1` §12 makes this atomic from the host's point of view: a plug-in that cannot
    /// read the blob must fail without changing anything.
    ///
    /// # Errors
    ///
    /// As [`TestHost::save_state`], plus [`HostErrorKind::Plugin`] for a blob the plug-in
    /// cannot read.
    pub fn load_state(&mut self, instance: InstanceId, bytes: &[u8]) -> HostResult<()> {
        match self.get_mut(instance)? {
            Instance::Native(native) => native.load_state(bytes),
            Instance::Loaded(loaded) => loaded.load_state(bytes),
        }
    }

    /// Clears delay lines, filters and voices. [main-thread]
    ///
    /// # Errors
    ///
    /// [`HostErrorKind::NoSuchInstance`], and whatever the lifecycle refused.
    pub fn reset(&mut self, instance: InstanceId) -> HostResult<()> {
        match self.get_mut(instance)? {
            Instance::Native(native) => {
                native.reset();
                Ok(())
            }
            Instance::Loaded(loaded) => loaded.reset(),
        }
    }

    /// Runs the main-thread callbacks a plug-in asked for. [main-thread]
    ///
    /// A real host does this on its idle timer. Nothing happens unless the plug-in called
    /// `request_callback` or scheduled a worker task, which is what makes it worth asserting
    /// on: a plug-in that never asks is never called.
    ///
    /// Returns the worker tasks that were run.
    ///
    /// # Errors
    ///
    /// [`HostErrorKind::NoSuchInstance`], and whatever the plug-in refused with.
    pub fn run_callbacks(&mut self, instance: InstanceId) -> HostResult<Vec<TaskId>> {
        let tasks = self.recorder.take_worker_tasks();
        match self.get_mut(instance)? {
            Instance::Native(native) => {
                for task in &tasks {
                    native.on_worker(*task);
                }
                native.on_main_thread();
            }
            Instance::Loaded(loaded) => {
                // The ABI has no `on_worker`: a module is told about queued work through
                // `on_main_thread`, and matches it against its own queue (`abi-v1` §11.6).
                loaded.on_main_thread()?;
            }
        }
        Ok(tasks)
    }

    /// The plug-in's current latency. [main-thread]
    ///
    /// # Errors
    ///
    /// [`HostErrorKind::NoSuchInstance`].
    pub fn latency(&mut self, instance: InstanceId) -> HostResult<Latency> {
        Ok(match self.get_mut(instance)? {
            Instance::Native(native) => native.latency(),
            Instance::Loaded(loaded) => loaded.latency(),
        })
    }

    /// The plug-in's current tail. [main-thread]
    ///
    /// # Errors
    ///
    /// [`HostErrorKind::NoSuchInstance`].
    pub fn tail(&mut self, instance: InstanceId) -> HostResult<Tail> {
        Ok(match self.get_mut(instance)? {
            Instance::Native(native) => native.tail(),
            Instance::Loaded(loaded) => loaded.tail(),
        })
    }

    /// How many events the plug-in produced during the last block. [main-thread]
    ///
    /// # Errors
    ///
    /// [`HostErrorKind::NoSuchInstance`].
    pub fn output_event_count(&self, instance: InstanceId) -> HostResult<usize> {
        Ok(match self.get(instance)? {
            Instance::Native(native) => native.output_events().len(),
            Instance::Loaded(loaded) => loaded.output_events().len(),
        })
    }

    /// The events a native instance produced during the last block. [main-thread]
    ///
    /// Only available for an installed plug-in: a loaded one produces ABI records rather
    /// than model events, and translating them here would hide the difference the two paths
    /// exist to expose.
    ///
    /// # Errors
    ///
    /// [`HostErrorKind::NoSuchInstance`], and [`HostErrorKind::Unsupported`] for a loaded
    /// instance.
    pub fn output_events(&self, instance: InstanceId) -> HostResult<&daux_events::EventBuffer> {
        match self.get(instance)? {
            Instance::Native(native) => Ok(native.output_events()),
            Instance::Loaded(_) => Err(HostError::new(
                HostErrorKind::Unsupported,
                "a loaded instance produces ABI event records; read them through the runtime",
            )),
        }
    }

    /// Refuses a block no plug-in could be given.
    fn check_block(
        &self,
        input: &AudioStorage<f32>,
        output: &mut AudioStorage<f32>,
    ) -> HostResult<()> {
        let frames = output.frames();
        if frames == 0 || output.channel_count() == 0 {
            return Err(HostError::new(
                HostErrorKind::BadBlock,
                "a block needs at least one frame and one output channel",
            ));
        }
        if frames > self.config.max_block_size as usize {
            return Err(HostError::new(
                HostErrorKind::BadBlock,
                format!(
                    "{frames} frames is more than the {} the instance was activated for \
                     (abi-v1 §8)",
                    self.config.max_block_size
                ),
            ));
        }
        if input.channel_count() != 0 && input.frames() != frames {
            return Err(HostError::new(
                HostErrorKind::BadBlock,
                format!(
                    "the input has {} frames and the output {frames}; one block is one \
                     length",
                    input.frames()
                ),
            ));
        }
        Ok(())
    }

    fn push(&mut self, instance: Instance) -> InstanceId {
        let index = self.instances.len();
        self.instances.push(Some(instance));
        InstanceId(u32::try_from(index).unwrap_or(u32::MAX))
    }

    fn get(&self, instance: InstanceId) -> HostResult<&Instance> {
        self.instances
            .get(instance.get() as usize)
            .and_then(Option::as_ref)
            .ok_or_else(|| unknown(instance))
    }

    fn get_mut(&mut self, instance: InstanceId) -> HostResult<&mut Instance> {
        self.instances
            .get_mut(instance.get() as usize)
            .and_then(Option::as_mut)
            .ok_or_else(|| unknown(instance))
    }

    /// Records a failure from one of the infallible calls where it can still be seen.
    fn log_failure(&self, what: &str, error: &HostError) {
        use daux_runtime::daux_core::daux_rt::LogLevel;
        self.services
            .log()
            .log(LogLevel::Error, &format!("{what}: {error}"));
    }
}

fn unknown(instance: InstanceId) -> HostError {
    HostError::new(
        HostErrorKind::NoSuchInstance,
        format!("{instance} is not loaded"),
    )
}
