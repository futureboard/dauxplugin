//! [`DauxGraphic`] over a CPU framebuffer and a [`Presenter`].

use daux_graphics::{
    DauxGraphic, DauxGraphicResult, GraphicCapabilities, GraphicContext, GraphicDescriptor,
    GraphicError, LogicalSize, PhysicalSize, ScaleFactor,
};

use crate::{Presenter, SoftwareFramebuffer};

/// What a draw callback is told about the frame it is drawing.
///
/// [main-thread]
#[derive(Clone, Copy, PartialEq, Debug)]
#[non_exhaustive]
pub struct FrameInfo {
    /// The framebuffer's size in physical pixels. The same as
    /// [`SoftwareFramebuffer::size`], repeated here so a callback that only reads the info
    /// does not have to borrow the buffer twice.
    pub size: PhysicalSize,
    /// Physical pixels per logical pixel. A callback that lays out in logical units
    /// multiplies by this.
    pub scale: ScaleFactor,
    /// How many frames this editor has drawn since it was opened. Reset by every `open`,
    /// which is what makes an animation restart rather than jump when an editor is reopened.
    pub index: u64,
}

/// An editor that rasterises into main memory and hands the result to a [`Presenter`].
///
/// This is the fallback path: no GPU device, no shader compilation, nothing that can fail
/// because a driver is out of date. It is also the shape a custom renderer wants — a plug-in
/// that draws its own pixels writes a draw callback and never touches OpenGL at all.
///
/// # What it owns
///
/// One [`SoftwareFramebuffer`], one presenter, and the draw callback. It owns **no** DSP
/// state: the callback captures whatever the plug-in wants it to see, typically the shared
/// `Arc<dyn Params>` and the reader end of a `daux-rt` channel. Dropping the editor therefore
/// cannot change what the plug-in outputs.
///
/// # Lifecycle
///
/// The framebuffer is allocated at [`open`](DauxGraphic::open) and released at
/// [`close`](DauxGraphic::close), so an editor that is never opened costs one small struct and
/// an editor that is opened and closed repeatedly holds no memory in between.
///
/// # Example
///
/// ```
/// use daux_graphics::{DauxGraphic, LogicalSize};
/// use daux_graphics_gl::{NullPresenter, SoftwareEditor};
///
/// let mut editor = SoftwareEditor::new(
///     NullPresenter::new(),
///     LogicalSize::new(320.0, 240.0),
///     |frame, info| {
///         frame.fill([16, 16, 20, 255]);
///         let _ = info.index;
///     },
/// );
/// assert_eq!(editor.descriptor().preferred_size, LogicalSize::new(320.0, 240.0));
/// ```
///
/// [main-thread]
pub struct SoftwareEditor<P: Presenter> {
    presenter: P,
    frame: SoftwareFramebuffer,
    draw: Box<dyn FnMut(&mut SoftwareFramebuffer, FrameInfo)>,
    descriptor: GraphicDescriptor,
    size: PhysicalSize,
    scale: ScaleFactor,
    index: u64,
    open: bool,
    present_error: Option<GraphicError>,
}

impl<P: Presenter> core::fmt::Debug for SoftwareEditor<P> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SoftwareEditor")
            .field("profile", &self.presenter.profile())
            .field("open", &self.open)
            .field("size", &self.size)
            .field("frames", &self.index)
            .field("present_error", &self.present_error)
            .finish_non_exhaustive()
    }
}

impl<P: Presenter> SoftwareEditor<P> {
    /// [main-thread] Builds a fixed-size editor of `size` logical pixels.
    pub fn new(
        presenter: P,
        size: LogicalSize,
        draw: impl FnMut(&mut SoftwareFramebuffer, FrameInfo) + 'static,
    ) -> Self {
        let capabilities = GraphicCapabilities::new().with(presenter.profile());
        Self::with_descriptor(
            presenter,
            GraphicDescriptor::fixed(capabilities, size),
            draw,
        )
    }

    /// [main-thread] Builds an editor with an explicit descriptor, for a resizable editor or
    /// one that keeps an aspect ratio.
    ///
    /// The descriptor's capabilities are replaced with the presenter's single profile: an
    /// editor cannot honour a profile its presenter does not implement, and advertising one
    /// produces a negotiation the host then cannot satisfy.
    pub fn with_descriptor(
        presenter: P,
        mut descriptor: GraphicDescriptor,
        draw: impl FnMut(&mut SoftwareFramebuffer, FrameInfo) + 'static,
    ) -> Self {
        descriptor.capabilities = GraphicCapabilities::new().with(presenter.profile());
        let size = descriptor.preferred_size.to_physical(ScaleFactor::ONE);
        Self {
            presenter,
            frame: SoftwareFramebuffer::empty(),
            draw: Box::new(draw),
            descriptor,
            size,
            scale: ScaleFactor::ONE,
            index: 0,
            open: false,
            present_error: None,
        }
    }

    /// [main-thread] The presenter, for anything this adapter does not expose.
    #[must_use]
    pub const fn presenter(&self) -> &P {
        &self.presenter
    }

    /// [main-thread] The presenter, mutably.
    pub const fn presenter_mut(&mut self) -> &mut P {
        &mut self.presenter
    }

    /// [main-thread] The framebuffer the last frame was drawn into.
    ///
    /// Empty while the editor is closed.
    #[must_use]
    pub const fn frame(&self) -> &SoftwareFramebuffer {
        &self.frame
    }

    /// [main-thread] `true` between a successful `open` and the matching `close`.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.open
    }

    /// [main-thread] How many frames have been drawn since the editor was opened.
    #[must_use]
    pub const fn frame_index(&self) -> u64 {
        self.index
    }

    /// [main-thread] The last presentation failure, removing it.
    ///
    /// [`DauxGraphic::tick`] cannot return an error — a host's idle callback has nowhere to
    /// put one — so a lost context is kept here instead of being swallowed. A host or preview
    /// harness should drain this after ticking; a plug-in that never looks simply keeps
    /// trying, which is right for a transient failure.
    #[must_use]
    pub fn take_present_error(&mut self) -> Option<GraphicError> {
        self.present_error.take()
    }

    /// [main-thread] Draws one frame and presents it.
    ///
    /// Split out from [`DauxGraphic::tick`] so a preview harness can see the failure directly
    /// instead of fishing it out with [`take_present_error`](Self::take_present_error).
    ///
    /// # Errors
    ///
    /// [`InvalidState`](daux_graphics::GraphicErrorKind::InvalidState) when the editor is not
    /// open, and whatever the presenter returned otherwise.
    pub fn run_frame(&mut self) -> DauxGraphicResult<()> {
        if !self.open {
            return Err(GraphicError::invalid_state(
                "the editor was asked to draw before it was opened",
            ));
        }
        let info = FrameInfo {
            size: self.frame.size(),
            scale: self.scale,
            index: self.index,
        };
        (self.draw)(&mut self.frame, info);
        self.index += 1;
        self.presenter.present(&self.frame)
    }
}

impl<P: Presenter> DauxGraphic for SoftwareEditor<P> {
    fn descriptor(&self) -> GraphicDescriptor {
        self.descriptor.clone()
    }

    fn capabilities(&self) -> GraphicCapabilities {
        GraphicCapabilities::new().with(self.presenter.profile())
    }

    fn open(&mut self, ctx: &mut GraphicContext<'_>) -> DauxGraphicResult<()> {
        if self.open {
            return Err(GraphicError::invalid_state(
                "the software editor is already open",
            ));
        }
        let offered = self.presenter.profile();
        if ctx.profile() != offered {
            return Err(GraphicError::negotiation(
                "the host chose a graphic profile this editor never offered",
            ));
        }

        self.size = ctx.size();
        self.scale = ctx.scale();
        // Allocated before the presenter is opened: a size the framebuffer refuses must not
        // leave a half-opened presenter behind, and this is the step that can fail.
        self.frame.resize(self.size)?;
        self.presenter.open(ctx)?;
        self.index = 0;
        self.open = true;
        Ok(())
    }

    fn resize(&mut self, size: PhysicalSize) -> DauxGraphicResult<()> {
        if size.is_empty() {
            return Err(GraphicError::invalid_argument(
                "an editor cannot be resized to zero",
            ));
        }
        if self.open {
            // Same order as `open`: the fallible allocation first, so a refused size leaves
            // the presenter believing the size it can actually draw.
            self.frame.resize(size)?;
            self.presenter.resize(size)?;
        }
        self.size = size;
        Ok(())
    }

    fn scale_factor_changed(&mut self, scale: ScaleFactor) {
        self.scale = scale;
    }

    fn tick(&mut self) {
        if !self.open {
            return;
        }
        if let Err(e) = self.run_frame() {
            self.present_error = Some(e);
        }
    }

    fn close(&mut self) {
        if !self.open {
            // Idempotent, as the trait requires.
            return;
        }
        self.open = false;
        self.presenter.close();
        // Releasing the pixels matters: a DAW with fifty instances whose editors are all
        // closed should not still be holding fifty megabytes of framebuffers.
        self.frame = SoftwareFramebuffer::empty();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NullPresenter, Presenter, profile};
    use daux_graphics::{GraphicErrorKind, GraphicProfile, GraphicRenderer, WindowTarget};
    use daux_host_services::HostServices;
    use std::cell::Cell;
    use std::rc::Rc;

    fn software() -> GraphicProfile {
        profile(GraphicRenderer::Software)
    }

    fn context<'a>(host: &'a HostServices, profile: GraphicProfile) -> GraphicContext<'a> {
        GraphicContext::new(
            WindowTarget::win32(0x3000).expect("a non-null hwnd is a valid target"),
            PhysicalSize::new(200, 100),
            ScaleFactor::new(2.0).expect("2.0 is in range"),
            profile,
            host,
        )
    }

    /// An editor whose draw callback counts frames and paints a recognisable colour.
    fn counting_editor() -> (SoftwareEditor<NullPresenter>, Rc<Cell<usize>>) {
        let draws = Rc::new(Cell::new(0usize));
        let seen = Rc::clone(&draws);
        let editor = SoftwareEditor::new(
            NullPresenter::new(),
            LogicalSize::new(100.0, 50.0),
            move |frame, _info| {
                seen.set(seen.get() + 1);
                frame.fill([7, 8, 9, 255]);
            },
        );
        (editor, draws)
    }

    #[test]
    fn the_editor_advertises_exactly_its_presenters_profile() {
        let (editor, _) = counting_editor();
        let caps = editor.capabilities();
        assert_eq!(caps.profiles(), [software()]);
        assert_eq!(editor.descriptor().capabilities, caps);
        assert!(caps.has_fallback());
    }

    #[test]
    fn a_full_cycle_allocates_draws_presents_and_then_releases_the_pixels() {
        let host = HostServices::default();
        let (mut editor, draws) = counting_editor();
        assert!(
            editor.frame().is_empty(),
            "an unopened editor must not have allocated a framebuffer"
        );

        editor.open(&mut context(&host, software())).expect("open");
        assert_eq!(editor.frame().size(), PhysicalSize::new(200, 100));

        editor.tick();
        editor.tick();
        assert_eq!(draws.get(), 2);
        assert_eq!(editor.presenter().frames(), 2);
        assert_eq!(
            editor.presenter().last_pixel(),
            Some([7, 8, 9, 255]),
            "the presenter received the pixels the callback wrote"
        );
        assert_eq!(editor.frame_index(), 2);

        editor.close();
        assert!(
            editor.frame().is_empty(),
            "closing must release the framebuffer, not keep it for a reopen that may never come"
        );
        assert_eq!(editor.presenter().closes(), 1);
    }

    #[test]
    fn the_draw_callback_is_told_the_size_and_scale_the_host_chose() {
        let host = HostServices::default();
        let seen = Rc::new(Cell::new(None::<FrameInfo>));
        let recorder = Rc::clone(&seen);
        let mut editor = SoftwareEditor::new(
            NullPresenter::new(),
            LogicalSize::new(100.0, 50.0),
            move |_frame, info| recorder.set(Some(info)),
        );

        editor.open(&mut context(&host, software())).expect("open");
        editor.tick();
        let info = seen.get().expect("the callback ran");
        assert_eq!(info.size, PhysicalSize::new(200, 100));
        assert_eq!(info.scale.get(), 2.0);
        assert_eq!(
            info.index, 0,
            "the first frame of an open editor is frame 0"
        );

        editor.tick();
        assert_eq!(seen.get().expect("ran again").index, 1);
    }

    #[test]
    fn reopening_restarts_the_frame_counter_so_animations_do_not_jump() {
        let host = HostServices::default();
        let (mut editor, _) = counting_editor();
        editor.open(&mut context(&host, software())).expect("open");
        editor.tick();
        editor.tick();
        assert_eq!(editor.frame_index(), 2);
        editor.close();

        editor
            .open(&mut context(&host, software()))
            .expect("reopen");
        assert_eq!(editor.frame_index(), 0);
    }

    #[test]
    fn opening_twice_is_refused_and_the_presenter_is_not_opened_again() {
        let host = HostServices::default();
        let (mut editor, _) = counting_editor();
        editor.open(&mut context(&host, software())).expect("first");

        let err = editor
            .open(&mut context(&host, software()))
            .expect_err("a second open without a close is a host bug");
        assert_eq!(err.kind(), GraphicErrorKind::InvalidState);
        assert_eq!(editor.presenter().opens(), 1);
    }

    #[test]
    fn opening_with_a_profile_that_was_never_offered_is_refused_before_anything_is_allocated() {
        let host = HostServices::default();
        let (mut editor, _) = counting_editor();
        let wrong = profile(GraphicRenderer::OpenGl);

        let err = editor
            .open(&mut context(&host, wrong))
            .expect_err("the host picked something we cannot draw");
        assert_eq!(err.kind(), GraphicErrorKind::Negotiation);
        assert!(!editor.is_open());
        assert!(editor.frame().is_empty());
        assert_eq!(editor.presenter().opens(), 0);
    }

    #[test]
    fn closing_is_idempotent_and_closing_before_opening_does_nothing() {
        let host = HostServices::default();
        let (mut editor, _) = counting_editor();
        editor.close();
        assert_eq!(editor.presenter().closes(), 0);

        editor.open(&mut context(&host, software())).expect("open");
        editor.close();
        editor.close();
        assert_eq!(editor.presenter().closes(), 1);
    }

    #[test]
    fn ticking_while_closed_draws_nothing() {
        let (mut editor, draws) = counting_editor();
        editor.tick();
        editor.tick();
        assert_eq!(draws.get(), 0);
        assert_eq!(editor.presenter().frames(), 0);
        assert_eq!(
            editor
                .run_frame()
                .expect_err("drawing a closed editor is a state error")
                .kind(),
            GraphicErrorKind::InvalidState
        );
    }

    #[test]
    fn a_resize_reallocates_the_framebuffer_and_tells_the_presenter() {
        let host = HostServices::default();
        let (mut editor, _) = counting_editor();
        editor.open(&mut context(&host, software())).expect("open");

        editor.resize(PhysicalSize::new(64, 32)).expect("resize");
        assert_eq!(editor.frame().size(), PhysicalSize::new(64, 32));
        assert_eq!(editor.presenter().size(), PhysicalSize::new(64, 32));

        editor.tick();
        assert_eq!(
            editor.presenter().last_frame_size(),
            PhysicalSize::new(64, 32)
        );
    }

    #[test]
    fn resizing_to_zero_is_refused_and_leaves_the_editor_drawable() {
        let host = HostServices::default();
        let (mut editor, _) = counting_editor();
        editor.open(&mut context(&host, software())).expect("open");

        let err = editor
            .resize(PhysicalSize::new(300, 0))
            .expect_err("a zero-height surface cannot be drawn into");
        assert_eq!(err.kind(), GraphicErrorKind::InvalidArgument);
        assert_eq!(editor.frame().size(), PhysicalSize::new(200, 100));

        editor.tick();
        assert_eq!(editor.presenter().frames(), 1, "the editor still draws");
    }

    #[test]
    fn a_hostile_resize_is_refused_without_disturbing_the_current_frame() {
        // A host that reports a nonsense window size must not be able to make a plug-in
        // attempt a 64-exbibyte allocation and abort the process.
        let host = HostServices::default();
        let (mut editor, _) = counting_editor();
        editor.open(&mut context(&host, software())).expect("open");

        let err = editor
            .resize(PhysicalSize::new(u32::MAX, u32::MAX))
            .expect_err("that size must be refused");
        assert_eq!(err.kind(), GraphicErrorKind::CapacityExceeded);
        assert_eq!(
            editor.frame().size(),
            PhysicalSize::new(200, 100),
            "the editor must still be showing its old, valid frame"
        );
        assert_eq!(
            editor.presenter().size(),
            PhysicalSize::new(200, 100),
            "the presenter must not have been told about a size it cannot be given"
        );

        editor.tick();
        assert_eq!(editor.presenter().frames(), 1);
    }

    #[test]
    fn opening_at_a_hostile_size_fails_without_leaving_a_half_open_presenter() {
        // A host claiming a window larger than the framebuffer ceiling.
        let host = HostServices::default();
        let (mut editor, _) = counting_editor();
        let mut ctx = GraphicContext::new(
            WindowTarget::win32(0x3000).expect("valid"),
            PhysicalSize::new(u32::MAX, 4),
            ScaleFactor::ONE,
            software(),
            &host,
        );

        let err = editor
            .open(&mut ctx)
            .expect_err("that size must be refused");
        assert_eq!(err.kind(), GraphicErrorKind::CapacityExceeded);
        assert!(!editor.is_open());
        assert_eq!(
            editor.presenter().opens(),
            0,
            "the presenter must not have been opened for an editor that failed to open"
        );
    }

    #[test]
    fn a_presenter_failure_is_kept_rather_than_swallowed_by_tick() {
        struct Failing(NullPresenter);

        impl Presenter for Failing {
            fn profile(&self) -> GraphicProfile {
                self.0.profile()
            }
            fn open(&mut self, ctx: &mut GraphicContext<'_>) -> DauxGraphicResult<()> {
                self.0.open(ctx)
            }
            fn resize(&mut self, size: PhysicalSize) -> DauxGraphicResult<()> {
                self.0.resize(size)
            }
            fn present(&mut self, frame: &SoftwareFramebuffer) -> DauxGraphicResult<()> {
                self.0.present(frame)?;
                Err(GraphicError::new(
                    GraphicErrorKind::Renderer,
                    "the context was lost",
                ))
            }
            fn close(&mut self) {
                self.0.close();
            }
        }

        let host = HostServices::default();
        let mut editor = SoftwareEditor::new(
            Failing(NullPresenter::new()),
            LogicalSize::new(20.0, 20.0),
            |frame, _| frame.fill([1, 1, 1, 1]),
        );
        editor.open(&mut context(&host, software())).expect("open");
        editor.tick();

        let err = editor
            .take_present_error()
            .expect("the failure was recorded");
        assert_eq!(err.kind(), GraphicErrorKind::Renderer);
        assert!(editor.take_present_error().is_none(), "taking it clears it");
        assert!(
            editor.is_open(),
            "a transient presentation failure must not close the editor"
        );
    }

    #[test]
    fn a_descriptors_capabilities_are_replaced_by_the_presenters() {
        let lie = GraphicCapabilities::new().with(GraphicProfile::new(
            daux_graphics::GraphicFramework::Egui,
            GraphicRenderer::Wgpu,
            daux_graphics::PresentationMode::SharedTexture,
        ));
        let editor = SoftwareEditor::with_descriptor(
            NullPresenter::new(),
            GraphicDescriptor::fixed(lie, LogicalSize::new(10.0, 10.0)),
            |_, _| {},
        );
        assert_eq!(editor.capabilities().profiles(), [software()]);
    }
}
