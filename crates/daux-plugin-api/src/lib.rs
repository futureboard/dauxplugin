//! The safe, high-level DAUx plug-in authoring API.
//!
//! This crate defines almost nothing. That is the point: the object model lives in
//! [`daux_core`], buffers in [`daux_audio`], parameters in [`daux_parameter`], and so on.
//! `daux-plugin-api` gathers those crates into one namespace and adds the three pieces of
//! glue that would otherwise be copied into every format adapter and every plug-in:
//!
//! | Item | What it solves |
//! |---|---|
//! | [`PluginInstance`] | Drives a `Box<dyn DauxPlugin>` through the abi-v1 §7 lifecycle and refuses every transition the state machine does not allow. |
//! | [`SingleFactory`] / [`PluginRegistry`] | Turn one or many `DauxPlugin` types into a [`DauxFactory`] without hand-writing the enumeration. |
//! | [`take_editor`] / [`editor`] | Re-type `DauxPlugin::create_editor`'s opaque `Box<dyn Any>` into a `Box<dyn DauxGraphic>`. |
//!
//! There is deliberately **no second set of traits** here. If you find yourself wanting one,
//! the change belongs in `daux-core`.
//!
//! # Getting things into scope
//!
//! ```
//! use daux_plugin_api::prelude::*;
//! ```
//!
//! [`prelude`] carries the traits a plug-in implements and the types their signatures
//! mention — nothing else. The crate root additionally re-exports the full public surface of
//! every model crate, so `daux_plugin_api::AudioStorage` and `daux_plugin_api::SpscRingBuffer`
//! resolve without naming `daux-audio` or `daux-rt` in your `Cargo.toml`.
//!
//! # Writing a plug-in
//!
//! Four impls and a factory. Nothing here is DAUx-, VST3- or CLAP-specific: the same object
//! is exported to all three.
//!
//! ```
//! use daux_plugin_api::prelude::*;
//!
//! #[derive(Default)]
//! struct Bypass;
//!
//! impl DauxProcessor for Bypass {
//!     fn prepare(&mut self, config: &ProcessConfig) -> DauxResult<()> {
//!         config.validate()
//!     }
//!     fn process<'a>(
//!         &mut self,
//!         _ctx: &ProcessContext<'a>,
//!         audio: &mut AudioBuses<'a, f32>,
//!         _events: &mut ProcessEvents<'a>,
//!     ) -> ProcessStatus {
//!         audio.silence_outputs();
//!         ProcessStatus::ContinueIfNotQuiet
//!     }
//! }
//!
//! impl Params for Bypass {
//!     fn param_refs(&self) -> Vec<(ParamId, &dyn Param)> {
//!         Vec::new()
//!     }
//! }
//!
//! impl DauxController for Bypass {
//!     fn params(&self) -> &dyn Params {
//!         self
//!     }
//!     fn save_state(&self, _w: &mut StateWriter) -> DauxResult<()> {
//!         Ok(())
//!     }
//!     fn load_state(&mut self, _r: &StateReader) -> DauxResult<()> {
//!         Ok(())
//!     }
//! }
//!
//! impl DauxPlugin for Bypass {
//!     fn descriptor() -> PluginDescriptor {
//!         PluginDescriptor::builder("com.example.bypass", "Bypass").build().unwrap()
//!     }
//!     fn bus_layout(&self) -> BusLayout {
//!         BusLayout::stereo_effect()
//!     }
//!     fn processor(&mut self) -> &mut dyn DauxProcessor {
//!         self
//!     }
//!     fn controller(&mut self) -> &mut dyn DauxController {
//!         self
//!     }
//! }
//!
//! // One plug-in per module: the usual case.
//! let single = SingleFactory::<Bypass>::new();
//! assert_eq!(single.plugin_count(), 1);
//! assert!(single.create("com.example.bypass").is_ok());
//!
//! // Several plug-ins in one module. Two with the same permanent id is rejected, never
//! // silently shadowed.
//! let mut registry = PluginRegistry::new();
//! registry.register::<Bypass>();
//! assert_eq!(registry.plugin_count(), 1);
//! assert!(registry.try_register::<Bypass>().is_err());
//! ```
//!
//! # Name collisions between the re-exported crates
//!
//! Two crates export a module called `status`, so a bare glob would make
//! `daux_plugin_api::status` ambiguous. The collision is resolved deliberately rather than
//! left to the compiler:
//!
//! | Name | Resolves to | The other one |
//! |---|---|---|
//! | `status` | [`daux_core::status`] — the ABI status codes (`OK`, `INVALID_STATE`, …) | [`daux_midi::status`], the MIDI 1.0 status bytes, still reachable by that path |
//!
//! Everything else that appears twice is genuinely the *same* item reached by two paths —
//! `ParamId` (via `daux-parameter` and `daux-host-services`), `LogLevel`, `RtLogRecord`,
//! `RT_LOG_MESSAGE_BYTES` (via `daux-rt` and `daux-host-services`) — which Rust resolves
//! without ambiguity. A handful of names are merely *similar* and are not collisions:
//! [`InputEvent`] is a UI event from `daux-graphics`, [`InputEvents`] is the audio-thread
//! event list from `daux-events`; [`Version`] is a plug-in's product version,
//! [`StateVersion`] its state schema's.
//!
//! Ambiguity from a glob re-export is only reported at the *use* site, so the check that
//! matters is one made from outside this crate:
//!
//! ```
//! use daux_plugin_api::*;
//!
//! // `status` is the ABI one, unambiguously.
//! assert_eq!(status::INVALID_STATE, -5);
//! // Names that arrive from more than one crate resolve to one item.
//! let _: ParamId = ParamId::new(1);
//! let _: LogLevel = LogLevel::Warn;
//! // …and every crate's own types are here under their own names.
//! let _: ProcessStatus = ProcessStatus::Sleep;
//! let _: SampleFormat = SampleFormat::F64;
//! let _: PhysicalSize = PhysicalSize::new(64, 64);
//! let _: StateVersion = StateVersion(2);
//! let _: Transport = Transport::EMPTY;
//! let _: Midi1Message = Midi1Message::note_on(0, 60, 100);
//! ```
//!
//! # Threading
//!
//! Every item in this crate is `[main-thread]` except [`PluginInstance::process`],
//! [`PluginInstance::process_f64`], [`PluginInstance::start_processing`],
//! [`PluginInstance::stop_processing`] and [`PluginInstance::reset`], which are
//! `[audio-thread]` and allocate nothing — including when they refuse a call.

mod editor;
mod factory;
mod instance;

pub mod prelude;

#[cfg(test)]
mod testkit;

pub use editor::{boxed_editor, downcast_editor, editor, take_editor};
pub use factory::{PluginRegistry, SingleFactory};
pub use instance::{InstanceState, PluginInstance};

/// The model crates themselves, so a plug-in can name one explicitly when a glob would be
/// ambiguous or unclear — `daux_plugin_api::daux_midi::status::NOTE_ON`, for instance.
pub use {
    daux_audio, daux_core, daux_events, daux_graphics, daux_host_services, daux_midi,
    daux_parameter, daux_rt, daux_state, daux_transport,
};

pub use daux_audio::*;
pub use daux_core::*;
pub use daux_events::*;
pub use daux_graphics::*;
pub use daux_host_services::*;
pub use daux_midi::*;
pub use daux_parameter::*;
pub use daux_rt::*;
pub use daux_state::*;
pub use daux_transport::*;

// An explicit import outranks a glob, which is exactly how the `status` collision documented
// above is settled: the ABI status codes win the short name because they are what a plug-in
// returns across a boundary, while `daux_midi::status` is only ever read next to other MIDI
// bytes and reads better qualified.
pub use daux_core::status;

/// Implementation detail of the derives. Not a public API and not covered by semantic
/// versioning: it exists so generated code can name types through a single path.
///
/// This is the **redirect target**. Generated code normally writes
/// `::daux_plugin::__private::…`, which is why a plug-in crate can depend on the facade
/// alone — but a crate inside this workspace cannot depend on `daux-plugin` without a cycle,
/// so `#[params(crate = ::daux_plugin_api)]`, `#[plugin(crate = ..)]` and
/// `#[state(crate = ..)]` point the same expansion here instead.
///
/// It therefore has to carry the **whole** set, not just the parameter half:
/// `#[derive(DauxState)]` names five `daux-state` types and `#[derive(DauxPlugin)]` names
/// four `daux-core` types plus `SampleFormats`. Dropping any one of them turns the documented
/// escape hatch into a resolution error at the call site, in generated code the author never
/// wrote. The unit test at the bottom of this file is what keeps the two `__private` modules
/// in step.
#[doc(hidden)]
pub mod __private {
    // `#[derive(DauxParams)]`.
    pub use daux_parameter::{
        BoolParam, EnumParam, FloatParam, IntParam, MeterParam, Param, ParamEnum, ParamFlags,
        ParamId, ParamMigration, ParamRange, Params, Smoothing,
    };

    // `#[derive(DauxPlugin)]`.
    pub use daux_audio::SampleFormats;
    pub use daux_core::{Capabilities, Category, PluginDescriptor, Version};

    // `#[derive(DauxState)]`.
    pub use daux_state::{StateError, StateReader, StateResult, StateVersion, StateWriter};
}

/// The allocation tripwire, installed only while compiling this crate's tests, so that
/// "refusing an out-of-order call allocates nothing" is a checked assertion rather than a
/// comment. Production builds are untouched.
#[cfg(test)]
#[global_allocator]
static TEST_ALLOCATOR: daux_rt::CountingAllocator = daux_rt::CountingAllocator;

#[cfg(test)]
mod tests {
    use super::*;

    /// The documented winner of the one real name collision, and proof the loser is still
    /// reachable. If a future crate adds another `status`, this test keeps compiling but the
    /// glob would start failing at every use site — so the explicit import must stay.
    #[test]
    fn the_status_collision_resolves_to_the_abi_status_codes() {
        assert_eq!(status::INVALID_STATE, -5);
        assert_eq!(status::OK, 0);
        // The MIDI one is not gone, only qualified.
        assert_eq!(daux_midi::status::NOTE_ON, 0x90);
        assert_eq!(daux_midi::status::NOTE_OFF, 0x80);
    }

    /// Names that appear through more than one path must be the *same* type, not two types
    /// that happen to share a name — otherwise a plug-in and a host would silently disagree.
    #[test]
    fn duplicated_re_exports_are_the_same_type() {
        const fn same<T>(_: &T, _: &T) {}
        // `ParamId` arrives via daux-parameter and via daux-host-services.
        same(
            &daux_parameter::ParamId::new(1),
            &daux_host_services::ParamId::new(1),
        );
        // `LogLevel` arrives via daux-rt and via daux-host-services.
        same(
            &daux_rt::LogLevel::Info,
            &daux_host_services::LogLevel::Info,
        );
        // And the root re-export is that same type again.
        let id: ParamId = daux_parameter::ParamId::new(7);
        assert_eq!(id.get(), 7);
    }

    /// A spot check that each of the ten crates really is reachable from the root, since a
    /// missing `pub use` here is invisible until a downstream crate fails to compile.
    #[test]
    fn every_model_crate_is_reachable_from_the_root() {
        let _: ProcessStatus = ProcessStatus::Continue; // daux-core
        let _: SampleFormat = SampleFormat::F32; // daux-audio
        let _: EventFlags = EventFlags::NONE; // daux-events
        let _: Midi1Kind = Midi1Message::note_on(0, 60, 100).kind(); // daux-midi
        let _: ParamFlags = ParamFlags::AUTOMATABLE; // daux-parameter
        let _: StateVersion = StateVersion(1); // daux-state
        let _: TimeSignature = TimeSignature::COMMON; // daux-transport
        let _: HostServices = HostServices::null(); // daux-host-services
        let _: PhysicalSize = PhysicalSize::new(1, 1); // daux-graphics
        let _: AtomicF32 = AtomicF32::new(0.0); // daux-rt
    }

    /// `#[derive(DauxParams)]` may be pointed at this crate instead of `daux-plugin`, so the
    /// path its output names has to exist here too.
    #[test]
    fn the_macro_support_module_exposes_what_generated_code_names() {
        let p = __private::FloatParam::new(
            __private::ParamId::new(1),
            "Gain",
            0.0,
            __private::ParamRange::Linear {
                min: -60.0,
                max: 6.0,
            },
        );
        let as_param: &dyn __private::Param = &p;
        assert_eq!(as_param.info().id, ParamId::new(1));
    }

    /// Names every one of the 22 items generated code can emit, so that a re-export removed
    /// here fails *here* rather than inside somebody's `#[derive(..)]` expansion.
    ///
    /// `#[params(crate = ::daux_plugin_api)]` and its `plugin`/`state` siblings redirect the
    /// derives at this module, so the set has to match `daux_plugin::__private` exactly. It
    /// once carried only the 13 parameter names, which meant redirecting `DauxState` or
    /// `DauxPlugin` here failed to resolve ten types — with no error anywhere until an author
    /// tried the documented escape hatch.
    #[test]
    fn the_redirect_target_carries_every_name_the_derives_emit() {
        // `#[derive(DauxParams)]` — 13. `EnumParam` and `ParamEnum` are exercised with a
        // real value rather than a type alias, because a generic type is only proved
        // nameable once its bound is actually satisfied.
        let waveform = __private::EnumParam::new(
            __private::ParamId::new(2),
            "Shape",
            __private_test_enum::Waveform::Saw,
        );
        assert_eq!(waveform.value(), __private_test_enum::Waveform::Saw);

        type _P1 = __private::BoolParam;
        type _P3 = __private::FloatParam;
        type _P4 = __private::IntParam;
        type _P5 = __private::MeterParam;
        type _P6 = dyn __private::Param;
        type _P7 = __private::ParamFlags;
        type _P8 = __private::ParamId;
        type _P9 = __private::ParamMigration;
        type _P10 = __private::ParamRange;
        type _P11 = dyn __private::Params;
        type _P12 = __private::Smoothing;
        fn _p13<E: __private::ParamEnum>() {}

        // `#[derive(DauxPlugin)]` — 5.
        type _D1 = __private::SampleFormats;
        type _D2 = __private::Capabilities;
        type _D3 = __private::Category;
        type _D4 = __private::PluginDescriptor;
        type _D5 = __private::Version;

        // `#[derive(DauxState)]` — 5. `StateResult` is a generic alias, so it is named
        // through a use rather than a `type` binding.
        type _S1 = __private::StateError;
        type _S2 = __private::StateReader;
        type _S3 = __private::StateVersion;
        type _S4 = __private::StateWriter;
        fn _s5(r: __private::StateResult<u8>) -> Option<u8> {
            r.ok()
        }

        // And they are the *same* types, not a second definition that happens to compile.
        let _: __private::Category = Category::Effect;
        let _: __private::StateVersion = StateVersion(1);
        let _: __private::SampleFormats = SampleFormats::F32;
    }
}

/// A parameter enum used only by the re-export test above; `EnumParam<E>` needs a concrete
/// `E` to be nameable as a type.
#[cfg(test)]
mod __private_test_enum {
    /// The two shapes.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum Waveform {
        /// A sine.
        Sine,
        /// A saw.
        Saw,
    }

    impl daux_parameter::ParamEnum for Waveform {
        const VARIANTS: &'static [Self] = &[Self::Sine, Self::Saw];
        fn name(self) -> &'static str {
            match self {
                Self::Sine => "Sine",
                Self::Saw => "Saw",
            }
        }
        fn index(self) -> u32 {
            self as u32
        }
        fn from_index(index: u32) -> Option<Self> {
            Self::VARIANTS.get(index as usize).copied()
        }
    }
}
