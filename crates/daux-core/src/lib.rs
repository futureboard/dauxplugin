//! The format-neutral DAUx plug-in object model.
//!
//! This crate is the centre of DAUxPlug. A plug-in is written against the traits here (via
//! the friendlier `daux-plugin` facade), and every export format — `.axt`, VST3, CLAP —
//! translates *to* this model rather than the model bending toward any of them. Nothing in
//! here knows what a VST3 `IComponent` or a `clap_plugin` is, and nothing ever should: that
//! translation lives in `crates/daux-format-*`.
//!
//! # The shape of a plug-in
//!
//! | Trait | Thread | Owns |
//! |---|---|---|
//! | [`DauxProcessor`] | audio | DSP state, buffers, voices |
//! | [`DauxController`] | main | parameters, state, host communication |
//! | [`DauxPlugin`] | main | both halves, bus and event topology, the editor |
//! | [`DauxFactory`] | main | enumeration and instantiation |
//!
//! The processor/controller split is not organisational tidiness. It is what lets the audio
//! thread run under the rules of `docs/architecture/realtime.md` while the main thread
//! allocates, formats strings and talks to the host — and it is enforced by the types:
//! [`ProcessContext`] can only reach
//! [`RtHostServices`](daux_host_services::RtHostServices), never the full
//! [`HostServices`](daux_host_services::HostServices).
//!
//! # Dependencies
//!
//! `daux-core` depends only on the foundation crates and on nothing external. It adds no new
//! primitive types: buffers come from `daux-audio`, events from `daux-events`, parameters
//! from `daux-parameter`, and so on. What it adds is the *shape* those pieces are assembled
//! into.

mod capabilities;
mod category;
mod descriptor;
mod error;
mod id;
mod plugin;
mod ports;
mod process;
mod version;

pub use capabilities::Capabilities;
pub use category::Category;
pub use descriptor::{PluginDescriptor, PluginDescriptorBuilder};
pub use error::{DauxError, DauxResult, ErrorKind, status};
pub use id::PluginId;
pub use plugin::{DauxController, DauxFactory, DauxPlugin, DauxProcessor};
pub use ports::{EventPortInfo, EventPortLayout};
pub use process::{
    Latency, ProcessConfig, ProcessContext, ProcessEvents, ProcessMode, ProcessStatus, Tail,
};
pub use version::Version;

/// The foundation crates, re-exported so a downstream crate can name the types in these
/// signatures without depending on each one individually.
pub use {
    daux_audio, daux_events, daux_host_services, daux_midi, daux_parameter, daux_rt, daux_state,
    daux_transport,
};
