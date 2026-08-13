//! `daux.audio-ports/1` — bus topology (abi-v1 §11.1).
//!
//! `daux-audio`'s layout, purpose and flag codes are transcribed from the same section of the
//! specification, so the translation is a field copy rather than a mapping table. The one thing
//! that is *not* a copy is the port id: abi-v1 §14 makes it permanent per plug-in, and it comes
//! from [`BusInfo::id`](daux_plugin_api::BusInfo), never from the index.

use daux_abi::{
    DAUX_ERR_NOT_FOUND, DAUX_OK, DauxAudioPortInfoV1, DauxAudioPortsApiV1, DauxBool, DauxName,
    DauxPluginHandle, DauxStatus, daux_bool_is_true,
};
use daux_plugin_api::BusInfo;

use crate::instance::with_instance;
use crate::panic::{Refusal, status_of_error};

/// [main-thread] Number of buses in one direction.
///
/// # Safety
///
/// See [`with_instance`].
unsafe extern "C" fn count(p: DauxPluginHandle, is_input: DauxBool) -> u32 {
    // SAFETY: forwarded verbatim from this function's own contract.
    unsafe {
        with_instance(p, |state| {
            let Ok(layout) = state.instance.bus_layout() else {
                return 0;
            };
            let buses = if daux_bool_is_true(is_input) {
                layout.inputs.len()
            } else {
                layout.outputs.len()
            };
            u32::try_from(buses).unwrap_or(u32::MAX)
        })
    }
}

/// [main-thread] Fills `out` with one bus description.
///
/// # Safety
///
/// `out` is null or points at a writable, aligned [`DauxAudioPortInfoV1`]. See
/// [`with_instance`].
unsafe extern "C" fn get(
    p: DauxPluginHandle,
    index: u32,
    is_input: DauxBool,
    out: *mut DauxAudioPortInfoV1,
) -> DauxStatus {
    let body = |state: &mut crate::instance::AxtInstance| {
        if out.is_null() {
            return DauxStatus::INVALID_ARG;
        }
        // SAFETY: `size` is the first field of every revision and can be read before the rest
        // of the structure is trusted (abi-v1 §3); the caller guarantees the pointer is
        // aligned.
        let declared = unsafe { (&raw const (*out).size).read() };
        if declared != 0 && (declared as usize) < DauxAudioPortInfoV1::MIN_SIZE_V1_0 {
            return daux_abi::DAUX_ERR_ABI_MISMATCH;
        }
        let layout = match state.instance.bus_layout() {
            Ok(layout) => layout,
            Err(err) => return status_of_error(&err),
        };
        let buses = if daux_bool_is_true(is_input) {
            &layout.inputs
        } else {
            &layout.outputs
        };
        let Some(bus) = buses.get(index as usize) else {
            return DAUX_ERR_NOT_FOUND;
        };
        // SAFETY: `out` is non-null and, per this function's contract, writable and aligned;
        // the size check above proved it covers the v1.0 revision. Nothing is read out of it
        // first, so its previous contents may be anything.
        unsafe { write_port(bus, &mut *out) };
        DAUX_OK
    };
    // SAFETY: forwarded verbatim from this function's own contract.
    unsafe { with_instance(p, body) }
}

/// Copies one bus into its ABI form.
fn write_port(bus: &BusInfo, out: &mut DauxAudioPortInfoV1) {
    *out = DauxAudioPortInfoV1::new();
    out.id = bus.id;
    out.name = DauxName::new(&bus.name);
    out.channel_count = u32::from(bus.channel_count());
    out.layout = bus.layout.as_bits();
    out.purpose = bus.purpose.as_bits();
    out.flags = bus.flags.bits();
}

/// The `daux.audio-ports/1` table.
///
/// `set_active` is null: `daux-core` has no hook for activating a bus, so advertising one would
/// be a promise this adapter cannot keep. A null optional entry is how the ABI spells that
/// (abi-v1 §2.3).
pub(crate) static TABLE: DauxAudioPortsApiV1 = DauxAudioPortsApiV1 {
    size: DauxAudioPortsApiV1::SIZE,
    _pad0: 0,
    count,
    get,
    set_active: None,
    reserved: [0; 4],
};
