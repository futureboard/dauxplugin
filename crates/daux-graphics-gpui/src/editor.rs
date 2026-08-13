//! [`DauxGraphic`] over `gpui_embedded`.

use daux_graphics::{
    DauxGraphic, DauxGraphicResult, GraphicCapabilities, GraphicContext, GraphicDescriptor,
    GraphicError, GraphicFramework, GraphicProfile, GraphicRenderer, InputEvent, InputResponse,
    LogicalSize, PhysicalSize, PresentationMode, ScaleFactor,
};
use gpui::{App, Entity, Render, Window, px, size};
use gpui_embedded::{EmbeddedAppBuilder, HostSurface, plugin::PluginEditor};

use crate::convert;

/// The one profile this backend can offer.
///
/// GPUI renders through `wgpu` and `gpui_embedded` draws into a surface the host owns, so
/// there is exactly one combination — advertising more would be a lie the negotiation would
/// then act on.
pub fn profile() -> GraphicProfile {
    GraphicProfile::new(
        GraphicFramework::Gpui,
        GraphicRenderer::Wgpu,
        PresentationMode::EmbeddedSurface,
    )
}

/// A GPUI editor wearing the DAUx editor interface.
///
/// # What this type is and is not
///
/// It is the adapter between two lifecycles that already match closely:
/// `gpui_embedded::plugin::PluginEditor` was written for exactly this job — it creates no
/// windows, owns no event loop, holds no global state, and never calls `exit`. Upstream GPUI
/// assumes it *is* the application, which is why this backend is built on the
/// `futureboard/gpui-se` fork rather than crates.io `gpui`.
///
/// It is not a widget library. What the editor draws is the `V: Render` the plug-in supplies.
///
/// # Threading
///
/// The thread that first opens this editor becomes GPUI's main thread and every later call
/// must come from it. Plug-in hosts call editor methods on one thread, so this is satisfied
/// by construction — but it means the audio thread must never reach this type. It cannot:
/// [`DauxGraphic`] is only ever handed out on the main thread.
pub struct GpuiEditor<V: Render> {
    editor: PluginEditor<V>,
    descriptor: GraphicDescriptor,
}

impl<V: Render + 'static> GpuiEditor<V> {
    /// [main-thread] Builds an editor of `size` logical pixels that renders `build_root`.
    ///
    /// Nothing is created until [`DauxGraphic::open`]: an editor that is never opened costs
    /// one allocation, which is what makes it cheap for a plug-in to always have one.
    pub fn new(
        size: LogicalSize,
        build_root: impl Fn(&mut Window, &mut App) -> Entity<V> + 'static,
    ) -> Self {
        Self::with_descriptor(
            GraphicDescriptor::fixed(GraphicCapabilities::new().with(profile()), size),
            build_root,
        )
    }

    /// [main-thread] Builds an editor with an explicit descriptor, for a resizable editor or
    /// one with an aspect ratio to keep.
    ///
    /// The descriptor's capabilities are replaced with this backend's single profile: a GPUI
    /// editor cannot honour a profile GPUI does not implement, and letting a caller advertise
    /// one would produce a negotiation the host then cannot satisfy.
    pub fn with_descriptor(
        mut descriptor: GraphicDescriptor,
        build_root: impl Fn(&mut Window, &mut App) -> Entity<V> + 'static,
    ) -> Self {
        descriptor.capabilities = GraphicCapabilities::new().with(profile());
        let preferred = descriptor.preferred_size;
        Self {
            editor: PluginEditor::new(
                size(px(preferred.width as f32), px(preferred.height as f32)),
                build_root,
            ),
            descriptor,
        }
    }

    /// [main-thread] Customises the GPUI instance built at each open.
    ///
    /// Use this for fonts, an [`EmbeddedHost`](gpui_embedded::EmbeddedHost) implementation, or
    /// a waker. The builder runs once per [`DauxGraphic::open`], so each open gets a fresh
    /// instance — which is what makes closing and reopening an editor leak nothing.
    #[must_use]
    pub fn with_builder(mut self, builder: impl FnMut() -> EmbeddedAppBuilder + 'static) -> Self {
        self.editor = self.editor.with_builder(builder);
        self
    }

    /// [main-thread] The underlying `gpui_embedded` editor, for anything this adapter does
    /// not expose.
    pub fn inner(&self) -> &PluginEditor<V> {
        &self.editor
    }

    /// [main-thread] The underlying editor, mutably.
    pub fn inner_mut(&mut self) -> &mut PluginEditor<V> {
        &mut self.editor
    }

    /// [main-thread] `true` while GPUI is running inside a host surface.
    pub fn is_open(&self) -> bool {
        self.editor.is_open()
    }
}

impl<V: Render + 'static> DauxGraphic for GpuiEditor<V> {
    fn descriptor(&self) -> GraphicDescriptor {
        self.descriptor.clone()
    }

    fn capabilities(&self) -> GraphicCapabilities {
        GraphicCapabilities::new().with(profile())
    }

    fn open(&mut self, ctx: &mut GraphicContext<'_>) -> DauxGraphicResult<()> {
        if self.editor.is_open() {
            return Err(GraphicError::invalid_state("the editor is already open"));
        }

        let target = ctx.target();
        let window = target.raw_window_handle().ok_or_else(|| {
            GraphicError::unsupported("the host window handle is not one GPUI can render into")
        })?;
        let display = target.raw_display_handle().ok_or_else(|| {
            GraphicError::unsupported("the host provided no display handle")
        })?;

        // SAFETY: `HostSurface::from_raw` requires the handles to name a live window that
        // outlives the surface. `GraphicContext` documents the host's window as valid from
        // `open` until `close`, `WindowTarget` rejects null handles at construction, and
        // `close` below tears the GPUI instance down before returning — so the surface never
        // outlives the window the host owns. We do not store the handles anywhere else.
        let surface = unsafe { HostSurface::from_raw(window, display) };

        self.editor.set_scale(convert::scale(ctx.scale()));
        self.editor
            .set_physical_size(ctx.size().width, ctx.size().height);
        self.editor.open(surface).map_err(|e| {
            GraphicError::new(
                daux_graphics::GraphicErrorKind::Renderer,
                format!("GPUI could not start in the host's surface: {e}"),
            )
        })?;
        Ok(())
    }

    fn resize(&mut self, size: PhysicalSize) -> DauxGraphicResult<()> {
        if size.is_empty() {
            return Err(GraphicError::invalid_argument(
                "an editor cannot be resized to zero",
            ));
        }
        self.editor.set_physical_size(size.width, size.height);
        Ok(())
    }

    fn scale_factor_changed(&mut self, scale: ScaleFactor) {
        self.editor.set_scale(convert::scale(scale));
    }

    fn on_input(&mut self, event: &InputEvent) -> InputResponse {
        if !self.editor.is_open() {
            return InputResponse::Ignored;
        }
        // Focus is window state in GPUI, not an event: it is the one case the translation
        // handles by calling a method rather than by producing a `HostEvent`.
        if let InputEvent::Focus(focused) = event {
            self.editor.set_focused(*focused);
            return InputResponse::Consumed;
        }

        let mut consumed = false;
        for host_event in convert::to_host_events(event) {
            let result = self.editor.dispatch(host_event);
            consumed |= !result.propagate;
        }
        InputResponse::consumed_if(consumed)
    }

    fn tick(&mut self) {
        self.editor.idle();
    }

    fn close(&mut self) {
        // Idempotent, as the trait requires: `PluginEditor::close` on a closed editor is a
        // no-op. The `HostSurface` was moved into the GPUI instance at `open` and dies with
        // it here, which is what keeps it from outliving the host's window.
        self.editor.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use daux_graphics::{HostGraphicCaps, PresentationModeSet};

    #[test]
    fn the_backend_advertises_exactly_one_profile() {
        let caps = GraphicCapabilities::new().with(profile());
        assert_eq!(caps.len(), 1);
        assert_eq!(profile().framework, GraphicFramework::Gpui);
        assert_eq!(profile().renderer, GraphicRenderer::Wgpu);
        assert_eq!(profile().presentation, PresentationMode::EmbeddedSurface);
    }

    #[test]
    fn the_profile_is_the_universally_supported_shape() {
        // An embedded surface is the one presentation mode every host can provide, so this
        // backend negotiates successfully against an ordinary in-process host.
        assert!(profile().is_fallback());
        let caps = GraphicCapabilities::new().with(profile());
        assert_eq!(
            caps.negotiate_with_fallback(&HostGraphicCaps::in_process()),
            Some(profile())
        );

        // A host that cannot drive an embedded surface at all is the one case with no
        // agreement to reach — this backend has nothing else to offer.
        let no_surface = HostGraphicCaps::in_process()
            .with_presentation(PresentationModeSet::only(PresentationMode::NativeWindow));
        assert_eq!(caps.negotiate(&no_surface), None);
    }
}
