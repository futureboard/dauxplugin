//! Two-state parameter.

use daux_rt::AtomicF64;

use crate::{Param, ParamFlags, ParamId, ParamInfo, ParamRange, text};

/// A switch: bypass, invert, sync, mute.
///
/// Plain values are `0.0` and `1.0`, which is what the ABI, automation and saved state
/// see; `value()`/`set()` present the same thing as a `bool`. The stored value is kept
/// in a [`daux_rt::AtomicF64`] so the type behaves exactly like every other parameter
/// when shared through one `Arc<Params>`.
///
/// ```
/// use daux_parameter::{BoolParam, Param, ParamId};
///
/// let invert = BoolParam::new(ParamId(2), "Invert", false).with_labels("Normal", "Inverted");
/// assert!(!invert.value());
/// invert.set_normalized(1.0);
/// assert!(invert.value());
/// assert_eq!(invert.text(invert.plain()), "Inverted");
/// assert_eq!(invert.from_text("normal"), Some(0.0));
/// ```
pub struct BoolParam {
    id: ParamId,
    name: String,
    group: String,
    off_label: String,
    on_label: String,
    default: bool,
    flags: ParamFlags,
    value: AtomicF64,
}

impl BoolParam {
    /// `[main-thread]` Builds a switch with the labels `"Off"` and `"On"`.
    #[must_use]
    pub fn new(id: impl Into<ParamId>, name: impl Into<String>, default: bool) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            group: String::new(),
            off_label: String::from("Off"),
            on_label: String::from("On"),
            default,
            flags: ParamFlags::DEFAULT | ParamFlags::STEPPED,
            value: AtomicF64::new(f64::from(u8::from(default))),
        }
    }

    /// `[main-thread]` Renames the two states, e.g. `("Normal", "Inverted")`.
    ///
    /// Both labels stay parseable by [`Param::from_text`], alongside the usual
    /// `on`/`off`/`true`/`false`/`1`/`0` spellings.
    #[must_use]
    pub fn with_labels(mut self, off: impl Into<String>, on: impl Into<String>) -> Self {
        self.off_label = off.into();
        self.on_label = on.into();
        self
    }

    /// `[main-thread]` Sets the `/`-separated group path.
    #[must_use]
    pub fn with_group(mut self, group: impl Into<String>) -> Self {
        self.group = group.into();
        self
    }

    /// `[main-thread]` Replaces the flags; `STEPPED` is re-added automatically.
    ///
    /// A plug-in's bypass control is just a `BoolParam` with
    /// [`ParamFlags::BYPASS`] set.
    #[must_use]
    pub fn with_flags(mut self, flags: ParamFlags) -> Self {
        self.flags = flags.with(ParamFlags::STEPPED);
        self
    }

    /// `[any-thread]` Current state.
    #[inline]
    #[must_use]
    pub fn value(&self) -> bool {
        self.value.get() >= 0.5
    }

    /// `[any-thread]` Stores a state.
    #[inline]
    pub fn set(&self, v: bool) {
        self.value.set(f64::from(u8::from(v)));
    }

    /// `[any-thread]` The parameter's permanent id.
    #[inline]
    #[must_use]
    pub fn id(&self) -> ParamId {
        self.id
    }

    /// `[any-thread]` The reset state.
    #[inline]
    #[must_use]
    pub fn default_value(&self) -> bool {
        self.default
    }

    /// `[any-thread]` Display name.
    #[inline]
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// `[any-thread]` The label shown for a state.
    #[inline]
    #[must_use]
    pub fn label(&self, state: bool) -> &str {
        if state {
            &self.on_label
        } else {
            &self.off_label
        }
    }
}

impl core::fmt::Debug for BoolParam {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("BoolParam")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("default", &self.default)
            .field("flags", &self.flags)
            .field("value", &self.value())
            .finish()
    }
}

impl Param for BoolParam {
    fn info(&self) -> ParamInfo {
        ParamInfo::new(
            self.id,
            self.name.clone(),
            &ParamRange::Boolean,
            f64::from(u8::from(self.default)),
            self.flags,
        )
        .with_group(self.group.clone())
    }

    #[inline]
    fn plain(&self) -> f64 {
        self.value.get()
    }

    #[inline]
    fn set_plain(&self, v: f64) {
        self.value.set(ParamRange::Boolean.clamp(v));
    }

    #[inline]
    fn normalized(&self) -> f64 {
        self.value.get()
    }

    #[inline]
    fn set_normalized(&self, v: f64) {
        self.value.set(ParamRange::Boolean.denormalize(v));
    }

    fn to_text(&self, plain: f64, out: &mut String) {
        out.clear();
        out.push_str(self.label(plain >= 0.5));
    }

    fn from_text(&self, text: &str) -> Option<f64> {
        let trimmed = text.trim();
        if trimmed.eq_ignore_ascii_case(&self.on_label) {
            return Some(1.0);
        }
        if trimmed.eq_ignore_ascii_case(&self.off_label) {
            return Some(0.0);
        }
        text::parse_bool(trimmed).map(|on| f64::from(u8::from(on)))
    }

    #[inline]
    fn reset(&self) {
        self.set(self.default);
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

    #[test]
    fn info_describes_a_two_state_parameter() {
        let p = BoolParam::new(ParamId(2), "Bypass", false)
            .with_group("Global")
            .with_flags(ParamFlags::AUTOMATABLE | ParamFlags::BYPASS);
        let info = p.info();
        assert_eq!(info.min, 0.0);
        assert_eq!(info.max, 1.0);
        assert_eq!(info.default, 0.0);
        assert_eq!(info.step_count, 1);
        assert_eq!(info.group, "Global");
        assert!(
            info.flags
                .contains(ParamFlags::BYPASS | ParamFlags::STEPPED)
        );
        assert_eq!(p.name(), "Bypass");
        assert!(!p.default_value());
    }

    #[test]
    fn states_map_onto_plain_and_normalised_values() {
        let p = BoolParam::new(ParamId(2), "Invert", false);
        assert!(!p.value());
        assert_eq!(p.plain(), 0.0);
        assert_eq!(p.normalized(), 0.0);

        p.set(true);
        assert!(p.value());
        assert_eq!(p.plain(), 1.0);
        assert_eq!(p.normalized(), 1.0);

        p.set_normalized(0.49);
        assert!(!p.value());
        p.set_normalized(0.5);
        assert!(p.value(), "the halfway point counts as on");
        p.set_plain(0.2);
        assert!(!p.value());
        p.set_plain(37.0);
        assert!(p.value());
        p.set_plain(f64::NAN);
        assert!(!p.value());
    }

    #[test]
    fn text_uses_the_labels_and_parses_generously() {
        let p = BoolParam::new(ParamId(2), "Invert", false).with_labels("Normal", "Inverted");
        let mut s = String::new();
        p.to_text(1.0, &mut s);
        assert_eq!(s, "Inverted");
        p.to_text(0.0, &mut s);
        assert_eq!(s, "Normal");

        assert_eq!(p.from_text("Inverted"), Some(1.0));
        assert_eq!(p.from_text("  inverted "), Some(1.0));
        assert_eq!(p.from_text("NORMAL"), Some(0.0));
        assert_eq!(p.from_text("on"), Some(1.0));
        assert_eq!(p.from_text("true"), Some(1.0));
        assert_eq!(p.from_text("off"), Some(0.0));
        assert_eq!(p.from_text("0"), Some(0.0));
        assert_eq!(p.from_text("1"), Some(1.0));
        assert_eq!(p.from_text("maybe"), None);
        assert_eq!(p.from_text(""), None);
    }

    #[test]
    fn default_labels_are_off_and_on() {
        let p = BoolParam::new(ParamId(2), "Sync", true);
        assert_eq!(p.label(false), "Off");
        assert_eq!(p.label(true), "On");
        assert_eq!(p.text(p.plain()), "On");
        assert!(p.value());
    }

    #[test]
    fn reset_restores_the_default() {
        let p = BoolParam::new(ParamId(2), "Sync", true);
        p.set(false);
        assert!(!p.value());
        p.reset();
        assert!(p.value());
    }

    #[test]
    fn is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<BoolParam>();
    }
}
