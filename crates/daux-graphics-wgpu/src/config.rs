//! What a plug-in asks for when it creates a device and a swapchain.

use daux_graphics::{GraphicFramework, GraphicProfile, GraphicRenderer, PresentationMode};

/// [main-thread] The profile this backend offers, for a given presentation mode.
///
/// The framework is always [`GraphicFramework::Custom`]: this crate renders, it does not lay
/// out widgets. A UI toolkit that draws through it — `daux-graphics-egui` with a wgpu painter,
/// for instance — advertises its own framework instead.
#[must_use]
pub fn profile(presentation: PresentationMode) -> GraphicProfile {
    GraphicProfile::new(
        GraphicFramework::Custom,
        GraphicRenderer::Wgpu,
        presentation,
    )
}

/// [main-thread] The profiles a wgpu renderer can honour, most preferred first.
///
/// An embedded surface first, because it is the mode every host can provide and the one
/// `daux-graphics` requires as a fallback; a native window second, for a standalone preview.
/// [`PresentationMode::SharedTexture`] is deliberately absent: rendering to a texture the host
/// imports needs a negotiated handle kind and an explicit fence, and advertising it without
/// implementing that produces a negotiation the host then cannot satisfy.
#[must_use]
pub fn capabilities() -> daux_graphics::GraphicCapabilities {
    daux_graphics::GraphicCapabilities::new()
        .with(profile(PresentationMode::EmbeddedSurface))
        .with(profile(PresentationMode::NativeWindow))
}

/// [any-thread] Which swapchain presentation behaviour a plug-in wants.
///
/// Editors are not games: a plug-in that spins the GPU at 500 fps to draw a knob is stealing
/// power budget from the audio thread that shares the machine with it. [`Vsync::On`] is the
/// default and the only one a shipping plug-in should normally use.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum Vsync {
    /// Tear-free, frame-rate limited to the display. Always supported.
    #[default]
    On,
    /// Present as fast as the GPU can, tearing if it must. For profiling, not for shipping.
    Off,
    /// Tear-free and *not* frame-rate limited: the newest frame replaces the queued one.
    ///
    /// Lower latency than [`On`](Self::On) where it is supported, and it falls back to
    /// [`On`](Self::On) where it is not.
    LowLatency,
}

/// [any-thread] Which swapchain colour format a plug-in prefers.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum FormatPreference {
    /// An sRGB format, so the GPU converts on write and blending happens in linear space.
    ///
    /// The right default for a UI: colours picked in a design tool are sRGB, and a
    /// non-sRGB swapchain makes every gradient and every anti-aliased edge too dark.
    #[default]
    Srgb,
    /// A non-sRGB format, for a renderer that does its own colour conversion.
    Linear,
    /// Whatever the surface lists first.
    Any,
}

/// [any-thread] How the swapchain's alpha channel is composited by the window system.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum AlphaPreference {
    /// Ignore alpha; the editor is a solid rectangle. What a plug-in editor wants, because a
    /// host composites it into its own window and a stray alpha channel shows the DAW's
    /// background through the UI.
    #[default]
    Opaque,
    /// Ask the compositor to blend the editor over what is behind it.
    Transparent,
}

/// What a plug-in asks the GPU for.
///
/// Every field is a *preference*: the surface reports what it can actually do and this crate
/// picks the nearest match, because an editor that refuses to open because the swapchain does
/// not support one particular present mode is worse than one that runs at a slightly different
/// present mode.
///
/// [main-thread]
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct SurfaceConfig {
    /// Which GPU to prefer when the machine has more than one.
    pub power_preference: wgpu::PowerPreference,
    /// Which graphics APIs to consider.
    pub backends: wgpu::Backends,
    /// Presentation behaviour.
    pub vsync: Vsync,
    /// Swapchain colour format.
    pub format: FormatPreference,
    /// Swapchain alpha behaviour.
    pub alpha: AlphaPreference,
    /// How many display refreshes may be in flight. Clamped to `1..=4` on use.
    pub frame_latency: u32,
    /// Label the device and swapchain carry in graphics debuggers and validation messages.
    pub label: &'static str,
}

impl Default for SurfaceConfig {
    /// [main-thread] The settings a plug-in editor should normally ship with.
    ///
    /// A low-power adapter, because an editor is a few thousand triangles and waking the
    /// discrete GPU for it costs the user battery and, on some laptops, an audible fan; vsync
    /// on; an sRGB, opaque swapchain; and two frames of latency, which is the balance between
    /// input lag and stutter that a UI wants.
    fn default() -> Self {
        Self {
            power_preference: wgpu::PowerPreference::LowPower,
            backends: wgpu::Backends::all(),
            vsync: Vsync::On,
            format: FormatPreference::Srgb,
            alpha: AlphaPreference::Opaque,
            frame_latency: 2,
            label: "daux editor",
        }
    }
}

impl SurfaceConfig {
    /// [main-thread] The default configuration.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// [main-thread] Prefers the discrete GPU. Rarely right for an editor.
    #[must_use]
    pub const fn with_power_preference(mut self, preference: wgpu::PowerPreference) -> Self {
        self.power_preference = preference;
        self
    }

    /// [main-thread] Restricts which graphics APIs may be used.
    #[must_use]
    pub const fn with_backends(mut self, backends: wgpu::Backends) -> Self {
        self.backends = backends;
        self
    }

    /// [main-thread] Sets the presentation behaviour.
    #[must_use]
    pub const fn with_vsync(mut self, vsync: Vsync) -> Self {
        self.vsync = vsync;
        self
    }

    /// [main-thread] Sets the swapchain colour format preference.
    #[must_use]
    pub const fn with_format(mut self, format: FormatPreference) -> Self {
        self.format = format;
        self
    }

    /// [main-thread] Sets the swapchain alpha behaviour.
    #[must_use]
    pub const fn with_alpha(mut self, alpha: AlphaPreference) -> Self {
        self.alpha = alpha;
        self
    }

    /// [main-thread] Sets how many display refreshes may be in flight.
    #[must_use]
    pub const fn with_frame_latency(mut self, frames: u32) -> Self {
        self.frame_latency = frames;
        self
    }

    /// [main-thread] Sets the debug label.
    #[must_use]
    pub const fn with_label(mut self, label: &'static str) -> Self {
        self.label = label;
        self
    }

    /// [main-thread] The frame latency actually used, clamped to a sane range.
    ///
    /// Zero is not a valid swapchain depth, and a large value trades latency the user can feel
    /// for throughput an editor does not need. The clamp is here rather than in the setter so
    /// that a struct built by hand — the type has public fields — is just as safe.
    #[must_use]
    pub const fn effective_frame_latency(&self) -> u32 {
        if self.frame_latency < 1 {
            1
        } else if self.frame_latency > 4 {
            4
        } else {
            self.frame_latency
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use daux_graphics::HostGraphicCaps;

    #[test]
    fn the_default_configuration_is_the_one_a_plug_in_should_ship() {
        let config = SurfaceConfig::new();
        assert_eq!(config.power_preference, wgpu::PowerPreference::LowPower);
        assert_eq!(config.vsync, Vsync::On);
        assert_eq!(config.format, FormatPreference::Srgb);
        assert_eq!(config.alpha, AlphaPreference::Opaque);
        assert_eq!(config.effective_frame_latency(), 2);
    }

    #[test]
    fn the_builders_change_one_thing_each() {
        let config = SurfaceConfig::new()
            .with_vsync(Vsync::LowLatency)
            .with_format(FormatPreference::Linear)
            .with_alpha(AlphaPreference::Transparent)
            .with_power_preference(wgpu::PowerPreference::HighPerformance)
            .with_backends(wgpu::Backends::VULKAN)
            .with_frame_latency(3)
            .with_label("test");
        assert_eq!(config.vsync, Vsync::LowLatency);
        assert_eq!(config.format, FormatPreference::Linear);
        assert_eq!(config.alpha, AlphaPreference::Transparent);
        assert_eq!(
            config.power_preference,
            wgpu::PowerPreference::HighPerformance
        );
        assert_eq!(config.backends, wgpu::Backends::VULKAN);
        assert_eq!(config.effective_frame_latency(), 3);
        assert_eq!(config.label, "test");
    }

    #[test]
    fn a_nonsense_frame_latency_is_clamped_rather_than_handed_to_the_driver() {
        // A depth of zero is not a swapchain; a depth of a thousand is seconds of input lag.
        let mut config = SurfaceConfig::new();
        config.frame_latency = 0;
        assert_eq!(config.effective_frame_latency(), 1);
        config.frame_latency = u32::MAX;
        assert_eq!(config.effective_frame_latency(), 4);
        config.frame_latency = 1;
        assert_eq!(config.effective_frame_latency(), 1);
        config.frame_latency = 4;
        assert_eq!(config.effective_frame_latency(), 4);
    }

    #[test]
    fn the_offered_profiles_include_the_mandatory_fallback_and_negotiate() {
        let caps = capabilities();
        assert_eq!(caps.len(), 2);
        assert!(
            caps.has_fallback(),
            "without an embedded-surface profile the editor is invisible in a VST3 host"
        );
        assert_eq!(
            caps.profiles()[0],
            profile(PresentationMode::EmbeddedSurface),
            "the mode every host supports must be offered first"
        );
        assert_eq!(
            caps.negotiate(&HostGraphicCaps::in_process()),
            Some(profile(PresentationMode::EmbeddedSurface))
        );
        caps.validate().expect("the offered set must be valid");

        for p in caps.profiles() {
            assert_eq!(p.framework, GraphicFramework::Custom);
            assert_eq!(p.renderer, GraphicRenderer::Wgpu);
            assert_ne!(
                p.presentation,
                PresentationMode::SharedTexture,
                "shared textures need negotiation and a fence this backend does not implement"
            );
        }
    }

    #[test]
    fn a_host_with_no_gpu_cannot_be_given_a_wgpu_profile() {
        let cpu_only =
            HostGraphicCaps::in_process().with_renderers(GraphicRenderer::Software.into());
        assert_eq!(
            capabilities().negotiate(&cpu_only),
            None,
            "a host that refuses GPU renderers must not be handed one anyway"
        );
    }
}
