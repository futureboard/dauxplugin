//! `daux.state/1` — save and load (abi-v1 §11.3, §12).
//!
//! # What is in a blob
//!
//! ```text
//! DAUXST\0\0  version = Params::state_schema_version()
//!   params/            ← written by this adapter, one f64 per parameter, keyed by id
//!     "1" = -6.0
//!     "2" = 1.0
//!   …                  ← whatever DauxController::save_state wrote, at the top level
//! ```
//!
//! Parameter values are written by the framework rather than by the plug-in — `daux-core` says
//! so, and it is the only way a plug-in gets parameter persistence right by default. The
//! `params` group is therefore **reserved**: a controller that writes a top-level key of that
//! name will lose the argument with itself.
//!
//! Keys are the decimal parameter **id**, never the index or the name. Ids are permanent
//! (abi-v1 §14) and are exactly what survives a plug-in being reordered or renamed. Ids that
//! moved are replayed through [`Params::migrations`](daux_plugin_api::Params::migrations) on
//! load, so a value saved by v1 lands on the renamed parameter in v2.
//!
//! # Loading is atomic
//!
//! abi-v1 §12 requires `load` to be atomic from the host's point of view: a failed load must
//! not leave half a preset applied. Parameter values are snapshotted before anything is
//! written and restored if the controller then refuses the blob, so a rejected preset leaves
//! the instance exactly as it was.

use daux_abi::{
    DAUX_ERR_VERSION, DAUX_OK, DauxPluginHandle, DauxStateApiV1, DauxStatus, DauxStreamV1,
};
use daux_plugin_api::{
    ParamId, StateReader, StateVersion, StateWriter, migrate_param_id, status as core_status,
};

use crate::instance::{AxtInstance, with_instance};
use crate::panic::status_of_error;
use crate::stream;

/// The group every parameter value is written into.
const PARAMS_GROUP: &str = "params";

/// Largest state blob this adapter will pull from a host, matching `daux-state`'s own default
/// limit. A preset is kilobytes; anything near this is a broken or hostile stream.
const MAX_STATE_BYTES: usize = 64 * 1024 * 1024;

/// [main-thread] Writes the instance's state into the host-owned stream.
///
/// # Safety
///
/// `s` is null or points at a [`DauxStreamV1`] valid for the call. See
/// [`with_instance`](crate::instance::with_instance).
unsafe extern "C" fn save(p: DauxPluginHandle, s: *const DauxStreamV1) -> DauxStatus {
    let body = |state: &mut AxtInstance| {
        let bytes = match encode(state) {
            Ok(bytes) => bytes,
            Err(status) => return status,
        };
        // SAFETY: this function's contract guarantees the stream is null or valid for the call,
        // which is exactly what `write_all` requires.
        match unsafe { stream::write_all(s, &bytes) } {
            Ok(()) => DAUX_OK,
            Err(status) => status,
        }
    };
    // SAFETY: forwarded verbatim from this function's own contract.
    unsafe { with_instance(p, body) }
}

/// [main-thread] Restores the instance's state from the host-owned stream.
///
/// # Safety
///
/// As [`save`].
unsafe extern "C" fn load(p: DauxPluginHandle, s: *const DauxStreamV1) -> DauxStatus {
    let body = |state: &mut AxtInstance| {
        // SAFETY: this function's contract guarantees the stream is null or valid for the call,
        // which is exactly what `read_all` requires.
        let bytes = match unsafe { stream::read_all(s, MAX_STATE_BYTES) } {
            Ok(bytes) => bytes,
            Err(status) => return status,
        };
        decode(state, &bytes)
    };
    // SAFETY: forwarded verbatim from this function's own contract.
    unsafe { with_instance(p, body) }
}

/// Serialises parameters and then whatever the controller adds.
fn encode(state: &mut AxtInstance) -> Result<Vec<u8>, DauxStatus> {
    let schema = match state.instance.params() {
        Ok(params) => params.state_schema_version(),
        Err(err) => return Err(status_of_error(&err)),
    };
    let mut writer = StateWriter::new(StateVersion(schema));

    writer.begin_group(PARAMS_GROUP);
    match state.instance.params() {
        Ok(params) => {
            for (id, param) in params.param_refs() {
                writer.put_f64(&id.get().to_string(), param.plain());
            }
        }
        Err(err) => return Err(status_of_error(&err)),
    }
    writer.end_group();

    if let Err(err) = state.instance.save_state(&mut writer) {
        return Err(status_of_error(&err));
    }
    writer
        .try_finish()
        .map_err(|_| DauxStatus::from_raw(core_status::IO))
}

/// Applies a blob, atomically as far as parameters are concerned.
fn decode(state: &mut AxtInstance, bytes: &[u8]) -> DauxStatus {
    let reader = match StateReader::from_bytes(bytes) {
        Ok(reader) => reader,
        // A blob this build cannot parse at all is a version or corruption problem, and the
        // host must be told rather than left believing the preset loaded.
        Err(_) => return DauxStatus::from_raw(core_status::INVALID_ARG),
    };

    let params = match state.instance.params() {
        Ok(params) => params,
        Err(err) => return status_of_error(&err),
    };
    if reader.version().0 > params.state_schema_version() {
        // A blob written by a *newer* version of this plug-in. abi-v1 §12 says refuse with no
        // side effects rather than guess.
        return DAUX_ERR_VERSION;
    }

    // Snapshot first: everything below can still fail, and a half-applied preset is a bug.
    let snapshot: Vec<(ParamId, f64)> = params
        .param_refs()
        .into_iter()
        .map(|(id, param)| (id, param.plain()))
        .collect();

    if let Some(group) = reader.opt_group(PARAMS_GROUP) {
        let migrations = params.migrations();
        for key in group.keys() {
            let Ok(saved_id) = key.parse::<u32>() else {
                // Not a parameter id: a future revision may put something else in here, and
                // ignoring it is what keeps this loadable by an older build.
                continue;
            };
            let Some(value) = group.opt_f64(key) else {
                continue;
            };
            // A parameter that was renamed lands on its new id; one that was removed is
            // dropped (abi-v1 §14 forbids reusing its id, so nothing else can claim it).
            let Some(id) = migrate_param_id(migrations, ParamId::new(saved_id)) else {
                continue;
            };
            if let Some(param) = params.param(id) {
                param.set_plain(value);
            }
        }
    }

    if let Err(err) = state.instance.load_state(&reader) {
        // Put every parameter back exactly as it was: from the host's point of view the load
        // never happened (abi-v1 §12).
        if let Ok(params) = state.instance.params() {
            for (id, value) in snapshot {
                if let Some(param) = params.param(id) {
                    param.set_plain(value);
                }
            }
        }
        return status_of_error(&err);
    }
    DAUX_OK
}

/// The `daux.state/1` table.
pub(crate) static TABLE: DauxStateApiV1 = DauxStateApiV1 {
    size: DauxStateApiV1::SIZE,
    _pad0: 0,
    save,
    load,
    reserved: [0; 4],
};
