//! Deterministic, allocation-free signal generation.
//!
//! A real-time test needs input that is the same on every machine and every run: a failure
//! that only reproduces with one seed is not a failure anyone can fix. Everything here is
//! reproducible from its parameters alone, writes into a caller-owned slice, and never
//! allocates, so it may be used inside a [`daux_rt::AllocGuard`] scope.

/// A xorshift64\* pseudo-random generator.
///
/// Not cryptographic and not meant to be: it exists so that "noise" means the same
/// sequence of samples in CI as on a developer's machine. The algorithm is fixed here on
/// purpose — swapping it would change every recorded expectation.
///
/// [audio-thread]
#[derive(Clone, Copy, Debug)]
pub struct Rng {
    state: u64,
}

impl Rng {
    /// [any-thread] Seeds the generator. Seed `0` is remapped, since xorshift is stuck there.
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 {
                0x9E37_79B9_7F4A_7C15
            } else {
                seed
            },
        }
    }

    /// [audio-thread] The next 64-bit word.
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// [audio-thread] The next sample in `-1.0 ..= 1.0`.
    pub fn next_sample(&mut self) -> f32 {
        // 24 bits is exactly an `f32` mantissa, so every value is representable exactly and
        // the distribution has no gaps or repeats.
        let bits = (self.next_u64() >> 40) as u32; // 0 ..= 16_777_215
        bits as f32 / 8_388_608.0 - 1.0
    }
}

/// [audio-thread] Fills `out` with white noise. Returns the advanced generator state.
pub fn fill_noise(out: &mut [f32], rng: &mut Rng) {
    for sample in out.iter_mut() {
        *sample = rng.next_sample();
    }
}

/// [audio-thread] Fills `out` with a sine at `frequency` Hz, continuing from `phase`.
///
/// Returns the phase to pass to the next block, wrapped into `0.0 ..= 1.0` so it never
/// loses precision however long the test runs.
#[must_use]
pub fn fill_sine(out: &mut [f32], frequency: f64, sample_rate: f64, phase: f64) -> f64 {
    let increment = frequency / sample_rate;
    let mut current = phase;
    for sample in out.iter_mut() {
        *sample = (core::f64::consts::TAU * current).sin() as f32;
        current += increment;
        if current >= 1.0 {
            current -= 1.0;
        }
    }
    current
}

/// [audio-thread] Fills `out` with a linear ramp from `from` to `to` inclusive.
pub fn fill_ramp(out: &mut [f32], from: f32, to: f32) {
    let last = out.len().saturating_sub(1);
    if last == 0 {
        out.fill(from);
        return;
    }
    for (i, sample) in out.iter_mut().enumerate() {
        *sample = from + (to - from) * (i as f32 / last as f32);
    }
}

/// [audio-thread] Fills `out` with silence and a single unit impulse at `at`.
///
/// An out-of-range `at` leaves the buffer silent, which is what a test asking for an
/// impulse past the end of the block means.
pub fn fill_impulse(out: &mut [f32], at: usize) {
    out.fill(0.0);
    if let Some(sample) = out.get_mut(at) {
        *sample = 1.0;
    }
}

/// [main-thread] Fills every channel of an [`daux_audio::AudioStorage`] with a sine.
///
/// Each channel starts from the same phase, so the channels are identical — which is what
/// a test that later checks per-channel gain wants.
pub fn fill_storage_sine(
    storage: &mut daux_audio::AudioStorage<f32>,
    frequency: f64,
    sample_rate: f64,
) {
    for channel in 0..storage.channel_count() {
        if let Some(samples) = storage.channel_mut(channel) {
            let _ = fill_sine(samples, frequency, sample_rate, 0.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_generator_is_reproducible_and_bounded() {
        let first: Vec<f32> = (0..64)
            .scan(Rng::new(7), |rng, _| Some(rng.next_sample()))
            .collect();
        let second: Vec<f32> = (0..64)
            .scan(Rng::new(7), |rng, _| Some(rng.next_sample()))
            .collect();
        assert_eq!(first, second, "the same seed must give the same samples");
        assert!(first.iter().all(|s| (-1.0..=1.0).contains(s)), "{first:?}");
        // A different seed must not give the same stream.
        let other: Vec<f32> = (0..64)
            .scan(Rng::new(8), |rng, _| Some(rng.next_sample()))
            .collect();
        assert_ne!(first, other);
    }

    #[test]
    fn a_zero_seed_still_produces_a_stream() {
        let mut rng = Rng::new(0);
        let a = rng.next_u64();
        let b = rng.next_u64();
        assert_ne!(a, 0);
        assert_ne!(a, b);
    }

    #[test]
    fn a_sine_carries_its_phase_across_blocks() {
        let mut whole = [0.0f32; 128];
        let _ = fill_sine(&mut whole, 100.0, 48_000.0, 0.0);

        let mut first = [0.0f32; 64];
        let mut second = [0.0f32; 64];
        let phase = fill_sine(&mut first, 100.0, 48_000.0, 0.0);
        let _ = fill_sine(&mut second, 100.0, 48_000.0, phase);

        for i in 0..64 {
            assert!((whole[i] - first[i]).abs() < 1e-6, "frame {i}");
            assert!((whole[64 + i] - second[i]).abs() < 1e-6, "frame {}", 64 + i);
        }
    }

    #[test]
    fn a_ramp_hits_both_ends_and_survives_degenerate_lengths() {
        let mut out = [0.0f32; 5];
        fill_ramp(&mut out, -1.0, 1.0);
        assert_eq!(out[0], -1.0);
        assert_eq!(out[4], 1.0);
        assert!((out[2] - 0.0).abs() < 1e-6);

        let mut one = [9.0f32; 1];
        fill_ramp(&mut one, 0.5, 1.0);
        assert_eq!(one[0], 0.5);

        let mut none: [f32; 0] = [];
        fill_ramp(&mut none, 0.0, 1.0);
    }

    #[test]
    fn an_impulse_past_the_end_leaves_silence_rather_than_panicking() {
        let mut out = [1.0f32; 8];
        fill_impulse(&mut out, 99);
        assert!(out.iter().all(|s| *s == 0.0));

        fill_impulse(&mut out, 3);
        assert_eq!(out[3], 1.0);
        assert_eq!(out.iter().filter(|s| **s != 0.0).count(), 1);
    }

    #[test]
    fn generation_allocates_nothing() {
        let mut buffer = [0.0f32; 256];
        let mut rng = Rng::new(1);
        let ((), allocations) = daux_rt::AllocGuard::scope(|| {
            fill_noise(&mut buffer, &mut rng);
            let _ = fill_sine(&mut buffer, 440.0, 48_000.0, 0.0);
            fill_ramp(&mut buffer, 0.0, 1.0);
            fill_impulse(&mut buffer, 0);
        });
        assert_eq!(allocations, 0);
    }
}
