//! A real plug-in, for testing the harness that tests plug-ins.
//!
//! Deliberately not a mock: it reads automation out of its event list, answers note events,
//! saves and loads state, reports latency and tail, and talks back to the host. A harness
//! proved correct against a stub would only be proved correct against the harness's own
//! assumptions.
//!
//! Everything the plug-in does is recorded in a [`Probe`] the test keeps a handle to, so the
//! lifecycle the harness is supposed to drive — prepare, activate, process, reset,
//! deactivate — can be asserted from outside rather than assumed.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use daux_audio::{AudioBuses, BusLayout, ChannelLayout};
use daux_events::DauxEvent;
use daux_parameter::{FloatParam, Param, ParamId, ParamRange, Params};
use daux_runtime::daux_core::daux_host_services::{HostServices, TaskId};
use daux_runtime::daux_core::daux_state::{StateReader, StateWriter};
use daux_runtime::daux_core::{
    Capabilities, Category, DauxController, DauxError, DauxPlugin, DauxProcessor, DauxResult,
    ErrorKind, Latency, PluginDescriptor, ProcessConfig, ProcessContext, ProcessEvents,
    ProcessStatus, Tail,
};

/// The gain parameter's permanent id. Never renumber one of these.
pub(crate) const GAIN_ID: ParamId = ParamId(1);
/// The bypass parameter's permanent id.
pub(crate) const BYPASS_ID: ParamId = ParamId(2);

/// What the plug-in has been asked to do, visible from the test that installed it.
#[derive(Debug, Default)]
pub(crate) struct Probe {
    pub(crate) prepares: AtomicUsize,
    pub(crate) activations: AtomicUsize,
    pub(crate) deactivations: AtomicUsize,
    pub(crate) resets: AtomicUsize,
    pub(crate) blocks: AtomicUsize,
    pub(crate) notes: AtomicUsize,
    pub(crate) main_thread_calls: AtomicUsize,
    pub(crate) worker_tasks: Mutex<Vec<TaskId>>,
    pub(crate) has_host: AtomicBool,
    /// The largest block the plug-in was told to expect, from `prepare`.
    pub(crate) max_block_size: AtomicUsize,
}

impl Probe {
    fn bump(counter: &AtomicUsize) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn count(counter: &AtomicUsize) -> usize {
        counter.load(Ordering::Relaxed)
    }

    pub(crate) fn tasks(&self) -> Vec<TaskId> {
        self.worker_tasks
            .lock()
            .map_or_else(|e| e.into_inner().clone(), |g| g.clone())
    }
}

/// Shared parameter state: the processor and the controller hold the same `Arc`, which is
/// the shape `daux-parameter` is designed around.
pub(crate) struct GainParams {
    gain: FloatParam,
    bypass: FloatParam,
}

impl Default for GainParams {
    fn default() -> Self {
        Self {
            gain: FloatParam::new(
                GAIN_ID,
                "Gain",
                1.0,
                ParamRange::Linear { min: 0.0, max: 4.0 },
            ),
            bypass: FloatParam::new(BYPASS_ID, "Bypass", 0.0, ParamRange::Boolean),
        }
    }
}

impl Params for GainParams {
    fn param_refs(&self) -> Vec<(ParamId, &dyn Param)> {
        vec![(GAIN_ID, &self.gain), (BYPASS_ID, &self.bypass)]
    }

    /// Overridden so the audio thread can look a parameter up without allocating.
    fn param(&self, id: ParamId) -> Option<&dyn Param> {
        match id {
            GAIN_ID => Some(&self.gain),
            BYPASS_ID => Some(&self.bypass),
            _ => None,
        }
    }

    fn state_schema_version(&self) -> u32 {
        2
    }
}

/// A gain with sample-accurate automation, a note counter and reported latency.
pub(crate) struct GainPlugin {
    processor: GainProcessor,
    controller: GainController,
}

impl Default for GainPlugin {
    fn default() -> Self {
        Self::with_probe(Arc::new(Probe::default()))
    }
}

impl GainPlugin {
    /// Builds the plug-in around a probe the caller keeps. [main-thread]
    pub(crate) fn with_probe(probe: Arc<Probe>) -> Self {
        let params = Arc::new(GainParams::default());
        Self {
            processor: GainProcessor {
                params: Arc::clone(&params),
                probe: Arc::clone(&probe),
            },
            controller: GainController { params, probe },
        }
    }
}

impl DauxPlugin for GainPlugin {
    fn descriptor() -> PluginDescriptor {
        PluginDescriptor::builder("com.futureboard.test.gain", "Test Gain")
            .vendor("Futureboard Studio")
            .version((1, 2, 3))
            .category(Category::Effect)
            .capabilities(Capabilities::AUDIO_EFFECT.union(Capabilities::MIDI_INPUT))
            .state_schema_version(2)
            .build()
            .expect("the fixture's own descriptor must be valid")
    }

    fn bus_layout(&self) -> BusLayout {
        BusLayout::effect(ChannelLayout::Stereo)
    }

    fn processor(&mut self) -> &mut dyn DauxProcessor {
        &mut self.processor
    }

    fn controller(&mut self) -> &mut dyn DauxController {
        &mut self.controller
    }
}

pub(crate) struct GainProcessor {
    params: Arc<GainParams>,
    probe: Arc<Probe>,
}

impl DauxProcessor for GainProcessor {
    fn prepare(&mut self, config: &ProcessConfig) -> DauxResult<()> {
        Probe::bump(&self.probe.prepares);
        self.probe
            .max_block_size
            .store(config.max_block_size as usize, Ordering::Relaxed);
        Ok(())
    }

    fn activate(&mut self) -> DauxResult<()> {
        Probe::bump(&self.probe.activations);
        Ok(())
    }

    fn deactivate(&mut self) {
        Probe::bump(&self.probe.deactivations);
    }

    fn reset(&mut self) {
        Probe::bump(&self.probe.resets);
    }

    fn process<'a>(
        &mut self,
        ctx: &ProcessContext<'a>,
        audio: &mut AudioBuses<'a, f32>,
        events: &mut ProcessEvents<'a>,
    ) -> ProcessStatus {
        Probe::bump(&self.probe.blocks);

        // Automation first, so a value that arrives at offset 0 applies to the whole block.
        let (input_events, output_events) = events.split();
        for index in 0..input_events.len() {
            match input_events.get(index) {
                Some(DauxEvent::ParamValue(event)) => {
                    if let Some(param) = self.params.param(ParamId(event.param_id)) {
                        param.set_plain(event.value);
                    }
                }
                Some(DauxEvent::NoteOn(note)) => {
                    Probe::bump(&self.probe.notes);
                    // Echoing the note back proves the output list reaches the host.
                    let _ = output_events.try_push(&DauxEvent::NoteOn(note));
                }
                _ => {}
            }
        }

        let gain = if self.params.bypass.plain() >= 0.5 {
            1.0
        } else {
            self.params.gain.plain() as f32
        };

        let frames = ctx.frames();
        let input = audio.input(0);
        let Some(mut output) = audio.main_output() else {
            return ProcessStatus::Continue;
        };
        for channel in 0..output.channel_count() {
            let destination = output.channel_mut(channel);
            match input
                .as_ref()
                .and_then(|buffer| buffer.get_channel(channel))
            {
                Some(source) => {
                    let count = frames.min(destination.len()).min(source.len());
                    for frame in 0..count {
                        destination[frame] = source[frame] * gain;
                    }
                }
                // No input bus: behave like an instrument and write silence rather than
                // leaving whatever the host's buffer happened to contain.
                None => {
                    let count = frames.min(destination.len());
                    destination[..count].fill(0.0);
                }
            }
        }
        ProcessStatus::Continue
    }

    fn latency(&self) -> Latency {
        Latency::Samples(64)
    }

    fn tail(&self) -> Tail {
        Tail::Samples(128)
    }
}

pub(crate) struct GainController {
    params: Arc<GainParams>,
    probe: Arc<Probe>,
}

impl DauxController for GainController {
    fn params(&self) -> &dyn Params {
        self.params.as_ref()
    }

    fn save_state(&self, writer: &mut StateWriter) -> DauxResult<()> {
        writer.put_f64("gain", self.params.gain.plain());
        writer.put_bool("bypass", self.params.bypass.plain() >= 0.5);
        Ok(())
    }

    fn load_state(&mut self, reader: &StateReader) -> DauxResult<()> {
        if let Some(gain) = reader.opt_f64("gain") {
            self.params.gain.set_plain(gain);
        }
        if let Some(bypass) = reader.opt_bool("bypass") {
            self.params.bypass.set_plain(if bypass { 1.0 } else { 0.0 });
        }
        Ok(())
    }

    fn set_host(&mut self, host: HostServices) {
        // Exactly what a real plug-in does with the services: report what it knows, and
        // degrade where the host offers nothing.
        if let Some(latency) = host.latency() {
            latency.set_samples(64);
        }
        if let Some(tail) = host.tail() {
            tail.changed();
        }
        self.probe.has_host.store(true, Ordering::Relaxed);
    }

    fn on_main_thread(&mut self) {
        Probe::bump(&self.probe.main_thread_calls);
    }

    fn on_worker(&mut self, task: TaskId) {
        match self.probe.worker_tasks.lock() {
            Ok(mut tasks) => tasks.push(task),
            Err(poisoned) => poisoned.into_inner().push(task),
        }
    }
}

/// A plug-in whose `prepare` refuses, to prove the harness reports it rather than carrying
/// on with an unprepared processor.
#[derive(Default)]
pub(crate) struct RefusingPlugin {
    processor: RefusingProcessor,
    controller: GainController,
}

impl Default for GainController {
    fn default() -> Self {
        Self {
            params: Arc::new(GainParams::default()),
            probe: Arc::new(Probe::default()),
        }
    }
}

#[derive(Default)]
pub(crate) struct RefusingProcessor;

impl DauxProcessor for RefusingProcessor {
    fn prepare(&mut self, _config: &ProcessConfig) -> DauxResult<()> {
        Err(DauxError::new(
            ErrorKind::Unsupported,
            "this plug-in cannot run at that sample rate",
        ))
    }

    fn process<'a>(
        &mut self,
        _ctx: &ProcessContext<'a>,
        _audio: &mut AudioBuses<'a, f32>,
        _events: &mut ProcessEvents<'a>,
    ) -> ProcessStatus {
        ProcessStatus::Error
    }
}

impl DauxPlugin for RefusingPlugin {
    fn descriptor() -> PluginDescriptor {
        PluginDescriptor::builder("com.futureboard.test.refusing", "Refusing")
            .vendor("Futureboard Studio")
            .build()
            .expect("valid")
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
}
