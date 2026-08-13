//! Owned, preallocated planar storage for hosts, tests and offline rendering.

use core::fmt;
use core::mem;

use crate::buffer::{AudioBufferMut, AudioBufferRef};
use crate::error::{AudioError, AudioResult};
use crate::sample::Sample;

/// Owned planar audio storage. `[main-thread]`
///
/// One contiguous allocation of `channels * frames` samples plus one array of channel
/// pointers into it, so it can be handed to anything that expects the DAUx planar
/// representation with no copying and no per-block bookkeeping. Channels are laid out one
/// after another and never overlap, which is exactly what [`AudioBufferMut::from_raw`]
/// requires.
///
/// Only [`AudioStorage::new`], [`AudioStorage::clone`] and the interleaving helpers
/// allocate. [`as_ref`] and [`as_mut`] are `O(1)` and allocation-free, so a preallocated
/// `AudioStorage` may be *used* from the audio thread even though it must be *created* off
/// it.
///
/// [`as_ref`]: AudioStorage::as_ref
/// [`as_mut`]: AudioStorage::as_mut
pub struct AudioStorage<T: Sample> {
    /// Owning pointer to `channels * frames` samples, from a `Vec<T>` decomposed with
    /// [`mem::forget`] so that no `Vec`/`Box` uniqueness assertion can ever invalidate the
    /// channel pointers derived from it.
    data: *mut T,
    data_len: usize,
    data_cap: usize,
    /// Owning pointer to `channels` channel pointers, decomposed the same way.
    ptrs: *mut *mut T,
    ptrs_cap: usize,
    channels: usize,
    frames: usize,
}

// SAFETY: `AudioStorage` uniquely owns both allocations — the raw pointers are the only
// handles to them, they are never shared, and `Drop` frees them exactly once. With
// `T: Sample` (itself `Send`), moving the whole owner to another thread is sound.
unsafe impl<T: Sample> Send for AudioStorage<T> {}
// SAFETY: every mutating method takes `&mut self`, so `&AudioStorage` grants read-only
// access to `T: Sync` samples, exactly like `&[T]`.
unsafe impl<T: Sample> Sync for AudioStorage<T> {}

impl<T: Sample> AudioStorage<T> {
    /// Allocates `channels * frames` samples of silence. `[main-thread]` — **allocates**.
    ///
    /// # Panics
    ///
    /// If `channels * frames` overflows `usize`, or if the allocator fails.
    #[must_use]
    pub fn new(channels: usize, frames: usize) -> Self {
        let total = channels
            .checked_mul(frames)
            .expect("daux-audio: channels * frames overflows usize");

        let mut data: Vec<T> = vec![T::ZERO; total];
        let data_ptr = data.as_mut_ptr();
        let data_cap = data.capacity();
        // The allocation now has exactly one owner: `data_ptr`. Decomposing the `Vec`
        // instead of storing it keeps the channel pointers below free of any aliasing
        // assertion a live `Vec`/`Box` field would impose.
        mem::forget(data);

        let mut ptrs: Vec<*mut T> = Vec::with_capacity(channels);
        for c in 0..channels {
            // SAFETY: `c < channels`, so `c * frames <= total`, i.e. the offset is inside
            // the allocation or one past its end (only when `frames == 0`, where the
            // pointer is never dereferenced). Both cases are valid for `add`.
            ptrs.push(unsafe { data_ptr.add(c * frames) });
        }
        let ptrs_ptr = ptrs.as_mut_ptr();
        let ptrs_cap = ptrs.capacity();
        mem::forget(ptrs);

        Self {
            data: data_ptr,
            data_len: total,
            data_cap,
            ptrs: ptrs_ptr,
            ptrs_cap,
            channels,
            frames,
        }
    }

    /// Number of channels. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn channel_count(&self) -> usize {
        self.channels
    }

    /// Number of frames per channel. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn frames(&self) -> usize {
        self.frames
    }

    /// `true` when the storage holds no samples. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.data_len == 0
    }

    /// Total number of samples, `channels * frames`. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn sample_count(&self) -> usize {
        self.data_len
    }

    /// A read-only view of the whole buffer. `[audio-thread]` — allocation-free.
    #[inline]
    #[must_use]
    #[allow(
        clippy::should_implement_trait,
        reason = "the cross-crate contract fixes this name; `AsRef` cannot return a view type"
    )]
    pub fn as_ref(&self) -> AudioBufferRef<'_, T> {
        // SAFETY: `ptrs` points to `channels` initialised channel pointers (built in
        // `new`), each addressing `frames` initialised samples inside the single `data`
        // allocation, which this borrow keeps alive and immutable. `*mut T` and `*const T`
        // share a layout, so reading the array as `*const *const T` is valid. `data_len`
        // came from a successful allocation, so it cannot exceed `isize::MAX` bytes.
        unsafe {
            AudioBufferRef::from_raw(
                self.ptrs.cast::<*const T>().cast_const(),
                self.channels,
                self.frames,
            )
        }
    }

    /// A writable view of the whole buffer. `[audio-thread]` — allocation-free.
    #[inline]
    #[must_use]
    #[allow(
        clippy::should_implement_trait,
        reason = "the cross-crate contract fixes this name; `AsMut` cannot return a view type"
    )]
    pub fn as_mut(&mut self) -> AudioBufferMut<'_, T> {
        // SAFETY: as in `as_ref`, plus the extra requirement of the mutable constructor:
        // the channels are consecutive, equally sized, non-overlapping windows of one
        // allocation, so no two channel pointers address the same sample. The exclusive
        // borrow of `self` guarantees no other reference to the samples exists.
        unsafe { AudioBufferMut::from_raw(self.ptrs.cast_const(), self.channels, self.frames) }
    }

    /// The whole buffer as one flat planar slice: channel 0's frames, then channel 1's, and
    /// so on. `[any-thread]`
    #[inline]
    #[must_use]
    pub fn as_slice(&self) -> &[T] {
        // SAFETY: `data` points to `data_len` initialised, contiguous samples in a single
        // live allocation owned by `self`; the shared borrow keeps them alive and
        // immutable. `Vec` never allocates more than `isize::MAX` bytes.
        unsafe { core::slice::from_raw_parts(self.data.cast_const(), self.data_len) }
    }

    /// The whole buffer as one flat planar slice, writable. `[any-thread]`
    #[inline]
    #[must_use]
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        // SAFETY: as in `as_slice`; the exclusive borrow of `self` makes this the only live
        // reference to the samples.
        unsafe { core::slice::from_raw_parts_mut(self.data, self.data_len) }
    }

    /// The samples of channel `index`, or `None` if it does not exist. `[any-thread]`
    #[inline]
    #[must_use]
    pub fn channel(&self, index: usize) -> Option<&[T]> {
        self.as_ref().get_channel(index)
    }

    /// The samples of channel `index`, writable, or `None` if it does not exist.
    /// `[any-thread]`
    #[inline]
    #[must_use]
    pub fn channel_mut(&mut self, index: usize) -> Option<&mut [T]> {
        if index >= self.channels {
            return None;
        }
        let start = index * self.frames;
        let end = start + self.frames;
        Some(&mut self.as_mut_slice()[start..end])
    }

    /// Writes silence to every sample. `[audio-thread]` — allocation-free.
    pub fn fill_silence(&mut self) {
        self.as_mut_slice().fill(T::ZERO);
    }

    /// Writes `value` to every sample. `[audio-thread]` — allocation-free.
    pub fn fill(&mut self, value: T) {
        self.as_mut_slice().fill(value);
    }

    /// Builds planar storage from an interleaved buffer. `[main-thread]` — **allocates**.
    ///
    /// # Errors
    ///
    /// [`AudioError::ZeroChannels`] if `channels` is zero, or [`AudioError::NotDivisible`]
    /// if `interleaved.len()` is not a whole number of frames.
    pub fn from_interleaved(interleaved: &[T], channels: usize) -> AudioResult<Self> {
        if channels == 0 {
            return Err(AudioError::ZeroChannels);
        }
        if interleaved.len() % channels != 0 {
            return Err(AudioError::NotDivisible {
                len: interleaved.len(),
                channels,
            });
        }
        let frames = interleaved.len() / channels;
        let mut storage = Self::new(channels, frames);
        {
            let flat = storage.as_mut_slice();
            for (i, sample) in interleaved.iter().enumerate() {
                let channel = i % channels;
                let frame = i / channels;
                flat[channel * frames + frame] = *sample;
            }
        }
        Ok(storage)
    }

    /// Copies the buffer into a freshly allocated interleaved `Vec`. `[main-thread]` —
    /// **allocates**.
    #[must_use]
    pub fn to_interleaved(&self) -> Vec<T> {
        let mut out = vec![T::ZERO; self.data_len];
        let flat = self.as_slice();
        for c in 0..self.channels {
            for f in 0..self.frames {
                out[f * self.channels + c] = flat[c * self.frames + f];
            }
        }
        out
    }
}

impl<T: Sample> Drop for AudioStorage<T> {
    fn drop(&mut self) {
        // SAFETY: both pointers came from `Vec::into_raw_parts`-style decomposition in
        // `new` (or `clone`) with exactly these lengths and capacities, from the global
        // allocator, and neither has been freed before — `Drop` runs once. Rebuilding the
        // `Vec`s hands both allocations back to the allocator that produced them. `T` is
        // `Copy` and needs no element drop, and `*mut T` has no drop glue either.
        unsafe {
            drop(Vec::from_raw_parts(self.ptrs, self.channels, self.ptrs_cap));
            drop(Vec::from_raw_parts(self.data, self.data_len, self.data_cap));
        }
    }
}

impl<T: Sample> Clone for AudioStorage<T> {
    /// `[main-thread]` — **allocates**.
    fn clone(&self) -> Self {
        let mut copy = Self::new(self.channels, self.frames);
        copy.as_mut_slice().copy_from_slice(self.as_slice());
        copy
    }
}

impl<T: Sample> fmt::Debug for AudioStorage<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AudioStorage")
            .field("channels", &self.channels)
            .field("frames", &self.frames)
            .finish()
    }
}

impl<T: Sample> PartialEq for AudioStorage<T> {
    fn eq(&self, other: &Self) -> bool {
        self.channels == other.channels
            && self.frames == other.frames
            && self.as_slice() == other.as_slice()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_allocates_silence_with_the_requested_shape() {
        let s = AudioStorage::<f32>::new(3, 5);
        assert_eq!(s.channel_count(), 3);
        assert_eq!(s.frames(), 5);
        assert_eq!(s.sample_count(), 15);
        assert!(!s.is_empty());
        assert_eq!(s.as_slice().len(), 15);
        assert!(s.as_slice().iter().all(|&v| v == 0.0));
        assert_eq!(s.as_ref().channel_count(), 3);
        assert_eq!(s.as_ref().frames(), 5);
    }

    #[test]
    fn channels_are_contiguous_and_disjoint() {
        let mut s = AudioStorage::<f64>::new(3, 4);
        {
            let mut buf = s.as_mut();
            for (c, channel) in buf.split_channels_mut().enumerate() {
                channel.fill(c as f64 + 1.0);
            }
        }
        // Planar layout: channel 0's frames, then channel 1's, then channel 2's.
        assert_eq!(
            s.as_slice(),
            &[1.0, 1.0, 1.0, 1.0, 2.0, 2.0, 2.0, 2.0, 3.0, 3.0, 3.0, 3.0]
        );
        assert_eq!(s.channel(1).unwrap(), &[2.0, 2.0, 2.0, 2.0]);
        assert!(s.channel(3).is_none());
        assert_eq!(s.channel_mut(2).unwrap(), &[3.0, 3.0, 3.0, 3.0]);
        assert!(s.channel_mut(3).is_none());
        s.channel_mut(0).unwrap()[0] = -1.0;
        assert_eq!(s.as_ref().channel(0)[0], -1.0);
    }

    #[test]
    fn degenerate_shapes_are_valid() {
        for (channels, frames) in [(0usize, 0usize), (0, 16), (16, 0), (1, 1)] {
            let mut s = AudioStorage::<f32>::new(channels, frames);
            assert_eq!(s.channel_count(), channels);
            assert_eq!(s.frames(), frames);
            assert_eq!(s.sample_count(), channels * frames);
            assert_eq!(s.is_empty(), channels * frames == 0);
            assert_eq!(s.as_slice().len(), channels * frames);
            assert_eq!(s.as_ref().channel_count(), channels);
            assert_eq!(s.as_mut().frames(), frames);
            s.fill_silence();
            // Every channel of a zero-frame buffer is a distinct, empty, valid slice.
            let mut buf = s.as_mut();
            assert_eq!(buf.split_channels_mut().count(), channels);
            assert_eq!(s.clone(), s);
        }
    }

    #[test]
    fn fill_and_clone_and_eq() {
        let mut s = AudioStorage::<f32>::new(2, 3);
        s.fill(0.75);
        assert!(s.as_slice().iter().all(|&v| v == 0.75));
        let c = s.clone();
        assert_eq!(c, s);
        assert_eq!(c.channel_count(), 2);
        assert_eq!(c.frames(), 3);
        s.fill_silence();
        assert_ne!(c, s);
        assert_ne!(
            AudioStorage::<f32>::new(2, 3),
            AudioStorage::<f32>::new(3, 2)
        );
        assert_ne!(
            AudioStorage::<f32>::new(2, 3),
            AudioStorage::<f32>::new(2, 4)
        );
    }

    #[test]
    fn interleaved_round_trip() {
        // Two channels: L = 1,3,5  R = 2,4,6.
        let interleaved = [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0];
        let s = AudioStorage::from_interleaved(&interleaved, 2).unwrap();
        assert_eq!(s.channel_count(), 2);
        assert_eq!(s.frames(), 3);
        assert_eq!(s.channel(0).unwrap(), &[1.0, 3.0, 5.0]);
        assert_eq!(s.channel(1).unwrap(), &[2.0, 4.0, 6.0]);
        assert_eq!(s.to_interleaved(), interleaved);

        // Mono is the identity.
        let mono = AudioStorage::from_interleaved(&interleaved, 1).unwrap();
        assert_eq!(mono.frames(), 6);
        assert_eq!(mono.to_interleaved(), interleaved);

        // Six channels, one frame.
        let wide = AudioStorage::from_interleaved(&interleaved, 6).unwrap();
        assert_eq!(wide.frames(), 1);
        assert_eq!(wide.channel(5).unwrap(), &[6.0]);
        assert_eq!(wide.to_interleaved(), interleaved);

        // Empty input.
        let empty = AudioStorage::<f32>::from_interleaved(&[], 2).unwrap();
        assert_eq!(empty.frames(), 0);
        assert_eq!(empty.channel_count(), 2);
        assert!(empty.to_interleaved().is_empty());
    }

    #[test]
    fn interleaved_rejects_malformed_input() {
        assert_eq!(
            AudioStorage::<f32>::from_interleaved(&[1.0, 2.0, 3.0], 2).unwrap_err(),
            AudioError::NotDivisible {
                len: 3,
                channels: 2
            }
        );
        assert_eq!(
            AudioStorage::<f32>::from_interleaved(&[1.0], 0).unwrap_err(),
            AudioError::ZeroChannels
        );
        // Zero channels is rejected even for empty input, because the frame count would be
        // undefined.
        assert_eq!(
            AudioStorage::<f32>::from_interleaved(&[], 0).unwrap_err(),
            AudioError::ZeroChannels
        );
    }

    #[test]
    #[should_panic(expected = "overflows usize")]
    fn oversized_request_panics_instead_of_wrapping() {
        let _ = AudioStorage::<f32>::new(usize::MAX, 2);
    }

    #[test]
    fn storage_is_send_and_sync() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<AudioStorage<f32>>();
        assert_sync::<AudioStorage<f64>>();
    }

    #[test]
    fn debug_reports_the_shape() {
        let s = AudioStorage::<f32>::new(2, 8);
        let text = format!("{s:?}");
        assert!(text.contains("channels: 2"), "{text}");
        assert!(text.contains("frames: 8"), "{text}");
    }

    #[test]
    fn many_allocations_do_not_leak_or_corrupt() {
        // Exercises the manual allocation/deallocation path repeatedly, including the
        // degenerate shapes, so a mismatched capacity would show up under a leak checker.
        for i in 0..64usize {
            let mut s = AudioStorage::<f64>::new(i % 5, i % 7);
            s.fill(i as f64);
            assert!(s.as_slice().iter().all(|&v| v == i as f64));
        }
    }
}
