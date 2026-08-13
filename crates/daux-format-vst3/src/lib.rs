//! VST3 export adapter for DAUx plug-ins — pure Rust, no Steinberg C++ SDK.
//!
//! A plug-in written once against `daux-plugin` becomes a VST3 by adding one line:
//!
//! ```ignore
//! daux_format_vst3::export_entry!(daux_plugin_api::SingleFactory<MyPlugin>);
//! ```
//!
//! That emits `GetPluginFactory` and the platform's module hooks, and everything else in this
//! crate is what happens behind them. There is no C++ anywhere: VST3 is a COM ABI, and a COM
//! ABI is a `#[repr(C)]` struct whose first field points at a table of `extern "system"`
//! function pointers, which Rust expresses natively. See [`com`] for the three details that
//! are easy to get wrong (calling convention, platform-dependent result codes,
//! platform-dependent interface ids) and [`api`] for the transcribed interfaces.
//!
//! # What the adapter is
//!
//! | Module | Job |
//! |---|---|
//! | [`com`] | the COM primitives: `TUid`, result codes, `FUnknown`, reference counting |
//! | [`api`] | every VST3 interface, structure and constant this adapter touches |
//! | [`factory`] | `IPluginFactory` 1/2/3 over a [`DauxFactory`](daux_plugin_api::DauxFactory) |
//! | [`component`] | `IComponent` + `IAudioProcessor` + `IEditController` + `IConnectionPoint` on one object |
//! | [`view`] | `IPlugView` over a [`DauxGraphic`](daux_plugin_api::DauxGraphic) |
//! | [`params`] | the parameter mirror: the controller half's race-free view of the plug-in |
//! | [`mapping`] | plain ↔ normalised, categories, speaker arrangements, transport |
//! | [`events`] | VST3 events ↔ DAUx events |
//! | [`stream`] | state through `IBStream` |
//! | [`guard`] | `catch_unwind` at every boundary, and poisoning after one |
//! | [`compat`] | what VST3 cannot express, reported at build time |
//!
//! # The four things a VST3 adapter usually gets wrong
//!
//! **1. Normalised versus plain values.** VST3 automation is a number in `0..=1`; a DAUx
//! parameter is a real-world value on its own curve. Converting with `min + n * (max - min)`
//! works for a linear parameter and is catastrophically wrong for a logarithmic one — a
//! filter cutoff automated to half travel is 632 Hz, not 10 kHz, and the difference is
//! invisible in a code review and obvious in a mix. Every conversion in this crate goes
//! through [`mapping::Curve`], which reconstructs the parameter's own curve exactly. Nothing
//! else in DAUx normalises: plain values are what cross the ABI and what land in a project
//! file, which is why changing a curve in version 2 of a plug-in cannot corrupt automation
//! written by version 1.
//!
//! **2. The component/controller split.** VST3 models a plug-in as two objects that may live
//! in different processes; DAUx models it as one object with an audio-thread half and a
//! main-thread half that share parameters directly. This adapter exports a
//! *single-component effect* — one COM object wearing both interfaces — because splitting
//! them would put a host round trip between an editor's knob and the DSP, and would turn a
//! meter into a serialised message. `IComponent::getControllerClassId` answers
//! `kNotImplemented`, which is VST3's documented way of saying "query `IEditController` from
//! me". The full mapping table is in [`component`].
//!
//! **3. Parameter ids.** Some adapters hash parameter *names* into VST3 `ParamID`s. DAUx ids
//! are already stable `u32`s and are permanent by contract (abi-v1 §14), so they are used
//! verbatim. Renaming a parameter is free; renumbering silently moves every saved automation
//! lane onto the wrong control. The same applies to class ids, which are a frozen 128-bit
//! hash of the permanent plug-in id — see [`cid`].
//!
//! **4. Reference counting.** Every object here has exactly one count shared by all its
//! interface heads, `queryInterface` for `FUnknown` returns the same pointer from every head
//! (COM identity), and the one place an object is created and immediately handed to
//! `queryInterface` — [`factory`]'s `createInstance` — releases its own reference whether the
//! query succeeded or not, so a refused interface frees the object rather than leaking it.
//!
//! # Panics never cross the boundary
//!
//! Every exported function runs its body inside `catch_unwind`. A panic becomes
//! `kInternalError` and **poisons** the object: every later call returns `kNotInitialized`
//! without re-entering plug-in code that has already broken its own invariants. That is
//! abi-v1 §17, and it is implemented once in [`guard`] rather than at sixty call sites.
//!
//! # Testing without a host
//!
//! The test suite builds a fake factory, a fake plug-in and fake host objects — a `Vec`-backed
//! `IBStream`, an `IParameterChanges`, an `IEventList`, an `IComponentHandler` — with real
//! vtables, and drives the generated C entry points through raw pointers. The vtable layout,
//! the reference counting and the panic boundary can only break at the ABI, so that is where
//! they are tested.

pub mod api;
pub mod cid;
pub mod com;
pub mod compat;
pub mod component;
pub mod entry;
pub mod events;
pub mod factory;
pub mod guard;
pub mod mapping;
pub mod params;
pub mod stream;
pub mod strings;
pub mod view;

pub use compat::{CompatibilityWarning, WarningLevel, compatibility_report};

#[cfg(test)]
mod testkit;

#[cfg(test)]
mod tests;
