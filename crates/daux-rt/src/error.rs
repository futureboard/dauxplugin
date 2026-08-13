//! Error types shared by the bounded containers.

use core::fmt;

/// The container had no room for the value; the value is handed back to the caller.
///
/// Real-time code must never lose data silently, so every bounded `push` in this
/// crate returns the rejected value instead of dropping it. The caller decides
/// whether to retry, to count the overflow, or to drop the value explicitly.
///
/// [any-thread]
pub struct Full<T>(
    /// The value that could not be stored.
    pub T,
);

impl<T> Full<T> {
    /// Consumes the error and returns the rejected value. [any-thread]
    #[inline]
    pub fn into_inner(self) -> T {
        self.0
    }
}

// A manual impl keeps `Result::unwrap` usable for payloads that are not `Debug`.
impl<T> fmt::Debug for Full<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Full(..)")
    }
}

impl<T> fmt::Display for Full<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("bounded container is full")
    }
}

impl<T> std::error::Error for Full<T> {}

/// The operation needed more room than the container has left.
///
/// Used where the rejected input is borrowed and therefore cannot be returned,
/// such as [`FixedVec::extend_from_slice`](crate::FixedVec::extend_from_slice).
/// The container is always left untouched when this is returned.
///
/// [any-thread]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct CapacityError;

impl fmt::Display for CapacityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("not enough capacity left")
    }
}

impl std::error::Error for CapacityError {}

#[cfg(test)]
mod tests {
    use super::{CapacityError, Full};

    /// Deliberately not `Debug`: `Full<T>` must stay `unwrap`-able regardless.
    struct NotDebug(u32);

    #[test]
    fn full_returns_the_value() {
        let e = Full(NotDebug(7));
        assert_eq!(e.into_inner().0, 7);
    }

    #[test]
    fn full_is_debug_without_a_debug_payload() {
        // Via a function so the payload is not a literal the optimiser (or
        // clippy) can see through: the point is that `unwrap` compiles at all.
        fn rejected(value: u32) -> Result<(), Full<NotDebug>> {
            Err(Full(NotDebug(value)))
        }
        assert_eq!(format!("{:?}", rejected(1).unwrap_err()), "Full(..)");
    }

    #[test]
    fn errors_display_and_are_std_errors() {
        let f: Box<dyn std::error::Error> = Box::new(Full(1u8));
        assert_eq!(f.to_string(), "bounded container is full");
        let c: Box<dyn std::error::Error> = Box::new(CapacityError);
        assert_eq!(c.to_string(), "not enough capacity left");
    }
}
