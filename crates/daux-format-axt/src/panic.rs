//! The boundary guard: catching unwinds and turning them into status codes (abi-v1 §17).
//!
//! A Rust panic unwinding across `extern "C"` is undefined behaviour, so every exported
//! function in this crate wraps its whole body in [`catch`]. What a caught panic *becomes*
//! depends on the function's return type, which is what [`Refusal`] encodes: `DAUX_ERR_PANIC`
//! for a status, `DAUX_PROCESS_ERROR` for `process`, `0`/false for the plain-integer getters,
//! and nothing at all for the `void` entries.
//!
//! The second half of §17 is poisoning, and it lives with the objects that can be poisoned:
//! [`crate::factory`] and [`crate::instance`]. This module only stops the unwind.

use std::panic::{AssertUnwindSafe, catch_unwind};

use daux_abi::{
    DAUX_ERR_INVALID_ARG, DAUX_ERR_INVALID_STATE, DAUX_ERR_PANIC, DAUX_FALSE, DAUX_PROCESS_ERROR,
    DauxStatus,
};
use daux_plugin_api::{DauxError, DauxResult};

/// What an exported function returns when it cannot run the plug-in's code at all.
///
/// Implemented once per ABI return type so that the guards can be written generically instead
/// of once per entry, which is how one of a dozen near-identical wrappers ends up returning the
/// wrong code.
pub(crate) trait Refusal: Copy {
    /// A required pointer argument was null, or a `size` field was too small to be a valid
    /// v1.0 structure.
    const INVALID_ARG: Self;
    /// The object is poisoned after an earlier panic and refuses further work (§17.3).
    const POISONED: Self;
    /// A panic was caught inside this call (§17.2).
    const PANICKED: Self;
}

impl Refusal for DauxStatus {
    const INVALID_ARG: Self = DAUX_ERR_INVALID_ARG;
    const POISONED: Self = DAUX_ERR_INVALID_STATE;
    const PANICKED: Self = DAUX_ERR_PANIC;
}

impl Refusal for () {
    const INVALID_ARG: Self = ();
    const POISONED: Self = ();
    const PANICKED: Self = ();
}

/// `process` returns a bare `i32` (abi-v1 §8), and every failure is
/// [`DAUX_PROCESS_ERROR`].
impl Refusal for i32 {
    const INVALID_ARG: Self = DAUX_PROCESS_ERROR;
    const POISONED: Self = DAUX_PROCESS_ERROR;
    const PANICKED: Self = DAUX_PROCESS_ERROR;
}

/// Covers the counting getters (`plugin_count`, `count`, `latency`, `tail`) and every
/// [`DauxBool`](daux_abi::DauxBool) entry, for which the refusal is `0` / false.
impl Refusal for u32 {
    const INVALID_ARG: Self = DAUX_FALSE;
    const POISONED: Self = DAUX_FALSE;
    const PANICKED: Self = DAUX_FALSE;
}

/// [any-thread] Runs `f`, converting an unwind into [`Refusal::PANICKED`].
///
/// The closure is wrapped in [`AssertUnwindSafe`] because none of the state an adapter touches
/// is `UnwindSafe` — a `Box<dyn DauxPlugin>` never is. That is sound here precisely because of
/// §17.3: the object a panic escaped from is poisoned by the caller of this function and is
/// never entered again, so no torn invariant can be observed afterwards.
#[inline]
pub(crate) fn catch<R: Refusal>(f: impl FnOnce() -> R) -> R {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(value) => value,
        Err(_payload) => R::PANICKED,
    }
}

/// [any-thread] Runs `f`, reporting whether it unwound.
///
/// The caller uses the `bool` to poison the object before returning the refusal, which is the
/// half of §17 [`catch`] deliberately does not do for it.
#[inline]
pub(crate) fn catch_reporting<R>(f: impl FnOnce() -> R) -> Result<R, ()> {
    catch_unwind(AssertUnwindSafe(f)).map_err(|_payload| ())
}

/// [any-thread] The ABI status for a `DauxResult`.
///
/// `daux-core`'s error kinds already carry the `DAUX_ERR_*` values (its `status` module is the
/// same transcription this crate's `daux-abi` constants are), so this is a widening, not a
/// mapping table that could drift.
#[inline]
pub(crate) fn status_of(result: DauxResult<()>) -> DauxStatus {
    match result {
        Ok(()) => daux_abi::DAUX_OK,
        Err(err) => status_of_error(&err),
    }
}

/// [any-thread] The ABI status for one error.
#[inline]
pub(crate) fn status_of_error(err: &DauxError) -> DauxStatus {
    DauxStatus::from_raw(err.status_code())
}
