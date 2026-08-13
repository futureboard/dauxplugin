//! Preallocated per-channel scratch memory for the audio thread.

use core::slice::ChunksExactMut;

/// Per-channel scratch memory, allocated once and reused every block.
///
/// One contiguous allocation of `channels * frames` items, chunked into equal
/// per-channel slices. Contiguity matters: the whole scratch area fits a
/// predictable number of cache lines and the channel slices are laid out exactly
/// like the planar buffers a host hands to `process`.
///
/// ```
/// use daux_rt::ScratchBuffers;
///
/// let mut scratch = ScratchBuffers::<f32>::new(2, 512);   // in prepare()
/// let left = scratch.slice_mut(0, 64);                    // in process()
/// left.fill(0.0);
/// assert_eq!(left.len(), 64);
/// ```
///
/// [any-thread]
pub struct ScratchBuffers<T> {
    data: Box<[T]>,
    channels: usize,
    frames: usize,
}

impl<T: Copy + Default> ScratchBuffers<T> {
    /// Allocates `channels * frames` items, zero-initialised with
    /// `T::default()`.
    ///
    /// This is the only allocating operation on the type. Size it from
    /// `ProcessConfig::max_block_size` in `prepare`, never in `process`.
    ///
    /// # Panics
    ///
    /// Panics if `channels * frames` overflows `usize`, or if the allocation
    /// fails.
    ///
    /// [main-thread]
    #[must_use]
    pub fn new(channels: usize, frames: usize) -> Self {
        let total = channels
            .checked_mul(frames)
            .expect("daux-rt: ScratchBuffers size overflows usize");
        Self {
            data: vec![T::default(); total].into_boxed_slice(),
            channels,
            frames,
        }
    }

    /// Number of channels. [audio-thread]
    #[inline]
    #[must_use]
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Number of frames per channel. [audio-thread]
    #[inline]
    #[must_use]
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Whether there is no scratch memory at all. [audio-thread]
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// The whole scratch area for channel `channel`. [audio-thread]
    ///
    /// # Panics
    ///
    /// Panics if `channel >= channels()`. Use
    /// [`get_channel`](ScratchBuffers::get_channel) where the index comes from
    /// outside the plug-in.
    #[inline]
    #[must_use]
    pub fn channel(&self, channel: usize) -> &[T] {
        self.get_channel(channel)
            .expect("daux-rt: ScratchBuffers channel index out of range")
    }

    /// The whole scratch area for channel `channel`. [audio-thread]
    ///
    /// # Panics
    ///
    /// Panics if `channel >= channels()`. Use
    /// [`get_channel_mut`](ScratchBuffers::get_channel_mut) where the index comes
    /// from outside the plug-in.
    #[inline]
    #[must_use]
    pub fn channel_mut(&mut self, channel: usize) -> &mut [T] {
        self.get_channel_mut(channel)
            .expect("daux-rt: ScratchBuffers channel index out of range")
    }

    /// The whole scratch area for channel `channel`, or `None` when the index is
    /// out of range. Never panics. [audio-thread]
    #[inline]
    #[must_use]
    pub fn get_channel(&self, channel: usize) -> Option<&[T]> {
        let start = channel.checked_mul(self.frames)?;
        if channel >= self.channels {
            return None;
        }
        self.data.get(start..start + self.frames)
    }

    /// The whole scratch area for channel `channel`, or `None` when the index is
    /// out of range. Never panics. [audio-thread]
    #[inline]
    #[must_use]
    pub fn get_channel_mut(&mut self, channel: usize) -> Option<&mut [T]> {
        let start = channel.checked_mul(self.frames)?;
        if channel >= self.channels {
            return None;
        }
        let end = start + self.frames;
        self.data.get_mut(start..end)
    }

    /// The first `frames` items of channel `channel` — the usual call, because a
    /// block is normally shorter than `max_block_size`.
    ///
    /// `frames` is clamped to [`frames()`](ScratchBuffers::frames) so a host that
    /// reports a longer block than it prepared for cannot make this panic in a
    /// release build; a debug build asserts instead, because that host is buggy.
    /// [audio-thread]
    ///
    /// # Panics
    ///
    /// Panics if `channel >= channels()`, and in debug builds if `frames`
    /// exceeds the prepared block size.
    #[inline]
    #[must_use]
    pub fn slice_mut(&mut self, channel: usize, frames: usize) -> &mut [T] {
        debug_assert!(
            frames <= self.frames,
            "daux-rt: ScratchBuffers::slice_mut asked for more frames than were prepared"
        );
        let frames = frames.min(self.frames);
        &mut self.channel_mut(channel)[..frames]
    }

    /// Iterates every channel mutably, which is the way to touch several
    /// channels at once without fighting the borrow checker. [audio-thread]
    #[inline]
    pub fn iter_channels_mut(&mut self) -> ChunksExactMut<'_, T> {
        // `chunks_exact_mut` rejects a chunk size of zero, so a zero-frame
        // scratch area is expressed as an empty slice chunked by one instead.
        let end = self.channels * self.frames;
        self.data[..end].chunks_exact_mut(self.frames.max(1))
    }

    /// Resets every channel to `T::default()`. [audio-thread]
    #[inline]
    pub fn clear(&mut self) {
        self.data.fill(T::default());
    }

    /// The whole scratch area as one contiguous, channel-major slice.
    /// [audio-thread]
    #[inline]
    #[must_use]
    pub fn as_slice(&self) -> &[T] {
        &self.data
    }

    /// The whole scratch area as one contiguous, channel-major slice.
    /// [audio-thread]
    #[inline]
    #[must_use]
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        &mut self.data
    }
}

impl<T> core::fmt::Debug for ScratchBuffers<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ScratchBuffers")
            .field("channels", &self.channels)
            .field("frames", &self.frames)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::ScratchBuffers;
    use crate::alloc_probe::AllocGuard;

    #[test]
    fn starts_zeroed_and_reports_its_shape() {
        let s = ScratchBuffers::<f32>::new(4, 128);
        assert_eq!(s.channels(), 4);
        assert_eq!(s.frames(), 128);
        assert!(!s.is_empty());
        assert_eq!(s.as_slice().len(), 512);
        assert!(s.as_slice().iter().all(|&v| v == 0.0));
    }

    #[test]
    fn channels_are_disjoint_and_contiguous() {
        let mut s = ScratchBuffers::<u32>::new(3, 4);
        for channel in 0..3 {
            s.channel_mut(channel).fill(channel as u32 + 1);
        }
        assert_eq!(s.channel(0), &[1, 1, 1, 1]);
        assert_eq!(s.channel(1), &[2, 2, 2, 2]);
        assert_eq!(s.channel(2), &[3, 3, 3, 3]);
        assert_eq!(s.as_slice(), &[1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3]);
    }

    #[test]
    fn out_of_range_channels_are_reported_not_guessed() {
        let mut s = ScratchBuffers::<f32>::new(2, 8);
        assert!(s.get_channel(2).is_none());
        assert!(s.get_channel_mut(2).is_none());
        assert!(
            s.get_channel(usize::MAX).is_none(),
            "the offset must not overflow"
        );
        assert!(s.get_channel_mut(1).is_some());
    }

    #[test]
    #[should_panic(expected = "channel index out of range")]
    fn channel_mut_panics_on_a_bad_index() {
        let mut s = ScratchBuffers::<f32>::new(1, 4);
        let _ = s.channel_mut(1);
    }

    #[test]
    fn slice_mut_shortens_to_the_block_length() {
        let mut s = ScratchBuffers::<f32>::new(2, 512);
        let block = s.slice_mut(1, 64);
        assert_eq!(block.len(), 64);
        block.fill(1.0);
        assert_eq!(s.channel(1)[..64].iter().sum::<f32>(), 64.0);
        assert_eq!(
            s.channel(1)[64],
            0.0,
            "only the requested frames are touched"
        );
        assert_eq!(s.channel(0).iter().sum::<f32>(), 0.0);
    }

    #[test]
    fn slice_mut_asks_for_zero_frames() {
        let mut s = ScratchBuffers::<f32>::new(1, 4);
        assert!(s.slice_mut(0, 0).is_empty());
    }

    #[test]
    #[cfg(not(debug_assertions))]
    fn slice_mut_clamps_an_over_long_block_in_release() {
        let mut s = ScratchBuffers::<f32>::new(1, 4);
        assert_eq!(s.slice_mut(0, 9_999).len(), 4);
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "more frames than were prepared")]
    fn slice_mut_asserts_on_an_over_long_block_in_debug() {
        let mut s = ScratchBuffers::<f32>::new(1, 4);
        let _ = s.slice_mut(0, 9_999);
    }

    #[test]
    fn degenerate_shapes_do_not_panic() {
        let mut zero_frames = ScratchBuffers::<f32>::new(4, 0);
        assert!(zero_frames.is_empty());
        assert_eq!(zero_frames.iter_channels_mut().count(), 0);
        assert_eq!(zero_frames.channel(3).len(), 0);

        let mut zero_channels = ScratchBuffers::<f32>::new(0, 512);
        assert!(zero_channels.is_empty());
        assert_eq!(zero_channels.iter_channels_mut().count(), 0);
        assert!(zero_channels.get_channel(0).is_none());
    }

    #[test]
    fn iter_channels_mut_visits_each_channel_once() {
        let mut s = ScratchBuffers::<i32>::new(3, 2);
        for (index, channel) in s.iter_channels_mut().enumerate() {
            channel.fill(index as i32);
        }
        assert_eq!(s.as_slice(), &[0, 0, 1, 1, 2, 2]);
        s.clear();
        assert!(s.as_slice().iter().all(|&v| v == 0));
    }

    #[test]
    fn audio_thread_use_does_not_allocate() {
        let mut s = ScratchBuffers::<f32>::new(8, 1024);
        let ((), allocations) = AllocGuard::scope(|| {
            for block in 0..100 {
                for channel in 0..8 {
                    s.slice_mut(channel, 512).fill(block as f32);
                }
                for channel in s.iter_channels_mut() {
                    channel[0] = 0.0;
                }
                s.clear();
            }
        });
        assert_eq!(allocations, 0, "ScratchBuffers use allocated");
    }
}
