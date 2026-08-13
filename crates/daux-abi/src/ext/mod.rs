//! Standard extension ids and their function tables (`abi-v1` §11).
//!
//! Extensions are looked up by NUL-free UTF-8 id and return a pointer to a `#[repr(C)]`
//! function table owned by the providing module. Ids embed their version; a new version is
//! a new id. **Unknown ids MUST return null rather than fail.**
//!
//! Vendor extensions MUST use a reverse-DNS prefix; the `daux.` prefix is reserved.
//!
//! Four ids in the §11 table — [`NOTE_PORTS`], [`HOST_LATENCY`], [`HOST_TAIL`] and
//! [`HOST_TIMER`] — are named by the specification but have no function table defined in
//! v1.0, so this crate transcribes their ids only. A module that is asked for one of them
//! returns null, which is the specified behaviour for an unsupported extension.

pub mod audio_ports;
pub mod gui;
pub mod params;
pub mod render;
pub mod state;

use crate::string::DauxStrView;

/// Bus topology. Provider: plug-in. Table: [`audio_ports::DauxAudioPortsApiV1`].
pub const AUDIO_PORTS: &str = "daux.audio-ports/1";
/// Event port topology. Provider: plug-in. No table is defined in ABI v1.0.
pub const NOTE_PORTS: &str = "daux.note-ports/1";
/// Parameter model. Provider: plug-in. Table: [`params::DauxParamsApiV1`].
pub const PARAMS: &str = "daux.params/1";
/// Save / load. Provider: plug-in. Table: [`state::DauxStateApiV1`].
pub const STATE: &str = "daux.state/1";
/// Editor lifecycle. Provider: plug-in. Table: [`gui::DauxGuiApiV1`].
pub const GUI: &str = "daux.gui/1";
/// Latency reporting. Provider: plug-in. Table: [`render::DauxLatencyApiV1`].
pub const LATENCY: &str = "daux.latency/1";
/// Tail length. Provider: plug-in. Table: [`render::DauxTailApiV1`].
pub const TAIL: &str = "daux.tail/1";
/// Real-time / offline switch. Provider: plug-in. Table: [`render::DauxRenderApiV1`].
pub const RENDER: &str = "daux.render/1";

/// Structured logging. Provider: host. Table: [`DauxHostLogApiV1`](crate::DauxHostLogApiV1).
pub const HOST_LOG: &str = "daux.host.log/1";
/// Automation gestures and rescan requests. Provider: host.
/// Table: [`DauxHostParamsApiV1`](crate::DauxHostParamsApiV1).
pub const HOST_PARAMS: &str = "daux.host.params/1";
/// Latency change notification. Provider: host. No table is defined in ABI v1.0.
pub const HOST_LATENCY: &str = "daux.host.latency/1";
/// Tail change notification. Provider: host. No table is defined in ABI v1.0.
pub const HOST_TAIL: &str = "daux.host.tail/1";
/// Off-thread work scheduling. Provider: host.
/// Table: [`DauxHostWorkerApiV1`](crate::DauxHostWorkerApiV1).
pub const HOST_WORKER: &str = "daux.host.worker/1";
/// Resize requests and editor close notification. Provider: host.
/// Table: [`DauxHostGuiApiV1`](crate::DauxHostGuiApiV1).
pub const HOST_GUI: &str = "daux.host.gui/1";
/// Periodic main-thread callback. Provider: host. No table is defined in ABI v1.0.
pub const HOST_TIMER: &str = "daux.host.timer/1";

/// GPU surface hand-off (§13). Provider: both.
/// Table payload: [`DauxSharedTextureV1`](crate::DauxSharedTextureV1).
pub const SHARED_TEXTURE: &str = "com.futureboard.daux.shared-texture/1";

/// Every extension id defined by `abi-v1` §11, in table order.
pub const ALL_IDS: &[&str] = &[
    AUDIO_PORTS,
    NOTE_PORTS,
    PARAMS,
    STATE,
    GUI,
    LATENCY,
    TAIL,
    RENDER,
    HOST_LOG,
    HOST_PARAMS,
    HOST_LATENCY,
    HOST_TAIL,
    HOST_WORKER,
    HOST_GUI,
    HOST_TIMER,
    SHARED_TEXTURE,
];

/// [any-thread] `true` when the extension id `view` names `id`.
///
/// This is the comparison a `get_extension` implementation performs. It never allocates and
/// never panics; a malformed or non-UTF-8 view simply matches nothing.
///
/// # Safety
///
/// `view` must satisfy the contract of [`DauxStrView::as_bytes`]: it points to `len`
/// initialised bytes that stay valid for the duration of this call.
#[inline]
#[must_use]
pub unsafe fn id_matches(view: DauxStrView, id: &str) -> bool {
    // SAFETY: forwarded verbatim from this function's own safety contract. The borrow ends
    // before the function returns, so the lifetime chosen here cannot escape.
    let Some(bytes) = (unsafe { view.as_bytes() }) else {
        return false;
    };
    bytes == id.as_bytes()
}
