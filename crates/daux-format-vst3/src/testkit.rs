//! Fake hosts and fake plug-ins, so the adapter can be driven exactly as a DAW drives it.
//!
//! Everything here is `#[cfg(test)]`. The point is that the tests call the *generated C entry
//! points* through raw pointers — `GetPluginFactory`, `queryInterface`, `setActive`,
//! `process` — rather than the Rust functions behind them, because the vtable layout, the
//! reference counting and the panic boundary are the parts that can only break at the ABI.
//!
//! The host objects (`VecStream`, `FakeParameterChanges`, `FakeEventList`,
//! `FakeComponentHandler`) are real COM objects with real vtables, built the same way the
//! adapter builds its own. They live on the test's stack and their `release` never frees,
//! which is what lets a test assert on their contents afterwards.

use core::ffi::c_void;
use core::sync::atomic::{AtomicU32, Ordering};

use daux_plugin_api::{
    AudioBuses, BusLayout, DauxController, DauxPlugin, DauxProcessor, DauxResult, ErrorKind,
    EventPortLayout, FloatParam, IntParam, Latency, Param, ParamId, ParamRange, Params,
    PluginDescriptor, ProcessConfig, ProcessContext, ProcessEvents, ProcessStatus, StateReader,
    StateWriter, Tail, Version, editor,
};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::api::{
    Event, IBStreamVtbl, IComponentHandlerVtbl, IEventListVtbl, IParamValueQueueVtbl,
    IParameterChangesVtbl,
};
use crate::com::{TResult, TUid, result};

// ---------------------------------------------------------------------------------------
// IBStream
// ---------------------------------------------------------------------------------------

/// How a [`VecStream`] misbehaves.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StreamMode {
    /// A well-behaved stream.
    Normal,
    /// Every read reports a full buffer and never reaches the end.
    Endless,
    /// Every read and write fails.
    Failing,
    /// Writes are accepted until this many bytes are stored, then refused.
    Capacity(usize),
    /// Writes take at most this many bytes per call.
    Dribble(usize),
}

/// A `Vec`-backed `IBStream`, as a real COM object.
#[repr(C)]
pub struct VecStream {
    vtbl: *const IBStreamVtbl,
    ref_count: AtomicU32,
    data: Vec<u8>,
    position: usize,
    mode: StreamMode,
}

static VEC_STREAM_VTBL: IBStreamVtbl = IBStreamVtbl {
    query_interface: VecStream::query_interface,
    add_ref: VecStream::add_ref,
    release: VecStream::release,
    read: VecStream::read,
    write: VecStream::write,
    seek: VecStream::seek,
    tell: VecStream::tell,
};

impl VecStream {
    /// An empty, well-behaved stream.
    #[must_use]
    pub fn new() -> Self {
        Self::with_mode(StreamMode::Normal)
    }

    /// A stream that never reports the end of its data.
    #[must_use]
    pub fn endless() -> Self {
        Self::with_mode(StreamMode::Endless)
    }

    /// A stream whose every operation fails.
    #[must_use]
    pub fn failing() -> Self {
        Self::with_mode(StreamMode::Failing)
    }

    /// A stream that stops accepting bytes after `limit`.
    #[must_use]
    pub fn with_capacity_limit(limit: usize) -> Self {
        Self::with_mode(StreamMode::Capacity(limit))
    }

    /// A stream that accepts at most `chunk` bytes per write.
    #[must_use]
    pub fn dribbling(chunk: usize) -> Self {
        Self::with_mode(StreamMode::Dribble(chunk))
    }

    /// A stream preloaded with `bytes`, positioned at the start.
    #[must_use]
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        let mut stream = Self::new();
        stream.data = bytes;
        stream
    }

    fn with_mode(mode: StreamMode) -> Self {
        Self {
            vtbl: &raw const VEC_STREAM_VTBL,
            ref_count: AtomicU32::new(1),
            data: Vec::new(),
            position: 0,
            mode,
        }
    }

    /// The COM pointer a host would hand over.
    pub fn as_com(&mut self) -> *mut c_void {
        (&raw mut *self).cast::<c_void>()
    }

    /// What has been written so far.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.data
    }

    /// Recovers `&mut Self` from a `this` pointer.
    ///
    /// # Safety
    ///
    /// `this` must be a pointer previously returned by [`VecStream::as_com`] whose object is
    /// still alive and not otherwise borrowed.
    unsafe fn from_this<'a>(this: *mut c_void) -> &'a mut Self {
        // SAFETY: the caller promises `this` came from `as_com`, so it points at a live
        // `VecStream` with the vtable as its first field. VST3 calls are single-threaded per
        // object, so the exclusive borrow does not alias.
        unsafe { &mut *this.cast::<Self>() }
    }

    unsafe extern "system" fn query_interface(
        this: *mut c_void,
        _iid: *const TUid,
        obj: *mut *mut c_void,
    ) -> TResult {
        if obj.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: `obj` was just checked non-null and the host owns it.
        unsafe { *obj = this };
        result::OK
    }

    unsafe extern "system" fn add_ref(this: *mut c_void) -> u32 {
        // SAFETY: `this` came from `as_com`.
        let me = unsafe { Self::from_this(this) };
        me.ref_count.fetch_add(1, Ordering::AcqRel) + 1
    }

    unsafe extern "system" fn release(this: *mut c_void) -> u32 {
        // SAFETY: `this` came from `as_com`. The object lives on the test's stack, so
        // reaching zero frees nothing; the count is only here to be asserted on.
        let me = unsafe { Self::from_this(this) };
        me.ref_count.fetch_sub(1, Ordering::AcqRel) - 1
    }

    unsafe extern "system" fn read(
        this: *mut c_void,
        buffer: *mut c_void,
        num_bytes: i32,
        num_read: *mut i32,
    ) -> TResult {
        // SAFETY: `this` came from `as_com`.
        let me = unsafe { Self::from_this(this) };
        if me.mode == StreamMode::Failing {
            return result::INTERNAL_ERROR;
        }
        let want = usize::try_from(num_bytes).unwrap_or(0);
        let take = if me.mode == StreamMode::Endless {
            want
        } else {
            want.min(me.data.len().saturating_sub(me.position))
        };
        if take > 0 && !buffer.is_null() {
            let dst = buffer.cast::<u8>();
            for i in 0..take {
                let byte = if me.mode == StreamMode::Endless {
                    0x5A
                } else {
                    me.data[me.position + i]
                };
                // SAFETY: the caller promised `num_bytes` writable bytes at `buffer`, and
                // `take <= want <= num_bytes`.
                unsafe { *dst.add(i) = byte };
            }
        }
        if me.mode != StreamMode::Endless {
            me.position += take;
        }
        if !num_read.is_null() {
            // SAFETY: non-null and owned by the caller.
            unsafe { *num_read = i32::try_from(take).unwrap_or(i32::MAX) };
        }
        result::OK
    }

    unsafe extern "system" fn write(
        this: *mut c_void,
        buffer: *mut c_void,
        num_bytes: i32,
        num_written: *mut i32,
    ) -> TResult {
        // SAFETY: `this` came from `as_com`.
        let me = unsafe { Self::from_this(this) };
        if me.mode == StreamMode::Failing {
            return result::INTERNAL_ERROR;
        }
        let offered = usize::try_from(num_bytes).unwrap_or(0);
        let take = match me.mode {
            StreamMode::Capacity(limit) => offered.min(limit.saturating_sub(me.data.len())),
            StreamMode::Dribble(chunk) => offered.min(chunk),
            _ => offered,
        };
        if take > 0 && !buffer.is_null() {
            let src = buffer.cast::<u8>();
            for i in 0..take {
                // SAFETY: the caller promised `num_bytes` readable bytes at `buffer`.
                me.data.push(unsafe { *src.add(i) });
            }
            me.position = me.data.len();
        }
        if !num_written.is_null() {
            // SAFETY: non-null and owned by the caller.
            unsafe { *num_written = i32::try_from(take).unwrap_or(i32::MAX) };
        }
        result::OK
    }

    unsafe extern "system" fn seek(
        this: *mut c_void,
        pos: i64,
        mode: i32,
        out: *mut i64,
    ) -> TResult {
        // SAFETY: `this` came from `as_com`.
        let me = unsafe { Self::from_this(this) };
        let base = match mode {
            crate::api::seek_mode::CUR => me.position as i64,
            crate::api::seek_mode::END => me.data.len() as i64,
            _ => 0,
        };
        let target = base.saturating_add(pos).clamp(0, me.data.len() as i64);
        me.position = target as usize;
        if !out.is_null() {
            // SAFETY: non-null and owned by the caller.
            unsafe { *out = target };
        }
        result::OK
    }

    unsafe extern "system" fn tell(this: *mut c_void, out: *mut i64) -> TResult {
        // SAFETY: `this` came from `as_com`.
        let me = unsafe { Self::from_this(this) };
        if out.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: non-null and owned by the caller.
        unsafe { *out = me.position as i64 };
        result::OK
    }
}

impl Default for VecStream {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------------------
// IParameterChanges / IParamValueQueue
// ---------------------------------------------------------------------------------------

/// One automation lane: a parameter id and its points.
#[repr(C)]
pub struct FakeParamQueue {
    vtbl: *const IParamValueQueueVtbl,
    /// The parameter this lane belongs to.
    pub id: u32,
    /// `(sample offset, normalised value)` in the order the host queued them.
    pub points: Vec<(i32, f64)>,
}

static PARAM_QUEUE_VTBL: IParamValueQueueVtbl = IParamValueQueueVtbl {
    query_interface: FakeParamQueue::query_interface,
    add_ref: FakeParamQueue::add_ref,
    release: FakeParamQueue::release,
    get_parameter_id: FakeParamQueue::get_parameter_id,
    get_point_count: FakeParamQueue::get_point_count,
    get_point: FakeParamQueue::get_point,
    add_point: FakeParamQueue::add_point,
};

impl FakeParamQueue {
    /// An empty lane for one parameter.
    #[must_use]
    pub fn new(id: u32) -> Self {
        Self {
            vtbl: &raw const PARAM_QUEUE_VTBL,
            id,
            points: Vec::new(),
        }
    }

    /// Appends an automation point.
    #[must_use]
    pub fn with_point(mut self, sample_offset: i32, normalized: f64) -> Self {
        self.points.push((sample_offset, normalized));
        self
    }

    unsafe fn from_this<'a>(this: *mut c_void) -> &'a mut Self {
        // SAFETY: the caller only ever passes a pointer to a live `FakeParamQueue`.
        unsafe { &mut *this.cast::<Self>() }
    }

    unsafe extern "system" fn query_interface(
        this: *mut c_void,
        _iid: *const TUid,
        obj: *mut *mut c_void,
    ) -> TResult {
        if obj.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: checked non-null.
        unsafe { *obj = this };
        result::OK
    }

    unsafe extern "system" fn add_ref(_this: *mut c_void) -> u32 {
        1
    }

    unsafe extern "system" fn release(_this: *mut c_void) -> u32 {
        1
    }

    unsafe extern "system" fn get_parameter_id(this: *mut c_void) -> u32 {
        // SAFETY: a live queue.
        unsafe { Self::from_this(this) }.id
    }

    unsafe extern "system" fn get_point_count(this: *mut c_void) -> i32 {
        // SAFETY: a live queue.
        i32::try_from(unsafe { Self::from_this(this) }.points.len()).unwrap_or(i32::MAX)
    }

    unsafe extern "system" fn get_point(
        this: *mut c_void,
        index: i32,
        sample_offset: *mut i32,
        value: *mut f64,
    ) -> TResult {
        // SAFETY: a live queue.
        let me = unsafe { Self::from_this(this) };
        let Ok(index) = usize::try_from(index) else {
            return result::INVALID_ARGUMENT;
        };
        let Some(&(offset, v)) = me.points.get(index) else {
            return result::INVALID_ARGUMENT;
        };
        if sample_offset.is_null() || value.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: both were just checked non-null and are owned by the caller.
        unsafe {
            *sample_offset = offset;
            *value = v;
        }
        result::OK
    }

    unsafe extern "system" fn add_point(
        this: *mut c_void,
        sample_offset: i32,
        value: f64,
        index: *mut i32,
    ) -> TResult {
        // SAFETY: a live queue.
        let me = unsafe { Self::from_this(this) };
        me.points.push((sample_offset, value));
        if !index.is_null() {
            // SAFETY: checked non-null.
            unsafe { *index = i32::try_from(me.points.len() - 1).unwrap_or(i32::MAX) };
        }
        result::OK
    }
}

/// A host's `IParameterChanges` for one block.
#[repr(C)]
pub struct FakeParameterChanges {
    vtbl: *const IParameterChangesVtbl,
    /// The lanes. Boxed on purpose: `getParameterData` hands the plug-in a pointer *into*
    /// this list, and a bare `Vec<FakeParamQueue>` would move its elements the next time it
    /// grew, leaving the plug-in holding a dangling queue.
    #[allow(clippy::vec_box)]
    pub queues: Vec<Box<FakeParamQueue>>,
}

static PARAMETER_CHANGES_VTBL: IParameterChangesVtbl = IParameterChangesVtbl {
    query_interface: FakeParameterChanges::query_interface,
    add_ref: FakeParameterChanges::add_ref,
    release: FakeParameterChanges::release,
    get_parameter_count: FakeParameterChanges::get_parameter_count,
    get_parameter_data: FakeParameterChanges::get_parameter_data,
    add_parameter_data: FakeParameterChanges::add_parameter_data,
};

impl FakeParameterChanges {
    /// An empty change list.
    #[must_use]
    pub fn new() -> Self {
        Self {
            vtbl: &raw const PARAMETER_CHANGES_VTBL,
            queues: Vec::new(),
        }
    }

    /// Adds a lane.
    #[must_use]
    pub fn with_queue(mut self, queue: FakeParamQueue) -> Self {
        self.queues.push(Box::new(queue));
        self
    }

    /// The COM pointer a host would put in `ProcessData`.
    pub fn as_com(&mut self) -> *mut c_void {
        (&raw mut *self).cast::<c_void>()
    }

    /// The lane for a parameter, if one was created for it.
    #[must_use]
    pub fn queue(&self, id: u32) -> Option<&FakeParamQueue> {
        self.queues.iter().find(|q| q.id == id).map(Box::as_ref)
    }

    unsafe fn from_this<'a>(this: *mut c_void) -> &'a mut Self {
        // SAFETY: the caller only ever passes a pointer from `as_com`.
        unsafe { &mut *this.cast::<Self>() }
    }

    unsafe extern "system" fn query_interface(
        this: *mut c_void,
        _iid: *const TUid,
        obj: *mut *mut c_void,
    ) -> TResult {
        if obj.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: checked non-null.
        unsafe { *obj = this };
        result::OK
    }

    unsafe extern "system" fn add_ref(_this: *mut c_void) -> u32 {
        1
    }

    unsafe extern "system" fn release(_this: *mut c_void) -> u32 {
        1
    }

    unsafe extern "system" fn get_parameter_count(this: *mut c_void) -> i32 {
        // SAFETY: a live change list.
        i32::try_from(unsafe { Self::from_this(this) }.queues.len()).unwrap_or(i32::MAX)
    }

    unsafe extern "system" fn get_parameter_data(this: *mut c_void, index: i32) -> *mut c_void {
        // SAFETY: a live change list.
        let me = unsafe { Self::from_this(this) };
        usize::try_from(index)
            .ok()
            .and_then(|i| me.queues.get_mut(i))
            .map_or(core::ptr::null_mut(), |q| (&raw mut **q).cast::<c_void>())
    }

    unsafe extern "system" fn add_parameter_data(
        this: *mut c_void,
        id: *const u32,
        index: *mut i32,
    ) -> *mut c_void {
        // SAFETY: a live change list.
        let me = unsafe { Self::from_this(this) };
        if id.is_null() {
            return core::ptr::null_mut();
        }
        // SAFETY: checked non-null; VST3 passes a pointer to one `ParamID`.
        let id = unsafe { *id };
        let position = me
            .queues
            .iter()
            .position(|q| q.id == id)
            .unwrap_or_else(|| {
                me.queues.push(Box::new(FakeParamQueue::new(id)));
                me.queues.len() - 1
            });
        if !index.is_null() {
            // SAFETY: checked non-null.
            unsafe { *index = i32::try_from(position).unwrap_or(i32::MAX) };
        }
        (&raw mut *me.queues[position]).cast::<c_void>()
    }
}

impl Default for FakeParameterChanges {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------------------
// IEventList
// ---------------------------------------------------------------------------------------

/// A host's `IEventList` for one block.
#[repr(C)]
pub struct FakeEventList {
    vtbl: *const IEventListVtbl,
    /// The events, in the order the host queued them.
    pub events: Vec<Event>,
}

static EVENT_LIST_VTBL: IEventListVtbl = IEventListVtbl {
    query_interface: FakeEventList::query_interface,
    add_ref: FakeEventList::add_ref,
    release: FakeEventList::release,
    get_event_count: FakeEventList::get_event_count,
    get_event: FakeEventList::get_event,
    add_event: FakeEventList::add_event,
};

impl FakeEventList {
    /// An empty list.
    #[must_use]
    pub fn new() -> Self {
        Self {
            vtbl: &raw const EVENT_LIST_VTBL,
            events: Vec::new(),
        }
    }

    /// Adds an event.
    #[must_use]
    pub fn with(mut self, event: Event) -> Self {
        self.events.push(event);
        self
    }

    /// The COM pointer a host would put in `ProcessData`.
    pub fn as_com(&mut self) -> *mut c_void {
        (&raw mut *self).cast::<c_void>()
    }

    unsafe fn from_this<'a>(this: *mut c_void) -> &'a mut Self {
        // SAFETY: the caller only ever passes a pointer from `as_com`.
        unsafe { &mut *this.cast::<Self>() }
    }

    unsafe extern "system" fn query_interface(
        this: *mut c_void,
        _iid: *const TUid,
        obj: *mut *mut c_void,
    ) -> TResult {
        if obj.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: checked non-null.
        unsafe { *obj = this };
        result::OK
    }

    unsafe extern "system" fn add_ref(_this: *mut c_void) -> u32 {
        1
    }

    unsafe extern "system" fn release(_this: *mut c_void) -> u32 {
        1
    }

    unsafe extern "system" fn get_event_count(this: *mut c_void) -> i32 {
        // SAFETY: a live list.
        i32::try_from(unsafe { Self::from_this(this) }.events.len()).unwrap_or(i32::MAX)
    }

    unsafe extern "system" fn get_event(this: *mut c_void, index: i32, out: *mut Event) -> TResult {
        // SAFETY: a live list.
        let me = unsafe { Self::from_this(this) };
        let Ok(index) = usize::try_from(index) else {
            return result::INVALID_ARGUMENT;
        };
        let Some(event) = me.events.get(index) else {
            return result::INVALID_ARGUMENT;
        };
        if out.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: checked non-null and owned by the caller.
        unsafe { *out = *event };
        result::OK
    }

    unsafe extern "system" fn add_event(this: *mut c_void, event: *mut Event) -> TResult {
        // SAFETY: a live list.
        let me = unsafe { Self::from_this(this) };
        if event.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: checked non-null; the plug-in owns the event for the call.
        me.events.push(unsafe { *event });
        result::OK
    }
}

impl Default for FakeEventList {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------------------
// IComponentHandler
// ---------------------------------------------------------------------------------------

/// What a plug-in told the host through `IComponentHandler`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum HandlerCall {
    /// `beginEdit(id)`.
    Begin(u32),
    /// `performEdit(id, normalised)`.
    Perform(u32, f64),
    /// `endEdit(id)`.
    End(u32),
    /// `restartComponent(flags)`.
    Restart(i32),
}

/// A host's `IComponentHandler` that records everything.
#[repr(C)]
pub struct FakeComponentHandler {
    vtbl: *const IComponentHandlerVtbl,
    ref_count: AtomicU32,
    /// Every call, in order.
    pub calls: Vec<HandlerCall>,
}

static COMPONENT_HANDLER_VTBL: IComponentHandlerVtbl = IComponentHandlerVtbl {
    query_interface: FakeComponentHandler::query_interface,
    add_ref: FakeComponentHandler::add_ref,
    release: FakeComponentHandler::release,
    begin_edit: FakeComponentHandler::begin_edit,
    perform_edit: FakeComponentHandler::perform_edit,
    end_edit: FakeComponentHandler::end_edit,
    restart_component: FakeComponentHandler::restart_component,
};

impl FakeComponentHandler {
    /// A handler that has heard nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self {
            vtbl: &raw const COMPONENT_HANDLER_VTBL,
            ref_count: AtomicU32::new(1),
            calls: Vec::new(),
        }
    }

    /// The COM pointer a host would hand to `setComponentHandler`.
    pub fn as_com(&mut self) -> *mut c_void {
        (&raw mut *self).cast::<c_void>()
    }

    /// How many references the plug-in currently holds.
    #[must_use]
    pub fn ref_count(&self) -> u32 {
        self.ref_count.load(Ordering::Acquire)
    }

    unsafe fn from_this<'a>(this: *mut c_void) -> &'a mut Self {
        // SAFETY: the caller only ever passes a pointer from `as_com`.
        unsafe { &mut *this.cast::<Self>() }
    }

    unsafe extern "system" fn query_interface(
        this: *mut c_void,
        _iid: *const TUid,
        obj: *mut *mut c_void,
    ) -> TResult {
        if obj.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: checked non-null.
        unsafe { *obj = this };
        result::OK
    }

    unsafe extern "system" fn add_ref(this: *mut c_void) -> u32 {
        // SAFETY: a live handler.
        let me = unsafe { Self::from_this(this) };
        me.ref_count.fetch_add(1, Ordering::AcqRel) + 1
    }

    unsafe extern "system" fn release(this: *mut c_void) -> u32 {
        // SAFETY: a live handler; the object lives on the test's stack and is never freed.
        let me = unsafe { Self::from_this(this) };
        me.ref_count.fetch_sub(1, Ordering::AcqRel) - 1
    }

    unsafe extern "system" fn begin_edit(this: *mut c_void, id: u32) -> TResult {
        // SAFETY: a live handler.
        unsafe { Self::from_this(this) }
            .calls
            .push(HandlerCall::Begin(id));
        result::OK
    }

    unsafe extern "system" fn perform_edit(this: *mut c_void, id: u32, value: f64) -> TResult {
        // SAFETY: a live handler.
        unsafe { Self::from_this(this) }
            .calls
            .push(HandlerCall::Perform(id, value));
        result::OK
    }

    unsafe extern "system" fn end_edit(this: *mut c_void, id: u32) -> TResult {
        // SAFETY: a live handler.
        unsafe { Self::from_this(this) }
            .calls
            .push(HandlerCall::End(id));
        result::OK
    }

    unsafe extern "system" fn restart_component(this: *mut c_void, flags: i32) -> TResult {
        // SAFETY: a live handler.
        unsafe { Self::from_this(this) }
            .calls
            .push(HandlerCall::Restart(flags));
        result::OK
    }
}

impl Default for FakeComponentHandler {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------------------
// A plug-in to drive
// ---------------------------------------------------------------------------------------

/// Everything the test plug-in has been asked to do, shared with the test.
#[derive(Debug, Default)]
pub struct Counts {
    /// `prepare` calls.
    pub prepares: AtomicU32,
    /// `activate` calls.
    pub activates: AtomicU32,
    /// `deactivate` calls.
    pub deactivates: AtomicU32,
    /// `reset` calls.
    pub resets: AtomicU32,
    /// `process` calls.
    pub processes: AtomicU32,
    /// `save_state` calls.
    pub saves: AtomicU32,
    /// `load_state` calls.
    pub loads: AtomicU32,
    /// `create_editor` calls.
    pub editors: AtomicU32,
    /// Events the last `process` saw.
    pub events_seen: AtomicU32,
    /// `true` once the plug-in has been dropped.
    pub dropped: AtomicBool,
    /// Makes `process` panic.
    pub panic_in_process: AtomicBool,
    /// Makes `prepare` panic.
    pub panic_in_prepare: AtomicBool,
    /// Makes `process` emit an output parameter change, as a meter or a follower would.
    pub emit_param_output: AtomicBool,
}

impl Counts {
    /// Reads a counter.
    #[must_use]
    pub fn get(counter: &AtomicU32) -> u32 {
        counter.load(Ordering::Acquire)
    }
}

/// The parameters of [`SpyPlugin`]: one of each interesting kind.
pub struct SpyParams {
    /// A linear gain in dB.
    pub gain: FloatParam,
    /// A logarithmic cutoff in Hz — the parameter whose curve a linear adapter gets wrong.
    pub cutoff: FloatParam,
    /// A discrete voice count.
    pub voices: IntParam,
}

impl Default for SpyParams {
    fn default() -> Self {
        Self {
            gain: FloatParam::new(ParamId(1), "Gain", 0.0, ParamRange::linear(-60.0, 12.0))
                .with_unit("dB"),
            cutoff: FloatParam::new(
                ParamId(2),
                "Cutoff",
                1_000.0,
                ParamRange::logarithmic(20.0, 20_000.0),
            )
            .with_unit("Hz"),
            voices: IntParam::new(ParamId(3), "Voices", 8, 1, 16),
        }
    }
}

impl Params for SpyParams {
    fn param_refs(&self) -> Vec<(ParamId, &dyn Param)> {
        vec![
            (ParamId(1), &self.gain),
            (ParamId(2), &self.cutoff),
            (ParamId(3), &self.voices),
        ]
    }

    fn param(&self, id: ParamId) -> Option<&dyn Param> {
        match id.get() {
            1 => Some(&self.gain),
            2 => Some(&self.cutoff),
            3 => Some(&self.voices),
            _ => None,
        }
    }
}

/// A trivial editor, so `createView` has something to hand out.
pub struct SpyEditor {
    /// How many times `open` has been called.
    pub opens: u32,
    /// How many times `close` has been called.
    pub closes: u32,
    /// The last size the host asked for.
    pub last_size: daux_plugin_api::PhysicalSize,
}

impl daux_plugin_api::DauxGraphic for SpyEditor {
    fn descriptor(&self) -> daux_plugin_api::GraphicDescriptor {
        use daux_plugin_api::{
            GraphicCapabilities, GraphicDescriptor, GraphicFramework, GraphicProfile,
            GraphicRenderer, LogicalSize, PresentationMode,
        };
        GraphicDescriptor::resizable(
            GraphicCapabilities::new().with(GraphicProfile::new(
                GraphicFramework::Custom,
                GraphicRenderer::Software,
                PresentationMode::EmbeddedSurface,
            )),
            LogicalSize::new(640.0, 480.0),
            LogicalSize::new(320.0, 240.0),
        )
    }

    fn open(
        &mut self,
        _ctx: &mut daux_plugin_api::GraphicContext<'_>,
    ) -> daux_plugin_api::DauxGraphicResult<()> {
        self.opens += 1;
        Ok(())
    }

    fn resize(
        &mut self,
        size: daux_plugin_api::PhysicalSize,
    ) -> daux_plugin_api::DauxGraphicResult<()> {
        self.last_size = size;
        Ok(())
    }

    fn close(&mut self) {
        self.closes += 1;
    }
}

/// A plug-in that records what it was asked to do and can be told to panic.
pub struct SpyPlugin {
    params: SpyParams,
    counts: Arc<Counts>,
    latency: Latency,
    tail: Tail,
    headless: bool,
}

impl SpyPlugin {
    /// The permanent id every test uses.
    pub const ID: &'static str = "com.example.spy";

    /// A plug-in sharing `counts` with the test.
    #[must_use]
    pub fn new(counts: Arc<Counts>) -> Self {
        Self {
            params: SpyParams::default(),
            counts,
            latency: Latency::Samples(64),
            tail: Tail::Samples(128),
            headless: false,
        }
    }

    /// The same, without an editor.
    #[must_use]
    pub fn headless(counts: Arc<Counts>) -> Self {
        let mut plugin = Self::new(counts);
        plugin.headless = true;
        plugin
    }
}

impl Default for SpyPlugin {
    fn default() -> Self {
        Self::new(Arc::new(Counts::default()))
    }
}

impl Drop for SpyPlugin {
    fn drop(&mut self) {
        self.counts.dropped.store(true, Ordering::Release);
    }
}

impl DauxProcessor for SpyPlugin {
    fn prepare(&mut self, config: &ProcessConfig) -> DauxResult<()> {
        assert!(
            !self.counts.panic_in_prepare.load(Ordering::Acquire),
            "the test asked prepare to panic"
        );
        self.counts.prepares.fetch_add(1, Ordering::AcqRel);
        config.validate()
    }

    fn activate(&mut self) -> DauxResult<()> {
        self.counts.activates.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    fn deactivate(&mut self) {
        self.counts.deactivates.fetch_add(1, Ordering::AcqRel);
    }

    fn reset(&mut self) {
        self.counts.resets.fetch_add(1, Ordering::AcqRel);
    }

    fn process<'a>(
        &mut self,
        _ctx: &ProcessContext<'a>,
        audio: &mut AudioBuses<'a, f32>,
        events: &mut ProcessEvents<'a>,
    ) -> ProcessStatus {
        assert!(
            !self.counts.panic_in_process.load(Ordering::Acquire),
            "the test asked process to panic"
        );
        self.counts.processes.fetch_add(1, Ordering::AcqRel);
        self.counts.events_seen.store(
            u32::try_from(events.len()).unwrap_or(u32::MAX),
            Ordering::Release,
        );

        // Apply parameter events the adapter translated, so a test can prove that VST3
        // automation arrived as a *plain* value.
        for index in 0..events.input().len() {
            if let Some(daux_plugin_api::DauxEvent::ParamValue(e)) = events.input().get(index) {
                if let Some(param) = self.params.param(ParamId(e.param_id)) {
                    param.set_plain(e.value);
                }
            }
        }

        // A DSP-driven parameter change: what a meter, an envelope follower or a learned
        // macro produces, and what has to reach the host's automation lane.
        if self.counts.emit_param_output.load(Ordering::Acquire) {
            let _ = events
                .output()
                .try_push(&daux_plugin_api::DauxEvent::ParamValue(
                    daux_plugin_api::ParamEvent {
                        header: daux_plugin_api::EventHeader::at(3),
                        param_id: 2,
                        value: 20_000.0,
                        ..daux_plugin_api::ParamEvent::default()
                    },
                ));
        }

        // Multiply by the gain so a test can hear whether the block arrived.
        let gain = 10f32.powf(self.params.gain.plain() as f32 / 20.0);
        let input = audio.main_input();
        if let Some(mut output) = audio.main_output() {
            for channel in 0..output.channel_count() {
                let source: Option<&[f32]> = input.as_ref().and_then(|i| i.get_channel(channel));
                let target = output.channel_mut(channel);
                match source {
                    Some(source) => {
                        for (o, i) in target.iter_mut().zip(source) {
                            *o = *i * gain;
                        }
                    }
                    None => target.fill(0.0),
                }
            }
        }
        ProcessStatus::Continue
    }

    fn latency(&self) -> Latency {
        self.latency
    }

    fn tail(&self) -> Tail {
        self.tail
    }
}

impl DauxController for SpyPlugin {
    fn params(&self) -> &dyn Params {
        &self.params
    }

    fn save_state(&self, w: &mut StateWriter) -> DauxResult<()> {
        self.counts.saves.fetch_add(1, Ordering::AcqRel);
        w.put_str("spy.marker", "hello");
        Ok(())
    }

    fn load_state(&mut self, r: &StateReader) -> DauxResult<()> {
        self.counts.loads.fetch_add(1, Ordering::AcqRel);
        if r.opt_str("spy.marker").is_none() {
            return Err(ErrorKind::Io.error("the marker is missing"));
        }
        Ok(())
    }
}

impl DauxPlugin for SpyPlugin {
    fn descriptor() -> PluginDescriptor {
        PluginDescriptor::builder(Self::ID, "Spy")
            .vendor("Example")
            .version(Version::new(2, 3, 4))
            .url("https://example.com")
            .feature("Filter")
            .capabilities(
                daux_plugin_api::Capabilities::NONE
                    .with_audio_effect()
                    .with_has_gui(),
            )
            .build()
            .expect("the spy descriptor is valid")
    }

    fn bus_layout(&self) -> BusLayout {
        BusLayout::stereo_effect()
    }

    fn event_ports(&self) -> EventPortLayout {
        EventPortLayout::instrument()
    }

    fn processor(&mut self) -> &mut dyn DauxProcessor {
        self
    }

    fn controller(&mut self) -> &mut dyn DauxController {
        self
    }

    fn create_editor(&mut self) -> Option<Box<dyn core::any::Any>> {
        if self.headless {
            return None;
        }
        self.counts.editors.fetch_add(1, Ordering::AcqRel);
        editor(SpyEditor {
            opens: 0,
            closes: 0,
            last_size: daux_plugin_api::PhysicalSize::new(0, 0),
        })
    }
}
