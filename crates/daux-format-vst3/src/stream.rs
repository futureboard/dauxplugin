//! Reading and writing state through the host's `IBStream`, and what goes in it.
//!
//! VST3 hands the plug-in a byte stream and asks it to write whatever it needs to be
//! reconstructed. What DAUx writes into it is a `daux-state` document — the same bytes the
//! `.axt` and CLAP exports write — so a preset saved by one export of a plug-in is readable
//! by the others, and so the schema-migration machinery in `daux-state` applies unchanged.
//!
//! # Layout of the blob
//!
//! ```text
//! DAUXST\0\0                      daux-state container header
//!   daux.params/<id> : f64        one plain value per parameter, keyed by permanent id
//!   …                             whatever DauxController::save_state wrote
//! ```
//!
//! Parameters are keyed by their **permanent id** rather than by index, so adding, removing
//! or reordering parameters in a later version cannot shift a saved value onto the wrong
//! control. A value whose id no longer exists is ignored; a parameter whose id is missing
//! from the blob keeps its default. That is what makes a v2 plug-in open a v1 project.
//!
//! # Hostile streams
//!
//! A stream is host-controlled input and is treated as such: reads are bounded by
//! [`MAX_STATE_BYTES`], a stream that never reports EOF is cut off rather than followed, and
//! a truncated or corrupt document produces an error instead of a panic — `daux-state`
//! bounds-checks every field itself.

use core::ffi::c_void;

use daux_plugin_api::{
    DauxError, DauxResult, ErrorKind, Params, PluginInstance, StateReader, StateVersion,
    StateWriter,
};

use crate::api::{IBStreamVtbl, seek_mode};
use crate::com::{TResult, result};
use crate::params::ParamTable;

/// Largest state blob this adapter will read from a host, 64 MiB.
///
/// The same bound `daux-state` applies by default. A plug-in with more state than this is
/// doing something a stream is the wrong tool for; a *host* offering more than this is
/// either broken or hostile, and either way the answer is to stop reading.
pub const MAX_STATE_BYTES: usize = 64 * 1024 * 1024;

/// How much is asked for per `IBStream::read`.
const CHUNK: usize = 64 * 1024;

/// The group every parameter value lives under.
const PARAM_GROUP: &str = "daux.params";

/// `[main-thread]` Reads a host stream to its end.
///
/// # Errors
///
/// [`result::INVALID_ARGUMENT`] for a null stream, [`result::INTERNAL_ERROR`] when the host
/// reports a read failure, and [`result::OUT_OF_MEMORY`] when the stream is longer than
/// [`MAX_STATE_BYTES`].
///
/// # Safety
///
/// `stream` must be null or a live `IBStream` that stays alive for the call.
pub unsafe fn read_all(stream: *mut c_void) -> Result<Vec<u8>, TResult> {
    if stream.is_null() {
        return Err(result::INVALID_ARGUMENT);
    }
    // SAFETY: the caller promises a live COM object, so its first word is a vtable pointer
    // whose layout `IBStreamVtbl` describes. The reference borrows nothing past this line.
    let vtbl = unsafe { *stream.cast::<*const IBStreamVtbl>() };
    if vtbl.is_null() {
        return Err(result::INVALID_ARGUMENT);
    }

    let mut out: Vec<u8> = Vec::new();
    loop {
        let start = out.len();
        if start >= MAX_STATE_BYTES {
            return Err(result::OUT_OF_MEMORY);
        }
        let want = CHUNK.min(MAX_STATE_BYTES - start);
        out.resize(start + want, 0);

        let mut got: i32 = 0;
        // SAFETY: `out[start..]` is `want` initialised, writable bytes we own; `want` fits in
        // an `i32` because `CHUNK` does; `got` is a live local. The host writes at most
        // `want` bytes and reports how many in `got`.
        let status = unsafe {
            ((*vtbl).read)(
                stream,
                out.as_mut_ptr().add(start).cast::<c_void>(),
                i32::try_from(want).unwrap_or(i32::MAX),
                &raw mut got,
            )
        };
        if !result::is_ok(status) {
            return Err(result::INTERNAL_ERROR);
        }
        // A host at EOF may answer `kResultOk` with zero bytes or `kResultFalse`; both mean
        // stop. A negative count is a broken host and is treated as EOF rather than trusted.
        let read = usize::try_from(got).unwrap_or(0).min(want);
        out.truncate(start + read);
        if read == 0 {
            return Ok(out);
        }
    }
}

/// `[main-thread]` Writes every byte of `bytes` to a host stream.
///
/// A short write is retried rather than treated as success: hosts that stream to disk do
/// return short counts, and a state blob that is one byte short is a corrupt preset.
///
/// # Errors
///
/// [`result::INVALID_ARGUMENT`] for a null stream, [`result::INTERNAL_ERROR`] when the host
/// fails or stops accepting bytes.
///
/// # Safety
///
/// As [`read_all`].
pub unsafe fn write_all(stream: *mut c_void, bytes: &[u8]) -> TResult {
    if stream.is_null() {
        return result::INVALID_ARGUMENT;
    }
    // SAFETY: as `read_all` — the caller promises a live `IBStream`.
    let vtbl = unsafe { *stream.cast::<*const IBStreamVtbl>() };
    if vtbl.is_null() {
        return result::INVALID_ARGUMENT;
    }

    let mut written = 0usize;
    while written < bytes.len() {
        let chunk = (bytes.len() - written).min(CHUNK);
        let mut took: i32 = 0;
        // SAFETY: `bytes[written..written + chunk]` is a live, readable slice. VST3's `write`
        // takes a non-const `void*` even though it only reads from it, which is why the cast
        // sheds constness; the host must not write through it and no conforming host does.
        let status = unsafe {
            ((*vtbl).write)(
                stream,
                bytes
                    .as_ptr()
                    .add(written)
                    .cast::<u8>()
                    .cast_mut()
                    .cast::<c_void>(),
                i32::try_from(chunk).unwrap_or(i32::MAX),
                &raw mut took,
            )
        };
        if !result::is_ok(status) {
            return result::INTERNAL_ERROR;
        }
        let took = usize::try_from(took).unwrap_or(0).min(chunk);
        if took == 0 {
            // No progress and no error: the stream is full or broken. Either way, looping
            // for ever is not an option.
            return result::INTERNAL_ERROR;
        }
        written += took;
    }
    result::OK
}

/// `[main-thread]` Rewinds a stream, ignoring hosts that cannot seek.
///
/// VST3 does not promise the cursor is at zero when `setState` is called, and several hosts
/// hand over a stream positioned at the end of the previous plug-in's data.
///
/// # Safety
///
/// As [`read_all`].
pub unsafe fn rewind(stream: *mut c_void) {
    if stream.is_null() {
        return;
    }
    // SAFETY: as `read_all`.
    let vtbl = unsafe { *stream.cast::<*const IBStreamVtbl>() };
    if vtbl.is_null() {
        return;
    }
    let mut position: i64 = 0;
    // SAFETY: `position` is a live local; a host that cannot seek returns an error, which is
    // ignored on purpose — reading from wherever it left the cursor is the best we can do.
    unsafe {
        let _ = ((*vtbl).seek)(stream, 0, seek_mode::SET, &raw mut position);
    }
}

/// `[main-thread]` Serialises a plug-in's parameters and controller state.
///
/// # Errors
///
/// Whatever [`DauxController::save_state`](daux_plugin_api::DauxController::save_state)
/// returned, or [`ErrorKind::Internal`] when the document could not be encoded — an
/// over-long key or a state larger than `daux-state`'s limits.
pub fn save(instance: &mut PluginInstance, table: &ParamTable) -> DauxResult<Vec<u8>> {
    let schema = instance
        .descriptor()
        .map_or(1, |d| d.state_schema_version.max(1));
    let mut writer = StateWriter::new(StateVersion(schema));

    writer.begin_group(PARAM_GROUP);
    let mut key = String::with_capacity(16);
    for entry in table.entries() {
        key.clear();
        use core::fmt::Write as _;
        let _ = write!(key, "{}", entry.vst3_id());
        writer.put_f64(&key, entry.plain());
    }
    writer.end_group();

    instance.save_state(&mut writer)?;
    writer.try_finish().map_err(|e| {
        DauxError::new(
            ErrorKind::Internal,
            format!("state could not be encoded: {e}"),
        )
    })
}

/// `[main-thread]` Restores what [`save`] wrote, into both the plug-in and the mirror.
///
/// Parameters missing from the blob keep their current value, and values whose id the
/// plug-in no longer has are ignored: that is what lets version 2 of a plug-in open a
/// project saved by version 1.
///
/// # Errors
///
/// [`ErrorKind::Io`] when the bytes are not a `daux-state` document, or whatever
/// [`DauxController::load_state`](daux_plugin_api::DauxController::load_state) returned.
pub fn load(instance: &mut PluginInstance, table: &ParamTable, bytes: &[u8]) -> DauxResult<()> {
    let reader = StateReader::from_bytes(bytes)
        .map_err(|e| DauxError::new(ErrorKind::Io, format!("state is not readable: {e}")))?;

    {
        let params = instance.params()?;
        apply_params(&reader, table, params);
    }
    instance.load_state(&reader)?;
    // `load_state` may set parameters itself, so the mirror is refreshed from the truth
    // rather than from what was just read.
    let params = instance.params()?;
    table.refresh_from(params);
    Ok(())
}

/// `[main-thread]` Applies only the parameter values of a state document.
///
/// Split out because VST3's `IEditController::setComponentState` hands the controller half
/// the *component's* blob and expects it to mirror the values without re-running the
/// plug-in's own `load_state`.
pub fn apply_params(reader: &StateReader, table: &ParamTable, params: &dyn Params) {
    let mut path = String::with_capacity(32);
    for entry in table.entries() {
        path.clear();
        use core::fmt::Write as _;
        let _ = write!(path, "{PARAM_GROUP}/{}", entry.vst3_id());
        let Some(plain) = reader.opt_f64(&path) else {
            continue;
        };
        entry.set_normalized(entry.curve.to_normalized(plain));
        if entry.is_read_only() {
            continue;
        }
        if let Some(param) = params.param(entry.id) {
            param.set_plain(plain);
        }
    }
}

/// `[main-thread]` Reads the parameter values of a state document into the mirror only.
///
/// Used by the controller half, which has no plug-in of its own to write through.
pub fn mirror_params(reader: &StateReader, table: &ParamTable) {
    let mut path = String::with_capacity(32);
    for entry in table.entries() {
        path.clear();
        use core::fmt::Write as _;
        let _ = write!(path, "{PARAM_GROUP}/{}", entry.vst3_id());
        if let Some(plain) = reader.opt_f64(&path) {
            entry.set_normalized(entry.curve.to_normalized(plain));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::VecStream;

    #[test]
    fn a_round_trip_through_a_host_stream_preserves_every_byte() {
        let payload: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let mut stream = VecStream::new();
        // SAFETY: `stream.as_com()` hands out a live `IBStream` that outlives the calls.
        unsafe {
            assert_eq!(write_all(stream.as_com(), &payload), result::OK);
            rewind(stream.as_com());
            let back = read_all(stream.as_com()).expect("the stream reads back");
            assert_eq!(back, payload);
        }
    }

    #[test]
    fn an_empty_stream_reads_as_an_empty_blob() {
        let mut stream = VecStream::new();
        // SAFETY: a live stream.
        let back = unsafe { read_all(stream.as_com()) }.expect("an empty stream is not an error");
        assert!(back.is_empty());
    }

    #[test]
    fn a_null_stream_is_refused_rather_than_dereferenced() {
        // SAFETY: null is checked for before any dereference.
        unsafe {
            assert_eq!(
                read_all(core::ptr::null_mut()),
                Err(result::INVALID_ARGUMENT)
            );
            assert_eq!(
                write_all(core::ptr::null_mut(), b"x"),
                result::INVALID_ARGUMENT
            );
            rewind(core::ptr::null_mut());
        }
    }

    #[test]
    fn a_stream_that_never_ends_is_cut_off_rather_than_followed() {
        let mut stream = VecStream::endless();
        // SAFETY: a live stream that always reports a full read.
        let err = unsafe { read_all(stream.as_com()) }.expect_err("an endless stream must not win");
        assert_eq!(err, result::OUT_OF_MEMORY);
    }

    #[test]
    fn a_failing_stream_reports_an_error_instead_of_a_short_read() {
        let mut stream = VecStream::failing();
        // SAFETY: a live stream whose read/write always fail.
        unsafe {
            assert_eq!(read_all(stream.as_com()), Err(result::INTERNAL_ERROR));
            assert_eq!(write_all(stream.as_com(), b"hello"), result::INTERNAL_ERROR);
        }
    }

    #[test]
    fn a_stream_that_stops_accepting_bytes_does_not_spin() {
        let mut stream = VecStream::with_capacity_limit(4);
        // SAFETY: a live stream that accepts only four bytes.
        let status = unsafe { write_all(stream.as_com(), b"more than four bytes") };
        assert_eq!(status, result::INTERNAL_ERROR);
        assert_eq!(stream.bytes().len(), 4);
    }

    #[test]
    fn short_writes_are_retried_until_everything_is_out() {
        let payload = vec![7u8; 5000];
        let mut stream = VecStream::dribbling(17);
        // SAFETY: a live stream that accepts 17 bytes at a time.
        unsafe {
            assert_eq!(write_all(stream.as_com(), &payload), result::OK);
        }
        assert_eq!(stream.bytes(), payload.as_slice());
    }
}
