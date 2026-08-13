//! Continuous floating-point parameter.

use daux_rt::AtomicF64;

use crate::{
    FormatFn, Param, ParamFlags, ParamId, ParamInfo, ParamRange, ParseFn, Smoother, Smoothing, text,
};

/// A continuous, real-valued parameter: gain, frequency, mix, time.
///
/// The value lives in a [`daux_rt::AtomicF64`], so `&FloatParam` is [`Sync`] and one
/// `Arc<Params>` can be shared by the processor, the controller and the editor with no
/// lock anywhere. Reads and writes are single relaxed atomic operations and are safe
/// from any thread.
///
/// Construction is builder-style and happens once, on the main thread:
///
/// ```
/// use daux_parameter::{FloatParam, Param, ParamFlags, ParamId, ParamRange, Smoothing};
///
/// let cutoff = FloatParam::new(
///         ParamId::from_name("cutoff"),
///         "Cutoff",
///         1_000.0,
///         ParamRange::logarithmic(20.0, 20_000.0),
///     )
///     .with_unit("Hz")
///     .with_decimals(1)
///     .with_group("Filter")
///     .with_smoothing(Smoothing::Exponential { ms: 20.0 })
///     .with_flags(ParamFlags::AUTOMATABLE | ParamFlags::MODULATABLE);
///
/// cutoff.set_normalized(0.5);
/// assert!((cutoff.value() - 632.455_532_033_675_9).abs() < 1e-9);
/// assert_eq!(cutoff.text(cutoff.value()), "632.5 Hz");
/// ```
pub struct FloatParam {
    id: ParamId,
    name: String,
    group: String,
    unit: String,
    range: ParamRange,
    default: f64,
    flags: ParamFlags,
    decimals: u8,
    smoothing: Smoothing,
    formatter: Option<FormatFn>,
    parser: Option<ParseFn>,
    value: AtomicF64,
}

impl FloatParam {
    /// `[main-thread]` Builds a parameter with `default` as both its initial and its
    /// reset value.
    ///
    /// # Panics
    ///
    /// Panics if `range` is unusable — most importantly a
    /// [`ParamRange::Logarithmic`] whose bounds are not strictly positive. Parameters
    /// are built once, on the main thread, while the plug-in is constructed: failing
    /// loudly there turns a silently dead control into an error the author sees on
    /// their first run. Validate with [`ParamRange::validate`] first if the bounds come
    /// from data rather than from source code.
    #[must_use]
    pub fn new(
        id: impl Into<ParamId>,
        name: impl Into<String>,
        default: f64,
        range: ParamRange,
    ) -> Self {
        let name = name.into();
        if let Err(err) = range.validate() {
            panic!("FloatParam \"{name}\" cannot use {range:?}: {err}");
        }
        let default = range.clamp(default);
        Self {
            id: id.into(),
            name,
            group: String::new(),
            unit: String::new(),
            range,
            default,
            flags: ParamFlags::DEFAULT,
            decimals: 2,
            smoothing: Smoothing::None,
            formatter: None,
            parser: None,
            value: AtomicF64::new(default),
        }
    }

    /// `[main-thread]` Sets the unit suffix used by `to_text`, e.g. `"dB"`.
    #[must_use]
    pub fn with_unit(mut self, unit: impl Into<String>) -> Self {
        self.unit = unit.into();
        self
    }

    /// `[main-thread]` Sets the `/`-separated group path, e.g. `"Filter/Env"`.
    #[must_use]
    pub fn with_group(mut self, group: impl Into<String>) -> Self {
        self.group = group.into();
        self
    }

    /// `[main-thread]` Replaces the flags. `AUTOMATABLE` is the default, and `STEPPED`
    /// is added automatically when the range is discrete.
    #[must_use]
    pub fn with_flags(mut self, flags: ParamFlags) -> Self {
        self.flags = flags;
        self
    }

    /// `[main-thread]` Sets how many fraction digits `to_text` writes (default `2`).
    #[must_use]
    pub fn with_decimals(mut self, decimals: u8) -> Self {
        self.decimals = decimals;
        self
    }

    /// `[main-thread]` Records how a processor should ramp this parameter.
    ///
    /// The parameter itself never smooths — it is a shared atomic cell with no notion
    /// of a sample rate. The processor asks for a [`Smoother`] with
    /// [`smoother`](Self::smoother) in `prepare` and drives it per sample.
    #[must_use]
    pub fn with_smoothing(mut self, smoothing: Smoothing) -> Self {
        self.smoothing = smoothing;
        self
    }

    /// `[main-thread]` Overrides value formatting, e.g. to print `"Off"` at `-inf` or
    /// to switch between `Hz` and `kHz`.
    ///
    /// The unit suffix is *not* appended afterwards; a custom formatter owns the whole
    /// string.
    #[must_use]
    pub fn with_formatter(mut self, formatter: FormatFn) -> Self {
        self.formatter = Some(formatter);
        self
    }

    /// `[main-thread]` Overrides text parsing. The result is still clamped to the
    /// range.
    #[must_use]
    pub fn with_parser(mut self, parser: ParseFn) -> Self {
        self.parser = Some(parser);
        self
    }

    /// `[any-thread]` Current value in real-world units.
    #[inline]
    #[must_use]
    pub fn value(&self) -> f64 {
        self.value.get()
    }

    /// `[any-thread]` Current value as `f32`, the form most DSP wants.
    #[inline]
    #[must_use]
    pub fn value_f32(&self) -> f32 {
        self.value.get() as f32
    }

    /// `[any-thread]` Stores a real-world value, clamped to the range.
    #[inline]
    pub fn set(&self, v: f64) {
        self.value.set(self.range.clamp(v));
    }

    /// `[any-thread]` The parameter's permanent id.
    #[inline]
    #[must_use]
    pub fn id(&self) -> ParamId {
        self.id
    }

    /// `[any-thread]` The value curve.
    #[inline]
    #[must_use]
    pub fn range(&self) -> ParamRange {
        self.range
    }

    /// `[any-thread]` The reset value.
    #[inline]
    #[must_use]
    pub fn default_value(&self) -> f64 {
        self.default
    }

    /// `[any-thread]` The configured smoothing.
    #[inline]
    #[must_use]
    pub fn smoothing(&self) -> Smoothing {
        self.smoothing
    }

    /// `[any-thread]` Display name.
    #[inline]
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// `[any-thread]` Unit suffix, `""` when there is none.
    #[inline]
    #[must_use]
    pub fn unit(&self) -> &str {
        &self.unit
    }

    /// `[main-thread]` Builds a [`Smoother`] configured from
    /// [`with_smoothing`](Self::with_smoothing) and seeded with the current value.
    ///
    /// Call this in `prepare`, keep the smoother in the processor, and feed it
    /// `set_target(param.value_f32())` once per block.
    #[must_use]
    pub fn smoother(&self) -> Smoother {
        let mut smoother = Smoother::new(self.smoothing);
        smoother.reset_to(self.value_f32());
        smoother
    }
}

impl core::fmt::Debug for FloatParam {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("FloatParam")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("unit", &self.unit)
            .field("range", &self.range)
            .field("default", &self.default)
            .field("flags", &self.flags)
            .field("value", &self.value.get())
            .finish()
    }
}

impl Param for FloatParam {
    fn info(&self) -> ParamInfo {
        ParamInfo::new(
            self.id,
            self.name.clone(),
            &self.range,
            self.default,
            self.flags,
        )
        .with_group(self.group.clone())
        .with_unit(self.unit.clone())
    }

    #[inline]
    fn plain(&self) -> f64 {
        self.value.get()
    }

    #[inline]
    fn set_plain(&self, v: f64) {
        self.set(v);
    }

    #[inline]
    fn normalized(&self) -> f64 {
        self.range.normalize(self.value.get())
    }

    #[inline]
    fn set_normalized(&self, v: f64) {
        self.value.set(self.range.denormalize(v));
    }

    fn to_text(&self, plain: f64, out: &mut String) {
        match self.formatter {
            Some(f) => {
                out.clear();
                f(plain, out);
            }
            None => text::format_value(plain, self.decimals, &self.unit, out),
        }
    }

    fn from_text(&self, text: &str) -> Option<f64> {
        let parsed = match self.parser {
            Some(p) => p(text)?,
            None => text::parse_value(text)?,
        };
        Some(self.range.clamp(parsed))
    }

    #[inline]
    fn reset(&self) {
        self.value.set(self.default);
    }

    #[inline]
    fn id(&self) -> ParamId {
        self.id
    }

    #[inline]
    fn flags(&self) -> ParamFlags {
        if self.range.is_stepped() {
            self.flags.with(ParamFlags::STEPPED)
        } else {
            self.flags
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn gain() -> FloatParam {
        FloatParam::new(ParamId(1), "Gain", 0.0, ParamRange::linear(-60.0, 12.0))
            .with_unit("dB")
            .with_decimals(1)
            .with_smoothing(Smoothing::Exponential { ms: 20.0 })
    }

    #[test]
    fn builder_fills_in_the_info() {
        let p = gain()
            .with_group("Output")
            .with_flags(ParamFlags::AUTOMATABLE | ParamFlags::MODULATABLE);
        let info = p.info();
        assert_eq!(info.id, ParamId(1));
        assert_eq!(info.name, "Gain");
        assert_eq!(info.group, "Output");
        assert_eq!(info.unit, "dB");
        assert_eq!(info.min, -60.0);
        assert_eq!(info.max, 12.0);
        assert_eq!(info.default, 0.0);
        assert_eq!(info.step_count, 0);
        assert!(info.flags.contains(ParamFlags::MODULATABLE));
        assert_eq!(p.smoothing(), Smoothing::Exponential { ms: 20.0 });
        assert_eq!(p.name(), "Gain");
        assert_eq!(p.unit(), "dB");
        assert_eq!(p.default_value(), 0.0);
        assert_eq!(p.range(), ParamRange::linear(-60.0, 12.0));
    }

    #[test]
    fn values_are_clamped_on_the_way_in() {
        let p = gain();
        p.set_plain(100.0);
        assert_eq!(p.plain(), 12.0);
        p.set_plain(-1000.0);
        assert_eq!(p.plain(), -60.0);
        p.set_plain(f64::NAN);
        assert_eq!(p.plain(), -60.0);
        p.set(-6.0);
        assert_eq!(p.value(), -6.0);
        assert!((p.value_f32() - (-6.0f32)).abs() < 1e-6);
    }

    #[test]
    fn normalised_access_round_trips() {
        let p = gain();
        for n in [0.0, 0.25, 0.5, 0.75, 1.0] {
            p.set_normalized(n);
            assert!((p.normalized() - n).abs() < 1e-9, "at {n}");
        }
        p.set_normalized(0.0);
        assert_eq!(p.plain(), -60.0);
        p.set_normalized(1.0);
        assert_eq!(p.plain(), 12.0);
        // Out-of-range normalised input is clamped rather than wrapped.
        p.set_normalized(-1.0);
        assert_eq!(p.plain(), -60.0);
        p.set_normalized(2.0);
        assert_eq!(p.plain(), 12.0);
    }

    #[test]
    fn text_round_trips_through_the_ui() {
        let p = gain();
        let mut buffer = String::new();
        p.to_text(-6.0, &mut buffer);
        assert_eq!(buffer, "-6.0 dB");
        assert_eq!(p.from_text(&buffer), Some(-6.0));
        assert_eq!(p.from_text("  -6.0 dB "), Some(-6.0));
        assert_eq!(p.from_text("6dB"), Some(6.0));
        assert_eq!(p.from_text("+3"), Some(3.0));
        assert_eq!(p.from_text("1e1"), Some(10.0));
        // Parsed values are clamped to the range like any other write.
        assert_eq!(p.from_text("999 dB"), Some(12.0));
        assert_eq!(p.from_text("nonsense"), None);
        assert_eq!(p.from_text(""), None);
    }

    #[test]
    fn to_text_reuses_the_callers_buffer() {
        let p = gain();
        let mut buffer = String::with_capacity(64);
        let capacity = buffer.capacity();
        for value in [-60.0, -6.0, 0.0, 12.0] {
            p.to_text(value, &mut buffer);
            assert!(buffer.ends_with(" dB"));
        }
        assert_eq!(
            buffer.capacity(),
            capacity,
            "formatting must not reallocate"
        );
        assert_eq!(buffer, "12.0 dB");
    }

    #[test]
    fn custom_formatter_and_parser_take_over() {
        fn to_percent(plain: f64, out: &mut String) {
            out.clear();
            out.push_str(if plain >= 0.5 { "hot" } else { "cold" });
        }
        fn from_words(text: &str) -> Option<f64> {
            match text.trim() {
                "hot" => Some(1.0),
                "cold" => Some(0.0),
                _ => None,
            }
        }

        let p = FloatParam::new(ParamId(9), "Heat", 0.0, ParamRange::UNIT)
            .with_formatter(to_percent)
            .with_parser(from_words);

        let mut s = String::from("junk");
        p.to_text(0.9, &mut s);
        assert_eq!(s, "hot");
        p.to_text(0.1, &mut s);
        assert_eq!(s, "cold");
        assert_eq!(p.from_text("hot"), Some(1.0));
        assert_eq!(p.from_text(" cold "), Some(0.0));
        assert_eq!(p.from_text("7"), None);
    }

    #[test]
    fn reset_restores_the_default() {
        let p = gain();
        p.set_plain(-24.0);
        assert_eq!(p.plain(), -24.0);
        p.reset();
        assert_eq!(p.plain(), 0.0);
    }

    #[test]
    fn default_is_clamped_into_the_range() {
        let p = FloatParam::new(ParamId(1), "Odd", 500.0, ParamRange::linear(0.0, 10.0));
        assert_eq!(p.plain(), 10.0);
        assert_eq!(p.default_value(), 10.0);
        assert_eq!(p.info().default, 10.0);
    }

    #[test]
    fn stepped_ranges_add_the_flag_and_quantise() {
        let p = FloatParam::new(ParamId(1), "Steps", 0.0, ParamRange::stepped(0, 4));
        assert!(p.flags().is_stepped());
        assert!(p.info().flags.is_stepped());
        assert_eq!(p.info().step_count, 4);
        p.set_plain(2.4);
        assert_eq!(p.plain(), 2.0);
        p.set_normalized(0.6);
        assert_eq!(p.plain(), 2.0);
    }

    #[test]
    fn logarithmic_parameters_behave() {
        let p = FloatParam::new(
            ParamId(1),
            "Cutoff",
            1000.0,
            ParamRange::logarithmic(20.0, 20_000.0),
        )
        .with_unit("Hz")
        .with_decimals(0);

        // A plain value survives the trip through the curve untouched, which is the
        // whole reason automation is stored in plain units.
        let n = p.normalized();
        assert!((p.range().denormalize(n) - 1000.0).abs() < 1e-9);
        // Half the travel is the geometric mean of the bounds.
        p.set_normalized(0.5);
        assert!((p.value() - 632.4555320336759).abs() < 1e-9);
        assert_eq!(p.text(p.value()), "632 Hz");
    }

    #[test]
    #[should_panic(expected = "logarithmic parameter range needs strictly positive bounds")]
    fn rejects_an_impossible_logarithmic_range_at_construction() {
        let _ = FloatParam::new(
            ParamId(1),
            "Cutoff",
            1.0,
            ParamRange::Logarithmic {
                min: 0.0,
                max: 20_000.0,
            },
        );
    }

    #[test]
    fn smoother_starts_at_the_current_value() {
        let p = gain();
        p.set_plain(-12.0);
        let mut smoother = p.smoother();
        smoother.prepare(48_000.0);
        assert!((smoother.next() - (-12.0)).abs() < 1e-6);
        assert!(!smoother.is_smoothing());
    }

    #[test]
    fn a_write_from_another_thread_is_visible_here() {
        // This is the property the whole design rests on: the UI thread stores a plain
        // value and the audio thread sees it on its next block, with no lock and no
        // message queue.
        let p = Arc::new(gain());
        let stop = Arc::new(AtomicBool::new(false));

        let writer = {
            let p = Arc::clone(&p);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                for i in 0..1_000 {
                    p.set_plain(f64::from(i % 13) - 6.0);
                }
                p.set_plain(-42.0);
                stop.store(true, Ordering::Release);
            })
        };

        // Stand in for the audio thread: keep reading, never block, and check that the
        // value is always one the writer actually stored.
        let mut observed_final = false;
        for _ in 0..10_000_000 {
            let v = p.plain();
            assert!(
                (-60.0..=12.0).contains(&v),
                "torn or out-of-range value {v}"
            );
            if stop.load(Ordering::Acquire) && p.plain() == -42.0 {
                observed_final = true;
                break;
            }
        }
        writer.join().expect("writer thread");
        assert!(observed_final || p.plain() == -42.0);
        assert_eq!(p.plain(), -42.0);
    }

    #[test]
    fn is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<FloatParam>();
    }
}
