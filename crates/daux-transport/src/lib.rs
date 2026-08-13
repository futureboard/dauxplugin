//! Host transport, timeline and musical time model for DAUxPlug.
//!
//! [`Transport`] is the format-neutral, plain-Rust mirror of `DauxTransportV1`
//! (`docs/specifications/abi-v1.md` §10): where the block sits on the host timeline, how
//! fast the music is moving, and what the host is doing with the loop and the recorder.
//! It is `Copy`, `Send`, `Sync`, holds no references and allocates nothing, so it can be
//! snapshotted into an event, handed to the UI, or read from the audio thread freely.
//!
//! This crate has **no dependencies** and contains no `unsafe`.
//!
//! # A field is not readable unless the host promised it
//!
//! Hosts differ wildly in what they report. ABI v1 therefore pairs most fields with a
//! `HAS_*` bit and forbids hosts from fabricating values. Every accessor here honours
//! that: [`Transport::tempo`], [`Transport::beats`], [`Transport::time_signature`] and
//! friends return `Option` and are `None` unless the matching [`TransportFlags`] bit is
//! set. The fields stay public so ABI adapters can populate them, but a plug-in that goes
//! through the accessors cannot accidentally read a number the host never wrote.
//!
//! ```
//! use daux_transport::{Transport, TransportBuilder};
//!
//! // A host that only knows about tempo.
//! let t = TransportBuilder::new().playing(true).tempo(128.0).build();
//! assert_eq!(t.tempo(), Some(128.0));
//! assert_eq!(t.beats(), None);
//! assert_eq!(t.time_signature(), None);
//!
//! // No transport at all.
//! assert_eq!(Transport::EMPTY.tempo(), None);
//! ```
//!
//! # Musical time and tempo ramps
//!
//! Musical positions are quarter-note beats, matching the ABI. Positions describe the
//! **first sample of the current block**; conversions measure spans *from* that boundary,
//! and [`Transport::beats_at`] / [`Transport::seconds_at`] turn a sample-accurate event
//! offset into an absolute position.
//!
//! A host that is automating the tempo reports `tempo` (BPM at the first sample) together
//! with `tempo_increment` (BPM added per sample), which makes the tempo a straight line
//! across the block:
//!
//! ```text
//! T(t) = tempo + tempo_increment · t                                   [BPM]
//! ```
//!
//! Musical time therefore advances by `T(t) / (60 · sample_rate)` beats per sample, and
//! the beats spanned by `n` samples is the integral of that rate — **not** `n · tempo`:
//!
//! ```text
//! B(n) = ∫₀ⁿ (tempo + tempo_increment · t) / (60 · sample_rate) dt
//!      = (tempo · n + tempo_increment · n² / 2) / (60 · sample_rate)
//! ```
//!
//! [`Transport::samples_to_beats`] evaluates `B(n)` directly.
//! [`Transport::beats_to_samples`] inverts it by solving
//! `½·tempo_increment·n² + tempo·n − 60·sample_rate·beats = 0` in a cancellation-free
//! form, picking the root whose instantaneous tempo is non-negative and reproducing the
//! steady-tempo result exactly when `tempo_increment` is `0.0`. A decelerating ramp that
//! would reach 0 BPM before the requested beat is unreachable, and reports `None` rather
//! than an invented answer.
//!
//! ```
//! use daux_transport::TransportBuilder;
//!
//! let sr = 48_000.0;
//! let steady = TransportBuilder::new().tempo(120.0).build();
//! assert_eq!(steady.beats_to_samples(1.0, sr), Some(24_000.0));
//!
//! // Accelerating: the same beat arrives sooner.
//! let ramp = TransportBuilder::new().tempo_ramp(120.0, 0.001).build();
//! assert!(ramp.beats_to_samples(1.0, sr).unwrap() < 24_000.0);
//! ```
//!
//! # Building a transport
//!
//! [`TransportBuilder`] is how hosts, adapters and tests construct instances whose flags
//! always match the values that were actually supplied.

#![forbid(unsafe_code)]

mod builder;
mod events;
mod flags;
mod signature;
mod transport;

pub use builder::TransportBuilder;
pub use daux_events;
pub use flags::TransportFlags;
pub use signature::TimeSignature;
pub use transport::Transport;
