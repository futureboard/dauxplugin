//! Assembling one block's bus views from `DauxProcessV1` (abi-v1 §8).
//!
//! The host hands over an array of [`DauxAudioBufferV1`] per direction; a plug-in expects an
//! [`AudioBuses`]. The two arrays of views that bridge them are allocated once in `activate`
//! and refilled in place every block, because `process` must not allocate.
//!
//! # What is checked, and why
//!
//! `channel_count` and `frame_count` are the host's word and cannot be verified — but
//! everything derived from them can be, and each check below corresponds to a way a real host
//! gets it wrong:
//!
//! * a bus array pointer that is null while the count is non-zero;
//! * a `data32`/`data64` array that is null, or that is the *other* one than the activated
//!   sample format promised (abi-v1 §8 requires exactly one to be non-null and to match);
//! * an individual channel pointer that is null;
//! * more buses than the plug-in declared through `daux.audio-ports/1`.
//!
//! The first three make the block unusable and are reported as `DAUX_PROCESS_ERROR`; the last
//! is survivable, so the extra buses are simply not shown to the plug-in.

use daux_abi::{DauxAudioBufferV1, DauxProcessV1};
use daux_plugin_api::daux_rt::FixedVec;
use daux_plugin_api::{AudioBufferMut, AudioBufferRef, AudioBuses, Sample, SampleFormat};

/// Why a block could not be assembled. Every variant means `DAUX_PROCESS_ERROR`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BlockError {
    /// A bus array pointer was null while its count said otherwise.
    MissingBusArray,
    /// A bus had no channel-pointer array for the activated sample format, or an individual
    /// channel pointer was null.
    MalformedBus,
}

/// The preallocated per-block bus views, one pair per sample format.
///
/// Both formats are sized because `activate` fixes the format for the activation but this
/// struct outlives one activation, and re-allocating on a format change would be an allocation
/// on a path that must not have one.
pub(crate) struct AudioScratch {
    inputs32: FixedVec<AudioBufferRef<'static, f32>>,
    outputs32: FixedVec<AudioBufferMut<'static, f32>>,
    inputs64: FixedVec<AudioBufferRef<'static, f64>>,
    outputs64: FixedVec<AudioBufferMut<'static, f64>>,
}

impl AudioScratch {
    /// [main-thread] Allocates room for `inputs` input and `outputs` output buses.
    ///
    /// This is the only allocating operation on the type; call it from `activate`.
    pub(crate) fn new(inputs: usize, outputs: usize) -> Self {
        Self {
            inputs32: FixedVec::with_capacity(inputs),
            outputs32: FixedVec::with_capacity(outputs),
            inputs64: FixedVec::with_capacity(inputs),
            outputs64: FixedVec::with_capacity(outputs),
        }
    }

    /// [audio-thread] Forgets every view. Called at the end of each block.
    ///
    /// Not tidiness: it is what makes the `'static` the views are *stored* with harmless, since
    /// no view outlives the `process` call that built it.
    pub(crate) fn clear(&mut self) {
        self.inputs32.clear();
        self.outputs32.clear();
        self.inputs64.clear();
        self.outputs64.clear();
    }

    /// [audio-thread] Fills the `f32` views from `block`.
    ///
    /// # Safety
    ///
    /// `block` is a validated [`DauxProcessV1`] whose bus arrays and channel pointers stay
    /// valid, and are not otherwise aliased, until the current `process` call returns
    /// (abi-v1 §16.3).
    pub(crate) unsafe fn fill_f32(&mut self, block: &DauxProcessV1) -> Result<(), BlockError> {
        self.clear();
        let frames = block.frame_count as usize;
        // SAFETY: forwarded verbatim from this function's own contract.
        unsafe {
            fill_inputs(
                &mut self.inputs32,
                block.audio_inputs,
                block.audio_input_count,
                frames,
                SampleFormat::F32,
            )?;
            fill_outputs(
                &mut self.outputs32,
                block.audio_outputs,
                block.audio_output_count,
                frames,
                SampleFormat::F32,
            )
        }
    }

    /// [audio-thread] Fills the `f64` views from `block`. See [`AudioScratch::fill_f32`].
    ///
    /// # Safety
    ///
    /// As [`AudioScratch::fill_f32`].
    pub(crate) unsafe fn fill_f64(&mut self, block: &DauxProcessV1) -> Result<(), BlockError> {
        self.clear();
        let frames = block.frame_count as usize;
        // SAFETY: forwarded verbatim from this function's own contract.
        unsafe {
            fill_inputs(
                &mut self.inputs64,
                block.audio_inputs,
                block.audio_input_count,
                frames,
                SampleFormat::F64,
            )?;
            fill_outputs(
                &mut self.outputs64,
                block.audio_outputs,
                block.audio_output_count,
                frames,
                SampleFormat::F64,
            )
        }
    }

    /// [audio-thread] The `f32` buses for this block.
    pub(crate) fn buses_f32(&mut self, frames: usize) -> AudioBuses<'_, f32> {
        let Self {
            inputs32,
            outputs32,
            ..
        } = self;
        // SAFETY: the only change is shortening the lifetime parameter of the element type
        // from `'static` to the borrow of `self`, which is always valid — `&mut` is invariant,
        // so the compiler cannot do it, but no pointer, length or provenance changes. The
        // `'static` the elements are *stored* with is never observed: `fill_*` rebuilds them at
        // the start of every block from the host's pointers, `clear` empties the vector at the
        // end of it, and nothing reads an element in between except through this borrow.
        let outputs: &mut [AudioBufferMut<'_, f32>] =
            unsafe { core::mem::transmute(outputs32.as_mut_slice()) };
        AudioBuses::new(inputs32.as_slice(), outputs, frames)
    }

    /// [audio-thread] The `f64` buses for this block. See [`AudioScratch::buses_f32`].
    pub(crate) fn buses_f64(&mut self, frames: usize) -> AudioBuses<'_, f64> {
        let Self {
            inputs64,
            outputs64,
            ..
        } = self;
        // SAFETY: as `buses_f32`.
        let outputs: &mut [AudioBufferMut<'_, f64>] =
            unsafe { core::mem::transmute(outputs64.as_mut_slice()) };
        AudioBuses::new(inputs64.as_slice(), outputs, frames)
    }
}

/// The channel-pointer array of `bus` for `T`, with the channel count that is safe to build a
/// view from.
///
/// The count comes back as `0` for a bus whose array is absent in a zero-frame block: nothing
/// would ever be read through it, and a view that claims channels it has no pointers for is
/// exactly the shape that turns a host's mistake into undefined behaviour.
///
/// # Safety
///
/// `bus` is a live [`DauxAudioBufferV1`] whose channel arrays stay valid for the call.
unsafe fn channel_array<T: Sample>(
    bus: &DauxAudioBufferV1,
    frames: usize,
    format: SampleFormat,
) -> Result<(*const *const T, usize), BlockError> {
    let channels = bus.channel_count as usize;
    let raw = match format {
        SampleFormat::F32 => bus.data32.cast::<*const T>(),
        SampleFormat::F64 => bus.data64.cast::<*const T>(),
    };
    if raw.is_null() {
        // A missing array is only survivable when nothing would be read through it, which is
        // the case for a zero-channel bus and for an empty block.
        if channels == 0 || frames == 0 {
            return Ok((raw, 0));
        }
        return Err(BlockError::MalformedBus);
    }
    if channels == 0 || frames == 0 {
        return Ok((raw, channels));
    }
    for index in 0..channels {
        // SAFETY: the caller guarantees the array holds `channel_count` initialised pointers,
        // and `index` is below that count.
        if unsafe { (*raw.add(index)).is_null() } {
            return Err(BlockError::MalformedBus);
        }
    }
    Ok((raw, channels))
}

/// Reads the bus array the host published, or `None` when it is absent.
///
/// # Safety
///
/// `buses` is null or points at `count` initialised [`DauxAudioBufferV1`]s valid for the call.
unsafe fn bus_slice<'a>(
    buses: *const DauxAudioBufferV1,
    count: u32,
) -> Result<&'a [DauxAudioBufferV1], BlockError> {
    if count == 0 {
        return Ok(&[]);
    }
    if buses.is_null() {
        return Err(BlockError::MissingBusArray);
    }
    // SAFETY: non-null was checked, and the caller guarantees `count` initialised elements that
    // stay valid for the whole `process` call, which outlives `'a` here.
    Ok(unsafe { core::slice::from_raw_parts(buses, count as usize) })
}

/// # Safety
///
/// As [`AudioScratch::fill_f32`].
unsafe fn fill_inputs<T: Sample>(
    views: &mut FixedVec<AudioBufferRef<'static, T>>,
    buses: *const DauxAudioBufferV1,
    count: u32,
    frames: usize,
    format: SampleFormat,
) -> Result<(), BlockError> {
    // SAFETY: forwarded verbatim from this function's own contract.
    let buses = unsafe { bus_slice(buses, count) }?;
    for bus in buses {
        if views.is_full() {
            // More buses than the plug-in declared. Showing it fewer is defined behaviour;
            // growing the vector here would allocate on the audio thread.
            break;
        }
        // SAFETY: forwarded verbatim from this function's own contract.
        let (array, channels) = unsafe { channel_array::<T>(bus, frames, format) }?;
        // SAFETY: `channel_array` established that the array is non-null whenever `channels`
        // is non-zero, and that when `frames > 0` every one of its `channels` entries is
        // non-null; abi-v1 §8 makes each of them point at `frame_count` samples that stay
        // valid for the call. The `'static` chosen here is immediately narrowed by
        // `buses_f32`/`buses_f64`, which are the only ways the view is ever read.
        let view = unsafe {
            AudioBufferRef::<'static, T>::from_raw_with_mask(
                array,
                channels,
                frames,
                bus.constant_mask,
            )
        };
        // `is_full` was checked above, so this cannot fail.
        let _ = views.push(view);
    }
    Ok(())
}

/// # Safety
///
/// As [`AudioScratch::fill_f32`], plus: the output channel regions do not overlap one another,
/// which abi-v1 §8 requires of a host handing out writable buffers.
unsafe fn fill_outputs<T: Sample>(
    views: &mut FixedVec<AudioBufferMut<'static, T>>,
    buses: *mut DauxAudioBufferV1,
    count: u32,
    frames: usize,
    format: SampleFormat,
) -> Result<(), BlockError> {
    // SAFETY: forwarded verbatim from this function's own contract.
    let buses = unsafe { bus_slice(buses.cast_const(), count) }?;
    for bus in buses {
        if views.is_full() {
            break;
        }
        // SAFETY: forwarded verbatim from this function's own contract.
        let (array, channels) = unsafe { channel_array::<T>(bus, frames, format) }?;
        // SAFETY: as the input case, with reads upgraded to writes: the host published these
        // buffers as outputs, so they are writable for the call, and abi-v1 §8 requires the
        // channels of one output bus not to overlap each other.
        let view = unsafe {
            AudioBufferMut::<'static, T>::from_raw_with_mask(
                array.cast::<*mut T>(),
                channels,
                frames,
                bus.constant_mask,
            )
        };
        let _ = views.push(view);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A host-side block: owned sample storage plus the pointer arrays the ABI wants.
    struct FakeBlock {
        samples: Vec<Vec<f32>>,
        channel_ptrs: Vec<Vec<*mut f32>>,
        buses: Vec<DauxAudioBufferV1>,
    }

    impl FakeBlock {
        fn new(buses: &[usize], frames: usize) -> Self {
            let mut samples = Vec::new();
            for (bus, channels) in buses.iter().enumerate() {
                for channel in 0..*channels {
                    samples.push(vec![(bus * 10 + channel) as f32; frames]);
                }
            }
            let mut this = Self {
                samples,
                channel_ptrs: Vec::new(),
                buses: Vec::new(),
            };
            let mut next = 0usize;
            for channels in buses {
                let mut ptrs = Vec::new();
                for _ in 0..*channels {
                    ptrs.push(this.samples[next].as_mut_ptr());
                    next += 1;
                }
                this.channel_ptrs.push(ptrs);
            }
            for (index, channels) in buses.iter().enumerate() {
                let mut abi = DauxAudioBufferV1::new();
                abi.channel_count = *channels as u32;
                abi.data32 = this.channel_ptrs[index].as_ptr();
                this.buses.push(abi);
            }
            this
        }

        fn ptr(&self) -> *const DauxAudioBufferV1 {
            self.buses.as_ptr()
        }

        fn ptr_mut(&mut self) -> *mut DauxAudioBufferV1 {
            self.buses.as_mut_ptr()
        }

        fn count(&self) -> u32 {
            self.buses.len() as u32
        }
    }

    fn block(input: &FakeBlock, output: &mut FakeBlock, frames: u32) -> DauxProcessV1 {
        let mut b = DauxProcessV1::new();
        b.frame_count = frames;
        b.audio_input_count = input.count();
        b.audio_inputs = input.ptr();
        b.audio_output_count = output.count();
        b.audio_outputs = output.ptr_mut();
        b
    }

    #[test]
    fn a_well_formed_block_becomes_buses_a_plugin_can_use() {
        const FRAMES: usize = 8;
        let input = FakeBlock::new(&[2, 1], FRAMES);
        let mut output = FakeBlock::new(&[2], FRAMES);
        let abi = block(&input, &mut output, FRAMES as u32);

        let mut scratch = AudioScratch::new(2, 1);
        // SAFETY: `input` and `output` own their storage and outlive this scope.
        unsafe { scratch.fill_f32(&abi) }.expect("well formed");

        {
            let mut buses = scratch.buses_f32(FRAMES);
            assert_eq!(buses.input_count(), 2);
            assert_eq!(buses.output_count(), 1);
            assert_eq!(buses.frames(), FRAMES);
            assert_eq!(buses.main_input().unwrap().sample(1, 0), Some(1.0));
            assert_eq!(buses.input(1).unwrap().channel_count(), 1);

            // Writing through the view really does reach the host's memory.
            buses.main_output().unwrap().fill(0.25);
        }
        scratch.clear();
        assert!(output.samples.iter().all(|c| c.iter().all(|s| *s == 0.25)));
    }

    #[test]
    fn extra_buses_are_dropped_rather_than_allocated_for() {
        const FRAMES: usize = 4;
        let input = FakeBlock::new(&[1, 1, 1], FRAMES);
        let mut output = FakeBlock::new(&[1, 1], FRAMES);
        let abi = block(&input, &mut output, FRAMES as u32);

        // The plug-in declared one bus in each direction.
        let mut scratch = AudioScratch::new(1, 1);
        // SAFETY: the fake block outlives this scope.
        unsafe { scratch.fill_f32(&abi) }.expect("well formed");
        let buses = scratch.buses_f32(FRAMES);
        assert_eq!(buses.input_count(), 1);
        assert_eq!(buses.output_count(), 1);
    }

    #[test]
    fn a_null_bus_array_with_a_non_zero_count_is_refused() {
        let input = FakeBlock::new(&[1], 4);
        let mut output = FakeBlock::new(&[1], 4);
        let mut abi = block(&input, &mut output, 4);
        abi.audio_inputs = core::ptr::null();

        let mut scratch = AudioScratch::new(1, 1);
        // SAFETY: the fake block outlives this scope; the null array is the case under test.
        let result = unsafe { scratch.fill_f32(&abi) };
        assert_eq!(result, Err(BlockError::MissingBusArray));
    }

    #[test]
    fn a_bus_with_the_wrong_sample_format_is_refused() {
        const FRAMES: usize = 4;
        let input = FakeBlock::new(&[1], FRAMES);
        let mut output = FakeBlock::new(&[1], FRAMES);
        let abi = block(&input, &mut output, FRAMES as u32);

        let mut scratch = AudioScratch::new(1, 1);
        // The host published `data32`, but the activation promised `f64` — exactly one of the
        // two is non-null and it must be the activated one (abi-v1 §8).
        // SAFETY: the fake block outlives this scope.
        let result = unsafe { scratch.fill_f64(&abi) };
        assert_eq!(result, Err(BlockError::MalformedBus));
    }

    #[test]
    fn a_null_channel_pointer_is_refused_before_it_is_dereferenced() {
        const FRAMES: usize = 4;
        let mut input = FakeBlock::new(&[2], FRAMES);
        input.channel_ptrs[0][1] = core::ptr::null_mut();
        input.buses[0].data32 = input.channel_ptrs[0].as_ptr();
        let mut output = FakeBlock::new(&[1], FRAMES);
        let abi = block(&input, &mut output, FRAMES as u32);

        let mut scratch = AudioScratch::new(1, 1);
        // SAFETY: the fake block outlives this scope; the null channel is the case under test.
        let result = unsafe { scratch.fill_f32(&abi) };
        assert_eq!(result, Err(BlockError::MalformedBus));
    }

    #[test]
    fn a_zero_frame_block_tolerates_null_channel_arrays() {
        let mut input = FakeBlock::new(&[2], 0);
        input.buses[0].data32 = core::ptr::null();
        let mut output = FakeBlock::new(&[1], 0);
        output.buses[0].data32 = core::ptr::null();
        let abi = block(&input, &mut output, 0);

        let mut scratch = AudioScratch::new(1, 1);
        // SAFETY: nothing is ever read through the arrays when `frames == 0`.
        unsafe { scratch.fill_f32(&abi) }.expect("zero frames is not malformed");
        assert_eq!(scratch.buses_f32(0).frames(), 0);
    }

    #[test]
    fn the_constant_mask_is_carried_across() {
        const FRAMES: usize = 4;
        let mut input = FakeBlock::new(&[2], FRAMES);
        input.buses[0].constant_mask = 0b10;
        let mut output = FakeBlock::new(&[1], FRAMES);
        let abi = block(&input, &mut output, FRAMES as u32);

        let mut scratch = AudioScratch::new(1, 1);
        // SAFETY: the fake block outlives this scope.
        unsafe { scratch.fill_f32(&abi) }.expect("well formed");
        let buses = scratch.buses_f32(FRAMES);
        let main = buses.main_input().unwrap();
        assert!(!main.is_channel_constant(0));
        assert!(main.is_channel_constant(1));
    }

    #[test]
    fn refilling_reuses_the_same_storage_every_block() {
        const FRAMES: usize = 4;
        let input = FakeBlock::new(&[1], FRAMES);
        let mut output = FakeBlock::new(&[1], FRAMES);
        let abi = block(&input, &mut output, FRAMES as u32);
        let mut scratch = AudioScratch::new(1, 1);

        for _ in 0..3 {
            // SAFETY: the fake block outlives this scope.
            unsafe { scratch.fill_f32(&abi) }.expect("well formed");
            assert_eq!(scratch.buses_f32(FRAMES).input_count(), 1);
            scratch.clear();
        }
        assert_eq!(scratch.buses_f32(FRAMES).input_count(), 0);
    }
}
