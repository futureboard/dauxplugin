//! Focused DSP building blocks with runtime SIMD dispatch for DAUxPlug.
//!
//! This crate is deliberately small. It is not a DSP library; it is the set of
//! primitives that turn up in *every second plug-in* — a gain conversion, a
//! biquad, a DC blocker, a meter ballistic, a delay line — plus the vector
//! helpers that make block-rate work run at the width of the host CPU. Anything
//! more specialised belongs in the plug-in that needs it, where it can be tuned
//! against a real signal instead of a general-purpose compromise.
//!
//! # Real-time contract
//!
//! Every `process`, `process_block`, `read` and `write` here is
//! **[audio-thread]**: no allocation, no lock, no I/O, no unbounded loop, and no
//! reachable panic. Only [`DelayLine::new`] allocates, and it is
//! **[main-thread]** — call it from `prepare`.
//!
//! Coefficient computation is separated from sample processing throughout.
//! Constructors and `set_*` methods evaluate the transcendental functions once;
//! the per-sample path is multiplies and adds. Retuning a live filter keeps its
//! state, so sweeping a cutoff between blocks does not click:
//!
//! ```
//! # use daux_dsp::Biquad;
//! let mut filter = Biquad::lowpass(48_000.0, 1_000.0, 0.707);
//! # let mut block = [0.0_f32; 128];
//! // ...per block, on the audio thread:
//! filter.set_lowpass(48_000.0, 2_000.0, 0.707); // state preserved, no click
//! filter.process_block(&mut block);
//! ```
//!
//! # Denormals
//!
//! Every recursive structure flushes its state through [`flush_denormal_f64`].
//! A filter ringing down into the subnormal range can cost tens of times its
//! normal CPU budget on some hardware — a real-time failure that appears only
//! after the music stops. The guard is unconditional and costs a compare and a
//! conditional move; see the [`denormal`](self#denormals) notes on each type.
//!
//! # Numeric choices
//!
//! Recursive filters keep their coefficients and state in `f64` while taking and
//! returning `f32`. A biquad is a handful of instructions either way, and `f64`
//! state removes the cancellation that makes `f32` sections noisy below roughly
//! `0.001 · sample_rate` — precisely where DC blockers and low shelves live.
//! [`DelayLine`] stores `f32`, because it stores *audio*, and doubling a
//! multi-second buffer would cost cache, not accuracy.
//!
//! # Vector helpers
//!
//! [`simd`] dispatches on a `OnceLock`-cached CPU feature probe and produces
//! bit-identical results on every path. Call [`simd::prime`] from `prepare` so
//! that detection never happens on the audio thread.

mod biquad;
mod delay;
mod denormal;
mod follower;
mod gain;
mod onepole;
mod range;

pub mod simd;

pub use biquad::{Biquad, BiquadCoeffs};
pub use delay::DelayLine;
pub use denormal::{
    DENORMAL_THRESHOLD_F32, DENORMAL_THRESHOLD_F64, flush_denormal, flush_denormal_f64,
};
pub use follower::PeakFollower;
pub use gain::{db_to_gain, db_to_gain_f64, gain_to_db, gain_to_db_f64};
pub use onepole::{DcBlocker, OnePole};

#[cfg(test)]
mod tests {
    use super::*;

    /// A miniature signal chain, exercising the crate the way a plug-in would:
    /// high-pass, saturate, block the DC the saturator introduced, delay, mix,
    /// meter. The point is that the pieces compose without surprises.
    #[test]
    fn the_pieces_compose_into_a_working_chain() {
        const SR: f64 = 48_000.0;
        const BLOCK: usize = 128;

        let mut hp = Biquad::highpass(SR, 80.0, core::f64::consts::FRAC_1_SQRT_2);
        let mut dc = DcBlocker::new(SR);
        let mut delay = DelayLine::new(BLOCK);
        let mut meter = PeakFollower::new(SR, 0.0, 300.0);
        let gain = db_to_gain(-6.0);
        simd::prime();

        let mut dry = [0.0_f32; BLOCK];
        let mut wet = [0.0_f32; BLOCK];

        for block in 0..64 {
            for (i, x) in dry.iter_mut().enumerate() {
                let n = block * BLOCK + i;
                *x = (n as f32 * 0.05).sin() * 0.8 + 0.2; // signal plus DC offset
            }

            hp.process_block(&mut dry);
            // An asymmetric saturator: exactly the thing that manufactures DC.
            for x in dry.iter_mut() {
                *x = x.tanh() * if *x > 0.0 { 1.0 } else { 0.7 };
            }
            dc.process_block(&mut dry);

            for (i, &x) in dry.iter().enumerate() {
                wet[i] = delay.process(x, 64.5);
            }

            simd::apply_gain_ramp(&mut wet, gain, gain * 0.5);
            simd::add_from(&mut dry, &wet);
            meter.process_block(&dry);

            for &x in &dry {
                assert!(x.is_finite(), "block {block} produced a non-finite sample");
            }
        }

        let peak = meter.value();
        assert!(peak > 0.0 && peak < 4.0, "implausible meter reading {peak}");
        assert!(simd::peak_abs(&dry) <= 4.0);
    }

    /// The audio-thread surface must be usable from a `Send` context that owns
    /// its state, which is how a processor is actually driven.
    #[test]
    fn audio_thread_types_are_send() {
        fn assert_send<T: Send>() {}
        assert_send::<Biquad>();
        assert_send::<BiquadCoeffs>();
        assert_send::<OnePole>();
        assert_send::<DcBlocker>();
        assert_send::<PeakFollower>();
        assert_send::<DelayLine>();
    }

    /// Round-tripping a level through both conversions and a meter is the most
    /// common thing a plug-in does with this crate.
    #[test]
    fn a_metered_gain_stage_reads_back_the_level_it_applied() {
        let mut buf = [1.0_f32; 512];
        simd::apply_gain(&mut buf, db_to_gain(-12.0));
        let db = gain_to_db(simd::peak_abs(&buf));
        assert!((db + 12.0).abs() < 1.0e-4, "measured {db} dB");
    }
}
