//! The four traits every DAUx plug-in is built from.
//!
//! The split is deliberate and is the reason the rest of the system can be simple:
//!
//! - [`DauxProcessor`] is the audio thread and nothing else. It may not allocate, lock, or
//!   block, and it never sees the main-thread host services.
//! - [`DauxController`] is the main thread: parameters, state, host communication.
//! - [`DauxPlugin`] owns both halves and describes the instance's topology.
//! - [`DauxFactory`] enumerates and instantiates, without loading anything it does not have to.
//!
//! A processor and its controller live in one object graph but are called from different
//! threads at overlapping times. Anything they share must go through the lock-free primitives
//! in `daux-rt`; nothing in this crate will do that for you.

use daux_audio::{AudioBuses, BusLayout};
use daux_host_services::{HostServices, TaskId};
use daux_parameter::Params;
use daux_state::{StateReader, StateWriter};

use crate::{
    DauxResult, EventPortLayout, Latency, PluginDescriptor, ProcessConfig, ProcessContext,
    ProcessEvents, ProcessStatus, Tail,
};

/// The audio-thread half of a plug-in.
///
/// Every method here except [`prepare`](Self::prepare) runs under the rules of
/// `docs/architecture/realtime.md`: no allocation, no deallocation, no locks, no syscalls, no
/// unbounded loops, no panics. `prepare` is the one place a processor is allowed — and
/// expected — to allocate, because it runs on the main thread while the plug-in is inactive.
///
/// # Lifecycle
///
/// ```text
/// prepare ──► activate ──► process* ──► deactivate ──► (prepare again | drop)
///                ▲                          │
///                └──────────────────────────┘
/// ```
///
/// `reset` may be called at any point between `activate` and `deactivate`.
pub trait DauxProcessor: Send {
    /// [main-thread] Allocates everything this processor will need, sized from `config`.
    ///
    /// Called while inactive. May be called again with a different configuration; a processor
    /// must tolerate being re-prepared without being dropped. The host will not call
    /// [`process`](Self::process) until [`activate`](Self::activate) has succeeded.
    ///
    /// # Errors
    ///
    /// Any [`DauxError`](crate::DauxError). The host will not activate a processor whose
    /// `prepare` failed.
    fn prepare(&mut self, config: &ProcessConfig) -> DauxResult<()>;

    /// [audio-thread] Arms the processor for playback.
    ///
    /// Runs on the audio thread with the real-time rules in force: everything it needs must
    /// already exist. The default does nothing, which is correct for most plug-ins.
    ///
    /// # Errors
    ///
    /// Any [`DauxError`](crate::DauxError); the host will not call `process`.
    fn activate(&mut self) -> DauxResult<()> {
        Ok(())
    }

    /// [audio-thread] Disarms the processor. Must not fail and must not deallocate.
    fn deactivate(&mut self) {}

    /// [audio-thread] Clears every internal state that depends on past audio — delay lines,
    /// filter memory, envelope positions, voice allocation.
    ///
    /// Called when the host relocates the playhead or drops the tail. Must not change any
    /// parameter value: a reset is about history, not about configuration.
    fn reset(&mut self) {}

    /// [audio-thread] Processes one block.
    ///
    /// `audio.frames()` and [`ctx.frames()`](ProcessContext::frames) agree and never exceed
    /// the prepared `max_block_size`. Everything borrowed by `ctx`, `audio` and `events` is
    /// invalid the moment this returns.
    ///
    /// A processor that returns [`ProcessStatus::Error`] must still leave its output buffers
    /// in a defined state; silence is the safe choice.
    fn process<'a>(
        &mut self,
        ctx: &ProcessContext<'a>,
        audio: &mut AudioBuses<'a, f32>,
        events: &mut ProcessEvents<'a>,
    ) -> ProcessStatus;

    /// [audio-thread] Processes one block of `f64` audio.
    ///
    /// Only called when the descriptor advertises
    /// [`SampleFormat::F64`](daux_audio::SampleFormat::F64). The default refuses, which is
    /// why advertising `f64` without overriding this is a bug the descriptor validator
    /// cannot catch.
    fn process_f64<'a>(
        &mut self,
        _ctx: &ProcessContext<'a>,
        _audio: &mut AudioBuses<'a, f64>,
        _events: &mut ProcessEvents<'a>,
    ) -> ProcessStatus {
        ProcessStatus::Error
    }

    /// [audio-thread] How far output lags input, in samples at the prepared rate.
    ///
    /// May change only while inactive, or after asking the host to restart. Changing it
    /// silently mid-stream desynchronises every other track in the session.
    fn latency(&self) -> Latency {
        Latency::Zero
    }

    /// [audio-thread] How long this processor keeps producing after its input goes quiet.
    fn tail(&self) -> Tail {
        Tail::None
    }
}

/// The main-thread half of a plug-in: parameters, state and host communication.
///
/// A controller is never called from the audio thread, so it may allocate freely. It shares
/// parameter values with its processor, which is the one piece of cross-thread state every
/// plug-in has; route it through a `daux-rt` primitive, never a `Mutex`.
pub trait DauxController: Send {
    /// [main-thread] The plug-in's parameters.
    ///
    /// The set is fixed for the lifetime of the instance, and each parameter's id is
    /// permanent across versions (abi-v1 §5).
    fn params(&self) -> &dyn Params;

    /// [main-thread] Writes everything needed to reproduce this instance.
    ///
    /// Parameter values are written by the framework; this is for anything else — a loaded
    /// sample path, a wavetable, a modulation matrix.
    ///
    /// # Errors
    ///
    /// Any [`DauxError`](crate::DauxError); the host will report the save as failed rather
    /// than storing a partial blob.
    fn save_state(&self, w: &mut StateWriter) -> DauxResult<()>;

    /// [main-thread] Restores what [`save_state`](Self::save_state) wrote.
    ///
    /// The reader has already run the migration chain, so `r` is always at the current schema
    /// version. A missing key means an older version did not write it: supply a default
    /// rather than failing, or a user's old project stops opening.
    ///
    /// # Errors
    ///
    /// Any [`DauxError`](crate::DauxError).
    fn load_state(&mut self, r: &StateReader) -> DauxResult<()>;

    /// [main-thread] Hands the controller the host services it may use.
    ///
    /// Called once, before anything else, and only if the host offers services at all. Every
    /// service inside is an `Option`: a plug-in must work when the host provides none.
    fn set_host(&mut self, _host: HostServices) {}

    /// [main-thread] Runs work the audio thread asked for via
    /// [`RtHostServices::request_callback`](daux_host_services::RtHostServices::request_callback).
    ///
    /// This is how a processor gets something allocated, logged or told to the host without
    /// doing it itself.
    fn on_main_thread(&mut self) {}

    /// [any-thread] Runs a task scheduled through the host's worker pool.
    ///
    /// Called on a host-owned thread that is neither the audio thread nor necessarily the
    /// main thread. Whatever this touches must be synchronised.
    fn on_worker(&mut self, _task: TaskId) {}
}

/// One plug-in instance: its two halves, its topology, and optionally its editor.
pub trait DauxPlugin: Send + 'static {
    /// [main-thread] The static description of this plug-in.
    ///
    /// An associated function rather than a method, so a factory can describe a plug-in
    /// without instantiating one.
    fn descriptor() -> PluginDescriptor
    where
        Self: Sized;

    /// [main-thread] The audio bus topology this instance currently presents.
    fn bus_layout(&self) -> BusLayout;

    /// [main-thread] The event port topology this instance currently presents.
    fn event_ports(&self) -> EventPortLayout {
        EventPortLayout::default()
    }

    /// [main-thread] The audio-thread half.
    fn processor(&mut self) -> &mut dyn DauxProcessor;

    /// [main-thread] The main-thread half.
    fn controller(&mut self) -> &mut dyn DauxController;

    /// [main-thread] Creates an editor, or `None` for a headless plug-in.
    ///
    /// The editor's lifetime is independent of the processor's: it may be created and
    /// destroyed many times while the processor keeps running, and closing it must never
    /// touch DSP state.
    ///
    /// The return type is opaque because `daux-core` must not depend on `daux-graphics`;
    /// `daux-plugin-api` re-types it into a real editor handle.
    ///
    /// Deliberately not `Send`: an editor is created, driven and destroyed on the thread the
    /// host calls back on, and requiring `Send` would rule out every real UI toolkit — see
    /// [`DauxGraphic`](daux_graphics::DauxGraphic). The audio thread cannot reach this
    /// method, because [`ProcessContext`] cannot reach a `DauxPlugin`.
    fn create_editor(&mut self) -> Option<Box<dyn std::any::Any>> {
        None
    }

    /// [main-thread] Whether this plug-in can run with `layout`.
    ///
    /// Hosts negotiate by proposing layouts and keeping the first accepted one. Accepting
    /// everything, as the default does, means the host will hand you layouts you must then
    /// handle in `process`.
    fn accepts_bus_layout(&self, _layout: &BusLayout) -> bool {
        true
    }
}

/// The entry point of a module: what plug-ins it contains and how to build one.
///
/// A factory must be cheap to construct and must not load resources — a scanner builds one
/// per module purely to read descriptors, and doing real work here makes scanning a library
/// of hundreds of plug-ins slow.
pub trait DauxFactory: Send + Sync + 'static {
    /// [main-thread] How many plug-ins this module exports.
    fn plugin_count(&self) -> usize;

    /// [main-thread] The descriptor at `index`, or `None` when out of range.
    fn descriptor(&self, index: usize) -> Option<PluginDescriptor>;

    /// [main-thread] Instantiates the plug-in with the given permanent id.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::NotFound`](crate::ErrorKind::NotFound) when no plug-in in this module has
    /// that id, or any other [`DauxError`](crate::DauxError) when construction fails.
    fn create(&self, id: &str) -> DauxResult<Box<dyn DauxPlugin>>;

    /// [main-thread] Every descriptor this module exports, in index order.
    ///
    /// A convenience over [`descriptor`](Self::descriptor); stops at the first `None` so a
    /// factory that miscounts cannot make a scanner loop.
    fn descriptors(&self) -> Vec<PluginDescriptor> {
        (0..self.plugin_count())
            .map_while(|i| self.descriptor(i))
            .collect()
    }

    /// [main-thread] `true` when this module exports a plug-in with that id.
    fn contains(&self, id: &str) -> bool {
        (0..self.plugin_count())
            .map_while(|i| self.descriptor(i))
            .any(|d| d.id == *id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ErrorKind, PluginId};
    use daux_parameter::ParamId;

    /// A processor that does nothing, to prove the defaults are usable as-is.
    struct Silent {
        prepared: Option<ProcessConfig>,
        resets: usize,
    }

    impl DauxProcessor for Silent {
        fn prepare(&mut self, config: &ProcessConfig) -> DauxResult<()> {
            config.validate()?;
            self.prepared = Some(*config);
            Ok(())
        }

        fn reset(&mut self) {
            self.resets += 1;
        }

        fn process<'a>(
            &mut self,
            _ctx: &ProcessContext<'a>,
            audio: &mut AudioBuses<'a, f32>,
            _events: &mut ProcessEvents<'a>,
        ) -> ProcessStatus {
            audio.silence_outputs();
            ProcessStatus::ContinueIfNotQuiet
        }
    }

    /// A parameter set with nothing in it.
    struct NoParams;

    impl Params for NoParams {
        fn param_refs(&self) -> Vec<(ParamId, &dyn daux_parameter::Param)> {
            Vec::new()
        }
    }

    struct EmptyController(NoParams);

    impl DauxController for EmptyController {
        fn params(&self) -> &dyn Params {
            &self.0
        }

        fn save_state(&self, _w: &mut StateWriter) -> DauxResult<()> {
            Ok(())
        }

        fn load_state(&mut self, _r: &StateReader) -> DauxResult<()> {
            Ok(())
        }
    }

    struct Nothing {
        processor: Silent,
        controller: EmptyController,
    }

    impl DauxPlugin for Nothing {
        fn descriptor() -> PluginDescriptor {
            PluginDescriptor::builder("com.example.nothing", "Nothing")
                .build()
                .unwrap()
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

    struct OnePlugin;

    impl DauxFactory for OnePlugin {
        fn plugin_count(&self) -> usize {
            1
        }

        fn descriptor(&self, index: usize) -> Option<PluginDescriptor> {
            (index == 0).then(Nothing::descriptor)
        }

        fn create(&self, id: &str) -> DauxResult<Box<dyn DauxPlugin>> {
            if id != Nothing::descriptor().id.as_str() {
                return Err(ErrorKind::NotFound.error(format!("no plug-in `{id}` in this module")));
            }
            Ok(Box::new(Nothing {
                processor: Silent {
                    prepared: None,
                    resets: 0,
                },
                controller: EmptyController(NoParams),
            }))
        }
    }

    #[test]
    fn the_trait_defaults_are_enough_for_a_minimal_plug_in() {
        let factory = OnePlugin;
        let Ok(mut plugin) = factory.create("com.example.nothing") else {
            panic!("the factory must build its own plug-in");
        };

        assert_eq!(plugin.event_ports(), EventPortLayout::none());
        assert!(plugin.create_editor().is_none());
        assert!(plugin.accepts_bus_layout(&BusLayout::stereo_effect()));

        let processor = plugin.processor();
        assert_eq!(processor.latency(), Latency::Zero);
        assert_eq!(processor.tail(), Tail::None);
        processor.prepare(&ProcessConfig::new(48_000.0, 256)).unwrap();
        processor.activate().unwrap();
        processor.reset();
        processor.deactivate();

        assert!(plugin.controller().params().param_refs().is_empty());
    }

    #[test]
    fn prepare_rejects_a_configuration_it_cannot_size_from() {
        let mut p = Silent {
            prepared: None,
            resets: 0,
        };
        let err = p.prepare(&ProcessConfig::new(0.0, 256)).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidArgument);
        assert!(p.prepared.is_none());
    }

    #[test]
    fn f64_processing_is_refused_unless_it_is_implemented() {
        let mut p = Silent {
            prepared: None,
            resets: 0,
        };
        let config = ProcessConfig::new(48_000.0, 64);
        let host = daux_host_services::RtHostServices::null();
        let ctx = ProcessContext::new(0, &config, &host);
        let mut audio = AudioBuses::<f64>::empty(0);
        let input = daux_events::EventBuffer::with_capacity(1, 16);
        let mut output = daux_events::EventBuffer::with_capacity(1, 16);
        let mut events = ProcessEvents::new(&input, &mut output);
        assert_eq!(
            p.process_f64(&ctx, &mut audio, &mut events),
            ProcessStatus::Error
        );
    }

    #[test]
    fn a_factory_enumerates_and_looks_up_by_id() {
        let factory = OnePlugin;
        assert_eq!(factory.plugin_count(), 1);
        assert_eq!(factory.descriptors().len(), 1);
        assert!(factory.contains("com.example.nothing"));
        assert!(!factory.contains("com.example.missing"));
        assert!(factory.descriptor(1).is_none());

        let Err(err) = factory.create("com.example.missing") else {
            panic!("an unknown id must not produce a plug-in");
        };
        assert_eq!(err.kind(), ErrorKind::NotFound);
    }

    #[test]
    fn descriptors_stops_at_the_first_gap_rather_than_looping() {
        /// A deliberately broken factory: it claims three, it has one.
        struct Miscounts;
        impl DauxFactory for Miscounts {
            fn plugin_count(&self) -> usize {
                3
            }
            fn descriptor(&self, index: usize) -> Option<PluginDescriptor> {
                (index == 0).then(Nothing::descriptor)
            }
            fn create(&self, _id: &str) -> DauxResult<Box<dyn DauxPlugin>> {
                Err(ErrorKind::NotFound.error("nothing here"))
            }
        }
        assert_eq!(Miscounts.descriptors().len(), 1);
        assert!(!Miscounts.contains("com.example.anything"));
    }

    #[test]
    fn the_permanent_id_is_the_one_the_factory_answers_to() {
        let id = PluginId::new("com.example.nothing").unwrap();
        assert_eq!(Nothing::descriptor().id, id);
    }
}
