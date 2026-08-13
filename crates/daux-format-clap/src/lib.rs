//! CLAP export for DAUx plug-ins, in pure Rust.
//!
//! A plug-in written once against `daux-plugin` becomes a CLAP plug-in by adding one line:
//!
//! ```ignore
//! daux_format_clap::export_entry!(MyFactory);
//! ```
//!
//! That emits the `clap_entry` symbol a CLAP host looks for, and everything below it: the
//! factory, the instance, and the `audio-ports`, `note-ports`, `params`, `state`, `gui`,
//! `latency`, `tail` and `render` extensions.
//!
//! # No C, anywhere
//!
//! There is no `clap-sys` dependency and no vendored header. [`abi`] is a hand-written
//! `#[repr(C)]` transcription of the CLAP 1.2 structs this adapter needs, in the same spirit
//! as `daux-abi`'s transcription of `docs/specifications/abi-v1.md`. Nothing that crosses
//! the boundary is a Rust `enum`, a `Vec`, a `String`, a reference or a generic.
//!
//! # How well the two models fit
//!
//! Better than any other format DAUx targets, because CLAP made the same choices:
//!
//! | Concept | CLAP | DAUx | Fit |
//! |---|---|---|---|
//! | Parameter values | plain | plain | identical — no normalisation anywhere |
//! | Events | sample-accurate, sorted | sample-accurate, sorted | identical, ordering preserved |
//! | Process result | continue / sleep / tail | [`ProcessStatus`](daux_plugin_api::ProcessStatus) | identical codes |
//! | Note expression | seven dimensions | seven dimensions | identical ids |
//! | Tail | samples or `UINT32_MAX` | `None`/`Samples`/`Infinite`/`Unknown` | `Unknown` folds into infinite |
//! | Transport | fixed-point beats/seconds | `f64` | exact for the values the factor represents |
//!
//! What CLAP cannot express — a shared-texture editor, "unusable without a GUI", sandbox
//! safety, dynamic bus renegotiation — is reported by
//! [`compatibility_report`], so `daux build` prints it rather than leaving an author to
//! find out from a bug report.
//!
//! # The rules every exported function follows
//!
//! * Its whole body is wrapped in `catch_unwind`; a panic never crosses the boundary and is
//!   converted into CLAP's failure value (abi-v1 §17).
//! * A panic **poisons** the instance: every later call refuses rather than re-entering
//!   plug-in code whose invariants have already broken once. `destroy` still works.
//! * `process` allocates nothing, locks nothing and blocks on nothing. When it cannot take
//!   exclusive access it silences the outputs and returns `CLAP_PROCESS_ERROR` rather than
//!   waiting.
//! * There is no global mutable state. The one process-wide thing is the write-once
//!   descriptor table behind `clap_entry`, which abi-v1 §16.1 describes as immortal; every
//!   instance owns everything else, so hundreds of them coexist without sharing anything.
//!
//! # Known limitations
//!
//! * **No in-place processing.** Every audio port advertises
//!   `in_place_pair = CLAP_INVALID_ID`. Handing one buffer to a plug-in as both input and
//!   output would mean a live `&` and `&mut` to the same samples, which is undefined
//!   behaviour whatever the DSP does with them.
//! * **Embedded editors only.** A floating editor means the plug-in owns a top-level
//!   window; `DauxGraphic` is built around the host handing over a view (abi-v1 §11.4).
//!   Wayland is refused as well, because `clap_window` carries one pointer and Wayland needs
//!   a surface *and* a display.
//! * **Main-thread parameter reads contend with `process`.** `DauxPlugin::processor` and
//!   `DauxController::controller` both take `&mut self`, so an adapter cannot serve
//!   `params.get_value` on the UI thread while `process` runs on the audio thread without
//!   serialising the two. The audio thread always wins; the UI thread waits for at most one
//!   block. Removing the contention needs a shared, interior-mutable handle to the parameter
//!   set in `daux-core`; the module documentation of this crate's `lock` module carries
//!   the full argument.
//! * **No `clap.audio-ports-config`, `clap.voice-info`, `clap.preset-load` or
//!   `clap.timer-support`.** They are additions, not translations, and each one needs a DAUx
//!   concept that does not exist yet.
//!
//! # Testing an adapter without a host
//!
//! The tests in this crate build a fake factory, a fake `clap_host` and a real
//! `clap_process`, then drive the generated C entry points through raw pointers — including
//! a plug-in that panics on demand, to prove that the panic is caught, that the instance is
//! poisoned, and that `destroy` still works afterwards.

pub mod abi;

mod compat;
mod descriptor;
mod entry;
mod events;
mod host;
mod lock;
mod params;
mod plugin;
mod text;
mod transport;

#[cfg(test)]
mod testkit;

pub use compat::{CompatibilityWarning, WarningSeverity, compatibility_report};
pub use descriptor::{OwnedDescriptor, clap_expressible_capabilities, clap_features};
pub use entry::ClapEntry;
pub use events::{ClapInputList, ClapOutputList};
pub use host::ClapHostBridge;
pub use lock::{InstanceLock, LockGuard};
pub use params::{fill_param_info, param_flags_to_clap};
pub use plugin::{ClapInstance, NAME_CAPACITY, fallback_editor_size, published_extensions};
pub use transport::{
    beats_from_f64, beats_to_f64, seconds_from_f64, seconds_to_f64, transport_from_clap,
    transport_to_clap,
};

/// The DAUx authoring API, re-exported so a plug-in crate can name the types in
/// [`compatibility_report`]'s signature without adding a dependency.
pub use daux_plugin_api;

#[cfg(test)]
mod tests {
    use super::*;
    use daux_plugin_api::{Capabilities, PluginDescriptor};

    #[test]
    fn the_crate_surface_is_reachable_from_the_root() {
        let d = PluginDescriptor::builder("com.example.surface", "Surface")
            .capabilities(Capabilities::AUDIO_EFFECT)
            .build()
            .unwrap();
        assert!(compatibility_report(&d).is_empty());
        assert_eq!(clap_features(&d), ["audio-effect"]);
        assert!(clap_expressible_capabilities().is_audio_effect());
        assert_eq!(NAME_CAPACITY, abi::CLAP_NAME_SIZE);
        assert_eq!(published_extensions().len(), 8);
        assert_eq!(fallback_editor_size().width, 640.0);
        assert_eq!(beats_to_f64(beats_from_f64(2.5)), 2.5);
    }

    /// `export_entry!` has to work from outside the module that defines it, with the
    /// factory named by a path the caller chooses. Emitting the symbol here would collide
    /// with nothing, but a second `clap_entry` in one binary would, so the macro's body is
    /// exercised through the same associated const it expands to.
    #[test]
    fn the_entry_const_the_macro_expands_to_is_well_formed() {
        use crate::testkit::TestFactory;
        let entry = ClapEntry::<TestFactory>::ENTRY;
        assert_eq!(entry.clap_version, abi::ClapVersion::CURRENT);
        assert!(entry.clap_version.is_compatible());
    }
}
