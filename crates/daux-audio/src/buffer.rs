//! Borrowed, planar, zero-copy audio buffer views.
//!
//! # Memory model
//!
//! DAUx audio is **planar**: a bus is an array of `channel_count` pointers, each to
//! `frame_count` contiguous samples (`abi-v1` §8). The views in this module wrap exactly
//! that representation — the array the host handed over is used in place, and no view ever
//! copies, allocates or takes ownership.
//!
//! # In-place processing
//!
//! Input and output buffers **may alias**: a host is allowed to pass the same memory as
//! input and output for in-place processing, and `abi-v1` §8 requires plug-ins to cope.
//! Nothing in this module assumes otherwise. In particular [`AudioBufferMut::copy_from`]
//! is a `memmove`, not a `memcpy`, so it stays correct when source and destination overlap,
//! and no API here materialises a `&[T]` and a `&mut [T]` over the same channel at once.
//!
//! Aliasing *between the channels of one mutable view* is a different matter: it is
//! forbidden by the safety contract of the `from_raw` constructors, because
//! [`AudioBufferMut::split_channels_mut`] hands out one `&mut [T]` per channel at once.

use core::fmt;
use core::iter::FusedIterator;
use core::marker::PhantomData;
use core::ptr::NonNull;
use core::slice;

use crate::error::{AudioError, AudioResult};
use crate::sample::Sample;

/// A dangling but well-aligned pointer array, used by the empty views. Never dereferenced,
/// because every accessor checks the channel count first.
#[inline]
const fn dangling_array<P>() -> *const P {
    NonNull::<P>::dangling().as_ptr().cast_const()
}

/// The constant-mask bits that correspond to `channels` real channels.
#[inline]
const fn full_mask(channels: usize) -> u64 {
    if channels >= u64::BITS as usize {
        u64::MAX
    } else {
        (1u64 << channels) - 1
    }
}

/// An empty `&mut [T]` that borrows nothing.
#[inline]
fn empty_mut<'a, T>() -> &'a mut [T] {
    // SAFETY: `NonNull::dangling` is non-null and correctly aligned for `T`, and a
    // zero-length slice never reads or writes memory, so it can alias nothing and needs no
    // allocation. The lifetime is unconstrained on purpose: an empty slice grants access to
    // nothing at all.
    unsafe { slice::from_raw_parts_mut(NonNull::<T>::dangling().as_ptr(), 0) }
}

/// Builds the sample slice of one channel of a planar pointer array.
///
/// # Safety
///
/// * `ptrs` points to an initialised array of at least `index + 1` channel pointers.
/// * `frames > 0` (callers must special-case zero themselves).
/// * `ptrs[index]` is non-null, aligned for `T`, and points to at least `offset + frames`
///   initialised `T`s in one allocation, readable and unwritten for `'a`.
#[inline]
unsafe fn channel_slice<'a, T>(
    ptrs: *const *const T,
    index: usize,
    offset: usize,
    frames: usize,
) -> &'a [T] {
    // SAFETY: the caller guarantees `index` is inside the pointer array and that the
    // channel pointer covers `offset + frames` initialised, readable samples for `'a`.
    // `offset` is only ever non-zero on a sub-block, which bounds-checks against the
    // parent's frame count, so the offset pointer stays inside the same allocation.
    unsafe {
        let base = *ptrs.add(index);
        let start = if offset == 0 { base } else { base.add(offset) };
        slice::from_raw_parts(start, frames)
    }
}

/// Builds the exclusive sample slice of one channel of a planar pointer array.
///
/// # Safety
///
/// Everything [`channel_slice`] requires, with "readable" upgraded to "writable", plus:
/// the returned slice must be unique — no other live reference may address the same
/// samples, which for a well-formed [`AudioBufferMut`] means `index` is handed out at most
/// once for `'a`.
#[inline]
unsafe fn channel_slice_mut<'a, T>(
    ptrs: *const *mut T,
    index: usize,
    offset: usize,
    frames: usize,
) -> &'a mut [T] {
    // SAFETY: the caller guarantees `index` is inside the pointer array, that the channel
    // pointer covers `offset + frames` initialised, writable samples for `'a`, and that
    // this channel is not handed out twice, so the resulting `&mut` is unique.
    unsafe {
        let base = *ptrs.add(index);
        let start = if offset == 0 { base } else { base.add(offset) };
        slice::from_raw_parts_mut(start, frames)
    }
}

/// Reads the pointer of channel `index`, offset to the start of the view.
///
/// # Safety
///
/// `ptrs` points to an initialised array of at least `index + 1` channel pointers, and when
/// `offset > 0` the channel pointer is non-null and covers at least `offset` samples.
#[inline]
unsafe fn channel_start<T>(ptrs: *const *const T, index: usize, offset: usize) -> *const T {
    // SAFETY: the caller guarantees `index` is inside the array. When `offset == 0` the
    // stored pointer is returned untouched, so a host that passes null pointers for a
    // zero-frame block cannot make this compute an out-of-bounds address.
    unsafe {
        let base = *ptrs.add(index);
        if offset == 0 { base } else { base.add(offset) }
    }
}

// ---------------------------------------------------------------------------------------
// Shared view
// ---------------------------------------------------------------------------------------

/// A read-only view of one planar audio bus for one block. `[audio-thread]`
///
/// Cheap to copy, never owns anything, and every accessor is allocation-free, lock-free and
/// branch-light.
pub struct AudioBufferRef<'a, T: Sample> {
    /// Array of `channels` pointers to the start of the *block* for each channel.
    ptrs: *const *const T,
    channels: usize,
    /// Frame offset of this view inside the block the pointers describe.
    offset: usize,
    /// Number of frames this view exposes.
    frames: usize,
    constant_mask: u64,
    _marker: PhantomData<&'a [T]>,
}

// SAFETY: the view is semantically `&'a [&'a [T]]`: it only ever hands out shared
// references into memory owned elsewhere, has no interior mutability, and `T: Sample` is
// itself `Send + Sync`. Sending it therefore moves nothing but a shared borrow.
unsafe impl<T: Sample> Send for AudioBufferRef<'_, T> {}
// SAFETY: see the `Send` impl above — sharing the view lets two threads read the same
// immutable samples, which is exactly what `&[&[T]]` allows.
unsafe impl<T: Sample> Sync for AudioBufferRef<'_, T> {}

impl<T: Sample> Clone for AudioBufferRef<'_, T> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: Sample> Copy for AudioBufferRef<'_, T> {}

impl<T: Sample> fmt::Debug for AudioBufferRef<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AudioBufferRef")
            .field("channels", &self.channels)
            .field("frames", &self.frames)
            .field("constant_mask", &format_args!("{:#x}", self.constant_mask))
            .finish()
    }
}

impl<'a, T: Sample> AudioBufferRef<'a, T> {
    /// An empty view: zero channels, zero frames. `[audio-thread]`
    #[inline]
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            ptrs: dangling_array::<*const T>(),
            channels: 0,
            offset: 0,
            frames: 0,
            constant_mask: 0,
            _marker: PhantomData,
        }
    }

    /// Wraps a host-supplied array of channel pointers. `[audio-thread]`
    ///
    /// # Safety
    ///
    /// All of the following must hold for the whole lifetime `'a`, which the caller chooses
    /// and which must not outlive the host's buffers — for a plug-in that means the
    /// duration of one `process` call, since pointers must never be retained past it
    /// (`abi-v1` §16):
    ///
    /// * If `channels > 0`, `ptrs` is non-null, aligned for `*const T`, and points to an
    ///   initialised array of at least `channels` pointers. If `channels == 0`, `ptrs` is
    ///   never read and may be null or dangling.
    /// * If `frames > 0`, each of the first `channels` entries of that array is non-null,
    ///   aligned for `T`, and points to at least `frames` initialised, readable `T`s inside
    ///   a single allocation. If `frames == 0` the channel pointers are never read and may
    ///   be null.
    /// * `frames * size_of::<T>()` does not exceed `isize::MAX`.
    /// * No `&mut` reference to those samples exists and nothing writes to them while this
    ///   view, or anything derived from it, is alive. Channels *may* alias one another and
    ///   *may* be the same memory the host also passes as an output for in-place
    ///   processing, because this view only ever produces shared references.
    /// * `channels` and `frames` are the real extents; they are trusted without checking.
    #[inline]
    #[must_use]
    pub const unsafe fn from_raw(ptrs: *const *const T, channels: usize, frames: usize) -> Self {
        debug_assert!(
            channels == 0 || !ptrs.is_null(),
            "AudioBufferRef::from_raw: null channel-pointer array with channels > 0"
        );
        Self {
            ptrs,
            channels,
            offset: 0,
            frames,
            constant_mask: 0,
            _marker: PhantomData,
        }
    }

    /// Like [`from_raw`] but also carries the host's constant/silence mask.
    /// `[audio-thread]`
    ///
    /// # Safety
    ///
    /// Identical to [`from_raw`]. `constant_mask` is a hint and is never trusted for memory
    /// safety: a wrong mask produces wrong audio, never undefined behaviour.
    ///
    /// [`from_raw`]: AudioBufferRef::from_raw
    #[inline]
    #[must_use]
    pub const unsafe fn from_raw_with_mask(
        ptrs: *const *const T,
        channels: usize,
        frames: usize,
        constant_mask: u64,
    ) -> Self {
        // SAFETY: the caller upholds the contract of `from_raw`, which this function
        // repeats verbatim.
        let mut view = unsafe { Self::from_raw(ptrs, channels, frames) };
        view.constant_mask = constant_mask;
        view
    }

    /// Number of channels in this view. `[audio-thread]`
    #[inline]
    #[must_use]
    pub const fn channel_count(&self) -> usize {
        self.channels
    }

    /// Number of frames in this view. `[audio-thread]`
    #[inline]
    #[must_use]
    pub const fn frames(&self) -> usize {
        self.frames
    }

    /// `true` when the view addresses no samples at all. `[audio-thread]`
    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.channels == 0 || self.frames == 0
    }

    /// Total number of samples, `channels * frames`. `[audio-thread]`
    #[inline]
    #[must_use]
    pub const fn sample_count(&self) -> usize {
        self.channels * self.frames
    }

    /// The samples of channel `index`. `[audio-thread]`
    ///
    /// # Panics
    ///
    /// If `index >= channel_count()`, exactly like indexing a slice out of range. Audio
    /// code that cannot prove the index is in range must use [`get_channel`] instead.
    ///
    /// [`get_channel`]: AudioBufferRef::get_channel
    #[inline]
    #[must_use]
    pub fn channel(&self, index: usize) -> &'a [T] {
        match self.get_channel(index) {
            Some(channel) => channel,
            None => out_of_range(index, self.channels),
        }
    }

    /// The samples of channel `index`, or `None` if it does not exist. `[audio-thread]`
    #[inline]
    #[must_use]
    pub fn get_channel(&self, index: usize) -> Option<&'a [T]> {
        if index >= self.channels {
            return None;
        }
        if self.frames == 0 {
            return Some(&[]);
        }
        // SAFETY: `index < channels`, so the array entry exists and is initialised, and
        // `frames > 0`, so `from_raw` guarantees that channel is non-null, aligned and
        // covers `offset + frames` initialised samples that stay immutable and alive for
        // `'a` — exactly what `channel_slice` requires.
        Some(unsafe { channel_slice(self.ptrs, index, self.offset, self.frames) })
    }

    /// Raw pointer to the first sample of channel `index`. `[audio-thread]`
    ///
    /// For adapters that must hand the pointer back to a C API. `None` when the channel
    /// does not exist; the pointer is only meaningful while this view is alive and only
    /// guaranteed dereferenceable when [`frames`] is non-zero.
    ///
    /// [`frames`]: AudioBufferRef::frames
    #[inline]
    #[must_use]
    pub fn channel_ptr(&self, index: usize) -> Option<*const T> {
        if index >= self.channels {
            return None;
        }
        // SAFETY: `index < channels`, so this array entry exists and is initialised; when
        // `offset > 0` the view is a bounds-checked sub-block, so offsetting stays inside
        // the channel's allocation.
        Some(unsafe { channel_start(self.ptrs, index, self.offset) })
    }

    /// The raw channel-pointer array this view was built from. `[audio-thread]`
    ///
    /// Only meaningful together with [`channel_count`]; for a sub-block the pointers
    /// address the start of the *block*, not of the view — see [`frame_offset`].
    ///
    /// [`channel_count`]: AudioBufferRef::channel_count
    /// [`frame_offset`]: AudioBufferRef::frame_offset
    #[inline]
    #[must_use]
    pub const fn as_ptr(&self) -> *const *const T {
        self.ptrs
    }

    /// Frame offset of this view inside the block its pointers describe. `[audio-thread]`
    #[inline]
    #[must_use]
    pub const fn frame_offset(&self) -> usize {
        self.offset
    }

    /// One sample, or `None` if either index is out of range. `[audio-thread]`
    #[inline]
    #[must_use]
    pub fn sample(&self, channel: usize, frame: usize) -> Option<T> {
        self.get_channel(channel)?.get(frame).copied()
    }

    /// Iterates the channels as slices. `[audio-thread]`
    #[inline]
    #[must_use]
    pub fn iter(&self) -> Channels<'a, T> {
        Channels {
            ptrs: self.ptrs,
            offset: self.offset,
            frames: self.frames,
            next: 0,
            end: self.channels,
            _marker: PhantomData,
        }
    }

    /// A view of `len` frames starting at frame `start`, or `None` if that range does not
    /// fit. `[audio-thread]`
    ///
    /// Sub-blocks are how sample-accurate automation is applied without copying: split the
    /// block at every event time and process the pieces.
    #[inline]
    #[must_use]
    pub fn sub_block(&self, start: usize, len: usize) -> Option<Self> {
        let end = start.checked_add(len)?;
        if end > self.frames {
            return None;
        }
        Some(Self {
            ptrs: self.ptrs,
            channels: self.channels,
            offset: self.offset + start,
            frames: len,
            // A channel that is constant over the block is constant over any sub-range.
            constant_mask: self.constant_mask,
            _marker: PhantomData,
        })
    }

    /// Splits into `[0, mid)` and `[mid, frames)`, or `None` if `mid > frames`.
    /// `[audio-thread]`
    #[inline]
    #[must_use]
    pub fn split_at_frame(&self, mid: usize) -> Option<(Self, Self)> {
        Some((
            self.sub_block(0, mid)?,
            self.sub_block(mid, self.frames - mid)?,
        ))
    }

    /// The host's constant/silence mask: bit `c` set means channel `c` holds the same value
    /// for the whole block. `[audio-thread]`
    ///
    /// A zero mask means "no information", never "not constant".
    #[inline]
    #[must_use]
    pub const fn constant_mask(&self) -> u64 {
        self.constant_mask
    }

    /// Returns the view with a different constant mask. `[audio-thread]`
    #[inline]
    #[must_use]
    pub const fn with_constant_mask(mut self, mask: u64) -> Self {
        self.constant_mask = mask;
        self
    }

    /// `true` when the host promised channel `index` is constant across the block.
    /// `[audio-thread]`
    ///
    /// A hint only: `false` is always safe to act on, and channels beyond bit 63 or beyond
    /// [`channel_count`] can never be reported constant. Treat `true` as an optimisation
    /// opportunity — the samples themselves are still valid to read either way.
    ///
    /// [`channel_count`]: AudioBufferRef::channel_count
    #[inline]
    #[must_use]
    pub const fn is_channel_constant(&self, index: usize) -> bool {
        index < self.channels
            && index < u64::BITS as usize
            && self.constant_mask & (1u64 << index) != 0
    }

    /// `true` when every channel of this view is flagged constant. `[audio-thread]`
    ///
    /// Always `false` for an empty mask, for a buffer with no channels, and for a buffer
    /// with more than 64 channels, since the mask cannot describe one.
    #[inline]
    #[must_use]
    pub const fn all_channels_constant(&self) -> bool {
        self.channels > 0
            && self.channels <= u64::BITS as usize
            && self.constant_mask & full_mask(self.channels) == full_mask(self.channels)
    }

    /// Inspects channel `index` and reports whether every sample equals the first one.
    /// `[any-thread]`
    ///
    /// `O(frames)` and allocation-free, but unlike [`is_channel_constant`] it reads the
    /// audio, so it belongs in hosts, tests and offline code rather than a hot DSP loop. A
    /// channel with zero frames counts as constant. NaN never compares equal to itself, so
    /// a channel containing NaN is never reported constant.
    ///
    /// [`is_channel_constant`]: AudioBufferRef::is_channel_constant
    #[must_use]
    pub fn scan_channel_constant(&self, index: usize) -> bool {
        match self.get_channel(index) {
            None => false,
            Some(channel) => match channel.first() {
                None => true,
                Some(first) => channel.iter().all(|s| s == first),
            },
        }
    }

    /// Computes the constant mask by inspecting the samples. `[any-thread]`
    ///
    /// `O(channels * frames)`, allocation-free. Channels at index 64 and above cannot be
    /// represented in the mask and are left clear.
    #[must_use]
    pub fn scan_constant_mask(&self) -> u64 {
        let mut mask = 0u64;
        let scanned = self.channels.min(u64::BITS as usize);
        for c in 0..scanned {
            if self.scan_channel_constant(c) {
                mask |= 1u64 << c;
            }
        }
        mask
    }

    /// The frame at `index`, or `None` if out of range. `[audio-thread]`
    #[inline]
    #[must_use]
    pub fn frame(&self, index: usize) -> Option<Frame<'a, T>> {
        if index >= self.frames {
            return None;
        }
        Some(Frame {
            ptrs: self.ptrs,
            channels: self.channels,
            index: self.offset + index,
            _marker: PhantomData,
        })
    }

    /// Iterates frame by frame — the planar equivalent of walking an interleaved buffer.
    /// `[audio-thread]`
    #[inline]
    #[must_use]
    pub fn iter_frames(&self) -> Frames<'a, T> {
        Frames {
            ptrs: self.ptrs,
            channels: self.channels,
            next: self.offset,
            end: self.offset + self.frames,
            _marker: PhantomData,
        }
    }
}

impl<'a, T: Sample> IntoIterator for AudioBufferRef<'a, T> {
    type Item = &'a [T];
    type IntoIter = Channels<'a, T>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a, T: Sample> IntoIterator for &AudioBufferRef<'a, T> {
    type Item = &'a [T];
    type IntoIter = Channels<'a, T>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<T: Sample> Default for AudioBufferRef<'_, T> {
    #[inline]
    fn default() -> Self {
        Self::empty()
    }
}

// ---------------------------------------------------------------------------------------
// Mutable view
// ---------------------------------------------------------------------------------------

/// A writable view of one planar audio bus for one block. `[audio-thread]`
///
/// The view borrows the host's memory exclusively for `'a`; it never owns or frees it.
pub struct AudioBufferMut<'a, T: Sample> {
    ptrs: *const *mut T,
    channels: usize,
    offset: usize,
    frames: usize,
    constant_mask: u64,
    _marker: PhantomData<&'a mut [T]>,
}

// SAFETY: the view is semantically `&'a mut [&'a mut [T]]` — an exclusive borrow of memory
// owned elsewhere, with no interior mutability. `T: Sample` is `Send`, so moving that
// exclusive borrow to another thread is sound.
unsafe impl<T: Sample> Send for AudioBufferMut<'_, T> {}
// SAFETY: `&AudioBufferMut` only hands out shared references to `T: Sync` samples; every
// mutating method takes `&mut self`, which Rust guarantees is unique.
unsafe impl<T: Sample> Sync for AudioBufferMut<'_, T> {}

impl<T: Sample> fmt::Debug for AudioBufferMut<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AudioBufferMut")
            .field("channels", &self.channels)
            .field("frames", &self.frames)
            .field("constant_mask", &format_args!("{:#x}", self.constant_mask))
            .finish()
    }
}

impl<'a, T: Sample> AudioBufferMut<'a, T> {
    /// An empty view: zero channels, zero frames. `[audio-thread]`
    #[inline]
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            ptrs: dangling_array::<*mut T>(),
            channels: 0,
            offset: 0,
            frames: 0,
            constant_mask: 0,
            _marker: PhantomData,
        }
    }

    /// Wraps a host-supplied array of channel pointers for writing. `[audio-thread]`
    ///
    /// # Safety
    ///
    /// Everything [`AudioBufferRef::from_raw`] requires, with reads upgraded to writes,
    /// plus one addition that is easy to get wrong:
    ///
    /// * If `channels > 0`, `ptrs` is non-null, aligned for `*mut T`, and points to an
    ///   initialised array of at least `channels` pointers, valid for `'a`.
    /// * If `frames > 0`, each of the first `channels` entries is non-null, aligned for
    ///   `T`, and points to at least `frames` initialised `T`s in one allocation that is
    ///   **writable** for `'a`. If `frames == 0` the channel pointers are never read.
    /// * **The `channels` regions do not overlap each other.**
    ///   [`split_channels_mut`] hands out one `&mut [T]` per channel simultaneously, so two
    ///   channel pointers into the same memory would produce aliasing `&mut`s, which is
    ///   undefined behaviour. This is the one aliasing rule a host must respect.
    /// * No other reference — shared or exclusive — to those samples exists while this view
    ///   or anything derived from it is alive. The buffer *may* be the same memory the host
    ///   also passed as an input for in-place processing, provided the corresponding
    ///   [`AudioBufferRef`] is not alive at the same time.
    /// * `frames * size_of::<T>()` does not exceed `isize::MAX`, and `channels`/`frames`
    ///   are the real extents; they are trusted without checking.
    ///
    /// [`split_channels_mut`]: AudioBufferMut::split_channels_mut
    #[inline]
    #[must_use]
    pub const unsafe fn from_raw(ptrs: *const *mut T, channels: usize, frames: usize) -> Self {
        debug_assert!(
            channels == 0 || !ptrs.is_null(),
            "AudioBufferMut::from_raw: null channel-pointer array with channels > 0"
        );
        Self {
            ptrs,
            channels,
            offset: 0,
            frames,
            constant_mask: 0,
            _marker: PhantomData,
        }
    }

    /// Like [`from_raw`] but also carries the host's constant/silence mask.
    /// `[audio-thread]`
    ///
    /// # Safety
    ///
    /// Identical to [`from_raw`]; the mask is a hint and is never trusted for memory
    /// safety.
    ///
    /// [`from_raw`]: AudioBufferMut::from_raw
    #[inline]
    #[must_use]
    pub const unsafe fn from_raw_with_mask(
        ptrs: *const *mut T,
        channels: usize,
        frames: usize,
        constant_mask: u64,
    ) -> Self {
        // SAFETY: the caller upholds the contract of `from_raw`, repeated verbatim above.
        let mut view = unsafe { Self::from_raw(ptrs, channels, frames) };
        view.constant_mask = constant_mask;
        view
    }

    /// Number of channels in this view. `[audio-thread]`
    #[inline]
    #[must_use]
    pub const fn channel_count(&self) -> usize {
        self.channels
    }

    /// Number of frames in this view. `[audio-thread]`
    #[inline]
    #[must_use]
    pub const fn frames(&self) -> usize {
        self.frames
    }

    /// `true` when the view addresses no samples at all. `[audio-thread]`
    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.channels == 0 || self.frames == 0
    }

    /// Total number of samples, `channels * frames`. `[audio-thread]`
    #[inline]
    #[must_use]
    pub const fn sample_count(&self) -> usize {
        self.channels * self.frames
    }

    /// Frame offset of this view inside the block its pointers describe. `[audio-thread]`
    #[inline]
    #[must_use]
    pub const fn frame_offset(&self) -> usize {
        self.offset
    }

    /// The raw channel-pointer array this view was built from. `[audio-thread]`
    #[inline]
    #[must_use]
    pub const fn as_ptr(&self) -> *const *mut T {
        self.ptrs
    }

    /// A read-only view of the same samples. `[audio-thread]`
    ///
    /// Named `as_ref` to mirror [`AudioStorage::as_ref`]; it is not the `AsRef` trait,
    /// because the result borrows for a shorter lifetime than `'a`.
    ///
    /// [`AudioStorage::as_ref`]: crate::AudioStorage::as_ref
    #[inline]
    #[must_use]
    #[allow(
        clippy::should_implement_trait,
        reason = "the cross-crate contract fixes this name; `AsRef` cannot express the reborrow"
    )]
    pub fn as_ref(&self) -> AudioBufferRef<'_, T> {
        AudioBufferRef {
            // `*mut T` and `*const T` have identical layout and validity, so an array of
            // the former is a valid array of the latter for reading.
            ptrs: self.ptrs.cast::<*const T>(),
            channels: self.channels,
            offset: self.offset,
            frames: self.frames,
            constant_mask: self.constant_mask,
            _marker: PhantomData,
        }
    }

    /// A shorter-lived copy of this view, for passing to a helper without giving up the
    /// original. `[audio-thread]`
    #[inline]
    #[must_use]
    pub fn reborrow(&mut self) -> AudioBufferMut<'_, T> {
        AudioBufferMut {
            ptrs: self.ptrs,
            channels: self.channels,
            offset: self.offset,
            frames: self.frames,
            constant_mask: self.constant_mask,
            _marker: PhantomData,
        }
    }

    /// The samples of channel `index`, read-only. `[audio-thread]`
    ///
    /// # Panics
    ///
    /// If `index >= channel_count()`. Use [`get_channel`] when the index is not known to be
    /// in range.
    ///
    /// [`get_channel`]: AudioBufferMut::get_channel
    #[inline]
    #[must_use]
    pub fn channel(&self, index: usize) -> &[T] {
        match self.get_channel(index) {
            Some(channel) => channel,
            None => out_of_range(index, self.channels),
        }
    }

    /// The samples of channel `index`, read-only, or `None` if it does not exist.
    /// `[audio-thread]`
    #[inline]
    #[must_use]
    pub fn get_channel(&self, index: usize) -> Option<&[T]> {
        if index >= self.channels {
            return None;
        }
        if self.frames == 0 {
            return Some(&[]);
        }
        // SAFETY: `index < channels`, so the array entry exists and is initialised, and
        // `frames > 0`, so `from_raw` guarantees the channel covers `offset + frames`
        // initialised samples. The result borrows `self` shared, so no `&mut` to the same
        // samples can exist while it lives.
        Some(unsafe {
            channel_slice(
                self.ptrs.cast::<*const T>(),
                index,
                self.offset,
                self.frames,
            )
        })
    }

    /// The samples of channel `index`, writable. `[audio-thread]`
    ///
    /// # Panics
    ///
    /// If `index >= channel_count()`. Use [`get_channel_mut`] when the index is not known
    /// to be in range.
    ///
    /// [`get_channel_mut`]: AudioBufferMut::get_channel_mut
    #[inline]
    #[must_use]
    pub fn channel_mut(&mut self, index: usize) -> &mut [T] {
        let channels = self.channels;
        match self.get_channel_mut(index) {
            Some(channel) => channel,
            None => out_of_range(index, channels),
        }
    }

    /// The samples of channel `index`, writable, or `None` if it does not exist.
    /// `[audio-thread]`
    #[inline]
    #[must_use]
    pub fn get_channel_mut(&mut self, index: usize) -> Option<&mut [T]> {
        if index >= self.channels {
            return None;
        }
        if self.frames == 0 {
            return Some(empty_mut());
        }
        // SAFETY: `index < channels`, so the array entry exists and is initialised, and
        // `frames > 0`, so `from_raw` guarantees the channel is writable for
        // `offset + frames` samples. The slice is tied to the exclusive borrow of `self`,
        // so it is the only live reference to that channel.
        Some(unsafe { channel_slice_mut(self.ptrs, index, self.offset, self.frames) })
    }

    /// Two distinct channels at once, for stereo-style processing. `[audio-thread]`
    ///
    /// `None` when either index is out of range or the two indices are equal — the latter
    /// because handing out two `&mut` to one channel would be undefined behaviour.
    #[inline]
    #[must_use]
    pub fn channel_pair_mut(&mut self, a: usize, b: usize) -> Option<(&mut [T], &mut [T])> {
        if a == b || a >= self.channels || b >= self.channels {
            return None;
        }
        if self.frames == 0 {
            return Some((empty_mut(), empty_mut()));
        }
        // SAFETY: both indices are in range and distinct, and `from_raw` guarantees
        // distinct channels never overlap, so the two slices address disjoint memory and
        // neither is handed out twice. Both borrow `self` exclusively for the same
        // lifetime, which is sound precisely because they are disjoint.
        let left = unsafe { channel_slice_mut(self.ptrs, a, self.offset, self.frames) };
        // SAFETY: as above, for the other channel.
        let right = unsafe { channel_slice_mut(self.ptrs, b, self.offset, self.frames) };
        Some((left, right))
    }

    /// Raw pointer to the first sample of channel `index`. `[audio-thread]`
    ///
    /// `None` when the channel does not exist; only guaranteed dereferenceable when
    /// [`frames`] is non-zero.
    ///
    /// [`frames`]: AudioBufferMut::frames
    #[inline]
    #[must_use]
    pub fn channel_ptr_mut(&mut self, index: usize) -> Option<*mut T> {
        if index >= self.channels {
            return None;
        }
        // SAFETY: `index < channels`, so this array entry exists and is initialised; when
        // `offset > 0` the view is a bounds-checked sub-block, so offsetting stays inside
        // the channel's allocation.
        let start = unsafe { channel_start(self.ptrs.cast::<*const T>(), index, self.offset) };
        Some(start.cast_mut())
    }

    /// Iterates the channels as read-only slices. `[audio-thread]`
    #[inline]
    #[must_use]
    pub fn iter(&self) -> Channels<'_, T> {
        self.as_ref().iter()
    }

    /// Hands out every channel as a separate `&mut [T]`, all live at once. `[audio-thread]`
    ///
    /// The slices are disjoint by construction: distinct channels address distinct memory,
    /// which the `from_raw` safety contract requires, and the iterator yields each channel
    /// exactly once.
    #[inline]
    #[must_use]
    pub fn split_channels_mut(&mut self) -> ChannelsMut<'_, T> {
        ChannelsMut {
            ptrs: self.ptrs,
            offset: self.offset,
            frames: self.frames,
            next: 0,
            end: self.channels,
            _marker: PhantomData,
        }
    }

    /// Alias for [`split_channels_mut`], spelled the way `std` collections spell it.
    /// `[audio-thread]`
    ///
    /// [`split_channels_mut`]: AudioBufferMut::split_channels_mut
    #[inline]
    #[must_use]
    pub fn iter_mut(&mut self) -> ChannelsMut<'_, T> {
        self.split_channels_mut()
    }

    /// A view of `len` frames starting at frame `start`, or `None` if that range does not
    /// fit. `[audio-thread]`
    #[inline]
    #[must_use]
    pub fn sub_block_mut(&mut self, start: usize, len: usize) -> Option<AudioBufferMut<'_, T>> {
        let end = start.checked_add(len)?;
        if end > self.frames {
            return None;
        }
        Some(AudioBufferMut {
            ptrs: self.ptrs,
            channels: self.channels,
            offset: self.offset + start,
            frames: len,
            constant_mask: self.constant_mask,
            _marker: PhantomData,
        })
    }

    /// Splits into `[0, mid)` and `[mid, frames)`, or `None` if `mid > frames`.
    /// `[audio-thread]`
    ///
    /// The halves are disjoint frame ranges of the same channels, so both can be written at
    /// once — the allocation-free way to apply sample-accurate automation.
    #[must_use]
    pub fn split_at_frame_mut(
        &mut self,
        mid: usize,
    ) -> Option<(AudioBufferMut<'_, T>, AudioBufferMut<'_, T>)> {
        if mid > self.frames {
            return None;
        }
        let head = AudioBufferMut {
            ptrs: self.ptrs,
            channels: self.channels,
            offset: self.offset,
            frames: mid,
            constant_mask: self.constant_mask,
            _marker: PhantomData,
        };
        let tail = AudioBufferMut {
            ptrs: self.ptrs,
            channels: self.channels,
            offset: self.offset + mid,
            frames: self.frames - mid,
            constant_mask: self.constant_mask,
            _marker: PhantomData,
        };
        Some((head, tail))
    }

    /// The constant/silence mask for this bus. `[audio-thread]`
    ///
    /// On an output buffer the mask is an *output*: a plug-in that knows it wrote silence
    /// should say so with [`set_constant_mask`] so the host can skip downstream work.
    ///
    /// The mask travels with the view by value. A view obtained from [`AudioBuses::output`]
    /// is a temporary copy, so an adapter that must return the mask to the host should read
    /// it back through [`AudioBuses::output_slot_mut`] instead.
    ///
    /// [`set_constant_mask`]: AudioBufferMut::set_constant_mask
    /// [`AudioBuses::output`]: crate::AudioBuses::output
    /// [`AudioBuses::output_slot_mut`]: crate::AudioBuses::output_slot_mut
    #[inline]
    #[must_use]
    pub const fn constant_mask(&self) -> u64 {
        self.constant_mask
    }

    /// Replaces the constant/silence mask. `[audio-thread]`
    #[inline]
    pub const fn set_constant_mask(&mut self, mask: u64) {
        self.constant_mask = mask;
    }

    /// Clears the constant/silence mask, i.e. claims nothing about the contents.
    /// `[audio-thread]`
    #[inline]
    pub const fn clear_constant_mask(&mut self) {
        self.constant_mask = 0;
    }

    /// `true` when channel `index` is flagged constant. `[audio-thread]`
    #[inline]
    #[must_use]
    pub const fn is_channel_constant(&self, index: usize) -> bool {
        index < self.channels
            && index < u64::BITS as usize
            && self.constant_mask & (1u64 << index) != 0
    }

    /// Writes [`Sample::ZERO`] to every sample and marks every channel constant.
    /// `[audio-thread]`
    ///
    /// Allocation-free, and safe on in-place buffers: it only touches this view's frames.
    /// Note that on a sub-block the mask still describes whole channels, so code that
    /// silences a block piece by piece should set the mask once at the end instead.
    pub fn fill_silence(&mut self) {
        for channel in self.split_channels_mut() {
            channel.fill(T::ZERO);
        }
        self.constant_mask = full_mask(self.channels);
    }

    /// Writes `value` to every sample of every channel and marks every channel constant.
    /// `[audio-thread]`
    pub fn fill(&mut self, value: T) {
        for channel in self.split_channels_mut() {
            channel.fill(value);
        }
        self.constant_mask = full_mask(self.channels);
    }

    /// Copies `src` into this buffer, channel by channel. `[audio-thread]`
    ///
    /// Implemented with `memmove`, so it stays correct when `src` and `self` are the very
    /// same host memory (in-place processing) or overlap partially. The source's constant
    /// mask is adopted, because the copy makes the contents identical.
    ///
    /// # Errors
    ///
    /// [`AudioError::ChannelCountMismatch`] or [`AudioError::FrameCountMismatch`] when the
    /// shapes differ; nothing is written in that case.
    pub fn copy_from(&mut self, src: &AudioBufferRef<'_, T>) -> AudioResult<()> {
        if src.channel_count() != self.channels {
            return Err(AudioError::ChannelCountMismatch {
                expected: self.channels,
                found: src.channel_count(),
            });
        }
        if src.frames() != self.frames {
            return Err(AudioError::FrameCountMismatch {
                expected: self.frames,
                found: src.frames(),
            });
        }
        if self.frames > 0 {
            let frames = self.frames;
            for c in 0..self.channels {
                let (Some(from), Some(to)) = (src.channel_ptr(c), self.channel_ptr_mut(c)) else {
                    continue;
                };
                // SAFETY: `c` is a valid channel of both views, and both guarantee `frames`
                // valid samples from the pointer they returned. `copy` is `memmove`: it is
                // defined even when the two regions are identical or partially overlap,
                // which is exactly what in-place processing produces. Only raw pointers are
                // live here — no Rust reference to either region exists — so no aliasing
                // rule can be broken.
                unsafe { core::ptr::copy(from, to, frames) };
            }
        }
        self.constant_mask = src.constant_mask();
        Ok(())
    }

    /// The frame at `index`, or `None` if out of range. `[audio-thread]`
    #[inline]
    #[must_use]
    pub fn frame_mut(&mut self, index: usize) -> Option<FrameMut<'_, T>> {
        if index >= self.frames {
            return None;
        }
        Some(FrameMut {
            ptrs: self.ptrs,
            channels: self.channels,
            index: self.offset + index,
            _marker: PhantomData,
        })
    }

    /// Iterates frame by frame, writable — the planar equivalent of walking an interleaved
    /// buffer. `[audio-thread]`
    #[inline]
    #[must_use]
    pub fn iter_frames_mut(&mut self) -> FramesMut<'_, T> {
        FramesMut {
            ptrs: self.ptrs,
            channels: self.channels,
            next: self.offset,
            end: self.offset + self.frames,
            _marker: PhantomData,
        }
    }

    /// Iterates frame by frame, read-only. `[audio-thread]`
    #[inline]
    #[must_use]
    pub fn iter_frames(&self) -> Frames<'_, T> {
        self.as_ref().iter_frames()
    }
}

impl<T: Sample> Default for AudioBufferMut<'_, T> {
    #[inline]
    fn default() -> Self {
        Self::empty()
    }
}

impl<'a, 'b, T: Sample> IntoIterator for &'b mut AudioBufferMut<'a, T> {
    type Item = &'b mut [T];
    type IntoIter = ChannelsMut<'b, T>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.split_channels_mut()
    }
}

#[cold]
#[inline(never)]
fn out_of_range(index: usize, channels: usize) -> ! {
    panic!("channel index {index} out of range for {channels} channels");
}

// ---------------------------------------------------------------------------------------
// Channel iterators
// ---------------------------------------------------------------------------------------

/// Iterator over the channels of an [`AudioBufferRef`] as slices. `[audio-thread]`
pub struct Channels<'a, T: Sample> {
    ptrs: *const *const T,
    offset: usize,
    frames: usize,
    next: usize,
    end: usize,
    _marker: PhantomData<&'a [T]>,
}

impl<T: Sample> fmt::Debug for Channels<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Channels")
            .field("remaining", &(self.end - self.next))
            .field("frames", &self.frames)
            .finish()
    }
}

impl<'a, T: Sample> Iterator for Channels<'a, T> {
    type Item = &'a [T];

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        if self.next >= self.end {
            return None;
        }
        let index = self.next;
        self.next += 1;
        if self.frames == 0 {
            return Some(&[]);
        }
        // SAFETY: `index < end`, which is the channel count of the originating buffer, so
        // the array entry exists and — with `frames > 0` — covers `offset + frames`
        // initialised, readable samples for `'a`.
        Some(unsafe { channel_slice(self.ptrs, index, self.offset, self.frames) })
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.end - self.next;
        (remaining, Some(remaining))
    }

    #[inline]
    fn count(self) -> usize {
        self.end - self.next
    }
}

impl<T: Sample> DoubleEndedIterator for Channels<'_, T> {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.next >= self.end {
            return None;
        }
        self.end -= 1;
        if self.frames == 0 {
            return Some(&[]);
        }
        // SAFETY: `end` was greater than `next >= 0` before the decrement, so the new value
        // is a valid channel index of the originating buffer; see `next`.
        Some(unsafe { channel_slice(self.ptrs, self.end, self.offset, self.frames) })
    }
}

impl<T: Sample> ExactSizeIterator for Channels<'_, T> {}
impl<T: Sample> FusedIterator for Channels<'_, T> {}

/// Iterator handing out one exclusive slice per channel. `[audio-thread]`
///
/// Yielding `&'a mut [T]` rather than a borrow of the iterator is sound because the
/// channels of a mutable buffer never overlap (see [`AudioBufferMut::from_raw`]) and each
/// index is yielded exactly once, from one end or the other.
pub struct ChannelsMut<'a, T: Sample> {
    ptrs: *const *mut T,
    offset: usize,
    frames: usize,
    next: usize,
    end: usize,
    _marker: PhantomData<&'a mut [T]>,
}

impl<T: Sample> fmt::Debug for ChannelsMut<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChannelsMut")
            .field("remaining", &(self.end - self.next))
            .field("frames", &self.frames)
            .finish()
    }
}

impl<'a, T: Sample> Iterator for ChannelsMut<'a, T> {
    type Item = &'a mut [T];

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        if self.next >= self.end {
            return None;
        }
        let index = self.next;
        self.next += 1;
        if self.frames == 0 {
            return Some(empty_mut());
        }
        // SAFETY: `index < end <= channel_count`, so the array entry exists and covers
        // `offset + frames` writable samples for `'a`. `next` has just been advanced past
        // `index` and `end` never moves below `next`, so no end of this iterator can yield
        // the same channel again; combined with the `from_raw` guarantee that channels are
        // disjoint, every slice handed out is unique.
        Some(unsafe { channel_slice_mut(self.ptrs, index, self.offset, self.frames) })
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.end - self.next;
        (remaining, Some(remaining))
    }

    #[inline]
    fn count(self) -> usize {
        self.end - self.next
    }
}

impl<T: Sample> DoubleEndedIterator for ChannelsMut<'_, T> {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.next >= self.end {
            return None;
        }
        self.end -= 1;
        if self.frames == 0 {
            return Some(empty_mut());
        }
        // SAFETY: the decremented `end` is a valid channel index and is now outside the
        // range either end of the iterator will visit, so this channel can never be yielded
        // again; see `next` for the rest.
        Some(unsafe { channel_slice_mut(self.ptrs, self.end, self.offset, self.frames) })
    }
}

impl<T: Sample> ExactSizeIterator for ChannelsMut<'_, T> {}
impl<T: Sample> FusedIterator for ChannelsMut<'_, T> {}

// ---------------------------------------------------------------------------------------
// Frame views
// ---------------------------------------------------------------------------------------

/// One frame across all channels of a read-only buffer. `[audio-thread]`
///
/// Lets planar data be processed in the interleaved style — one frame at a time, all
/// channels together — without copying anything.
pub struct Frame<'a, T: Sample> {
    ptrs: *const *const T,
    channels: usize,
    /// Absolute sample index inside each channel: the view's offset is already added.
    index: usize,
    _marker: PhantomData<&'a [T]>,
}

impl<T: Sample> Clone for Frame<'_, T> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: Sample> Copy for Frame<'_, T> {}

impl<T: Sample> fmt::Debug for Frame<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Frame")
            .field("channels", &self.channels)
            .finish()
    }
}

impl<'a, T: Sample> Frame<'a, T> {
    /// Number of channels in this frame. `[audio-thread]`
    #[inline]
    #[must_use]
    pub const fn channel_count(&self) -> usize {
        self.channels
    }

    /// The sample of `channel`, or `None` if out of range. `[audio-thread]`
    #[inline]
    #[must_use]
    pub fn get(&self, channel: usize) -> Option<T> {
        if channel >= self.channels {
            return None;
        }
        // SAFETY: `channel < channels`, so the array entry exists and is initialised, and
        // `index` is a frame the originating buffer covers, so the sample lies inside that
        // channel's allocation, is initialised and aligned, and stays readable for `'a`.
        unsafe {
            let base = *self.ptrs.add(channel);
            Some(base.add(self.index).read())
        }
    }

    /// Iterates the frame's samples, one per channel. `[audio-thread]`
    #[inline]
    #[must_use]
    pub fn iter(&self) -> FrameSamples<'a, T> {
        FrameSamples {
            frame: *self,
            next: 0,
            end: self.channels,
        }
    }
}

impl<'a, T: Sample> IntoIterator for Frame<'a, T> {
    type Item = T;
    type IntoIter = FrameSamples<'a, T>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Iterator over one frame's samples, one per channel. `[audio-thread]`
pub struct FrameSamples<'a, T: Sample> {
    frame: Frame<'a, T>,
    next: usize,
    end: usize,
}

impl<T: Sample> fmt::Debug for FrameSamples<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameSamples")
            .field("remaining", &(self.end - self.next))
            .finish()
    }
}

impl<T: Sample> Iterator for FrameSamples<'_, T> {
    type Item = T;

    #[inline]
    fn next(&mut self) -> Option<T> {
        if self.next >= self.end {
            return None;
        }
        let channel = self.next;
        self.next += 1;
        self.frame.get(channel)
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.end - self.next;
        (remaining, Some(remaining))
    }
}

impl<T: Sample> DoubleEndedIterator for FrameSamples<'_, T> {
    #[inline]
    fn next_back(&mut self) -> Option<T> {
        if self.next >= self.end {
            return None;
        }
        self.end -= 1;
        self.frame.get(self.end)
    }
}

impl<T: Sample> ExactSizeIterator for FrameSamples<'_, T> {}
impl<T: Sample> FusedIterator for FrameSamples<'_, T> {}

/// Iterator over the frames of a read-only buffer. `[audio-thread]`
pub struct Frames<'a, T: Sample> {
    ptrs: *const *const T,
    channels: usize,
    next: usize,
    end: usize,
    _marker: PhantomData<&'a [T]>,
}

impl<T: Sample> fmt::Debug for Frames<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Frames")
            .field("remaining", &(self.end - self.next))
            .field("channels", &self.channels)
            .finish()
    }
}

impl<'a, T: Sample> Iterator for Frames<'a, T> {
    type Item = Frame<'a, T>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        if self.next >= self.end {
            return None;
        }
        let index = self.next;
        self.next += 1;
        Some(Frame {
            ptrs: self.ptrs,
            channels: self.channels,
            index,
            _marker: PhantomData,
        })
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.end - self.next;
        (remaining, Some(remaining))
    }
}

impl<T: Sample> DoubleEndedIterator for Frames<'_, T> {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.next >= self.end {
            return None;
        }
        self.end -= 1;
        Some(Frame {
            ptrs: self.ptrs,
            channels: self.channels,
            index: self.end,
            _marker: PhantomData,
        })
    }
}

impl<T: Sample> ExactSizeIterator for Frames<'_, T> {}
impl<T: Sample> FusedIterator for Frames<'_, T> {}

/// One frame across all channels of a writable buffer. `[audio-thread]`
pub struct FrameMut<'a, T: Sample> {
    ptrs: *const *mut T,
    channels: usize,
    index: usize,
    _marker: PhantomData<&'a mut [T]>,
}

impl<T: Sample> fmt::Debug for FrameMut<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameMut")
            .field("channels", &self.channels)
            .finish()
    }
}

impl<T: Sample> FrameMut<'_, T> {
    /// Number of channels in this frame. `[audio-thread]`
    #[inline]
    #[must_use]
    pub const fn channel_count(&self) -> usize {
        self.channels
    }

    /// The sample of `channel`, or `None` if out of range. `[audio-thread]`
    #[inline]
    #[must_use]
    pub fn get(&self, channel: usize) -> Option<T> {
        if channel >= self.channels {
            return None;
        }
        // SAFETY: `channel < channels`, so the array entry exists and is initialised, and
        // `index` is a frame the originating buffer covers, so the sample is inside that
        // channel's allocation and initialised. The read borrows `self` shared, so no
        // exclusive reference to the same sample is live.
        unsafe {
            let base = *self.ptrs.add(channel);
            Some(base.add(self.index).read())
        }
    }

    /// Writes the sample of `channel`; returns `false` if out of range. `[audio-thread]`
    #[inline]
    pub fn set(&mut self, channel: usize, value: T) -> bool {
        if channel >= self.channels {
            return false;
        }
        // SAFETY: `channel < channels`, so the array entry exists and is initialised, and
        // `index` is a frame the buffer covers, so the destination is inside that channel's
        // allocation, aligned and writable. `&mut self` makes this the only live reference
        // to it, and distinct frames of one buffer never share a sample.
        unsafe {
            let base = *self.ptrs.add(channel);
            base.add(self.index).write(value);
        }
        true
    }

    /// An exclusive reference to the sample of `channel`. `[audio-thread]`
    #[inline]
    #[must_use]
    pub fn get_mut(&mut self, channel: usize) -> Option<&mut T> {
        if channel >= self.channels {
            return None;
        }
        // SAFETY: `channel < channels`, so the array entry exists and is initialised; the
        // sample at `index` is inside that channel's allocation, initialised, aligned and
        // writable. The reference borrows `self` exclusively, distinct channels never
        // overlap and distinct frames never share a sample, so it cannot alias anything.
        unsafe {
            let base = *self.ptrs.add(channel);
            Some(&mut *base.add(self.index))
        }
    }
}

/// Iterator over the frames of a writable buffer. `[audio-thread]`
///
/// Yielding `FrameMut<'a, T>` rather than a borrow of the iterator is sound because
/// distinct frames address distinct samples and each index is yielded exactly once.
pub struct FramesMut<'a, T: Sample> {
    ptrs: *const *mut T,
    channels: usize,
    next: usize,
    end: usize,
    _marker: PhantomData<&'a mut [T]>,
}

impl<T: Sample> fmt::Debug for FramesMut<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FramesMut")
            .field("remaining", &(self.end - self.next))
            .field("channels", &self.channels)
            .finish()
    }
}

impl<'a, T: Sample> Iterator for FramesMut<'a, T> {
    type Item = FrameMut<'a, T>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        if self.next >= self.end {
            return None;
        }
        let index = self.next;
        self.next += 1;
        Some(FrameMut {
            ptrs: self.ptrs,
            channels: self.channels,
            index,
            _marker: PhantomData,
        })
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.end - self.next;
        (remaining, Some(remaining))
    }
}

impl<T: Sample> DoubleEndedIterator for FramesMut<'_, T> {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.next >= self.end {
            return None;
        }
        self.end -= 1;
        Some(FrameMut {
            ptrs: self.ptrs,
            channels: self.channels,
            index: self.end,
            _marker: PhantomData,
        })
    }
}

impl<T: Sample> ExactSizeIterator for FramesMut<'_, T> {}
impl<T: Sample> FusedIterator for FramesMut<'_, T> {}

// ---------------------------------------------------------------------------------------
// Safe constructors from Rust slices
// ---------------------------------------------------------------------------------------

/// Builds a read-only view from per-channel slices, using `scratch` to hold the pointer
/// array the view needs. `[any-thread]`
///
/// Entirely safe: the borrow checker keeps the channels and the scratch array alive for as
/// long as the view. Allocation-free — the caller owns both buffers — so it is usable from
/// the audio thread when the scratch array was preallocated.
///
/// # Errors
///
/// [`AudioError::SizeMismatch`] if `scratch` is shorter than `channels`, or
/// [`AudioError::FrameCountMismatch`] if the channels are not all the same length.
pub fn view_from_slices<'a, T: Sample>(
    channels: &[&'a [T]],
    scratch: &'a mut [*const T],
) -> AudioResult<AudioBufferRef<'a, T>> {
    if scratch.len() < channels.len() {
        return Err(AudioError::SizeMismatch {
            expected: channels.len(),
            found: scratch.len(),
        });
    }
    let frames = channels.first().map_or(0, |c| c.len());
    for channel in channels {
        if channel.len() != frames {
            return Err(AudioError::FrameCountMismatch {
                expected: frames,
                found: channel.len(),
            });
        }
    }
    for (slot, channel) in scratch.iter_mut().zip(channels) {
        *slot = channel.as_ptr();
    }
    // SAFETY: `scratch` now holds `channels.len()` pointers taken from live shared slices
    // that all borrow `'a`; each is non-null, aligned and covers `frames` initialised
    // samples. `scratch` is itself borrowed for `'a`, so the pointer array outlives the
    // view, and the shared borrows keep the samples immutable throughout.
    Ok(unsafe { AudioBufferRef::from_raw(scratch.as_ptr(), channels.len(), frames) })
}

/// Builds a writable view from per-channel slices, using `scratch` to hold the pointer
/// array. `[any-thread]`
///
/// Entirely safe: `channels` is borrowed exclusively for `'a`, so the caller cannot reach
/// the underlying slices again while the view exists, and Rust's own rules guarantee the
/// channels do not overlap. Allocation-free.
///
/// # Errors
///
/// [`AudioError::SizeMismatch`] if `scratch` is shorter than `channels`, or
/// [`AudioError::FrameCountMismatch`] if the channels are not all the same length.
pub fn view_from_slices_mut<'a, T: Sample>(
    channels: &'a mut [&'a mut [T]],
    scratch: &'a mut [*mut T],
) -> AudioResult<AudioBufferMut<'a, T>> {
    if scratch.len() < channels.len() {
        return Err(AudioError::SizeMismatch {
            expected: channels.len(),
            found: scratch.len(),
        });
    }
    let frames = channels.first().map_or(0, |c| c.len());
    for channel in channels.iter() {
        if channel.len() != frames {
            return Err(AudioError::FrameCountMismatch {
                expected: frames,
                found: channel.len(),
            });
        }
    }
    let count = channels.len();
    for (slot, channel) in scratch.iter_mut().zip(channels.iter_mut()) {
        *slot = channel.as_mut_ptr();
    }
    // SAFETY: `scratch` now holds `count` pointers derived from distinct `&mut [T]`s, so
    // they are non-null, aligned, writable for `frames` samples and — because Rust
    // guarantees exclusive references never alias — pairwise disjoint, which is the extra
    // requirement `AudioBufferMut::from_raw` makes. Both `channels` and `scratch` are
    // borrowed exclusively for `'a`, so the pointer array outlives the view and nothing
    // else can reach the samples while it is alive.
    Ok(unsafe { AudioBufferMut::from_raw(scratch.as_ptr(), count, frames) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::AudioStorage;

    fn ramp(channels: usize, frames: usize) -> AudioStorage<f32> {
        let mut s = AudioStorage::<f32>::new(channels, frames);
        {
            let mut buf = s.as_mut();
            for (c, channel) in buf.split_channels_mut().enumerate() {
                for (f, sample) in channel.iter_mut().enumerate() {
                    *sample = (c * 100 + f) as f32;
                }
            }
        }
        s
    }

    #[test]
    fn empty_views_are_inert() {
        let r = AudioBufferRef::<f32>::empty();
        assert_eq!(r.channel_count(), 0);
        assert_eq!(r.frames(), 0);
        assert_eq!(r.sample_count(), 0);
        assert!(r.is_empty());
        assert!(r.get_channel(0).is_none());
        assert!(r.channel_ptr(0).is_none());
        assert!(r.sample(0, 0).is_none());
        assert_eq!(r.iter().count(), 0);
        assert_eq!(r.iter_frames().count(), 0);
        assert!(r.frame(0).is_none());
        assert!(!r.all_channels_constant());
        assert!(!r.is_channel_constant(0));
        assert_eq!(r.scan_constant_mask(), 0);
        assert!(!r.scan_channel_constant(0));
        assert!(!r.as_ptr().is_null());

        let mut m = AudioBufferMut::<f64>::empty();
        assert!(m.is_empty());
        assert!(m.get_channel_mut(0).is_none());
        assert!(m.channel_ptr_mut(0).is_none());
        assert_eq!(m.split_channels_mut().count(), 0);
        assert_eq!(m.iter_frames_mut().count(), 0);
        assert!(m.frame_mut(0).is_none());
        assert!(m.channel_pair_mut(0, 1).is_none());
        m.fill_silence();
        assert_eq!(m.constant_mask(), 0);
        assert_eq!(m.as_ref().channel_count(), 0);
        assert_eq!(AudioBufferRef::<f32>::default().channel_count(), 0);
        assert_eq!(AudioBufferMut::<f32>::default().frames(), 0);
    }

    #[test]
    fn zero_channels_with_frames() {
        let mut s = AudioStorage::<f32>::new(0, 64);
        let r = s.as_ref();
        assert_eq!(r.frames(), 64);
        assert_eq!(r.channel_count(), 0);
        assert_eq!(r.iter().count(), 0);
        // Frames still exist, they simply have no channels in them.
        assert_eq!(r.iter_frames().count(), 64);
        assert_eq!(r.frame(0).unwrap().channel_count(), 0);
        assert!(r.frame(0).unwrap().get(0).is_none());
        assert_eq!(r.frame(0).unwrap().iter().count(), 0);

        let mut buf = s.as_mut();
        buf.fill_silence();
        assert_eq!(buf.constant_mask(), 0);
        assert_eq!(buf.split_channels_mut().count(), 0);
    }

    #[test]
    fn channels_with_zero_frames() {
        let mut z = AudioStorage::<f32>::new(3, 0);
        let r = z.as_ref();
        assert_eq!(r.channel_count(), 3);
        assert!(r.channel(2).is_empty());
        assert_eq!(r.iter().count(), 3);
        assert!(r.iter().all(<[f32]>::is_empty));
        assert_eq!(r.iter_frames().count(), 0);
        assert!(r.scan_channel_constant(0));
        assert_eq!(r.scan_constant_mask(), 0b111);
        assert!(r.channel_ptr(0).is_some());

        let mut buf = z.as_mut();
        assert_eq!(buf.split_channels_mut().count(), 3);
        for channel in buf.split_channels_mut() {
            assert!(channel.is_empty());
        }
        let (a, b) = buf.channel_pair_mut(0, 2).unwrap();
        assert!(a.is_empty() && b.is_empty());
        assert!(buf.channel_mut(1).is_empty());
        buf.fill_silence();
        assert_eq!(buf.constant_mask(), 0b111);
    }

    #[test]
    fn single_frame_and_single_channel() {
        let s = ramp(1, 1);
        let r = s.as_ref();
        assert_eq!(r.channel_count(), 1);
        assert_eq!(r.frames(), 1);
        assert_eq!(r.sample_count(), 1);
        assert_eq!(r.channel(0), &[0.0]);
        assert_eq!(r.sample(0, 0), Some(0.0));
        assert!(r.sample(0, 1).is_none());
        assert!(r.sample(1, 0).is_none());
        assert_eq!(r.iter_frames().count(), 1);
        assert!(r.scan_channel_constant(0));
    }

    #[test]
    fn channel_access_and_iteration() {
        let s = ramp(3, 4);
        let r = s.as_ref();
        assert_eq!(r.channel(0), &[0.0, 1.0, 2.0, 3.0]);
        assert_eq!(r.channel(2), &[200.0, 201.0, 202.0, 203.0]);
        assert!(r.get_channel(3).is_none());
        assert_eq!(r.sample(1, 3), Some(103.0));

        let collected: Vec<&[f32]> = r.iter().collect();
        assert_eq!(collected.len(), 3);
        assert_eq!(collected[1][0], 100.0);
        assert_eq!(r.iter().len(), 3);
        assert_eq!(r.iter().count(), 3);

        // Both ends, and the two must never overlap.
        let mut it = r.iter();
        assert_eq!(it.next().unwrap()[0], 0.0);
        assert_eq!(it.next_back().unwrap()[0], 200.0);
        assert_eq!(it.next().unwrap()[0], 100.0);
        assert!(it.next().is_none());
        assert!(it.next_back().is_none());

        // `for x in buffer` and `for x in &buffer` both work.
        assert_eq!((&r).into_iter().count(), 3);
        assert_eq!(r.into_iter().count(), 3);
    }

    #[test]
    #[should_panic(expected = "channel index 3 out of range for 3 channels")]
    fn channel_panics_out_of_range() {
        let s = ramp(3, 2);
        let _ = s.as_ref().channel(3);
    }

    #[test]
    #[should_panic(expected = "channel index 9 out of range for 2 channels")]
    fn channel_mut_panics_out_of_range() {
        let mut s = ramp(2, 2);
        let _ = s.as_mut().channel_mut(9);
    }

    #[test]
    fn split_channels_mut_hands_out_disjoint_slices() {
        let mut s = AudioStorage::<f32>::new(4, 8);
        {
            let mut buf = s.as_mut();
            let mut channels: Vec<&mut [f32]> = buf.split_channels_mut().collect();
            assert_eq!(channels.len(), 4);
            // All four are alive at once; writing through one must not disturb the others.
            for (c, channel) in channels.iter_mut().enumerate() {
                channel.fill(c as f32);
            }
        }
        for c in 0..4 {
            assert!(s.as_ref().channel(c).iter().all(|&v| v == c as f32));
        }
    }

    #[test]
    fn channel_pair_mut_rejects_aliasing_requests() {
        let mut s = AudioStorage::<f32>::new(2, 4);
        {
            let mut buf = s.as_mut();
            assert!(buf.channel_pair_mut(0, 0).is_none());
            assert!(buf.channel_pair_mut(0, 2).is_none());
            assert!(buf.channel_pair_mut(2, 0).is_none());
            let (left, right) = buf.channel_pair_mut(0, 1).unwrap();
            left.fill(1.0);
            right.fill(-1.0);
        }
        assert_eq!(s.as_ref().channel(0)[0], 1.0);
        assert_eq!(s.as_ref().channel(1)[0], -1.0);
    }

    #[test]
    fn frame_iteration_reads_all_channels() {
        let s = ramp(2, 3);
        let r = s.as_ref();
        let frames: Vec<Vec<f32>> = r.iter_frames().map(|f| f.iter().collect()).collect();
        assert_eq!(
            frames,
            vec![vec![0.0, 100.0], vec![1.0, 101.0], vec![2.0, 102.0]]
        );
        let f0 = r.frame(0).unwrap();
        assert_eq!(f0.channel_count(), 2);
        assert_eq!(f0.get(0), Some(0.0));
        assert_eq!(f0.get(1), Some(100.0));
        assert_eq!(f0.get(2), None);
        assert!(r.frame(3).is_none());

        let mut it = r.iter_frames();
        assert_eq!(it.len(), 3);
        assert_eq!(it.next_back().unwrap().get(0), Some(2.0));
        assert_eq!(it.next().unwrap().get(0), Some(0.0));
        assert_eq!(it.len(), 1);
        assert_eq!(it.next().unwrap().get(0), Some(1.0));
        assert!(it.next().is_none());

        let mut samples = f0.iter();
        assert_eq!(samples.next_back(), Some(100.0));
        assert_eq!(samples.next(), Some(0.0));
        assert_eq!(samples.next(), None);
        assert_eq!(f0.into_iter().count(), 2);
    }

    #[test]
    fn frame_iteration_writes_all_channels() {
        let mut s = AudioStorage::<f32>::new(2, 3);
        {
            let mut buf = s.as_mut();
            for (i, mut frame) in buf.iter_frames_mut().enumerate() {
                assert!(frame.set(0, i as f32));
                assert!(frame.set(1, -(i as f32)));
                assert!(!frame.set(2, 0.0));
                assert_eq!(frame.get(0), Some(i as f32));
                assert_eq!(frame.get(2), None);
                *frame.get_mut(0).unwrap() += 0.5;
                assert!(frame.get_mut(5).is_none());
                assert_eq!(frame.channel_count(), 2);
            }
        }
        assert_eq!(s.as_ref().channel(0), &[0.5, 1.5, 2.5]);
        assert_eq!(s.as_ref().channel(1), &[0.0, -1.0, -2.0]);

        // Frames can be collected and written out of order: they are disjoint.
        {
            let mut buf = s.as_mut();
            let mut frames: Vec<FrameMut<'_, f32>> = buf.iter_frames_mut().collect();
            assert_eq!(frames.len(), 3);
            frames[2].set(0, 99.0);
            frames[0].set(0, -99.0);
        }
        assert_eq!(s.as_ref().channel(0), &[-99.0, 1.5, 99.0]);

        let mut buf = s.as_mut();
        {
            let mut it = buf.iter_frames_mut();
            assert_eq!(it.len(), 3);
            assert_eq!(it.next_back().unwrap().get(0), Some(99.0));
            assert_eq!(it.next().unwrap().get(0), Some(-99.0));
            assert_eq!(it.len(), 1);
        }
        assert!(buf.frame_mut(3).is_none());
    }

    #[test]
    fn sub_blocks_alias_the_parent_without_copying() {
        let s = ramp(2, 8);
        let r = s.as_ref();
        let tail = r.sub_block(4, 4).unwrap();
        assert_eq!(tail.frames(), 4);
        assert_eq!(tail.frame_offset(), 4);
        assert_eq!(tail.channel(0), &[4.0, 5.0, 6.0, 7.0]);
        assert_eq!(tail.channel(1), &[104.0, 105.0, 106.0, 107.0]);
        assert_eq!(tail.iter_frames().count(), 4);
        assert_eq!(tail.frame(0).unwrap().get(0), Some(4.0));
        assert_eq!(tail.iter().next().unwrap(), &[4.0, 5.0, 6.0, 7.0]);

        // Nested sub-blocks compose.
        let inner = tail.sub_block(1, 2).unwrap();
        assert_eq!(inner.channel(0), &[5.0, 6.0]);
        assert_eq!(inner.frame_offset(), 5);

        // Boundaries.
        assert_eq!(r.sub_block(8, 0).unwrap().frames(), 0);
        assert_eq!(r.sub_block(0, 8).unwrap().frames(), 8);
        assert!(r.sub_block(8, 1).is_none());
        assert!(r.sub_block(9, 0).is_none());
        assert!(r.sub_block(1, usize::MAX).is_none());

        let (head, rest) = r.split_at_frame(3).unwrap();
        assert_eq!(head.frames(), 3);
        assert_eq!(rest.frames(), 5);
        assert_eq!(rest.channel(0)[0], 3.0);
        assert!(r.split_at_frame(9).is_none());
        let (all, none) = r.split_at_frame(8).unwrap();
        assert_eq!(all.frames(), 8);
        assert_eq!(none.frames(), 0);
    }

    #[test]
    fn split_at_frame_mut_writes_two_halves_at_once() {
        let mut s = AudioStorage::<f32>::new(2, 6);
        {
            let mut buf = s.as_mut();
            let (mut head, mut tail) = buf.split_at_frame_mut(2).unwrap();
            assert_eq!(head.frames(), 2);
            assert_eq!(tail.frames(), 4);
            head.fill(1.0);
            tail.fill(2.0);
        }
        assert_eq!(s.as_ref().channel(0), &[1.0, 1.0, 2.0, 2.0, 2.0, 2.0]);
        assert_eq!(s.as_ref().channel(1), &[1.0, 1.0, 2.0, 2.0, 2.0, 2.0]);

        {
            let mut buf = s.as_mut();
            assert!(buf.split_at_frame_mut(7).is_none());
            let (empty, whole) = buf.split_at_frame_mut(0).unwrap();
            assert_eq!(empty.frames(), 0);
            assert_eq!(whole.frames(), 6);
        }

        // Sub-block writes only touch their own range.
        s.as_mut().sub_block_mut(1, 2).unwrap().fill(9.0);
        assert_eq!(s.as_ref().channel(0), &[1.0, 9.0, 9.0, 2.0, 2.0, 2.0]);
        let mut buf = s.as_mut();
        assert!(buf.sub_block_mut(5, 2).is_none());
        assert!(buf.sub_block_mut(2, usize::MAX).is_none());
        assert_eq!(buf.sub_block_mut(6, 0).unwrap().frames(), 0);
    }

    #[test]
    fn constant_mask_edges() {
        let s = ramp(3, 4);
        let r = s.as_ref();
        // A zero mask claims nothing.
        assert_eq!(r.constant_mask(), 0);
        assert!(!r.is_channel_constant(0));
        assert!(!r.all_channels_constant());

        let flagged = r.with_constant_mask(0b101);
        assert!(flagged.is_channel_constant(0));
        assert!(!flagged.is_channel_constant(1));
        assert!(flagged.is_channel_constant(2));
        // Bits beyond the channel count never report true.
        assert!(!flagged.is_channel_constant(3));
        assert!(!flagged.is_channel_constant(usize::MAX));
        assert!(!flagged.all_channels_constant());
        assert!(r.with_constant_mask(0b111).all_channels_constant());
        assert!(r.with_constant_mask(u64::MAX).all_channels_constant());
        // The mask survives sub-blocking: constant over the block implies constant over any
        // part of it.
        assert!(
            r.with_constant_mask(0b101)
                .sub_block(1, 2)
                .unwrap()
                .is_channel_constant(2)
        );
    }

    #[test]
    fn constant_mask_beyond_64_channels() {
        let s = AudioStorage::<f32>::new(65, 1);
        let r = s.as_ref().with_constant_mask(u64::MAX);
        assert!(r.is_channel_constant(63));
        assert!(!r.is_channel_constant(64));
        // 65 channels cannot be described by a 64-bit mask, so "all constant" is false.
        assert!(!r.all_channels_constant());
        // Scanning only ever reports the first 64.
        assert_eq!(s.as_ref().scan_constant_mask(), u64::MAX);

        let exactly_64 = AudioStorage::<f32>::new(64, 1);
        assert!(
            exactly_64
                .as_ref()
                .with_constant_mask(u64::MAX)
                .all_channels_constant()
        );
        assert_eq!(exactly_64.as_ref().scan_constant_mask(), u64::MAX);
    }

    #[test]
    fn scanning_detects_real_constants() {
        let mut s = AudioStorage::<f32>::new(3, 4);
        {
            let mut buf = s.as_mut();
            buf.channel_mut(0).fill(0.0);
            buf.channel_mut(1).fill(0.25);
            buf.channel_mut(2).copy_from_slice(&[0.0, 0.0, 0.0, 1.0]);
        }
        let r = s.as_ref();
        assert!(r.scan_channel_constant(0));
        assert!(r.scan_channel_constant(1));
        assert!(!r.scan_channel_constant(2));
        assert!(!r.scan_channel_constant(3));
        assert_eq!(r.scan_constant_mask(), 0b011);

        // NaN never equals itself, so a NaN channel is never "constant".
        s.as_mut().channel_mut(0).fill(f32::NAN);
        assert!(!s.as_ref().scan_channel_constant(0));
        // +0.0 and -0.0 compare equal, so a mix of both still counts as constant silence.
        s.as_mut()
            .channel_mut(0)
            .copy_from_slice(&[0.0, -0.0, 0.0, -0.0]);
        assert!(s.as_ref().scan_channel_constant(0));
    }

    #[test]
    fn fill_and_silence_set_the_mask() {
        let mut s = ramp(3, 4);
        let mut buf = s.as_mut();
        buf.fill(0.5);
        assert_eq!(buf.constant_mask(), 0b111);
        assert!(buf.is_channel_constant(2));
        assert!(!buf.is_channel_constant(3));
        assert!(buf.channel(0).iter().all(|&v| v == 0.5));
        buf.fill_silence();
        assert!(buf.channel(1).iter().all(|&v| v == 0.0));
        buf.clear_constant_mask();
        assert_eq!(buf.constant_mask(), 0);
        buf.set_constant_mask(0b010);
        assert!(buf.is_channel_constant(1));
    }

    #[test]
    fn copy_from_checks_shapes() {
        let src = ramp(2, 4);
        let mut dst = AudioStorage::<f32>::new(2, 4);
        {
            let mut buf = dst.as_mut();
            buf.copy_from(&src.as_ref().with_constant_mask(0b10))
                .unwrap();
            // The mask travels with the copy.
            assert_eq!(buf.constant_mask(), 0b10);
        }
        assert_eq!(dst.as_ref().channel(0), src.as_ref().channel(0));
        assert_eq!(dst.as_ref().channel(1), src.as_ref().channel(1));

        let mut wrong_channels = AudioStorage::<f32>::new(3, 4);
        assert_eq!(
            wrong_channels.as_mut().copy_from(&src.as_ref()),
            Err(AudioError::ChannelCountMismatch {
                expected: 3,
                found: 2
            })
        );
        let mut wrong_frames = AudioStorage::<f32>::new(2, 5);
        assert_eq!(
            wrong_frames.as_mut().copy_from(&src.as_ref()),
            Err(AudioError::FrameCountMismatch {
                expected: 5,
                found: 4
            })
        );
        // Nothing was written by the failed calls.
        assert!(wrong_frames.as_ref().channel(0).iter().all(|&v| v == 0.0));

        // Empty shapes are legal.
        let empty = AudioStorage::<f32>::new(0, 0);
        AudioStorage::<f32>::new(0, 0)
            .as_mut()
            .copy_from(&empty.as_ref())
            .unwrap();
        let zero_frames = AudioStorage::<f32>::new(2, 0);
        AudioStorage::<f32>::new(2, 0)
            .as_mut()
            .copy_from(&zero_frames.as_ref())
            .unwrap();
    }

    #[test]
    fn copy_from_is_safe_when_source_and_destination_are_the_same_memory() {
        // In-place processing: the host hands the plug-in one allocation for both
        // directions. `copy_from` must be a no-op, not undefined behaviour.
        let mut s = ramp(2, 4);
        let before: Vec<f32> = s.as_ref().channel(0).to_vec();
        {
            let mut dst = s.as_mut();
            let ptrs: Vec<*const f32> = (0..dst.channel_count())
                .map(|c| dst.channel_ptr_mut(c).unwrap().cast_const())
                .collect();
            // SAFETY: the pointers were just taken from `dst`, which is alive for the whole
            // block and covers 4 readable frames per channel. The shared view is only ever
            // read from, and `copy_from` touches the samples through raw pointers only, so
            // no `&`/`&mut` pair over the same memory is ever created.
            let input = unsafe { AudioBufferRef::<f32>::from_raw(ptrs.as_ptr(), 2, 4) };
            dst.copy_from(&input).unwrap();
        }
        assert_eq!(s.as_ref().channel(0), before.as_slice());
    }

    #[test]
    fn reborrow_and_as_ref_track_the_parent() {
        let mut s = ramp(2, 4);
        let mut buf = s.as_mut();
        buf.set_constant_mask(0b01);
        {
            let mut short = buf.reborrow();
            assert_eq!(short.frames(), 4);
            assert_eq!(short.constant_mask(), 0b01);
            short.channel_mut(0)[0] = 42.0;
        }
        assert_eq!(buf.channel(0)[0], 42.0);
        let read_only = buf.as_ref();
        assert_eq!(read_only.channel(0)[0], 42.0);
        assert_eq!(read_only.constant_mask(), 0b01);
        assert_eq!(buf.iter().count(), 2);
        assert_eq!(buf.iter_frames().count(), 4);
        assert_eq!(buf.sample_count(), 8);
        assert_eq!(buf.frame_offset(), 0);
        assert!(!buf.as_ptr().is_null());
    }

    #[test]
    fn iterator_adaptors_over_channels_mut() {
        let mut s = AudioStorage::<f32>::new(3, 2);
        {
            let mut buf = s.as_mut();
            assert_eq!(buf.split_channels_mut().len(), 3);
            assert_eq!(buf.split_channels_mut().count(), 3);
            let mut it = buf.split_channels_mut();
            it.next().unwrap().fill(1.0);
            it.next_back().unwrap().fill(3.0);
            it.next().unwrap().fill(2.0);
            assert!(it.next().is_none());
            assert!(it.next_back().is_none());
        }
        assert_eq!(s.as_ref().channel(0), &[1.0, 1.0]);
        assert_eq!(s.as_ref().channel(1), &[2.0, 2.0]);
        assert_eq!(s.as_ref().channel(2), &[3.0, 3.0]);

        // `for channel in &mut buffer`, and `iter_mut` as the idiomatic alias.
        {
            let mut buf = s.as_mut();
            for channel in &mut buf {
                channel.fill(0.0);
            }
            assert_eq!(buf.iter_mut().count(), 3);
        }
        assert!(s.as_ref().channel(2).iter().all(|&v| v == 0.0));
    }

    #[test]
    fn views_from_rust_slices() {
        let left = [1.0f32, 2.0, 3.0];
        let right = [4.0f32, 5.0, 6.0];
        let channels: [&[f32]; 2] = [&left, &right];
        let mut scratch = [core::ptr::null::<f32>(); 2];
        let view = view_from_slices(&channels, &mut scratch).unwrap();
        assert_eq!(view.channel_count(), 2);
        assert_eq!(view.frames(), 3);
        assert_eq!(view.channel(1), &[4.0, 5.0, 6.0]);

        // Zero channels is fine and yields an empty view.
        let mut scratch: [*const f32; 0] = [];
        let view = view_from_slices::<f32>(&[], &mut scratch).unwrap();
        assert!(view.is_empty());
    }

    #[test]
    fn views_from_rust_slices_reject_bad_input() {
        let left = [1.0f32, 2.0];
        let right = [4.0f32];
        let channels: [&[f32]; 2] = [&left, &right];
        let mut scratch = [core::ptr::null::<f32>(); 2];
        assert_eq!(
            view_from_slices(&channels, &mut scratch).unwrap_err(),
            AudioError::FrameCountMismatch {
                expected: 2,
                found: 1
            }
        );
        let mut small = [core::ptr::null::<f32>(); 1];
        assert_eq!(
            view_from_slices(&channels, &mut small).unwrap_err(),
            AudioError::SizeMismatch {
                expected: 2,
                found: 1
            }
        );
    }

    #[test]
    fn mutable_views_from_rust_slices() {
        let mut left = [0.0f32; 3];
        let mut right = [0.0f32; 3];
        {
            let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
            let mut scratch = [core::ptr::null_mut::<f32>(); 2];
            let mut view = view_from_slices_mut(&mut channels, &mut scratch).unwrap();
            assert_eq!(view.channel_count(), 2);
            assert_eq!(view.frames(), 3);
            view.fill(7.0);
            view.channel_mut(1)[0] = -1.0;
        }
        assert_eq!(left, [7.0, 7.0, 7.0]);
        assert_eq!(right, [-1.0, 7.0, 7.0]);
    }

    #[test]
    fn mutable_views_from_rust_slices_reject_bad_input() {
        let mut left = [0.0f32; 3];
        let mut right = [0.0f32; 2];
        let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
        let mut scratch = [core::ptr::null_mut::<f32>(); 2];
        assert_eq!(
            view_from_slices_mut(&mut channels, &mut scratch).unwrap_err(),
            AudioError::FrameCountMismatch {
                expected: 3,
                found: 2
            }
        );
    }

    #[test]
    fn mutable_views_reject_a_short_scratch_array() {
        let mut left = [0.0f32; 2];
        let mut right = [0.0f32; 2];
        let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
        let mut small = [core::ptr::null_mut::<f32>(); 1];
        assert_eq!(
            view_from_slices_mut(&mut channels, &mut small).unwrap_err(),
            AudioError::SizeMismatch {
                expected: 2,
                found: 1
            }
        );
    }

    #[test]
    fn views_are_send_and_sync() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<AudioBufferRef<'_, f32>>();
        assert_sync::<AudioBufferRef<'_, f32>>();
        assert_send::<AudioBufferMut<'_, f64>>();
        assert_sync::<AudioBufferMut<'_, f64>>();
    }

    #[test]
    fn debug_output_is_informative() {
        let mut s = ramp(2, 4);
        assert!(format!("{:?}", s.as_ref()).contains("channels: 2"));
        assert!(format!("{:?}", s.as_ref().iter()).contains("remaining"));
        assert!(format!("{:?}", s.as_ref().iter_frames()).contains("channels"));
        assert!(format!("{:?}", s.as_ref().frame(0).unwrap()).contains("channels"));
        assert!(format!("{:?}", s.as_ref().frame(0).unwrap().iter()).contains("remaining"));
        let mut buf = s.as_mut();
        assert!(format!("{buf:?}").contains("frames: 4"));
        assert!(format!("{:?}", buf.split_channels_mut()).contains("remaining"));
        assert!(format!("{:?}", buf.iter_frames_mut()).contains("channels"));
        assert!(format!("{:?}", buf.frame_mut(0).unwrap()).contains("channels"));
    }

    #[test]
    fn full_mask_saturates() {
        assert_eq!(full_mask(0), 0);
        assert_eq!(full_mask(1), 1);
        assert_eq!(full_mask(63), (1u64 << 63) - 1);
        assert_eq!(full_mask(64), u64::MAX);
        assert_eq!(full_mask(usize::MAX), u64::MAX);
    }
}
