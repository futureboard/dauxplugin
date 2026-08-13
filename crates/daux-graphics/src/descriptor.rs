//! What an editor tells the host about itself before it is opened.

use crate::{GraphicCapabilities, GraphicError, GraphicProfile, LogicalSize};

/// The static description of a plug-in editor.
///
/// A host reads this before creating anything, to decide how big to make the window it will
/// hand over and whether it may let the user resize it. Everything here is in **logical**
/// units: the host applies the display's scale factor, so an editor never has to.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct GraphicDescriptor {
    /// The framework/renderer/presentation combinations this editor can offer, in the
    /// editor's own order of preference.
    pub capabilities: GraphicCapabilities,
    /// The size the editor would like to open at.
    pub preferred_size: LogicalSize,
    /// The smallest size it can usefully be shown at, if it has a floor.
    pub min_size: Option<LogicalSize>,
    /// The largest size it can usefully be shown at, if it has a ceiling.
    pub max_size: Option<LogicalSize>,
    /// Whether the user may resize the editor at all.
    pub resizable: bool,
    /// A width÷height ratio the host must preserve while resizing, if any.
    pub keeps_aspect: Option<f64>,
}

impl GraphicDescriptor {
    /// [main-thread] A fixed-size editor with the given capabilities.
    ///
    /// The common case: an editor with a designed layout that does not reflow.
    pub fn fixed(capabilities: GraphicCapabilities, size: LogicalSize) -> Self {
        Self {
            capabilities,
            preferred_size: size,
            min_size: None,
            max_size: None,
            resizable: false,
            keeps_aspect: None,
        }
    }

    /// [main-thread] A resizable editor with a floor and no ceiling.
    pub fn resizable(
        capabilities: GraphicCapabilities,
        preferred: LogicalSize,
        min: LogicalSize,
    ) -> Self {
        Self {
            capabilities,
            preferred_size: preferred,
            min_size: Some(min),
            max_size: None,
            resizable: true,
            keeps_aspect: None,
        }
    }

    /// Sets an upper bound on the editor's size.
    #[must_use]
    pub fn with_max_size(mut self, max: LogicalSize) -> Self {
        self.max_size = Some(max);
        self
    }

    /// Asks the host to preserve the preferred size's aspect ratio while resizing.
    ///
    /// Ignored when the preferred size has no usable ratio.
    #[must_use]
    pub fn keeping_aspect(mut self) -> Self {
        self.keeps_aspect = self.preferred_size.aspect_ratio();
        self
    }

    /// Asks the host to preserve an explicit width÷height ratio while resizing.
    #[must_use]
    pub fn with_aspect_ratio(mut self, ratio: f64) -> Self {
        self.keeps_aspect = (ratio.is_finite() && ratio > 0.0).then_some(ratio);
        self
    }

    /// [main-thread] Clamps a proposed size to what this editor will accept.
    ///
    /// A host should route every resize through this rather than trusting the user's drag:
    /// it applies the bounds and the aspect ratio in the order that cannot produce a size
    /// outside the bounds.
    pub fn clamp(&self, proposed: LogicalSize) -> LogicalSize {
        if !self.resizable {
            return self.preferred_size;
        }
        let bounded = proposed.clamped(self.min_size, self.max_size);
        match self.keeps_aspect {
            // Re-clamp: fixing the aspect ratio can push a dimension back out of bounds.
            Some(ratio) => bounded
                .with_aspect_ratio(ratio)
                .clamped(self.min_size, self.max_size),
            None => bounded,
        }
    }

    /// [main-thread] Rejects a descriptor a host cannot act on.
    ///
    /// # Errors
    ///
    /// [`GraphicErrorKind::InvalidArgument`](crate::GraphicErrorKind::InvalidArgument) when a
    /// size is not usable or the bounds contradict each other, and whatever
    /// [`GraphicCapabilities::validate`] reports.
    pub fn validate(&self) -> Result<(), GraphicError> {
        self.capabilities.validate()?;
        if !self.preferred_size.is_usable() {
            return Err(GraphicError::invalid_argument(
                "the preferred editor size is not a usable size",
            ));
        }
        if let Some(min) = self.min_size
            && !min.is_usable()
        {
            return Err(GraphicError::invalid_argument(
                "the minimum editor size is not a usable size",
            ));
        }
        if let Some(max) = self.max_size
            && !max.is_usable()
        {
            return Err(GraphicError::invalid_argument(
                "the maximum editor size is not a usable size",
            ));
        }
        if let (Some(min), Some(max)) = (self.min_size, self.max_size)
            && (min.width > max.width || min.height > max.height)
        {
            return Err(GraphicError::invalid_argument(
                "the minimum editor size exceeds the maximum",
            ));
        }
        Ok(())
    }

    /// [main-thread] The profile this editor and `host` agree on, if any.
    ///
    /// A convenience over [`GraphicCapabilities::negotiate_with_fallback`].
    pub fn negotiate(&self, host: &crate::HostGraphicCaps) -> Option<GraphicProfile> {
        self.capabilities.negotiate_with_fallback(host)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GraphicFramework, GraphicRenderer, PresentationMode};

    fn caps() -> GraphicCapabilities {
        GraphicCapabilities::new().with(GraphicProfile::new(
            GraphicFramework::Egui,
            GraphicRenderer::Wgpu,
            PresentationMode::EmbeddedSurface,
        ))
    }

    #[test]
    fn a_fixed_editor_refuses_every_proposed_size() {
        let d = GraphicDescriptor::fixed(caps(), LogicalSize::new(640.0, 480.0));
        assert!(!d.resizable);
        d.validate().unwrap();
        assert_eq!(d.clamp(LogicalSize::new(2000.0, 2000.0)), d.preferred_size);
        assert_eq!(d.clamp(LogicalSize::new(1.0, 1.0)), d.preferred_size);
    }

    #[test]
    fn a_resizable_editor_clamps_to_its_bounds() {
        let d = GraphicDescriptor::resizable(
            caps(),
            LogicalSize::new(800.0, 600.0),
            LogicalSize::new(400.0, 300.0),
        )
        .with_max_size(LogicalSize::new(1600.0, 1200.0));
        d.validate().unwrap();

        assert_eq!(
            d.clamp(LogicalSize::new(100.0, 100.0)),
            LogicalSize::new(400.0, 300.0)
        );
        assert_eq!(
            d.clamp(LogicalSize::new(9000.0, 9000.0)),
            LogicalSize::new(1600.0, 1200.0)
        );
        assert_eq!(
            d.clamp(LogicalSize::new(1000.0, 700.0)),
            LogicalSize::new(1000.0, 700.0)
        );
    }

    #[test]
    fn a_locked_aspect_ratio_never_escapes_the_bounds() {
        let d = GraphicDescriptor::resizable(
            caps(),
            LogicalSize::new(800.0, 400.0),
            LogicalSize::new(400.0, 200.0),
        )
        .with_max_size(LogicalSize::new(1600.0, 800.0))
        .keeping_aspect();
        assert_eq!(d.keeps_aspect, Some(2.0));

        let clamped = d.clamp(LogicalSize::new(9000.0, 100.0));
        assert!(clamped.width <= 1600.0, "{clamped:?}");
        assert!(clamped.height <= 800.0, "{clamped:?}");
        assert!(clamped.width >= 400.0, "{clamped:?}");
        assert!(clamped.height >= 200.0, "{clamped:?}");
    }

    #[test]
    fn a_nonsense_aspect_ratio_is_dropped_rather_than_stored() {
        let d = GraphicDescriptor::fixed(caps(), LogicalSize::new(100.0, 100.0));
        assert_eq!(d.clone().with_aspect_ratio(f64::NAN).keeps_aspect, None);
        assert_eq!(d.clone().with_aspect_ratio(0.0).keeps_aspect, None);
        assert_eq!(d.clone().with_aspect_ratio(-2.0).keeps_aspect, None);
        assert_eq!(d.with_aspect_ratio(1.5).keeps_aspect, Some(1.5));
    }

    #[test]
    fn validation_catches_contradictory_bounds() {
        let d = GraphicDescriptor::resizable(
            caps(),
            LogicalSize::new(800.0, 600.0),
            LogicalSize::new(900.0, 700.0),
        )
        .with_max_size(LogicalSize::new(400.0, 300.0));
        assert!(d.validate().is_err());
    }

    #[test]
    fn validation_catches_an_unusable_size() {
        let d = GraphicDescriptor::fixed(caps(), LogicalSize::new(0.0, 480.0));
        assert!(d.validate().is_err());

        let d = GraphicDescriptor::fixed(caps(), LogicalSize::new(f64::NAN, 480.0));
        assert!(d.validate().is_err());
    }
}
