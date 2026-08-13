//! [`SharedRegion`]: a description of memory two processes can both see.
//!
//! # What this type is, and what it is not
//!
//! A `SharedRegion` is a **descriptor**, not an owner. It records where a mapping starts,
//! how long it is, what it is aligned to and whether this process may write it. Creating
//! one is `unsafe` precisely because the descriptor makes no promise about the memory: the
//! thing that mapped it — an OS shared-memory object, or [`LoopbackDataPlane`] in-process —
//! is what keeps it alive, and it is that owner's job to outlive every descriptor it hands
//! out.
//!
//! # Why every accessor is `unsafe`
//!
//! The peer can write this memory. Rust's aliasing rules say a `&[u8]` must not change
//! underneath its holder, so borrowing shared memory as a slice is only sound while this
//! process has *exclusive* access to it. That exclusivity comes from the ownership
//! handshake in [`DataPlane`](crate::DataPlane), not from the type system, so every
//! accessor here is `unsafe` and states the requirement in its safety contract. The rule is
//! always the same: **hold the region between a successful `acquire` and the matching
//! `publish`, and never across them.**
//!
//! What is *not* left to the caller is arithmetic. Every offset and length is bounds-checked
//! against the real length of the region, every plane is alignment-checked before it is
//! read as samples, and every [`AudioBlockHeader`] is validated against the region that
//! holds it. A hostile header cannot point a "channel" at memory outside the mapping,
//! because the pointer is computed from the region rather than believed from the header.
//!
//! [`LoopbackDataPlane`]: crate::LoopbackDataPlane

use core::ptr::NonNull;

use daux_protocol::{AudioBlockHeader, ProtocolLimits, sample_bytes};

use crate::error::{IpcError, IpcResult};

/// What this process may do with a mapped region. [any-thread]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RegionRole {
    /// The mapping is read-only here; the peer produces its contents.
    ReadOnly,
    /// This process may write the region while it owns it.
    ReadWrite,
}

impl RegionRole {
    /// [any-thread] `true` when writes are permitted.
    #[inline]
    #[must_use]
    pub const fn is_writable(self) -> bool {
        matches!(self, Self::ReadWrite)
    }
}

/// A mapped block of memory shared with the peer process. [audio-thread]
///
/// `Send` but not `Sync`: a region descriptor is handed to the audio thread for the
/// lifetime of a stream, but two threads holding it at once would defeat the ownership
/// handshake that makes access to the memory sound in the first place.
///
/// # Why every accessor is `unsafe`
///
/// The peer can write this memory, and Rust's aliasing rules say a `&[u8]` must not change
/// underneath its holder — so borrowing shared memory as a slice is sound only while this
/// process has *exclusive* access to it. That exclusivity comes from the ownership
/// handshake, not from the type system, and every accessor states it in its safety
/// contract. The rule is always the same: **hold the region between a successful
/// [`DataPlane::acquire`](crate::DataPlane::acquire) and the matching
/// [`publish`](crate::DataPlane::publish), and never across them.**
///
/// What is *not* left to the caller is arithmetic. Every offset and length is
/// bounds-checked against the real length of the region, every plane is alignment-checked
/// before it is read as samples, and every [`AudioBlockHeader`] is validated against the
/// region that holds it — so a hostile header cannot point a "channel" at memory outside
/// the mapping.
#[derive(Debug)]
pub struct SharedRegion {
    base: NonNull<u8>,
    len: usize,
    align: usize,
    role: RegionRole,
}

// SAFETY: `SharedRegion` owns no memory — it is a pointer, a length, an alignment and a
// role, all of them plain data. Moving that description to another thread cannot race with
// anything, because the description itself is never mutated through a shared reference and
// no destructor touches the pointee. Every operation that actually dereferences `base` is
// an `unsafe fn` whose contract already requires the caller to hold exclusive access under
// the data-plane ownership handshake, so `Send` adds no obligation that was not already
// there. `Sync` is deliberately *not* implemented: sharing one descriptor between threads
// would let two of them believe they own the same region.
unsafe impl Send for SharedRegion {}

impl SharedRegion {
    /// [main-thread] Describes a mapping that already exists.
    ///
    /// The obvious mistakes are caught rather than trusted: a null base, a zero length, an
    /// alignment that is not a power of two, and a base that does not satisfy the alignment
    /// it claims. Those checks do not replace the safety contract below — they only turn
    /// the errors a caller can be told about into errors instead of undefined behaviour.
    ///
    /// # Safety
    ///
    /// The caller guarantees all of the following for as long as the returned
    /// `SharedRegion` — or any slice derived from it — is alive:
    ///
    /// * `base` points at a single mapping of at least `len` bytes that stays mapped, at
    ///   the same address, in this process;
    /// * every byte of the mapping is initialised, so reading it is never a read of
    ///   uninitialised memory (map with zero-filled pages, which every OS provides);
    /// * the mapping is not unmapped, resized or remapped by anyone while this descriptor
    ///   exists;
    /// * `role` is honest: the mapping really is writable if [`RegionRole::ReadWrite`] is
    ///   claimed, or writing through it will fault;
    /// * accesses obey the ownership handshake, so that no other process or thread reads or
    ///   writes the bytes this process is reading or writing.
    ///
    /// # Errors
    ///
    /// [`IpcErrorKind::InvalidArgument`](crate::IpcErrorKind::InvalidArgument) for a null
    /// base, a zero length, an alignment that is not a power of two, or a base that is not
    /// aligned to it.
    pub unsafe fn from_raw_parts(
        base: *mut u8,
        len: usize,
        align: usize,
        role: RegionRole,
    ) -> IpcResult<Self> {
        let base = NonNull::new(base).ok_or(IpcError::invalid_argument("SharedRegion::base"))?;
        if len == 0 {
            return Err(IpcError::invalid_argument("SharedRegion::len"));
        }
        if align == 0 || !align.is_power_of_two() {
            return Err(IpcError::invalid_argument("SharedRegion::align"));
        }
        if base.as_ptr() as usize % align != 0 {
            return Err(IpcError::invalid_argument("SharedRegion::alignment"));
        }
        Ok(Self {
            base,
            len,
            align,
            role,
        })
    }

    /// [audio-thread] Length of the mapping in bytes. Never zero.
    #[inline]
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// [audio-thread] Always `false`; a zero-length region cannot be constructed.
    ///
    /// Present so that callers and lints that expect `is_empty` beside `len` find it.
    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// [audio-thread] Alignment the base address satisfies.
    #[inline]
    #[must_use]
    pub const fn align(&self) -> usize {
        self.align
    }

    /// [audio-thread] What this process may do with the region.
    #[inline]
    #[must_use]
    pub const fn role(&self) -> RegionRole {
        self.role
    }

    /// [audio-thread] `true` when this process may write the region.
    #[inline]
    #[must_use]
    pub const fn is_writable(&self) -> bool {
        self.role.is_writable()
    }

    /// [audio-thread] The base address, for a caller doing its own pointer arithmetic.
    #[inline]
    #[must_use]
    pub const fn as_ptr(&self) -> *const u8 {
        self.base.as_ptr().cast_const()
    }

    /// [audio-thread] The base address for writing, or `None` on a read-only mapping.
    #[inline]
    #[must_use]
    pub fn as_mut_ptr(&mut self) -> Option<*mut u8> {
        self.is_writable().then_some(self.base.as_ptr())
    }

    /// [audio-thread] `true` when `[offset, offset + len)` lies inside the region.
    ///
    /// Overflow counts as "outside", so a hostile offset near `u64::MAX` answers `false`
    /// rather than wrapping into a plausible-looking range.
    #[must_use]
    pub fn contains(&self, offset: u64, len: usize) -> bool {
        usize::try_from(offset)
            .ok()
            .and_then(|start| start.checked_add(len))
            .is_some_and(|end| end <= self.len)
    }

    /// [audio-thread] Borrows `len` bytes at `offset`.
    ///
    /// # Safety
    ///
    /// The caller currently owns the region — it lies between a successful
    /// [`DataPlane::acquire`](crate::DataPlane::acquire) and the matching
    /// [`publish`](crate::DataPlane::publish) — so that no other process or thread writes
    /// these bytes while the returned slice is alive. The bounds themselves are checked
    /// here and need no guarantee from the caller.
    ///
    /// # Errors
    ///
    /// [`IpcErrorKind::InvalidArgument`](crate::IpcErrorKind::InvalidArgument) when the
    /// span does not fit inside the region.
    pub unsafe fn bytes(&self, offset: u64, len: usize) -> IpcResult<&[u8]> {
        let start = self.checked_start("SharedRegion::bytes", offset, len)?;
        // SAFETY: `checked_start` proved `start + len <= self.len`, so the whole span lies
        // inside the mapping the caller promised at construction is live, fully mapped and
        // initialised for `self.len` bytes. `u8` needs no alignment beyond one. The slice
        // borrows `self`, so it cannot outlive the descriptor, and the caller's ownership
        // of the region for the duration is exactly what `bytes`' safety contract requires,
        // which is what rules out a concurrent write by the peer.
        Ok(unsafe { core::slice::from_raw_parts(self.base.as_ptr().add(start).cast_const(), len) })
    }

    /// [audio-thread] Borrows `len` bytes at `offset` for writing.
    ///
    /// # Safety
    ///
    /// As [`SharedRegion::bytes`], and additionally no other reference to these bytes
    /// exists for as long as the returned slice is alive.
    ///
    /// # Errors
    ///
    /// [`IpcErrorKind::InvalidArgument`](crate::IpcErrorKind::InvalidArgument) when the
    /// span does not fit, or
    /// [`IpcErrorKind::InvalidState`](crate::IpcErrorKind::InvalidState) on a read-only
    /// mapping.
    pub unsafe fn bytes_mut(&mut self, offset: u64, len: usize) -> IpcResult<&mut [u8]> {
        if !self.is_writable() {
            return Err(IpcError::invalid_state("SharedRegion::bytes_mut"));
        }
        let start = self.checked_start("SharedRegion::bytes_mut", offset, len)?;
        // SAFETY: as in `bytes`, with the span bounds-checked against the live mapping and
        // `u8` imposing no alignment. The mapping is writable because `role` says so, and
        // the caller has promised that claim is honest. `&mut self` gives this call
        // exclusive access to the descriptor, and the caller's safety contract extends that
        // exclusivity to the peer process, so the returned unique slice really is unique.
        Ok(unsafe { core::slice::from_raw_parts_mut(self.base.as_ptr().add(start), len) })
    }

    /// [audio-thread] Reads and validates the [`AudioBlockHeader`] at offset zero.
    ///
    /// This is the data plane's trust boundary. The header is checked against the *real*
    /// length of this region, so on success every offset and count it declares is known to
    /// fit, which is what lets the plane accessors below index without further doubt.
    ///
    /// # Safety
    ///
    /// As [`SharedRegion::bytes`]: the caller owns the region for the duration of the call.
    ///
    /// # Errors
    ///
    /// [`IpcErrorKind::Protocol`](crate::IpcErrorKind::Protocol) when the region is too
    /// small for a header, when the magic or layout version is not this build's, or when
    /// [`AudioBlockHeader::validate`] rejects the claims it makes.
    pub unsafe fn read_header(&self, limits: &ProtocolLimits) -> IpcResult<AudioBlockHeader> {
        // SAFETY: the caller's ownership guarantee is forwarded unchanged, and `bytes`
        // itself checks that the header span fits inside the region.
        let raw = unsafe { self.bytes(0, AudioBlockHeader::SIZE) }?;
        let header = AudioBlockHeader::read_from(raw).map_err(IpcError::protocol)?;
        header
            .validate(self.len, limits)
            .map_err(IpcError::protocol)?;
        Ok(header)
    }

    /// [audio-thread] Stamps `header` at offset zero, after validating it against this
    /// region.
    ///
    /// Validating before writing means a producer cannot publish a header that its own
    /// consumer would be obliged to reject.
    ///
    /// # Safety
    ///
    /// As [`SharedRegion::bytes_mut`].
    ///
    /// # Errors
    ///
    /// [`IpcErrorKind::Protocol`](crate::IpcErrorKind::Protocol) when the header does not
    /// describe this region, or
    /// [`IpcErrorKind::InvalidState`](crate::IpcErrorKind::InvalidState) on a read-only
    /// mapping.
    pub unsafe fn write_header(
        &mut self,
        header: &AudioBlockHeader,
        limits: &ProtocolLimits,
    ) -> IpcResult<()> {
        // Writability first: on a read-only mapping the contents of the header are beside
        // the point, and reporting the header's flaws would hide the real problem.
        if !self.is_writable() {
            return Err(IpcError::invalid_state("SharedRegion::write_header"));
        }
        header
            .validate(self.len, limits)
            .map_err(IpcError::protocol)?;
        let source = header.as_bytes();
        // SAFETY: the caller's ownership and writability guarantees are forwarded
        // unchanged; `bytes_mut` bounds-checks the header span against the region.
        let destination = unsafe { self.bytes_mut(0, AudioBlockHeader::SIZE) }?;
        destination.copy_from_slice(source);
        Ok(())
    }

    /// [audio-thread] Borrows one input channel plane as `f32` samples.
    ///
    /// `header` must have come from [`SharedRegion::read_header`] on this same region;
    /// passing a header from anywhere else is safe but will simply fail the bounds check.
    ///
    /// # Safety
    ///
    /// As [`SharedRegion::bytes`].
    ///
    /// # Errors
    ///
    /// [`IpcErrorKind::InvalidArgument`](crate::IpcErrorKind::InvalidArgument) when the
    /// channel does not exist, the block is not single precision, the plane does not fit
    /// inside the region, or the plane is not `f32`-aligned.
    pub unsafe fn input_plane_f32(
        &self,
        header: &AudioBlockHeader,
        channel: u32,
    ) -> IpcResult<&[f32]> {
        let (start, frames) =
            self.plane_span(header, header.input_offset, header.input_channels, channel)?;
        // SAFETY: `plane_span` checked that `start + frames * 4` is inside the mapping and
        // that `base + start` is `f32`-aligned; the caller has promised the mapping is live
        // and initialised, and every bit pattern of four initialised bytes is a valid `f32`
        // (including NaNs and denormals, which are values, not traps). The slice borrows
        // `self`, and the caller's ownership of the region for the duration — required by
        // this function's safety contract — is what rules out a concurrent write.
        Ok(unsafe {
            core::slice::from_raw_parts(
                self.base.as_ptr().add(start).cast_const().cast::<f32>(),
                frames,
            )
        })
    }

    /// [audio-thread] Borrows one output channel plane as `f32` samples, for writing.
    ///
    /// # Safety
    ///
    /// As [`SharedRegion::bytes_mut`].
    ///
    /// # Errors
    ///
    /// As [`SharedRegion::input_plane_f32`], plus
    /// [`IpcErrorKind::InvalidState`](crate::IpcErrorKind::InvalidState) on a read-only
    /// mapping.
    pub unsafe fn output_plane_f32_mut(
        &mut self,
        header: &AudioBlockHeader,
        channel: u32,
    ) -> IpcResult<&mut [f32]> {
        if !self.is_writable() {
            return Err(IpcError::invalid_state(
                "SharedRegion::output_plane_f32_mut",
            ));
        }
        let (start, frames) = self.plane_span(
            header,
            header.output_offset,
            header.output_channels,
            channel,
        )?;
        // SAFETY: as in `input_plane_f32` for bounds, alignment and validity. The mapping
        // is writable because `role` says so and the caller promised that claim is honest.
        // `&mut self` makes the borrow exclusive within this process, and the caller's
        // ownership of the region extends that exclusivity across the process boundary.
        Ok(unsafe {
            core::slice::from_raw_parts_mut(self.base.as_ptr().add(start).cast::<f32>(), frames)
        })
    }

    /// Bounds-checks a span and returns its byte offset from the base.
    fn checked_start(&self, context: &'static str, offset: u64, len: usize) -> IpcResult<usize> {
        let start = usize::try_from(offset).map_err(|_| IpcError::invalid_argument(context))?;
        let end = start
            .checked_add(len)
            .ok_or(IpcError::invalid_argument(context))?;
        if end > self.len {
            return Err(IpcError::invalid_argument(context));
        }
        Ok(start)
    }

    /// Resolves one channel plane to a byte offset and a frame count, checking that it is
    /// inside the region and correctly aligned for `f32`.
    fn plane_span(
        &self,
        header: &AudioBlockHeader,
        plane_offset: u64,
        channels: u32,
        channel: u32,
    ) -> IpcResult<(usize, usize)> {
        const CONTEXT: &str = "SharedRegion::plane";
        if channel >= channels {
            return Err(IpcError::invalid_argument(CONTEXT));
        }
        if sample_bytes(header.sample_format) != Some(size_of::<f32>()) {
            return Err(IpcError::invalid_argument("SharedRegion::sample_format"));
        }
        let frames = header.frame_count as usize;
        let bytes = frames
            .checked_mul(size_of::<f32>())
            .ok_or(IpcError::invalid_argument(CONTEXT))?;
        let start = u64::from(channel)
            .checked_mul(header.channel_stride)
            .and_then(|skip| plane_offset.checked_add(skip))
            .ok_or(IpcError::invalid_argument(CONTEXT))?;
        let start = self.checked_start(CONTEXT, start, bytes)?;
        // The stride must actually cover a whole block, or "channel 1" would begin inside
        // channel 0 and the two slices would alias.
        if header.channel_stride < bytes as u64 {
            return Err(IpcError::invalid_argument("SharedRegion::channel_stride"));
        }
        if !self
            .base
            .as_ptr()
            .wrapping_add(start)
            .cast::<f32>()
            .is_aligned()
        {
            return Err(IpcError::invalid_argument("SharedRegion::plane_alignment"));
        }
        Ok((start, frames))
    }
}

#[cfg(test)]
mod tests {
    use super::{RegionRole, SharedRegion};
    use crate::error::IpcErrorKind;
    use daux_protocol::{AudioBlockHeader, AudioBlockLayout, ProtocolLimits, REGION_ALIGN};

    /// A heap buffer aligned like a real mapping, so the tests exercise the same
    /// arithmetic the shared-memory path will.
    struct Backing {
        bytes: Vec<u64>,
        len: usize,
    }

    impl Backing {
        fn new(len: usize) -> Self {
            // `u64` elements give 8-byte alignment, which is what `AudioBlockHeader` and
            // `f32` planes need; `REGION_ALIGN` padding leaves room to offset the base.
            let words = len.div_ceil(size_of::<u64>()) + REGION_ALIGN;
            Self {
                bytes: vec![0u64; words],
                len,
            }
        }

        fn region(&mut self, role: RegionRole) -> SharedRegion {
            let base = self.bytes.as_mut_ptr().cast::<u8>();
            // SAFETY: `base` points at `self.bytes`, which is alive for as long as this
            // `Backing` is and is at least `len` bytes long by construction; the vector is
            // fully initialised with zeros. The region is only used inside a single test
            // that never hands it to another thread, so nothing else reads or writes it.
            unsafe { SharedRegion::from_raw_parts(base, self.len, size_of::<u64>(), role) }.unwrap()
        }
    }

    fn layout() -> AudioBlockLayout {
        AudioBlockLayout::new(2, 2, 64)
            .with_events(8, 8)
            .with_blob_bytes(64)
    }

    #[test]
    fn a_descriptor_refuses_the_arguments_that_cannot_describe_a_mapping() {
        let mut storage = [0u64; 8];
        let base = storage.as_mut_ptr().cast::<u8>();
        // SAFETY: every call below is expected to fail before it can dereference anything;
        // `base` is nonetheless a real, live, 64-byte allocation owned by this frame, so
        // even the calls that succeed describe genuinely valid memory.
        unsafe {
            assert_eq!(
                SharedRegion::from_raw_parts(core::ptr::null_mut(), 8, 8, RegionRole::ReadWrite)
                    .unwrap_err()
                    .kind(),
                IpcErrorKind::InvalidArgument
            );
            assert_eq!(
                SharedRegion::from_raw_parts(base, 0, 8, RegionRole::ReadWrite)
                    .unwrap_err()
                    .kind(),
                IpcErrorKind::InvalidArgument
            );
            assert_eq!(
                SharedRegion::from_raw_parts(base, 8, 0, RegionRole::ReadWrite)
                    .unwrap_err()
                    .kind(),
                IpcErrorKind::InvalidArgument
            );
            assert_eq!(
                SharedRegion::from_raw_parts(base, 8, 3, RegionRole::ReadWrite)
                    .unwrap_err()
                    .kind(),
                IpcErrorKind::InvalidArgument,
                "an alignment that is not a power of two is nonsense"
            );
            assert_eq!(
                SharedRegion::from_raw_parts(base.add(1), 8, 8, RegionRole::ReadWrite)
                    .unwrap_err()
                    .kind(),
                IpcErrorKind::InvalidArgument,
                "a base that does not meet its own claimed alignment"
            );
            let ok = SharedRegion::from_raw_parts(base, 64, 8, RegionRole::ReadOnly).unwrap();
            assert_eq!(ok.len(), 64);
            assert!(!ok.is_empty());
            assert_eq!(ok.align(), 8);
            assert_eq!(ok.role(), RegionRole::ReadOnly);
            assert!(!ok.is_writable());
        }
    }

    #[test]
    fn a_read_only_region_refuses_every_write_path() {
        let mut backing = Backing::new(4096);
        let mut region = backing.region(RegionRole::ReadOnly);
        assert!(region.as_mut_ptr().is_none());
        // SAFETY: the region describes `backing`, which is alive and exclusively owned by
        // this test; nothing else touches it while these calls run.
        unsafe {
            assert_eq!(
                region.bytes_mut(0, 8).unwrap_err().kind(),
                IpcErrorKind::InvalidState
            );
            assert_eq!(
                region
                    .write_header(&AudioBlockHeader::new(), &ProtocolLimits::new())
                    .unwrap_err()
                    .kind(),
                IpcErrorKind::InvalidState
            );
            // Reading is still fine.
            assert_eq!(region.bytes(0, 8).unwrap(), &[0u8; 8]);
        }
    }

    #[test]
    fn every_span_is_bounds_checked_against_the_real_length() {
        let mut backing = Backing::new(256);
        let mut region = backing.region(RegionRole::ReadWrite);
        assert!(region.contains(0, 256));
        assert!(!region.contains(0, 257));
        assert!(!region.contains(1, 256));
        assert!(!region.contains(u64::MAX, 1), "overflow is outside");
        assert!(!region.contains(u64::from(u32::MAX) << 40, 1));
        // SAFETY: the region describes `backing`, alive and exclusively owned here.
        unsafe {
            assert_eq!(region.bytes(250, 6).unwrap().len(), 6);
            assert_eq!(
                region.bytes(250, 7).unwrap_err().kind(),
                IpcErrorKind::InvalidArgument
            );
            assert_eq!(
                region.bytes(0, usize::MAX).unwrap_err().kind(),
                IpcErrorKind::InvalidArgument
            );
            assert_eq!(
                region.bytes_mut(u64::MAX, 1).unwrap_err().kind(),
                IpcErrorKind::InvalidArgument
            );
        }
    }

    #[test]
    fn writes_through_a_region_are_visible_to_the_next_read() {
        let mut backing = Backing::new(256);
        let mut region = backing.region(RegionRole::ReadWrite);
        // SAFETY: the region describes `backing`, alive and exclusively owned here; the
        // mutable borrow ends before the shared one begins.
        unsafe {
            region.bytes_mut(64, 4).unwrap().copy_from_slice(b"DAUx");
            assert_eq!(region.bytes(64, 4).unwrap(), b"DAUx");
            assert_eq!(region.bytes(63, 1).unwrap(), &[0]);
        }
    }

    #[test]
    fn a_header_round_trips_through_a_region_and_is_validated_on_the_way_in() {
        let limits = ProtocolLimits::new();
        let layout = layout();
        let mut backing = Backing::new(layout.region_len(&limits).unwrap());
        let mut region = backing.region(RegionRole::ReadWrite);
        let header = layout.header(7, &limits).unwrap();
        // SAFETY: the region describes `backing`, alive and exclusively owned here.
        unsafe {
            region.write_header(&header, &limits).unwrap();
            let read_back = region.read_header(&limits).unwrap();
            assert_eq!(read_back, header);
            assert_eq!(read_back.instance, 7);
        }
    }

    /// The reason `read_header` exists at all: a header from a hostile peer must not be
    /// able to point a plane outside the mapping.
    #[test]
    fn a_header_that_does_not_fit_the_region_is_rejected() {
        let limits = ProtocolLimits::new();
        let layout = layout();
        let full = layout.region_len(&limits).unwrap();
        let header = layout.header(1, &limits).unwrap();

        // The same header over a region that is one byte too short.
        let mut backing = Backing::new(full - 1);
        let mut region = backing.region(RegionRole::ReadWrite);
        // SAFETY: the region describes `backing`, alive and exclusively owned here.
        unsafe {
            assert_eq!(
                region
                    .write_header(&header, &limits)
                    .unwrap_err()
                    .kind()
                    .as_str(),
                "invalid-layout"
            );
        }

        // And a header written by hand that claims an output plane past the end.
        let mut backing = Backing::new(full);
        let mut region = backing.region(RegionRole::ReadWrite);
        let mut hostile = header;
        hostile.output_offset = (full as u64) - 8;
        // SAFETY: the region describes `backing`, alive and exclusively owned here.
        unsafe {
            assert_eq!(
                region
                    .write_header(&hostile, &limits)
                    .unwrap_err()
                    .kind()
                    .as_str(),
                "invalid-layout"
            );
            // Written straight into the bytes, bypassing validation, it still cannot fool
            // the reader.
            region
                .bytes_mut(0, AudioBlockHeader::SIZE)
                .unwrap()
                .copy_from_slice(hostile.as_bytes());
            assert_eq!(
                region.read_header(&limits).unwrap_err().kind().as_str(),
                "invalid-layout"
            );
        }
    }

    #[test]
    fn a_truncated_region_cannot_even_hold_a_header() {
        let mut backing = Backing::new(AudioBlockHeader::SIZE - 1);
        let region = backing.region(RegionRole::ReadWrite);
        // SAFETY: the region describes `backing`, alive and exclusively owned here.
        unsafe {
            assert_eq!(
                region
                    .read_header(&ProtocolLimits::new())
                    .unwrap_err()
                    .kind(),
                IpcErrorKind::InvalidArgument
            );
        }
    }

    #[test]
    fn sample_planes_are_addressed_per_channel_and_never_alias() {
        let limits = ProtocolLimits::new();
        let layout = layout();
        let mut backing = Backing::new(layout.region_len(&limits).unwrap());
        let mut region = backing.region(RegionRole::ReadWrite);
        let header = layout.header(1, &limits).unwrap();
        // SAFETY: the region describes `backing`, alive and exclusively owned here; each
        // borrow below ends before the next begins.
        unsafe {
            region.write_header(&header, &limits).unwrap();
            for channel in 0..2u32 {
                let plane = region.output_plane_f32_mut(&header, channel).unwrap();
                assert_eq!(plane.len(), 64);
                plane.fill(channel as f32 + 1.0);
            }
            // Reading the *input* planes must not see the output planes' values: they are
            // different memory.
            for channel in 0..2u32 {
                let plane = region.input_plane_f32(&header, channel).unwrap();
                assert_eq!(plane.len(), 64);
                assert!(plane.iter().all(|s| *s == 0.0), "planes overlap");
            }
        }
    }

    #[test]
    fn a_channel_that_does_not_exist_is_refused() {
        let limits = ProtocolLimits::new();
        let layout = layout();
        let mut backing = Backing::new(layout.region_len(&limits).unwrap());
        let mut region = backing.region(RegionRole::ReadWrite);
        let header = layout.header(1, &limits).unwrap();
        // SAFETY: the region describes `backing`, alive and exclusively owned here.
        unsafe {
            assert_eq!(
                region.input_plane_f32(&header, 2).unwrap_err().kind(),
                IpcErrorKind::InvalidArgument
            );
            assert_eq!(
                region
                    .output_plane_f32_mut(&header, u32::MAX)
                    .unwrap_err()
                    .kind(),
                IpcErrorKind::InvalidArgument
            );
        }
    }

    #[test]
    fn a_double_precision_block_is_not_read_as_f32() {
        let limits = ProtocolLimits::new();
        let layout = layout().with_sample_format(1 << 1);
        let mut backing = Backing::new(layout.region_len(&limits).unwrap());
        let region = backing.region(RegionRole::ReadWrite);
        let header = layout.header(1, &limits).unwrap();
        // SAFETY: the region describes `backing`, alive and exclusively owned here.
        unsafe {
            assert_eq!(
                region.input_plane_f32(&header, 0).unwrap_err().context(),
                "SharedRegion::sample_format"
            );
        }
    }

    /// A header claiming a stride narrower than a block would make consecutive channels
    /// overlap, which would hand out two aliasing `&mut [f32]`.
    #[test]
    fn a_stride_that_would_make_channels_overlap_is_refused() {
        let limits = ProtocolLimits::new();
        let layout = layout();
        let mut backing = Backing::new(layout.region_len(&limits).unwrap());
        let mut region = backing.region(RegionRole::ReadWrite);
        let mut header = layout.header(1, &limits).unwrap();
        header.channel_stride = 4;
        // SAFETY: the region describes `backing`, alive and exclusively owned here.
        unsafe {
            assert_eq!(
                region
                    .output_plane_f32_mut(&header, 1)
                    .unwrap_err()
                    .context(),
                "SharedRegion::channel_stride"
            );
        }
    }

    #[test]
    fn a_misaligned_plane_is_refused_rather_than_read_unaligned() {
        let limits = ProtocolLimits::new();
        let layout = layout();
        let mut backing = Backing::new(layout.region_len(&limits).unwrap());
        let region = backing.region(RegionRole::ReadWrite);
        let mut header = layout.header(1, &limits).unwrap();
        header.input_offset += 1;
        // SAFETY: the region describes `backing`, alive and exclusively owned here.
        unsafe {
            assert_eq!(
                region.input_plane_f32(&header, 0).unwrap_err().context(),
                "SharedRegion::plane_alignment"
            );
        }
    }

    #[test]
    fn a_read_only_region_hands_out_input_planes_but_not_output_planes() {
        let limits = ProtocolLimits::new();
        let layout = layout();
        let mut backing = Backing::new(layout.region_len(&limits).unwrap());
        let header = layout.header(1, &limits).unwrap();
        {
            let mut writable = backing.region(RegionRole::ReadWrite);
            // SAFETY: the region describes `backing`, alive and exclusively owned here.
            unsafe { writable.write_header(&header, &limits) }.unwrap();
        }
        let mut region = backing.region(RegionRole::ReadOnly);
        // SAFETY: the region describes `backing`, alive and exclusively owned here.
        unsafe {
            assert_eq!(region.input_plane_f32(&header, 0).unwrap().len(), 64);
            assert_eq!(
                region.output_plane_f32_mut(&header, 0).unwrap_err().kind(),
                IpcErrorKind::InvalidState
            );
        }
    }

    /// The contract requires that anything crossing to the audio thread is `Send`.
    #[test]
    fn a_region_descriptor_can_be_moved_to_the_audio_thread() {
        const fn assert_send<T: Send>() {}
        assert_send::<SharedRegion>();

        let mut backing = Backing::new(256);
        let region = backing.region(RegionRole::ReadWrite);
        let region = std::thread::spawn(move || {
            let mut region = region;
            // SAFETY: the region describes `backing`, which outlives the join below, and
            // this worker is the only thread touching it while it runs.
            unsafe { region.bytes_mut(0, 4) }
                .unwrap()
                .copy_from_slice(b"sent");
            region
        })
        .join()
        .unwrap();
        // SAFETY: the worker has joined, so this thread is again the only one with access.
        assert_eq!(unsafe { region.bytes(0, 4) }.unwrap(), b"sent");
    }
}
