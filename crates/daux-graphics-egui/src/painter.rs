//! Where an egui frame turns into pixels.
//!
//! `egui` is a layout and interaction library: it produces shapes, not pixels. Something has
//! to own a device, upload the font atlas and rasterise the triangles, and that something is
//! necessarily specific to a rendering API. This crate deliberately depends on no GPU crate,
//! so the renderer is supplied from outside as an [`EguiPainter`].
//!
//! That split is what lets one editor run on `daux-graphics-wgpu` on a machine with a working
//! GPU and on `daux-graphics-gl` on one without, with the plug-in's own UI code unchanged.

use daux_graphics::{
    DauxGraphicResult, GraphicContext, GraphicFramework, GraphicProfile, GraphicRenderer,
    PhysicalSize, PresentationMode,
};

/// [main-thread] The profile an egui editor offers for a given renderer.
///
/// Always [`PresentationMode::EmbeddedSurface`]: it is the one mode every host can provide,
/// and `daux-graphics` requires an editor to offer it as a fallback. A painter that really
/// does present some other way overrides [`EguiPainter::profile`] instead of using this.
#[must_use]
pub fn profile(renderer: GraphicRenderer) -> GraphicProfile {
    GraphicProfile::new(
        GraphicFramework::Egui,
        renderer,
        PresentationMode::EmbeddedSurface,
    )
}

/// Turns the output of one egui frame into pixels on the host's surface.
///
/// # Contract
///
/// Implementations are driven by [`EguiEditor`](crate::EguiEditor) in exactly this order:
/// [`open`](Self::open), then any number of [`resize`](Self::resize) and
/// [`paint`](Self::paint) calls, then [`close`](Self::close). `open` is never called twice
/// without a `close` in between, and `close` must be idempotent.
///
/// # The texture delta is not optional
///
/// [`egui::FullOutput::textures_delta`] carries the font atlas and every user texture egui
/// created or freed this frame. A painter **must** apply it (or, if it draws nothing, clear
/// it): `epaint::TexturesDelta` asserts on drop that it was handled, so a painter that
/// forgets will panic the host in a debug build and silently lose the font atlas in a release
/// one. [`HeadlessPainter`] shows the minimum correct handling.
///
/// [main-thread]
pub trait EguiPainter {
    /// [main-thread] The framework/renderer/presentation combination this painter honours.
    ///
    /// [`GraphicProfile::framework`] must be [`GraphicFramework::Egui`]; an editor built on a
    /// painter that says otherwise refuses to open, because the host would have been told it
    /// was getting something this crate cannot draw.
    fn profile(&self) -> GraphicProfile;

    /// [main-thread] Creates the device, swapchain and texture storage for `ctx`'s window.
    ///
    /// # Errors
    ///
    /// Any [`GraphicError`](daux_graphics::GraphicError). The editor reports the failure to
    /// the host and stays closed; `close` is not called for a failed `open`.
    fn open(&mut self, ctx: &mut GraphicContext<'_>) -> DauxGraphicResult<()>;

    /// [main-thread] The host's surface changed size, in physical pixels.
    ///
    /// Never called with an empty size — the editor rejects those before they get here.
    ///
    /// # Errors
    ///
    /// Any [`GraphicError`](daux_graphics::GraphicError); the host keeps the previous size.
    fn resize(&mut self, size: PhysicalSize) -> DauxGraphicResult<()>;

    /// [main-thread] Draws one finished egui frame.
    ///
    /// Use [`egui::Context::tessellate`] with [`egui::FullOutput::shapes`] and
    /// [`egui::FullOutput::pixels_per_point`] to get triangles, and apply
    /// [`egui::FullOutput::textures_delta`] around the draw: `set` before, `free` after.
    ///
    /// # Errors
    ///
    /// Any [`GraphicError`](daux_graphics::GraphicError). A lost device or a surface that
    /// could not be acquired belongs here; the editor keeps the error for the host to read
    /// rather than tearing itself down mid-frame.
    fn paint(&mut self, ctx: &egui::Context, output: egui::FullOutput) -> DauxGraphicResult<()>;

    /// [main-thread] Releases everything [`open`](Self::open) created. Must be idempotent.
    fn close(&mut self);
}

/// A painter that runs egui in full but draws nothing.
///
/// This is not a stub: it builds the same [`egui::RawInput`], runs the same layout and
/// interaction pass and performs the same tessellation as a real painter, then discards the
/// triangles. That makes it exactly the right thing for
///
/// * unit and integration tests of editor logic, which must not need a GPU,
/// * `daux validate`-style checks that an editor opens, lays out and closes cleanly,
/// * and a headless preview host.
///
/// It reports [`GraphicRenderer::Software`], which is honest: no GPU is involved. What it
/// does not do is put anything on screen.
///
/// [main-thread]
#[derive(Debug, Default)]
pub struct HeadlessPainter {
    opened: usize,
    closed: usize,
    frames: usize,
    size: PhysicalSize,
    last_primitives: usize,
    last_textures_set: usize,
    tessellate: bool,
}

impl HeadlessPainter {
    /// [main-thread] A painter that runs the full tessellation pass and throws the result
    /// away.
    ///
    /// Tessellating costs real time but exercises the code path a GPU painter would take,
    /// which is what makes a test written against this painter meaningful.
    #[must_use]
    pub fn new() -> Self {
        Self {
            tessellate: true,
            ..Self::default()
        }
    }

    /// [main-thread] A painter that skips tessellation.
    ///
    /// Faster, and enough when the test only cares that a frame ran and that input reached
    /// the widgets.
    #[must_use]
    pub fn without_tessellation() -> Self {
        Self::default()
    }

    /// [main-thread] How many times the editor opened this painter.
    #[must_use]
    pub const fn opens(&self) -> usize {
        self.opened
    }

    /// [main-thread] How many times the editor closed this painter.
    #[must_use]
    pub const fn closes(&self) -> usize {
        self.closed
    }

    /// [main-thread] How many frames have been painted since construction.
    #[must_use]
    pub const fn frames(&self) -> usize {
        self.frames
    }

    /// [main-thread] The size the painter was last told about.
    #[must_use]
    pub const fn size(&self) -> PhysicalSize {
        self.size
    }

    /// [main-thread] How many clipped primitives the last frame tessellated to.
    ///
    /// Always `0` when built with [`without_tessellation`](Self::without_tessellation).
    #[must_use]
    pub const fn last_primitives(&self) -> usize {
        self.last_primitives
    }

    /// [main-thread] How many textures the last frame asked to be uploaded.
    ///
    /// The first frame of an editor's life is always at least one: the font atlas.
    #[must_use]
    pub const fn last_textures_set(&self) -> usize {
        self.last_textures_set
    }
}

impl EguiPainter for HeadlessPainter {
    fn profile(&self) -> GraphicProfile {
        profile(GraphicRenderer::Software)
    }

    fn open(&mut self, ctx: &mut GraphicContext<'_>) -> DauxGraphicResult<()> {
        self.opened += 1;
        self.size = ctx.size();
        Ok(())
    }

    fn resize(&mut self, size: PhysicalSize) -> DauxGraphicResult<()> {
        self.size = size;
        Ok(())
    }

    fn paint(&mut self, ctx: &egui::Context, output: egui::FullOutput) -> DauxGraphicResult<()> {
        let egui::FullOutput {
            shapes,
            pixels_per_point,
            mut textures_delta,
            ..
        } = output;

        self.frames += 1;
        self.last_textures_set = textures_delta.set.len();
        self.last_primitives = if self.tessellate {
            ctx.tessellate(shapes, pixels_per_point).len()
        } else {
            0
        };

        // Not a formality: `TexturesDelta` asserts on drop that its contents were handled.
        // A painter that draws nothing still has to say so, or it takes the host down.
        textures_delta.clear();
        Ok(())
    }

    fn close(&mut self) {
        self.closed += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use daux_graphics::{GraphicCapabilities, HostGraphicCaps};

    #[test]
    fn the_offered_profile_is_one_every_host_can_provide() {
        for renderer in [
            GraphicRenderer::Wgpu,
            GraphicRenderer::OpenGl,
            GraphicRenderer::Software,
        ] {
            let p = profile(renderer);
            assert_eq!(p.framework, GraphicFramework::Egui);
            assert_eq!(p.renderer, renderer);
            assert!(p.is_fallback(), "{p} must be an embedded-surface profile");

            let caps = GraphicCapabilities::new().with(p);
            caps.validate()
                .unwrap_or_else(|e| panic!("{p} should validate: {e}"));
            assert_eq!(caps.negotiate(&HostGraphicCaps::in_process()), Some(p));
        }
    }

    #[test]
    fn the_headless_painter_reports_software() {
        assert_eq!(
            HeadlessPainter::new().profile(),
            profile(GraphicRenderer::Software)
        );
        assert_eq!(HeadlessPainter::new().frames(), 0);
        assert_eq!(HeadlessPainter::default().last_primitives(), 0);
    }

    #[test]
    fn discarding_a_frame_clears_the_texture_delta_instead_of_asserting() {
        // `TexturesDelta` asserts on drop when it still holds unapplied deltas, so this test
        // is what stops a "does nothing" painter from taking a host down on its first frame.
        let ctx = egui::Context::default();
        let mut painter = HeadlessPainter::new();
        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.label("the font atlas is uploaded on the first frame");
        });
        assert!(
            !output.textures_delta.set.is_empty(),
            "the first frame must really carry a delta, or this test proves nothing"
        );
        painter.paint(&ctx, output).expect("headless paint");

        assert_eq!(painter.frames(), 1);
        assert!(painter.last_textures_set() >= 1);
        assert!(
            painter.last_primitives() > 0,
            "a label must tessellate to at least one primitive"
        );
    }

    #[test]
    fn skipping_tessellation_still_handles_the_texture_delta() {
        let ctx = egui::Context::default();
        let mut painter = HeadlessPainter::without_tessellation();
        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.label("hello");
        });
        painter.paint(&ctx, output).expect("headless paint");
        assert_eq!(painter.frames(), 1);
        assert_eq!(painter.last_primitives(), 0);
    }
}
