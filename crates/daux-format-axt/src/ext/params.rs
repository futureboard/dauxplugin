//! `daux.params/1` — the parameter model (abi-v1 §11.2).
//!
//! # Values are plain, always
//!
//! Every value that crosses this table is a real-world value: dB, Hz, an enum index. The
//! normalised `0..=1` form never leaves the plug-in. That is not a style choice — it is what
//! makes a curve change in version 2 of a plug-in safe for automation written by version 1,
//! because a host's automation lane stores the number it was given.
//!
//! # Indices and ids
//!
//! `get_info` addresses parameters by **index**, everything else by **id**. The index is the
//! position in [`Params::param_refs`](daux_plugin_api::Params::param_refs), which a plug-in
//! must keep stable; the id is permanent forever (abi-v1 §14).
//!
//! # `flush` and the audio thread
//!
//! `flush` is `[main-thread when inactive, audio-thread otherwise]`, so it must not allocate.
//! It looks parameters up with [`Params::param`](daux_plugin_api::Params::param), whose default
//! implementation walks `param_refs` and *does* allocate — which is exactly why `daux-parameter`
//! documents that any `Params` reachable from the audio thread has to override it, and why the
//! derive macro does.

use daux_abi::{
    DAUX_ERR_NOT_FOUND, DAUX_OK, DauxEventListV1, DauxParamInfoV1, DauxParamsApiV1,
    DauxPluginHandle, DauxStatus, DauxStrView, DauxText,
};
use daux_plugin_api::{DauxEvent, InputEvents, Param, ParamId, ParamInfo};

use crate::events::AbiInputEvents;
use crate::instance::{AxtInstance, with_instance};
use crate::panic::{Refusal, status_of_error};

/// [main-thread] Number of parameters.
///
/// # Safety
///
/// See [`with_instance`].
unsafe extern "C" fn count(p: DauxPluginHandle) -> u32 {
    // SAFETY: forwarded verbatim from this function's own contract.
    unsafe {
        with_instance(p, |state| match state.instance.params() {
            Ok(params) => u32::try_from(params.param_refs().len()).unwrap_or(u32::MAX),
            Err(_) => 0,
        })
    }
}

/// [main-thread] Fills `out` with the parameter at `index`.
///
/// # Safety
///
/// `out` is null or points at a writable, aligned [`DauxParamInfoV1`]. See [`with_instance`].
unsafe extern "C" fn get_info(
    p: DauxPluginHandle,
    index: u32,
    out: *mut DauxParamInfoV1,
) -> DauxStatus {
    let body = |state: &mut AxtInstance| {
        if out.is_null() {
            return DauxStatus::INVALID_ARG;
        }
        // SAFETY: `size` is present in every revision and readable before the rest of the
        // structure is trusted (abi-v1 §3); the caller guarantees the pointer is aligned.
        let declared = unsafe { (&raw const (*out).size).read() };
        if declared != 0 && (declared as usize) < DauxParamInfoV1::MIN_SIZE_V1_0 {
            return daux_abi::DAUX_ERR_ABI_MISMATCH;
        }
        let params = match state.instance.params() {
            Ok(params) => params,
            Err(err) => return status_of_error(&err),
        };
        let refs = params.param_refs();
        let Some((_, param)) = refs.get(index as usize) else {
            return DAUX_ERR_NOT_FOUND;
        };
        let info = param.info();
        // SAFETY: `out` is non-null and, per this function's contract, writable and aligned;
        // the size check above proved it covers the v1.0 revision. It is written whole, so its
        // previous contents may be anything.
        unsafe { write_info(&info, &mut *out) };
        DAUX_OK
    };
    // SAFETY: forwarded verbatim from this function's own contract.
    unsafe { with_instance(p, body) }
}

/// [main-thread] Reads one plain value.
///
/// # Safety
///
/// `out` is null or points at a writable, aligned `f64`. See [`with_instance`].
unsafe extern "C" fn get_value(p: DauxPluginHandle, id: u32, out: *mut f64) -> DauxStatus {
    let body = |state: &mut AxtInstance| {
        if out.is_null() {
            return DauxStatus::INVALID_ARG;
        }
        let Some(value) = with_param(state, id, |param| param.plain()) else {
            return DAUX_ERR_NOT_FOUND;
        };
        // SAFETY: `out` is non-null and, per this function's contract, a writable aligned `f64`.
        unsafe { out.write(value) };
        DAUX_OK
    };
    // SAFETY: forwarded verbatim from this function's own contract.
    unsafe { with_instance(p, body) }
}

/// [main-thread] Formats a plain value for display.
///
/// # Safety
///
/// `out` is null or points at a writable, aligned [`DauxText`]. See [`with_instance`].
unsafe extern "C" fn value_to_text(
    p: DauxPluginHandle,
    id: u32,
    value: f64,
    out: *mut DauxText,
) -> DauxStatus {
    let body = |state: &mut AxtInstance| {
        if out.is_null() {
            return DauxStatus::INVALID_ARG;
        }
        let mut text = String::new();
        let Some(()) = with_param(state, id, |param| param.to_text(value, &mut text)) else {
            return DAUX_ERR_NOT_FOUND;
        };
        // SAFETY: `out` is non-null and, per this function's contract, a writable aligned
        // `DauxText`. It is written whole, so its previous contents may be anything.
        unsafe { out.write(DauxText::new(&text)) };
        DAUX_OK
    };
    // SAFETY: forwarded verbatim from this function's own contract.
    unsafe { with_instance(p, body) }
}

/// [main-thread] Parses user input into a plain value.
///
/// # Safety
///
/// `text` points at `text.len` readable bytes for the call; `out` is null or points at a
/// writable, aligned `f64`. See [`with_instance`].
unsafe extern "C" fn text_to_value(
    p: DauxPluginHandle,
    id: u32,
    text: DauxStrView,
    out: *mut f64,
) -> DauxStatus {
    let body = |state: &mut AxtInstance| {
        if out.is_null() {
            return DauxStatus::INVALID_ARG;
        }
        // SAFETY: this function's contract guarantees the view is readable for the call; the
        // borrow ends well inside it.
        let Some(text) = (unsafe { text.as_str() }) else {
            return DauxStatus::INVALID_ARG;
        };
        match with_param(state, id, |param| param.from_text(text)) {
            // The parameter exists and accepted the text.
            Some(Some(value)) => {
                // SAFETY: `out` is non-null and, per this function's contract, a writable
                // aligned `f64`.
                unsafe { out.write(value) };
                DAUX_OK
            }
            // The parameter exists and rejected the text.
            Some(None) => DauxStatus::INVALID_ARG,
            None => DAUX_ERR_NOT_FOUND,
        }
    };
    // SAFETY: forwarded verbatim from this function's own contract.
    unsafe { with_instance(p, body) }
}

/// [main-thread when inactive, audio-thread otherwise] Applies parameter events outside
/// `process`.
///
/// Only `PARAM_VALUE` is applied. `PARAM_MOD` is deliberately ignored: modulation is a
/// per-block offset that belongs to `process`, and writing it into the parameter itself would
/// make it permanent — the classic way a modulated parameter drifts.
///
/// Nothing is written to `out_events`: a plug-in has no `daux-core` hook for producing events
/// outside `process`, and inventing one here would be a format concept leaking into the model.
///
/// # Safety
///
/// `in_events` and `out_events` are null or valid [`DauxEventListV1`]s for the call. See
/// [`with_instance`].
unsafe extern "C" fn flush(
    p: DauxPluginHandle,
    in_events: *const DauxEventListV1,
    _out_events: *const DauxEventListV1,
) {
    let body = |state: &mut AxtInstance| {
        // SAFETY: this function's contract guarantees the list is null or valid for the call,
        // which is what the adapter's constructor requires; it tolerates null.
        let events = unsafe { AbiInputEvents::new(in_events) };
        let Ok(params) = state.instance.params() else {
            return;
        };
        for index in 0..events.len() {
            if let Some(DauxEvent::ParamValue(event)) = events.get(index) {
                if let Some(param) = params.param(ParamId::new(event.param_id)) {
                    param.set_plain(event.value);
                }
            }
        }
    };
    // SAFETY: forwarded verbatim from this function's own contract.
    unsafe { with_instance(p, body) }
}

/// Runs `body` with the parameter `id` names, or `None` when there is no such parameter.
fn with_param<R>(
    state: &mut AxtInstance,
    id: u32,
    body: impl FnOnce(&dyn Param) -> R,
) -> Option<R> {
    let params = state.instance.params().ok()?;
    let param = params.param(ParamId::new(id))?;
    Some(body(param))
}

/// Copies one parameter description into its ABI form.
fn write_info(info: &ParamInfo, out: &mut DauxParamInfoV1) {
    *out = DauxParamInfoV1::new();
    out.id = info.id.get();
    out.flags = info.flags.bits();
    out.step_count = info.step_count;
    out.name = daux_abi::DauxName::new(&info.name);
    out.group = daux_abi::DauxName::new(&info.group);
    out.unit = daux_abi::DauxName::new(&info.unit);
    out.min_value = info.min;
    out.max_value = info.max;
    out.default_value = info.default;
    // The cookie is a plug-in-private accelerator. This adapter looks parameters up by id, so
    // there is nothing to accelerate and a null cookie is the honest answer (abi-v1 §11.2).
}

/// The `daux.params/1` table.
pub(crate) static TABLE: DauxParamsApiV1 = DauxParamsApiV1 {
    size: DauxParamsApiV1::SIZE,
    _pad0: 0,
    count,
    get_info,
    get_value,
    value_to_text,
    text_to_value,
    flush,
    reserved: [0; 4],
};
