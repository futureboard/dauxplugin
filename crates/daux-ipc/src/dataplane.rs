//! The data plane: shared audio regions and the handshake that hands them over.
//!
//! # Why ownership is explicit
//!
//! Two processes sharing one buffer need to agree, every block, on who may touch it. The
//! alternative — locking — is not available: the audio thread must not block, and the peer
//! holding the lock may be a process that has just died. So the region is handed back and
//! forth instead. Exactly one side owns it at a time, ownership moves on a release/acquire
//! pair, and a side that does not own it is told [`WouldBlock`] rather than being made to
//! wait.
//!
//! ```text
//!    host                                     sandbox
//!    ----                                     -------
//!    acquire(0)      -> Ok(seq)               acquire(0) -> Err(WouldBlock)
//!    write input planes, stamp the header
//!    publish(0, seq)  ─── ownership ──────>   acquire(0) -> Ok(seq)
//!                                             read input, write output, set status
//!    acquire(0) <──── ownership ───────────   publish(0, seq)
//!    read output planes
//! ```
//!
//! That is also the crash story: if the sandbox dies holding the region, the host's
//! `acquire` keeps reporting [`WouldBlock`], the audio thread outputs silence for that
//! instance, and nothing blocks on a process that no longer exists.
//!
//! # Real-time safety
//!
//! [`DataPlane::acquire`] and [`DataPlane::publish`] are two atomic operations each. They
//! allocate nothing, lock nothing and loop over nothing, so they are safe to call from
//! `process`. Everything expensive — mapping, laying the region out, stamping the initial
//! header — happens in the constructor, on the main thread.
//!
//! [`WouldBlock`]: crate::IpcErrorKind::WouldBlock

use core::fmt;
use std::alloc::Layout;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use daux_protocol::{AudioBlockLayout, ProtocolLimits, REGION_ALIGN};

use crate::error::{IpcError, IpcResult};
use crate::region::{RegionRole, SharedRegion};

/// Which side of a sandboxed connection an endpoint is. [any-thread]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DataPlaneEndpoint {
    /// The DAW-side process, which owns every region to begin with.
    Host,
    /// The isolated process that loads the plug-in binary.
    Sandbox,
}

impl DataPlaneEndpoint {
    /// [audio-thread] The other side.
    #[inline]
    #[must_use]
    pub const fn peer(self) -> Self {
        match self {
            Self::Host => Self::Sandbox,
            Self::Sandbox => Self::Host,
        }
    }

    /// [audio-thread] The value stored in a region's owner word.
    #[inline]
    #[must_use]
    const fn as_u32(self) -> u32 {
        match self {
            Self::Host => 0,
            Self::Sandbox => 1,
        }
    }
}

/// Access to the shared audio regions of one connection. [audio-thread]
///
/// One region carries one block: its [`AudioBlockHeader`](daux_protocol::AudioBlockHeader),
/// its sample planes, its event arrays and its blob pool. A connection has one region per
/// audio bus pair, in bus order.
///
/// Every method here is allocation-free and non-blocking, because exactly one side owns a
/// region at a time and ownership is handed over rather than locked: [`DataPlane::acquire`]
/// a region, use it, [`DataPlane::publish`] it, and never touch it in between. A side that
/// does not own a region is told [`WouldBlock`](crate::IpcErrorKind::WouldBlock) instead of
/// being made to wait, which is what keeps a dead peer from stalling the audio thread.
pub trait DataPlane {
    /// [audio-thread] The regions this endpoint has mapped, in bus order.
    fn audio_regions(&self) -> &[SharedRegion];

    /// [audio-thread] The regions this endpoint has mapped, mutably.
    fn audio_regions_mut(&mut self) -> &mut [SharedRegion];

    /// [audio-thread] `true` when this endpoint currently owns region `index`.
    ///
    /// `false` for an index that does not exist, so a caller never has to bounds-check
    /// twice.
    fn owns(&self, index: usize) -> bool;

    /// [audio-thread] Takes ownership of region `index`, returning the sequence number the
    /// peer published with.
    ///
    /// Never waits. A region the peer still owns — because it is working, or because it has
    /// died — reports [`WouldBlock`](crate::IpcErrorKind::WouldBlock), and the caller
    /// decides what to do with the block it was going to render.
    ///
    /// # Errors
    ///
    /// [`IpcErrorKind::InvalidArgument`](crate::IpcErrorKind::InvalidArgument) for an index
    /// that does not exist, and
    /// [`IpcErrorKind::WouldBlock`](crate::IpcErrorKind::WouldBlock) when the peer still
    /// owns the region.
    fn acquire(&mut self, index: usize) -> IpcResult<u64>;

    /// [audio-thread] Hands region `index` to the peer, tagged with `sequence`.
    ///
    /// Every write this endpoint made to the region is visible to the peer's matching
    /// [`DataPlane::acquire`], and this endpoint must not touch the region again until it
    /// acquires it back.
    ///
    /// # Errors
    ///
    /// [`IpcErrorKind::InvalidArgument`](crate::IpcErrorKind::InvalidArgument) for an index
    /// that does not exist, and
    /// [`IpcErrorKind::InvalidState`](crate::IpcErrorKind::InvalidState) when this endpoint
    /// does not own the region.
    fn publish(&mut self, index: usize, sequence: u64) -> IpcResult<()>;
}

/// One region's backing memory and its ownership word.
///
/// Shared by both endpoints through an `Arc`, so the memory outlives whichever of them is
/// dropped first.
struct RegionSlot {
    base: NonNull<u8>,
    layout: Layout,
    len: usize,
    /// [`DataPlaneEndpoint::as_u32`] of whichever side may touch the region.
    owner: AtomicU32,
    /// Block counter the current owner was handed.
    sequence: AtomicU64,
}

// SAFETY: `RegionSlot` owns its allocation outright — `base` came from `alloc_zeroed` in
// the constructor and is freed exactly once in `Drop` — so moving it between threads
// transfers a unique owner, never a shared borrow. Sharing it (`Sync`) is sound because
// the only fields reachable through `&self` are two atomics, and the allocation itself is
// never touched through this type: it is reached solely through `SharedRegion`, whose
// accessors are `unsafe` and require the caller to hold the region under the ownership
// handshake these very atomics implement. The `Release` store in `publish` and the
// `Acquire` load in `acquire` order the region's contents around that transfer, so the two
// sides' accesses are ordered rather than concurrent.
unsafe impl Send for RegionSlot {}
// SAFETY: as above.
unsafe impl Sync for RegionSlot {}

impl Drop for RegionSlot {
    fn drop(&mut self) {
        // SAFETY: `base` is the non-null pointer `std::alloc::alloc_zeroed` returned for
        // exactly `self.layout` in `LoopbackDataPlane::pair_with_layouts`, it has not been
        // freed before (this is the sole `dealloc`, in the sole `Drop`, of a type that is
        // never cloned), and `Drop` runs once when the last `Arc` holding the slot goes
        // away — so no `SharedRegion` derived from it can still be alive.
        unsafe { std::alloc::dealloc(self.base.as_ptr(), self.layout) };
    }
}

/// Every region of one connection, shared by both endpoints.
struct SharedRegions {
    slots: Vec<RegionSlot>,
}

/// An in-process [`DataPlane`]: real regions, real handshake, no operating system.
/// [audio-thread]
///
/// The regions are heap allocations aligned to [`REGION_ALIGN`] rather than shared-memory
/// mappings, and that is the *only* difference from the sandboxed path. The layout, the
/// header validation, the ownership protocol and the memory ordering are the same code, so
/// a plug-in host driven through this type is exercising the transport that ships, not a
/// stand-in for it.
///
/// Created in connected pairs by [`LoopbackDataPlane::pair`]. The host endpoint owns every
/// region to begin with.
pub struct LoopbackDataPlane {
    shared: Arc<SharedRegions>,
    endpoint: DataPlaneEndpoint,
    regions: Vec<SharedRegion>,
}

impl LoopbackDataPlane {
    /// [main-thread] Creates a connected pair with one region of the given layout.
    ///
    /// # Errors
    ///
    /// As [`LoopbackDataPlane::pair_with_layouts`].
    pub fn pair(
        layout: &AudioBlockLayout,
        instance: u64,
        limits: &ProtocolLimits,
    ) -> IpcResult<(Self, Self)> {
        Self::pair_with_layouts(core::slice::from_ref(layout), instance, limits)
    }

    /// [main-thread] Creates a connected pair with one region per layout, in bus order.
    ///
    /// Allocates and zero-fills every region and stamps its initial header, so a consumer
    /// that reads a region before the producer has written anything sees a valid, silent
    /// block rather than uninitialised memory.
    ///
    /// # Errors
    ///
    /// [`IpcErrorKind::InvalidArgument`](crate::IpcErrorKind::InvalidArgument) when no
    /// layout is given or a layout's size cannot be expressed as an allocation,
    /// [`IpcErrorKind::OutOfMemory`](crate::IpcErrorKind::OutOfMemory) when a region cannot
    /// be allocated, and
    /// [`IpcErrorKind::Protocol`](crate::IpcErrorKind::Protocol) when a layout is outside
    /// `limits` or does not describe a usable region.
    pub fn pair_with_layouts(
        layouts: &[AudioBlockLayout],
        instance: u64,
        limits: &ProtocolLimits,
    ) -> IpcResult<(Self, Self)> {
        if layouts.is_empty() {
            return Err(IpcError::invalid_argument("LoopbackDataPlane::layouts"));
        }
        let mut slots = Vec::with_capacity(layouts.len());
        for block in layouts {
            let len = block.region_len(limits).map_err(IpcError::protocol)?;
            let header = block.header(instance, limits).map_err(IpcError::protocol)?;
            let layout = Layout::from_size_align(len, REGION_ALIGN)
                .map_err(|_| IpcError::invalid_argument("LoopbackDataPlane::region_layout"))?;
            // SAFETY: `region_len` always includes the block header, so `len` — and hence
            // `layout.size()` — is non-zero, which is what `alloc_zeroed` requires of its
            // caller. A null return is handled below rather than dereferenced.
            let raw = unsafe { std::alloc::alloc_zeroed(layout) };
            let base =
                NonNull::new(raw).ok_or(IpcError::out_of_memory("LoopbackDataPlane::region"))?;
            // Pushed before anything else can fail, so that an error below still frees it
            // through `RegionSlot::drop`.
            slots.push(RegionSlot {
                base,
                layout,
                len,
                owner: AtomicU32::new(DataPlaneEndpoint::Host.as_u32()),
                sequence: AtomicU64::new(0),
            });
            let Some(slot) = slots.last() else {
                return Err(IpcError::invalid_state("LoopbackDataPlane::slots"));
            };
            let mut region = Self::describe(slot)?;
            // SAFETY: `region` describes the allocation made two statements above: it is
            // live, `len` bytes long, zero-initialised by `alloc_zeroed` and writable, and
            // no other descriptor for it exists yet, so this thread has exclusive access.
            unsafe { region.write_header(&header, limits) }?;
        }

        let shared = Arc::new(SharedRegions { slots });
        Ok((
            Self::for_endpoint(&shared, DataPlaneEndpoint::Host)?,
            Self::for_endpoint(&shared, DataPlaneEndpoint::Sandbox)?,
        ))
    }

    /// [any-thread] Which side of the connection this endpoint is.
    #[inline]
    #[must_use]
    pub const fn endpoint_role(&self) -> DataPlaneEndpoint {
        self.endpoint
    }

    /// [audio-thread] Number of regions, one per audio bus pair.
    #[inline]
    #[must_use]
    pub fn region_count(&self) -> usize {
        self.regions.len()
    }

    /// [audio-thread] The block counter region `index` currently carries, whoever owns it.
    ///
    /// Diagnostics only: acting on a sequence number for a region this endpoint does not
    /// own is a race by construction.
    #[must_use]
    pub fn sequence(&self, index: usize) -> Option<u64> {
        self.shared
            .slots
            .get(index)
            .map(|slot| slot.sequence.load(Ordering::Relaxed))
    }

    /// Builds the descriptor an endpoint uses to reach one slot's memory.
    fn describe(slot: &RegionSlot) -> IpcResult<SharedRegion> {
        // SAFETY: `slot.base` is the live `alloc_zeroed` allocation of `slot.len` bytes,
        // aligned to `REGION_ALIGN` because that is the alignment it was allocated with,
        // and fully initialised because `alloc_zeroed` zeroed it. It stays mapped at the
        // same address until the last `Arc<SharedRegions>` drops, which outlives every
        // endpoint holding a descriptor, and it is genuinely writable. Exclusive access is
        // the caller's business under the ownership handshake, which is exactly what the
        // safety contract of `SharedRegion`'s accessors demands.
        unsafe {
            SharedRegion::from_raw_parts(
                slot.base.as_ptr(),
                slot.len,
                REGION_ALIGN,
                RegionRole::ReadWrite,
            )
        }
    }

    fn for_endpoint(shared: &Arc<SharedRegions>, endpoint: DataPlaneEndpoint) -> IpcResult<Self> {
        let mut regions = Vec::with_capacity(shared.slots.len());
        for slot in &shared.slots {
            regions.push(Self::describe(slot)?);
        }
        Ok(Self {
            shared: Arc::clone(shared),
            endpoint,
            regions,
        })
    }

    fn slot(&self, index: usize) -> IpcResult<&RegionSlot> {
        self.shared
            .slots
            .get(index)
            .ok_or(IpcError::invalid_argument("LoopbackDataPlane::index"))
    }
}

impl fmt::Debug for LoopbackDataPlane {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoopbackDataPlane")
            .field("endpoint", &self.endpoint)
            .field("regions", &self.regions.len())
            .field(
                "owned",
                &(0..self.regions.len()).filter(|i| self.owns(*i)).count(),
            )
            .finish()
    }
}

impl DataPlane for LoopbackDataPlane {
    #[inline]
    fn audio_regions(&self) -> &[SharedRegion] {
        &self.regions
    }

    #[inline]
    fn audio_regions_mut(&mut self) -> &mut [SharedRegion] {
        &mut self.regions
    }

    fn owns(&self, index: usize) -> bool {
        self.shared
            .slots
            .get(index)
            .is_some_and(|slot| slot.owner.load(Ordering::Acquire) == self.endpoint.as_u32())
    }

    fn acquire(&mut self, index: usize) -> IpcResult<u64> {
        let slot = self.slot(index)?;
        // Acquire: the peer's `Release` store of `owner` in `publish` happens after every
        // write it made to the region, so seeing our own id here means those writes are
        // visible to us.
        if slot.owner.load(Ordering::Acquire) != self.endpoint.as_u32() {
            return Err(IpcError::would_block("LoopbackDataPlane::acquire"));
        }
        Ok(slot.sequence.load(Ordering::Relaxed))
    }

    fn publish(&mut self, index: usize, sequence: u64) -> IpcResult<()> {
        let slot = self.slot(index)?;
        if slot.owner.load(Ordering::Acquire) != self.endpoint.as_u32() {
            return Err(IpcError::invalid_state("LoopbackDataPlane::publish"));
        }
        // Relaxed is enough for the counter: it is only read after the `Acquire` load of
        // `owner` below has already synchronised with this thread.
        slot.sequence.store(sequence, Ordering::Relaxed);
        // Release: everything written to the region before this store is visible to the
        // peer's `Acquire` load. This is the line that transfers ownership.
        slot.owner
            .store(self.endpoint.peer().as_u32(), Ordering::Release);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{DataPlane, DataPlaneEndpoint, LoopbackDataPlane};
    use crate::error::IpcErrorKind;
    use daux_protocol::{AudioBlockLayout, ProtocolLimits};

    fn layout() -> AudioBlockLayout {
        AudioBlockLayout::new(2, 2, 64)
            .with_events(8, 8)
            .with_blob_bytes(64)
    }

    fn pair() -> (LoopbackDataPlane, LoopbackDataPlane) {
        LoopbackDataPlane::pair(&layout(), 1, &ProtocolLimits::new()).unwrap()
    }

    #[test]
    fn the_host_starts_owning_every_region_and_the_sandbox_owns_none() {
        let (host, sandbox) = pair();
        assert_eq!(host.endpoint_role(), DataPlaneEndpoint::Host);
        assert_eq!(sandbox.endpoint_role(), DataPlaneEndpoint::Sandbox);
        assert_eq!(host.region_count(), 1);
        assert!(host.owns(0));
        assert!(!sandbox.owns(0));
        assert!(!host.owns(1), "an index that does not exist is not owned");
        assert_eq!(host.sequence(0), Some(0));
        assert_eq!(host.sequence(1), None);
    }

    #[test]
    fn the_initial_header_is_already_valid_so_an_early_reader_sees_silence() {
        let (host, _sandbox) = pair();
        let limits = ProtocolLimits::new();
        let region = &host.audio_regions()[0];
        // SAFETY: the host owns region 0 from the start and nothing has published it away,
        // so this thread has exclusive access to it.
        let header = unsafe { region.read_header(&limits) }.unwrap();
        assert_eq!(header.instance, 1);
        assert_eq!(header.frame_count, 64);
        assert_eq!(header.input_channels, 2);
        // SAFETY: as above.
        let plane = unsafe { region.input_plane_f32(&header, 0) }.unwrap();
        assert!(plane.iter().all(|s| *s == 0.0), "the region is zero-filled");
    }

    /// A full block, the way the sandbox will really run one.
    #[test]
    fn a_block_of_audio_crosses_the_plane_and_comes_back_processed() {
        let (mut host, mut sandbox) = pair();
        let limits = ProtocolLimits::new();

        // ---- host: fill the input, shorten the block, hand it over -------------------
        assert_eq!(host.acquire(0).unwrap(), 0);
        let region = &mut host.audio_regions_mut()[0];
        // SAFETY: the host acquired region 0 above and has not published it, so it holds
        // exclusive access for the whole of this section.
        let mut header = unsafe { region.read_header(&limits) }.unwrap();
        header.frame_count = 32;
        header.sequence = 9;
        // SAFETY: as above.
        unsafe { region.write_header(&header, &limits) }.unwrap();
        for channel in 0..2u32 {
            // SAFETY: as above; each plane borrow ends before the next begins.
            let plane = unsafe { region.output_plane_f32_mut(&header, channel) }.unwrap();
            assert_eq!(plane.len(), 32, "the shortened block is what is exposed");
        }
        // The host writes the *input* planes; `input_plane_f32` only reads, so reach them
        // through the raw byte view.
        let input_offset = header.input_offset;
        let stride = header.channel_stride;
        for channel in 0..2u64 {
            // SAFETY: as above. The span is bounds-checked by `bytes_mut`.
            let bytes =
                unsafe { region.bytes_mut(input_offset + channel * stride, 32 * 4) }.unwrap();
            for (frame, chunk) in bytes.chunks_exact_mut(4).enumerate() {
                chunk.copy_from_slice(&(frame as f32 + channel as f32).to_le_bytes());
            }
        }
        host.publish(0, 9).unwrap();
        assert!(!host.owns(0));

        // ---- sandbox: read the input, double it into the output ----------------------
        assert_eq!(sandbox.acquire(0).unwrap(), 9);
        let region = &mut sandbox.audio_regions_mut()[0];
        // SAFETY: the sandbox acquired region 0, so ownership — and with it exclusive
        // access — has been transferred to this thread.
        let header = unsafe { region.read_header(&limits) }.unwrap();
        assert_eq!(header.frame_count, 32);
        assert_eq!(header.sequence, 9);
        for channel in 0..2u32 {
            // SAFETY: as above. The input borrow is copied out before the output borrow is
            // taken, so the two never overlap.
            let input: Vec<f32> = unsafe { region.input_plane_f32(&header, channel) }
                .unwrap()
                .to_vec();
            assert_eq!(input[3], 3.0 + channel as f32);
            // SAFETY: as above.
            let output = unsafe { region.output_plane_f32_mut(&header, channel) }.unwrap();
            for (out, inp) in output.iter_mut().zip(&input) {
                *out = inp * 2.0;
            }
        }
        sandbox.publish(0, 9).unwrap();

        // ---- host: the processed block is back ---------------------------------------
        assert_eq!(host.acquire(0).unwrap(), 9);
        let region = &mut host.audio_regions_mut()[0];
        // SAFETY: the host has acquired region 0 back, so ownership is here again.
        let header = unsafe { region.read_header(&limits) }.unwrap();
        for channel in 0..2u32 {
            // SAFETY: as above.
            let output = unsafe { region.output_plane_f32_mut(&header, channel) }.unwrap();
            assert_eq!(output[0], 2.0 * channel as f32);
            assert_eq!(output[3], 2.0 * (3.0 + channel as f32));
            assert_eq!(output.len(), 32);
        }
    }

    #[test]
    fn a_side_that_does_not_own_a_region_is_told_to_come_back_later() {
        let (mut host, mut sandbox) = pair();
        assert_eq!(
            sandbox.acquire(0).unwrap_err().kind(),
            IpcErrorKind::WouldBlock
        );
        assert_eq!(
            sandbox.publish(0, 1).unwrap_err().kind(),
            IpcErrorKind::InvalidState,
            "publishing a region you do not own is a bug, not backpressure"
        );
        host.publish(0, 1).unwrap();
        assert_eq!(
            host.acquire(0).unwrap_err().kind(),
            IpcErrorKind::WouldBlock
        );
        assert_eq!(
            host.publish(0, 2).unwrap_err().kind(),
            IpcErrorKind::InvalidState,
            "publishing twice must not steal the region back"
        );
        assert_eq!(sandbox.acquire(0).unwrap(), 1);
    }

    /// The crash case: a sandbox that dies holding the region must not stall the host.
    #[test]
    fn a_peer_that_never_gives_the_region_back_does_not_block_the_audio_thread() {
        let (mut host, sandbox) = pair();
        host.publish(0, 1).unwrap();
        drop(sandbox);
        for _ in 0..8 {
            assert_eq!(
                host.acquire(0).unwrap_err().kind(),
                IpcErrorKind::WouldBlock,
                "the host polls and moves on; it never waits"
            );
        }
        // The memory is still alive and readable through the descriptor the host holds.
        assert_eq!(host.region_count(), 1);
        assert_eq!(host.sequence(0), Some(1));
    }

    #[test]
    fn an_index_that_does_not_exist_is_refused_on_every_path() {
        let (mut host, _sandbox) = pair();
        assert_eq!(
            host.acquire(3).unwrap_err().kind(),
            IpcErrorKind::InvalidArgument
        );
        assert_eq!(
            host.publish(3, 1).unwrap_err().kind(),
            IpcErrorKind::InvalidArgument
        );
        assert!(!host.owns(3));
        assert_eq!(host.audio_regions().len(), 1);
    }

    #[test]
    fn several_buses_get_several_independent_regions() {
        let limits = ProtocolLimits::new();
        let layouts = [
            AudioBlockLayout::new(2, 2, 64),
            AudioBlockLayout::new(1, 0, 64),
        ];
        let (mut host, mut sandbox) =
            LoopbackDataPlane::pair_with_layouts(&layouts, 5, &limits).unwrap();
        assert_eq!(host.region_count(), 2);
        assert_ne!(
            host.audio_regions()[0].as_ptr(),
            host.audio_regions()[1].as_ptr(),
            "each bus gets its own memory"
        );
        assert_ne!(
            host.audio_regions()[0].len(),
            host.audio_regions()[1].len(),
            "and its own size"
        );

        // Handing one over leaves the other where it was.
        host.publish(0, 1).unwrap();
        assert!(!host.owns(0));
        assert!(host.owns(1));
        assert_eq!(sandbox.acquire(0).unwrap(), 1);
        assert_eq!(
            sandbox.acquire(1).unwrap_err().kind(),
            IpcErrorKind::WouldBlock
        );
    }

    #[test]
    fn a_plane_with_no_layouts_is_refused() {
        assert_eq!(
            LoopbackDataPlane::pair_with_layouts(&[], 1, &ProtocolLimits::new())
                .unwrap_err()
                .kind(),
            IpcErrorKind::InvalidArgument
        );
    }

    #[test]
    fn a_layout_outside_the_limits_is_refused_before_anything_is_allocated() {
        let limits = ProtocolLimits::new().with_audio_bounds(2, 64, 8);
        // 8 channels against a 2-channel bound.
        let err =
            LoopbackDataPlane::pair(&AudioBlockLayout::new(8, 8, 64), 1, &limits).unwrap_err();
        assert_eq!(err.kind().as_str(), "limit-exceeded");
        // And a block longer than the bound.
        let err =
            LoopbackDataPlane::pair(&AudioBlockLayout::new(2, 2, 4096), 1, &limits).unwrap_err();
        assert_eq!(err.kind().as_str(), "limit-exceeded");
    }

    #[test]
    fn the_regions_outlive_whichever_endpoint_is_dropped_first() {
        let (host, sandbox) = pair();
        let limits = ProtocolLimits::new();
        drop(host);
        // The sandbox's descriptor still points at live, valid memory.
        let region = &sandbox.audio_regions()[0];
        // SAFETY: the host endpoint is gone, so this thread is the only one with a
        // descriptor for the region, and the `Arc` inside `sandbox` keeps the allocation
        // alive.
        let header = unsafe { region.read_header(&limits) }.unwrap();
        assert_eq!(header.instance, 1);
    }

    /// The audio thread runs this every block. It must not allocate.
    #[test]
    fn the_handshake_allocates_nothing() {
        let (mut host, mut sandbox) = pair();
        let (result, allocations) = daux_rt::AllocGuard::scope(|| {
            let mut checksum = 0u64;
            for block in 1..=64u64 {
                if host.acquire(0).is_err() {
                    return None;
                }
                if host.publish(0, block).is_err() {
                    return None;
                }
                checksum = checksum.wrapping_add(sandbox.acquire(0).ok()?);
                sandbox.publish(0, block).ok()?;
                let _ = host.audio_regions().len();
                let _ = sandbox.owns(0);
            }
            Some(checksum)
        });
        assert_eq!(result, Some((1..=64u64).sum()));
        if daux_rt::counting_allocator_installed() {
            assert_eq!(allocations, 0, "the data-plane handshake allocated");
        }
    }

    /// The regions really are shared: two threads, one buffer, ownership passing between
    /// them.
    #[test]
    fn a_region_can_be_handed_to_another_thread_and_back() {
        let (mut host, mut sandbox) = pair();
        let limits = ProtocolLimits::new();
        let header = {
            let region = &host.audio_regions()[0];
            // SAFETY: the host owns region 0 and has not published it.
            unsafe { region.read_header(&limits) }.unwrap()
        };
        {
            let region = &mut host.audio_regions_mut()[0];
            // SAFETY: as above.
            let plane = unsafe { region.output_plane_f32_mut(&header, 0) }.unwrap();
            plane.fill(0.25);
        }
        host.publish(0, 1).unwrap();

        let worker = std::thread::spawn(move || {
            while sandbox.acquire(0).is_err() {
                std::thread::yield_now();
            }
            let region = &mut sandbox.audio_regions_mut()[0];
            // SAFETY: `acquire` succeeded, so ownership — and therefore exclusive access —
            // was transferred to this thread by the host's `Release` store.
            let plane = unsafe { region.output_plane_f32_mut(&header, 0) }.unwrap();
            let sum: f32 = plane.iter().sum();
            plane.fill(-1.0);
            sandbox.publish(0, 2).unwrap();
            sum
        });
        assert_eq!(worker.join().unwrap(), 0.25 * 64.0);

        assert_eq!(host.acquire(0).unwrap(), 2);
        let region = &host.audio_regions()[0];
        // SAFETY: the worker has joined and the host has acquired the region back.
        let plane = unsafe { region.input_plane_f32(&header, 0) }.unwrap();
        assert!(plane.iter().all(|s| *s == 0.0), "inputs were untouched");
        let region = &mut host.audio_regions_mut()[0];
        // SAFETY: as above.
        let plane = unsafe { region.output_plane_f32_mut(&header, 0) }.unwrap();
        assert!(plane.iter().all(|s| *s == -1.0));
    }
}
