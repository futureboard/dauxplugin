//! Logical and physical geometry, and the [`ScaleFactor`] that maps between them.
//!
//! Logical units are what an editor lays out in: a knob is "48 logical pixels wide" on
//! every display. Physical units are what the compositor allocates: the same knob is 48
//! device pixels at 100 % scaling and 96 at 200 %. Mixing the two is the classic HiDPI
//! bug, so the two are **different types** and the only way from one to the other is a
//! conversion that names the scale factor explicitly.
//!
//! ```
//! use daux_graphics::{LogicalSize, PhysicalSize, ScaleFactor};
//!
//! let scale = ScaleFactor::new(2.0).expect("2.0 is a valid scale factor");
//! let logical = LogicalSize::new(400.0, 300.0);
//! assert_eq!(logical.to_physical(scale), PhysicalSize::new(800, 600));
//! assert_eq!(PhysicalSize::new(800, 600).to_logical(scale), logical);
//! ```

use core::fmt;

/// `[any-thread]` HiDPI scale factor: physical pixels per logical pixel.
///
/// Validated on construction, so no code downstream has to defend against `0.0`, a
/// negative factor or `NaN` — a division by the scale factor is always safe and a
/// conversion always terminates.
///
/// The accepted interval is [`ScaleFactor::MIN`]`..=`[`ScaleFactor::MAX`]. Real hosts
/// report values between `1.0` and `4.0`; the wider interval exists so an unusual
/// configuration is honoured rather than silently clamped, while a nonsense value from a
/// misbehaving host is still rejected.
#[derive(Clone, Copy, PartialEq, PartialOrd, Debug)]
pub struct ScaleFactor(f64);

impl ScaleFactor {
    /// `[any-thread]` The identity: one physical pixel per logical pixel.
    pub const ONE: Self = Self(1.0);

    /// `[any-thread]` Smallest accepted factor.
    pub const MIN: f64 = 0.05;

    /// `[any-thread]` Largest accepted factor.
    pub const MAX: f64 = 64.0;

    /// `[any-thread]` Builds a scale factor, rejecting anything outside
    /// [`MIN`](Self::MIN)`..=`[`MAX`](Self::MAX), including `NaN` and infinities.
    ///
    /// ```
    /// # use daux_graphics::ScaleFactor;
    /// assert!(ScaleFactor::new(1.5).is_some());
    /// assert!(ScaleFactor::new(0.0).is_none());
    /// assert!(ScaleFactor::new(-2.0).is_none());
    /// assert!(ScaleFactor::new(f64::NAN).is_none());
    /// ```
    #[must_use]
    pub fn new(value: f64) -> Option<Self> {
        (value.is_finite() && (Self::MIN..=Self::MAX).contains(&value)).then_some(Self(value))
    }

    /// `[any-thread]` Builds a scale factor, clamping instead of failing.
    ///
    /// `NaN` becomes [`ONE`](Self::ONE), because a host that reports `NaN` has told us
    /// nothing and unscaled is the least surprising answer.
    #[must_use]
    pub fn new_clamped(value: f64) -> Self {
        if value.is_nan() {
            Self::ONE
        } else {
            Self(value.clamp(Self::MIN, Self::MAX))
        }
    }

    /// `[any-thread]` The factor as a plain `f64`, always finite and `> 0`.
    #[inline]
    #[must_use]
    pub const fn get(self) -> f64 {
        self.0
    }

    /// `[any-thread]` `true` when this is exactly `1.0`, the common non-HiDPI case.
    #[inline]
    #[must_use]
    pub fn is_identity(self) -> bool {
        self.0 == 1.0
    }
}

impl Default for ScaleFactor {
    /// `[any-thread]` [`ScaleFactor::ONE`].
    fn default() -> Self {
        Self::ONE
    }
}

impl fmt::Display for ScaleFactor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x", self.0)
    }
}

/// Rounds a logical coordinate to a physical pixel count without ever producing `NaN`,
/// a negative value or an out-of-range `u32`.
fn to_pixels(value: f64, scale: f64) -> u32 {
    let scaled = value * scale;
    // NaN and anything at or below zero are nonsense and floor to 0. `+inf` is *not*
    // nonsense: it is what a large-but-legitimate size overflows to once multiplied by the
    // scale factor, and it must saturate upward with every other too-large value. Testing
    // `is_finite` here instead would silently turn the largest possible window into a
    // zero-sized one.
    if scaled.is_nan() || scaled <= 0.0 {
        return 0;
    }
    let rounded = scaled.round();
    if rounded >= f64::from(u32::MAX) {
        u32::MAX
    } else {
        // Truncation is exact: `rounded` is a non-negative integral value below u32::MAX.
        rounded as u32
    }
}

/// `[any-thread]` A size in scale-independent logical pixels.
///
/// This is the unit an editor lays out in and the unit [`GraphicDescriptor`] states its
/// preferred, minimum and maximum sizes in.
///
/// [`GraphicDescriptor`]: crate::GraphicDescriptor
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct LogicalSize {
    /// Width in logical pixels.
    pub width: f64,
    /// Height in logical pixels.
    pub height: f64,
}

impl LogicalSize {
    /// `[any-thread]` The empty size.
    pub const ZERO: Self = Self {
        width: 0.0,
        height: 0.0,
    };

    /// `[any-thread]` Builds a size.
    #[inline]
    #[must_use]
    pub const fn new(width: f64, height: f64) -> Self {
        Self { width, height }
    }

    /// `[any-thread]` A square size.
    #[inline]
    #[must_use]
    pub const fn square(side: f64) -> Self {
        Self::new(side, side)
    }

    /// `[any-thread]` `true` when both dimensions are finite and strictly positive —
    /// the precondition for a size that can actually be presented.
    #[must_use]
    pub fn is_usable(self) -> bool {
        self.width.is_finite() && self.height.is_finite() && self.width > 0.0 && self.height > 0.0
    }

    /// `[main-thread]` Converts to physical pixels with an explicit scale factor.
    ///
    /// Rounds to the nearest pixel and saturates rather than wrapping: a hostile or
    /// nonsensical size yields `0` or [`u32::MAX`], never a wrapped-around window.
    #[must_use]
    pub fn to_physical(self, scale: ScaleFactor) -> PhysicalSize {
        PhysicalSize {
            width: to_pixels(self.width, scale.get()),
            height: to_pixels(self.height, scale.get()),
        }
    }

    /// `[any-thread]` Component-wise multiplication by a plain factor.
    #[must_use]
    pub fn scaled(self, factor: f64) -> Self {
        Self::new(self.width * factor, self.height * factor)
    }

    /// `[any-thread]` Clamps into an optional minimum and maximum.
    ///
    /// A minimum larger than the maximum is resolved in favour of the minimum, matching
    /// what window managers do and keeping the result usable rather than empty.
    #[must_use]
    pub fn clamped(self, min: Option<Self>, max: Option<Self>) -> Self {
        let mut out = self;
        if let Some(max) = max {
            out.width = out.width.min(max.width);
            out.height = out.height.min(max.height);
        }
        if let Some(min) = min {
            out.width = out.width.max(min.width);
            out.height = out.height.max(min.height);
        }
        out
    }

    /// `[any-thread]` Width divided by height, or `None` when that is not a usable
    /// number.
    #[must_use]
    pub fn aspect_ratio(self) -> Option<f64> {
        self.is_usable().then(|| self.width / self.height)
    }

    /// `[any-thread]` The smallest size with ratio `ratio` that covers `self`.
    ///
    /// Of the two candidates — keep the width and grow the height, or keep the height and
    /// grow the width — this picks the one that grows, never the one that shrinks. That is
    /// what makes dragging a corner feel the same whichever edge the user grabbed: the
    /// dimension the user pulled is always honoured, and the other one follows.
    ///
    /// Picking the *nearer* candidate instead would be tempting and is wrong: for a 4:3
    /// editor dragged from 800×600 to 800×900, shrinking back to 800×600 is numerically
    /// closer than growing to 1200×900, so the user's drag would simply snap away.
    ///
    /// Because this only ever grows, the result can exceed a maximum size. Callers that have
    /// bounds must clamp *after* calling this — see [`GraphicDescriptor::clamp`].
    ///
    /// A non-usable `ratio` or `self` is returned unchanged.
    ///
    /// [`GraphicDescriptor::clamp`]: crate::GraphicDescriptor::clamp
    #[must_use]
    pub fn with_aspect_ratio(self, ratio: f64) -> Self {
        if !ratio.is_finite() || ratio <= 0.0 || !self.is_usable() {
            return self;
        }
        // `self.width / self.height > ratio` without the division, which cannot divide by a
        // zero that `is_usable` has already excluded but reads more clearly this way.
        if self.width > self.height * ratio {
            // Too wide for the ratio: keep the width, grow the height to match.
            Self::new(self.width, self.width / ratio)
        } else {
            // Too tall for the ratio: keep the height, grow the width to match.
            Self::new(self.height * ratio, self.height)
        }
    }
}

impl fmt::Display for LogicalSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{} logical", self.width, self.height)
    }
}

/// `[any-thread]` A size in device pixels, as the compositor sees it.
///
/// Physical sizes are what `abi-v1` §11.4 puts on the wire (`get_size`/`set_size` are
/// specified in physical pixels) and what a swapchain is created with.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct PhysicalSize {
    /// Width in device pixels.
    pub width: u32,
    /// Height in device pixels.
    pub height: u32,
}

impl PhysicalSize {
    /// `[any-thread]` The empty size.
    pub const ZERO: Self = Self {
        width: 0,
        height: 0,
    };

    /// `[any-thread]` Builds a size.
    #[inline]
    #[must_use]
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// `[any-thread]` `true` when either dimension is zero, i.e. nothing can be drawn.
    #[inline]
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// `[any-thread]` Pixel count, computed in `u64` so it cannot overflow.
    #[inline]
    #[must_use]
    pub const fn area(self) -> u64 {
        self.width as u64 * self.height as u64
    }

    /// `[main-thread]` Converts to logical pixels with an explicit scale factor.
    #[must_use]
    pub fn to_logical(self, scale: ScaleFactor) -> LogicalSize {
        LogicalSize {
            width: f64::from(self.width) / scale.get(),
            height: f64::from(self.height) / scale.get(),
        }
    }
}

impl fmt::Display for PhysicalSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{} px", self.width, self.height)
    }
}

/// `[any-thread]` A position in logical pixels, relative to the editor's top-left corner.
///
/// Every coordinate in [`InputEvent`](crate::InputEvent) is logical: an editor's hit
/// testing is written once and works at any scale factor.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct LogicalPoint {
    /// Distance from the left edge, in logical pixels; grows rightwards.
    pub x: f64,
    /// Distance from the top edge, in logical pixels; grows downwards.
    pub y: f64,
}

impl LogicalPoint {
    /// `[any-thread]` The editor's top-left corner.
    pub const ORIGIN: Self = Self { x: 0.0, y: 0.0 };

    /// `[any-thread]` Builds a point.
    #[inline]
    #[must_use]
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    /// `[any-thread]` Converts to device pixels with an explicit scale factor.
    #[must_use]
    pub fn to_physical(self, scale: ScaleFactor) -> PhysicalPoint {
        PhysicalPoint::new(self.x * scale.get(), self.y * scale.get())
    }

    /// `[any-thread]` Moves the point by a vector.
    #[must_use]
    pub fn translated(self, by: LogicalVector) -> Self {
        Self::new(self.x + by.x, self.y + by.y)
    }

    /// `[any-thread]` `true` when the point lies inside a rectangle of `size` anchored at
    /// the origin. Half-open on the right and bottom edges, as hit testing wants.
    #[must_use]
    pub fn is_inside(self, size: LogicalSize) -> bool {
        self.x >= 0.0 && self.y >= 0.0 && self.x < size.width && self.y < size.height
    }
}

/// `[any-thread]` A position in device pixels, as delivered by the platform.
///
/// Kept fractional because macOS and Wayland report sub-pixel pointer positions.
/// Convert with [`to_logical`](PhysicalPoint::to_logical) before handing it to editor
/// code.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct PhysicalPoint {
    /// Distance from the left edge, in device pixels.
    pub x: f64,
    /// Distance from the top edge, in device pixels.
    pub y: f64,
}

impl PhysicalPoint {
    /// `[any-thread]` The editor's top-left corner.
    pub const ORIGIN: Self = Self { x: 0.0, y: 0.0 };

    /// `[any-thread]` Builds a point.
    #[inline]
    #[must_use]
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    /// `[any-thread]` Converts to logical pixels with an explicit scale factor.
    #[must_use]
    pub fn to_logical(self, scale: ScaleFactor) -> LogicalPoint {
        LogicalPoint::new(self.x / scale.get(), self.y / scale.get())
    }
}

/// `[any-thread]` A displacement in logical pixels: a scroll amount, a drag delta.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct LogicalVector {
    /// Horizontal component; positive points right.
    pub x: f64,
    /// Vertical component; positive points down.
    pub y: f64,
}

impl LogicalVector {
    /// `[any-thread]` No displacement.
    pub const ZERO: Self = Self { x: 0.0, y: 0.0 };

    /// `[any-thread]` Builds a vector.
    #[inline]
    #[must_use]
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    /// `[any-thread]` Component-wise multiplication.
    #[must_use]
    pub fn scaled(self, factor: f64) -> Self {
        Self::new(self.x * factor, self.y * factor)
    }

    /// `[any-thread]` `true` when both components are exactly zero.
    #[must_use]
    pub fn is_zero(self) -> bool {
        self.x == 0.0 && self.y == 0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_factors_reject_nonsense() {
        assert_eq!(ScaleFactor::new(1.0).map(ScaleFactor::get), Some(1.0));
        assert_eq!(
            ScaleFactor::new(ScaleFactor::MIN).map(ScaleFactor::get),
            Some(ScaleFactor::MIN)
        );
        assert_eq!(
            ScaleFactor::new(ScaleFactor::MAX).map(ScaleFactor::get),
            Some(ScaleFactor::MAX)
        );
        assert!(ScaleFactor::new(0.0).is_none());
        assert!(ScaleFactor::new(-1.0).is_none());
        assert!(ScaleFactor::new(f64::NAN).is_none());
        assert!(ScaleFactor::new(f64::INFINITY).is_none());
        assert!(ScaleFactor::new(ScaleFactor::MAX + 1.0).is_none());
        assert!(ScaleFactor::new(ScaleFactor::MIN / 2.0).is_none());
    }

    #[test]
    fn clamping_construction_never_fails() {
        assert_eq!(ScaleFactor::new_clamped(f64::NAN), ScaleFactor::ONE);
        assert_eq!(ScaleFactor::new_clamped(0.0).get(), ScaleFactor::MIN);
        assert_eq!(
            ScaleFactor::new_clamped(f64::INFINITY).get(),
            ScaleFactor::MAX
        );
        assert_eq!(ScaleFactor::new_clamped(-3.0).get(), ScaleFactor::MIN);
        assert_eq!(ScaleFactor::new_clamped(2.0).get(), 2.0);
        assert!(ScaleFactor::default().is_identity());
    }

    #[test]
    fn logical_and_physical_round_trip_at_common_scales() {
        for raw in [1.0, 1.25, 1.5, 2.0, 3.0] {
            let scale = ScaleFactor::new(raw).expect("common scale factor");
            let logical = LogicalSize::new(400.0, 300.0);
            let physical = logical.to_physical(scale);
            assert_eq!(physical.to_logical(scale), logical, "scale {raw}");
        }
    }

    #[test]
    fn conversion_rounds_to_the_nearest_pixel() {
        let scale = ScaleFactor::new(1.5).expect("valid");
        // 100.4 * 1.5 = 150.6 -> 151; 100.2 * 1.5 = 150.3 -> 150.
        let size = LogicalSize::new(100.4, 100.2).to_physical(scale);
        assert_eq!(size, PhysicalSize::new(151, 150));
    }

    #[test]
    fn conversion_saturates_instead_of_wrapping() {
        let scale = ScaleFactor::new(64.0).expect("valid");
        let huge = LogicalSize::new(f64::MAX, 1e12).to_physical(scale);
        assert_eq!(huge, PhysicalSize::new(u32::MAX, u32::MAX));

        let nonsense = LogicalSize::new(f64::NAN, -400.0).to_physical(scale);
        assert_eq!(nonsense, PhysicalSize::ZERO);
    }

    #[test]
    fn usability_checks_catch_degenerate_sizes() {
        assert!(LogicalSize::new(10.0, 10.0).is_usable());
        assert!(!LogicalSize::ZERO.is_usable());
        assert!(!LogicalSize::new(-1.0, 10.0).is_usable());
        assert!(!LogicalSize::new(f64::NAN, 10.0).is_usable());
        assert!(!LogicalSize::new(f64::INFINITY, 10.0).is_usable());
        assert!(PhysicalSize::new(0, 10).is_empty());
        assert!(!PhysicalSize::new(1, 1).is_empty());
        assert_eq!(
            PhysicalSize::new(u32::MAX, u32::MAX).area(),
            u64::from(u32::MAX) * u64::from(u32::MAX)
        );
    }

    #[test]
    fn clamping_prefers_the_minimum_when_bounds_are_inverted() {
        let min = LogicalSize::new(200.0, 200.0);
        let max = LogicalSize::new(100.0, 100.0);
        let clamped = LogicalSize::new(50.0, 400.0).clamped(Some(min), Some(max));
        assert_eq!(clamped, min);

        let only_max = LogicalSize::new(500.0, 500.0).clamped(None, Some(max));
        assert_eq!(only_max, max);
        let unbounded = LogicalSize::new(500.0, 500.0).clamped(None, None);
        assert_eq!(unbounded, LogicalSize::new(500.0, 500.0));
    }

    #[test]
    fn aspect_ratio_snapping_picks_the_nearer_candidate() {
        let target = LogicalSize::new(800.0, 600.0);
        let ratio = target.aspect_ratio().expect("usable");
        assert!((ratio - 4.0 / 3.0).abs() < 1e-12);

        // Dragged mostly wider: keeping the width is the smaller correction.
        let wider = LogicalSize::new(1000.0, 600.0).with_aspect_ratio(ratio);
        assert_eq!(wider, LogicalSize::new(1000.0, 750.0));
        // Dragged mostly taller: keeping the height wins.
        let taller = LogicalSize::new(800.0, 900.0).with_aspect_ratio(ratio);
        assert_eq!(taller, LogicalSize::new(1200.0, 900.0));
        // Already correct: unchanged.
        assert_eq!(target.with_aspect_ratio(ratio), target);
    }

    #[test]
    fn aspect_ratio_snapping_ignores_nonsense() {
        let size = LogicalSize::new(800.0, 600.0);
        assert_eq!(size.with_aspect_ratio(0.0), size);
        assert_eq!(size.with_aspect_ratio(-2.0), size);
        assert_eq!(size.with_aspect_ratio(f64::NAN), size);
        assert_eq!(LogicalSize::ZERO.with_aspect_ratio(1.0), LogicalSize::ZERO);
        assert_eq!(LogicalSize::ZERO.aspect_ratio(), None);
    }

    #[test]
    fn points_convert_and_hit_test() {
        let scale = ScaleFactor::new(2.0).expect("valid");
        let physical = PhysicalPoint::new(64.0, 32.0);
        let logical = physical.to_logical(scale);
        assert_eq!(logical, LogicalPoint::new(32.0, 16.0));
        assert_eq!(logical.to_physical(scale), physical);

        let bounds = LogicalSize::new(32.0, 16.0);
        assert!(LogicalPoint::ORIGIN.is_inside(bounds));
        assert!(!logical.is_inside(bounds), "the far edge is exclusive");
        assert!(!LogicalPoint::new(-0.5, 4.0).is_inside(bounds));
        assert!(LogicalPoint::new(31.9, 15.9).is_inside(bounds));
    }

    #[test]
    fn vectors_translate_points() {
        let moved = LogicalPoint::new(10.0, 10.0).translated(LogicalVector::new(-4.0, 2.5));
        assert_eq!(moved, LogicalPoint::new(6.0, 12.5));
        assert!(LogicalVector::ZERO.is_zero());
        assert!(!LogicalVector::new(0.0, 1.0).is_zero());
        assert_eq!(
            LogicalVector::new(2.0, 3.0).scaled(2.0),
            LogicalVector::new(4.0, 6.0)
        );
        assert_eq!(
            LogicalSize::square(4.0).scaled(0.5),
            LogicalSize::new(2.0, 2.0)
        );
    }

    #[test]
    fn display_is_unambiguous_about_units() {
        assert_eq!(
            LogicalSize::new(400.0, 300.0).to_string(),
            "400x300 logical"
        );
        assert_eq!(PhysicalSize::new(800, 600).to_string(), "800x600 px");
        assert_eq!(ScaleFactor::new(1.5).expect("valid").to_string(), "1.5x");
    }
}
