//! Derive macros for DAUxPlug: [`DauxParams`], [`DauxPlugin`] and [`DauxState`].
//!
//! This crate is glue, not policy. Every macro here turns a struct and its attributes
//! into code that a careful author could have written by hand, and it refuses to guess:
//! anything ambiguous is a compile error with the exact line to write instead.
//!
//! # What the macros generate
//!
//! | Derive | Generates | Never generates |
//! |---|---|---|
//! | [`DauxParams`] | `impl Params` + an inherent `new()` when every field is described | any DSP, any state |
//! | [`DauxPlugin`] | an inherent `descriptor()` | any DSP, any trait impl |
//! | [`DauxState`] | inherent `save_state` / `load_state` | any schema migration |
//!
//! `#[derive(DauxPlugin)]` deliberately stops at the descriptor. A plug-in's audio
//! behaviour is the one thing a macro must never invent, so `DauxProcessor`,
//! `DauxController` and `DauxPlugin` are always written by hand.
//!
//! # The `::daux_plugin` path
//!
//! Generated code names every type through `::daux_plugin::__private::*`, so a plug-in
//! crate depends on `daux-plugin` alone and never on `daux-core`, `daux-parameter` or
//! `daux-state` directly. Crates inside this workspace that do not depend on the facade
//! redirect the path with `crate = ::some_other_crate` on the container attribute:
//!
//! ```ignore
//! #[derive(DauxParams)]
//! #[params(crate = ::daux_plugin_api)]
//! struct MyParams { /* … */ }
//! ```
//!
//! # Threads
//!
//! Everything generated here is `[main-thread]`: constructing parameters, building a
//! descriptor and reading or writing state all allocate. The one exception is
//! `Params::param`, which the [`DauxParams`] expansion lowers to a `match` on the raw id
//! precisely so that it is allocation-free and `[audio-thread]`-safe, as required by
//! `docs/specifications/abi-v1.md` §15.

mod attr;
mod params;
mod plugin;
mod state;

use proc_macro::TokenStream;
use syn::{DeriveInput, parse_macro_input};

/// Generates `impl Params` for a parameter bank.
///
/// Every field that is a parameter carries `#[param(..)]`; every field that is not
/// carries `#[param(skip)]` or no attribute at all. Ids are checked for uniqueness while
/// the macro runs, or — when an id is a constant the macro cannot evaluate — by a
/// generated `const` block, because a repeated id makes every saved project ambiguous.
///
/// The container accepts `#[params(state_schema_version = .., migrations = .., crate = ..)]`.
///
/// # Field keys
///
/// | Key | Meaning |
/// |---|---|
/// | `id = 1` / `id = "gain"` / `id = CONST` | the permanent parameter id (required) |
/// | `name = "Gain"` | display name; writing it opts the field into the generated `new()` |
/// | `range = -60.0..=12.0` | inclusive bounds, in the parameter's own units |
/// | `default = 0.0` | reset value |
/// | `unit = "dB"`, `group = "Output"`, `decimals = 1` | display |
/// | `curve = "linear" \| "log" \| "skew(2.0)" \| "stepped"` | value mapping |
/// | `smoothing = "none" \| "linear(20.0)" \| "exponential(20.0)"` | ramp intent |
/// | `flags(automatable, hidden, …)` | `ParamFlags` bits |
/// | `labels("Off", "On")` | `BoolParam` state names |
/// | `skip` | not a parameter |
///
/// An inherent `new()` is generated only when **every** field of the struct is a
/// parameter described completely by its attribute; a skipped field, an unannotated
/// field or a field carrying only `id` means the author has to write `new()` themselves,
/// which is exactly the point at which a generated one would have been wrong.
///
/// `[main-thread]` for the generated `new()` and `param_refs`; `[any-thread]` for the
/// generated `param` lookup, which never allocates.
#[proc_macro_derive(DauxParams, attributes(param, params))]
pub fn derive_daux_params(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    params::derive(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Generates an inherent `descriptor()` from `#[plugin(..)]`.
///
/// It generates **only** the descriptor: no processor, no controller, no `impl
/// DauxPlugin`. Delegate to it from the hand-written trait impl:
///
/// ```ignore
/// #[derive(DauxPlugin)]
/// #[plugin(id = "com.example.gain", name = "Gain", vendor = "Example",
///          version = "1.0.0", category = "effect", capabilities(audio_effect, has_gui))]
/// struct Gain { /* … */ }
///
/// impl daux_plugin::DauxPlugin for Gain {
///     fn descriptor() -> daux_plugin::PluginDescriptor { Self::descriptor() }
///     // … the parts a macro must not invent
/// }
/// ```
///
/// The id grammar (reverse-DNS, lower-case ASCII, at most 127 bytes), the version
/// spelling, the category name, the capability names and the feature tags are all
/// checked while the macro runs, so a descriptor that would fail
/// `PluginDescriptor::validate` at run time fails to compile instead.
///
/// `[main-thread]`.
#[proc_macro_derive(DauxPlugin, attributes(plugin))]
pub fn derive_daux_plugin(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    plugin::derive(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Generates inherent `save_state` / `load_state` over `#[state(..)]` fields.
///
/// Each annotated field is written under a key that defaults to the field's name, in
/// declaration order, so a blob written by one build reads back into the next one. Field
/// types are recognised by name — `f32`, `f64`, the integer types, `bool`, `String` and
/// `Vec<u8>` — and anything else needs `kind = ".."`, `nested` or `skip`.
///
/// | Key | Meaning |
/// |---|---|
/// | `key = "gain"` | storage key; defaults to the field name |
/// | `kind = "f64" \| "i64" \| "bool" \| "str" \| "bytes"` | override the inferred codec |
/// | `default` / `default = EXPR` | a missing key restores this instead of failing |
/// | `nested` | the field is itself `#[derive(DauxState)]`, stored as a group |
/// | `skip` | not part of the state |
///
/// The container accepts `#[state(version = 2, group = "dsp", crate = ..)]`.
///
/// Integers round-trip through `i64` with checked conversions in both directions, so a
/// hostile blob cannot silently truncate a field; a value that does not fit is a
/// `StateError`, never a wrapped number.
///
/// `[main-thread]`: both generated methods allocate.
#[proc_macro_derive(DauxState, attributes(state))]
pub fn derive_daux_state(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    state::derive(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}
