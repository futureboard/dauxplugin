//! The editor trait and the context it is opened with.

use core::fmt;

use daux_host_services::HostServices;

use crate::{
    DauxGraphicResult, GraphicCapabilities, GraphicDescriptor, GraphicProfile, InputEvent,
    InputResponse, PhysicalSize, PresentationMode, ScaleFactor, WindowTarget,
};

/// Everything an editor needs at the moment it is opened.
///
/// The window handle inside is owned by the **host** and is valid only between
/// [`DauxGraphic::open`] and [`DauxGraphic::close`]. An editor that stores it and uses it
/// afterwards is reading freed memory; that is the single most common way a plug-in editor
/// crashes a DAW.
///
/// [main-thread]
pub struct GraphicContext<'a> {
    target: WindowTarget,
    scale: ScaleFactor,
    size: PhysicalSize,
    profile: GraphicProfile,
    host: &'a HostServices,
}

impl fmt::Debug for GraphicContext<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GraphicContext")
            .field("target", &self.target.api())
            .field("scale", &self.scale.get())
            .field("size", &self.size)
            .field("profile", &self.profile)
            .finish_non_exhaustive()
    }
}

impl<'a> GraphicContext<'a> {
    /// [main-thread] Builds the context a host hands to [`DauxGraphic::open`].
    pub fn new(
        target: WindowTarget,
        size: PhysicalSize,
        scale: ScaleFactor,
        profile: GraphicProfile,
        host: &'a HostServices,
    ) -> Self {
        Self {
            target,
            scale,
            size,
            profile,
            host,
        }
    }

    /// [main-thread] The native window or view to render into.
    ///
    /// Valid only for the duration of the open editor. Never retain it.
    pub const fn target(&self) -> WindowTarget {
        self.target
    }

    /// [main-thread] Physical pixels per logical pixel on the display the editor is on.
    pub const fn scale(&self) -> ScaleFactor {
        self.scale
    }

    /// [main-thread] The scale factor as a bare number.
    pub const fn scale_factor(&self) -> f64 {
        self.scale.get()
    }

    /// [main-thread] The size of the host's window, in physical pixels.
    pub const fn size(&self) -> PhysicalSize {
        self.size
    }

    /// [main-thread] The framework/renderer/presentation combination the host agreed to.
    ///
    /// An editor that advertised more than one profile must honour this one; the host has
    /// already told the user what it is going to get.
    pub const fn profile(&self) -> GraphicProfile {
        self.profile
    }

    /// [main-thread] How the editor's pixels reach the host.
    pub const fn presentation(&self) -> PresentationMode {
        self.profile.presentation
    }

    /// [main-thread] The host's main-thread services.
    ///
    /// This is the full [`HostServices`], not the real-time subset: an editor runs on the
    /// main thread and may allocate, format text and call the host freely.
    pub const fn host(&self) -> &HostServices {
        self.host
    }
}

/// A plug-in editor.
///
/// # Lifetime
///
/// An editor's lifetime is **independent of the processor's** — the ninth architectural rule
/// in `CLAUDE.md`, and the one most often broken. A user may open and close an editor a
/// hundred times while audio never stops, and may never open it at all. Therefore:
///
/// - [`close`](Self::close) must not touch DSP state, and must be safe to call when the
///   editor never opened.
/// - Nothing the processor needs may live only inside the editor.
/// - Every value the editor shows is read from the parameter set or from a `daux-rt` channel,
///   never from the processor directly.
///
/// # Threading
///
/// Every method is `[main-thread]`, and the trait is deliberately **neither `Send` nor
/// `Sync`**. An editor is created, driven and destroyed on one thread — the one the host
/// calls back on, which on macOS must be the main thread because that is where plug-in hosts
/// run editor callbacks.
///
/// Requiring `Send` here would buy nothing and cost everything: it is exactly the bound that
/// no real UI toolkit can satisfy. GPUI is built on `Rc`, egui's context is too, and a
/// backend cannot work around that without adding a lock on the UI path. The audio thread is
/// kept away from editors by construction instead — `ProcessContext` cannot reach a
/// `DauxPlugin`, so there is no path from `process` to this trait to protect.
pub trait DauxGraphic {
    /// [main-thread] What this editor can do and how big it wants to be.
    ///
    /// Called before [`open`](Self::open), possibly several times, and possibly without the
    /// editor ever being opened.
    fn descriptor(&self) -> GraphicDescriptor;

    /// [main-thread] The framework/renderer/presentation combinations this editor offers.
    ///
    /// Derived from [`descriptor`](Self::descriptor) by default; override only if computing
    /// the full descriptor is expensive.
    fn capabilities(&self) -> GraphicCapabilities {
        self.descriptor().capabilities
    }

    /// [main-thread] Creates the editor's rendering resources inside the host's window.
    ///
    /// Called at most once without an intervening [`close`](Self::close). On failure the
    /// editor must leave nothing behind: the host will not call `close` for an `open` that
    /// returned an error.
    ///
    /// # Errors
    ///
    /// Any [`GraphicError`](crate::GraphicError). The host reports the editor as unavailable
    /// and carries on processing audio.
    fn open(&mut self, ctx: &mut GraphicContext<'_>) -> DauxGraphicResult<()>;

    /// [main-thread] The host's window changed size, in physical pixels.
    ///
    /// The host has already clamped through [`GraphicDescriptor::clamp`], so this is a size
    /// the editor said it would accept.
    ///
    /// # Errors
    ///
    /// Any [`GraphicError`](crate::GraphicError); the host keeps the previous size.
    fn resize(&mut self, size: PhysicalSize) -> DauxGraphicResult<()>;

    /// [main-thread] The editor moved to a display with a different pixel density.
    fn scale_factor_changed(&mut self, _scale: ScaleFactor) {}

    /// [main-thread] Handles one input event.
    ///
    /// Return [`InputResponse::Ignored`] for anything the editor did not use, so the host's
    /// own shortcuts keep working — see [`InputEvent::is_host_reserved`].
    fn on_input(&mut self, _event: &InputEvent) -> InputResponse {
        InputResponse::Ignored
    }

    /// [main-thread] The host's idle or frame callback.
    ///
    /// This is where an editor polls its channels from the audio thread and repaints. It must
    /// be cheap when nothing changed: a host calls it many times a second and a plug-in that
    /// does real work here will show up in the DAW's UI thread profile.
    fn tick(&mut self) {}

    /// [main-thread] Destroys everything [`open`](Self::open) created.
    ///
    /// Called before the host destroys its window, and must be idempotent — a host that has
    /// lost track of its own state may call it twice. Must not fail and must not touch DSP
    /// state.
    fn close(&mut self);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        GraphicFramework, GraphicRenderer, Key, LogicalSize, Modifiers, WindowApi,
    };

    fn profile() -> GraphicProfile {
        GraphicProfile::new(
            GraphicFramework::Custom,
            GraphicRenderer::Software,
            PresentationMode::EmbeddedSurface,
        )
    }

    /// An editor that records what it was told, to prove the defaults behave.
    #[derive(Default)]
    struct Recorder {
        opened: usize,
        closed: usize,
        resizes: Vec<PhysicalSize>,
        scales: Vec<f64>,
        ticks: usize,
    }

    impl DauxGraphic for Recorder {
        fn descriptor(&self) -> GraphicDescriptor {
            GraphicDescriptor::fixed(
                GraphicCapabilities::new().with(profile()),
                LogicalSize::new(400.0, 300.0),
            )
        }

        fn open(&mut self, _ctx: &mut GraphicContext<'_>) -> DauxGraphicResult<()> {
            self.opened += 1;
            Ok(())
        }

        fn resize(&mut self, size: PhysicalSize) -> DauxGraphicResult<()> {
            self.resizes.push(size);
            Ok(())
        }

        fn scale_factor_changed(&mut self, scale: ScaleFactor) {
            self.scales.push(scale.get());
        }

        fn tick(&mut self) {
            self.ticks += 1;
        }

        fn close(&mut self) {
            self.closed += 1;
        }
    }

    fn context<'a>(host: &'a HostServices) -> GraphicContext<'a> {
        GraphicContext::new(
            WindowTarget::win32(0x1234).expect("a non-null hwnd is a valid target"),
            PhysicalSize::new(800, 600),
            ScaleFactor::new(2.0).expect("2.0 is in range"),
            profile(),
            host,
        )
    }

    #[test]
    fn the_context_reports_what_the_host_agreed_to() {
        let host = HostServices::default();
        let ctx = context(&host);
        assert_eq!(ctx.size(), PhysicalSize::new(800, 600));
        assert_eq!(ctx.scale_factor(), 2.0);
        assert_eq!(ctx.presentation(), PresentationMode::EmbeddedSurface);
        assert_eq!(ctx.profile(), profile());
        assert_eq!(ctx.target().api(), WindowApi::Win32);
    }

    #[test]
    fn capabilities_default_to_the_descriptors() {
        let r = Recorder::default();
        assert_eq!(r.capabilities(), r.descriptor().capabilities);
        assert!(r.capabilities().has_fallback());
    }

    #[test]
    fn an_editor_can_be_driven_through_a_whole_open_close_cycle() {
        let host = HostServices::default();
        let mut r = Recorder::default();
        let mut ctx = context(&host);

        r.open(&mut ctx).unwrap();
        r.resize(PhysicalSize::new(1024, 768)).unwrap();
        r.scale_factor_changed(ScaleFactor::ONE);
        r.tick();
        r.tick();
        r.close();

        assert_eq!(r.opened, 1);
        assert_eq!(r.closed, 1);
        assert_eq!(r.resizes, [PhysicalSize::new(1024, 768)]);
        assert_eq!(r.scales, [1.0]);
        assert_eq!(r.ticks, 2);
    }

    #[test]
    fn closing_twice_is_allowed() {
        let mut r = Recorder::default();
        r.close();
        r.close();
        assert_eq!(r.closed, 2);
    }

    #[test]
    fn input_is_ignored_by_default_so_host_shortcuts_keep_working() {
        let mut r = Recorder::default();
        let space = InputEvent::Key {
            key: Key::Space,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        assert_eq!(r.on_input(&space), InputResponse::Ignored);
        assert!(space.is_host_reserved());
    }

    #[test]
    fn an_editor_is_usable_as_a_trait_object() {
        let host = HostServices::default();
        let mut boxed: Box<dyn DauxGraphic> = Box::new(Recorder::default());
        let mut ctx = context(&host);
        boxed.open(&mut ctx).unwrap();
        boxed.tick();
        boxed.close();
        assert_eq!(boxed.descriptor().preferred_size, LogicalSize::new(400.0, 300.0));
    }
}
