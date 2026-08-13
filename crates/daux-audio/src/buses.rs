//! Multi-bus audio for one processing block.

use core::fmt;
use core::ptr::NonNull;
use core::slice;

use crate::buffer::{AudioBufferMut, AudioBufferRef};
use crate::sample::Sample;

/// Every audio bus of one `process` call. `[audio-thread]`
///
/// Built by the host or the format adapter from the per-bus views it has already validated;
/// constructing one is safe and allocation-free, because all the `unsafe` lives in the
/// [`AudioBufferRef`]/[`AudioBufferMut`] constructors. The adapter owns the two arrays of
/// views and must keep them alive for the whole call — they are usually preallocated in
/// `activate`, never in `process`.
///
/// Bus `0` of each direction is the main bus (`abi-v1` §11.1); [`main_input`] and
/// [`main_output`] are shorthand for it.
///
/// Input and output buses may share memory when the host processes in place, which is why
/// the inputs are handed out as shared views and the outputs as exclusive ones.
///
/// [`main_input`]: AudioBuses::main_input
/// [`main_output`]: AudioBuses::main_output
pub struct AudioBuses<'a, T: Sample> {
    inputs: &'a [AudioBufferRef<'a, T>],
    outputs: &'a mut [AudioBufferMut<'a, T>],
    frames: usize,
}

impl<T: Sample> fmt::Debug for AudioBuses<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AudioBuses")
            .field("inputs", &self.inputs.len())
            .field("outputs", &self.outputs.len())
            .field("frames", &self.frames)
            .finish()
    }
}

impl<'a, T: Sample> AudioBuses<'a, T> {
    /// Assembles the buses of one block. `[audio-thread]` — allocation-free.
    ///
    /// `frames` is the block length; it is carried separately so that a plug-in with no
    /// buses at all (a MIDI effect, say) still knows how long the block is. In debug builds
    /// every bus is checked against it.
    #[must_use]
    pub fn new(
        inputs: &'a [AudioBufferRef<'a, T>],
        outputs: &'a mut [AudioBufferMut<'a, T>],
        frames: usize,
    ) -> Self {
        debug_assert!(
            inputs.iter().all(|b| b.frames() == frames),
            "AudioBuses::new: an input bus disagrees with the block's frame count"
        );
        debug_assert!(
            outputs.iter().all(|b| b.frames() == frames),
            "AudioBuses::new: an output bus disagrees with the block's frame count"
        );
        Self {
            inputs,
            outputs,
            frames,
        }
    }

    /// An empty set of buses for a block of `frames` frames. `[audio-thread]`
    #[must_use]
    pub fn empty(frames: usize) -> Self {
        Self {
            inputs: &[],
            // SAFETY: `NonNull::dangling` is non-null and correctly aligned, and a
            // zero-length slice never reads or writes memory, so it aliases nothing and
            // needs no allocation.
            outputs: unsafe { slice::from_raw_parts_mut(NonNull::dangling().as_ptr(), 0) },
            frames,
        }
    }

    /// Number of frames in this block. `[audio-thread]`
    #[inline]
    #[must_use]
    pub const fn frames(&self) -> usize {
        self.frames
    }

    /// Number of input buses. `[audio-thread]`
    #[inline]
    #[must_use]
    pub const fn input_count(&self) -> usize {
        self.inputs.len()
    }

    /// Number of output buses. `[audio-thread]`
    #[inline]
    #[must_use]
    pub const fn output_count(&self) -> usize {
        self.outputs.len()
    }

    /// Input bus `bus`, or `None` if it does not exist. `[audio-thread]`
    #[inline]
    #[must_use]
    pub fn input(&self, bus: usize) -> Option<AudioBufferRef<'a, T>> {
        self.inputs.get(bus).copied()
    }

    /// Output bus `bus`, or `None` if it does not exist. `[audio-thread]`
    ///
    /// The returned view is a reborrow: writes reach the host's memory immediately, but
    /// changes to its [constant mask] apply only to the copy. Use [`output_slot_mut`] when
    /// the mask has to survive.
    ///
    /// [constant mask]: AudioBufferMut::constant_mask
    /// [`output_slot_mut`]: AudioBuses::output_slot_mut
    #[inline]
    #[must_use]
    pub fn output(&mut self, bus: usize) -> Option<AudioBufferMut<'_, T>> {
        self.outputs.get_mut(bus).map(AudioBufferMut::reborrow)
    }

    /// The stored view for output bus `bus`, so that changes to its constant mask persist
    /// for the adapter to hand back to the host. `[audio-thread]`
    #[inline]
    #[must_use]
    pub fn output_slot_mut(&mut self, bus: usize) -> Option<&mut AudioBufferMut<'a, T>> {
        self.outputs.get_mut(bus)
    }

    /// The main input bus, i.e. bus `0`. `[audio-thread]`
    #[inline]
    #[must_use]
    pub fn main_input(&self) -> Option<AudioBufferRef<'a, T>> {
        self.input(0)
    }

    /// The main output bus, i.e. bus `0`. `[audio-thread]`
    #[inline]
    #[must_use]
    pub fn main_output(&mut self) -> Option<AudioBufferMut<'_, T>> {
        self.output(0)
    }

    /// All input buses. `[audio-thread]`
    #[inline]
    #[must_use]
    pub fn inputs(&self) -> &'a [AudioBufferRef<'a, T>] {
        self.inputs
    }

    /// All output buses, read-only. `[audio-thread]`
    #[inline]
    #[must_use]
    pub fn outputs(&self) -> &[AudioBufferMut<'a, T>] {
        self.outputs
    }

    /// All output buses, writable — for adapters that need to touch several at once.
    /// `[audio-thread]`
    #[inline]
    #[must_use]
    pub fn outputs_mut(&mut self) -> &mut [AudioBufferMut<'a, T>] {
        self.outputs
    }

    /// Iterates the input buses. `[audio-thread]`
    #[inline]
    pub fn iter_inputs(&self) -> impl ExactSizeIterator<Item = AudioBufferRef<'a, T>> {
        self.inputs.iter().copied()
    }

    /// Total number of input channels across all buses. `[audio-thread]`
    #[must_use]
    pub fn total_input_channels(&self) -> usize {
        self.inputs.iter().map(AudioBufferRef::channel_count).sum()
    }

    /// Total number of output channels across all buses. `[audio-thread]`
    #[must_use]
    pub fn total_output_channels(&self) -> usize {
        self.outputs.iter().map(AudioBufferMut::channel_count).sum()
    }

    /// Writes silence to every output bus and marks every output channel constant.
    /// `[audio-thread]`
    ///
    /// The usual response to a bypass or an error: never leave the host's output buffers
    /// holding whatever was in them.
    pub fn silence_outputs(&mut self) {
        for bus in self.outputs.iter_mut() {
            bus.fill_silence();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::AudioStorage;

    #[test]
    fn empty_buses_still_report_the_block_length() {
        let mut buses = AudioBuses::<f32>::empty(512);
        assert_eq!(buses.frames(), 512);
        assert_eq!(buses.input_count(), 0);
        assert_eq!(buses.output_count(), 0);
        assert!(buses.input(0).is_none());
        assert!(buses.output(0).is_none());
        assert!(buses.main_input().is_none());
        assert!(buses.main_output().is_none());
        assert!(buses.output_slot_mut(0).is_none());
        assert_eq!(buses.total_input_channels(), 0);
        assert_eq!(buses.total_output_channels(), 0);
        assert!(buses.inputs().is_empty());
        assert!(buses.outputs().is_empty());
        assert!(buses.outputs_mut().is_empty());
        assert_eq!(buses.iter_inputs().count(), 0);
        // Silencing nothing is not an error.
        buses.silence_outputs();
        assert!(format!("{buses:?}").contains("frames: 512"));
    }

    #[test]
    fn main_and_auxiliary_buses() {
        let main_in = AudioStorage::<f32>::new(2, 8);
        let side_in = AudioStorage::<f32>::new(1, 8);
        let mut main_out = AudioStorage::<f32>::new(2, 8);
        let mut aux_out = AudioStorage::<f32>::new(4, 8);

        {
            let inputs = [main_in.as_ref(), side_in.as_ref()];
            let mut outputs = [main_out.as_mut(), aux_out.as_mut()];
            let mut buses = AudioBuses::new(&inputs, &mut outputs, 8);

            assert_eq!(buses.frames(), 8);
            assert_eq!(buses.input_count(), 2);
            assert_eq!(buses.output_count(), 2);
            assert_eq!(buses.total_input_channels(), 3);
            assert_eq!(buses.total_output_channels(), 6);
            assert_eq!(buses.main_input().unwrap().channel_count(), 2);
            assert_eq!(buses.input(1).unwrap().channel_count(), 1);
            assert!(buses.input(2).is_none());
            assert_eq!(buses.main_output().unwrap().channel_count(), 2);
            assert_eq!(buses.output(1).unwrap().channel_count(), 4);
            assert!(buses.output(2).is_none());
            assert_eq!(buses.iter_inputs().len(), 2);
            assert_eq!(buses.inputs().len(), 2);
            assert_eq!(buses.outputs().len(), 2);

            // Writing through a bus reaches the underlying storage.
            buses.main_output().unwrap().fill(0.5);
            buses.output(1).unwrap().fill(-0.5);
        }
        assert!(main_out.as_slice().iter().all(|&v| v == 0.5));
        assert!(aux_out.as_slice().iter().all(|&v| v == -0.5));
    }

    #[test]
    fn output_masks_persist_only_through_the_slot() {
        let mut out = AudioStorage::<f32>::new(2, 4);
        let mut outputs = [out.as_mut()];
        let mut buses = AudioBuses::new(&[], &mut outputs, 4);

        // A reborrowed view carries a copy of the mask.
        buses.output(0).unwrap().set_constant_mask(0b11);
        assert_eq!(buses.output(0).unwrap().constant_mask(), 0);

        // The slot is the stored view, so the mask survives.
        buses.output_slot_mut(0).unwrap().set_constant_mask(0b11);
        assert_eq!(buses.output(0).unwrap().constant_mask(), 0b11);
        assert_eq!(buses.outputs()[0].constant_mask(), 0b11);
    }

    #[test]
    fn silence_outputs_clears_every_bus() {
        let mut a = AudioStorage::<f32>::new(2, 4);
        let mut b = AudioStorage::<f32>::new(1, 4);
        a.fill(1.0);
        b.fill(1.0);
        {
            let mut outputs = [a.as_mut(), b.as_mut()];
            let mut buses = AudioBuses::new(&[], &mut outputs, 4);
            buses.silence_outputs();
            assert_eq!(buses.outputs()[0].constant_mask(), 0b11);
            assert_eq!(buses.outputs()[1].constant_mask(), 0b1);
        }
        assert!(a.as_slice().iter().all(|&v| v == 0.0));
        assert!(b.as_slice().iter().all(|&v| v == 0.0));
    }

    #[test]
    fn an_instrument_has_no_input_buses() {
        let mut out = AudioStorage::<f32>::new(2, 16);
        let mut outputs = [out.as_mut()];
        let mut buses = AudioBuses::new(&[], &mut outputs, 16);
        assert_eq!(buses.input_count(), 0);
        assert!(buses.main_input().is_none());
        assert_eq!(buses.output_count(), 1);
        assert_eq!(buses.main_output().unwrap().frames(), 16);
    }

    #[test]
    fn in_place_processing_shares_one_allocation() {
        // The host hands the same memory in as input and out as output. The input view is
        // built from the output view's own channel pointers, so both address exactly the
        // same samples — the case `abi-v1` §8 explicitly allows.
        let mut storage = AudioStorage::<f32>::new(2, 4);
        storage.fill(2.0);
        {
            let mut output = storage.as_mut();
            let ptrs: Vec<*const f32> = (0..output.channel_count())
                .map(|c| output.channel_ptr_mut(c).unwrap().cast_const())
                .collect();
            // SAFETY: the pointers were just read out of `output`, which stays alive for
            // the whole block and guarantees 4 readable samples per channel. The shared
            // view is only read, and the two views never create a `&`/`&mut` pair over the
            // same sample at the same time.
            let input = unsafe { AudioBufferRef::<f32>::from_raw(ptrs.as_ptr(), 2, 4) };

            let inputs = [input];
            let mut outputs = [output];
            let mut buses = AudioBuses::new(&inputs, &mut outputs, 4);
            assert_eq!(buses.main_input().unwrap().sample(0, 0), Some(2.0));
            // A trivial in-place gain: read each frame, write it back doubled.
            let gain = 2.0;
            {
                let mut out = buses.main_output().unwrap();
                for mut frame in out.iter_frames_mut() {
                    for c in 0..frame.channel_count() {
                        let v = frame.get(c).unwrap();
                        frame.set(c, v * gain);
                    }
                }
            }
            // The change is visible through the input view: it is the same memory.
            assert_eq!(buses.main_input().unwrap().sample(1, 3), Some(4.0));
        }
        assert!(storage.as_slice().iter().all(|&v| v == 4.0));
    }

    #[test]
    fn buses_are_send_and_sync() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<AudioBuses<'_, f32>>();
        assert_sync::<AudioBuses<'_, f32>>();
    }
}
