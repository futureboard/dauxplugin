//! Where inside the host's surface the editor's pixels go.

use daux_graphics::PhysicalSize;

/// `[any-thread]` How an editor's pixels are fitted into a surface of a different size.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum Scaling {
    /// Fill the whole surface, changing the aspect ratio if it does not match.
    ///
    /// The default, and the right choice for an editor that redraws at the surface's own size:
    /// there is nothing to stretch, because the content is always exactly the right shape.
    #[default]
    Stretch,
    /// Keep the content's aspect ratio and centre it, leaving bars on two sides.
    ///
    /// For an editor with a fixed design size that the host is allowed to resize anyway.
    Contain,
}

/// `[any-thread]` A rectangle in surface pixels, with the origin at the **bottom left**.
///
/// Bottom-left, not top-left, because that is what `glViewport` and `glScissor` take. Every
/// other coordinate in DAUx has its origin at the top left, so the flip happens here, once,
/// rather than being remembered at each call site.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub struct Viewport {
    /// Distance from the surface's left edge, in pixels.
    pub x: i32,
    /// Distance from the surface's **bottom** edge, in pixels.
    pub y: i32,
    /// Width in pixels.
    pub width: i32,
    /// Height in pixels.
    pub height: i32,
}

impl Viewport {
    /// `[any-thread]` The empty viewport, which draws nothing.
    pub const EMPTY: Self = Self {
        x: 0,
        y: 0,
        width: 0,
        height: 0,
    };

    /// `[any-thread]` The whole surface.
    ///
    /// Dimensions are clamped to [`i32::MAX`]: `glViewport` takes signed integers, and a
    /// surface wider than that would otherwise arrive as a negative width, which is a
    /// `GL_INVALID_VALUE` rather than a big window.
    #[must_use]
    pub fn fill(surface: PhysicalSize) -> Self {
        Self {
            x: 0,
            y: 0,
            width: clamp_to_i32(surface.width),
            height: clamp_to_i32(surface.height),
        }
    }

    /// `[any-thread]` The largest centred rectangle of `content`'s aspect ratio that fits
    /// inside `surface`.
    ///
    /// Returns [`EMPTY`](Self::EMPTY) when either size is degenerate, so a minimised window
    /// draws nothing instead of dividing by zero.
    #[must_use]
    pub fn fit_contain(content: PhysicalSize, surface: PhysicalSize) -> Self {
        if content.is_empty() || surface.is_empty() {
            return Self::EMPTY;
        }
        // All of this is done in `u64`: `width * height` for two `u32`s overflows a `u32` for
        // any surface past about 65 000 pixels on a side, which a multi-monitor span reaches.
        let (cw, ch) = (u64::from(content.width), u64::from(content.height));
        let (sw, sh) = (u64::from(surface.width), u64::from(surface.height));

        // Compare `cw/ch` with `sw/sh` without dividing.
        let (width, height) = if cw * sh > sw * ch {
            // The content is wider than the surface: the width is the binding constraint.
            (sw, (sw * ch / cw).max(1))
        } else {
            (((sh * cw) / ch).max(1), sh)
        };

        Self {
            // Integer division truncates, which puts a one-pixel remainder on the right or
            // bottom rather than splitting it — the only stable choice, since the alternative
            // makes the content jitter by a pixel as the surface is dragged.
            x: clamp_to_i32(((sw.saturating_sub(width)) / 2) as u32),
            y: clamp_to_i32(((sh.saturating_sub(height)) / 2) as u32),
            width: clamp_to_i32(width.min(u64::from(u32::MAX)) as u32),
            height: clamp_to_i32(height.min(u64::from(u32::MAX)) as u32),
        }
    }

    /// `[any-thread]` The viewport `scaling` asks for.
    #[must_use]
    pub fn for_scaling(scaling: Scaling, content: PhysicalSize, surface: PhysicalSize) -> Self {
        match scaling {
            Scaling::Stretch => Self::fill(surface),
            Scaling::Contain => Self::fit_contain(content, surface),
        }
    }

    /// `[any-thread]` Whether this viewport covers no pixels.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.width <= 0 || self.height <= 0
    }
}

/// Narrows a pixel count to what `glViewport` accepts, saturating instead of wrapping.
fn clamp_to_i32(value: u32) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size(w: u32, h: u32) -> PhysicalSize {
        PhysicalSize::new(w, h)
    }

    #[test]
    fn filling_covers_the_whole_surface() {
        assert_eq!(
            Viewport::fill(size(800, 600)),
            Viewport {
                x: 0,
                y: 0,
                width: 800,
                height: 600
            }
        );
        assert!(Viewport::fill(size(0, 600)).is_empty());
        assert!(Viewport::EMPTY.is_empty());
        assert!(!Viewport::fill(size(1, 1)).is_empty());
    }

    #[test]
    fn containing_letterboxes_a_wide_editor_in_a_tall_surface() {
        // 800x400 content (2:1) inside a 800x800 surface: full width, half height, centred.
        let v = Viewport::fit_contain(size(800, 400), size(800, 800));
        assert_eq!(v.width, 800);
        assert_eq!(v.height, 400);
        assert_eq!(v.x, 0);
        assert_eq!(v.y, 200);
    }

    #[test]
    fn containing_pillarboxes_a_tall_editor_in_a_wide_surface() {
        // 400x800 content (1:2) inside a 1600x800 surface: full height, quarter width.
        let v = Viewport::fit_contain(size(400, 800), size(1600, 800));
        assert_eq!(v.width, 400);
        assert_eq!(v.height, 800);
        assert_eq!(v.x, 600);
        assert_eq!(v.y, 0);
    }

    #[test]
    fn a_matching_aspect_ratio_fills_exactly_with_no_bars() {
        let v = Viewport::fit_contain(size(400, 300), size(1600, 1200));
        assert_eq!(
            v,
            Viewport {
                x: 0,
                y: 0,
                width: 1600,
                height: 1200
            }
        );
    }

    #[test]
    fn the_contained_rectangle_never_escapes_the_surface() {
        // Property check over awkward ratios, including prime sizes where the integer
        // arithmetic cannot come out even.
        let contents = [
            size(1, 1),
            size(3, 7),
            size(1920, 1080),
            size(101, 97),
            size(1, 4096),
            size(4096, 1),
        ];
        let surfaces = [
            size(1, 1),
            size(13, 11),
            size(800, 600),
            size(1, 1000),
            size(1000, 1),
            size(2560, 1440),
        ];
        for content in contents {
            for surface in surfaces {
                let v = Viewport::fit_contain(content, surface);
                assert!(v.width >= 1 && v.height >= 1, "{content:?} in {surface:?}");
                assert!(
                    v.x >= 0 && v.y >= 0,
                    "{content:?} in {surface:?} produced {v:?}"
                );
                assert!(
                    v.x + v.width <= surface.width as i32,
                    "{content:?} in {surface:?} produced {v:?}, which is wider than the surface"
                );
                assert!(
                    v.y + v.height <= surface.height as i32,
                    "{content:?} in {surface:?} produced {v:?}, which is taller than the surface"
                );
            }
        }
    }

    #[test]
    fn a_degenerate_size_draws_nothing_rather_than_dividing_by_zero() {
        assert_eq!(
            Viewport::fit_contain(size(0, 0), size(800, 600)),
            Viewport::EMPTY
        );
        assert_eq!(
            Viewport::fit_contain(size(800, 600), size(0, 0)),
            Viewport::EMPTY
        );
        assert_eq!(
            Viewport::fit_contain(size(800, 0), size(800, 600)),
            Viewport::EMPTY
        );
        assert_eq!(
            Viewport::fit_contain(size(800, 600), size(1, 0)),
            Viewport::EMPTY
        );
    }

    #[test]
    fn enormous_surfaces_saturate_instead_of_wrapping_into_a_negative_viewport() {
        // `glViewport` takes signed ints; a wrapped width is `GL_INVALID_VALUE`, which shows
        // up as a black editor rather than as an error anyone can act on.
        let huge = size(u32::MAX, u32::MAX);
        let v = Viewport::fill(huge);
        assert_eq!(v.width, i32::MAX);
        assert_eq!(v.height, i32::MAX);

        let contained = Viewport::fit_contain(size(16, 9), huge);
        assert!(contained.width > 0 && contained.height > 0);
        assert!(contained.x >= 0 && contained.y >= 0);
    }

    #[test]
    fn the_aspect_ratio_survives_scaling_to_within_a_pixel() {
        let v = Viewport::fit_contain(size(1920, 1080), size(1000, 1000));
        let ratio = f64::from(v.width) / f64::from(v.height);
        assert!(
            (ratio - 1920.0 / 1080.0).abs() < 0.01,
            "aspect ratio drifted to {ratio}"
        );
    }

    #[test]
    fn scaling_picks_between_the_two_strategies() {
        let content = size(400, 400);
        let surface = size(800, 400);
        assert_eq!(
            Viewport::for_scaling(Scaling::Stretch, content, surface),
            Viewport::fill(surface)
        );
        assert_eq!(
            Viewport::for_scaling(Scaling::Contain, content, surface),
            Viewport::fit_contain(content, surface)
        );
        assert_eq!(Scaling::default(), Scaling::Stretch);
    }
}
