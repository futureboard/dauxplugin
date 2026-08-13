//! The three orthogonal axes of an editor — UI framework, rendering backend and
//! presentation mode — and the negotiation types built on them.
//!
//! Most SDKs have one knob called "the graphics backend". DAUxPlug has three, because in
//! practice these three things vary independently:
//!
//! ```text
//!   UI framework          Rendering backend       Presentation mode
//!   ────────────          ─────────────────       ─────────────────
//!   egui                  wgpu                    NativeWindow
//!   GPUI                  OpenGL                  EmbeddedSurface
//!   custom                software                SharedTexture
//!                                                 ExternalWindow
//! ```
//!
//! A point in that space is a [`GraphicProfile`]. What a plug-in offers is an ordered
//! list of profiles ([`GraphicCapabilities`]); what a host can provide is a set of
//! constraints ([`HostGraphicCaps`]). Negotiation intersects the two and lets the host
//! pick, and it always has an answer as long as the plug-in kept the mandatory
//! [`PresentationMode::EmbeddedSurface`] fallback.
//!
//! ```
//! use daux_graphics::{
//!     GraphicCapabilities, GraphicFramework, GraphicProfile, GraphicRenderer,
//!     HostGraphicCaps, PresentationMode, PresentationModeSet, WindowApi, WindowApiSet,
//! };
//!
//! // "egui+wgpu on an embedded surface, or egui+software on a native window."
//! let plugin = GraphicCapabilities::new()
//!     .with(GraphicProfile::new(
//!         GraphicFramework::Egui,
//!         GraphicRenderer::Wgpu,
//!         PresentationMode::EmbeddedSurface,
//!     ))
//!     .with(GraphicProfile::new(
//!         GraphicFramework::Egui,
//!         GraphicRenderer::Software,
//!         PresentationMode::NativeWindow,
//!     ));
//!
//! // A host that only parents child windows and cannot do GPU work.
//! let host = HostGraphicCaps::new()
//!     .with_presentation(PresentationModeSet::only(PresentationMode::EmbeddedSurface))
//!     .with_window_apis(WindowApiSet::only(WindowApi::Win32))
//!     .with_renderers(GraphicRenderer::Software.into());
//!
//! // No agreement: the host cannot run wgpu and cannot host a native window.
//! assert_eq!(plugin.negotiate(&host), None);
//! ```

use core::fmt;

use crate::bitset::define_bit_set;
use crate::error::{GraphicError, GraphicErrorKind};
use crate::texture::SharedTextureCaps;
use crate::window::{WindowApi, WindowApiSet};

/// `[any-thread]` Which UI toolkit draws the editor's widgets.
///
/// This axis is **never constrained by the host**: what a plug-in uses to lay out its
/// own pixels is its own business. It is modelled explicitly anyway because a host, a
/// bundle manifest and `daux inspect` all want to report it, and because a plug-in that
/// supports two frameworks needs to say which one a given profile refers to.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum GraphicFramework {
    /// The `egui` immediate-mode toolkit, via `daux-graphics-egui`.
    Egui,
    /// Zed's GPUI toolkit, via `daux-graphics-gpui`.
    Gpui,
    /// Anything else, including hand-rolled drawing straight onto the renderer.
    Custom,
}

impl GraphicFramework {
    /// `[any-thread]` A short, stable, lowercase name used in manifests and logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Egui => "egui",
            Self::Gpui => "gpui",
            Self::Custom => "custom",
        }
    }
}

impl fmt::Display for GraphicFramework {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

define_bit_set!(
    /// `[any-thread]` A set of [`GraphicFramework`]s.
    GraphicFrameworkSet: GraphicFramework {
        Egui = 0,
        Gpui = 1,
        Custom = 2,
    }
);

impl From<GraphicFramework> for GraphicFrameworkSet {
    fn from(value: GraphicFramework) -> Self {
        Self::only(value)
    }
}

/// `[any-thread]` What turns the editor's draw calls into pixels.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum GraphicRenderer {
    /// `wgpu`, i.e. Vulkan / Metal / D3D12 / GL under one API.
    Wgpu,
    /// OpenGL or OpenGL ES directly.
    OpenGl,
    /// CPU rasterisation into a pixel buffer the host or the window system blits.
    ///
    /// Always available, never fast. This is the renderer of last resort and the reason
    /// an editor can be shown on a machine with no working GPU driver.
    Software,
}

impl GraphicRenderer {
    /// `[any-thread]` A short, stable, lowercase name used in manifests and logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Wgpu => "wgpu",
            Self::OpenGl => "opengl",
            Self::Software => "software",
        }
    }

    /// `[any-thread]` Whether this renderer needs a GPU device at all.
    #[must_use]
    pub const fn needs_gpu(self) -> bool {
        matches!(self, Self::Wgpu | Self::OpenGl)
    }
}

impl fmt::Display for GraphicRenderer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

define_bit_set!(
    /// `[any-thread]` A set of [`GraphicRenderer`]s.
    GraphicRendererSet: GraphicRenderer {
        Wgpu = 0,
        OpenGl = 1,
        Software = 2,
    }
);

impl From<GraphicRenderer> for GraphicRendererSet {
    fn from(value: GraphicRenderer) -> Self {
        Self::only(value)
    }
}

/// `[any-thread]` How the editor's pixels reach the host's screen.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PresentationMode {
    /// The plug-in creates its own top-level window. Used by standalone previews and by
    /// hosts that only support floating editors.
    NativeWindow,
    /// The host passes a parent window and the plug-in parents its content into it.
    ///
    /// This is what VST3 and CLAP hosts do, and it is the universal fallback: a plug-in
    /// that cannot do this cannot show an editor everywhere.
    EmbeddedSurface,
    /// The plug-in renders into a GPU resource the host imports and composites itself.
    ///
    /// The DAUx-native path (`abi-v1` §13). No nested child window, no z-order fights,
    /// and the host can transform the plug-in's pixels. Requires negotiation and MUST
    /// have a fallback.
    SharedTexture,
    /// The editor lives in another process and the host is told where it is.
    ///
    /// Exists so that sandboxed hosting is a first-class mode rather than something
    /// bolted on later.
    ExternalWindow,
}

impl PresentationMode {
    /// `[any-thread]` A short, stable, lowercase name used in manifests and logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NativeWindow => "native-window",
            Self::EmbeddedSurface => "embedded-surface",
            Self::SharedTexture => "shared-texture",
            Self::ExternalWindow => "external-window",
        }
    }

    /// `[any-thread]` Whether this mode hands the editor a platform window handle it
    /// must draw into or parent itself to.
    #[must_use]
    pub const fn uses_window_handle(self) -> bool {
        matches!(self, Self::NativeWindow | Self::EmbeddedSurface)
    }

    /// `[any-thread]` The mode every plug-in and every host must support.
    pub const FALLBACK: Self = Self::EmbeddedSurface;
}

impl fmt::Display for PresentationMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

define_bit_set!(
    /// `[any-thread]` A set of [`PresentationMode`]s.
    PresentationModeSet: PresentationMode {
        NativeWindow = 0,
        EmbeddedSurface = 1,
        SharedTexture = 2,
        ExternalWindow = 3,
    }
);

impl From<PresentationMode> for PresentationModeSet {
    fn from(value: PresentationMode) -> Self {
        Self::only(value)
    }
}

/// `[any-thread]` One point in the framework × renderer × presentation space.
///
/// A profile is a complete, self-consistent answer to "how would you show your editor?".
/// Plug-ins offer several, ordered by preference; hosts accept or reject each one whole,
/// because accepting "egui + wgpu" but rejecting the presentation mode it was offered
/// with is not an agreement anyone can act on.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct GraphicProfile {
    /// Which toolkit draws the widgets.
    pub framework: GraphicFramework,
    /// Which renderer turns them into pixels.
    pub renderer: GraphicRenderer,
    /// How those pixels reach the host.
    pub presentation: PresentationMode,
}

impl GraphicProfile {
    /// `[any-thread]` Builds a profile.
    #[must_use]
    pub const fn new(
        framework: GraphicFramework,
        renderer: GraphicRenderer,
        presentation: PresentationMode,
    ) -> Self {
        Self {
            framework,
            renderer,
            presentation,
        }
    }

    /// `[any-thread]` The same profile presented a different way.
    #[must_use]
    pub const fn with_presentation(self, presentation: PresentationMode) -> Self {
        Self {
            presentation,
            ..self
        }
    }

    /// `[any-thread]` The same profile drawn by a different renderer.
    #[must_use]
    pub const fn with_renderer(self, renderer: GraphicRenderer) -> Self {
        Self { renderer, ..self }
    }

    /// `[any-thread]` Whether this profile is the universally supported fallback shape:
    /// an embedded surface ([`PresentationMode::FALLBACK`]), which every host can
    /// provide.
    #[must_use]
    pub const fn is_fallback(self) -> bool {
        matches!(self.presentation, PresentationMode::EmbeddedSurface)
    }
}

impl fmt::Display for GraphicProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}+{} on {}",
            self.framework, self.renderer, self.presentation
        )
    }
}

/// How many profiles one editor may offer.
///
/// Three frameworks × three renderers × four presentation modes is 36 in theory, but a
/// real editor offers a handful. Sixteen is generous and keeps [`GraphicCapabilities`]
/// `Copy` and allocation-free.
pub const MAX_GRAPHIC_PROFILES: usize = 16;

/// `[any-thread]` What an editor can do, in the editor's own order of preference.
///
/// Ordered, not a set: the first entry the host accepts is the one that gets used, so
/// "wgpu if you'll have it, software if you won't" is expressed by listing wgpu first.
/// Duplicates are ignored rather than rejected, so building the list from overlapping
/// feature flags is safe.
///
/// The type is `Copy` and never allocates: it is a fixed array plus a length, so a
/// plug-in can build one in a `const` and hand it out from `descriptor()` on any thread.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct GraphicCapabilities {
    profiles: [GraphicProfile; MAX_GRAPHIC_PROFILES],
    len: usize,
}

impl GraphicCapabilities {
    /// The value used to pad the unused tail of the array. Never observable.
    const PADDING: GraphicProfile = GraphicProfile::new(
        GraphicFramework::Custom,
        GraphicRenderer::Software,
        PresentationMode::EmbeddedSurface,
    );

    /// `[any-thread]` An empty capability list.
    ///
    /// An editor that returns this from [`DauxGraphic::capabilities`] is declaring that
    /// it cannot be shown at all — which is legal (headless plug-ins exist) but means a
    /// host will never open it.
    ///
    /// [`DauxGraphic::capabilities`]: crate::DauxGraphic::capabilities
    #[must_use]
    pub const fn new() -> Self {
        Self {
            profiles: [Self::PADDING; MAX_GRAPHIC_PROFILES],
            len: 0,
        }
    }

    /// `[any-thread]` Adds a profile, ignoring duplicates and silently dropping anything
    /// past [`MAX_GRAPHIC_PROFILES`].
    ///
    /// The infallible builder form, for `const` construction and for chaining. Use
    /// [`push`](Self::push) when a dropped profile would be a bug worth reporting.
    #[must_use]
    pub const fn with(mut self, profile: GraphicProfile) -> Self {
        // `const fn` cannot call iterator adapters, so the scan is written out.
        let mut i = 0;
        while i < self.len {
            if profile_eq(self.profiles[i], profile) {
                return self;
            }
            i += 1;
        }
        if self.len < MAX_GRAPHIC_PROFILES {
            self.profiles[self.len] = profile;
            self.len += 1;
        }
        self
    }

    /// `[main-thread]` Adds a profile, reporting overflow.
    ///
    /// Adding a profile that is already present succeeds and changes nothing, which
    /// keeps the operation idempotent.
    ///
    /// # Errors
    ///
    /// [`GraphicErrorKind::CapacityExceeded`] when the list already holds
    /// [`MAX_GRAPHIC_PROFILES`] distinct profiles.
    pub fn push(&mut self, profile: GraphicProfile) -> Result<(), GraphicError> {
        if self.supports(profile) {
            return Ok(());
        }
        if self.len == MAX_GRAPHIC_PROFILES {
            return Err(GraphicError::new_static(
                GraphicErrorKind::CapacityExceeded,
                "an editor may offer at most MAX_GRAPHIC_PROFILES graphic profiles",
            ));
        }
        self.profiles[self.len] = profile;
        self.len += 1;
        Ok(())
    }

    /// `[any-thread]` The offered profiles, most preferred first.
    #[must_use]
    pub fn profiles(&self) -> &[GraphicProfile] {
        &self.profiles[..self.len]
    }

    /// `[any-thread]` How many profiles are offered.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// `[any-thread]` Whether nothing at all is offered.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// `[any-thread]` Whether this exact profile is offered.
    #[must_use]
    pub fn supports(&self, profile: GraphicProfile) -> bool {
        self.profiles().contains(&profile)
    }

    /// `[any-thread]` The presentation modes covered by at least one profile.
    #[must_use]
    pub fn presentation_modes(&self) -> PresentationModeSet {
        self.profiles()
            .iter()
            .fold(PresentationModeSet::EMPTY, |set, p| {
                set.with(p.presentation)
            })
    }

    /// `[any-thread]` The frameworks covered by at least one profile.
    #[must_use]
    pub fn frameworks(&self) -> GraphicFrameworkSet {
        self.profiles()
            .iter()
            .fold(GraphicFrameworkSet::EMPTY, |set, p| set.with(p.framework))
    }

    /// `[any-thread]` The renderers covered by at least one profile.
    #[must_use]
    pub fn renderers(&self) -> GraphicRendererSet {
        self.profiles()
            .iter()
            .fold(GraphicRendererSet::EMPTY, |set, p| set.with(p.renderer))
    }

    /// `[any-thread]` Whether the mandatory embedded-surface fallback is present.
    ///
    /// `docs/architecture/graphics.md` and `abi-v1` §13 both require it: a plug-in must
    /// never *need* a shared texture or a floating window to show a UI.
    #[must_use]
    pub fn has_fallback(&self) -> bool {
        self.profiles().iter().any(|p| p.is_fallback())
    }

    /// `[main-thread]` Checks the list a plug-in author wrote.
    ///
    /// # Errors
    ///
    /// [`GraphicErrorKind::Unsupported`] when the list is empty or has no
    /// embedded-surface profile. Backends call this while opening so that a missing
    /// fallback is a clear error at development time instead of a plug-in that is
    /// invisible in one host and fine in another.
    pub fn validate(&self) -> Result<(), GraphicError> {
        if self.is_empty() {
            return Err(GraphicError::unsupported(
                "an editor must offer at least one graphic profile",
            ));
        }
        if !self.has_fallback() {
            return Err(GraphicError::unsupported(
                "an editor must offer a PresentationMode::EmbeddedSurface profile as its fallback",
            ));
        }
        Ok(())
    }

    /// `[main-thread]` The first offered profile the host accepts, or `None`.
    ///
    /// Preference order is the plug-in's: it knows which of its own paths is fastest and
    /// best-tested. The host's role is to veto, not to rank.
    #[must_use]
    pub fn negotiate(&self, host: &HostGraphicCaps) -> Option<GraphicProfile> {
        self.profiles().iter().copied().find(|p| host.accepts(*p))
    }

    /// `[main-thread]` Like [`negotiate`](Self::negotiate), but falls back to the
    /// mandatory embedded-surface profile when no offered profile is acceptable.
    ///
    /// Returns `None` only when the plug-in has no fallback profile or the host cannot
    /// even parent a child window — a combination that genuinely has no answer.
    #[must_use]
    pub fn negotiate_with_fallback(&self, host: &HostGraphicCaps) -> Option<GraphicProfile> {
        self.negotiate(host).or_else(|| {
            self.profiles()
                .iter()
                .copied()
                .find(|p| p.is_fallback() && host.accepts(p.with_presentation(PresentationMode::FALLBACK)))
        })
    }
}

/// `const`-compatible equality for [`GraphicProfile`], which cannot use `PartialEq` in a
/// `const fn`.
const fn profile_eq(a: GraphicProfile, b: GraphicProfile) -> bool {
    a.framework as u8 == b.framework as u8
        && a.renderer as u8 == b.renderer as u8
        && a.presentation as u8 == b.presentation as u8
}

impl Default for GraphicCapabilities {
    /// `[any-thread]` An empty list; see [`GraphicCapabilities::new`].
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for GraphicCapabilities {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.profiles()).finish()
    }
}

impl FromIterator<GraphicProfile> for GraphicCapabilities {
    /// `[main-thread]` Collects profiles in order; duplicates and overflow are dropped.
    fn from_iter<I: IntoIterator<Item = GraphicProfile>>(iter: I) -> Self {
        iter.into_iter().fold(Self::new(), Self::with)
    }
}

/// `[any-thread]` What a host can provide, as constraints on the three axes.
///
/// Note what is *absent*: the UI framework. A host has no business caring whether the
/// editor is drawn with egui or GPUI, so there is nothing here to constrain it with.
///
/// The default is the most permissive honest answer for an in-process host: every
/// presentation mode except [`PresentationMode::SharedTexture`] (which requires real
/// negotiation and therefore has to be opted into with
/// [`with_shared_texture`](Self::with_shared_texture)), the platform's window API, and
/// every renderer.
#[derive(Clone, Debug, Default)]
pub struct HostGraphicCaps {
    presentation: PresentationModeSet,
    window_apis: WindowApiSet,
    renderers: GraphicRendererSet,
    shared_texture: SharedTextureCaps,
}

impl HostGraphicCaps {
    /// `[any-thread]` A host that supports nothing; build it up with the `with_*`
    /// methods.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `[main-thread]` The typical in-process host: embedded surfaces and native
    /// windows, this platform's window API, every renderer, no shared textures.
    #[must_use]
    pub fn in_process() -> Self {
        Self {
            presentation: PresentationModeSet::only(PresentationMode::EmbeddedSurface)
                .with(PresentationMode::NativeWindow),
            window_apis: WindowApiSet::only(WindowApi::PLATFORM),
            renderers: GraphicRendererSet::ALL,
            shared_texture: SharedTextureCaps::none(),
        }
    }

    /// `[any-thread]` Sets the presentation modes the host can drive.
    #[must_use]
    pub fn with_presentation(mut self, modes: PresentationModeSet) -> Self {
        self.presentation = modes;
        self
    }

    /// `[any-thread]` Sets the window APIs the host can hand over.
    #[must_use]
    pub fn with_window_apis(mut self, apis: WindowApiSet) -> Self {
        self.window_apis = apis;
        self
    }

    /// `[any-thread]` Sets the renderers the host can live with.
    ///
    /// A host that cannot share an OpenGL context with a plug-in, or that composites in
    /// a way software rendering would stall, says so here.
    #[must_use]
    pub fn with_renderers(mut self, renderers: GraphicRendererSet) -> Self {
        self.renderers = renderers;
        self
    }

    /// `[main-thread]` Declares the shared-texture handle kinds and formats the host can
    /// import, and enables [`PresentationMode::SharedTexture`].
    #[must_use]
    pub fn with_shared_texture(mut self, caps: SharedTextureCaps) -> Self {
        if !caps.is_empty() {
            self.presentation.insert(PresentationMode::SharedTexture);
        }
        self.shared_texture = caps;
        self
    }

    /// `[any-thread]` The presentation modes the host can drive.
    #[must_use]
    pub fn presentation(&self) -> PresentationModeSet {
        self.presentation
    }

    /// `[any-thread]` The window APIs the host can hand over.
    #[must_use]
    pub fn window_apis(&self) -> WindowApiSet {
        self.window_apis
    }

    /// `[any-thread]` The renderers the host accepts.
    #[must_use]
    pub fn renderers(&self) -> GraphicRendererSet {
        self.renderers
    }

    /// `[any-thread]` The host's shared-texture import capabilities.
    #[must_use]
    pub fn shared_texture(&self) -> &SharedTextureCaps {
        &self.shared_texture
    }

    /// `[any-thread]` Whether this host could run that profile.
    ///
    /// The rules are deliberately conservative — an unsupported combination that fails
    /// here costs a fallback, while one that slips through costs a black window:
    ///
    /// * the presentation mode must be supported;
    /// * the renderer must be accepted;
    /// * a mode that hands over a window handle needs at least one window API, and an
    ///   embedded surface needs one the host actually implements;
    /// * [`PresentationMode::SharedTexture`] needs a non-empty import capability set.
    #[must_use]
    pub fn accepts(&self, profile: GraphicProfile) -> bool {
        if !self.presentation.contains(profile.presentation) {
            return false;
        }
        if !self.renderers.contains(profile.renderer) {
            return false;
        }
        if profile.presentation.uses_window_handle() && self.window_apis.is_empty() {
            return false;
        }
        if profile.presentation == PresentationMode::SharedTexture && self.shared_texture.is_empty()
        {
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::texture::{SharedTextureKind, TextureFormat};

    fn egui_wgpu_embedded() -> GraphicProfile {
        GraphicProfile::new(
            GraphicFramework::Egui,
            GraphicRenderer::Wgpu,
            PresentationMode::EmbeddedSurface,
        )
    }

    fn egui_software_native() -> GraphicProfile {
        GraphicProfile::new(
            GraphicFramework::Egui,
            GraphicRenderer::Software,
            PresentationMode::NativeWindow,
        )
    }

    fn gpui_wgpu_shared() -> GraphicProfile {
        GraphicProfile::new(
            GraphicFramework::Gpui,
            GraphicRenderer::Wgpu,
            PresentationMode::SharedTexture,
        )
    }

    #[test]
    fn the_three_axes_stay_independent() {
        let profile = egui_wgpu_embedded();
        let repointed = profile.with_presentation(PresentationMode::SharedTexture);
        assert_eq!(repointed.framework, GraphicFramework::Egui);
        assert_eq!(repointed.renderer, GraphicRenderer::Wgpu);
        assert_eq!(repointed.presentation, PresentationMode::SharedTexture);
        assert_eq!(
            profile.with_renderer(GraphicRenderer::Software).renderer,
            GraphicRenderer::Software
        );
        assert_eq!(
            profile.to_string(),
            "egui+wgpu on embedded-surface",
            "the Display form names all three axes"
        );
    }

    #[test]
    fn capabilities_preserve_preference_order_and_reject_duplicates() {
        let caps = GraphicCapabilities::new()
            .with(egui_wgpu_embedded())
            .with(egui_software_native())
            .with(egui_wgpu_embedded());
        assert_eq!(caps.len(), 2, "the duplicate was ignored");
        assert_eq!(caps.profiles()[0], egui_wgpu_embedded());
        assert_eq!(caps.profiles()[1], egui_software_native());
        assert!(caps.supports(egui_software_native()));
        assert!(!caps.supports(gpui_wgpu_shared()));
        assert!(!caps.is_empty());
    }

    #[test]
    fn capabilities_saturate_instead_of_overflowing() {
        let mut caps = GraphicCapabilities::new();
        // 3 frameworks x 3 renderers x 4 modes = 36 distinct profiles; only 16 fit.
        let mut pushed = 0usize;
        let mut refused = 0usize;
        for framework in GraphicFrameworkSet::ALL.iter() {
            for renderer in GraphicRendererSet::ALL.iter() {
                for presentation in PresentationModeSet::ALL.iter() {
                    match caps.push(GraphicProfile::new(framework, renderer, presentation)) {
                        Ok(()) => pushed += 1,
                        Err(e) => {
                            assert_eq!(e.kind(), GraphicErrorKind::CapacityExceeded);
                            refused += 1;
                        }
                    }
                }
            }
        }
        assert_eq!(pushed, MAX_GRAPHIC_PROFILES);
        assert_eq!(refused, 36 - MAX_GRAPHIC_PROFILES);
        assert_eq!(caps.len(), MAX_GRAPHIC_PROFILES);

        // The infallible builder drops the overflow silently instead.
        let full = caps.with(gpui_wgpu_shared().with_renderer(GraphicRenderer::OpenGl));
        assert_eq!(full.len(), MAX_GRAPHIC_PROFILES);
    }

    #[test]
    fn pushing_a_duplicate_is_idempotent_even_when_full() {
        let mut caps = GraphicCapabilities::new().with(egui_wgpu_embedded());
        assert!(caps.push(egui_wgpu_embedded()).is_ok());
        assert_eq!(caps.len(), 1);
    }

    #[test]
    fn validation_demands_the_universal_fallback() {
        let empty = GraphicCapabilities::new();
        assert_eq!(
            empty.validate().unwrap_err().kind(),
            GraphicErrorKind::Unsupported
        );

        let no_fallback = GraphicCapabilities::new()
            .with(gpui_wgpu_shared())
            .with(egui_software_native());
        assert!(!no_fallback.has_fallback());
        assert!(no_fallback.validate().is_err());

        let good = no_fallback.with(egui_wgpu_embedded());
        assert!(good.has_fallback());
        assert!(good.validate().is_ok());
    }

    #[test]
    fn negotiation_lets_the_host_pick_from_the_plug_ins_order() {
        let caps = GraphicCapabilities::new()
            .with(gpui_wgpu_shared())
            .with(egui_wgpu_embedded())
            .with(egui_software_native());

        // A GPU-capable host that cannot import textures skips the first entry.
        let host = HostGraphicCaps::in_process();
        assert_eq!(caps.negotiate(&host), Some(egui_wgpu_embedded()));

        // A host that can import D3D11 textures gets the plug-in's first choice.
        let sharing = HostGraphicCaps::in_process().with_shared_texture(
            SharedTextureCaps::new()
                .with_kind(SharedTextureKind::D3D11Shared)
                .with_format(TextureFormat::Bgra8UnormSrgb),
        );
        assert_eq!(caps.negotiate(&sharing), Some(gpui_wgpu_shared()));

        // A host with no GPU at all falls back to software in a native window.
        let cpu_only = HostGraphicCaps::in_process().with_renderers(GraphicRenderer::Software.into());
        assert_eq!(caps.negotiate(&cpu_only), Some(egui_software_native()));
    }

    #[test]
    fn negotiation_fails_when_nothing_intersects() {
        let caps = GraphicCapabilities::new().with(gpui_wgpu_shared());
        let host = HostGraphicCaps::in_process();
        assert_eq!(caps.negotiate(&host), None);
        assert_eq!(
            caps.negotiate_with_fallback(&host),
            None,
            "a plug-in with no fallback profile cannot be rescued"
        );

        let with_fallback = caps.with(egui_wgpu_embedded());
        assert_eq!(
            with_fallback.negotiate_with_fallback(&host),
            Some(egui_wgpu_embedded())
        );
    }

    #[test]
    fn a_host_without_a_window_api_cannot_take_a_window() {
        let caps = GraphicCapabilities::new().with(egui_wgpu_embedded());
        let host = HostGraphicCaps::in_process().with_window_apis(WindowApiSet::EMPTY);
        assert!(!host.accepts(egui_wgpu_embedded()));
        assert_eq!(caps.negotiate(&host), None);
    }

    #[test]
    fn shared_texture_support_requires_declared_import_caps() {
        let host = HostGraphicCaps::new()
            .with_presentation(PresentationModeSet::only(PresentationMode::SharedTexture))
            .with_renderers(GraphicRendererSet::ALL);
        assert!(
            !host.accepts(gpui_wgpu_shared()),
            "claiming the mode without any importable handle kind is not an offer"
        );

        let host = host.with_shared_texture(
            SharedTextureCaps::new()
                .with_kind(SharedTextureKind::DmaBuf)
                .with_format(TextureFormat::Rgba8Unorm),
        );
        assert!(host.accepts(gpui_wgpu_shared()));
        assert!(host.presentation().contains(PresentationMode::SharedTexture));
        assert!(!host.shared_texture().is_empty());
    }

    #[test]
    fn declaring_empty_shared_texture_caps_does_not_enable_the_mode() {
        let host = HostGraphicCaps::in_process().with_shared_texture(SharedTextureCaps::none());
        assert!(!host.presentation().contains(PresentationMode::SharedTexture));
        assert!(!host.accepts(gpui_wgpu_shared()));
    }

    #[test]
    fn capability_summaries_report_each_axis() {
        let caps: GraphicCapabilities =
            [gpui_wgpu_shared(), egui_wgpu_embedded(), egui_software_native()]
                .into_iter()
                .collect();
        assert_eq!(
            caps.frameworks(),
            GraphicFrameworkSet::only(GraphicFramework::Gpui).with(GraphicFramework::Egui)
        );
        assert_eq!(
            caps.renderers(),
            GraphicRendererSet::only(GraphicRenderer::Wgpu).with(GraphicRenderer::Software)
        );
        assert_eq!(
            caps.presentation_modes(),
            PresentationModeSet::only(PresentationMode::SharedTexture)
                .with(PresentationMode::EmbeddedSurface)
                .with(PresentationMode::NativeWindow)
        );
        assert_eq!(GraphicCapabilities::default().len(), 0);
    }

    #[test]
    fn bit_sets_behave_like_sets() {
        let mut set = PresentationModeSet::EMPTY;
        assert!(set.is_empty());
        set.insert(PresentationMode::NativeWindow);
        set.insert(PresentationMode::NativeWindow);
        assert_eq!(set.len(), 1);
        assert!(set.contains(PresentationMode::NativeWindow));
        set.remove(PresentationMode::SharedTexture);
        assert_eq!(set.len(), 1);
        set.remove(PresentationMode::NativeWindow);
        assert!(set.is_empty());

        let all = PresentationModeSet::ALL;
        assert_eq!(all.len(), 4);
        assert_eq!(all.iter().count(), 4);
        assert_eq!(
            all.intersection(PresentationModeSet::only(PresentationMode::SharedTexture)),
            PresentationModeSet::only(PresentationMode::SharedTexture)
        );
        assert!(all.intersects(PresentationModeSet::only(PresentationMode::ExternalWindow)));
        assert!(!PresentationModeSet::EMPTY.intersects(all));
        assert_eq!(
            PresentationModeSet::from_bits_truncate(u32::MAX),
            all,
            "unknown bits from a newer peer are dropped"
        );
        assert_eq!(
            all.without(PresentationMode::NativeWindow)
                | PresentationModeSet::only(PresentationMode::NativeWindow),
            all
        );
        assert_eq!(format!("{:?}", PresentationModeSet::only(PresentationMode::NativeWindow)), "{NativeWindow}");
    }

    #[test]
    fn axis_names_are_stable_and_distinct() {
        assert_eq!(GraphicFramework::Egui.to_string(), "egui");
        assert_eq!(GraphicRenderer::OpenGl.to_string(), "opengl");
        assert_eq!(PresentationMode::SharedTexture.to_string(), "shared-texture");
        assert!(GraphicRenderer::Wgpu.needs_gpu());
        assert!(!GraphicRenderer::Software.needs_gpu());
        assert!(PresentationMode::EmbeddedSurface.uses_window_handle());
        assert!(!PresentationMode::SharedTexture.uses_window_handle());
        assert!(!PresentationMode::ExternalWindow.uses_window_handle());
    }
}
