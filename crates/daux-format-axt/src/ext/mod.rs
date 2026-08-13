//! The standard plug-in-side extensions (abi-v1 §11).
//!
//! Each submodule owns one `static` function table and the entries that fill it. They are
//! `static`s rather than per-instance copies because none of them closes over anything: the
//! instance always arrives as a [`DauxPluginHandle`](daux_abi::DauxPluginHandle) argument, so
//! one table serves every instance in the process.
//!
//! Every entry goes through [`with_instance`](crate::instance::with_instance) and therefore
//! inherits the same two guarantees as the lifecycle entries: no unwind escapes, and a poisoned
//! instance refuses to run plug-in code (abi-v1 §17).
//!
//! `daux.note-ports/1` is listed in §11 but has no function table in ABI v1.0, so this crate
//! answers null for it, which is the specified behaviour for an extension a module does not
//! provide. The event port topology a plug-in declares is still reachable — through
//! [`EventPortLayout`](daux_plugin_api::EventPortLayout) on the plug-in side — and will be
//! published here when v1.1 defines the table.

pub(crate) mod audio_ports;
pub(crate) mod gui;
pub(crate) mod params;
pub(crate) mod render;
pub(crate) mod state;
