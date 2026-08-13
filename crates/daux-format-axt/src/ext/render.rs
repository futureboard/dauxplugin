//! `daux.latency/1`, `daux.tail/1` and `daux.render/1` (abi-v1 §11.5).
//!
//! Three tiny tables that share a file because they share a subject: what the host needs to
//! know about scheduling this plug-in.
//!
//! The tail encoding is exact rather than clamped. abi-v1 §11.5 defines
//! `DAUX_TAIL_INFINITE = u32::MAX`; `daux-core` adds `u32::MAX - 1` for "unknown", which a host
//! that does not know the convention reads as a very long but finite tail — the safe
//! misreading, since it keeps calling the plug-in.

use daux_abi::{
    DAUX_ERR_INVALID_STATE, DAUX_OK, DauxBool, DauxLatencyApiV1, DauxPluginHandle, DauxRenderApiV1,
    DauxStatus, DauxTailApiV1, daux_bool,
};
use daux_plugin_api::{InstanceState, ProcessMode};

use crate::instance::with_instance;

/// [main-thread] Latency in samples, or `0` when it cannot be asked for.
///
/// # Safety
///
/// See [`with_instance`].
unsafe extern "C" fn latency_get(p: DauxPluginHandle) -> u32 {
    // SAFETY: forwarded verbatim from this function's own contract.
    unsafe {
        with_instance(p, |state| {
            state.instance.latency().map_or(0, |l| l.samples())
        })
    }
}

/// [any-thread] Tail in samples, or [`DAUX_TAIL_INFINITE`](daux_abi::DAUX_TAIL_INFINITE).
///
/// # Safety
///
/// See [`with_instance`]. This entry is `[any-thread]` in the specification, but like every
/// other entry it reads the instance state, so a host must not call it concurrently with
/// another call on the same instance.
unsafe extern "C" fn tail_get(p: DauxPluginHandle) -> u32 {
    // SAFETY: forwarded verbatim from this function's own contract.
    unsafe { with_instance(p, |state| state.instance.tail().map_or(0, |t| t.samples())) }
}

/// [any-thread] Whether the plug-in must be scheduled in real time even for an offline render.
///
/// # Safety
///
/// See [`with_instance`].
unsafe extern "C" fn has_hard_realtime_requirement(p: DauxPluginHandle) -> DauxBool {
    // SAFETY: forwarded verbatim from this function's own contract.
    unsafe {
        with_instance(p, |state| {
            daux_bool(
                state
                    .descriptor
                    .as_ref()
                    .is_some_and(|d| d.capabilities.is_hard_realtime()),
            )
        })
    }
}

/// [main-thread, inactive only] Switches between the `DAUX_PROCESS_MODE_*` modes.
///
/// The mode is remembered and applied at the next `activate`, because it is part of the
/// configuration a processor sizes itself from and changing it under a live activation would
/// mean re-preparing on the wrong thread.
///
/// # Safety
///
/// See [`with_instance`].
unsafe extern "C" fn set_mode(p: DauxPluginHandle, mode: u32) -> DauxStatus {
    // SAFETY: forwarded verbatim from this function's own contract.
    unsafe {
        with_instance(p, |state| match state.instance.state() {
            InstanceState::Created | InstanceState::Inactive => {
                state.render_mode = Some(ProcessMode::from_code(mode));
                DAUX_OK
            }
            // Active or Processing: abi-v1 §11.5 marks this entry "inactive only".
            _ => DAUX_ERR_INVALID_STATE,
        })
    }
}

/// The `daux.latency/1` table.
pub(crate) static LATENCY_TABLE: DauxLatencyApiV1 = DauxLatencyApiV1 {
    size: DauxLatencyApiV1::SIZE,
    _pad0: 0,
    get: latency_get,
    reserved: [0; 2],
};

/// The `daux.tail/1` table.
pub(crate) static TAIL_TABLE: DauxTailApiV1 = DauxTailApiV1 {
    size: DauxTailApiV1::SIZE,
    _pad0: 0,
    get: tail_get,
    reserved: [0; 2],
};

/// The `daux.render/1` table.
pub(crate) static RENDER_TABLE: DauxRenderApiV1 = DauxRenderApiV1 {
    size: DauxRenderApiV1::SIZE,
    _pad0: 0,
    has_hard_realtime_requirement,
    set_mode,
    reserved: [0; 2],
};
