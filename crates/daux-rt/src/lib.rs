//! Real-time safe primitives for audio-thread code: lock-free queues, bounded
//! buffers, thread markers.
//!
//! Everything here exists to make one rule enforceable: **the audio thread never
//! allocates, never locks, never blocks and never panics.** The types in this
//! crate allocate exactly once, in a constructor that is documented
//! `[main-thread]`, and then behave predictably forever after.
//!
//! # What to reach for
//!
//! | Need | Type |
//! | ---- | ---- |
//! | Hand every item from one thread to one other thread | [`SpscRingBuffer`] |
//! | Hand every item from many threads to one thread | [`MpscQueue`] |
//! | Show the newest value to the UI, dropping the rest | [`TripleBuffer`] |
//! | Share a single float between threads | [`AtomicF32`], [`AtomicF64`] |
//! | A `Vec` that must never reallocate | [`FixedVec`] |
//! | Per-channel scratch memory | [`ScratchBuffers`] |
//! | Log from `process` | [`RtLogQueue`] |
//! | Catch wrong-thread bugs in debug builds | [`rt_assert_audio_thread!`] |
//! | Prove in a test that code did not allocate | [`AllocGuard`] |
//!
//! The distinction between the first three is the interesting one. A ring buffer
//! and an MPSC queue *keep* every item and fail loudly when full, returning the
//! rejected value in [`Full`] so nothing is ever lost silently. A triple buffer
//! deliberately *drops* intermediate values: the writer never waits, and the
//! reader always gets the most recent complete value. Meters and spectra want the
//! triple buffer; note events and parameter changes want a queue.
//!
//! # Thread annotations
//!
//! Every public item carries `[audio-thread]`, `[main-thread]` or `[any-thread]`
//! in its documentation, matching `docs/specifications/abi-v1.md` §15.
//! `[audio-thread]` means the operation is allocation-free, lock-free and
//! non-blocking, with the panic conditions spelled out where they exist.
//!
//! # Example
//!
//! ```
//! use daux_rt::{AtomicF32, ScratchBuffers, SpscRingBuffer, TripleBuffer};
//!
//! // prepare(): everything the audio thread will ever need is allocated here.
//! let mut scratch = ScratchBuffers::<f32>::new(2, 512);
//! let (mut commands, mut inbox) = SpscRingBuffer::with_capacity::<u32>(64);
//! let (mut meter_out, mut meter_in) = TripleBuffer::new(0.0f32);
//! let gain = AtomicF32::new(1.0);
//!
//! // process(): no allocation, no locking, no waiting.
//! commands.push(7).unwrap();
//! while let Some(_command) = inbox.pop() {}
//! let block = scratch.slice_mut(0, 64);
//! block.fill(gain.get());
//! meter_out.write(block.iter().fold(0.0f32, |peak, s| peak.max(s.abs())));
//!
//! // The UI thread, whenever it gets around to it:
//! assert_eq!(*meter_in.read(), 1.0);
//! ```
//!
//! This crate has no external dependencies, by architectural rule.

mod alloc_probe;
mod atomic;
mod cache;
mod error;
mod fixed_vec;
mod log;
mod mpsc;
mod scratch;
mod spsc;
mod thread;
mod triple;

pub use crate::alloc_probe::{
    AllocGuard, CountingAllocator, alloc_count, counting_allocator_installed, dealloc_count,
    thread_alloc_count,
};
pub use crate::atomic::{AtomicF32, AtomicF64};
pub use crate::error::{CapacityError, Full};
pub use crate::fixed_vec::FixedVec;
pub use crate::log::{LogLevel, RT_LOG_MESSAGE_BYTES, RtLogQueue, RtLogRecord};
pub use crate::mpsc::MpscQueue;
pub use crate::scratch::ScratchBuffers;
pub use crate::spsc::{Consumer, Producer, SpscRingBuffer};
pub use crate::thread::{
    ThreadClass, ThreadClassGuard, assert_audio_thread, current_thread_class,
    replace_current_thread_class, set_current_thread_class,
};
pub use crate::triple::{TripleBuffer, TripleReader, TripleWriter};

/// The crate's own tests run under the counting allocator so that "this does not
/// allocate" is checked rather than asserted. Nothing outside `cfg(test)` picks
/// this up, so production builds keep the platform allocator untouched.
#[cfg(test)]
#[global_allocator]
static TEST_ALLOCATOR: CountingAllocator = CountingAllocator;

#[cfg(test)]
mod tests {
    use super::{
        AtomicF32, Consumer, FixedVec, MpscQueue, Producer, RtLogQueue, ScratchBuffers,
        SpscRingBuffer, TripleBuffer, TripleReader, TripleWriter,
    };

    /// The contract requires that types crossing to the audio thread are `Send`
    /// and types shared with the UI are `Sync`. A compile-time check is worth
    /// more than a comment, because an accidental `Rc` in a field would
    /// otherwise only surface in a downstream crate.
    const fn assert_send<T: Send>() {}
    const fn assert_sync<T: Sync>() {}

    #[test]
    fn the_public_types_have_the_thread_bounds_the_contract_promises() {
        assert_send::<Producer<u32>>();
        assert_send::<Consumer<u32>>();
        assert_send::<MpscQueue<u32>>();
        assert_sync::<MpscQueue<u32>>();
        assert_send::<TripleWriter<f32>>();
        assert_send::<TripleReader<f32>>();
        assert_send::<AtomicF32>();
        assert_sync::<AtomicF32>();
        assert_send::<FixedVec<f32>>();
        assert_sync::<FixedVec<f32>>();
        assert_send::<ScratchBuffers<f32>>();
        assert_sync::<ScratchBuffers<f32>>();
        assert_send::<RtLogQueue>();
        assert_sync::<RtLogQueue>();
    }

    /// A full block's worth of work through every primitive at once, in the order
    /// a real `process` would use them, with the allocator watching.
    #[test]
    fn a_whole_process_block_allocates_nothing() {
        // prepare(): the only place that is allowed to allocate.
        let mut scratch = ScratchBuffers::<f32>::new(2, 512);
        let (mut to_audio, mut from_ui) = SpscRingBuffer::with_capacity::<f32>(16);
        let events = MpscQueue::<u32>::with_capacity(16);
        let (mut meter_out, mut meter_in) = TripleBuffer::new([0.0f32; 2]);
        let mut voices = FixedVec::<u32>::with_capacity(32);
        let log = RtLogQueue::with_capacity(8);
        let gain = AtomicF32::new(0.5);

        let ((), allocations) = crate::AllocGuard::scope(|| {
            for block in 0..256u32 {
                // Parameter changes arriving from the UI.
                to_audio.push(block as f32 / 256.0).ok();
                while let Some(value) = from_ui.pop() {
                    gain.set(value);
                }
                // Events arriving from anywhere.
                events.try_push(block).ok();
                while let Some(event) = events.pop() {
                    if voices.push(event).is_err() {
                        voices.clear();
                        log.try_log(crate::LogLevel::Warn, "voice list overflowed");
                    }
                }
                // The DSP itself.
                let g = gain.get();
                let mut peaks = [0.0f32; 2];
                for (channel, buffer) in scratch.iter_channels_mut().enumerate() {
                    let block_len = 128.min(buffer.len());
                    for (i, sample) in buffer[..block_len].iter_mut().enumerate() {
                        *sample = g * (i as f32 - 64.0) / 64.0;
                        peaks[channel] = peaks[channel].max(sample.abs());
                    }
                }
                // The snapshot the UI will pick up whenever it likes.
                meter_out.write(peaks);
            }
        });

        assert_eq!(allocations, 0, "a process block allocated");
        assert!(meter_in.read()[0] > 0.0);
        assert!(log.len() <= log.capacity());
    }
}
