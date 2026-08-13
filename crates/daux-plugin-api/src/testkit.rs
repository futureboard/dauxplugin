//! Test doubles shared by this crate's unit tests.
//!
//! Everything here is `#[cfg(test)]`. The point of the `Spy` is that it records what the
//! wrapper did to it without allocating on the audio thread, so the same plug-in can be used
//! both for state-machine assertions and for allocation assertions.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use daux_audio::{AudioBuses, AudioStorage, BusLayout};
use daux_core::{
    DauxController, DauxPlugin, DauxProcessor, DauxResult, ErrorKind, Latency, PluginDescriptor,
    ProcessConfig, ProcessContext, ProcessEvents, ProcessStatus, Tail,
};
use daux_events::EventBuffer;
use daux_graphics::{
    DauxGraphic, DauxGraphicResult, GraphicCapabilities, GraphicContext, GraphicDescriptor,
    GraphicFramework, GraphicProfile, GraphicRenderer, LogicalSize, PhysicalSize, PresentationMode,
};
use daux_host_services::{HostServices, RtHostServices, TaskId};
use daux_parameter::{Param, ParamId, Params};
use daux_state::{StateReader, StateWriter};

use crate::PluginInstance;

/// Runs `f` and asserts it allocated nothing, refusing to pass vacuously.
pub fn assert_no_alloc<R>(what: &str, f: impl FnOnce() -> R) -> R {
    assert!(
        daux_rt::counting_allocator_installed(),
        "the allocation tripwire is not installed, so this test would pass vacuously"
    );
    let (result, allocations) = daux_rt::AllocGuard::scope(f);
    assert_eq!(allocations, 0, "{what} allocated {allocations} time(s)");
    result
}

/// The error a refusal produced, for results whose `Ok` type is not `Debug` — a
/// `&dyn Params` or a `Box<dyn DauxGraphic>`, neither of which can be.
#[track_caller]
pub fn expect_err<T>(result: DauxResult<T>) -> daux_core::DauxError {
    match result {
        Ok(_) => panic!("expected the call to be refused, but it succeeded"),
        Err(err) => err,
    }
}

/// A real-time-safe `ProcessConfig` builder for tests.
pub fn config(sample_rate: f64, max_block_size: u32) -> ProcessConfig {
    ProcessConfig::new(sample_rate, max_block_size)
}

/// Everything the [`Spy`] was asked to do, readable from the test thread.
#[derive(Default)]
pub struct Counts {
    prepares: AtomicUsize,
    activates: AtomicUsize,
    deactivates: AtomicUsize,
    resets: AtomicUsize,
    processes: AtomicUsize,
    f64_processes: AtomicUsize,
    main_thread: AtomicUsize,
    workers: AtomicUsize,
    hosts: AtomicUsize,
    editors: AtomicUsize,
    saves: AtomicUsize,
    loads: AtomicUsize,
    rates: Mutex<Vec<f64>>,
    fail_prepare: AtomicBool,
    fail_activate: AtomicBool,
}

macro_rules! counter {
    ($name:ident) => {
        /// How many times this happened.
        pub fn $name(&self) -> usize {
            self.$name.load(Ordering::Relaxed)
        }
    };
}

impl Counts {
    counter!(prepares);
    counter!(activates);
    counter!(deactivates);
    counter!(resets);
    counter!(processes);
    counter!(f64_processes);
    counter!(main_thread);
    counter!(workers);
    counter!(hosts);
    counter!(editors);
    counter!(saves);
    counter!(loads);

    /// The sample rate of every `prepare`, in order.
    pub fn rates(&self) -> Vec<f64> {
        self.rates.lock().expect("no test panics inside").clone()
    }

    /// Makes the next `prepare` fail with [`ErrorKind::OutOfMemory`].
    pub fn fail_prepare(&self, fail: bool) {
        self.fail_prepare.store(fail, Ordering::Relaxed);
    }

    /// Makes the next `activate` fail with [`ErrorKind::Unsupported`].
    pub fn fail_activate(&self, fail: bool) {
        self.fail_activate.store(fail, Ordering::Relaxed);
    }
}

/// A parameter set with nothing in it.
struct NoParams;

impl Params for NoParams {
    fn param_refs(&self) -> Vec<(ParamId, &dyn Param)> {
        Vec::new()
    }
}

struct SpyProcessor {
    counts: Arc<Counts>,
}

impl DauxProcessor for SpyProcessor {
    fn prepare(&mut self, config: &ProcessConfig) -> DauxResult<()> {
        // Counted before the failure check, so `prepares` means "times the plug-in was
        // asked", which is what the tests about refusal need to distinguish.
        self.counts.prepares.fetch_add(1, Ordering::Relaxed);
        if self.counts.fail_prepare.load(Ordering::Relaxed) {
            return Err(ErrorKind::OutOfMemory.error("the spy was told to fail"));
        }
        self.counts
            .rates
            .lock()
            .expect("no test panics inside")
            .push(config.sample_rate);
        Ok(())
    }

    fn activate(&mut self) -> DauxResult<()> {
        // `activate` is `[audio-thread]`, so its failure path must not allocate either.
        if self.counts.fail_activate.load(Ordering::Relaxed) {
            return Err(ErrorKind::Unsupported.with_static("the spy was told to fail"));
        }
        self.counts.activates.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn deactivate(&mut self) {
        self.counts.deactivates.fetch_add(1, Ordering::Relaxed);
    }

    fn reset(&mut self) {
        self.counts.resets.fetch_add(1, Ordering::Relaxed);
    }

    fn process<'a>(
        &mut self,
        _ctx: &ProcessContext<'a>,
        _audio: &mut AudioBuses<'a, f32>,
        _events: &mut ProcessEvents<'a>,
    ) -> ProcessStatus {
        self.counts.processes.fetch_add(1, Ordering::Relaxed);
        ProcessStatus::Continue
    }

    fn process_f64<'a>(
        &mut self,
        _ctx: &ProcessContext<'a>,
        _audio: &mut AudioBuses<'a, f64>,
        _events: &mut ProcessEvents<'a>,
    ) -> ProcessStatus {
        self.counts.f64_processes.fetch_add(1, Ordering::Relaxed);
        ProcessStatus::Continue
    }

    fn latency(&self) -> Latency {
        Latency::Samples(64)
    }

    fn tail(&self) -> Tail {
        Tail::Samples(128)
    }
}

struct SpyController {
    counts: Arc<Counts>,
    params: NoParams,
}

impl DauxController for SpyController {
    fn params(&self) -> &dyn Params {
        &self.params
    }

    fn save_state(&self, w: &mut StateWriter) -> DauxResult<()> {
        self.counts.saves.fetch_add(1, Ordering::Relaxed);
        w.put_f64("spy", 1.0);
        Ok(())
    }

    fn load_state(&mut self, _r: &StateReader) -> DauxResult<()> {
        self.counts.loads.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn set_host(&mut self, _host: HostServices) {
        self.counts.hosts.fetch_add(1, Ordering::Relaxed);
    }

    fn on_main_thread(&mut self) {
        self.counts.main_thread.fetch_add(1, Ordering::Relaxed);
    }

    fn on_worker(&mut self, _task: TaskId) {
        self.counts.workers.fetch_add(1, Ordering::Relaxed);
    }
}

/// A plug-in that records everything the wrapper does to it.
pub struct Spy {
    processor: SpyProcessor,
    controller: SpyController,
    counts: Arc<Counts>,
}

impl Spy {
    /// Builds a spy reporting into `counts`.
    pub fn new(counts: Arc<Counts>) -> Self {
        Self {
            processor: SpyProcessor {
                counts: counts.clone(),
            },
            controller: SpyController {
                counts: counts.clone(),
                params: NoParams,
            },
            counts,
        }
    }

    /// The permanent id every test looks the spy up by.
    pub const ID: &'static str = "com.example.spy";
}

impl DauxPlugin for Spy {
    fn descriptor() -> PluginDescriptor {
        PluginDescriptor::builder(Self::ID, "Spy")
            .vendor("DAUxPlug tests")
            .build()
            .expect("the spy's descriptor is valid")
    }

    fn bus_layout(&self) -> BusLayout {
        BusLayout::stereo_effect()
    }

    fn processor(&mut self) -> &mut dyn DauxProcessor {
        &mut self.processor
    }

    fn controller(&mut self) -> &mut dyn DauxController {
        &mut self.controller
    }

    fn create_editor(&mut self) -> Option<Box<dyn std::any::Any>> {
        self.counts.editors.fetch_add(1, Ordering::Relaxed);
        crate::editor(SpyEditor::default())
    }
}

/// A minimal editor, enough to prove the downcast produced a usable object.
#[derive(Default)]
pub struct SpyEditor {
    /// How many times [`DauxGraphic::open`] ran.
    pub opened: usize,
    /// How many times [`DauxGraphic::close`] ran.
    pub closed: usize,
}

impl SpyEditor {
    /// The profile the spy editor advertises.
    pub fn profile() -> GraphicProfile {
        GraphicProfile::new(
            GraphicFramework::Custom,
            GraphicRenderer::Software,
            PresentationMode::EmbeddedSurface,
        )
    }
}

impl DauxGraphic for SpyEditor {
    fn descriptor(&self) -> GraphicDescriptor {
        GraphicDescriptor::fixed(
            GraphicCapabilities::new().with(Self::profile()),
            LogicalSize::new(320.0, 240.0),
        )
    }

    fn open(&mut self, _ctx: &mut GraphicContext<'_>) -> DauxGraphicResult<()> {
        self.opened += 1;
        Ok(())
    }

    fn resize(&mut self, _size: PhysicalSize) -> DauxGraphicResult<()> {
        Ok(())
    }

    fn close(&mut self) {
        self.closed += 1;
    }
}

/// A spy instance and the counters that watch it.
pub fn spy() -> (PluginInstance, Arc<Counts>) {
    let counts = Arc::new(Counts::default());
    (PluginInstance::for_plugin(Spy::new(counts.clone())), counts)
}

/// One block's worth of borrowed audio and events, preallocated so that running it can be
/// measured with [`assert_no_alloc`].
pub struct Block {
    frames: usize,
    audio32: AudioStorage<f32>,
    audio64: AudioStorage<f64>,
    input: EventBuffer,
    output: EventBuffer,
    config: ProcessConfig,
    host: RtHostServices,
}

/// A stereo block of `frames` frames with empty event lists.
pub fn block(frames: usize) -> Block {
    Block {
        frames,
        audio32: AudioStorage::new(2, frames),
        audio64: AudioStorage::new(2, frames),
        input: EventBuffer::with_capacity(8, 128),
        output: EventBuffer::with_capacity(8, 128),
        config: ProcessConfig::new(48_000.0, frames as u32),
        host: RtHostServices::null(),
    }
}

impl Block {
    /// Hands this block to `instance` as `f32` audio.
    pub fn run(&mut self, instance: &mut PluginInstance) -> ProcessStatus {
        let ctx = ProcessContext::new(self.frames, &self.config, &self.host);
        let mut outputs = [self.audio32.as_mut()];
        let mut buses = AudioBuses::new(&[], &mut outputs, self.frames);
        let mut events = ProcessEvents::new(&self.input, &mut self.output);
        instance.process(&ctx, &mut buses, &mut events)
    }

    /// Hands this block to `instance` as `f64` audio.
    pub fn run_f64(&mut self, instance: &mut PluginInstance) -> ProcessStatus {
        let ctx = ProcessContext::new(self.frames, &self.config, &self.host);
        let mut outputs = [self.audio64.as_mut()];
        let mut buses = AudioBuses::new(&[], &mut outputs, self.frames);
        let mut events = ProcessEvents::new(&self.input, &mut self.output);
        instance.process_f64(&ctx, &mut buses, &mut events)
    }

    /// Writes `value` into every output sample, so a refusal that leaves the buffer alone is
    /// distinguishable from one that silences it.
    pub fn fill_output(&mut self, value: f32) {
        self.audio32.fill(value);
        self.audio64.fill(f64::from(value));
    }

    /// `true` when every `f32` output sample is zero.
    pub fn output_is_silent(&self) -> bool {
        self.audio32.as_slice().iter().all(|&s| s == 0.0)
    }
}
