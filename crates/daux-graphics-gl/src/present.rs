//! Getting a finished [`SoftwareFramebuffer`] onto the host's surface.

use daux_graphics::{
    DauxGraphicResult, GraphicContext, GraphicError, GraphicFramework, GraphicProfile,
    GraphicRenderer, PhysicalSize, PresentationMode,
};
use glow::HasContext as _;

use crate::{GlBlitter, GlVersion, Scaling, SoftwareFramebuffer, Viewport};

/// [main-thread] The profile this backend offers for a given renderer.
///
/// Always [`GraphicFramework::Custom`] — this crate draws pixels, it does not lay out widgets —
/// and always [`PresentationMode::EmbeddedSurface`], the mode every host can provide and the
/// one `daux-graphics` requires as a fallback.
#[must_use]
pub fn profile(renderer: GraphicRenderer) -> GraphicProfile {
    GraphicProfile::new(
        GraphicFramework::Custom,
        renderer,
        PresentationMode::EmbeddedSurface,
    )
}

/// Where an editor's pixels go once they are drawn.
///
/// Two implementations ship here: [`GlPresenter`], which uploads the frame and blits it with
/// OpenGL, and [`NullPresenter`], which throws it away. A host with its own compositor can
/// write a third.
///
/// # Contract
///
/// [`SoftwareEditor`](crate::SoftwareEditor) calls these in order: [`open`](Self::open), then
/// any number of [`resize`](Self::resize) and [`present`](Self::present), then
/// [`close`](Self::close). `open` is never called twice without an intervening `close`, and
/// `close` must be idempotent.
///
/// [main-thread]
pub trait Presenter {
    /// [main-thread] The framework/renderer/presentation combination this presenter honours.
    fn profile(&self) -> GraphicProfile;

    /// [main-thread] Prepares to draw into `ctx`'s window.
    ///
    /// # Errors
    ///
    /// Any [`GraphicError`]. The editor reports the failure and stays closed; `close` is not
    /// called for a failed `open`.
    fn open(&mut self, ctx: &mut GraphicContext<'_>) -> DauxGraphicResult<()>;

    /// [main-thread] The host's surface changed size, in physical pixels.
    ///
    /// Never called with an empty size: the editor rejects those first.
    ///
    /// # Errors
    ///
    /// Any [`GraphicError`]; the host keeps the previous size.
    fn resize(&mut self, size: PhysicalSize) -> DauxGraphicResult<()>;

    /// [main-thread] Puts one finished frame on screen.
    ///
    /// # Errors
    ///
    /// Any [`GraphicError`]. A lost context belongs here; the editor keeps the error for the
    /// host to read rather than tearing itself down mid-frame.
    fn present(&mut self, frame: &SoftwareFramebuffer) -> DauxGraphicResult<()>;

    /// [main-thread] Releases everything `open` created. Must be idempotent.
    fn close(&mut self);
}

/// A presenter that draws nothing.
///
/// Not a stub — it records what it was given, which makes it the right thing for tests of
/// editor logic, for a headless preview host, and for a `daux validate`-style check that an
/// editor opens, draws and closes cleanly on a machine with no GPU and no window system.
///
/// [main-thread]
#[derive(Debug, Default)]
pub struct NullPresenter {
    opened: usize,
    closed: usize,
    frames: usize,
    size: PhysicalSize,
    last_frame_size: PhysicalSize,
    last_pixel: Option<[u8; 4]>,
}

impl NullPresenter {
    /// [main-thread] A presenter that discards every frame.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            opened: 0,
            closed: 0,
            frames: 0,
            size: PhysicalSize::ZERO,
            last_frame_size: PhysicalSize::ZERO,
            last_pixel: None,
        }
    }

    /// [main-thread] How many times the editor opened this presenter.
    #[must_use]
    pub const fn opens(&self) -> usize {
        self.opened
    }

    /// [main-thread] How many times the editor closed it.
    #[must_use]
    pub const fn closes(&self) -> usize {
        self.closed
    }

    /// [main-thread] How many frames have been presented.
    #[must_use]
    pub const fn frames(&self) -> usize {
        self.frames
    }

    /// [main-thread] The surface size the presenter was last told about.
    #[must_use]
    pub const fn size(&self) -> PhysicalSize {
        self.size
    }

    /// [main-thread] The size of the last frame presented.
    #[must_use]
    pub const fn last_frame_size(&self) -> PhysicalSize {
        self.last_frame_size
    }

    /// [main-thread] The top-left pixel of the last frame, so a test can tell that a draw
    /// callback really ran and really wrote something.
    #[must_use]
    pub const fn last_pixel(&self) -> Option<[u8; 4]> {
        self.last_pixel
    }
}

impl Presenter for NullPresenter {
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

    fn present(&mut self, frame: &SoftwareFramebuffer) -> DauxGraphicResult<()> {
        self.frames += 1;
        self.last_frame_size = frame.size();
        self.last_pixel = frame.pixel(0, 0);
        Ok(())
    }

    fn close(&mut self) {
        self.closed += 1;
    }
}

/// The platform half of an OpenGL editor: context creation, currency and buffer swapping.
///
/// `glow` loads GL entry points; it does not create contexts, and neither does this crate. On
/// Windows that is WGL, on macOS CGL, on Linux GLX or EGL — all of which need the host's
/// window handle and none of which belong in a backend crate. A host or plug-in supplies this
/// trait, typically over `glutin`, and [`GlPresenter`] does the rest.
///
/// # Contract
///
/// [`context`](Self::context) must return the same [`glow::Context`] for the lifetime of the
/// implementation, and it must be the one [`make_current`](Self::make_current) makes current.
/// Handing back a different context than the one that is current is what turns a blit into a
/// black window and, on some drivers, a crash.
///
/// [main-thread]
pub trait GlSurface {
    /// [main-thread] The loaded GL entry points.
    fn context(&self) -> &glow::Context;

    /// [main-thread] Makes this surface's context current on the calling thread.
    ///
    /// # Errors
    ///
    /// Any [`GraphicError`] — typically [`Renderer`](daux_graphics::GraphicErrorKind::Renderer)
    /// when the context was lost.
    fn make_current(&mut self) -> DauxGraphicResult<()>;

    /// [main-thread] Presents what was drawn since the last swap.
    ///
    /// # Errors
    ///
    /// Any [`GraphicError`].
    fn swap_buffers(&mut self) -> DauxGraphicResult<()>;

    /// [main-thread] Tells the platform layer the surface changed size.
    ///
    /// Does nothing by default: on Win32 and X11 the drawable follows the window on its own,
    /// and only EGL and some Wayland setups need to be told.
    ///
    /// # Errors
    ///
    /// Any [`GraphicError`].
    fn resize(&mut self, size: PhysicalSize) -> DauxGraphicResult<()> {
        let _ = size;
        Ok(())
    }
}

/// Presents software frames through OpenGL.
///
/// Owns a [`GlBlitter`] built at [`open`](Presenter::open) and destroyed at
/// [`close`](Presenter::close) — with the context made current first, which is the only time
/// GL objects may be deleted, and the reason `close` is where it happens rather than `Drop`.
///
/// [main-thread]
pub struct GlPresenter<S: GlSurface> {
    surface: S,
    blitter: Option<GlBlitter>,
    version: Option<GlVersion>,
    size: PhysicalSize,
    scaling: Scaling,
    clear: [f32; 4],
}

impl<S: GlSurface> core::fmt::Debug for GlPresenter<S> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("GlPresenter")
            .field("version", &self.version)
            .field("size", &self.size)
            .field("scaling", &self.scaling)
            .field("open", &self.blitter.is_some())
            .finish_non_exhaustive()
    }
}

impl<S: GlSurface> GlPresenter<S> {
    /// [main-thread] Builds a presenter over a platform surface.
    ///
    /// Nothing GL-related happens until [`open`](Presenter::open), so constructing one costs
    /// nothing and cannot fail.
    #[must_use]
    pub const fn new(surface: S) -> Self {
        Self {
            surface,
            blitter: None,
            version: None,
            size: PhysicalSize::ZERO,
            scaling: Scaling::Stretch,
            clear: [0.0, 0.0, 0.0, 1.0],
        }
    }

    /// [main-thread] Chooses how a frame is fitted into a surface of a different size.
    #[must_use]
    pub const fn with_scaling(mut self, scaling: Scaling) -> Self {
        self.scaling = scaling;
        self
    }

    /// [main-thread] Sets the colour behind the frame, which is what shows in the letterbox
    /// bars under [`Scaling::Contain`].
    #[must_use]
    pub const fn with_clear_color(mut self, rgba: [f32; 4]) -> Self {
        self.clear = rgba;
        self
    }

    /// [main-thread] The GL version reported by the context, once it has been opened.
    #[must_use]
    pub const fn version(&self) -> Option<GlVersion> {
        self.version
    }

    /// [main-thread] The platform surface, for anything this presenter does not expose.
    #[must_use]
    pub const fn surface(&self) -> &S {
        &self.surface
    }
}

impl<S: GlSurface> Presenter for GlPresenter<S> {
    fn profile(&self) -> GraphicProfile {
        profile(GraphicRenderer::OpenGl)
    }

    fn open(&mut self, ctx: &mut GraphicContext<'_>) -> DauxGraphicResult<()> {
        if self.blitter.is_some() {
            return Err(GraphicError::invalid_state(
                "the OpenGL presenter is already open",
            ));
        }
        self.surface.make_current()?;
        self.size = ctx.size();

        let version = GlVersion::from_glow(self.surface.context().version());
        // SAFETY: `make_current` above succeeded, so this surface's context is current on this
        // thread, and `GlSurface` guarantees `context()` returns that same context. The
        // blitter is destroyed in `close`, which makes the context current again first.
        let blitter = unsafe { GlBlitter::new(self.surface.context(), version)? };
        self.version = Some(version);
        self.blitter = Some(blitter);
        Ok(())
    }

    fn resize(&mut self, size: PhysicalSize) -> DauxGraphicResult<()> {
        self.size = size;
        self.surface.resize(size)
    }

    fn present(&mut self, frame: &SoftwareFramebuffer) -> DauxGraphicResult<()> {
        let Some(blitter) = self.blitter.as_mut() else {
            return Err(GraphicError::invalid_state(
                "the OpenGL presenter was asked to present before it was opened",
            ));
        };
        self.surface.make_current()?;

        // SAFETY: `make_current` succeeded immediately above, so the context is current on
        // this thread, and it is the one this blitter was created with — `open` built it from
        // `self.surface.context()` and nothing can replace the surface afterwards.
        unsafe {
            blitter.upload(self.surface.context(), frame)?;
            blitter.draw(
                self.surface.context(),
                self.size,
                Viewport::for_scaling(self.scaling, frame.size(), self.size),
                self.clear,
            );
        }
        self.surface.swap_buffers()
    }

    fn close(&mut self) {
        let Some(blitter) = self.blitter.take() else {
            // Idempotent, as the trait requires.
            return;
        };
        self.version = None;
        if self.surface.make_current().is_err() {
            // The context is gone, so its objects are gone with it. There is nothing to
            // delete and nowhere to report it: `close` cannot fail, and a lost context during
            // teardown is the normal way a host window disappears.
            return;
        }
        // SAFETY: `make_current` succeeded, so this blitter's own context is current on this
        // thread. `destroy` consumes the blitter, and it has already been taken out of
        // `self`, so nothing can use it afterwards.
        unsafe { blitter.destroy(self.surface.context()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use daux_graphics::{GraphicCapabilities, HostGraphicCaps, ScaleFactor, WindowTarget};
    use daux_host_services::HostServices;

    fn context<'a>(host: &'a HostServices, profile: GraphicProfile) -> GraphicContext<'a> {
        GraphicContext::new(
            WindowTarget::win32(0x2000).expect("a non-null hwnd is a valid target"),
            PhysicalSize::new(640, 480),
            ScaleFactor::ONE,
            profile,
            host,
        )
    }

    #[test]
    fn both_offered_profiles_are_ones_every_host_can_provide() {
        for renderer in [GraphicRenderer::OpenGl, GraphicRenderer::Software] {
            let p = profile(renderer);
            assert_eq!(p.framework, GraphicFramework::Custom);
            assert_eq!(p.renderer, renderer);
            assert!(p.is_fallback(), "{p} is not an embedded-surface profile");

            let caps = GraphicCapabilities::new().with(p);
            caps.validate()
                .unwrap_or_else(|e| panic!("{p} should validate: {e}"));
            assert_eq!(caps.negotiate(&HostGraphicCaps::in_process()), Some(p));
        }
    }

    #[test]
    fn the_null_presenter_records_what_it_was_handed() {
        let host = HostServices::default();
        let mut presenter = NullPresenter::new();
        assert_eq!(presenter.profile(), profile(GraphicRenderer::Software));

        presenter
            .open(&mut context(&host, presenter.profile()))
            .expect("open");
        assert_eq!(presenter.opens(), 1);
        assert_eq!(presenter.size(), PhysicalSize::new(640, 480));

        let mut frame = SoftwareFramebuffer::new(PhysicalSize::new(4, 4)).expect("small");
        frame.fill([1, 2, 3, 4]);
        presenter.present(&frame).expect("present");
        assert_eq!(presenter.frames(), 1);
        assert_eq!(presenter.last_frame_size(), PhysicalSize::new(4, 4));
        assert_eq!(presenter.last_pixel(), Some([1, 2, 3, 4]));

        presenter
            .resize(PhysicalSize::new(100, 50))
            .expect("resize");
        assert_eq!(presenter.size(), PhysicalSize::new(100, 50));

        presenter.close();
        assert_eq!(presenter.closes(), 1);
        assert_eq!(NullPresenter::default().frames(), 0);
    }

    #[test]
    fn presenting_an_empty_frame_is_recorded_rather_than_refused() {
        // A minimised host window produces a zero-sized frame every tick; that is normal, not
        // an error, and a presenter that rejects it fills the host's log with noise.
        let mut presenter = NullPresenter::new();
        let frame = SoftwareFramebuffer::empty();
        presenter.present(&frame).expect("an empty frame is fine");
        assert_eq!(presenter.frames(), 1);
        assert_eq!(presenter.last_pixel(), None);
    }
}
