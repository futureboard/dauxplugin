//! DAUxPlug — write an audio plug-in once in Rust, export it as .axt, VST3 and CLAP.
//!
//! This is the only crate a plug-in author depends on. Everything below it — the object
//! model, the buffers, the parameters, the state container, the derives, the format
//! adapters, the editor backends — arrives through this one name, at one version, behind
//! feature flags rather than a dependency list:
//!
//! ```toml
//! [dependencies]
//! daux-plugin = { version = "0.1", features = ["axt", "vst3", "clap"] }
//!
//! [lib]
//! crate-type = ["cdylib", "rlib"]
//! ```
//!
//! There is nothing else to add. A plug-in crate that names `daux-core`, `daux-parameter`
//! or `daux-format-vst3` in its own manifest has reached past the facade, and the two
//! versions it now pins can drift apart.
//!
//! # What is here
//!
//! | Path | Contents |
//! |---|---|
//! | the crate root | the whole of [`daux_plugin_api`], which is the whole of the model crates |
//! | [`prelude`] | the curated set an author puts in scope, plus the derives and [`export_plugin!`] |
//! | [`formats`] | the enabled format adapters, for the rare code that needs one by name |
//! | `graphics` | the editor abstraction and the enabled GUI backends |
//! | `dsp` | the small DSP toolbox, with runtime SIMD dispatch |
//! | [`export_plugin!`] | one line that emits the entry point of **every** enabled format |
//!
//! # A whole plug-in
//!
//! Four impls, a factory and one export. Nothing in it is DAUx-, VST3- or CLAP-specific:
//! the same object is what every enabled adapter presents to its host.
//!
//! ```
//! use daux_plugin::prelude::*;
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
//!     fn params(&self) -> &dyn Params { self }
//!     fn save_state(&self, _w: &mut StateWriter) -> DauxResult<()> { Ok(()) }
//!     fn load_state(&mut self, _r: &StateReader) -> DauxResult<()> { Ok(()) }
//! }
//!
//! impl DauxPlugin for Bypass {
//!     fn descriptor() -> PluginDescriptor {
//!         PluginDescriptor::builder("com.example.bypass", "Bypass").build().unwrap()
//!     }
//!     fn bus_layout(&self) -> BusLayout { BusLayout::stereo_effect() }
//!     fn processor(&mut self) -> &mut dyn DauxProcessor { self }
//!     fn controller(&mut self) -> &mut dyn DauxController { self }
//! }
//!
//! // The factory every adapter is handed. `export_plugin!(SingleFactory<Bypass>)` turns it
//! // into the exported symbols of this binary.
//! let factory = SingleFactory::<Bypass>::new();
//! assert_eq!(factory.plugin_count(), 1);
//! ```
//!
//! # Features
//!
//! | Feature | Default | Effect |
//! |---|:-:|---|
//! | `derive` | yes | `#[derive(DauxParams)]`, `#[derive(DauxPlugin)]`, `#[derive(DauxState)]` |
//! | `axt` | yes | [`export_plugin!`] emits `daux_plugin_entry_v1` |
//! | `vst3` | no | [`export_plugin!`] emits `GetPluginFactory` and the module hooks |
//! | `clap` | no | [`export_plugin!`] emits `clap_entry` |
//! | `gui` | yes | the `graphics` module |
//! | `dsp` | yes | the `dsp` module |
//! | `egui`, `gpui`, `wgpu`, `opengl` | no | one editor backend each, under `graphics`; each implies `gui` |
//! | `simd` | no | hand-vectorised paths in `dsp` |
//! | `serde` | no | `serde` impls on the state and metadata types |
//!
//! Turning `gui` or `dsp` off removes a *module*, never a capability of the model: the
//! editor traits of `daux-graphics` are part of the object model and are always at the
//! crate root. The GPU and UI stacks are what the flags actually keep out of a build.
//!
//! # Threading
//!
//! Every re-exported item keeps the annotation its own crate gave it — `[audio-thread]`,
//! `[main-thread]` or `[any-thread]`, matching `docs/specifications/abi-v1.md` §15. Nothing
//! is relaxed by passing through here. [`export_plugin!`] itself is compile-time only, and
//! the entry points it emits are `[main-thread]`.

// The facade is the single name a plug-in depends on, so it re-exports the model wholesale.
// The two modules below that share a name with one of `daux-plugin-api`'s — `prelude` and
// `__private` — deliberately shadow the glob: this crate's prelude adds the derives and the
// export macro, and its `__private` adds everything `#[derive(DauxState)]` and
// `#[derive(DauxPlugin)]` name, which the API crate's parameter-only version does not carry.
pub use daux_plugin_api::*;

/// The derives, re-exported at the root so `#[derive(daux_plugin::DauxParams)]` works without
/// importing the prelude.
///
/// The macro `DauxPlugin` and the trait [`DauxPlugin`] share a name on purpose and never
/// collide: a derive lives in the macro namespace and a trait in the type namespace, so
/// `#[derive(DauxPlugin)]` and `impl DauxPlugin for _` both resolve with both in scope.
///
/// `[main-thread]`: everything these generate allocates, except the `Params::param` lookup,
/// which is a `match` on the raw id and is therefore `[any-thread]`. The prelude carries them
/// too, with a worked example.
#[cfg(feature = "derive")]
pub use daux_plugin_macros::{DauxParams, DauxPlugin, DauxState};

pub mod prelude {
    //! What a plug-in author puts in scope.
    //!
    //! ```
    //! use daux_plugin::prelude::*;
    //! ```
    //!
    //! This is [`daux_plugin_api::prelude`] — the traits a plug-in implements and the types
    //! their signatures mention — plus the two things that only exist at the facade: the
    //! derives, and [`export_plugin!`](crate::export_plugin) itself.
    //!
    //! Everything the prelude leaves out is one qualified path away at the
    //! [crate root](crate): the ABI status codes, the lock-free queues, the bundle world,
    //! `daux_plugin::dsp`, `daux_plugin::graphics`.

    pub use daux_plugin_api::prelude::*;

    /// Exported here so a plug-in can write `export_plugin!(MyFactory);` with only the
    /// prelude imported.
    pub use crate::export_plugin;

    /// The derives, so that importing the prelude is enough to write `#[derive(DauxParams)]`.
    ///
    /// The derive `DauxPlugin` and the trait [`DauxPlugin`](daux_plugin_api::DauxPlugin) share
    /// a name and never collide: a derive lives in the macro namespace and a trait in the type
    /// namespace, so `#[derive(DauxPlugin)]` and `impl DauxPlugin for _` both resolve with the
    /// whole prelude in scope.
    ///
    /// ```
    /// use daux_plugin::prelude::*;
    ///
    /// #[derive(DauxParams)]
    /// struct GainParams {
    ///     #[param(id = 1, name = "Gain", range = -60.0..=12.0, default = 0.0, unit = "dB")]
    ///     gain: FloatParam,
    /// }
    ///
    /// let params = GainParams::new();
    /// assert_eq!(params.param_refs().len(), 1);
    /// // The lookup is by permanent id, and it allocates nothing.
    /// assert!(params.param(ParamId::new(1)).is_some());
    /// assert!(params.param(ParamId::new(2)).is_none());
    /// ```
    #[cfg(feature = "derive")]
    pub use daux_plugin_macros::*;
}

pub mod formats {
    //! The format adapters enabled by this build's features.
    //!
    //! A plug-in normally never names one: [`export_plugin!`](crate::export_plugin) emits
    //! every enabled entry point, and the adapters translate the same object in every
    //! direction. Reach in here for the two things that are genuinely format-specific — the
    //! compatibility report a build tool prints, and the ABI types a test drives directly.
    //!
    //! Each adapter reports incompatibilities in its own vocabulary — VST3's warnings are not
    //! CLAP's — so the three `CompatibilityWarning` types are deliberately distinct and are
    //! reached through their own module rather than flattened into one name here.

    /// The native format: `daux_plugin_entry_v1` over the DAUx C ABI.
    ///
    /// ```
    /// use daux_plugin::prelude::*;
    ///
    /// let descriptor = PluginDescriptor::builder("com.example.gain", "Gain")
    ///     .category(Category::Effect)
    ///     .build()
    ///     .unwrap();
    /// // AXT is the native format, so there is nothing it cannot express.
    /// assert!(daux_plugin::formats::axt::compatibility_report(&descriptor).is_empty());
    /// ```
    #[cfg(feature = "axt")]
    pub use daux_format_axt as axt;
    #[cfg(feature = "clap")]
    pub use daux_format_clap as clap;
    #[cfg(feature = "vst3")]
    pub use daux_format_vst3 as vst3;
}

/// `[any-thread]` The formats [`export_plugin!`] emits an entry point for in this build.
///
/// Always in the order they are exported — `"axt"`, `"vst3"`, `"clap"` — and empty when no
/// format feature is enabled, which is the one case in which [`export_plugin!`] refuses to
/// compile. A build tool prints this; a test asserts on it.
///
/// ```
/// for format in daux_plugin::FORMATS {
///     assert!(matches!(*format, "axt" | "vst3" | "clap"));
/// }
/// ```
pub const FORMATS: &[&str] = &[
    #[cfg(feature = "axt")]
    "axt",
    #[cfg(feature = "vst3")]
    "vst3",
    #[cfg(feature = "clap")]
    "clap",
];

#[cfg(feature = "gui")]
pub mod graphics {
    //! Editors: the framework-neutral abstraction, and the backends this build enables.
    //!
    //! The abstraction itself — [`DauxGraphic`], [`GraphicContext`], [`InputEvent`] — is part
    //! of the object model and is also at the [crate root](crate); it is repeated here so
    //! that an editor module can `use daux_plugin::graphics::*;` and have both the traits and
    //! its backend in scope.
    //!
    //! # Backends
    //!
    //! | Feature | Path | Backend |
    //! |---|---|---|
    //! | `egui` | `graphics::egui` | immediate-mode egui |
    //! | `gpui` | `graphics::gpui` | GPUI, over the `gpui_embedded` platform |
    //! | `wgpu` | `graphics::wgpu` | wgpu rendering surface |
    //! | `opengl` | `graphics::opengl` | OpenGL fallback surface |
    //!
    //! Each is an optional dependency, so a headless plug-in never compiles a GPU stack and
    //! `cargo build` for the workspace stays fast.
    //!
    //! # Lifetime
    //!
    //! An editor is created and destroyed many times while the processor keeps running, and
    //! closing one must never touch DSP state (`CLAUDE.md` rule 9). [`DauxGraphic`] is
    //! neither `Send` nor `Sync` for the same reason the toolkits are not: it lives on the
    //! host's main thread, and the audio thread cannot reach it.

    pub use daux_plugin_api::daux_graphics::*;

    #[cfg(feature = "egui")]
    pub use daux_graphics_egui as egui;
    #[cfg(feature = "opengl")]
    pub use daux_graphics_gl as opengl;
    #[cfg(feature = "gpui")]
    pub use daux_graphics_gpui as gpui;
    #[cfg(feature = "wgpu")]
    pub use daux_graphics_wgpu as wgpu;
}

/// The DSP toolbox: gain conversions, biquads, a delay line, meter ballistics and the
/// runtime-dispatched vector helpers.
///
/// Everything in `dsp::simd` picks its widest available path once and caches the choice, with
/// a scalar fallback that never faults on a CPU without the feature. The `simd` feature of
/// this crate turns on the hand-vectorised implementations; with it off the scalar paths are
/// used and the results are unchanged.
///
/// ```
/// use daux_plugin::dsp;
///
/// let mut block = [1.0_f32; 64];
/// dsp::simd::apply_gain(&mut block, dsp::db_to_gain(-6.0));
/// assert!((block[0] - 0.501_187_2).abs() < 1e-6);
/// ```
#[cfg(feature = "dsp")]
pub use daux_dsp as dsp;

/// Emits the entry point of every format this build enables, for one factory type.
///
/// One line per plug-in binary. With `features = ["axt", "vst3", "clap"]` it exports
/// `daux_plugin_entry_v1`, `GetPluginFactory` plus the platform's module hooks, and
/// `clap_entry` — three formats, one object, no per-format code in the plug-in.
///
/// `$factory` must implement [`DauxFactory`](daux_plugin_api::DauxFactory) and [`Default`]:
/// every format's entry point constructs the factory with no arguments, because that is all
/// a module export can do. [`SingleFactory<P>`](daux_plugin_api::SingleFactory) qualifies for
/// one plug-in, and a newtype around [`PluginRegistry`](daux_plugin_api::PluginRegistry)
/// qualifies for several.
///
#[cfg_attr(any(feature = "axt", feature = "vst3", feature = "clap"), doc = "```")]
#[cfg_attr(
    not(any(feature = "axt", feature = "vst3", feature = "clap")),
    doc = "```ignore"
)]
/// use daux_plugin::prelude::*;
///
/// # #[derive(Default)]
/// # struct Bypass;
/// # impl DauxProcessor for Bypass {
/// #     fn prepare(&mut self, config: &ProcessConfig) -> DauxResult<()> { config.validate() }
/// #     fn process<'a>(&mut self, _c: &ProcessContext<'a>, audio: &mut AudioBuses<'a, f32>,
/// #                    _e: &mut ProcessEvents<'a>) -> ProcessStatus {
/// #         audio.silence_outputs();
/// #         ProcessStatus::ContinueIfNotQuiet
/// #     }
/// # }
/// # impl Params for Bypass { fn param_refs(&self) -> Vec<(ParamId, &dyn Param)> { Vec::new() } }
/// # impl DauxController for Bypass {
/// #     fn params(&self) -> &dyn Params { self }
/// #     fn save_state(&self, _w: &mut StateWriter) -> DauxResult<()> { Ok(()) }
/// #     fn load_state(&mut self, _r: &StateReader) -> DauxResult<()> { Ok(()) }
/// # }
/// # impl DauxPlugin for Bypass {
/// #     fn descriptor() -> PluginDescriptor {
/// #         PluginDescriptor::builder("com.example.bypass", "Bypass").build().unwrap()
/// #     }
/// #     fn bus_layout(&self) -> BusLayout { BusLayout::stereo_effect() }
/// #     fn processor(&mut self) -> &mut dyn DauxProcessor { self }
/// #     fn controller(&mut self) -> &mut dyn DauxController { self }
/// # }
/// // …the four impls of `Bypass`, then:
///
/// export_plugin!(SingleFactory<Bypass>);
/// # fn main() {}
/// ```
///
/// # Invoke it once
///
/// A dynamic library has one `daux_plugin_entry_v1` and one `clap_entry`, so a second
/// expansion in the same binary is a duplicate-symbol error. That is the correct outcome: to
/// ship several plug-ins in one binary, register them all in one
/// [`PluginRegistry`](daux_plugin_api::PluginRegistry) and export that.
///
/// # Errors it produces at compile time
///
/// * A factory that is not [`DauxFactory`](daux_plugin_api::DauxFactory) + [`Default`] fails
///   on a bound named in this macro's own expansion, rather than somewhere inside an adapter.
/// * No format feature enabled is a [`compile_error!`] rather than a silently empty binary —
///   a plug-in that exports nothing is never what the author meant.
///
/// `[main-thread]` for everything the emitted entry points do.
#[macro_export]
macro_rules! export_plugin {
    ($factory:ty $(,)?) => {
        // Checked before any adapter sees the type, so a missing `Default` is reported here
        // and not as a trait-resolution failure inside a format crate the author never named.
        const _: () = {
            fn assert_exportable<F: $crate::DauxFactory + ::core::default::Default>() {}
            let _ = assert_exportable::<$factory>;
        };

        $crate::__daux_export_none!($factory);
        $crate::__daux_export_axt!($factory);
        $crate::__daux_export_vst3!($factory);
        $crate::__daux_export_clap!($factory);
    };
}

// Each format is delegated to its own macro so that the `#[cfg]` is evaluated while *this*
// crate is compiled, against *this* crate's features. A `#[cfg(feature = "axt")]` written
// inside the expansion of `export_plugin!` would instead be evaluated against the features of
// the plug-in crate that invoked it, where `axt` means nothing.

/// `export_plugin!`'s AXT half, present because the `axt` feature is on.
#[cfg(feature = "axt")]
#[doc(hidden)]
#[macro_export]
macro_rules! __daux_export_axt {
    ($factory:ty) => {
        $crate::__private::export_entry_axt!($factory);
    };
}

/// `export_plugin!`'s AXT half, absent because the `axt` feature is off.
#[cfg(not(feature = "axt"))]
#[doc(hidden)]
#[macro_export]
macro_rules! __daux_export_axt {
    ($factory:ty) => {};
}

/// `export_plugin!`'s VST3 half, present because the `vst3` feature is on.
#[cfg(feature = "vst3")]
#[doc(hidden)]
#[macro_export]
macro_rules! __daux_export_vst3 {
    ($factory:ty) => {
        $crate::__private::export_entry_vst3!($factory);
    };
}

/// `export_plugin!`'s VST3 half, absent because the `vst3` feature is off.
#[cfg(not(feature = "vst3"))]
#[doc(hidden)]
#[macro_export]
macro_rules! __daux_export_vst3 {
    ($factory:ty) => {};
}

/// `export_plugin!`'s CLAP half, present because the `clap` feature is on.
#[cfg(feature = "clap")]
#[doc(hidden)]
#[macro_export]
macro_rules! __daux_export_clap {
    ($factory:ty) => {
        $crate::__private::export_entry_clap!($factory);
    };
}

/// `export_plugin!`'s CLAP half, absent because the `clap` feature is off.
#[cfg(not(feature = "clap"))]
#[doc(hidden)]
#[macro_export]
macro_rules! __daux_export_clap {
    ($factory:ty) => {};
}

/// The refusal `export_plugin!` emits when no format feature is enabled.
#[cfg(not(any(feature = "axt", feature = "vst3", feature = "clap")))]
#[doc(hidden)]
#[macro_export]
macro_rules! __daux_export_none {
    ($factory:ty) => {
        ::core::compile_error!(
            "daux_plugin::export_plugin! has no format to export, because `daux-plugin` was \
             built with none of the `axt`, `vst3` and `clap` features. Enable at least one in \
             this crate's Cargo.toml, e.g. \
             `daux-plugin = { version = \"0.1\", features = [\"axt\", \"vst3\", \"clap\"] }`."
        );
    };
}

/// Nothing to refuse: at least one format feature is enabled.
#[cfg(any(feature = "axt", feature = "vst3", feature = "clap"))]
#[doc(hidden)]
#[macro_export]
macro_rules! __daux_export_none {
    ($factory:ty) => {};
}

/// Implementation detail of the derives and of [`export_plugin!`]. Not a public API and not
/// covered by semantic versioning.
///
/// Generated code names every type it touches through `::daux_plugin::__private::*`, which is
/// what lets a plug-in crate depend on this facade alone: `#[derive(DauxState)]` writes
/// `::daux_plugin::__private::StateWriter` and does not care that the type lives in
/// `daux-state`. The contents are therefore fixed by
/// `daux-plugin-macros` and by the format crates' `export_entry!`
/// macros, not chosen here — every name below is emitted by one of them.
#[doc(hidden)]
pub mod __private {
    // `#[derive(DauxParams)]`.
    pub use daux_plugin_api::daux_parameter::{
        BoolParam, EnumParam, FloatParam, IntParam, MeterParam, Param, ParamEnum, ParamFlags,
        ParamId, ParamMigration, ParamRange, Params, Smoothing,
    };

    // `#[derive(DauxPlugin)]`.
    pub use daux_plugin_api::daux_audio::SampleFormats;
    pub use daux_plugin_api::daux_core::{Capabilities, Category, PluginDescriptor, Version};

    // `#[derive(DauxState)]`.
    pub use daux_plugin_api::daux_state::{
        StateError, StateReader, StateResult, StateVersion, StateWriter,
    };

    // `export_plugin!`. The adapters' own macros are re-exported rather than re-implemented,
    // so the entry points a plug-in gets are byte for byte the ones each adapter's tests
    // drive. `$crate` inside them still resolves to the adapter crate, which is why a plug-in
    // never has to name `daux-format-axt` and friends in its manifest.
    #[cfg(feature = "axt")]
    pub use daux_format_axt::export_entry as export_entry_axt;
    #[cfg(feature = "clap")]
    pub use daux_format_clap::export_entry as export_entry_clap;
    #[cfg(feature = "vst3")]
    pub use daux_format_vst3::export_entry as export_entry_vst3;
}
