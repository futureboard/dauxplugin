//! The facade's promise: a plug-in crate depends on `daux-plugin` and on nothing else.
//!
//! This file is deliberately an *integration* test rather than a unit test. A unit test
//! inside `daux-plugin` can see the crate's items directly, which is exactly the view a
//! plug-in author does not have; only a separate crate that lists `daux-plugin` as its one
//! dependency proves the re-export graph is complete. There are no dev-dependencies here on
//! purpose — if a name resolves below, it resolves for a plug-in too.

#![allow(clippy::float_cmp)]

// ------------------------------------------------------------------ the crate root ---

/// Every model crate must be reachable from the facade's root, because the glob is the only
/// thing standing between an author and ten entries in their `Cargo.toml`. A missing
/// re-export is invisible until someone downstream fails to compile, so it is named here.
#[test]
fn every_model_crate_arrives_through_the_facade() {
    let _: daux_plugin::ProcessStatus = daux_plugin::ProcessStatus::Continue; // daux-core
    let _: daux_plugin::SampleFormat = daux_plugin::SampleFormat::F32; // daux-audio
    let _: daux_plugin::EventFlags = daux_plugin::EventFlags::NONE; // daux-events
    let _: daux_plugin::Midi1Message = daux_plugin::Midi1Message::note_on(0, 60, 100); // daux-midi
    let _: daux_plugin::ParamFlags = daux_plugin::ParamFlags::AUTOMATABLE; // daux-parameter
    let _: daux_plugin::StateVersion = daux_plugin::StateVersion(1); // daux-state
    let _: daux_plugin::TimeSignature = daux_plugin::TimeSignature::COMMON; // daux-transport
    let _: daux_plugin::HostServices = daux_plugin::HostServices::null(); // daux-host-services
    let _: daux_plugin::PhysicalSize = daux_plugin::PhysicalSize::new(1, 1); // daux-graphics
    let _: daux_plugin::AtomicF32 = daux_plugin::AtomicF32::new(0.0); // daux-rt

    // …and the crates themselves, for the rare qualified path.
    assert_eq!(daux_plugin::daux_midi::status::NOTE_ON, 0x90);
}

/// The one genuine name collision among the re-exported crates. `daux-plugin-api` settles it
/// in favour of the ABI status codes with an explicit import; a glob-of-a-glob could quietly
/// lose that, and the failure would land in a plug-in rather than here.
#[test]
fn the_status_collision_still_resolves_to_the_abi_codes_through_two_globs() {
    assert_eq!(daux_plugin::status::OK, 0);
    assert_eq!(daux_plugin::status::INVALID_STATE, -5);
    // The MIDI one is not gone, only qualified.
    assert_eq!(daux_plugin::daux_midi::status::NOTE_OFF, 0x80);
}

/// The facade's own `prelude` and `__private` share a name with `daux-plugin-api`'s and must
/// shadow them: the API crate's `__private` carries only the parameter types, so a
/// `#[derive(DauxState)]` in a plug-in would fail to resolve `StateWriter` if the glob won.
#[test]
fn the_facades_own_modules_shadow_the_glob_re_export() {
    // Present only in this crate's `__private`, never in `daux-plugin-api`'s.
    let _: daux_plugin::__private::StateVersion = daux_plugin::__private::StateVersion(3);
    let _: daux_plugin::__private::Category = daux_plugin::__private::Category::Effect;
}

// --------------------------------------------------------------------- __private ---

/// The names generated code writes as `::daux_plugin::__private::*`.
///
/// This list is not a design decision made here; it is a transcription of what
/// `daux-plugin-macros` emits, and it exists so that renaming one of these types anywhere in
/// the workspace fails in this crate rather than inside a plug-in author's macro expansion,
/// where the error would point at their struct and not at the cause.
#[test]
fn the_macro_support_module_carries_every_name_the_derives_emit() {
    // What `#[derive(DauxParams)]` emits.
    use daux_plugin::__private::{
        BoolParam, EnumParam, FloatParam, IntParam, MeterParam, Param, ParamEnum, ParamFlags,
        ParamId, ParamMigration, ParamRange, Params, Smoothing,
    };
    // What `#[derive(DauxPlugin)]` emits.
    use daux_plugin::__private::{
        Capabilities, Category, PluginDescriptor, SampleFormats, Version,
    };
    // What `#[derive(DauxState)]` emits.
    use daux_plugin::__private::{StateError, StateReader, StateResult, StateVersion, StateWriter};

    // Each name is used, not merely imported: an import can be satisfied by a re-export that
    // points at the wrong thing, a value cannot.
    let gain = FloatParam::new(
        ParamId::new(1),
        "Gain",
        0.0,
        ParamRange::Linear {
            min: -60.0,
            max: 6.0,
        },
    )
    .with_smoothing(Smoothing::Linear { ms: 5.0 })
    .with_flags(ParamFlags::AUTOMATABLE);
    let as_param: &dyn Param = &gain;
    assert_eq!(as_param.info().id, ParamId::new(1));

    let _: IntParam = IntParam::new(ParamId::new(2), "Voices", 8, 1, 16);
    let _: BoolParam = BoolParam::new(ParamId::new(3), "Invert", false);
    let _: MeterParam = MeterParam::new(
        ParamId::new(4),
        "Level",
        ParamRange::Linear {
            min: -60.0,
            max: 6.0,
        },
    );
    let _: ParamMigration = ParamMigration::rename(ParamId::new(9), ParamId::new(1));

    // `EnumParam` and `ParamEnum` are named by a generated field type and its bound.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Shape {
        Sine,
        Square,
    }
    impl ParamEnum for Shape {
        const VARIANTS: &'static [Self] = &[Self::Sine, Self::Square];
        fn name(self) -> &'static str {
            match self {
                Self::Sine => "Sine",
                Self::Square => "Square",
            }
        }
        fn index(self) -> u32 {
            self as u32
        }
        fn from_index(i: u32) -> Option<Self> {
            Self::VARIANTS.get(i as usize).copied()
        }
    }
    let shape: EnumParam<Shape> = EnumParam::new(ParamId::new(5), "Shape", Shape::Sine);
    assert_eq!(shape.value(), Shape::Sine);

    // The `Params` trait itself is what the derive implements.
    struct Empty;
    impl Params for Empty {
        fn param_refs(&self) -> Vec<(ParamId, &dyn Param)> {
            Vec::new()
        }
    }
    assert!(Empty.param(ParamId::new(1)).is_none());

    // `#[derive(DauxPlugin)]`'s vocabulary.
    let descriptor: PluginDescriptor = PluginDescriptor::builder("com.example.facade", "Facade")
        .category(Category::Effect)
        .capabilities(Capabilities::AUDIO_EFFECT)
        .version(Version::new(1, 2, 3))
        .sample_formats(SampleFormats::BOTH)
        .build()
        .expect("a descriptor the builder accepts");
    assert_eq!(descriptor.version, Version::new(1, 2, 3));

    // `#[derive(DauxState)]`'s vocabulary.
    let mut writer = StateWriter::new(StateVersion(2));
    writer.put_f64("gain", -6.0);
    let bytes = writer.finish();
    let reader = StateReader::from_bytes(&bytes).expect("round trip");
    let value: StateResult<f64> = reader.f64("gain");
    assert_eq!(value.expect("present"), -6.0);
    let missing: StateError = reader.f64("absent").expect_err("no such key");
    assert!(!missing.to_string().is_empty());
}

// ----------------------------------------------------------------- feature matrix ---

/// `FORMATS` is what a build tool prints and what `export_plugin!` actually emits, so the two
/// must agree with the features rather than with each other's assumptions.
#[test]
fn the_format_list_matches_the_enabled_features() {
    let mut expected: Vec<&str> = Vec::new();
    if cfg!(feature = "axt") {
        expected.push("axt");
    }
    if cfg!(feature = "vst3") {
        expected.push("vst3");
    }
    if cfg!(feature = "clap") {
        expected.push("clap");
    }
    assert_eq!(daux_plugin::FORMATS, expected.as_slice());

    // The order is the export order, not alphabetical: the native format first.
    if daux_plugin::FORMATS.len() > 1 {
        assert_eq!(daux_plugin::FORMATS[0], "axt");
    }
}

/// The adapters must be reachable by a stable path, because `daux build` prints their
/// compatibility reports and a plug-in cannot name the adapter crates itself.
#[cfg(feature = "axt")]
#[test]
fn the_axt_adapter_is_reachable_and_reports_compatibility() {
    use daux_plugin::prelude::*;

    let effect = PluginDescriptor::builder("com.example.gain", "Gain")
        .category(Category::Effect)
        .build()
        .expect("valid");
    // AXT is the native format: it can express everything the model can.
    assert!(daux_plugin::formats::axt::compatibility_report(&effect).is_empty());
    assert_eq!(
        daux_plugin::formats::axt::entry_symbol(),
        "daux_plugin_entry_v1"
    );
}

#[cfg(feature = "vst3")]
#[test]
fn the_vst3_adapter_is_reachable() {
    use daux_plugin::prelude::*;

    let descriptor = PluginDescriptor::builder("com.example.gain", "Gain")
        .category(Category::Effect)
        .build()
        .expect("valid");
    // The call is what matters: the report may legitimately be empty for a plain effect.
    let _ = daux_plugin::formats::vst3::compatibility_report(&descriptor);
}

#[cfg(feature = "clap")]
#[test]
fn the_clap_adapter_is_reachable() {
    use daux_plugin::prelude::*;

    let descriptor = PluginDescriptor::builder("com.example.gain", "Gain")
        .category(Category::Effect)
        .build()
        .expect("valid");
    let _ = daux_plugin::formats::clap::compatibility_report(&descriptor);
    assert_eq!(
        daux_plugin::formats::clap::abi::CLAP_PLUGIN_FACTORY_ID.to_bytes(),
        b"clap.plugin-factory"
    );
}

#[cfg(feature = "gui")]
#[test]
fn the_graphics_module_carries_the_editor_abstraction() {
    // The same items as the crate root, so `use daux_plugin::graphics::*;` in an editor module
    // brings the traits and the backend into scope together.
    let _: daux_plugin::graphics::LogicalSize = daux_plugin::graphics::LogicalSize::new(4.0, 2.0);
    let _: daux_plugin::graphics::PhysicalSize = daux_plugin::graphics::PhysicalSize::new(4, 2);
    let _: daux_plugin::graphics::InputResponse = daux_plugin::graphics::InputResponse::Ignored;
}

#[cfg(feature = "dsp")]
#[test]
fn the_dsp_module_is_the_real_toolbox() {
    assert!((daux_plugin::dsp::db_to_gain(0.0) - 1.0).abs() < 1e-6);
    assert!((daux_plugin::dsp::db_to_gain(-6.0) - 0.501_187_2).abs() < 1e-6);

    let mut block = [1.0_f32; 16];
    daux_plugin::dsp::simd::apply_gain(&mut block, 0.5);
    assert!(block.iter().all(|s| (*s - 0.5).abs() < 1e-7));
    assert!(!daux_plugin::dsp::simd::dispatch_name().is_empty());
}

// ------------------------------------------------- a plug-in, with only the prelude ---

mod prelude_only {
    //! A whole plug-in written with `use daux_plugin::prelude::*;` and nothing else in scope.
    //!
    //! If any item the four traits mention is missing from the facade's prelude, this module
    //! does not compile — which is the only test of a prelude that means anything.

    use daux_plugin::prelude::*;

    pub(super) struct Gain {
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
            PluginDescriptor::builder("com.example.facadegain", "Facade Gain")
                .vendor("DAUxPlug tests")
                .category(Category::Effect)
                .capabilities(Capabilities::AUDIO_EFFECT | Capabilities::HAS_GUI)
                .version(Version::new(1, 0, 0))
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
}

/// The plug-in above, driven through the lifecycle an adapter drives it through. It proves the
/// facade re-exports a working object model, not merely a set of names that type-check.
#[test]
fn a_prelude_only_plug_in_runs_the_whole_lifecycle() {
    use daux_plugin::prelude::*;

    let factory = SingleFactory::<prelude_only::Gain>::new();
    assert_eq!(factory.plugin_count(), 1);

    let plugin = factory
        .create("com.example.facadegain")
        .expect("the id the descriptor declares");
    let mut instance = daux_plugin::PluginInstance::new(plugin);
    instance.init().expect("init");
    instance
        .activate(&ProcessConfig::new(48_000.0, 128))
        .expect("activate");
    instance.start_processing().expect("start");
    assert_eq!(instance.params().expect("params").param_refs().len(), 1);
    assert!(
        instance
            .create_editor()
            .expect("editor creation is allowed here")
            .is_some(),
        "the editor is re-typed by the facade, not returned as an opaque Any"
    );
    instance.stop_processing().expect("stop");
    instance.deactivate().expect("deactivate");

    // An unknown id is refused rather than substituted.
    assert!(factory.create("com.example.not-here").is_err());
}
