//! [`DauxGraphic`] over an `egui` context and a pluggable painter.

use daux_graphics::{
    DauxGraphic, DauxGraphicResult, GraphicCapabilities, GraphicContext, GraphicDescriptor,
    GraphicError, GraphicFramework, InputEvent, InputResponse, LogicalSize, PhysicalSize,
    ScaleFactor,
};

use crate::{EguiPainter, InputTranslator};

/// An egui editor wearing the DAUx editor interface.
///
/// # What it owns
///
/// One [`egui::Context`], one [`InputTranslator`], one painter, and the closure that draws the
/// plug-in's UI. It owns **no** DSP state and no parameters: the closure captures whatever the
/// plug-in wants it to see — typically a clone of the `Arc<dyn Params>` the processor also
/// reads, and the reader end of a `daux-rt` channel for meters. That is what makes the ninth
/// architectural rule hold by construction: dropping this editor cannot change what the
/// plug-in outputs, because it never had the outputs.
///
/// # Lifecycle
///
/// Nothing is created until [`open`](DauxGraphic::open) and everything the painter made is
/// released by [`close`](DauxGraphic::close), so an editor that is opened and closed a hundred
/// times leaks nothing. The [`egui::Context`] itself survives across opens, which is
/// deliberate: it carries scroll positions, collapsing-header states and text selection, and a
/// user who closes and reopens an editor expects to find it as they left it.
///
/// # Threading
///
/// Every method is `[main-thread]`, and the type is neither `Send` nor `Sync` — `egui::Context`
/// is reference-counted, and so is everything a plug-in editor is built from. The audio thread
/// cannot reach it: `DauxGraphic` is only ever handed out on the main thread.
///
/// # Example
///
/// ```
/// use daux_graphics::{DauxGraphic, LogicalSize};
/// use daux_graphics_egui::{EguiEditor, HeadlessPainter};
///
/// let mut clicks = 0usize;
/// let editor = EguiEditor::new(
///     HeadlessPainter::new(),
///     LogicalSize::new(360.0, 240.0),
///     move |ui| {
///         if ui.button("bypass").clicked() {
///             clicks += 1;
///         }
///     },
/// );
/// assert_eq!(editor.descriptor().preferred_size, LogicalSize::new(360.0, 240.0));
/// ```
pub struct EguiEditor<P: EguiPainter> {
    ctx: egui::Context,
    painter: P,
    input: InputTranslator,
    ui: Box<dyn FnMut(&mut egui::Ui)>,
    descriptor: GraphicDescriptor,
    open: bool,
    paint_error: Option<GraphicError>,
}

impl<P: EguiPainter> std::fmt::Debug for EguiEditor<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EguiEditor")
            .field("profile", &self.painter.profile())
            .field("open", &self.open)
            .field("input", &self.input)
            .field("paint_error", &self.paint_error)
            .finish_non_exhaustive()
    }
}

impl<P: EguiPainter> EguiEditor<P> {
    /// [main-thread] Builds a fixed-size editor of `size` logical pixels.
    pub fn new(painter: P, size: LogicalSize, ui: impl FnMut(&mut egui::Ui) + 'static) -> Self {
        let capabilities = GraphicCapabilities::new().with(painter.profile());
        Self::with_descriptor(painter, GraphicDescriptor::fixed(capabilities, size), ui)
    }

    /// [main-thread] Builds an editor with an explicit descriptor, for a resizable editor or
    /// one that keeps an aspect ratio.
    ///
    /// The descriptor's capabilities are replaced with the painter's single profile: an editor
    /// cannot honour a profile its painter does not implement, and advertising one produces a
    /// negotiation the host then cannot satisfy.
    pub fn with_descriptor(
        painter: P,
        mut descriptor: GraphicDescriptor,
        ui: impl FnMut(&mut egui::Ui) + 'static,
    ) -> Self {
        descriptor.capabilities = GraphicCapabilities::new().with(painter.profile());
        let size = descriptor.preferred_size.to_physical(ScaleFactor::ONE);
        Self {
            ctx: egui::Context::default(),
            painter,
            input: InputTranslator::new(size, ScaleFactor::ONE),
            ui: Box::new(ui),
            descriptor,
            open: false,
            paint_error: None,
        }
    }

    /// [main-thread] The egui context this editor draws with.
    ///
    /// Use it to install a visual style, register fonts or add an image loader before the
    /// editor is opened. It survives across opens, so this only has to be done once.
    #[must_use]
    pub const fn context(&self) -> &egui::Context {
        &self.ctx
    }

    /// [main-thread] The painter, for anything this adapter does not expose.
    #[must_use]
    pub const fn painter(&self) -> &P {
        &self.painter
    }

    /// [main-thread] The painter, mutably.
    pub const fn painter_mut(&mut self) -> &mut P {
        &mut self.painter
    }

    /// [main-thread] `true` between a successful `open` and the matching `close`.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.open
    }

    /// [main-thread] The last painting failure, removing it.
    ///
    /// [`DauxGraphic::tick`] cannot return an error — a host's idle callback has nowhere to
    /// put one — so a lost device or an unacquirable surface is kept here instead of being
    /// swallowed. A host or preview harness should drain this after ticking; a plug-in that
    /// never looks simply keeps trying to paint, which is the right behaviour for a
    /// transient failure.
    #[must_use]
    pub fn take_paint_error(&mut self) -> Option<GraphicError> {
        self.paint_error.take()
    }

    /// [main-thread] Runs one egui frame and hands it to the painter.
    ///
    /// Split out from [`DauxGraphic::tick`] so a preview harness can see the failure directly
    /// instead of fishing it out with [`take_paint_error`](Self::take_paint_error).
    ///
    /// # Errors
    ///
    /// Whatever the painter returned. Nothing else here can fail: an egui pass either runs or
    /// panics inside egui.
    pub fn run_frame(&mut self) -> DauxGraphicResult<()> {
        let raw = self.input.take_raw_input();
        // Split the borrows by hand: the ui closure is `&mut`, the context is `&`.
        let ui = &mut self.ui;
        let output = self.ctx.run_ui(raw, |root| ui(root));
        self.painter.paint(&self.ctx, output)
    }
}

impl<P: EguiPainter> DauxGraphic for EguiEditor<P> {
    fn descriptor(&self) -> GraphicDescriptor {
        self.descriptor.clone()
    }

    fn capabilities(&self) -> GraphicCapabilities {
        GraphicCapabilities::new().with(self.painter.profile())
    }

    fn open(&mut self, ctx: &mut GraphicContext<'_>) -> DauxGraphicResult<()> {
        if self.open {
            return Err(GraphicError::invalid_state(
                "the egui editor is already open",
            ));
        }
        let offered = self.painter.profile();
        if offered.framework != GraphicFramework::Egui {
            return Err(GraphicError::unsupported(
                "the painter claims a framework other than egui, which this editor cannot draw",
            ));
        }
        if ctx.profile() != offered {
            return Err(GraphicError::negotiation(
                "the host chose a graphic profile this editor never offered",
            ));
        }

        self.input.set_size(ctx.size());
        self.input.set_scale(ctx.scale());
        self.painter.open(ctx)?;
        self.open = true;
        Ok(())
    }

    fn resize(&mut self, size: PhysicalSize) -> DauxGraphicResult<()> {
        if size.is_empty() {
            return Err(GraphicError::invalid_argument(
                "an editor cannot be resized to zero",
            ));
        }
        // Recorded whether or not the editor is open: hosts routinely settle on a size before
        // they hand over a window, and that size is the one `open` should use.
        self.input.set_size(size);
        if self.open {
            self.painter.resize(size)?;
        }
        Ok(())
    }

    fn scale_factor_changed(&mut self, scale: ScaleFactor) {
        self.input.set_scale(scale);
    }

    fn on_input(&mut self, event: &InputEvent) -> InputResponse {
        if !self.open {
            return InputResponse::Ignored;
        }
        if !self.input.push(event) {
            // Nothing egui can act on; leave it to the host.
            return InputResponse::Ignored;
        }

        // Whether egui *uses* an event is only known once the frame that sees it has run, so
        // the answer here is necessarily about the previous frame. That is what every egui
        // integration does, and it is right in practice: a widget that wanted the pointer
        // last frame still wants it this frame.
        let wanted = match event {
            InputEvent::PointerMoved { .. }
            | InputEvent::PointerButton { .. }
            | InputEvent::PointerLeft
            | InputEvent::Scroll { .. } => self.ctx.egui_wants_pointer_input(),
            InputEvent::Key { .. } | InputEvent::Text(_) => self.ctx.egui_wants_keyboard_input(),
            // Focus and modifier changes are notifications, not commands: telling the host we
            // consumed them would stop it updating its own idea of the modifier state.
            _ => false,
        };
        InputResponse::consumed_if(wanted)
    }

    fn tick(&mut self) {
        if !self.open {
            return;
        }
        if let Err(e) = self.run_frame() {
            self.paint_error = Some(e);
        }
    }

    fn close(&mut self) {
        if !self.open {
            // Idempotent, as the trait requires.
            return;
        }
        self.open = false;
        self.painter.close();
        // A click delivered just before the window went away must not be replayed into the
        // next editor: the widget it was aimed at may not even exist any more.
        self.input.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HeadlessPainter, profile};
    use daux_graphics::{
        GraphicErrorKind, GraphicProfile, GraphicRenderer, Key, Modifiers, PhysicalSize,
        PresentationMode, WindowTarget,
    };
    use daux_host_services::HostServices;
    use std::cell::Cell;
    use std::rc::Rc;

    fn context<'a>(host: &'a HostServices, profile: GraphicProfile) -> GraphicContext<'a> {
        GraphicContext::new(
            WindowTarget::win32(0x1234).expect("a non-null hwnd is a valid target"),
            PhysicalSize::new(800, 600),
            ScaleFactor::new(2.0).expect("2.0 is in range"),
            profile,
            host,
        )
    }

    fn software() -> GraphicProfile {
        profile(GraphicRenderer::Software)
    }

    /// An editor whose ui closure counts the frames it drew.
    fn counting_editor() -> (EguiEditor<HeadlessPainter>, Rc<Cell<usize>>) {
        let frames = Rc::new(Cell::new(0usize));
        let seen = Rc::clone(&frames);
        let editor = EguiEditor::new(
            HeadlessPainter::new(),
            LogicalSize::new(400.0, 300.0),
            move |ui| {
                seen.set(seen.get() + 1);
                ui.label("gain");
            },
        );
        (editor, frames)
    }

    #[test]
    fn the_editor_advertises_exactly_its_painters_profile() {
        let (editor, _) = counting_editor();
        let caps = editor.capabilities();
        assert_eq!(caps.len(), 1);
        assert_eq!(caps.profiles()[0], software());
        assert_eq!(editor.descriptor().capabilities, caps);
        assert!(
            caps.has_fallback(),
            "an editor with no embedded-surface profile is invisible in most hosts"
        );
    }

    #[test]
    fn a_descriptors_capabilities_are_replaced_by_the_painters() {
        // A caller that hand-builds a descriptor claiming wgpu must not be able to make a
        // software painter promise GPU rendering to the host.
        let lie = GraphicCapabilities::new().with(GraphicProfile::new(
            GraphicFramework::Egui,
            GraphicRenderer::Wgpu,
            PresentationMode::SharedTexture,
        ));
        let editor = EguiEditor::with_descriptor(
            HeadlessPainter::new(),
            GraphicDescriptor::fixed(lie, LogicalSize::new(100.0, 100.0)),
            |_| {},
        );
        assert_eq!(editor.capabilities().profiles(), [software()]);
    }

    #[test]
    fn a_full_open_tick_close_cycle_drives_the_painter() {
        let host = HostServices::default();
        let (mut editor, frames) = counting_editor();

        editor
            .open(&mut context(&host, software()))
            .expect("open with the offered profile");
        assert!(editor.is_open());
        assert_eq!(editor.painter().opens(), 1);

        editor.tick();
        editor.tick();
        assert_eq!(frames.get(), 2);
        assert_eq!(editor.painter().frames(), 2);
        assert!(
            editor.painter().last_primitives() > 0,
            "a label must produce something to draw"
        );

        editor.close();
        assert!(!editor.is_open());
        assert_eq!(editor.painter().closes(), 1);
    }

    #[test]
    fn opening_twice_is_refused_rather_than_leaking_the_first_surface() {
        let host = HostServices::default();
        let (mut editor, _) = counting_editor();
        editor.open(&mut context(&host, software())).expect("first");

        let err = editor
            .open(&mut context(&host, software()))
            .expect_err("a second open without a close is a host bug");
        assert_eq!(err.kind(), GraphicErrorKind::InvalidState);
        assert_eq!(
            editor.painter().opens(),
            1,
            "the painter must not have been opened a second time"
        );
    }

    #[test]
    fn opening_with_a_profile_that_was_never_offered_is_refused() {
        let host = HostServices::default();
        let (mut editor, _) = counting_editor();
        let wrong = profile(GraphicRenderer::Wgpu);

        let err = editor
            .open(&mut context(&host, wrong))
            .expect_err("the host picked something we cannot draw");
        assert_eq!(err.kind(), GraphicErrorKind::Negotiation);
        assert!(!editor.is_open());
        assert_eq!(editor.painter().opens(), 0);
    }

    #[test]
    fn closing_is_idempotent_and_closing_before_opening_does_nothing() {
        let host = HostServices::default();
        let (mut editor, _) = counting_editor();

        editor.close();
        assert_eq!(
            editor.painter().closes(),
            0,
            "closing an editor that never opened must not reach the painter"
        );

        editor.open(&mut context(&host, software())).expect("open");
        editor.close();
        editor.close();
        assert_eq!(editor.painter().closes(), 1);
    }

    #[test]
    fn ticking_while_closed_runs_no_frame() {
        let (mut editor, frames) = counting_editor();
        editor.tick();
        editor.tick();
        assert_eq!(frames.get(), 0);
        assert_eq!(editor.painter().frames(), 0);
    }

    #[test]
    fn an_editor_survives_being_reopened() {
        let host = HostServices::default();
        let (mut editor, frames) = counting_editor();
        for _ in 0..3 {
            editor.open(&mut context(&host, software())).expect("open");
            editor.tick();
            editor.close();
        }
        assert_eq!(frames.get(), 3);
        assert_eq!(editor.painter().opens(), 3);
        assert_eq!(editor.painter().closes(), 3);
    }

    #[test]
    fn resizing_to_zero_is_refused_and_a_real_resize_reaches_the_painter() {
        let host = HostServices::default();
        let (mut editor, _) = counting_editor();
        editor.open(&mut context(&host, software())).expect("open");

        let err = editor
            .resize(PhysicalSize::new(0, 480))
            .expect_err("a zero-width surface cannot be drawn into");
        assert_eq!(err.kind(), GraphicErrorKind::InvalidArgument);
        assert_eq!(
            editor.painter().size(),
            PhysicalSize::new(800, 600),
            "the rejected size must not have reached the painter"
        );

        editor.resize(PhysicalSize::new(1024, 768)).expect("resize");
        assert_eq!(editor.painter().size(), PhysicalSize::new(1024, 768));
    }

    #[test]
    fn a_resize_before_open_is_the_size_the_editor_opens_at() {
        let host = HostServices::default();
        let (mut editor, _) = counting_editor();
        editor.resize(PhysicalSize::new(320, 200)).expect("resize");
        editor.scale_factor_changed(ScaleFactor::ONE);

        // Opening replaces the recorded geometry with the host's, which is authoritative.
        editor.open(&mut context(&host, software())).expect("open");
        editor.tick();
        assert_eq!(editor.painter().size(), PhysicalSize::new(800, 600));
    }

    #[test]
    fn input_before_open_and_after_close_is_ignored() {
        let host = HostServices::default();
        let (mut editor, _) = counting_editor();
        let space = InputEvent::Key {
            key: Key::Space,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        assert_eq!(editor.on_input(&space), InputResponse::Ignored);

        editor.open(&mut context(&host, software())).expect("open");
        editor.close();
        assert_eq!(editor.on_input(&space), InputResponse::Ignored);
    }

    #[test]
    fn an_editor_with_no_interactive_widgets_leaves_every_key_to_the_host() {
        // The regression this guards: an editor that swallows the space bar breaks the DAW's
        // transport control, which users notice immediately and blame on the plug-in.
        let host = HostServices::default();
        let mut editor = EguiEditor::new(
            HeadlessPainter::new(),
            LogicalSize::new(200.0, 100.0),
            |ui| {
                ui.label("just a label");
            },
        );
        editor.open(&mut context(&host, software())).expect("open");
        editor.tick();

        for event in [
            InputEvent::Key {
                key: Key::Space,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            },
            InputEvent::Text("x".into()),
            InputEvent::PointerMoved {
                position: daux_graphics::LogicalPoint::new(10.0, 10.0),
                modifiers: Modifiers::NONE,
            },
        ] {
            assert_eq!(
                editor.on_input(&event),
                InputResponse::Ignored,
                "{event:?} was consumed by an editor with nothing to consume it"
            );
        }
    }

    #[test]
    fn an_editor_with_focused_text_entry_claims_the_keyboard() {
        let host = HostServices::default();
        let text = Rc::new(std::cell::RefCell::new(String::new()));
        let buffer = Rc::clone(&text);
        let mut editor = EguiEditor::new(
            HeadlessPainter::new(),
            LogicalSize::new(200.0, 100.0),
            move |ui| {
                let mut borrowed = buffer.borrow_mut();
                let response = ui.add(egui::TextEdit::singleline(&mut *borrowed).id("edit".into()));
                response.request_focus();
            },
        );
        editor.open(&mut context(&host, software())).expect("open");
        editor.tick();
        editor.tick();

        assert!(
            editor.context().egui_wants_keyboard_input(),
            "a focused text field must claim the keyboard, or this test proves nothing"
        );
        assert_eq!(
            editor.on_input(&InputEvent::Text("a".into())),
            InputResponse::Consumed
        );
        assert_eq!(
            editor.on_input(&InputEvent::PointerMoved {
                position: daux_graphics::LogicalPoint::new(1.0, 1.0),
                modifiers: Modifiers::NONE
            }),
            InputResponse::Ignored,
            "claiming the keyboard is not claiming the pointer"
        );
    }

    #[test]
    fn an_untranslatable_event_is_never_claimed_even_when_egui_wants_the_keyboard() {
        let host = HostServices::default();
        let text = Rc::new(std::cell::RefCell::new(String::new()));
        let buffer = Rc::clone(&text);
        let mut editor = EguiEditor::new(
            HeadlessPainter::new(),
            LogicalSize::new(200.0, 100.0),
            move |ui| {
                let mut borrowed = buffer.borrow_mut();
                ui.add(egui::TextEdit::singleline(&mut *borrowed).id("edit".into()))
                    .request_focus();
            },
        );
        editor.open(&mut context(&host, software())).expect("open");
        editor.tick();
        editor.tick();

        assert_eq!(
            editor.on_input(&InputEvent::Key {
                key: Key::Unknown(0x77),
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE
            }),
            InputResponse::Ignored,
            "an event egui never saw cannot have been consumed by it"
        );
    }

    #[test]
    fn closing_discards_input_that_arrived_just_before_the_window_went_away() {
        let host = HostServices::default();
        let (mut editor, _) = counting_editor();
        editor.open(&mut context(&host, software())).expect("open");
        editor.on_input(&InputEvent::PointerButton {
            position: daux_graphics::LogicalPoint::new(5.0, 5.0),
            button: daux_graphics::PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        });
        editor.close();

        editor
            .open(&mut context(&host, software()))
            .expect("reopen");
        editor.tick();
        // The click was dropped at close, so the reopened editor sees no button held. A
        // replayed press would arrive with no matching release and latch every widget under
        // the pointer into a drag that never ends.
        assert!(
            !editor.context().input(|i| i.pointer.primary_down()),
            "a click from the previous editor was replayed into the new one"
        );
    }

    #[test]
    fn a_painter_failure_is_kept_rather_than_swallowed_by_tick() {
        struct Failing(HeadlessPainter);

        impl EguiPainter for Failing {
            fn profile(&self) -> GraphicProfile {
                self.0.profile()
            }
            fn open(&mut self, ctx: &mut GraphicContext<'_>) -> DauxGraphicResult<()> {
                self.0.open(ctx)
            }
            fn resize(&mut self, size: PhysicalSize) -> DauxGraphicResult<()> {
                self.0.resize(size)
            }
            fn paint(
                &mut self,
                ctx: &egui::Context,
                output: egui::FullOutput,
            ) -> DauxGraphicResult<()> {
                // Still handle the delta, then report the failure a lost device would.
                self.0.paint(ctx, output)?;
                Err(GraphicError::new(
                    GraphicErrorKind::Renderer,
                    "the device was lost",
                ))
            }
            fn close(&mut self) {
                self.0.close();
            }
        }

        let host = HostServices::default();
        let mut editor = EguiEditor::new(
            Failing(HeadlessPainter::new()),
            LogicalSize::new(100.0, 100.0),
            |ui| {
                ui.label("x");
            },
        );
        editor.open(&mut context(&host, software())).expect("open");
        editor.tick();

        let err = editor.take_paint_error().expect("the failure was recorded");
        assert_eq!(err.kind(), GraphicErrorKind::Renderer);
        assert!(editor.take_paint_error().is_none(), "taking it clears it");
        assert!(
            editor.is_open(),
            "a transient paint failure must not close the editor"
        );
    }
}
