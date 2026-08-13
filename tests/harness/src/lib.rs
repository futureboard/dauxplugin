//! Shared fixtures for the DAUxPlug cross-crate test suites.
//!
//! The suites in `tests/` are the ones no single crate can write, because each of them
//! spans a boundary: the binary contract between `daux-abi` and
//! `docs/specifications/abi-v1.md`, the no-allocation guarantee between `daux-core`'s
//! object model and `daux-rt`'s allocation tripwire, the write/read round trip through
//! `daux-bundle`, and the scanner's behaviour over a tree of real directories.
//!
//! This library is the machinery those suites share:
//!
//! | Module | What it gives a test |
//! | --- | --- |
//! | [`temp`] | a self-deleting directory to build fixtures in |
//! | [`bundles`] | synthetic `.axt` bundles, well-formed and deliberately hostile |
//! | [`fakehost`] | a recording host implementing the `daux-host-services` traits |
//! | [`signal`] | deterministic, allocation-free signal generation |
//! | [`plugins`] | fixture plug-ins: a gain effect, an instrument, an event echo |
//! | [`rig`] | drives one `process` block, allocating nothing |
//! | [`assertions`] | peak/RMS/tolerance helpers with useful failure messages |
//!
//! # Real-time discipline
//!
//! Everything a test may want to run **inside** a
//! [`daux_rt::AllocGuard`](daux_rt::AllocGuard) scope — [`signal`], [`rig`], the
//! processors in [`plugins`], and the audio-thread half of [`fakehost`] — is
//! allocation-free by construction and marked `[audio-thread]`. Everything else
//! (building bundles, formatting messages, enumerating parameters) is `[main-thread]`
//! and allocates freely.
//!
//! The allocation counter only works when the test binary installs
//! [`daux_rt::CountingAllocator`] as its `#[global_allocator]`. A test that asserts on
//! allocation counts must also assert
//! [`daux_rt::counting_allocator_installed`], or it passes vacuously; the
//! [`rig::assert_no_alloc`] helper does both.

#![forbid(unsafe_code)]

pub mod assertions;
pub mod bundles;
pub mod fakehost;
pub mod plugins;
pub mod rig;
pub mod signal;
pub mod temp;

/// The host-service traits, reached through `daux-core`'s re-export.
///
/// `daux-tests` does not depend on `daux-host-services` directly — the dependency graph
/// in the root `Cargo.toml` is deliberate — so the fixtures name it through the crate
/// that already re-exports it.
pub use daux_core::daux_host_services as host_services;

pub use crate::temp::TempTree;

/// This library's own unit tests run under the counting allocator, so the fixtures' claim
/// to be allocation-free is measured rather than asserted. Integration tests install it
/// for themselves; nothing outside `cfg(test)` touches the global allocator.
#[cfg(test)]
#[global_allocator]
static TEST_ALLOCATOR: daux_rt::CountingAllocator = daux_rt::CountingAllocator;
