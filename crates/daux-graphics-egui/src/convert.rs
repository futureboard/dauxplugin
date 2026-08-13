//! Translating DAUx's framework-neutral input vocabulary into egui's.
//!
//! Every function here is a pure mapping with no state. The stateful part — remembering the
//! modifier set, the pointer position and the surface size between events — lives in
//! [`InputTranslator`](crate::InputTranslator), which is built on top of these.
//!
//! Anything egui has no equivalent for is reported as [`None`] and dropped by the caller,
//! never mapped onto "something close". A mouse button reported as the wrong button or a key
//! reported as the wrong key is worse than a lost event: it fires the wrong action.

use daux_graphics::{Key, LogicalPoint, Modifiers, PointerButton};

/// [main-thread] Maps DAUx modifiers onto egui's.
///
/// egui carries two extra fields DAUx does not: `mac_cmd`, which is the ⌘ key and must be
/// `false` off macOS, and `command`, egui's platform-independent shortcut modifier. Both are
/// derived here so that `Ctrl+A` on Windows and `⌘A` on macOS reach egui as the same
/// `command` shortcut — which is exactly the mapping
/// [`Modifiers::command`](daux_graphics::Modifiers::command) already describes.
#[must_use]
pub fn modifiers(m: Modifiers) -> egui::Modifiers {
    let mac_cmd = cfg!(target_os = "macos") && m.meta;
    egui::Modifiers {
        alt: m.alt,
        ctrl: m.ctrl,
        shift: m.shift,
        mac_cmd,
        command: m.command(),
    }
}

/// [main-thread] Maps a DAUx pointer button onto egui's.
///
/// egui names exactly two extra buttons, and the platform convention is that the first two
/// unnamed buttons are "back" and "forward"; those become `Extra1`/`Extra2`. A button beyond
/// that has no egui equivalent and is dropped rather than reported as a left click.
#[must_use]
pub fn button(b: PointerButton) -> Option<egui::PointerButton> {
    Some(match b {
        PointerButton::Primary => egui::PointerButton::Primary,
        PointerButton::Secondary => egui::PointerButton::Secondary,
        PointerButton::Middle => egui::PointerButton::Middle,
        PointerButton::Other(0) => egui::PointerButton::Extra1,
        PointerButton::Other(1) => egui::PointerButton::Extra2,
        // `PointerButton` is `#[non_exhaustive]`: a button this build does not know about is
        // dropped, never reported as a click on some other button.
        _ => return None,
    })
}

/// [main-thread] Maps a DAUx key onto egui's.
///
/// [`Key::Unknown`] carries a raw platform scan code, which cannot be turned into egui's
/// layout-independent vocabulary without guessing — so it is dropped. Characters are never
/// reconstructed from key codes; they arrive as
/// [`InputEvent::Text`](daux_graphics::InputEvent::Text) instead.
#[must_use]
pub fn key(k: Key) -> Option<egui::Key> {
    Some(match k {
        Key::Enter => egui::Key::Enter,
        Key::Escape => egui::Key::Escape,
        Key::Tab => egui::Key::Tab,
        Key::Backspace => egui::Key::Backspace,
        Key::Delete => egui::Key::Delete,
        Key::Space => egui::Key::Space,
        Key::ArrowUp => egui::Key::ArrowUp,
        Key::ArrowDown => egui::Key::ArrowDown,
        Key::ArrowLeft => egui::Key::ArrowLeft,
        Key::ArrowRight => egui::Key::ArrowRight,
        Key::Home => egui::Key::Home,
        Key::End => egui::Key::End,
        Key::PageUp => egui::Key::PageUp,
        Key::PageDown => egui::Key::PageDown,
        // `Key` is `#[non_exhaustive]`, and `Key::Unknown` is a raw platform code.
        _ => return None,
    })
}

/// [main-thread] Maps a logical point onto an egui position.
///
/// Both are in logical units with the origin at the editor's top-left corner and `y` growing
/// downwards, so this is a narrowing cast and nothing more. egui works in `f32`; a coordinate
/// that is not finite is clamped to the origin rather than poisoning egui's hit testing with
/// a `NaN` that would then compare `false` against every rectangle.
#[must_use]
pub fn pos(p: LogicalPoint) -> egui::Pos2 {
    if p.x.is_finite() && p.y.is_finite() {
        egui::pos2(p.x as f32, p.y as f32)
    } else {
        egui::Pos2::ZERO
    }
}

/// [main-thread] Maps a DAUx scroll delta onto egui's wheel delta, in points.
///
/// The sign convention is passed through unchanged, matching what every `winit`-based egui
/// integration does: the platform's wheel delta goes straight into
/// [`egui::Event::MouseWheel`]. Non-finite components become zero, because an infinite scroll
/// delta scrolls an egui `ScrollArea` to a `NaN` offset it never recovers from.
#[must_use]
pub fn scroll_delta(delta_x: f64, delta_y: f64) -> egui::Vec2 {
    let finite = |v: f64| if v.is_finite() { v as f32 } else { 0.0 };
    egui::vec2(finite(delta_x), finite(delta_y))
}

/// Whether a character is one a text field should receive.
///
/// Control characters (including the carriage return and the delete character some platforms
/// deliver alongside a key event) and the private-use areas — where several platforms encode
/// function and media keys — are not text. Letting them through types a replacement glyph
/// into whatever field has focus.
fn is_printable(c: char) -> bool {
    let private_use = ('\u{e000}'..='\u{f8ff}').contains(&c)
        || ('\u{f0000}'..='\u{ffffd}').contains(&c)
        || ('\u{100000}'..='\u{10fffd}').contains(&c);
    !private_use && !c.is_control()
}

/// [main-thread] Keeps only the characters of `text` that are really text.
///
/// Returns an empty string when nothing survives, which the caller treats as "no event".
/// Filtering per character rather than rejecting the whole chunk matters for IME commits,
/// which legitimately arrive as one multi-character string and may carry a stray control
/// character alongside real ones.
#[must_use]
pub fn filter_text(text: &str) -> String {
    text.chars().filter(|c| is_printable(*c)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifiers_derive_egui_command_from_the_platform_key() {
        let ctrl = modifiers(Modifiers {
            ctrl: true,
            ..Modifiers::NONE
        });
        let meta = modifiers(Modifiers {
            meta: true,
            ..Modifiers::NONE
        });

        assert!(ctrl.ctrl);
        assert!(!ctrl.mac_cmd, "mac_cmd must never be set off the ⌘ key");
        if cfg!(target_os = "macos") {
            assert!(meta.command && meta.mac_cmd);
            assert!(!ctrl.command, "Ctrl is not the shortcut modifier on macOS");
        } else {
            assert!(ctrl.command);
            assert!(!meta.command, "the Windows key is not a shortcut modifier");
            assert!(!meta.mac_cmd);
        }
    }

    #[test]
    fn every_modifier_survives_the_trip() {
        let all = modifiers(Modifiers {
            shift: true,
            ctrl: true,
            alt: true,
            meta: true,
        });
        assert!(all.shift && all.ctrl && all.alt && all.command);
        assert_eq!(modifiers(Modifiers::NONE), egui::Modifiers::NONE);
    }

    #[test]
    fn the_named_buttons_map_directly_and_the_rest_are_dropped() {
        assert_eq!(
            button(PointerButton::Primary),
            Some(egui::PointerButton::Primary)
        );
        assert_eq!(
            button(PointerButton::Secondary),
            Some(egui::PointerButton::Secondary)
        );
        assert_eq!(
            button(PointerButton::Middle),
            Some(egui::PointerButton::Middle)
        );
        assert_eq!(
            button(PointerButton::Other(0)),
            Some(egui::PointerButton::Extra1)
        );
        assert_eq!(
            button(PointerButton::Other(1)),
            Some(egui::PointerButton::Extra2)
        );
        assert_eq!(
            button(PointerButton::Other(9)),
            None,
            "a button egui cannot name must be dropped, not reported as a left click"
        );
    }

    #[test]
    fn every_named_key_has_a_distinct_egui_key() {
        let named = [
            Key::Enter,
            Key::Escape,
            Key::Tab,
            Key::Backspace,
            Key::Delete,
            Key::Space,
            Key::ArrowUp,
            Key::ArrowDown,
            Key::ArrowLeft,
            Key::ArrowRight,
            Key::Home,
            Key::End,
            Key::PageUp,
            Key::PageDown,
        ];
        let mut mapped = Vec::new();
        for k in named {
            let e = key(k).unwrap_or_else(|| panic!("{k:?} must map"));
            assert!(!mapped.contains(&e), "{k:?} collides with an earlier key");
            mapped.push(e);
        }
        assert_eq!(mapped.len(), named.len());
    }

    #[test]
    fn an_unknown_key_is_dropped_rather_than_guessed_at() {
        assert_eq!(key(Key::Unknown(0)), None);
        assert_eq!(key(Key::Unknown(0x41)), None);
    }

    #[test]
    fn positions_narrow_to_f32_and_refuse_to_carry_nan_into_hit_testing() {
        assert_eq!(pos(LogicalPoint::new(12.5, -3.0)), egui::pos2(12.5, -3.0));
        assert_eq!(pos(LogicalPoint::new(f64::NAN, 4.0)), egui::Pos2::ZERO);
        assert_eq!(pos(LogicalPoint::new(1.0, f64::INFINITY)), egui::Pos2::ZERO);
    }

    #[test]
    fn scroll_deltas_pass_through_but_never_infinitely() {
        assert_eq!(scroll_delta(-3.0, 4.0), egui::vec2(-3.0, 4.0));
        assert_eq!(scroll_delta(f64::INFINITY, 4.0), egui::vec2(0.0, 4.0));
        assert_eq!(scroll_delta(1.0, f64::NAN), egui::vec2(1.0, 0.0));
    }

    #[test]
    fn text_filtering_keeps_characters_and_drops_control_codes() {
        assert_eq!(filter_text("hej"), "hej");
        assert_eq!(filter_text("naïve ✓"), "naïve ✓");
        assert_eq!(filter_text("\r"), "");
        assert_eq!(filter_text("\u{7f}"), "");
        assert_eq!(filter_text("\u{1b}"), "");
        assert_eq!(
            filter_text("a\rb"),
            "ab",
            "a mixed chunk keeps its real characters instead of being thrown away whole"
        );
        assert_eq!(
            filter_text("\u{f702}"),
            "",
            "the private-use area is where platforms hide their function keys"
        );
        assert_eq!(filter_text(""), "");
    }
}
