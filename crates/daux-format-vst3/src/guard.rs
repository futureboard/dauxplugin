//! Catching panics at the boundary and poisoning what they came out of.
//!
//! A Rust panic unwinding through an `extern "system"` frame is undefined behaviour, and the
//! frame on the other side belongs to a DAW with a user's session in it. abi-v1 §17 therefore
//! requires three things of every exported function, and this module is where all three
//! happen exactly once instead of being re-typed at sixty call sites:
//!
//! 1. the body runs inside [`std::panic::catch_unwind`];
//! 2. a caught panic becomes the format's error code — [`result::INTERNAL_ERROR`] here;
//! 3. the object marks itself **poisoned** and refuses everything afterwards with
//!    [`result::NOT_INITIALIZED`], because a plug-in that has already broken its own
//!    invariants must never be re-entered.
//!
//! Poisoning is one-way and idempotent. A host is expected to treat a poisoned object as
//! unloadable-but-safe: release it and carry on, never abort.

use core::sync::atomic::{AtomicBool, Ordering};
use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::com::{TResult, result};

/// The poison flag of one COM object.
///
/// Separate from `daux_plugin_api::PluginInstance`'s own poison state because the flag must
/// be readable from methods that never touch the instance — the parameter mirror, for one —
/// and from the audio thread, where taking the instance is not possible.
#[derive(Debug, Default)]
pub struct Poison(AtomicBool);

impl Poison {
    /// `[any-thread]` A healthy object.
    #[must_use]
    pub const fn new() -> Self {
        Self(AtomicBool::new(false))
    }

    /// `[any-thread]` `true` once a panic has crossed this object's boundary.
    #[inline]
    #[must_use]
    pub fn is_poisoned(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    /// `[any-thread]` Marks the object unusable. Idempotent and allocation-free.
    #[inline]
    pub fn poison(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// `[any-thread]` Runs `f` under `catch_unwind`, converting a panic to a result code.
    ///
    /// Returns [`result::NOT_INITIALIZED`] without running `f` when the object is already
    /// poisoned, and [`result::INTERNAL_ERROR`] when `f` panics — poisoning the object on
    /// the way out.
    pub fn call(&self, f: impl FnOnce() -> TResult) -> TResult {
        self.call_or(result::INTERNAL_ERROR, f)
    }

    /// `[any-thread]` As [`call`](Self::call), with a caller-chosen failure code.
    ///
    /// `process` uses this to answer a panic with the code a host expects from a failed
    /// block rather than a generic error.
    pub fn call_or(&self, on_panic: TResult, f: impl FnOnce() -> TResult) -> TResult {
        if self.is_poisoned() {
            return result::NOT_INITIALIZED;
        }
        // SAFETY-of-unwind-safety: `AssertUnwindSafe` is sound here precisely *because* of
        // what happens on the error arm. A panic may leave the plug-in's own state torn, and
        // the poison flag makes sure nothing ever observes it again: every later call
        // returns before touching the instance. That is abi-v1 §17.3 expressed in types.
        match catch_unwind(AssertUnwindSafe(f)) {
            Ok(r) => r,
            Err(_) => {
                self.poison();
                on_panic
            }
        }
    }

    /// `[any-thread]` Runs `f` under `catch_unwind` **without** the poison gate.
    ///
    /// Reserved for `FUnknown`: a host that has just been told an object is poisoned still
    /// has to be able to `queryInterface` it, so that it can find the interfaces it holds and
    /// release them. Refusing there would strand the object and leak the plug-in — the exact
    /// outcome abi-v1 §17 is trying to avoid when it says a poisoned instance must be
    /// "unloadable-but-safe". A panic inside `f` still poisons.
    pub fn call_always(&self, f: impl FnOnce() -> TResult) -> TResult {
        match catch_unwind(AssertUnwindSafe(f)) {
            Ok(r) => r,
            Err(_) => {
                self.poison();
                result::INTERNAL_ERROR
            }
        }
    }

    /// `[any-thread]` As [`call`](Self::call) for a method that returns a value rather than a
    /// result code — `getLatencySamples`, `getParamNormalized`, `createView`.
    ///
    /// `fallback` is returned both when the object is poisoned and when `f` panics, because
    /// those signatures have no way to say "failed" other than by answering something safe.
    pub fn call_value<T>(&self, fallback: T, f: impl FnOnce() -> T) -> T {
        if self.is_poisoned() {
            return fallback;
        }
        match catch_unwind(AssertUnwindSafe(f)) {
            Ok(v) => v,
            Err(_) => {
                self.poison();
                fallback
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_healthy_object_just_runs_the_body() {
        let poison = Poison::new();
        assert!(!poison.is_poisoned());
        assert_eq!(poison.call(|| result::OK), result::OK);
        assert_eq!(poison.call_value(7, || 3), 3);
        assert!(!poison.is_poisoned());
    }

    #[test]
    fn a_panic_becomes_an_error_code_and_poisons_for_ever() {
        let poison = Poison::new();
        let quiet = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));

        assert_eq!(
            poison.call(|| panic!("the plug-in indexed past the end of its voice table")),
            result::INTERNAL_ERROR
        );
        assert!(poison.is_poisoned());

        // Everything afterwards is refused *without running the body*, which is the whole
        // point: the plug-in's invariants are already broken.
        let mut ran = false;
        assert_eq!(
            poison.call(|| {
                ran = true;
                result::OK
            }),
            result::NOT_INITIALIZED
        );
        assert!(!ran, "a poisoned object must not re-enter plug-in code");
        assert_eq!(poison.call_value(-1.0, || 0.5), -1.0);

        std::panic::set_hook(quiet);
    }

    #[test]
    fn a_panicking_value_method_falls_back_and_poisons() {
        let poison = Poison::new();
        let quiet = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));

        assert_eq!(poison.call_value(42u32, || panic!("boom")), 42);
        assert!(poison.is_poisoned());

        std::panic::set_hook(quiet);
    }

    #[test]
    fn the_failure_code_can_be_chosen_by_the_caller() {
        let poison = Poison::new();
        let quiet = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));

        assert_eq!(
            poison.call_or(result::INVALID_ARGUMENT, || panic!("boom")),
            result::INVALID_ARGUMENT
        );
        // …but a poisoned object still answers with the state code, not the caller's.
        assert_eq!(
            poison.call_or(result::INVALID_ARGUMENT, || result::OK),
            result::NOT_INITIALIZED
        );

        std::panic::set_hook(quiet);
    }

    #[test]
    fn funknown_keeps_working_after_a_panic_so_the_object_can_be_released() {
        let poison = Poison::new();
        poison.poison();
        let mut ran = false;
        assert_eq!(
            poison.call_always(|| {
                ran = true;
                result::OK
            }),
            result::OK
        );
        assert!(ran, "queryInterface must survive poisoning");

        // …and it still catches a panic of its own.
        let quiet = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        assert_eq!(
            poison.call_always(|| panic!("boom")),
            result::INTERNAL_ERROR
        );
        std::panic::set_hook(quiet);
    }

    #[test]
    fn poisoning_is_idempotent() {
        let poison = Poison::new();
        poison.poison();
        poison.poison();
        assert!(poison.is_poisoned());
        assert_eq!(poison.call(|| result::OK), result::NOT_INITIALIZED);
    }
}
