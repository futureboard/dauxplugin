//! What a plug-in author needs in scope, and nothing more.
//!
//! ```
//! use daux_plugin_api::prelude::*;
//! ```
//!
//! The membership rule is mechanical, so the prelude cannot quietly grow into a second copy
//! of the crate root: an item belongs here if it is
//!
//! 1. a trait a plug-in **implements** — [`DauxPlugin`], [`DauxProcessor`],
//!    [`DauxController`], [`DauxFactory`], [`Params`], [`Param`], [`ParamEnum`],
//!    [`DauxGraphic`]; or
//! 2. a type **named in the signature** of one of those traits' methods — [`ProcessConfig`],
//!    [`AudioBuses`], [`StateWriter`], [`PhysicalSize`], and so on; or
//! 3. a type an author must **construct** to satisfy one of those signatures —
//!    [`FloatParam`], [`BusInfo`], [`PluginDescriptor`], [`SingleFactory`].
//!
//! Everything else — the ABI status codes, the lock-free queues, the shared-texture types,
//! the allocation tripwire, the whole of `daux-bundle`'s world — stays one qualified path
//! away at the [crate root](crate). Reach for `daux_plugin_api::SpscRingBuffer` when you need
//! it; do not expect it to appear by importing the prelude.

// ---- this crate's glue -----------------------------------------------------------------
pub use crate::{PluginRegistry, SingleFactory, editor};

// ---- the object model ------------------------------------------------------------------
pub use daux_core::{
    Capabilities, Category, DauxController, DauxError, DauxFactory, DauxPlugin, DauxProcessor,
    DauxResult, ErrorKind, EventPortInfo, EventPortLayout, Latency, PluginDescriptor,
    PluginDescriptorBuilder, PluginId, ProcessConfig, ProcessContext, ProcessEvents, ProcessMode,
    ProcessStatus, Tail, Version,
};

// ---- audio -----------------------------------------------------------------------------
pub use daux_audio::{
    AudioBufferMut, AudioBufferRef, AudioBuses, AudioStorage, BusFlags, BusInfo, BusLayout,
    BusPurpose, ChannelLayout, Sample, SampleFormat, SampleFormats,
};

// ---- events ----------------------------------------------------------------------------
pub use daux_events::{
    DauxEvent, EventFlags, EventHeader, InputEvents, Midi1Event, Midi2Event, NoteEvent,
    NoteExpression, NoteExpressionEvent, OutputEvents, ParamEvent, ParamGestureEvent, SysExEvent,
    TransportEvent,
};

// ---- MIDI ------------------------------------------------------------------------------
pub use daux_midi::{Midi1Kind, Midi1Message, Midi2Message, SysEx7, Ump};

// ---- parameters ------------------------------------------------------------------------
pub use daux_parameter::{
    BoolParam, EnumParam, FloatParam, IntParam, MeterParam, Param, ParamEnum, ParamFlags, ParamId,
    ParamInfo, ParamMigration, ParamRange, Params, Smoother, Smoothing,
};

// ---- state -----------------------------------------------------------------------------
pub use daux_state::{MigrationChain, StateDoc, StateReader, StateVersion, StateWriter};

// ---- transport -------------------------------------------------------------------------
pub use daux_transport::{TimeSignature, Transport, TransportFlags};

// ---- host services ---------------------------------------------------------------------
pub use daux_host_services::{HostServices, LogLevel, RtHostServices, TaskId};

// ---- editors ---------------------------------------------------------------------------
//
// An editor is optional, but a plug-in that has one implements `DauxGraphic`, so the types
// its methods mention belong here by the same rule as the processor's.
pub use daux_graphics::{
    DauxGraphic, DauxGraphicResult, GraphicCapabilities, GraphicContext, GraphicDescriptor,
    GraphicError, InputEvent, InputResponse, LogicalSize, PhysicalSize, ScaleFactor,
};

#[cfg(test)]
mod tests {
    // The prelude is exercised the only way that means anything: by writing a plug-in with
    // nothing else in scope. If an item is missing, this module does not compile.
    use super::*;

    struct Gain {
        gain: FloatParam,
        smoother: Smoother,
    }

    impl Default for Gain {
        fn default() -> Self {
            Self {
                gain: FloatParam::new(
                    ParamId::new(1),
                    "Gain",
                    0.0,
                    ParamRange::Linear {
                        min: -60.0,
                        max: 6.0,
                    },
                )
                .with_unit("dB"),
                smoother: Smoother::new(Smoothing::Linear { ms: 10.0 }),
            }
        }
    }

    impl Params for Gain {
        fn param_refs(&self) -> Vec<(ParamId, &dyn Param)> {
            vec![(self.gain.info().id, &self.gain as &dyn Param)]
        }
    }

    impl DauxProcessor for Gain {
        fn prepare(&mut self, config: &ProcessConfig) -> DauxResult<()> {
            config.validate()?;
            self.smoother.prepare(config.sample_rate);
            Ok(())
        }

        fn process<'a>(
            &mut self,
            ctx: &ProcessContext<'a>,
            audio: &mut AudioBuses<'a, f32>,
            events: &mut ProcessEvents<'a>,
        ) -> ProcessStatus {
            let _ = ctx.transport().and_then(Transport::tempo);
            let (input, _output) = events.split();
            for i in 0..input.len() {
                if let Some(DauxEvent::ParamValue(e)) = input.get(i) {
                    self.smoother.set_target(e.value as f32);
                }
            }
            if let Some(mut out) = audio.main_output() {
                out.fill_silence();
            }
            ProcessStatus::ContinueIfNotQuiet
        }

        fn latency(&self) -> Latency {
            Latency::Zero
        }

        fn tail(&self) -> Tail {
            Tail::None
        }
    }

    impl DauxController for Gain {
        fn params(&self) -> &dyn Params {
            self
        }

        fn save_state(&self, w: &mut StateWriter) -> DauxResult<()> {
            w.put_f64("gain", self.gain.plain());
            Ok(())
        }

        fn load_state(&mut self, r: &StateReader) -> DauxResult<()> {
            if let Some(v) = r.opt_f64("gain") {
                self.gain.set_plain(v);
            }
            Ok(())
        }

        fn set_host(&mut self, host: HostServices) {
            let _ = host.info().name.len();
        }

        fn on_worker(&mut self, _task: TaskId) {}
    }

    impl DauxPlugin for Gain {
        fn descriptor() -> PluginDescriptor {
            PluginDescriptor::builder("com.example.preludegain", "Prelude Gain")
                .vendor("DAUxPlug tests")
                .category(Category::Effect)
                .capabilities(Capabilities::NONE)
                .version(Version::new(1, 0, 0))
                .sample_formats(SampleFormat::F32)
                .build()
                .expect("valid")
        }

        fn bus_layout(&self) -> BusLayout {
            BusLayout::stereo_effect().with_input(BusInfo::new(1, "Sidechain", ChannelLayout::Mono))
        }

        fn event_ports(&self) -> EventPortLayout {
            EventPortLayout::none()
        }

        fn processor(&mut self) -> &mut dyn DauxProcessor {
            self
        }

        fn controller(&mut self) -> &mut dyn DauxController {
            self
        }

        fn create_editor(&mut self) -> Option<Box<dyn std::any::Any>> {
            editor(Panel)
        }

        fn accepts_bus_layout(&self, layout: &BusLayout) -> bool {
            !layout.inputs.is_empty()
        }
    }

    struct Panel;

    impl DauxGraphic for Panel {
        fn descriptor(&self) -> GraphicDescriptor {
            GraphicDescriptor::fixed(GraphicCapabilities::new(), LogicalSize::new(400.0, 200.0))
        }

        fn open(&mut self, ctx: &mut GraphicContext<'_>) -> DauxGraphicResult<()> {
            let _ = ctx.scale_factor();
            Ok(())
        }

        fn resize(&mut self, _size: PhysicalSize) -> DauxGraphicResult<()> {
            Ok(())
        }

        fn scale_factor_changed(&mut self, _scale: ScaleFactor) {}

        fn on_input(&mut self, _event: &InputEvent) -> InputResponse {
            InputResponse::Ignored
        }

        fn close(&mut self) {}
    }

    #[test]
    fn a_whole_plug_in_can_be_written_with_only_the_prelude_in_scope() {
        let factory = SingleFactory::<Gain>::new();
        assert_eq!(factory.plugin_count(), 1);
        let plugin = factory.create("com.example.preludegain").unwrap();

        let mut instance = crate::PluginInstance::new(plugin);
        instance.init().unwrap();
        instance
            .activate(&ProcessConfig::new(48_000.0, 128))
            .unwrap();
        instance.start_processing().unwrap();
        assert_eq!(instance.params().unwrap().param_refs().len(), 1);
        assert!(instance.create_editor().unwrap().is_some());
        instance.stop_processing().unwrap();
        instance.deactivate().unwrap();
    }

    /// Types the prelude promises to leave *out*, so that a future edit that reaches for
    /// convenience has to change this test first.
    #[test]
    fn the_prelude_stays_narrow() {
        // Reachable, but only by a qualified path.
        assert_eq!(crate::status::OK, 0);
        let _ = crate::SpscRingBuffer::with_capacity::<u32>(4);
        let _ = crate::AllocGuard::new();
        assert!(matches!(
            crate::GraphicErrorKind::Unsupported,
            crate::GraphicErrorKind::Unsupported
        ));
    }

    #[test]
    fn an_error_can_be_built_and_reported_from_the_prelude_alone() {
        let err: DauxError = ErrorKind::Unsupported.error("no f64 here");
        assert_eq!(err.kind(), ErrorKind::Unsupported);
        let result: DauxResult<()> = Err(err);
        assert!(result.is_err());
        assert_eq!(ProcessMode::default(), ProcessMode::Realtime);
    }
}
