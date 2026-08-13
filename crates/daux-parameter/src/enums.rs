//! Enumeration parameter.

use daux_rt::AtomicF64;

use crate::{Param, ParamFlags, ParamId, ParamInfo, ParamRange, text};

/// A Rust enum usable as a parameter value.
///
/// Implement it by hand or let `#[derive(DauxParams)]` do it. The three pieces have to
/// agree: [`index`](ParamEnum::index) is the variant's **position in
/// [`VARIANTS`](ParamEnum::VARIANTS)**, [`from_index`](ParamEnum::from_index) is its
/// inverse, and those indices are the plain values that cross the ABI and land in saved
/// projects. Reordering `VARIANTS` therefore rewrites what old sessions recall, exactly
/// like renumbering a [`ParamId`] does — append new variants at the end instead.
///
/// ```
/// use daux_parameter::ParamEnum;
///
/// #[derive(Clone, Copy, PartialEq, Debug)]
/// enum Shape { Sine, Saw, Square }
///
/// impl ParamEnum for Shape {
///     const VARIANTS: &'static [Self] = &[Self::Sine, Self::Saw, Self::Square];
///     fn name(self) -> &'static str {
///         match self { Self::Sine => "Sine", Self::Saw => "Saw", Self::Square => "Square" }
///     }
///     fn index(self) -> u32 { self as u32 }
///     fn from_index(i: u32) -> Option<Self> { Self::VARIANTS.get(i as usize).copied() }
/// }
/// ```
pub trait ParamEnum: Copy + 'static {
    /// Every selectable variant, in the order the host shows them. Must not be empty
    /// and must not be reordered once shipped.
    const VARIANTS: &'static [Self];

    /// `[any-thread]` Display name of this variant.
    fn name(self) -> &'static str;

    /// `[any-thread]` Position of this variant in [`VARIANTS`](ParamEnum::VARIANTS).
    fn index(self) -> u32;

    /// `[any-thread]` Inverse of [`index`](ParamEnum::index); `None` when `i` is out of
    /// range.
    fn from_index(i: u32) -> Option<Self>;
}

/// A parameter whose value is one variant of `E`.
///
/// Stored as the variant's index in a [`daux_rt::AtomicF64`], so it automates, saves
/// and shares exactly like every other parameter. The index is quantised on every
/// write, which is what makes dragging a selector land squarely on a variant and makes
/// a recalled normalised value pick the same one.
///
/// ```
/// # use daux_parameter::{EnumParam, Param, ParamEnum, ParamId};
/// # #[derive(Clone, Copy, PartialEq, Debug)]
/// # enum Shape { Sine, Saw, Square }
/// # impl ParamEnum for Shape {
/// #     const VARIANTS: &'static [Self] = &[Self::Sine, Self::Saw, Self::Square];
/// #     fn name(self) -> &'static str {
/// #         match self { Self::Sine => "Sine", Self::Saw => "Saw", Self::Square => "Square" }
/// #     }
/// #     fn index(self) -> u32 { self as u32 }
/// #     fn from_index(i: u32) -> Option<Self> { Self::VARIANTS.get(i as usize).copied() }
/// # }
/// let shape = EnumParam::new(ParamId(5), "Shape", Shape::Saw);
/// assert_eq!(shape.value(), Shape::Saw);
/// assert_eq!(shape.text(shape.plain()), "Saw");
/// shape.set_normalized(1.0);
/// assert_eq!(shape.value(), Shape::Square);
/// assert_eq!(shape.from_text("sine"), Some(0.0));
/// ```
pub struct EnumParam<E: ParamEnum> {
    id: ParamId,
    name: String,
    group: String,
    default: E,
    flags: ParamFlags,
    range: ParamRange,
    value: AtomicF64,
}

impl<E: ParamEnum> EnumParam<E> {
    /// `[main-thread]` Builds a selector over every variant of `E`.
    ///
    /// An empty [`ParamEnum::VARIANTS`] is a broken implementation rather than a
    /// runtime condition; the parameter degrades to a single, constant state instead of
    /// panicking, so a mistake in one enum cannot take a whole session down.
    #[must_use]
    pub fn new(id: impl Into<ParamId>, name: impl Into<String>, default: E) -> Self {
        let count = Self::variant_count();
        let range = ParamRange::Stepped {
            min: 0,
            max: i64::from(count - 1),
        };
        let default_index = default.index().min(count - 1);
        Self {
            id: id.into(),
            name: name.into(),
            group: String::new(),
            default,
            flags: ParamFlags::DEFAULT | ParamFlags::STEPPED,
            range,
            value: AtomicF64::new(f64::from(default_index)),
        }
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

    /// `[any-thread]` Current variant.
    ///
    /// Falls back to the default variant if `E::from_index` rejects a stored index,
    /// which keeps this call panic-free even for a malformed [`ParamEnum`].
    #[inline]
    #[must_use]
    pub fn value(&self) -> E {
        E::from_index(self.index()).unwrap_or(self.default)
    }

    /// `[any-thread]` Stores a variant.
    #[inline]
    pub fn set(&self, v: E) {
        self.value.set(self.range.clamp(f64::from(v.index())));
    }

    /// `[any-thread]` Current variant index, always inside `0..variant_count()`.
    #[inline]
    #[must_use]
    pub fn index(&self) -> u32 {
        let clamped = self.range.clamp(self.value.get());
        clamped as u32
    }

    /// `[any-thread]` Number of selectable variants, at least `1`.
    #[inline]
    #[must_use]
    pub fn variant_count() -> u32 {
        let len = E::VARIANTS.len().max(1);
        u32::try_from(len).unwrap_or(u32::MAX)
    }

    /// `[any-thread]` Every selectable variant.
    #[inline]
    #[must_use]
    pub fn variants() -> &'static [E] {
        E::VARIANTS
    }

    /// `[any-thread]` The parameter's permanent id.
    #[inline]
    #[must_use]
    pub fn id(&self) -> ParamId {
        self.id
    }

    /// `[any-thread]` The reset variant.
    #[inline]
    #[must_use]
    pub fn default_value(&self) -> E {
        self.default
    }

    /// `[any-thread]` The underlying stepped range, `0..=variant_count() - 1`.
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

    /// Name of the variant at `plain`, or `None` when the index has no variant.
    fn name_at(plain: f64) -> Option<&'static str> {
        let index = plain.max(0.0) as u32;
        E::from_index(index).map(ParamEnum::name)
    }
}

impl<E: ParamEnum> core::fmt::Debug for EnumParam<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EnumParam")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("variants", &E::VARIANTS.len())
            .field("default", &self.default.name())
            .field("flags", &self.flags)
            .field("value", &self.value().name())
            .finish()
    }
}

impl<E: ParamEnum + Send + Sync> Param for EnumParam<E> {
    fn info(&self) -> ParamInfo {
        ParamInfo::new(
            self.id,
            self.name.clone(),
            &self.range,
            f64::from(self.default.index().min(Self::variant_count() - 1)),
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
        out.clear();
        match Self::name_at(self.range.clamp(plain)) {
            Some(name) => out.push_str(name),
            // A malformed `ParamEnum` still has to produce something a host can show.
            None => text::format_value(self.range.clamp(plain), 0, "", out),
        }
    }

    fn from_text(&self, text: &str) -> Option<f64> {
        let trimmed = text.trim();
        if let Some(variant) = E::VARIANTS
            .iter()
            .find(|variant| trimmed.eq_ignore_ascii_case(variant.name()))
        {
            return Some(f64::from(variant.index()));
        }
        // Fall back to a raw index so that automation written as a number still works.
        let parsed = crate::text::parse_value(trimmed)?;
        Some(self.range.clamp(parsed))
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

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Shape {
        Sine,
        Triangle,
        Saw,
        Square,
    }

    impl ParamEnum for Shape {
        const VARIANTS: &'static [Self] = &[Self::Sine, Self::Triangle, Self::Saw, Self::Square];

        fn name(self) -> &'static str {
            match self {
                Self::Sine => "Sine",
                Self::Triangle => "Triangle",
                Self::Saw => "Saw",
                Self::Square => "Square",
            }
        }

        fn index(self) -> u32 {
            self as u32
        }

        fn from_index(i: u32) -> Option<Self> {
            Self::VARIANTS.get(i as usize).copied()
        }
    }

    /// A deliberately broken implementation: no variants at all.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    struct Empty;

    impl ParamEnum for Empty {
        const VARIANTS: &'static [Self] = &[];
        fn name(self) -> &'static str {
            "Empty"
        }
        fn index(self) -> u32 {
            0
        }
        fn from_index(_: u32) -> Option<Self> {
            None
        }
    }

    fn shape() -> EnumParam<Shape> {
        EnumParam::new(ParamId(5), "Shape", Shape::Saw)
    }

    #[test]
    fn info_covers_every_variant() {
        let p = shape().with_group("Osc");
        let info = p.info();
        assert_eq!(info.min, 0.0);
        assert_eq!(info.max, 3.0);
        assert_eq!(info.step_count, 3);
        assert_eq!(info.default, 2.0);
        assert_eq!(info.group, "Osc");
        assert!(info.flags.is_stepped());
        assert_eq!(EnumParam::<Shape>::variant_count(), 4);
        assert_eq!(EnumParam::<Shape>::variants().len(), 4);
        assert_eq!(p.name(), "Shape");
        assert_eq!(p.default_value(), Shape::Saw);
        assert_eq!(p.range(), ParamRange::stepped(0, 3));
    }

    #[test]
    fn every_variant_round_trips_through_normalised() {
        let p = shape();
        for variant in EnumParam::<Shape>::variants() {
            p.set(*variant);
            assert_eq!(p.value(), *variant);
            let n = p.normalized();
            p.set_normalized(n);
            assert_eq!(
                p.value(),
                *variant,
                "{variant:?} did not survive normalisation"
            );
        }
        p.set(Shape::Sine);
        assert_eq!(p.normalized(), 0.0);
        p.set(Shape::Square);
        assert_eq!(p.normalized(), 1.0);
    }

    #[test]
    fn normalised_input_snaps_to_the_nearest_variant() {
        let p = shape();
        // Four variants, so each owns a sixth of the line either side of its position.
        p.set_normalized(0.0);
        assert_eq!(p.value(), Shape::Sine);
        p.set_normalized(0.16);
        assert_eq!(p.value(), Shape::Sine);
        p.set_normalized(0.17);
        assert_eq!(p.value(), Shape::Triangle);
        p.set_normalized(0.49);
        assert_eq!(p.value(), Shape::Triangle);
        // Exactly halfway rounds away from zero, so the upper variant wins.
        p.set_normalized(0.5);
        assert_eq!(p.value(), Shape::Saw);
        p.set_normalized(0.51);
        assert_eq!(p.value(), Shape::Saw);
        p.set_normalized(1.0);
        assert_eq!(p.value(), Shape::Square);
        // Out of range in either direction is clamped, never wrapped.
        p.set_normalized(-1.0);
        assert_eq!(p.value(), Shape::Sine);
        p.set_normalized(2.0);
        assert_eq!(p.value(), Shape::Square);
        p.set_plain(99.0);
        assert_eq!(p.value(), Shape::Square);
        p.set_plain(f64::NAN);
        assert_eq!(p.value(), Shape::Sine);
    }

    #[test]
    fn text_uses_variant_names() {
        let p = shape();
        let mut s = String::new();
        for variant in EnumParam::<Shape>::variants() {
            p.set(*variant);
            p.to_text(p.plain(), &mut s);
            assert_eq!(s, variant.name());
            assert_eq!(p.from_text(&s), Some(f64::from(variant.index())));
        }
        assert_eq!(p.from_text("  square "), Some(3.0));
        assert_eq!(p.from_text("SINE"), Some(0.0));
        // A bare index is accepted and clamped.
        assert_eq!(p.from_text("1"), Some(1.0));
        assert_eq!(p.from_text("9"), Some(3.0));
        assert_eq!(p.from_text("triangular"), None);
        assert_eq!(p.from_text(""), None);
    }

    #[test]
    fn reset_restores_the_default_variant() {
        let p = shape();
        p.set(Shape::Sine);
        assert_eq!(p.value(), Shape::Sine);
        p.reset();
        assert_eq!(p.value(), Shape::Saw);
        assert_eq!(p.index(), 2);
    }

    #[test]
    fn an_enum_with_no_variants_degrades_instead_of_panicking() {
        let p = EnumParam::new(ParamId(1), "Broken", Empty);
        assert_eq!(EnumParam::<Empty>::variant_count(), 1);
        assert_eq!(p.value(), Empty);
        assert_eq!(p.normalized(), 0.0);
        p.set_normalized(1.0);
        assert_eq!(p.plain(), 0.0);
        let mut s = String::new();
        p.to_text(0.0, &mut s);
        assert_eq!(s, "0", "a broken enum still formats to something showable");
        assert_eq!(p.info().step_count, 0);
    }

    #[test]
    fn is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<EnumParam<Shape>>();
    }
}
