//! Cache-line padding used by the lock-free containers.

/// A value padded and aligned to a full cache line.
///
/// Two atomics that live in the same cache line cause the cores that own them to
/// fight over that line even when the atomics are logically independent. Every
/// producer/consumer index pair in this crate is wrapped in `CachePadded` so the
/// producer and the consumer never write to the same line.
///
/// x86-64 and AArch64 prefetch cache lines in pairs, so the padding is 128 bytes
/// there and 64 bytes elsewhere.
#[cfg_attr(any(target_arch = "x86_64", target_arch = "aarch64"), repr(align(128)))]
#[cfg_attr(
    not(any(target_arch = "x86_64", target_arch = "aarch64")),
    repr(align(64))
)]
#[derive(Debug, Default)]
pub(crate) struct CachePadded<T>(pub(crate) T);

impl<T> CachePadded<T> {
    /// Wraps `value` on its own cache line.
    pub(crate) const fn new(value: T) -> Self {
        Self(value)
    }
}

#[cfg(test)]
mod tests {
    use super::CachePadded;
    use core::sync::atomic::AtomicUsize;

    #[test]
    fn padded_atomics_do_not_share_a_line() {
        let pair = [
            CachePadded::new(AtomicUsize::new(0)),
            CachePadded::new(AtomicUsize::new(0)),
        ];
        let a = core::ptr::addr_of!(pair[0].0) as usize;
        let b = core::ptr::addr_of!(pair[1].0) as usize;
        assert!(
            b - a >= 64,
            "padding is smaller than a cache line: {}",
            b - a
        );
        assert_eq!(a % align_of::<CachePadded<AtomicUsize>>(), 0);
    }
}
