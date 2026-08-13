//! Strongly typed, automation-friendly parameter system for DAUxPlug.
//!
//! # Plain values are the contract
//!
//! Every value that crosses a boundary — the ABI, a saved project, a host automation
//! lane, an editor widget — is a **plain** (real-world) value: `-6.0` dB, `440.0` Hz,
//! `3` for the fourth entry of an enum. Normalisation to `0..=1` is an internal,
//! plug-in-side concern of [`ParamRange`], exactly as required by
//! `docs/specifications/abi-v1.md` §11.2.
//!
//! The practical consequence is important enough to state twice: **changing a curve
//! never breaks existing automation**. Moving a filter cutoff from
//! [`ParamRange::Linear`] to [`ParamRange::Logarithmic`] changes how a knob feels and
//! how a normalised host lane maps onto the value, but a session that stored
//! `1000.0 Hz` still recalls `1000.0 Hz`. Only [`ParamId`]s and plain units are
//! permanent; curves are free to evolve.
//!
//! # Sharing
//!
//! Each concrete parameter stores its value in a [`daux_rt::AtomicF32`] /
//! [`daux_rt::AtomicF64`], so `&P` is [`Sync`] and one `Arc<dyn Params>` is shared by
//! the processor, the controller and the editor with no locks and no message passing
//! for the value itself. Gesture bookkeeping and host notification live in
//! `daux-host-services`, not here.
//!
//! # Threads
//!
//! Every public item is annotated `[audio-thread]`, `[main-thread]` or `[any-thread]`
//! matching `abi-v1` §15. In short:
//!
//! * value get/set ([`Param::plain`], [`Param::set_plain`], [`Param::normalized`],
//!   [`Param::set_normalized`], [`Param::reset`]) are `[any-thread]`, wait-free and
//!   allocation-free;
//! * construction, [`Param::info`], [`Param::from_text`] and [`Params::param_refs`]
//!   are `[main-thread]` and may allocate;
//! * [`Param::to_text`] allocates only inside the caller's `String`;
//! * [`Smoother::next`] / [`Smoother::next_block`] are `[audio-thread]`.
//!
//! # Example
//!
//! ```
//! use daux_parameter::{FloatParam, Param, ParamFlags, ParamId, ParamRange, Smoothing};
//!
//! let gain = FloatParam::new(ParamId(1), "Gain", 0.0, ParamRange::Linear { min: -60.0, max: 12.0 })
//!     .with_unit("dB")
//!     .with_decimals(1)
//!     .with_smoothing(Smoothing::Exponential { ms: 20.0 })
//!     .with_flags(ParamFlags::AUTOMATABLE | ParamFlags::MODULATABLE);
//!
//! gain.set_plain(-6.0);
//! assert_eq!(gain.plain(), -6.0);
//!
//! let mut text = String::new();
//! gain.to_text(gain.plain(), &mut text);
//! assert_eq!(text, "-6.0 dB");
//! assert_eq!(gain.from_text("  -6.0 dB "), Some(-6.0));
//! ```
#![forbid(unsafe_code)]

mod boolean;
mod enums;
mod flags;
mod float;
mod id;
mod info;
mod int;
mod meter;
mod migration;
mod param;
mod range;
mod smoothing;
pub mod text;

pub use boolean::BoolParam;
pub use enums::{EnumParam, ParamEnum};
pub use flags::ParamFlags;
pub use float::FloatParam;
pub use id::ParamId;
pub use info::ParamInfo;
pub use int::IntParam;
pub use meter::MeterParam;
pub use migration::{ParamMigration, migrate_param_id};
pub use param::{FormatFn, Param, Params, ParseFn};
pub use range::{ParamRange, RangeError};
pub use smoothing::{Smoother, Smoothing};

/// The crate's own tests run under the counting allocator so that "this does not
/// allocate" is a checked assertion rather than a comment. Production builds are
/// untouched: the attribute only exists while compiling the test harness.
#[cfg(test)]
#[global_allocator]
static TEST_ALLOCATOR: daux_rt::CountingAllocator = daux_rt::CountingAllocator;

#[cfg(test)]
mod realtime_tests {
    use super::*;
    use daux_rt::{AllocGuard, counting_allocator_installed};

    /// Runs `f` and asserts it allocated nothing.
    fn assert_no_alloc<R>(what: &str, f: impl FnOnce() -> R) -> R {
        assert!(
            counting_allocator_installed(),
            "the allocation tripwire is not installed, so this test would pass vacuously"
        );
        let (result, allocations) = AllocGuard::scope(f);
        assert_eq!(allocations, 0, "{what} allocated {allocations} time(s)");
        result
    }

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Shape {
        Sine,
        Saw,
    }

    impl ParamEnum for Shape {
        const VARIANTS: &'static [Self] = &[Self::Sine, Self::Saw];
        fn name(self) -> &'static str {
            match self {
                Self::Sine => "Sine",
                Self::Saw => "Saw",
            }
        }
        fn index(self) -> u32 {
            self as u32
        }
        fn from_index(i: u32) -> Option<Self> {
            Self::VARIANTS.get(i as usize).copied()
        }
    }

    #[test]
    fn value_access_never_allocates() {
        let gain = FloatParam::new(ParamId(1), "Gain", 0.0, ParamRange::linear(-60.0, 12.0));
        let count = IntParam::new(ParamId(2), "Voices", 8, 1, 16);
        let flip = BoolParam::new(ParamId(3), "Invert", false);
        let shape = EnumParam::new(ParamId(4), "Shape", Shape::Sine);
        let meter = MeterParam::new(ParamId(5), "Level", ParamRange::linear(-60.0, 6.0));

        assert_no_alloc("parameter value access", || {
            gain.set_plain(-6.0);
            let _ = gain.plain();
            gain.set_normalized(0.25);
            let _ = gain.normalized();
            gain.reset();
            let _ = gain.value_f32();
            let _ = gain.flags();
            let _ = gain.id();

            count.set(4);
            let _ = count.value();
            count.set_normalized(0.5);
            count.reset();

            flip.set(true);
            let _ = flip.value();
            flip.set_plain(0.0);
            flip.reset();

            shape.set(Shape::Saw);
            let _ = shape.value();
            shape.set_normalized(0.0);
            shape.reset();

            meter.set_value(-12.0);
            meter.push_peak(-3.0);
            let _ = meter.value();
            meter.clear();
        });
    }

    #[test]
    fn formatting_into_a_reserved_buffer_never_allocates() {
        let gain = FloatParam::new(ParamId(1), "Gain", 0.0, ParamRange::linear(-60.0, 12.0))
            .with_unit("dB");
        let shape = EnumParam::new(ParamId(4), "Shape", Shape::Sine);
        let mut out = String::with_capacity(64);

        assert_no_alloc("to_text into a reserved buffer", || {
            gain.to_text(-6.0, &mut out);
            gain.to_text(-12.345, &mut out);
            shape.to_text(1.0, &mut out);
        });
        assert_eq!(out, "Saw");
    }

    #[test]
    fn parsing_never_allocates() {
        let gain = FloatParam::new(ParamId(1), "Gain", 0.0, ParamRange::linear(-60.0, 12.0))
            .with_unit("dB");
        let flip = BoolParam::new(ParamId(3), "Invert", false);
        let shape = EnumParam::new(ParamId(4), "Shape", Shape::Sine);

        let parsed = assert_no_alloc("from_text", || {
            (
                gain.from_text("  -6.0 dB "),
                flip.from_text("on"),
                shape.from_text("saw"),
            )
        });
        assert_eq!(parsed, (Some(-6.0), Some(1.0), Some(1.0)));
    }

    #[test]
    fn the_smoother_never_allocates_on_the_audio_thread() {
        let mut smoother = Smoother::new(Smoothing::Exponential { ms: 5.0 });
        smoother.prepare(48_000.0);
        smoother.reset_to(0.0);
        let mut block = [0.0f32; 128];

        assert_no_alloc("smoother", || {
            smoother.set_target(1.0);
            for _ in 0..64 {
                let _ = smoother.next();
            }
            smoother.next_block(&mut block);
            let _ = smoother.is_smoothing();
            smoother.reset_to(0.5);
        });
        assert!(block[127] > 0.99, "the ramp still ran: {}", block[127]);
        assert_eq!(smoother.current(), 0.5);
    }

    #[test]
    fn range_mapping_never_allocates() {
        let ranges = [
            ParamRange::linear(-60.0, 12.0),
            ParamRange::skewed(0.0, 1.0, 0.3),
            ParamRange::logarithmic(20.0, 20_000.0),
            ParamRange::stepped(0, 16),
            ParamRange::Boolean,
        ];
        assert_no_alloc("range mapping", || {
            for range in &ranges {
                for i in 0..=10 {
                    let n = f64::from(i) / 10.0;
                    let plain = range.denormalize(n);
                    let _ = range.normalize(plain);
                    let _ = range.clamp(plain);
                    let _ = range.snap_normalized(n);
                }
            }
        });
    }
}
