//! The plug-in-side extensions of `abi-v1` §11, as safe host-side handles.
//!
//! Every extension is optional. `get_extension` returning null means "not supported" and is
//! a normal answer, so each accessor on [`LoadedPlugin`](crate::LoadedPlugin) returns an
//! `Option` and a host that finds `None` must carry on without the feature.
//!
//! A table that *is* returned is still untrusted: it is validated with
//! [`crate::probe::read_table`] and copied into host memory before a single entry is
//! called, so an undersized table or a null non-optional entry is refused rather than
//! jumped through.

use core::ffi::c_void;
use core::marker::PhantomData;
use core::mem::offset_of;

use daux_abi::{
    DAUX_ERR_IO, DAUX_FALSE, DAUX_OK, DauxGuiApiV1, DauxParamInfoV1, DauxParamsApiV1,
    DauxPluginHandle, DauxStateApiV1, DauxStatus, DauxStrView, DauxStreamV1, DauxText,
    DauxWindowV1,
};
use daux_parameter::{ParamFlags, ParamId, ParamInfo};

use crate::error::{RuntimeError, RuntimeErrorKind, RuntimeResult};
use crate::events::EventList;
use crate::probe::RequiredFn;

/// Largest state blob this host will accept from one `save`.
///
/// Matches `daux-state`'s own default limit. A module that keeps writing past it is either
/// broken or hostile, and either way the host must stop rather than grow a `Vec` until the
/// process dies.
pub const MAX_STATE_BYTES: usize = 64 * 1024 * 1024;

/// Non-optional entries of `daux.params/1`.
pub(crate) const PARAMS_REQUIRED: &[RequiredFn] = &[
    (offset_of!(DauxParamsApiV1, count), "count"),
    (offset_of!(DauxParamsApiV1, get_info), "get_info"),
    (offset_of!(DauxParamsApiV1, get_value), "get_value"),
    (offset_of!(DauxParamsApiV1, value_to_text), "value_to_text"),
    (offset_of!(DauxParamsApiV1, text_to_value), "text_to_value"),
    (offset_of!(DauxParamsApiV1, flush), "flush"),
];

/// Non-optional entries of `daux.state/1`.
pub(crate) const STATE_REQUIRED: &[RequiredFn] = &[
    (offset_of!(DauxStateApiV1, save), "save"),
    (offset_of!(DauxStateApiV1, load), "load"),
];

/// Non-optional entries of `daux.gui/1`. `set_scale` and `adjust_size` are `Option` in the
/// ABI and are deliberately absent.
pub(crate) const GUI_REQUIRED: &[RequiredFn] = &[
    (
        offset_of!(DauxGuiApiV1, is_api_supported),
        "is_api_supported",
    ),
    (offset_of!(DauxGuiApiV1, create), "create"),
    (offset_of!(DauxGuiApiV1, destroy), "destroy"),
    (offset_of!(DauxGuiApiV1, get_size), "get_size"),
    (offset_of!(DauxGuiApiV1, can_resize), "can_resize"),
    (offset_of!(DauxGuiApiV1, set_size), "set_size"),
    (offset_of!(DauxGuiApiV1, set_parent), "set_parent"),
    (offset_of!(DauxGuiApiV1, show), "show"),
    (offset_of!(DauxGuiApiV1, hide), "hide"),
];

/// Turns a status a module returned into a result.
fn check(what: &str, status: DauxStatus) -> RuntimeResult<()> {
    if status.0 == DAUX_OK.0 {
        Ok(())
    } else {
        Err(RuntimeError::from_status(what, status.0))
    }
}

/// The plug-in's parameter model. [main-thread]
///
/// Values crossing the ABI are always **plain** (real-world) values, never normalised
/// (`abi-v1` §11.2), so nothing here divides by a range.
#[derive(Debug)]
pub struct ParamsExt<'a> {
    handle: DauxPluginHandle,
    api: DauxParamsApiV1,
    /// Ties the handle to the instance it came from, so the extension cannot outlive it.
    plugin: PhantomData<&'a ()>,
}

impl<'a> ParamsExt<'a> {
    pub(crate) const fn new(handle: DauxPluginHandle, api: DauxParamsApiV1) -> Self {
        Self {
            handle,
            api,
            plugin: PhantomData,
        }
    }

    /// How many parameters the plug-in publishes. [main-thread]
    #[must_use]
    pub fn count(&self) -> u32 {
        // SAFETY: `api` is a validated copy of the table the instance published, and
        // `handle` is that instance's own handle, which is alive for `'a`.
        unsafe { (self.api.count)(self.handle) }
    }

    /// The parameter at `index`. [main-thread]
    ///
    /// # Errors
    ///
    /// Whatever status the module returned, and
    /// [`RuntimeErrorKind::Protocol`](crate::RuntimeErrorKind::Protocol) when it reports
    /// success but writes a record shorter than the v1.0 minimum.
    pub fn info(&self, index: u32) -> RuntimeResult<ParamInfo> {
        let mut raw = DauxParamInfoV1::new();
        // SAFETY: as in `count`. `raw` is a host-owned, fully initialised structure with
        // its `size` set, exactly as `abi-v1` §16.2 requires of a caller-owned out-buffer;
        // the module fills it and no allocation crosses the boundary.
        let status = unsafe { (self.api.get_info)(self.handle, index, &raw mut raw) };
        check("daux.params/1::get_info", status)?;
        if (raw.size as usize) < DauxParamInfoV1::MIN_SIZE_V1_0 {
            return Err(RuntimeError::abi(format!(
                "daux.params/1::get_info wrote size {} for parameter {index}, below the v1.0 \
                 minimum of {}",
                raw.size,
                DauxParamInfoV1::MIN_SIZE_V1_0
            )));
        }
        Ok(ParamInfo {
            id: ParamId(raw.id),
            name: raw.name.as_str().to_owned(),
            group: raw.group.as_str().to_owned(),
            unit: raw.unit.as_str().to_owned(),
            flags: ParamFlags::from_bits_truncate(raw.flags),
            step_count: raw.step_count,
            min: raw.min_value,
            max: raw.max_value,
            default: raw.default_value,
        })
    }

    /// Every parameter, in publication order. [main-thread] — allocates.
    ///
    /// # Errors
    ///
    /// As [`ParamsExt::info`], for the first parameter that fails.
    pub fn all(&self) -> RuntimeResult<Vec<ParamInfo>> {
        let count = self.count();
        let mut out = Vec::with_capacity(count as usize);
        for index in 0..count {
            out.push(self.info(index)?);
        }
        Ok(out)
    }

    /// The current plain value of `id`. [main-thread]
    ///
    /// # Errors
    ///
    /// Whatever status the module returned; `DAUX_ERR_NOT_FOUND` for an unknown id becomes
    /// [`RuntimeErrorKind::NotFound`](crate::RuntimeErrorKind::NotFound).
    pub fn value(&self, id: ParamId) -> RuntimeResult<f64> {
        let mut value = 0.0f64;
        // SAFETY: as in `info`; `value` is a host-owned `f64` the module writes through.
        let status = unsafe { (self.api.get_value)(self.handle, id.0, &raw mut value) };
        check("daux.params/1::get_value", status)?;
        Ok(value)
    }

    /// Formats `value` the way the plug-in displays it. [main-thread] — allocates.
    ///
    /// # Errors
    ///
    /// Whatever status the module returned.
    pub fn value_to_text(&self, id: ParamId, value: f64) -> RuntimeResult<String> {
        let mut text = DauxText::empty();
        // SAFETY: as in `info`. `DauxText` is a caller-owned fixed buffer of
        // `DAUX_TEXT_SIZE` bytes, which is the capacity `abi-v1` §11.2 specifies, so the
        // module cannot write past it however long its formatting is.
        let status = unsafe { (self.api.value_to_text)(self.handle, id.0, value, &raw mut text) };
        check("daux.params/1::value_to_text", status)?;
        Ok(text.as_str().to_owned())
    }

    /// Parses `text` into a plain value the way the plug-in would. [main-thread]
    ///
    /// # Errors
    ///
    /// Whatever status the module returned; a value the plug-in cannot parse is normally
    /// `DAUX_ERR_INVALID_ARG`.
    pub fn text_to_value(&self, id: ParamId, text: &str) -> RuntimeResult<f64> {
        let mut value = 0.0f64;
        // SAFETY: as in `info`. The `DauxStrView` borrows `text`, which outlives the call,
        // and `abi-v1` §2 makes that borrow valid for exactly the call's duration.
        let status = unsafe {
            (self.api.text_to_value)(
                self.handle,
                id.0,
                DauxStrView::from_str(text),
                &raw mut value,
            )
        };
        check("daux.params/1::text_to_value", status)?;
        Ok(value)
    }

    /// Applies queued parameter events while the plug-in is not processing.
    /// [main-thread when inactive, audio-thread otherwise]
    ///
    /// The counterpart to `process` for automation that arrives while the plug-in is idle:
    /// `input` is drained by the plug-in, and anything it answers with lands in `output`.
    pub fn flush(&self, input: &mut EventList, output: &mut EventList) {
        let in_list = input.as_abi();
        let out_list = output.as_abi();
        // SAFETY: as in `count`. Both lists are built from live, exclusively borrowed
        // `EventList`s and are passed by pointer to values on this stack frame, so every
        // pointer inside them is valid for exactly the duration of the call — the same
        // lifetime `abi-v1` §16.3 gives the lists inside `process`.
        unsafe {
            (self.api.flush)(self.handle, &raw const in_list, &raw const out_list);
        }
    }
}

/// The plug-in's save/load. [main-thread]
///
/// The host owns the stream and therefore the allocation: nothing crosses the boundary
/// except bytes, in a buffer the caller provides (`abi-v1` §16.2).
#[derive(Debug)]
pub struct StateExt<'a> {
    handle: DauxPluginHandle,
    api: DauxStateApiV1,
    plugin: PhantomData<&'a ()>,
}

/// Context behind a write-only [`DauxStreamV1`].
struct WriteStream {
    buffer: Vec<u8>,
    limit: usize,
}

/// Context behind a read-only [`DauxStreamV1`].
struct ReadStream<'a> {
    bytes: &'a [u8],
    position: usize,
}

unsafe extern "C" fn stream_write(ctx: *mut c_void, buf: *const u8, len: usize) -> isize {
    if ctx.is_null() || (buf.is_null() && len != 0) {
        return DAUX_ERR_IO.0 as isize;
    }
    // SAFETY: `ctx` is the pointer `StateExt::save` put in the stream and addresses a
    // `WriteStream` it exclusively owns for the duration of the `save` call. `abi-v1` §15
    // gives one instance no concurrent main-thread calls, so no second reference exists.
    let stream = unsafe { &mut *ctx.cast::<WriteStream>() };
    if len == 0 {
        return 0;
    }
    if stream.buffer.len().saturating_add(len) > stream.limit {
        return DAUX_ERR_IO.0 as isize;
    }
    // SAFETY: `abi-v1` §11.3 requires the caller of `write` to pass `len` readable bytes at
    // `buf`; the pointer was just checked non-null and the borrow ends in this call.
    let source = unsafe { core::slice::from_raw_parts(buf, len) };
    stream.buffer.extend_from_slice(source);
    // `len` is bounded by `limit`, which is far below `isize::MAX`.
    isize::try_from(len).unwrap_or(isize::MAX)
}

unsafe extern "C" fn stream_read(ctx: *mut c_void, buf: *mut u8, len: usize) -> isize {
    if ctx.is_null() || (buf.is_null() && len != 0) {
        return DAUX_ERR_IO.0 as isize;
    }
    // SAFETY: as in `stream_write`, for the `ReadStream` behind a `load` call.
    let stream = unsafe { &mut *ctx.cast::<ReadStream<'_>>() };
    let remaining = stream.bytes.len() - stream.position;
    let take = remaining.min(len);
    if take == 0 {
        // A short read means end of stream, which is `0`, not an error.
        return 0;
    }
    // SAFETY: `abi-v1` §11.3 requires `buf` to address `len` writable bytes, and `take` is
    // at most `len`. The source is a slice of the caller's own blob, so the regions cannot
    // overlap.
    unsafe {
        core::ptr::copy_nonoverlapping(stream.bytes.as_ptr().add(stream.position), buf, take);
    }
    stream.position += take;
    isize::try_from(take).unwrap_or(isize::MAX)
}

impl<'a> StateExt<'a> {
    pub(crate) const fn new(handle: DauxPluginHandle, api: DauxStateApiV1) -> Self {
        Self {
            handle,
            api,
            plugin: PhantomData,
        }
    }

    /// Reads the plug-in's state into a host-owned buffer. [main-thread] — allocates.
    ///
    /// # Errors
    ///
    /// Whatever status the module returned. A module that writes more than
    /// [`MAX_STATE_BYTES`] gets `DAUX_ERR_IO` from the stream and normally reports a
    /// failure of its own in turn.
    pub fn save(&self) -> RuntimeResult<Vec<u8>> {
        let mut context = WriteStream {
            buffer: Vec::new(),
            limit: MAX_STATE_BYTES,
        };
        let mut stream = DauxStreamV1::new();
        stream.ctx = (&raw mut context).cast::<c_void>();
        stream.write = Some(stream_write);

        // SAFETY: `api` is a validated copy of the table this instance published and
        // `handle` is its handle. The stream and its context are host-owned values on this
        // stack frame, alive for the whole call and borrowed by nothing else.
        let status = unsafe { (self.api.save)(self.handle, &raw const stream) };
        check("daux.state/1::save", status)?;
        Ok(context.buffer)
    }

    /// Restores the plug-in's state from `bytes`. [main-thread]
    ///
    /// `abi-v1` §12 makes `load` atomic from the host's point of view: a module that
    /// cannot read this schema version must fail with no side effects.
    ///
    /// # Errors
    ///
    /// Whatever status the module returned; `DAUX_ERR_VERSION` for a blob from a build the
    /// plug-in cannot read.
    pub fn load(&self, bytes: &[u8]) -> RuntimeResult<()> {
        let mut context = ReadStream { bytes, position: 0 };
        let mut stream = DauxStreamV1::new();
        stream.ctx = (&raw mut context).cast::<c_void>();
        stream.read = Some(stream_read);

        // SAFETY: as in `save`; the context borrows `bytes`, which outlives the call.
        let status = unsafe { (self.api.load)(self.handle, &raw const stream) };
        check("daux.state/1::load", status)
    }
}

/// The plug-in's editor. [main-thread] — every call, without exception (`abi-v1` §11.4).
///
/// The editor's lifetime is independent of the processor's: it may be created and destroyed
/// many times while the DSP keeps running, and destroying it must never touch DSP state.
#[derive(Debug)]
pub struct GuiExt<'a> {
    handle: DauxPluginHandle,
    api: DauxGuiApiV1,
    plugin: PhantomData<&'a ()>,
}

impl<'a> GuiExt<'a> {
    pub(crate) const fn new(handle: DauxPluginHandle, api: DauxGuiApiV1) -> Self {
        Self {
            handle,
            api,
            plugin: PhantomData,
        }
    }

    /// Whether the plug-in can host an editor for `api` in the requested mode.
    /// [main-thread]
    ///
    /// `api` is one of the `DAUX_WINDOW_API_*` constants.
    #[must_use]
    pub fn is_api_supported(&self, api: u32, floating: bool) -> bool {
        // SAFETY: `self.api` is a validated copy of the table this instance published and
        // `handle` is its handle, alive for `'a`.
        let answer =
            unsafe { (self.api.is_api_supported)(self.handle, api, bool_to_abi(floating)) };
        answer != DAUX_FALSE
    }

    /// Creates the editor. [main-thread]
    ///
    /// # Errors
    ///
    /// Whatever status the module returned.
    pub fn create(&self, api: u32, floating: bool) -> RuntimeResult<()> {
        // SAFETY: as in `is_api_supported`.
        let status = unsafe { (self.api.create)(self.handle, api, bool_to_abi(floating)) };
        check("daux.gui/1::create", status)
    }

    /// Destroys the editor. The DSP side is untouched. [main-thread]
    pub fn destroy(&self) {
        // SAFETY: as in `is_api_supported`.
        unsafe { (self.api.destroy)(self.handle) }
    }

    /// Reports the HiDPI scale factor. [main-thread]
    ///
    /// # Errors
    ///
    /// [`RuntimeErrorKind::Unsupported`](crate::RuntimeErrorKind::Unsupported) when the
    /// plug-in leaves the entry null, which `abi-v1` §11.4 permits.
    pub fn set_scale(&self, scale: f64) -> RuntimeResult<()> {
        let entry = self.api.set_scale.ok_or_else(|| {
            RuntimeError::new(
                RuntimeErrorKind::Unsupported,
                "daux.gui/1::set_scale is not implemented by this plug-in",
            )
        })?;
        // SAFETY: as in `is_api_supported`; the entry was just checked non-null.
        check("daux.gui/1::set_scale", unsafe {
            entry(self.handle, scale)
        })
    }

    /// The editor size in physical pixels. [main-thread]
    ///
    /// # Errors
    ///
    /// Whatever status the module returned.
    pub fn size(&self) -> RuntimeResult<(u32, u32)> {
        let (mut width, mut height) = (0u32, 0u32);
        // SAFETY: as in `is_api_supported`; both out-parameters are host-owned locals.
        let status = unsafe { (self.api.get_size)(self.handle, &raw mut width, &raw mut height) };
        check("daux.gui/1::get_size", status)?;
        Ok((width, height))
    }

    /// Whether the host may resize the editor. [main-thread]
    #[must_use]
    pub fn can_resize(&self) -> bool {
        // SAFETY: as in `is_api_supported`.
        unsafe { (self.api.can_resize)(self.handle) != DAUX_FALSE }
    }

    /// Rounds a proposed size to one the editor accepts. [main-thread]
    ///
    /// Returns the proposal unchanged when the plug-in leaves the entry null, which
    /// `abi-v1` §11.4 defines as "any size is accepted".
    ///
    /// # Errors
    ///
    /// Whatever status the module returned.
    pub fn adjust_size(&self, width: u32, height: u32) -> RuntimeResult<(u32, u32)> {
        let Some(entry) = self.api.adjust_size else {
            return Ok((width, height));
        };
        let (mut width, mut height) = (width, height);
        // SAFETY: as in `size`; the entry was just checked non-null.
        let status = unsafe { entry(self.handle, &raw mut width, &raw mut height) };
        check("daux.gui/1::adjust_size", status)?;
        Ok((width, height))
    }

    /// Applies a new editor size in physical pixels. [main-thread]
    ///
    /// # Errors
    ///
    /// Whatever status the module returned.
    pub fn set_size(&self, width: u32, height: u32) -> RuntimeResult<()> {
        // SAFETY: as in `is_api_supported`.
        check("daux.gui/1::set_size", unsafe {
            (self.api.set_size)(self.handle, width, height)
        })
    }

    /// Embeds the editor in the host's window. [main-thread]
    ///
    /// # Errors
    ///
    /// Whatever status the module returned.
    ///
    /// # Safety
    ///
    /// `window` must describe a live native window of the API it names, owned by the host
    /// and kept alive until the editor is destroyed. Nothing on this side of the boundary
    /// can check that a raw `HWND` or `NSView*` is real.
    pub unsafe fn set_parent(&self, window: &DauxWindowV1) -> RuntimeResult<()> {
        // SAFETY: `window` is a live host-owned value for the duration of the call, and the
        // caller guarantees the native handle inside it is valid.
        check("daux.gui/1::set_parent", unsafe {
            (self.api.set_parent)(self.handle, &raw const *window)
        })
    }

    /// Makes the editor visible. [main-thread]
    ///
    /// # Errors
    ///
    /// Whatever status the module returned.
    pub fn show(&self) -> RuntimeResult<()> {
        // SAFETY: as in `is_api_supported`.
        check("daux.gui/1::show", unsafe { (self.api.show)(self.handle) })
    }

    /// Hides the editor without destroying it. [main-thread]
    ///
    /// # Errors
    ///
    /// Whatever status the module returned.
    pub fn hide(&self) -> RuntimeResult<()> {
        // SAFETY: as in `is_api_supported`.
        check("daux.gui/1::hide", unsafe { (self.api.hide)(self.handle) })
    }
}

/// `abi-v1` §2: a producer writes exactly 0 or 1.
const fn bool_to_abi(value: bool) -> daux_abi::DauxBool {
    if value {
        daux_abi::DAUX_TRUE
    } else {
        DAUX_FALSE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stream callbacks are the one part of the extensions that runs *host* code on
    /// behalf of the module, so they are exercised directly with the shapes a module can
    /// legitimately and illegitimately produce.
    #[test]
    fn the_write_stream_accumulates_and_bounds() {
        let mut context = WriteStream {
            buffer: Vec::new(),
            limit: 8,
        };
        let ctx = (&raw mut context).cast::<c_void>();
        let data = [1u8, 2, 3, 4];

        // SAFETY: `ctx` addresses the live `context`, and `data` is four readable bytes.
        assert_eq!(unsafe { stream_write(ctx, data.as_ptr(), 4) }, 4);
        // SAFETY: as above.
        assert_eq!(unsafe { stream_write(ctx, data.as_ptr(), 4) }, 4);
        // Nine bytes would exceed the limit of eight.
        // SAFETY: as above.
        assert!(unsafe { stream_write(ctx, data.as_ptr(), 1) } < 0);
        assert_eq!(context.buffer, [1, 2, 3, 4, 1, 2, 3, 4]);

        // A zero-length write is legal and writes nothing.
        // SAFETY: `len == 0` makes the buffer pointer irrelevant, which the callback checks.
        assert_eq!(unsafe { stream_write(ctx, core::ptr::null(), 0) }, 0);
        // A null context or a null buffer with a real length is a module bug, not a crash.
        // SAFETY: both shapes are explicitly handled before any dereference.
        assert!(unsafe { stream_write(core::ptr::null_mut(), data.as_ptr(), 1) } < 0);
        // SAFETY: as above.
        assert!(unsafe { stream_write(ctx, core::ptr::null(), 4) } < 0);
    }

    #[test]
    fn the_read_stream_reports_end_of_stream_as_a_short_read() {
        let blob = [9u8, 8, 7, 6, 5];
        let mut context = ReadStream {
            bytes: &blob,
            position: 0,
        };
        let ctx = (&raw mut context).cast::<c_void>();
        let mut out = [0u8; 4];

        // SAFETY: `ctx` addresses the live `context` and `out` is four writable bytes.
        assert_eq!(unsafe { stream_read(ctx, out.as_mut_ptr(), 4) }, 4);
        assert_eq!(out, [9, 8, 7, 6]);
        // Only one byte left: a short read, not an error.
        // SAFETY: as above.
        assert_eq!(unsafe { stream_read(ctx, out.as_mut_ptr(), 4) }, 1);
        assert_eq!(out[0], 5);
        // And then end of stream, forever.
        // SAFETY: as above.
        assert_eq!(unsafe { stream_read(ctx, out.as_mut_ptr(), 4) }, 0);
        // SAFETY: as above.
        assert_eq!(unsafe { stream_read(ctx, out.as_mut_ptr(), 0) }, 0);
        // SAFETY: a null context is handled before any dereference.
        assert!(unsafe { stream_read(core::ptr::null_mut(), out.as_mut_ptr(), 4) } < 0);
    }

    #[test]
    fn an_empty_blob_reads_as_immediate_end_of_stream() {
        let mut context = ReadStream {
            bytes: &[],
            position: 0,
        };
        let ctx = (&raw mut context).cast::<c_void>();
        let mut out = [0u8; 4];
        // SAFETY: `ctx` addresses the live `context`; `out` is writable.
        assert_eq!(unsafe { stream_read(ctx, out.as_mut_ptr(), 4) }, 0);
    }

    #[test]
    fn abi_booleans_are_exactly_zero_or_one() {
        assert_eq!(bool_to_abi(true), daux_abi::DAUX_TRUE);
        assert_eq!(bool_to_abi(false), DAUX_FALSE);
    }
}
