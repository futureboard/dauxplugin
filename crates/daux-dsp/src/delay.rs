//! A power-of-two circular delay line with fractional reads.

/// Largest buffer [`DelayLine::new`] will allocate, in samples.
///
/// `2^28` samples is 93 minutes at 48 kHz and 1 GiB of `f32`. Clamping here
/// rather than trusting the caller keeps `new` free of overflow arithmetic and
/// turns an absurd request into a large-but-finite buffer instead of a panic
/// inside `next_power_of_two`.
const MAX_CAPACITY: usize = 1 << 28;

/// Buffer length for a requested maximum delay: the next power of two at or
/// above `max_samples + 1`, clamped into `2 ..= MAX_CAPACITY`.
///
/// Split out from [`DelayLine::new`] so the clamping can be tested without
/// allocating a gigabyte.
const fn capacity_for(max_samples: usize) -> usize {
    let wanted = max_samples.saturating_add(1);
    let bounded = if wanted < 2 {
        2
    } else if wanted > MAX_CAPACITY {
        MAX_CAPACITY
    } else {
        wanted
    };
    bounded.next_power_of_two()
}

/// A circular buffer of `f32` with a power-of-two capacity, read at fractional
/// delays by linear interpolation.
///
/// The power-of-two capacity is the whole trick: wrapping is a bitwise `AND`
/// rather than a modulo or a compare-and-branch, so reading and writing cost a
/// couple of instructions and behave identically no matter where the head sits.
/// It is also why every index expression below is provably in bounds — `i &
/// mask <= mask == len - 1` — which is what makes the safe indexing here free
/// of any reachable panic.
///
/// Interpolation is linear: two taps, one multiply-add. Linear interpolation
/// low-passes as the fractional part approaches 0.5 (about −3 dB at Nyquist),
/// which is inaudible for chorus and delay modulation and is the standard
/// trade-off; if you need a flat response for pitch-shifting, oversample or
/// build a higher-order interpolator on top of
/// [`read_int`](DelayLine::read_int).
///
/// ```
/// # use daux_dsp::DelayLine;
/// let mut line = DelayLine::new(8);
/// for x in [1.0_f32, 2.0, 3.0, 4.0] {
///     line.write(x);
/// }
/// assert_eq!(line.read(0.0), 4.0); // the sample just written
/// assert_eq!(line.read(1.0), 3.0);
/// assert_eq!(line.read(1.5), 2.5); // halfway between 3.0 and 2.0
/// ```
#[derive(Clone, Debug)]
pub struct DelayLine {
    /// Sample storage; `buf.len()` is always a power of two and at least 2.
    buf: Vec<f32>,
    /// `buf.len() - 1`, the wrap mask.
    mask: usize,
    /// Index the *next* [`write`](DelayLine::write) will fill.
    write: usize,
}

impl DelayLine {
    /// Largest delay, in samples, that any `DelayLine` can be asked for.
    ///
    /// [any-thread]
    pub const MAX_DELAY_SAMPLES: usize = MAX_CAPACITY - 1;

    /// Allocates a line that can serve delays of up to `max_samples`.
    /// [main-thread]
    ///
    /// **This allocates.** Call it from `prepare`, never from `process`.
    ///
    /// The buffer is rounded up to a power of two of at least `max_samples + 1`
    /// slots, so the requested delay is always reachable — asking for 1024
    /// really does give you a delay of 1024, not 1023.
    ///
    /// Requests above [`MAX_DELAY_SAMPLES`](Self::MAX_DELAY_SAMPLES) are
    /// clamped rather than refused; check
    /// [`max_delay`](DelayLine::max_delay) if the exact size matters.
    #[must_use]
    pub fn new(max_samples: usize) -> Self {
        let len = capacity_for(max_samples);
        Self {
            buf: vec![0.0; len],
            mask: len - 1,
            write: 0,
        }
    }

    /// Number of slots in the buffer — a power of two, at least 2.
    /// [any-thread]
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    /// Largest delay this line can serve, in samples: `capacity() - 1`.
    /// [any-thread]
    #[must_use]
    pub const fn max_delay(&self) -> usize {
        self.mask
    }

    /// Pushes one sample in and advances the head. [audio-thread]
    ///
    /// After this call `read(0.0)` returns `x`.
    #[inline]
    pub fn write(&mut self, x: f32) {
        let i = self.write;
        self.buf[i] = x;
        self.write = (i + 1) & self.mask;
    }

    /// Reads at a fractional delay using linear interpolation. [audio-thread]
    ///
    /// `delay` is in samples: `0.0` is the most recently written sample. Values
    /// outside `0.0 ..= max_delay()` are clamped, and `NaN` clamps to `0.0` —
    /// `f32::max` returns its other operand when one side is `NaN`, so a bad
    /// modulation source degrades to a dry read instead of an out-of-range
    /// index or a `NaN` in the output.
    #[inline]
    #[must_use]
    pub fn read(&self, delay: f32) -> f32 {
        let mask = self.mask;
        // `.max` first so NaN collapses to 0.0, then `.min` to bound it.
        let clamped = delay.max(0.0).min(mask as f32);
        // `mask as f32` rounds *up* for masks above 2^24, so bound the integer
        // part again rather than trusting the float compare.
        let whole = (clamped as usize).min(mask);
        let frac = clamped - whole as f32;

        let head = self.write.wrapping_sub(1) & mask;
        let a = self.buf[head.wrapping_sub(whole) & mask];
        let b = self.buf[head.wrapping_sub(whole + 1) & mask];
        a + (b - a) * frac
    }

    /// Reads at a whole-sample delay, with no interpolation. [audio-thread]
    ///
    /// `delay` is clamped to [`max_delay`](DelayLine::max_delay).
    #[inline]
    #[must_use]
    pub fn read_int(&self, delay: usize) -> f32 {
        let mask = self.mask;
        let head = self.write.wrapping_sub(1) & mask;
        self.buf[head.wrapping_sub(delay.min(mask)) & mask]
    }

    /// Writes `x`, then reads at `delay` — the usual one-line delay tap.
    /// [audio-thread]
    ///
    /// Because the write happens first, `delay == 0.0` returns `x` itself.
    #[inline]
    pub fn process(&mut self, x: f32, delay: f32) -> f32 {
        self.write(x);
        self.read(delay)
    }

    /// Zeroes the buffer and rewinds the head. [audio-thread]
    ///
    /// Allocation-free and lock-free, but it touches every slot, so it is
    /// `O(capacity)`: this belongs in `reset`, not in the middle of a block.
    pub fn clear(&mut self) {
        self.buf.fill(0.0);
        self.write = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fills a line with `0.0, 1.0, 2.0, …` so that `read_int(d) == n - 1 - d`.
    fn filled(capacity_request: usize, count: usize) -> DelayLine {
        let mut line = DelayLine::new(capacity_request);
        for i in 0..count {
            line.write(i as f32);
        }
        line
    }

    #[test]
    fn capacity_is_a_power_of_two_that_covers_the_request() {
        for &request in &[
            0_usize, 1, 2, 3, 4, 5, 7, 8, 100, 1_000, 1_024, 1_025, 48_000,
        ] {
            let line = DelayLine::new(request);
            assert!(line.capacity().is_power_of_two(), "request {request}");
            assert!(line.capacity() >= 2, "request {request}");
            assert!(
                line.max_delay() >= request,
                "request {request} gave {}",
                line.max_delay()
            );
        }
    }

    #[test]
    fn sizing_is_clamped_at_both_ends_without_overflowing() {
        // Checked through `capacity_for` so the huge cases cost no memory.
        assert_eq!(capacity_for(0), 2);
        assert_eq!(capacity_for(1), 2);
        assert_eq!(capacity_for(2), 4);
        assert_eq!(capacity_for(3), 4);
        assert_eq!(capacity_for(1_023), 1_024);
        assert_eq!(capacity_for(1_024), 2_048);
        assert_eq!(capacity_for(MAX_CAPACITY - 1), MAX_CAPACITY);
        assert_eq!(capacity_for(MAX_CAPACITY), MAX_CAPACITY);
        assert_eq!(capacity_for(usize::MAX), MAX_CAPACITY);
        assert_eq!(DelayLine::MAX_DELAY_SAMPLES, MAX_CAPACITY - 1);
    }

    #[test]
    fn a_fresh_line_reads_silence() {
        let line = DelayLine::new(16);
        for d in 0..=line.max_delay() {
            assert_eq!(line.read_int(d), 0.0);
            assert_eq!(line.read(d as f32), 0.0);
        }
    }

    #[test]
    fn zero_delay_returns_the_last_written_sample() {
        let mut line = DelayLine::new(16);
        for x in [0.25_f32, -0.5, 0.75] {
            line.write(x);
            assert_eq!(line.read(0.0), x);
            assert_eq!(line.read_int(0), x);
        }
    }

    #[test]
    fn integer_delays_walk_back_through_history() {
        // Exactly `capacity` samples written, so every slot holds known data.
        let line = filled(31, 32);
        assert_eq!(line.capacity(), 32);
        for d in 0..=line.max_delay() {
            let expected = 31.0 - d as f32;
            assert_eq!(line.read_int(d), expected, "delay {d}");
            assert_eq!(line.read(d as f32), expected, "delay {d}");
        }
    }

    #[test]
    fn fractional_reads_interpolate_linearly() {
        // Values are 0,1,2,… so a delay of d must read exactly `count-1-d`,
        // fractional part included: the ramp makes interpolation exact.
        let line = filled(64, 64);
        let mut d = 0.0_f32;
        while d <= 60.0 {
            let expected = 63.0 - d;
            let got = line.read(d);
            assert!(
                (got - expected).abs() < 1.0e-4,
                "delay {d}: got {got}, want {expected}"
            );
            d += 0.125;
        }
    }

    #[test]
    fn fractional_read_is_the_exact_convex_combination() {
        let mut line = DelayLine::new(8);
        for x in [10.0_f32, 20.0, 30.0, 40.0] {
            line.write(x);
        }
        assert_eq!(line.read(0.0), 40.0);
        assert_eq!(line.read(0.25), 37.5);
        assert_eq!(line.read(0.5), 35.0);
        assert_eq!(line.read(0.75), 32.5);
        assert_eq!(line.read(1.0), 30.0);
        assert_eq!(line.read(2.5), 15.0);
    }

    #[test]
    fn interpolation_reconstructs_a_smooth_signal_accurately() {
        // Linear interpolation of a band-limited signal is accurate to
        // O(h^2 * f^2); at 48 samples per cycle the worst-case error is tiny.
        let mut line = DelayLine::new(256);
        let w = core::f64::consts::TAU / 48.0;
        for n in 0..256 {
            line.write((w * f64::from(n)).sin() as f32);
        }
        let last = 255.0_f64;
        let mut d = 0.0_f64;
        while d <= 200.0 {
            let expected = (w * (last - d)).sin();
            let got = f64::from(line.read(d as f32));
            assert!(
                (got - expected).abs() < 3.0e-3,
                "delay {d}: got {got}, want {expected}"
            );
            d += 0.37;
        }
    }

    #[test]
    fn the_head_wraps_without_losing_alignment() {
        // Write far more than the capacity; the newest `capacity` samples must
        // still be readable and correctly ordered.
        let line = filled(8, 1_000);
        for d in 0..=line.max_delay() {
            assert_eq!(line.read_int(d), 999.0 - d as f32, "delay {d}");
        }
    }

    #[test]
    fn max_delay_is_reachable_and_stops_there() {
        let line = filled(31, 32);
        let max = line.max_delay();
        assert_eq!(line.read_int(max), 31.0 - max as f32);
        // Anything beyond saturates rather than wrapping into fresh samples.
        assert_eq!(line.read_int(max + 1), line.read_int(max));
        assert_eq!(line.read_int(usize::MAX), line.read_int(max));
        assert_eq!(line.read(max as f32 + 100.0), line.read(max as f32));
    }

    #[test]
    fn negative_and_nan_delays_clamp_to_the_dry_sample() {
        let mut line = DelayLine::new(16);
        line.write(0.5);
        assert_eq!(line.read(-1.0), 0.5);
        assert_eq!(line.read(-1.0e30), 0.5);
        assert_eq!(line.read(f32::NAN), 0.5);
        assert_eq!(line.read(f32::NEG_INFINITY), 0.5);
        assert_eq!(line.read(f32::INFINITY), line.read(line.max_delay() as f32));
    }

    #[test]
    fn capacity_one_request_still_yields_a_usable_line() {
        // The smallest line the constructor will build: two slots.
        let mut line = DelayLine::new(0);
        assert_eq!(line.capacity(), 2);
        assert_eq!(line.max_delay(), 1);
        line.write(1.0);
        line.write(2.0);
        assert_eq!(line.read(0.0), 2.0);
        assert_eq!(line.read(1.0), 1.0);
        assert_eq!(line.read(0.5), 1.5);
        // Delay 1 is the oldest slot; asking for more must not wrap forward.
        assert_eq!(line.read(5.0), 1.0);
    }

    #[test]
    fn process_writes_then_reads() {
        let mut line = DelayLine::new(8);
        assert_eq!(line.process(1.0, 0.0), 1.0);
        assert_eq!(line.process(2.0, 1.0), 1.0);
        assert_eq!(line.process(3.0, 2.0), 1.0);
    }

    #[test]
    fn a_delay_of_n_reproduces_the_input_n_samples_later() {
        const N: usize = 37;
        let mut line = DelayLine::new(N);
        let input: Vec<f32> = (0..500).map(|i| (i as f32 * 0.19).sin()).collect();
        let mut output = Vec::with_capacity(input.len());
        for &x in &input {
            output.push(line.process(x, N as f32));
        }
        for i in N..input.len() {
            assert_eq!(output[i], input[i - N], "sample {i}");
        }
        // The first N outputs come from the zero-filled buffer.
        assert!(output[..N].iter().all(|&y| y == 0.0));
    }

    #[test]
    fn clear_zeroes_the_buffer_and_rewinds() {
        let mut line = filled(16, 100);
        line.clear();
        for d in 0..=line.max_delay() {
            assert_eq!(line.read_int(d), 0.0);
        }
        line.write(3.0);
        assert_eq!(line.read(0.0), 3.0);
    }

    #[test]
    fn reads_never_produce_non_finite_values_from_finite_input() {
        let mut line = DelayLine::new(64);
        for i in 0..1_000 {
            let y = line.process((i as f32 * 0.3).cos(), (i % 200) as f32 * 0.5);
            assert!(y.is_finite());
        }
    }
}
