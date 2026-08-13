//! The two kinds of plug-in a harness can drive, behind one interface.
//!
//! * **Native** — a `Box<dyn DauxPlugin>` compiled into the same binary as the test. No ABI,
//!   no bundle, no `dlopen`: the plug-in's own Rust types are called directly. This is what
//!   makes a plug-in testable in `cargo test`, before it has ever been built as an `.axt`.
//! * **Loaded** — an `.axt` on disk, opened through `daux-runtime` and driven over the C
//!   ABI, exactly as a DAW drives it.
//!
//! Both are driven through the same `TestHost` calls, and the difference is deliberately
//! visible in one place only: [`Instance::is_native`]. A test that passes against the native
//! instance and fails against the loaded one has found a bug in the adapter, which is
//! precisely the comparison worth being able to make.

use daux_abi::{DAUX_EVENT_NOTE_OFF, DAUX_EVENT_NOTE_ON, DauxEventNoteV1, DauxEventParamV1};
use daux_audio::{AudioBuses, AudioStorage};
use daux_events::{DauxEvent, EventBuffer, EventHeader, NoteEvent, ParamEvent};
use daux_parameter::ParamId;
use daux_runtime::daux_core::daux_state::{StateReader, StateVersion, StateWriter};
use daux_runtime::daux_core::{
    DauxPlugin, Latency, PluginDescriptor, ProcessConfig, ProcessContext, ProcessEvents,
    ProcessStatus, Tail,
};
use daux_runtime::daux_host_services::RtHostServices;
use daux_runtime::{HostBlock, LoadedPlugin};
use daux_transport::Transport;

use crate::error::{HostError, HostErrorKind, HostResult};

/// How many events one block may carry in each direction.
///
/// Preallocated once per instance: a harness that grew its event buffer inside `process`
/// would allocate on the audio thread, which is the one thing a plug-in author uses this
/// crate to prove they do not do.
const EVENT_CAPACITY: usize = 512;

/// Bytes of variable-length event payload (system exclusive, custom events) per block.
const EVENT_BYTES: usize = 16 * 1024;

/// A handle to one plug-in inside a [`TestHost`](crate::TestHost). [any-thread]
///
/// Ids are never reused: unloading an instance leaves its id permanently invalid, so a test
/// that keeps a stale handle gets [`HostErrorKind::NoSuchInstance`] rather than someone
/// else's plug-in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InstanceId(pub(crate) u32);

impl InstanceId {
    /// The raw index, for a caller that wants to print it. [any-thread]
    #[inline]
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl core::fmt::Display for InstanceId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "instance {}", self.0)
    }
}

/// One plug-in the harness is driving.
pub(crate) enum Instance {
    /// Compiled into this binary.
    Native(Box<NativeInstance>),
    /// Loaded from an `.axt` bundle over the C ABI.
    Loaded(Box<LoadedInstance>),
}

impl Instance {
    /// The plug-in's static description. [main-thread]
    pub(crate) fn descriptor(&self) -> &PluginDescriptor {
        match self {
            Self::Native(native) => &native.descriptor,
            Self::Loaded(loaded) => &loaded.descriptor,
        }
    }

    /// Whether this instance is compiled in rather than loaded from a bundle. [any-thread]
    pub(crate) const fn is_native(&self) -> bool {
        matches!(self, Self::Native(_))
    }
}

/// A plug-in compiled into the same binary as the harness.
pub(crate) struct NativeInstance {
    plugin: Box<dyn DauxPlugin>,
    descriptor: PluginDescriptor,
    /// Events the host has queued for the next block.
    input_events: EventBuffer,
    /// Events the plug-in produced during the last block.
    output_events: EventBuffer,
    activated: bool,
}

impl NativeInstance {
    /// Prepares and activates the plug-in for `config`. [main-thread]
    ///
    /// `prepare` is where a plug-in allocates, so it happens here, once, and never again
    /// while the instance is processing.
    pub(crate) fn new(
        plugin: Box<dyn DauxPlugin>,
        descriptor: PluginDescriptor,
        config: &ProcessConfig,
    ) -> HostResult<Self> {
        let mut this = Self {
            plugin,
            descriptor,
            input_events: EventBuffer::with_capacity(EVENT_CAPACITY, EVENT_BYTES),
            output_events: EventBuffer::with_capacity(EVENT_CAPACITY, EVENT_BYTES),
            activated: false,
        };
        this.plugin.processor().prepare(config)?;
        this.plugin.processor().activate()?;
        this.activated = true;
        Ok(this)
    }

    /// The plug-in's parameter model. [main-thread]
    pub(crate) fn param_value(&mut self, id: ParamId) -> Option<f64> {
        self.plugin
            .controller()
            .params()
            .param(id)
            .map(daux_parameter::Param::plain)
    }

    /// Applies a plain value directly to the parameter object. [main-thread]
    pub(crate) fn set_param_value(&mut self, id: ParamId, value: f64) -> bool {
        match self.plugin.controller().params().param(id) {
            Some(param) => {
                param.set_plain(value);
                true
            }
            None => false,
        }
    }

    /// Queues an event for the next block. [main-thread]
    pub(crate) fn queue(&mut self, event: &DauxEvent<'_>) -> bool {
        self.input_events.try_push(event).is_ok()
    }

    /// Runs one block. [audio-thread]
    pub(crate) fn process(
        &mut self,
        config: &ProcessConfig,
        host: &RtHostServices,
        transport: &Transport,
        steady_time: i64,
        input: &AudioStorage<f32>,
        output: &mut AudioStorage<f32>,
    ) -> ProcessStatus {
        let frames = output.frames();
        let context = ProcessContext::new(frames, config, host)
            .with_transport(transport)
            .with_steady_time(steady_time);

        // The events are sorted before the plug-in sees them: `abi-v1` §9 promises a
        // sorted list, and a harness that skipped it would let a plug-in pass here and fail
        // in a DAW.
        self.input_events.sort_by_time();
        self.output_events.clear();

        let input_view = [input.as_ref()];
        // An instrument is handed no input bus at all rather than a zero-channel one: the
        // two are different in `bus_layout`, and a plug-in may legitimately index bus 0.
        let inputs = if input.channel_count() == 0 {
            &input_view[..0]
        } else {
            &input_view[..]
        };
        let mut outputs = [output.as_mut()];
        let mut buses = AudioBuses::new(inputs, &mut outputs, frames);
        let mut events =
            ProcessEvents::new(self.input_events.as_input(), self.output_events.as_output());

        let status = self
            .plugin
            .processor()
            .process(&context, &mut buses, &mut events);

        // `abi-v1` §9 makes sorting the *host's* job for output, since a plug-in may push
        // in any order.
        self.output_events.sort_by_time();
        self.input_events.clear();
        status
    }

    /// Serialises the plug-in's state. [main-thread]
    pub(crate) fn save_state(&mut self) -> HostResult<Vec<u8>> {
        let mut writer = StateWriter::new(StateVersion(self.descriptor.state_schema_version));
        self.plugin.controller().save_state(&mut writer)?;
        Ok(writer.finish())
    }

    /// Restores the plug-in's state. [main-thread]
    pub(crate) fn load_state(&mut self, bytes: &[u8]) -> HostResult<()> {
        let reader = StateReader::from_bytes(bytes).map_err(|error| {
            HostError::new(
                HostErrorKind::Plugin,
                format!("the state blob is not readable: {error}"),
            )
        })?;
        self.plugin.controller().load_state(&reader)?;
        Ok(())
    }

    /// Drains work the plug-in queued with `request_callback`. [main-thread]
    pub(crate) fn on_main_thread(&mut self) {
        self.plugin.controller().on_main_thread();
    }

    /// Runs one scheduled worker task on the main thread. [main-thread]
    pub(crate) fn on_worker(&mut self, task: daux_runtime::daux_host_services::TaskId) {
        self.plugin.controller().on_worker(task);
    }

    /// Clears delay lines and voices. [main-thread]
    pub(crate) fn reset(&mut self) {
        self.plugin.processor().reset();
    }

    /// The plug-in's current latency. [main-thread]
    pub(crate) fn latency(&mut self) -> Latency {
        self.plugin.processor().latency()
    }

    /// The plug-in's current tail. [main-thread]
    pub(crate) fn tail(&mut self) -> Tail {
        self.plugin.processor().tail()
    }

    /// The events the plug-in produced during the last block. [main-thread]
    pub(crate) const fn output_events(&self) -> &EventBuffer {
        &self.output_events
    }
}

impl Drop for NativeInstance {
    fn drop(&mut self) {
        if self.activated {
            self.plugin.processor().deactivate();
        }
    }
}

/// A plug-in loaded from an `.axt` bundle and driven over the C ABI.
pub(crate) struct LoadedInstance {
    plugin: LoadedPlugin,
    descriptor: PluginDescriptor,
    /// Rebound per block; never resized while processing.
    block: HostBlock<'static>,
    /// The input the plug-in actually sees.
    ///
    /// The ABI hands a plug-in `*mut f32` for its inputs too (`abi-v1` §8), so a harness
    /// that pointed those at the caller's `&AudioStorage` would be inviting a plug-in to
    /// write through a shared reference. Copying costs one `memcpy` a block and makes the
    /// harness's own promise — that `input` is not modified — true by construction.
    input_scratch: AudioStorage<f32>,
    input_channels: usize,
    output_channels: usize,
    max_frames: u32,
}

impl LoadedInstance {
    /// Activates the instance and preallocates its block. [main-thread]
    pub(crate) fn new(
        mut plugin: LoadedPlugin,
        descriptor: PluginDescriptor,
        config: &ProcessConfig,
    ) -> HostResult<Self> {
        plugin.activate(config)?;
        plugin.start_processing()?;
        Ok(Self {
            plugin,
            descriptor,
            block: HostBlock::new(&[], &[], config.max_block_size)
                .with_event_capacity(EVENT_CAPACITY, EVENT_BYTES),
            input_scratch: AudioStorage::new(0, 0),
            input_channels: usize::MAX,
            output_channels: usize::MAX,
            max_frames: config.max_block_size,
        })
    }

    /// The plug-in's parameter model, when it publishes one. [main-thread]
    pub(crate) fn param_value(&self, id: ParamId) -> Option<f64> {
        self.plugin.params()?.value(id).ok()
    }

    /// Queues a parameter change for the next block. [main-thread]
    pub(crate) fn queue_param(&mut self, id: ParamId, value: f64, time: u32) -> bool {
        let mut event = DauxEventParamV1::new();
        event.header.time = time;
        event.param_id = id.0;
        event.value = value;
        self.block.input_events_mut().push_param(&event).is_ok()
    }

    /// Queues a note event for the next block. [main-thread]
    pub(crate) fn queue_note(
        &mut self,
        on: bool,
        time: u32,
        channel: i16,
        key: i16,
        velocity: f64,
    ) -> bool {
        let mut event = DauxEventNoteV1::new();
        event.header.kind = if on {
            DAUX_EVENT_NOTE_ON
        } else {
            DAUX_EVENT_NOTE_OFF
        };
        event.header.time = time;
        event.channel = channel;
        event.key = key;
        event.velocity = velocity;
        self.block.input_events_mut().push_note(&event).is_ok()
    }

    /// Runs one block. [audio-thread]
    ///
    /// # Errors
    ///
    /// [`HostErrorKind::BadBlock`] when the block cannot be described to the plug-in at all.
    pub(crate) fn process(
        &mut self,
        transport: &Transport,
        steady_time: i64,
        input: &AudioStorage<f32>,
        output: &mut AudioStorage<f32>,
    ) -> HostResult<ProcessStatus> {
        let frames = output.frames();
        self.reshape(input.channel_count(), output.channel_count());

        // Copy the caller's input into storage this instance owns exclusively.
        for channel in 0..input.channel_count() {
            let (Some(source), Some(destination)) = (
                input.channel(channel),
                self.input_scratch.channel_mut(channel),
            ) else {
                continue;
            };
            let count = source.len().min(destination.len());
            destination[..count].copy_from_slice(&source[..count]);
        }

        let frame_count = u32::try_from(frames).map_err(|_| {
            HostError::new(
                HostErrorKind::BadBlock,
                format!("{frames} frames does not fit the ABI's u32"),
            )
        })?;
        self.block.set_frames(frame_count)?;
        self.block.set_transport(Some(transport));
        self.block.set_steady_time(Some(steady_time));

        for channel in 0..self.input_channels {
            let slice = self
                .input_scratch
                .channel_mut(channel)
                .ok_or_else(|| bad_block("input", channel))?;
            let (pointer, length) = (slice.as_mut_ptr(), slice.len());
            // SAFETY: `pointer` addresses `length` samples inside `input_scratch`, which this
            // instance owns exclusively and does not resize or drop until `unbind_all` below
            // has run. The binding is used only by the `process` call in this function, on
            // this thread, and nothing else holds a live Rust reference to the samples while
            // it does: the `slice` borrow ends at the end of this statement.
            unsafe { self.block.bind_input_raw(0, channel, pointer, length) }?;
        }
        for channel in 0..self.output_channels {
            let slice = output
                .channel_mut(channel)
                .ok_or_else(|| bad_block("output", channel))?;
            let (pointer, length) = (slice.as_mut_ptr(), slice.len());
            // SAFETY: as above, for the caller's output storage. `output` is borrowed
            // exclusively for the whole of this function, so the samples stay allocated,
            // writable and unmoved for the `process` call, and the `slice` borrow ends
            // before the plug-in is given the pointer.
            unsafe { self.block.bind_output_raw(0, channel, pointer, length) }?;
        }

        let status = self.plugin.process(&mut self.block);

        // No pointer into the caller's storage may survive this call: the next block may
        // hand over an entirely different buffer, and a stale binding would be read.
        self.block.unbind_all();
        self.block.input_events_mut().clear();
        Ok(status)
    }

    /// Rebuilds the block when the topology changes. [main-thread] — allocates.
    fn reshape(&mut self, inputs: usize, outputs: usize) {
        if self.input_channels == inputs && self.output_channels == outputs {
            return;
        }
        // One bus per direction, with as many channels as the caller's storage has. A
        // direction with no channels is described as *no bus at all*, which is what an
        // instrument's input and a MIDI effect's output really are.
        let inputs_u32 = [u32::try_from(inputs).unwrap_or(u32::MAX)];
        let outputs_u32 = [u32::try_from(outputs).unwrap_or(u32::MAX)];

        // The queue survives the rebuild. `queue_param` and `queue_note` push into the block
        // *before* the first `process` reveals the topology, so a reshape that started from a
        // fresh event list would drop every parameter change and note the caller set up —
        // and because `input_channels` starts at `usize::MAX`, the first block always
        // reshapes. That is exactly the path `TestHost::load` takes, so dropping here made
        // `set_param` and `send_note_on` silently do nothing for every loaded `.axt`.
        let pending = self.block.take_input_events();
        self.block = HostBlock::new(
            if inputs == 0 { &[] } else { &inputs_u32 },
            if outputs == 0 { &[] } else { &outputs_u32 },
            self.max_frames,
        )
        .with_event_capacity(EVENT_CAPACITY, EVENT_BYTES);
        // The list handed back is the freshly built one, which nothing has queued into.
        drop(self.block.set_input_events(pending));

        self.input_scratch = AudioStorage::new(inputs, self.max_frames as usize);
        self.input_channels = inputs;
        self.output_channels = outputs;
    }

    /// Serialises the plug-in's state through `daux.state/1`. [main-thread]
    pub(crate) fn save_state(&self) -> HostResult<Vec<u8>> {
        let state = self.plugin.state().ok_or_else(|| {
            HostError::new(
                HostErrorKind::Unsupported,
                "the plug-in does not implement `daux.state/1`",
            )
        })?;
        Ok(state.save()?)
    }

    /// Restores the plug-in's state through `daux.state/1`. [main-thread]
    pub(crate) fn load_state(&self, bytes: &[u8]) -> HostResult<()> {
        let state = self.plugin.state().ok_or_else(|| {
            HostError::new(
                HostErrorKind::Unsupported,
                "the plug-in does not implement `daux.state/1`",
            )
        })?;
        state.load(bytes)?;
        Ok(())
    }

    /// Drains work the plug-in queued with `request_callback`. [main-thread]
    pub(crate) fn on_main_thread(&mut self) -> HostResult<()> {
        self.plugin.on_main_thread()?;
        Ok(())
    }

    /// Clears delay lines and voices. [main-thread]
    pub(crate) fn reset(&mut self) -> HostResult<()> {
        // `abi-v1` §7 forbids `reset` while processing, so the run is stopped around it.
        self.plugin.stop_processing()?;
        let outcome = self.plugin.reset();
        self.plugin.start_processing()?;
        outcome?;
        Ok(())
    }

    /// The plug-in's declared latency. [main-thread]
    pub(crate) fn latency(&self) -> Latency {
        Latency::from_samples(self.plugin.latency())
    }

    /// The plug-in's declared tail. [main-thread]
    pub(crate) fn tail(&self) -> Tail {
        self.plugin.tail()
    }

    /// The events the plug-in produced during the last block. [main-thread]
    pub(crate) const fn output_events(&self) -> &daux_runtime::EventList {
        self.block.output_events()
    }
}

fn bad_block(direction: &str, channel: usize) -> HostError {
    HostError::new(
        HostErrorKind::BadBlock,
        format!("{direction} channel {channel} is missing from the audio handed to `process`"),
    )
}

/// Builds a note-on event for the model-side path. [main-thread]
pub(crate) fn note_event(time: u32, channel: i16, key: i16, velocity: f64) -> NoteEvent {
    NoteEvent {
        header: EventHeader::at(time),
        note_id: -1,
        channel,
        key,
        velocity,
        tuning: 0.0,
    }
}

/// Builds a parameter-value event for the model-side path. [main-thread]
pub(crate) fn param_event(time: u32, id: ParamId, value: f64) -> ParamEvent {
    ParamEvent {
        header: EventHeader::at(time),
        param_id: id.0,
        note_id: -1,
        channel: -1,
        key: -1,
        value,
    }
}
