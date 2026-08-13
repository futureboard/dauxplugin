//! Operating-system shared memory for the data plane. **Declared, not implemented in v1.**
//!
//! The audio path needs one buffer that both processes can see, mapped once and reused
//! every block: `CreateFileMappingW`/`MapViewOfFile` on Windows, `shm_open`/`mmap` on
//! Unix, or an anonymous mapping whose descriptor travels over `SCM_RIGHTS`. Whichever it
//! is, the result is a base pointer and a length — exactly what a [`SharedRegion`]
//! describes — so the rest of the crate needs no changes when this lands.
//!
//! Until then, both constructors report
//! [`IpcErrorKind::Unsupported`](crate::IpcErrorKind::Unsupported), and the in-process
//! [`LoopbackDataPlane`](crate::LoopbackDataPlane) provides the regions.

use crate::error::{IpcError, IpcResult};
use crate::region::SharedRegion;

/// A shared-memory object mapped into this process. [main-thread]
///
/// Cannot be constructed on this build. When it can, dropping it must unmap the view and
/// close the handle, and it must therefore outlive every [`SharedRegion`] derived from it —
/// which is why it owns the region rather than handing out copies of it.
#[derive(Debug)]
pub struct SharedMemoryMap {
    region: SharedRegion,
}

impl SharedMemoryMap {
    /// Longest object name accepted, for both `CreateFileMappingW` and `shm_open`.
    pub const MAX_NAME_LEN: usize = 128;

    /// Largest mapping this crate will create: 256 MiB, far past any plausible block, and
    /// small enough that a corrupt length cannot exhaust the address space.
    pub const MAX_LEN: usize = 256 * 1024 * 1024;

    /// [main-thread] Creates a new shared-memory object of `len` bytes and maps it.
    ///
    /// # Errors
    ///
    /// [`IpcErrorKind::InvalidArgument`](crate::IpcErrorKind::InvalidArgument) for an empty
    /// or over-long name or a zero length,
    /// [`IpcErrorKind::LimitExceeded`](crate::IpcErrorKind::LimitExceeded) for a length
    /// past [`SharedMemoryMap::MAX_LEN`], and
    /// [`IpcErrorKind::Unsupported`](crate::IpcErrorKind::Unsupported) always, on this
    /// build.
    pub fn create(name: &str, len: usize) -> IpcResult<Self> {
        check(name, len)?;
        Err(IpcError::unsupported("SharedMemoryMap::create"))
    }

    /// [main-thread] Maps an existing shared-memory object created by the peer.
    ///
    /// # Errors
    ///
    /// As [`SharedMemoryMap::create`].
    pub fn open(name: &str, len: usize) -> IpcResult<Self> {
        check(name, len)?;
        Err(IpcError::unsupported("SharedMemoryMap::open"))
    }

    /// [audio-thread] The mapped region.
    #[inline]
    #[must_use]
    pub const fn region(&self) -> &SharedRegion {
        &self.region
    }

    /// [audio-thread] The mapped region, mutably.
    #[inline]
    pub fn region_mut(&mut self) -> &mut SharedRegion {
        &mut self.region
    }
}

/// Rejects a name or length no mapping call could succeed with.
fn check(name: &str, len: usize) -> IpcResult<()> {
    if name.is_empty() || name.len() > SharedMemoryMap::MAX_NAME_LEN || len == 0 {
        return Err(IpcError::invalid_argument("SharedMemoryMap::name"));
    }
    if len > SharedMemoryMap::MAX_LEN {
        return Err(IpcError::limit(
            "SharedMemoryMap::len",
            SharedMemoryMap::MAX_LEN,
            len,
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::SharedMemoryMap;
    use crate::error::IpcErrorKind;
    use crate::region::{RegionRole, SharedRegion};

    #[test]
    fn both_constructors_refuse_cleanly_rather_than_panicking() {
        assert_eq!(
            SharedMemoryMap::create("daux-block-0", 4096)
                .err()
                .map(|e| e.kind()),
            Some(IpcErrorKind::Unsupported)
        );
        assert_eq!(
            SharedMemoryMap::open("daux-block-0", 4096)
                .err()
                .map(|e| e.kind()),
            Some(IpcErrorKind::Unsupported)
        );
    }

    #[test]
    fn an_impossible_name_or_length_is_rejected_before_the_unsupported_verdict() {
        let long_name = "x".repeat(SharedMemoryMap::MAX_NAME_LEN + 1);
        for (name, len) in [("", 4096), (long_name.as_str(), 4096), ("ok", 0)] {
            assert_eq!(
                SharedMemoryMap::create(name, len).err().map(|e| e.kind()),
                Some(IpcErrorKind::InvalidArgument)
            );
        }
    }

    /// A corrupt length from the peer must be capped before it becomes an address-space
    /// reservation.
    #[test]
    fn an_absurd_length_is_capped_rather_than_attempted() {
        let err = SharedMemoryMap::open("daux-block-0", usize::MAX).unwrap_err();
        assert_eq!(
            err.kind(),
            IpcErrorKind::LimitExceeded {
                limit: SharedMemoryMap::MAX_LEN,
                requested: usize::MAX,
            }
        );
        // Exactly at the cap is a size, not an error.
        assert_eq!(
            SharedMemoryMap::open("daux-block-0", SharedMemoryMap::MAX_LEN)
                .err()
                .map(|e| e.kind()),
            Some(IpcErrorKind::Unsupported)
        );
    }

    #[test]
    fn a_mapping_hands_out_the_region_it_owns() {
        let mut storage = [0u64; 64];
        let base = storage.as_mut_ptr().cast::<u8>();
        // SAFETY: `base` points at `storage`, a live, fully initialised, exclusively owned
        // 512-byte array on this frame that outlives `map`; nothing else reads or writes it
        // while the region exists, and it is genuinely writable.
        let region =
            unsafe { SharedRegion::from_raw_parts(base, 512, 8, RegionRole::ReadWrite) }.unwrap();
        let mut map = SharedMemoryMap { region };
        assert_eq!(map.region().len(), 512);
        assert!(map.region_mut().as_mut_ptr().is_some());
    }
}
