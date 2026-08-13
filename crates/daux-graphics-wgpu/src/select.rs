//! Turning what a plug-in asked for into something the surface actually supports.
//!
//! Every function here is pure: it takes the [`wgpu::SurfaceCapabilities`] the adapter
//! reported and a preference, and returns something guaranteed to be in that capability list.
//! Keeping the choice separate from the GPU calls is what makes it testable on a machine with
//! no GPU at all — and the choice is where the interesting failures are. Configuring a surface
//! with an unsupported present mode is a validation error or, on some drivers, a hang.

use crate::{AlphaPreference, FormatPreference, Vsync};

/// [main-thread] Picks a swapchain colour format.
///
/// Falls back to the surface's own first choice when the preference cannot be met, and to
/// [`wgpu::TextureFormat::Bgra8UnormSrgb`] when the surface reports no formats at all — which
/// happens when the surface and the adapter are incompatible, and which the caller detects
/// and reports separately rather than crashing here.
#[must_use]
pub fn choose_format(
    caps: &wgpu::SurfaceCapabilities,
    preference: FormatPreference,
) -> wgpu::TextureFormat {
    let matching = |want_srgb: bool| {
        caps.formats
            .iter()
            .copied()
            .find(|f| f.is_srgb() == want_srgb)
    };
    let chosen = match preference {
        FormatPreference::Srgb => matching(true),
        FormatPreference::Linear => matching(false),
        FormatPreference::Any => None,
    };
    chosen
        .or_else(|| caps.formats.first().copied())
        .unwrap_or(wgpu::TextureFormat::Bgra8UnormSrgb)
}

/// [main-thread] Picks a present mode.
///
/// [`wgpu::PresentMode::Fifo`] is the one mode the specification guarantees, so it is both the
/// answer for [`Vsync::On`] and the fallback for everything else. `Immediate` and `Mailbox`
/// are *not* requested unless the surface lists them: wgpu documents that configuring an
/// unsupported one panics.
#[must_use]
pub fn choose_present_mode(caps: &wgpu::SurfaceCapabilities, vsync: Vsync) -> wgpu::PresentMode {
    let supports = |mode: wgpu::PresentMode| caps.present_modes.contains(&mode);
    match vsync {
        Vsync::On => wgpu::PresentMode::Fifo,
        Vsync::Off => {
            if supports(wgpu::PresentMode::Immediate) {
                wgpu::PresentMode::Immediate
            } else if supports(wgpu::PresentMode::Mailbox) {
                wgpu::PresentMode::Mailbox
            } else {
                wgpu::PresentMode::Fifo
            }
        }
        Vsync::LowLatency => {
            if supports(wgpu::PresentMode::Mailbox) {
                wgpu::PresentMode::Mailbox
            } else {
                wgpu::PresentMode::Fifo
            }
        }
    }
}

/// [main-thread] Picks an alpha compositing mode.
///
/// `alpha_modes` always holds at least one entry, so the final fallback is unreachable in
/// practice; it is written out anyway because "always" is a claim about every driver ever
/// shipped and this code runs inside someone else's process.
#[must_use]
pub fn choose_alpha_mode(
    caps: &wgpu::SurfaceCapabilities,
    preference: AlphaPreference,
) -> wgpu::CompositeAlphaMode {
    let supports = |mode: wgpu::CompositeAlphaMode| caps.alpha_modes.contains(&mode);
    let ordered: &[wgpu::CompositeAlphaMode] = match preference {
        AlphaPreference::Opaque => &[
            wgpu::CompositeAlphaMode::Opaque,
            wgpu::CompositeAlphaMode::Inherit,
            wgpu::CompositeAlphaMode::Auto,
        ],
        AlphaPreference::Transparent => &[
            wgpu::CompositeAlphaMode::PreMultiplied,
            wgpu::CompositeAlphaMode::PostMultiplied,
            wgpu::CompositeAlphaMode::Inherit,
            wgpu::CompositeAlphaMode::Auto,
        ],
    };
    ordered
        .iter()
        .copied()
        .find(|m| supports(*m))
        .or_else(|| caps.alpha_modes.first().copied())
        .unwrap_or(wgpu::CompositeAlphaMode::Auto)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(
        formats: &[wgpu::TextureFormat],
        present: &[wgpu::PresentMode],
        alpha: &[wgpu::CompositeAlphaMode],
    ) -> wgpu::SurfaceCapabilities {
        wgpu::SurfaceCapabilities {
            formats: formats.to_vec(),
            present_modes: present.to_vec(),
            alpha_modes: alpha.to_vec(),
            ..Default::default()
        }
    }

    #[test]
    fn an_srgb_preference_finds_an_srgb_format_wherever_it_is_in_the_list() {
        let c = caps(
            &[
                wgpu::TextureFormat::Bgra8Unorm,
                wgpu::TextureFormat::Rgba8Unorm,
                wgpu::TextureFormat::Bgra8UnormSrgb,
            ],
            &[],
            &[],
        );
        let chosen = choose_format(&c, FormatPreference::Srgb);
        assert!(chosen.is_srgb(), "{chosen:?} is not an sRGB format");
        assert_eq!(chosen, wgpu::TextureFormat::Bgra8UnormSrgb);
    }

    #[test]
    fn a_linear_preference_avoids_srgb() {
        let c = caps(
            &[
                wgpu::TextureFormat::Bgra8UnormSrgb,
                wgpu::TextureFormat::Bgra8Unorm,
            ],
            &[],
            &[],
        );
        let chosen = choose_format(&c, FormatPreference::Linear);
        assert!(!chosen.is_srgb());
        assert_eq!(chosen, wgpu::TextureFormat::Bgra8Unorm);
    }

    #[test]
    fn any_takes_the_surfaces_own_first_choice() {
        let c = caps(
            &[
                wgpu::TextureFormat::Rgba16Float,
                wgpu::TextureFormat::Bgra8UnormSrgb,
            ],
            &[],
            &[],
        );
        assert_eq!(
            choose_format(&c, FormatPreference::Any),
            wgpu::TextureFormat::Rgba16Float
        );
    }

    #[test]
    fn an_unmeetable_format_preference_falls_back_to_something_the_surface_listed() {
        // A surface with only linear formats must not be configured with an sRGB one: it is a
        // validation error, and the editor never appears.
        let linear_only = caps(&[wgpu::TextureFormat::Bgra8Unorm], &[], &[]);
        let chosen = choose_format(&linear_only, FormatPreference::Srgb);
        assert_eq!(chosen, wgpu::TextureFormat::Bgra8Unorm);
        assert!(linear_only.formats.contains(&chosen));

        let srgb_only = caps(&[wgpu::TextureFormat::Bgra8UnormSrgb], &[], &[]);
        let chosen = choose_format(&srgb_only, FormatPreference::Linear);
        assert!(srgb_only.formats.contains(&chosen));
    }

    #[test]
    fn an_empty_format_list_still_produces_a_format_rather_than_panicking() {
        // `get_capabilities` returns an empty list when the surface and adapter cannot work
        // together. The caller reports that; this function must not be what falls over first.
        let empty = caps(&[], &[], &[]);
        for preference in [
            FormatPreference::Srgb,
            FormatPreference::Linear,
            FormatPreference::Any,
        ] {
            assert_eq!(
                choose_format(&empty, preference),
                wgpu::TextureFormat::Bgra8UnormSrgb
            );
        }
    }

    #[test]
    fn vsync_on_always_picks_the_one_guaranteed_mode() {
        for modes in [
            vec![],
            vec![wgpu::PresentMode::Fifo],
            vec![
                wgpu::PresentMode::Immediate,
                wgpu::PresentMode::Mailbox,
                wgpu::PresentMode::Fifo,
            ],
        ] {
            let c = caps(&[], &modes, &[]);
            assert_eq!(
                choose_present_mode(&c, Vsync::On),
                wgpu::PresentMode::Fifo,
                "Fifo is the only mode every surface supports"
            );
        }
    }

    #[test]
    fn an_unsupported_present_mode_is_never_requested() {
        // wgpu documents that configuring an unsupported Immediate or Mailbox panics, so this
        // is the difference between a fallback and taking the host down.
        let fifo_only = caps(&[], &[wgpu::PresentMode::Fifo], &[]);
        assert_eq!(
            choose_present_mode(&fifo_only, Vsync::Off),
            wgpu::PresentMode::Fifo
        );
        assert_eq!(
            choose_present_mode(&fifo_only, Vsync::LowLatency),
            wgpu::PresentMode::Fifo
        );

        let mailbox = caps(
            &[],
            &[wgpu::PresentMode::Fifo, wgpu::PresentMode::Mailbox],
            &[],
        );
        assert_eq!(
            choose_present_mode(&mailbox, Vsync::LowLatency),
            wgpu::PresentMode::Mailbox
        );
        assert_eq!(
            choose_present_mode(&mailbox, Vsync::Off),
            wgpu::PresentMode::Mailbox,
            "with no Immediate available, Mailbox is the closest to unthrottled"
        );

        let immediate = caps(
            &[],
            &[
                wgpu::PresentMode::Fifo,
                wgpu::PresentMode::Mailbox,
                wgpu::PresentMode::Immediate,
            ],
            &[],
        );
        assert_eq!(
            choose_present_mode(&immediate, Vsync::Off),
            wgpu::PresentMode::Immediate
        );
    }

    #[test]
    fn every_chosen_present_mode_is_one_the_surface_listed() {
        let listed = [
            wgpu::PresentMode::Fifo,
            wgpu::PresentMode::Mailbox,
            wgpu::PresentMode::Immediate,
        ];
        for count in 1..=listed.len() {
            let c = caps(&[], &listed[..count], &[]);
            for vsync in [Vsync::On, Vsync::Off, Vsync::LowLatency] {
                let chosen = choose_present_mode(&c, vsync);
                assert!(
                    c.present_modes.contains(&chosen),
                    "{vsync:?} chose {chosen:?}, which {:?} does not list",
                    c.present_modes
                );
            }
        }
    }

    #[test]
    fn opaque_is_preferred_and_falls_back_through_inherit() {
        let opaque = caps(&[], &[], &[wgpu::CompositeAlphaMode::Opaque]);
        assert_eq!(
            choose_alpha_mode(&opaque, AlphaPreference::Opaque),
            wgpu::CompositeAlphaMode::Opaque
        );

        // Wayland surfaces commonly report only `Inherit`.
        let inherit = caps(&[], &[], &[wgpu::CompositeAlphaMode::Inherit]);
        assert_eq!(
            choose_alpha_mode(&inherit, AlphaPreference::Opaque),
            wgpu::CompositeAlphaMode::Inherit
        );
    }

    #[test]
    fn a_transparent_request_falls_back_to_whatever_is_available() {
        let opaque_only = caps(&[], &[], &[wgpu::CompositeAlphaMode::Opaque]);
        let chosen = choose_alpha_mode(&opaque_only, AlphaPreference::Transparent);
        assert_eq!(
            chosen,
            wgpu::CompositeAlphaMode::Opaque,
            "a surface that cannot blend gets an opaque editor, not a failed one"
        );

        let premultiplied = caps(
            &[],
            &[],
            &[
                wgpu::CompositeAlphaMode::Opaque,
                wgpu::CompositeAlphaMode::PreMultiplied,
            ],
        );
        assert_eq!(
            choose_alpha_mode(&premultiplied, AlphaPreference::Transparent),
            wgpu::CompositeAlphaMode::PreMultiplied
        );
    }

    #[test]
    fn every_chosen_alpha_mode_is_one_the_surface_listed() {
        let all = [
            wgpu::CompositeAlphaMode::Auto,
            wgpu::CompositeAlphaMode::Opaque,
            wgpu::CompositeAlphaMode::PreMultiplied,
            wgpu::CompositeAlphaMode::PostMultiplied,
            wgpu::CompositeAlphaMode::Inherit,
        ];
        for mode in all {
            let c = caps(&[], &[], &[mode]);
            for preference in [AlphaPreference::Opaque, AlphaPreference::Transparent] {
                let chosen = choose_alpha_mode(&c, preference);
                assert_eq!(
                    chosen, mode,
                    "{preference:?} chose {chosen:?} from a surface offering only {mode:?}"
                );
            }
        }
    }

    #[test]
    fn an_empty_alpha_list_still_produces_a_mode() {
        let empty = caps(&[], &[], &[]);
        assert_eq!(
            choose_alpha_mode(&empty, AlphaPreference::Opaque),
            wgpu::CompositeAlphaMode::Auto
        );
    }
}
