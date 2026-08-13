//! Discrete integer parameter.

use daux_rt::AtomicF64;

use crate::{FormatFn, Param, ParamFlags, ParamId, ParamInfo, ParamRange, ParseFn, text};

/// A parameter with whole-number values: voice count, semitones, subdivision index.
///
/// The value is stored as an `f64` in a [`daux_rt::AtomicF64`] — that is the unit the
/// ABI, automation lanes and saved state all use — but it is always exactly an integer,
/// because every write goes through [`ParamRange::clamp`], which rounds.
///
/// ```
/// use daux_parameter::{IntParam, Param, ParamId};
///
/// let transpose = IntParam::new(ParamId(4), "Transpose", 0, -24, 24).with_unit("st");
/// transpose.set_plain(7.4);
/// assert_eq!(transpose.value(), 7);
/// assert_eq!(transpose.text(transpose.plain()), "7 st");
/// assert_eq!(transpose.from_text("-12 st"), Some(-12.0));
/// ```
pub struct IntParam {
    id: ParamId,
    name: String,
    group: String,
    unit: String,
    range: ParamRange,
    default: i64,
    flags: ParamFlags,
    formatter: Option<FormatFn>,
    parser: Option<ParseFn>,
    value: AtomicF64,
}

impl IntParam {
    /// `[main-thread]` Builds a parameter over `min..=max` inclusive.
    ///
    /// # Panics
    ///
    /// Panics if `min > max`. Like the other constructors this runs once at build time
    /// on the main thread, where a loud failure is far better than a control that
    /// silently does nothing in a user's session.
    #[must_use]
    pub fn new(
        id: impl Into<ParamId>,
        name: impl Into<String>,
        default: i64,
        min: i64,
        max: i64,
    ) -> Self {
        let name = name.into();
        let range = match ParamRange::try_stepped(min, max) {
            Ok(range) => range,
            Err(err) => panic!("IntParam \"{name}\" cannot use {min}..={max}: {err}"),
        };
        let default = default.clamp(min, max);
        Self {
            id: id.into(),
            name,
            group: String::new(),
            unit: String::new(),
            range,
            default,
            flags: ParamFlags::DEFAULT | ParamFlags::STEPPED,
            formatter: None,
            parser: None,
            value: AtomicF64::new(default as f64),
        }
    }

    /// `[main-thread]` Sets the unit suffix, e.g. `"st"` or `"voices"`.
    #[must_use]
    pub fn with_unit(mut self, unit: impl Into<String>) -> Self {
        self.unit = unit.into();
        self
    }

    /// `[main-thread]` Sets the `/`-separated group path.
    #[must_use]
    pub fn with_group(mut self, group: impl Into<String>) -> Self {
        self.group = group.into();
        self
    }

    /// `[main-thread]` Replaces the flags; `STEPPED` is re-added automatically.
    #[must_use]
    pub fn with_flags(mut self, flags: ParamFlags) -> Self {
        self.flags = flags.with(ParamFlags::STEPPED);
        self
    }

    /// `[main-thread]` Overrides value formatting, e.g. to print note names.
    #[must_use]
    pub fn with_formatter(mut self, formatter: FormatFn) -> Self {
        self.formatter = Some(formatter);
        self
    }

    /// `[main-thread]` Overrides text parsing. The result is still rounded and clamped.
    #[must_use]
    pub fn with_parser(mut self, parser: ParseFn) -> Self {
        self.parser = Some(parser);
        self
    }

    /// `[any-thread]` Current value.
    #[inline]
    #[must_use]
    pub fn value(&self) -> i64 {
        self.value.get() as i64
    }

    /// `[any-thread]` Stores a value, clamped to the range.
    #[inline]
    pub fn set(&self, v: i64) {
        self.value.set(self.range.clamp(v as f64));
    }

    /// `[any-thread]` The parameter's permanent id.
    #[inline]
    #[must_use]
    pub fn id(&self) -> ParamId {
        self.id
    }

    /// `[any-thread]` The value range.
    #[inline]
    #[must_use]
    pub fn range(&self) -> ParamRange {
        self.range
    }

    /// `[any-thread]` The reset value.
    #[inline]
    #[must_use]
    pub fn default_value(&self) -> i64 {
        self.default
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
}

impl core::fmt::Debug for IntParam {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("IntParam")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("unit", &self.unit)
            .field("range", &self.range)
            .field("default", &self.default)
            .field("flags", &self.flags)
            .field("value", &self.value())
            .finish()
    }
}

impl Param for IntParam {
    fn info(&self) -> ParamInfo {
        ParamInfo::new(
            self.id,
            self.name.clone(),
            &self.range,
            self.default as f64,
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
        self.value.set(self.range.clamp(v));
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
                f(self.range.clamp(plain), out);
            }
            None => text::format_value(self.range.clamp(plain), 0, &self.unit, out),
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
        self.value.set(self.default as f64);
    }

    #[inline]
    fn id(&self) -> ParamId {
        self.id
    }

    #[inline]
    fn flags(&self) -> ParamFlags {
        self.flags
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transpose() -> IntParam {
        IntParam::new(ParamId(4), "Transpose", 0, -24, 24).with_unit("st")
    }

    #[test]
    fn info_describes_a_stepped_parameter() {
        let p = transpose().with_group("Pitch");
        let info = p.info();
        assert_eq!(info.min, -24.0);
        assert_eq!(info.max, 24.0);
        assert_eq!(info.default, 0.0);
        assert_eq!(info.step_count, 48);
        assert_eq!(info.group, "Pitch");
        assert_eq!(info.unit, "st");
        assert!(info.flags.is_stepped());
        assert!(p.flags().is_stepped());
        assert_eq!(p.name(), "Transpose");
        assert_eq!(p.unit(), "st");
        assert_eq!(p.default_value(), 0);
        assert_eq!(p.range(), ParamRange::stepped(-24, 24));
    }

    #[test]
    fn values_are_rounded_and_clamped() {
        let p = transpose();
        p.set(7);
        assert_eq!(p.value(), 7);
        p.set_plain(7.4);
        assert_eq!(p.value(), 7);
        p.set_plain(7.6);
        assert_eq!(p.value(), 8);
        p.set_plain(-7.5);
        assert_eq!(p.value(), -8, "round-half-away-from-zero, like the host");
        p.set(1000);
        assert_eq!(p.value(), 24);
        p.set(-1000);
        assert_eq!(p.value(), -24);
        p.set_plain(f64::NAN);
        assert_eq!(p.value(), -24);
    }

    #[test]
    fn every_step_round_trips_through_normalised() {
        let p = transpose();
        for v in -24..=24 {
            p.set(v);
            let n = p.normalized();
            p.set_normalized(n);
            assert_eq!(p.value(), v, "step {v} did not survive normalisation");
        }
        p.set(-24);
        assert_eq!(p.normalized(), 0.0);
        p.set(24);
        assert_eq!(p.normalized(), 1.0);
        p.set(0);
        assert!((p.normalized() - 0.5).abs() < 1e-12);
    }

    #[test]
    fn a_single_value_range_is_harmless() {
        let p = IntParam::new(ParamId(1), "Fixed", 3, 3, 3);
        assert_eq!(p.value(), 3);
        assert_eq!(p.normalized(), 0.0);
        p.set_normalized(1.0);
        assert_eq!(p.value(), 3);
        p.set(99);
        assert_eq!(p.value(), 3);
        assert_eq!(p.info().step_count, 0);
    }

    #[test]
    fn text_round_trips() {
        let p = transpose();
        let mut buffer = String::new();
        p.to_text(-12.0, &mut buffer);
        assert_eq!(buffer, "-12 st");
        assert_eq!(p.from_text(&buffer), Some(-12.0));
        assert_eq!(p.from_text(" 7 "), Some(7.0));
        assert_eq!(p.from_text("7.6"), Some(8.0));
        assert_eq!(p.from_text("500"), Some(24.0));
        assert_eq!(p.from_text("st"), None);
        // Fractions never leak into the display.
        p.to_text(3.7, &mut buffer);
        assert_eq!(buffer, "4 st");
    }

    #[test]
    fn custom_formatter_sees_a_quantised_value() {
        fn note_name(plain: f64, out: &mut String) {
            const NAMES: [&str; 12] = [
                "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
            ];
            let index = (plain as i64).rem_euclid(12) as usize;
            out.clear();
            out.push_str(NAMES[index]);
        }
        let p = IntParam::new(ParamId(1), "Root", 0, 0, 11).with_formatter(note_name);
        let mut s = String::new();
        p.to_text(0.0, &mut s);
        assert_eq!(s, "C");
        p.to_text(9.4, &mut s);
        assert_eq!(s, "A");
        p.to_text(-5.0, &mut s);
        assert_eq!(s, "C", "out-of-range input is clamped before formatting");
    }

    #[test]
    fn reset_restores_the_default() {
        let p = IntParam::new(ParamId(1), "Voices", 8, 1, 16);
        p.set(1);
        assert_eq!(p.value(), 1);
        p.reset();
        assert_eq!(p.value(), 8);
    }

    #[test]
    fn default_is_clamped() {
        let p = IntParam::new(ParamId(1), "Voices", 99, 1, 16);
        assert_eq!(p.default_value(), 16);
        assert_eq!(p.value(), 16);
    }

    #[test]
    #[should_panic(expected = "stepped parameter range needs min <= max")]
    fn rejects_an_inverted_range() {
        let _ = IntParam::new(ParamId(1), "Broken", 0, 10, 0);
    }

    #[test]
    fn is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<IntParam>();
    }
}
