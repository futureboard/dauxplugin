//! One `process` call, assembled on the host side.
//!
//! [`HostBlock`] is the host's counterpart to `DauxProcessV1` (`abi-v1` §8): a preallocated
//! description of one block that is rebound per call and never resized. Everything that
//! allocates happens in [`HostBlock::new`]; every method a host calls between
//! `start_processing` and `stop_processing` only writes into storage that already exists.
//!
//! # Binding audio
//!
//! The ABI hands the plug-in `*mut f32` for **both** directions — `abi-v1` §8 keeps input
//! buffers `*mut` only so a host can pass one allocation for both — so the safe binding
//! methods take `&'a mut [f32]` for inputs as well. A host that cannot prove a plug-in
//! treats its inputs as read-only is not entitled to hand out a `&[f32]`, and in-place
//! processing, where the same buffer is both, cannot be expressed with two `&mut` at all.
//! [`HostBlock::bind_input_raw`] exists for exactly that case and is `unsafe` because the
//! aliasing rules become the caller's to uphold.

use core::marker::PhantomData;

use daux_abi::{DauxAudioBufferV1, DauxProcessV1, DauxTransportV1};
use daux_transport::Transport;

use crate::error::{RuntimeError, RuntimeErrorKind, RuntimeResult};
use crate::events::EventList;

/// Default number of events each direction of a block can carry.
const DEFAULT_EVENT_CAPACITY: usize = 512;

/// Default byte arena for each direction of a block.
const DEFAULT_EVENT_BYTES: usize = 64 * 1024;

/// One bus's channel pointers for the current block.
#[derive(Debug)]
struct BusBinding {
    /// One entry per channel; null until bound.
    channels: Vec<*mut f32>,
    /// Frames each bound pointer is valid for.
    lengths: Vec<usize>,
    /// Bit `c` set means channel `c` is constant for the whole block.
    constant_mask: u64,
}

impl BusBinding {
    fn new(channels: usize) -> Self {
        Self {
            channels: vec![core::ptr::null_mut(); channels],
            lengths: vec![0; channels],
            constant_mask: 0,
        }
    }

    fn unbind(&mut self) {
        self.channels.fill(core::ptr::null_mut());
        self.lengths.fill(0);
        self.constant_mask = 0;
    }
}

/// A preallocated, rebindable `process` block. [main-thread] to build, [audio-thread] to
/// use.
///
/// `'a` is the lifetime of the sample buffers bound into it: a block cannot outlive the
/// audio it points at.
#[derive(Debug)]
pub struct HostBlock<'a> {
    frames: u32,
    max_frames: u32,
    steady_time: i64,
    transport: Option<DauxTransportV1>,
    inputs: Vec<BusBinding>,
    outputs: Vec<BusBinding>,
    input_abi: Vec<DauxAudioBufferV1>,
    output_abi: Vec<DauxAudioBufferV1>,
    input_events: EventList,
    output_events: EventList,
    _borrow: PhantomData<&'a mut [f32]>,
}

// SAFETY: the raw pointers a block holds come either from `&'a mut [f32]`, which is `Send`,
// or from `bind_input_raw`/`bind_output_raw`, whose contract makes the caller responsible
// for the memory being usable from the thread that runs `process`. Nothing else in the
// struct is thread-affine: the event lists own plain arenas and the ABI mirrors are plain
// data. `HostBlock` is deliberately **not** `Sync`: `abi-v1` §15 says the audio-thread calls
// for one instance are never concurrent, so a block is used by one thread at a time.
unsafe impl Send for HostBlock<'_> {}

impl<'a> HostBlock<'a> {
    /// Preallocates a block for the given bus topology. [main-thread] — allocates.
    ///
    /// `input_channels[i]` is the channel count of input bus `i`, and likewise for outputs.
    /// `max_frames` is the largest block that will ever be bound; it should match the
    /// `max_block_size` the plug-in was activated with.
    #[must_use]
    pub fn new(input_channels: &[u32], output_channels: &[u32], max_frames: u32) -> Self {
        let inputs: Vec<BusBinding> = input_channels
            .iter()
            .map(|&n| BusBinding::new(n as usize))
            .collect();
        let outputs: Vec<BusBinding> = output_channels
            .iter()
            .map(|&n| BusBinding::new(n as usize))
            .collect();
        Self {
            frames: 0,
            max_frames,
            steady_time: -1,
            transport: None,
            input_abi: vec![DauxAudioBufferV1::empty(); inputs.len()],
            output_abi: vec![DauxAudioBufferV1::empty(); outputs.len()],
            inputs,
            outputs,
            input_events: EventList::with_capacity(DEFAULT_EVENT_CAPACITY, DEFAULT_EVENT_BYTES),
            output_events: EventList::with_capacity(DEFAULT_EVENT_CAPACITY, DEFAULT_EVENT_BYTES),
            _borrow: PhantomData,
        }
    }

    /// Replaces the event capacity of both directions. [main-thread] — allocates.
    #[must_use]
    pub fn with_event_capacity(mut self, events: usize, bytes: usize) -> Self {
        self.input_events = EventList::with_capacity(events, bytes);
        self.output_events = EventList::with_capacity(events, bytes);
        self
    }

    /// Frames in the block as currently set. [audio-thread]
    #[inline]
    #[must_use]
    pub const fn frames(&self) -> u32 {
        self.frames
    }

    /// The largest block this object was built for. [audio-thread]
    #[inline]
    #[must_use]
    pub const fn max_frames(&self) -> u32 {
        self.max_frames
    }

    /// Sets the frame count for the next `process`. [audio-thread]
    ///
    /// # Errors
    ///
    /// [`RuntimeErrorKind::InvalidArgument`] for zero, or for more frames than the block
    /// was built for. `abi-v1` §8 allows any count in `1 ..= max_block_size` and nothing
    /// outside it.
    pub fn set_frames(&mut self, frames: u32) -> RuntimeResult<()> {
        if frames == 0 || frames > self.max_frames {
            return Err(RuntimeError::new(
                RuntimeErrorKind::InvalidArgument,
                format!(
                    "frame count {frames} is outside 1..={} (abi-v1 §8)",
                    self.max_frames
                ),
            ));
        }
        self.frames = frames;
        Ok(())
    }

    /// The monotonic sample counter, or `None` when the host does not keep one.
    /// [audio-thread]
    #[inline]
    #[must_use]
    pub const fn steady_time(&self) -> Option<i64> {
        if self.steady_time < 0 {
            None
        } else {
            Some(self.steady_time)
        }
    }

    /// Sets the monotonic sample counter. `None` publishes `-1`, the ABI's "unavailable".
    /// [audio-thread]
    #[inline]
    pub const fn set_steady_time(&mut self, steady_time: Option<i64>) {
        self.steady_time = match steady_time {
            Some(t) if t >= 0 => t,
            _ => -1,
        };
    }

    /// Publishes the host transport, or removes it. [audio-thread]
    ///
    /// A plug-in must never read a field the host did not set, so the `HAS_*` flags travel
    /// verbatim: this converts, it does not invent.
    pub fn set_transport(&mut self, transport: Option<&Transport>) {
        self.transport = transport.map(to_abi_transport);
    }

    /// Number of input buses. [audio-thread]
    #[inline]
    #[must_use]
    pub fn input_bus_count(&self) -> usize {
        self.inputs.len()
    }

    /// Number of output buses. [audio-thread]
    #[inline]
    #[must_use]
    pub fn output_bus_count(&self) -> usize {
        self.outputs.len()
    }

    /// Channels on input bus `bus`, or `None` when there is no such bus. [audio-thread]
    #[inline]
    #[must_use]
    pub fn input_channel_count(&self, bus: usize) -> Option<usize> {
        self.inputs.get(bus).map(|b| b.channels.len())
    }

    /// Channels on output bus `bus`, or `None` when there is no such bus. [audio-thread]
    #[inline]
    #[must_use]
    pub fn output_channel_count(&self, bus: usize) -> Option<usize> {
        self.outputs.get(bus).map(|b| b.channels.len())
    }

    /// Binds one input channel. [audio-thread]
    ///
    /// Takes `&mut` because the ABI hands the plug-in a `*mut` — see the module
    /// documentation.
    ///
    /// # Errors
    ///
    /// [`RuntimeErrorKind::InvalidArgument`] when `bus` or `channel` is out of range.
    pub fn bind_input(
        &mut self,
        bus: usize,
        channel: usize,
        data: &'a mut [f32],
    ) -> RuntimeResult<()> {
        let (len, ptr) = (data.len(), data.as_mut_ptr());
        // SAFETY: `data` is a live `&'a mut [f32]` of exactly `len` samples, so the pointer
        // is valid, writable and unaliased for `'a`, which outlives every `process` call
        // this block can be used in.
        unsafe { self.bind_input_raw(bus, channel, ptr, len) }
    }

    /// Binds one output channel. [audio-thread]
    ///
    /// # Errors
    ///
    /// [`RuntimeErrorKind::InvalidArgument`] when `bus` or `channel` is out of range.
    pub fn bind_output(
        &mut self,
        bus: usize,
        channel: usize,
        data: &'a mut [f32],
    ) -> RuntimeResult<()> {
        let (len, ptr) = (data.len(), data.as_mut_ptr());
        // SAFETY: as in `bind_input`.
        unsafe { self.bind_output_raw(bus, channel, ptr, len) }
    }

    /// Binds one input channel from a raw pointer. [audio-thread]
    ///
    /// The escape hatch for in-place processing, where the same allocation is both an
    /// input and an output and cannot be described by two `&mut`.
    ///
    /// # Errors
    ///
    /// [`RuntimeErrorKind::InvalidArgument`] when `bus` or `channel` is out of range.
    ///
    /// # Safety
    ///
    /// `data` must point to at least `frames` `f32` samples that stay allocated, writable
    /// and unmoved for every `process` call this binding is used in, and that are safe to
    /// access from the thread that makes those calls. Aliasing another binding is permitted
    /// — `abi-v1` §8 allows input and output buffers to be the same allocation — but no
    /// live Rust reference to the same memory may exist across the call.
    pub unsafe fn bind_input_raw(
        &mut self,
        bus: usize,
        channel: usize,
        data: *mut f32,
        frames: usize,
    ) -> RuntimeResult<()> {
        bind(&mut self.inputs, "input", bus, channel, data, frames)
    }

    /// Binds one output channel from a raw pointer. [audio-thread]
    ///
    /// # Errors
    ///
    /// [`RuntimeErrorKind::InvalidArgument`] when `bus` or `channel` is out of range.
    ///
    /// # Safety
    ///
    /// As [`HostBlock::bind_input_raw`].
    pub unsafe fn bind_output_raw(
        &mut self,
        bus: usize,
        channel: usize,
        data: *mut f32,
        frames: usize,
    ) -> RuntimeResult<()> {
        bind(&mut self.outputs, "output", bus, channel, data, frames)
    }

    /// Marks channels of an input bus as constant for the whole block. [audio-thread]
    ///
    /// Purely an optimisation hint; a plug-in must tolerate a zero mask.
    pub fn set_input_constant_mask(&mut self, bus: usize, mask: u64) {
        if let Some(binding) = self.inputs.get_mut(bus) {
            binding.constant_mask = mask;
        }
    }

    /// Marks channels of an output bus as constant for the whole block. [audio-thread]
    pub fn set_output_constant_mask(&mut self, bus: usize, mask: u64) {
        if let Some(binding) = self.outputs.get_mut(bus) {
            binding.constant_mask = mask;
        }
    }

    /// Drops every channel binding, so a stale pointer cannot survive into the next block.
    /// [audio-thread]
    pub fn unbind_all(&mut self) {
        for bus in self.inputs.iter_mut().chain(self.outputs.iter_mut()) {
            bus.unbind();
        }
    }

    /// The events the plug-in will see. [audio-thread]
    #[inline]
    #[must_use]
    pub const fn input_events(&self) -> &EventList {
        &self.input_events
    }

    /// The events the plug-in will see, for the host to fill. [audio-thread]
    #[inline]
    pub const fn input_events_mut(&mut self) -> &mut EventList {
        &mut self.input_events
    }

    /// The events the plug-in produced during the last `process`. [audio-thread]
    #[inline]
    #[must_use]
    pub const fn output_events(&self) -> &EventList {
        &self.output_events
    }

    /// The events the plug-in produced, mutably. [audio-thread]
    #[inline]
    pub const fn output_events_mut(&mut self) -> &mut EventList {
        &mut self.output_events
    }

    /// Moves the queued input events out of this block. [main-thread]
    ///
    /// A block is built for one bus topology, so a host that has to change topology — a
    /// track goes from mono to stereo, a side-chain is connected — replaces the whole
    /// `HostBlock`. Anything already queued for the next `process` lives in the old block,
    /// and a host that simply dropped it would silently discard the parameter changes and
    /// notes it had accepted. Pair this with [`set_input_events`](HostBlock::set_input_events)
    /// on the replacement to carry the queue across.
    ///
    /// The list left behind is empty and holds no storage, so this block can no longer be
    /// queued into until a new one is installed. It allocates nothing.
    #[must_use = "the queue is removed from the block; install it somewhere or it is lost"]
    pub fn take_input_events(&mut self) -> EventList {
        // `with_capacity(0, 0)` allocates nothing: both a zero-length `vec![]` and a
        // zero-capacity `Vec` are dangling by construction.
        core::mem::replace(&mut self.input_events, EventList::with_capacity(0, 0))
    }

    /// Installs `events` as the queue the plug-in will see. [main-thread]
    ///
    /// The counterpart of [`take_input_events`](HostBlock::take_input_events). The list
    /// replaced is returned so a caller can keep whichever of the two has the capacity it
    /// wants; dropping it is fine and frees its arena.
    pub fn set_input_events(&mut self, events: EventList) -> EventList {
        core::mem::replace(&mut self.input_events, events)
    }

    /// Checks the block describes a call a plug-in may actually be given. [audio-thread]
    ///
    /// # Errors
    ///
    /// [`RuntimeErrorKind::InvalidArgument`] when the frame count is unset or out of range,
    /// when a channel was never bound, or when a bound buffer is shorter than the frame
    /// count — the three ways a host hands a plug-in a pointer it will read past.
    pub fn check(&self) -> RuntimeResult<()> {
        if self.frames == 0 || self.frames > self.max_frames {
            return Err(RuntimeError::new(
                RuntimeErrorKind::InvalidArgument,
                format!(
                    "frame count {} is outside 1..={} (abi-v1 §8)",
                    self.frames, self.max_frames
                ),
            ));
        }
        let frames = self.frames as usize;
        for (direction, buses) in [("input", &self.inputs), ("output", &self.outputs)] {
            for (bus, binding) in buses.iter().enumerate() {
                for (channel, (&ptr, &len)) in binding
                    .channels
                    .iter()
                    .zip(binding.lengths.iter())
                    .enumerate()
                {
                    if ptr.is_null() {
                        return Err(RuntimeError::new(
                            RuntimeErrorKind::InvalidArgument,
                            format!("{direction} bus {bus} channel {channel} is not bound"),
                        ));
                    }
                    if len < frames {
                        return Err(RuntimeError::new(
                            RuntimeErrorKind::InvalidArgument,
                            format!(
                                "{direction} bus {bus} channel {channel} holds {len} frames but \
                                 the block is {frames}"
                            ),
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    /// Builds the ABI view of this block and runs `f` with it. [audio-thread]
    ///
    /// The `DauxProcessV1` and the two `DauxEventListV1` values live on this function's
    /// stack frame, so every pointer inside them is valid for exactly the duration of `f` —
    /// which is precisely the lifetime `abi-v1` §16.3 gives them. Nothing allocates.
    pub(crate) fn with_raw<R>(&mut self, f: impl FnOnce(&DauxProcessV1) -> R) -> R {
        for index in 0..self.inputs.len() {
            let binding = &self.inputs[index];
            let buffer = DauxAudioBufferV1 {
                channel_count: binding.channels.len() as u32,
                _pad0: 0,
                data32: binding.channels.as_ptr(),
                data64: core::ptr::null(),
                constant_mask: binding.constant_mask,
            };
            self.input_abi[index] = buffer;
        }
        for index in 0..self.outputs.len() {
            let binding = &self.outputs[index];
            let buffer = DauxAudioBufferV1 {
                channel_count: binding.channels.len() as u32,
                _pad0: 0,
                data32: binding.channels.as_ptr(),
                data64: core::ptr::null(),
                constant_mask: binding.constant_mask,
            };
            self.output_abi[index] = buffer;
        }

        let transport = self
            .transport
            .as_ref()
            .map_or(core::ptr::null(), |t| &raw const *t);
        let audio_input_count = self.input_abi.len() as u32;
        let audio_output_count = self.output_abi.len() as u32;
        let audio_inputs = self.input_abi.as_ptr();
        let audio_outputs = self.output_abi.as_mut_ptr();
        let frame_count = self.frames;
        let steady_time = self.steady_time;

        let input_events = self.input_events.as_abi();
        let output_events = self.output_events.as_abi();

        let process = DauxProcessV1 {
            size: DauxProcessV1::SIZE,
            frame_count,
            steady_time,
            transport,
            audio_input_count,
            audio_output_count,
            audio_inputs,
            audio_outputs,
            in_events: &raw const input_events,
            out_events: &raw const output_events,
            reserved: [0; 6],
        };
        f(&process)
    }
}

/// Records one channel binding, or reports why it cannot be recorded.
fn bind(
    buses: &mut [BusBinding],
    direction: &str,
    bus: usize,
    channel: usize,
    data: *mut f32,
    frames: usize,
) -> RuntimeResult<()> {
    let binding = buses.get_mut(bus).ok_or_else(|| {
        RuntimeError::new(
            RuntimeErrorKind::InvalidArgument,
            format!("no {direction} bus {bus}"),
        )
    })?;
    let slot = binding.channels.get_mut(channel).ok_or_else(|| {
        RuntimeError::new(
            RuntimeErrorKind::InvalidArgument,
            format!("{direction} bus {bus} has no channel {channel}"),
        )
    })?;
    *slot = data;
    binding.lengths[channel] = frames;
    Ok(())
}

/// Converts the model's transport into the ABI's, flags and all. [audio-thread]
fn to_abi_transport(t: &Transport) -> DauxTransportV1 {
    let mut abi = DauxTransportV1::new();
    abi.flags = t.flags.bits();
    abi.song_pos_samples = t.song_pos_samples;
    abi.song_pos_beats = t.song_pos_beats;
    abi.song_pos_seconds = t.song_pos_seconds;
    abi.tempo = t.tempo;
    abi.tempo_increment = t.tempo_increment;
    abi.bar_start_beats = t.bar_start_beats;
    abi.bar_number = t.bar_number;
    abi.time_sig_numerator = t.time_signature.numerator;
    abi.time_sig_denominator = t.time_signature.denominator;
    abi.loop_start_beats = t.loop_start_beats;
    abi.loop_end_beats = t.loop_end_beats;
    abi.loop_start_seconds = t.loop_start_seconds;
    abi.loop_end_seconds = t.loop_end_seconds;
    abi
}

#[cfg(test)]
mod tests {
    use super::*;
    use daux_abi::{DAUX_TRANSPORT_HAS_TEMPO, DAUX_TRANSPORT_IS_PLAYING};
    use daux_transport::TransportBuilder;

    fn stereo_block<'a>(max_frames: u32) -> HostBlock<'a> {
        HostBlock::new(&[2], &[2], max_frames)
    }

    #[test]
    fn a_block_reports_its_topology() {
        let block = HostBlock::new(&[2, 1], &[2], 512);
        assert_eq!(block.input_bus_count(), 2);
        assert_eq!(block.output_bus_count(), 1);
        assert_eq!(block.input_channel_count(0), Some(2));
        assert_eq!(block.input_channel_count(1), Some(1));
        assert_eq!(block.input_channel_count(2), None);
        assert_eq!(block.output_channel_count(0), Some(2));
        assert_eq!(block.max_frames(), 512);
        assert_eq!(block.steady_time(), None);
    }

    #[test]
    fn frame_counts_outside_the_activated_range_are_refused() {
        let mut block = stereo_block(128);
        assert!(block.set_frames(0).is_err());
        assert!(block.set_frames(129).is_err());
        block.set_frames(1).expect("one frame is legal");
        block.set_frames(128).expect("the maximum is legal");
        assert_eq!(block.frames(), 128);
    }

    #[test]
    fn binding_out_of_range_is_refused_rather_than_ignored() {
        let mut first = [0.0f32; 64];
        let mut second = [0.0f32; 64];
        let mut block = stereo_block(64);
        let err = block.bind_input(9, 0, &mut first).unwrap_err();
        assert_eq!(err.kind(), RuntimeErrorKind::InvalidArgument);
        assert!(err.message().contains("bus 9"), "{err}");
        let err = block.bind_output(0, 5, &mut second).unwrap_err();
        assert_eq!(err.kind(), RuntimeErrorKind::InvalidArgument);
        assert!(err.message().contains("channel 5"), "{err}");
    }

    /// Every one of these is a pointer the plug-in would read past the end of.
    #[test]
    fn check_catches_every_way_a_block_can_be_unsafe_to_hand_over() {
        let mut left_in = [0.0f32; 64];
        let mut right_in = [0.0f32; 64];
        let mut left_out = [0.0f32; 64];
        let mut short_out = [0.0f32; 16];

        let mut block = stereo_block(64);
        // Frames never set.
        assert!(block.check().is_err());

        block.set_frames(64).unwrap();
        // Nothing bound.
        let err = block.check().unwrap_err();
        assert!(err.message().contains("not bound"), "{err}");

        block.bind_input(0, 0, &mut left_in).unwrap();
        block.bind_input(0, 1, &mut right_in).unwrap();
        block.bind_output(0, 0, &mut left_out).unwrap();
        // One output channel still missing.
        let err = block.check().unwrap_err();
        assert!(err.message().contains("channel 1"), "{err}");

        block.bind_output(0, 1, &mut short_out).unwrap();
        // Bound, but too short for the block.
        let err = block.check().unwrap_err();
        assert!(err.message().contains("16 frames"), "{err}");

        // A shorter block fits the same buffers.
        block.set_frames(16).unwrap();
        block.check().expect("16 frames fit every binding");
    }

    #[test]
    fn unbinding_makes_the_block_unusable_again() {
        let mut a = [0.0f32; 32];
        let mut b = [0.0f32; 32];
        let mut c = [0.0f32; 32];
        let mut d = [0.0f32; 32];
        let mut block = stereo_block(32);
        block.set_frames(32).unwrap();
        block.bind_input(0, 0, &mut a).unwrap();
        block.bind_input(0, 1, &mut b).unwrap();
        block.bind_output(0, 0, &mut c).unwrap();
        block.bind_output(0, 1, &mut d).unwrap();
        block.check().expect("fully bound");

        block.unbind_all();
        assert!(
            block.check().is_err(),
            "a stale pointer must not survive into the next block"
        );
    }

    #[test]
    fn steady_time_round_trips_through_the_abi_sentinel() {
        let mut block = stereo_block(32);
        assert_eq!(block.steady_time(), None);
        block.set_steady_time(Some(48_000));
        assert_eq!(block.steady_time(), Some(48_000));
        block.set_steady_time(None);
        assert_eq!(block.steady_time(), None);
        // A negative counter is not a counter.
        block.set_steady_time(Some(-5));
        assert_eq!(block.steady_time(), None);
    }

    /// A plug-in must never read a field the host did not set, so the flags must survive
    /// the conversion exactly.
    #[test]
    fn transport_conversion_carries_the_flags_verbatim() {
        let transport = TransportBuilder::new()
            .playing(true)
            .tempo(128.0)
            .sample_position(96_000)
            .build();
        let abi = to_abi_transport(&transport);
        assert_eq!(abi.flags, transport.flags.bits());
        assert_ne!(abi.flags & DAUX_TRANSPORT_HAS_TEMPO, 0);
        assert_ne!(abi.flags & DAUX_TRANSPORT_IS_PLAYING, 0);
        assert_eq!(abi.tempo, 128.0);
        assert_eq!(abi.song_pos_samples, 96_000);
        assert_eq!(abi.size, DauxTransportV1::SIZE);

        // A transport that promises nothing must raise no flags at all.
        let empty = to_abi_transport(&Transport::EMPTY);
        assert_eq!(empty.flags, 0);
    }

    /// The raw view is what the module actually sees; every pointer in it has to line up
    /// with what the host bound.
    #[test]
    fn the_raw_view_matches_the_bindings() {
        let mut left_in = [1.0f32; 8];
        let mut right_in = [2.0f32; 8];
        let mut left_out = [0.0f32; 8];
        let mut right_out = [0.0f32; 8];

        let mut block = stereo_block(8);
        block.set_frames(8).unwrap();
        block.set_steady_time(Some(1_024));
        block.set_input_constant_mask(0, 0b10);
        block.bind_input(0, 0, &mut left_in).unwrap();
        block.bind_input(0, 1, &mut right_in).unwrap();
        block.bind_output(0, 0, &mut left_out).unwrap();
        block.bind_output(0, 1, &mut right_out).unwrap();
        block.set_transport(Some(&TransportBuilder::new().tempo(90.0).build()));

        block.with_raw(|process| {
            assert_eq!(process.size, DauxProcessV1::SIZE);
            assert_eq!(process.frame_count, 8);
            assert_eq!(process.steady_time, 1_024);
            assert_eq!(process.audio_input_count, 1);
            assert_eq!(process.audio_output_count, 1);
            assert!(!process.transport.is_null());
            assert!(!process.in_events.is_null());
            assert!(!process.out_events.is_null());

            // SAFETY: `with_raw` guarantees every pointer inside `process` is valid for the
            // duration of this closure, which is exactly what a `process` call gets.
            let input = unsafe { *process.audio_inputs };
            assert_eq!(input.channel_count, 2);
            assert_eq!(input.constant_mask, 0b10);
            assert!(input.data64.is_null(), "an f32 block sets only data32");
            // SAFETY: `channel_count` is 2, so the pointer array has two entries, and each
            // addresses the eight samples the host bound.
            let samples = unsafe {
                let channels = core::slice::from_raw_parts(input.data32, 2);
                (
                    core::slice::from_raw_parts(channels[0], 8),
                    core::slice::from_raw_parts(channels[1], 8),
                )
            };
            assert_eq!(samples.0, [1.0; 8]);
            assert_eq!(samples.1, [2.0; 8]);

            // SAFETY: as above, for the single output bus.
            let output = unsafe { *process.audio_outputs };
            assert_eq!(output.channel_count, 2);

            // SAFETY: `in_events` is the list `with_raw` built from this block's own
            // storage; its context pointer is valid for the closure.
            let count = unsafe {
                let list = &*process.in_events;
                (list.count)(list.ctx)
            };
            assert_eq!(count, 0);
        });
    }

    /// The events the host queued must arrive, and the ones the plug-in pushes must land
    /// in the block's own output list.
    #[test]
    fn events_travel_in_both_directions_through_the_raw_view() {
        let mut samples = [0.0f32; 4];
        let mut block = HostBlock::new(&[], &[1], 4);
        block.set_frames(4).unwrap();
        block.bind_output(0, 0, &mut samples).unwrap();

        let mut note = daux_abi::DauxEventNoteV1::new();
        note.header.time = 2;
        note.key = 64;
        block.input_events_mut().push_note(&note).unwrap();

        block.with_raw(|process| {
            // SAFETY: both list pointers address values `with_raw` built on its own stack
            // frame; they are valid for this closure.
            unsafe {
                let input = &*process.in_events;
                assert_eq!((input.count)(input.ctx), 1);
                let record = (input.get)(input.ctx, 0);
                assert!(!record.is_null());
                assert_eq!(record.read_unaligned().time, 2);

                let output = &*process.out_events;
                let mut end = daux_abi::DauxEventNoteV1::new();
                end.header.kind = daux_abi::DAUX_EVENT_NOTE_END;
                end.header.time = 3;
                end.key = 64;
                assert_eq!((output.push)(output.ctx, (&raw const end).cast()).0, 0);
            }
        });

        assert_eq!(block.output_events().len(), 1);
        let produced = block.output_events().note(0).expect("a note event");
        assert_eq!(produced.key, 64);
        assert_eq!(produced.header.time, 3);
    }

    /// A host that changes bus topology replaces the whole block, and everything already
    /// queued for the next `process` has to survive the swap.
    ///
    /// This is a real regression: `daux-host` reshapes its block on the first `process`,
    /// because it only learns the channel counts when the caller hands it storage. Losing the
    /// queue there made every `set_param` and `send_note_on` on a loaded `.axt` a no-op, with
    /// no error anywhere — the plug-in simply ran at its default value.
    #[test]
    fn queued_input_events_survive_a_topology_change() {
        let mut mono = HostBlock::new(&[1], &[1], 8);
        let mut param = daux_abi::DauxEventParamV1::new();
        param.param_id = 7;
        param.value = -6.0;
        param.header.time = 3;
        mono.input_events_mut().push_param(&param).unwrap();
        let mut note = daux_abi::DauxEventNoteV1::new();
        note.header.time = 1;
        note.key = 69;
        mono.input_events_mut().push_note(&note).unwrap();
        assert_eq!(mono.input_events().len(), 2);

        // The host discovers it is actually stereo and rebuilds.
        let pending = mono.take_input_events();
        assert!(
            mono.input_events().is_empty(),
            "the queue is moved out, not copied"
        );
        let mut stereo = HostBlock::new(&[2], &[2], 8);
        let displaced = stereo.set_input_events(pending);
        assert!(
            displaced.is_empty(),
            "the list handed back is the new block's own, which nothing queued into"
        );

        assert_eq!(stereo.input_events().len(), 2);
        let carried_param = stereo.input_events().param(0).expect("the parameter event");
        assert_eq!(carried_param.param_id, 7);
        assert_eq!(carried_param.value, -6.0);
        assert_eq!(carried_param.header.time, 3);
        let carried_note = stereo.input_events().note(1).expect("the note event");
        assert_eq!(carried_note.key, 69);
        assert_eq!(carried_note.header.time, 1);

        // And the carried queue is still usable: it kept its arena, so the host can go on
        // pushing into the new block rather than having to rebuild the queue too.
        let mut second = daux_abi::DauxEventParamV1::new();
        second.param_id = 8;
        second.header.time = 5;
        stereo.input_events_mut().push_param(&second).unwrap();
        assert_eq!(stereo.input_events().len(), 3);
    }

    /// The emptied list left behind must be a usable `EventList`, not a poisoned one: a host
    /// that takes the queue and then never installs a new one must get a clean refusal rather
    /// than a panic on the next push.
    #[test]
    fn a_block_whose_queue_was_taken_refuses_pushes_instead_of_panicking() {
        let mut block = HostBlock::new(&[1], &[1], 8);
        let _ = block.take_input_events();
        assert_eq!(block.input_events().capacity(), 0);
        assert_eq!(block.input_events().byte_capacity(), 0);
        let note = daux_abi::DauxEventNoteV1::new();
        assert!(
            block.input_events_mut().push_note(&note).is_err(),
            "a zero-capacity list is full, and reports it"
        );
        assert!(block.input_events().is_empty());
    }

    #[test]
    fn a_block_can_be_handed_to_another_thread() {
        const fn assert_send<T: Send>() {}
        assert_send::<HostBlock<'static>>();
    }
}
