//! Output meter "parameter".

use daux_rt::AtomicF32;

use crate::{Param, ParamFlags, ParamId, ParamInfo, ParamRange, text};

/// A value the plug-in publishes and the UI polls: level, gain reduction, detected
/// pitch.
///
/// It is modelled as a parameter because that is how hosts and generic UIs already see
/// such values (`READ_ONLY | IS_METER`, `abi-v1` §11.2), but the data flows the other
/// way: the audio thread writes with [`set_value`](MeterParam::set_value) or
/// [`push_peak`](MeterParam::push_peak), and the editor reads whenever it repaints. The
/// value lives in a [`daux_rt::AtomicF32`] — `f32` because that is what DSP produces
/// and because a meter never needs more.
///
/// Writing is a single relaxed atomic store: no lock, no allocation, no drift into the
/// host's automation system.
///
/// ```
/// use daux_parameter::{MeterParam, Param, ParamRange, ParamId};
///
/// let level = MeterParam::new(ParamId(100), "Output", ParamRange::linear(-60.0, 6.0))
///     .with_unit("dB")
///     .with_decimals(1);
///
/// // On the audio thread, once per block:
/// level.push_peak(-12.5);
/// level.push_peak(-18.0);
/// assert_eq!(level.value(), -12.5);
///
/// // On the UI thread, whenever it repaints:
/// assert_eq!(level.text(level.plain()), "-12.5 dB");
/// assert!(level.flags().is_read_only());
/// ```
pub struct MeterParam {
    id: ParamId,
    name: String,
    group: String,
    unit: String,
    range: ParamRange,
    default: f64,
    flags: ParamFlags,
    decimals: u8,
    value: AtomicF32,
}

impl MeterParam {
    /// `[main-thread]` Builds a meter that rests at the bottom of `range`.
    ///
    /// # Panics
    ///
    /// Panics if `range` is unusable, for the same build-time reasons as
    /// [`FloatParam::new`](crate::FloatParam::new).
    #[must_use]
    pub fn new(id: impl Into<ParamId>, name: impl Into<String>, range: ParamRange) -> Self {
        let name = name.into();
        if let Err(err) = range.validate() {
            panic!("MeterParam \"{name}\" cannot use {range:?}: {err}");
        }
        let default = range.bounds().0;
        Self {
            id: id.into(),
            name,
            group: String::new(),
            unit: String::new(),
            range,
            default,
            flags: ParamFlags::METER_DEFAULT,
            decimals: 1,
            value: AtomicF32::new(default as f32),
        }
    }

    /// `[main-thread]` Sets the unit suffix, e.g. `"dB"`.
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

    /// `[main-thread]` Sets how many fraction digits `to_text` writes (default `1`).
    #[must_use]
    pub fn with_decimals(mut self, decimals: u8) -> Self {
        self.decimals = decimals;
        self
    }

    /// `[main-thread]` Replaces the flags; `READ_ONLY | IS_METER` are re-added.
    #[must_use]
    pub fn with_flags(mut self, flags: ParamFlags) -> Self {
        self.flags = flags.with(ParamFlags::METER_DEFAULT);
        self
    }

    /// `[audio-thread]` Publishes a new reading, clamped to the range.
    ///
    /// One relaxed atomic store: no allocation, no lock, no blocking.
    #[inline]
    pub fn set_value(&self, v: f32) {
        self.value.set(self.range.clamp(f64::from(v)) as f32);
    }

    /// `[audio-thread]` Publishes `v` only if it is louder than what is already there.
    ///
    /// This is the usual way to fill a peak meter: call it for each block (or each
    /// sample) and let the UI reset it by reading. `NaN` is ignored rather than
    /// latched.
    #[inline]
    pub fn push_peak(&self, v: f32) {
        if v > self.value.get() {
            self.set_value(v);
        }
    }

    /// `[any-thread]` Current reading.
    #[inline]
    #[must_use]
    pub fn value(&self) -> f32 {
        self.value.get()
    }

    /// `[audio-thread]` Drops the meter back to its resting value.
    #[inline]
    pub fn clear(&self) {
        self.value.set(self.default as f32);
    }

    /// `[any-thread]` The parameter's permanent id.
    #[inline]
    #[must_use]
    pub fn id(&self) -> ParamId {
        self.id
    }

    /// `[any-thread]` The meter's range.
    #[inline]
    #[must_use]
    pub fn range(&self) -> ParamRange {
        self.range
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

impl core::fmt::Debug for MeterParam {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MeterParam")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("unit", &self.unit)
            .field("range", &self.range)
            .field("flags", &self.flags)
            .field("value", &self.value.get())
            .finish()
    }
}

impl Param for MeterParam {
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
        f64::from(self.value.get())
    }

    /// `[any-thread]` Publishes a reading.
    ///
    /// The parameter is `READ_ONLY`, so a host must never call this; it exists so that
    /// the plug-in's own code, `reset` and offline rendering all go through one path.
    #[inline]
    fn set_plain(&self, v: f64) {
        self.value.set(self.range.clamp(v) as f32);
    }

    #[inline]
    fn normalized(&self) -> f64 {
        self.range.normalize(self.plain())
    }

    #[inline]
    fn set_normalized(&self, v: f64) {
        self.value.set(self.range.denormalize(v) as f32);
    }

    fn to_text(&self, plain: f64, out: &mut String) {
        text::format_value(plain, self.decimals, &self.unit, out);
    }

    fn from_text(&self, text: &str) -> Option<f64> {
        text::parse_value(text).map(|v| self.range.clamp(v))
    }

    #[inline]
    fn reset(&self) {
        self.clear();
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
    use std::sync::Arc;

    fn level() -> MeterParam {
        MeterParam::new(ParamId(100), "Output", ParamRange::linear(-60.0, 6.0)).with_unit("dB")
    }

    #[test]
    fn info_marks_it_read_only() {
        let p = level().with_group("Meters");
        let info = p.info();
        assert!(info.flags.is_read_only());
        assert!(info.flags.contains(ParamFlags::IS_METER));
        assert!(!info.flags.is_automatable());
        assert_eq!(info.min, -60.0);
        assert_eq!(info.max, 6.0);
        assert_eq!(info.default, -60.0);
        assert_eq!(info.unit, "dB");
        assert_eq!(info.group, "Meters");
        assert_eq!(p.name(), "Output");
        assert_eq!(p.unit(), "dB");
        assert_eq!(p.range(), ParamRange::linear(-60.0, 6.0));
    }

    #[test]
    fn custom_flags_keep_the_meter_bits() {
        let p = level().with_flags(ParamFlags::HIDDEN);
        assert!(
            p.flags()
                .contains(ParamFlags::HIDDEN | ParamFlags::IS_METER)
        );
        assert!(p.flags().is_read_only());
    }

    #[test]
    fn starts_at_the_bottom_of_the_range() {
        let p = level();
        assert_eq!(p.value(), -60.0);
        assert_eq!(p.plain(), -60.0);
        assert_eq!(p.normalized(), 0.0);
    }

    #[test]
    fn readings_are_clamped() {
        let p = level();
        p.set_value(-12.5);
        assert_eq!(p.value(), -12.5);
        p.set_value(1000.0);
        assert_eq!(p.value(), 6.0);
        p.set_value(-1000.0);
        assert_eq!(p.value(), -60.0);
        p.set_value(f32::NAN);
        assert_eq!(p.value(), -60.0, "NaN must not poison the display");
        p.set_value(f32::NEG_INFINITY);
        assert_eq!(p.value(), -60.0);
    }

    #[test]
    fn push_peak_keeps_the_loudest() {
        let p = level();
        p.push_peak(-30.0);
        assert_eq!(p.value(), -30.0);
        p.push_peak(-40.0);
        assert_eq!(p.value(), -30.0);
        p.push_peak(-6.0);
        assert_eq!(p.value(), -6.0);
        p.push_peak(f32::NAN);
        assert_eq!(p.value(), -6.0, "NaN must never latch");
        p.clear();
        assert_eq!(p.value(), -60.0);
    }

    #[test]
    fn text_formats_and_parses() {
        let p = level().with_decimals(1);
        let mut s = String::new();
        p.to_text(-12.5, &mut s);
        assert_eq!(s, "-12.5 dB");
        assert_eq!(p.from_text(" -12.5 dB "), Some(-12.5));
        assert_eq!(p.from_text("1000"), Some(6.0));
        assert_eq!(p.from_text("quiet"), None);
    }

    #[test]
    fn reset_clears_the_meter() {
        let p = level();
        p.set_value(0.0);
        p.reset();
        assert_eq!(p.value(), -60.0);
    }

    #[test]
    fn normalised_access_works_for_generic_uis() {
        let p = level();
        p.set_normalized(1.0);
        assert_eq!(p.value(), 6.0);
        p.set_normalized(0.0);
        assert_eq!(p.value(), -60.0);
        p.set_plain(-27.0);
        assert!((p.normalized() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn the_audio_thread_writes_and_the_ui_thread_reads() {
        let p = Arc::new(level());
        let writer = {
            let p = Arc::clone(&p);
            std::thread::spawn(move || {
                for i in 0..10_000 {
                    p.set_value(-60.0 + (i % 66) as f32);
                }
                p.set_value(-3.0);
            })
        };
        // The reader never blocks and never sees a value outside the range.
        for _ in 0..10_000 {
            let v = p.value();
            assert!((-60.0..=6.0).contains(&v), "out-of-range reading {v}");
        }
        writer.join().expect("writer thread");
        assert_eq!(p.value(), -3.0);
    }

    #[test]
    fn is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<MeterParam>();
    }
}
