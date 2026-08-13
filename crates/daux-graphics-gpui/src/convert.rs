//! Translating DAUx's framework-neutral input into GPUI's.

use daux_graphics::{InputEvent, Key, Modifiers, PointerButton, ScaleFactor};
use gpui::{Capslock, MouseButton, NavigationDirection, Point, ScrollDelta, px};
use gpui_embedded::HostEvent;

/// Maps DAUx modifiers onto GPUI's.
///
/// GPUI calls the Command/Windows/Super key `platform`, which is the same key DAUx's
/// [`Modifiers::meta`] names. `function` has no DAUx equivalent and stays false: no host
/// reports it through a plug-in editor API.
pub fn modifiers(m: Modifiers) -> gpui::Modifiers {
    gpui::Modifiers {
        control: m.ctrl,
        alt: m.alt,
        shift: m.shift,
        platform: m.meta,
        function: false,
    }
}

/// Maps a DAUx pointer button onto GPUI's.
///
/// Buttons past the middle one become navigation buttons, which is what a mouse's fourth and
/// fifth buttons almost always are; anything beyond that has no GPUI equivalent and is
/// reported as [`None`] rather than mislabelled as a left click.
pub fn button(b: PointerButton) -> Option<MouseButton> {
    Some(match b {
        PointerButton::Primary => MouseButton::Left,
        PointerButton::Secondary => MouseButton::Right,
        PointerButton::Middle => MouseButton::Middle,
        PointerButton::Other(0) => MouseButton::Navigate(NavigationDirection::Back),
        PointerButton::Other(1) => MouseButton::Navigate(NavigationDirection::Forward),
        // `PointerButton` is `#[non_exhaustive]`: a button this build does not know is
        // dropped, never reported as a click on some other button.
        _ => return None,
    })
}

/// The GPUI keystroke name for a DAUx key.
///
/// [`Key::Unknown`] has no name — a raw platform code cannot be translated into GPUI's
/// layout-independent vocabulary, and guessing would bind the wrong action.
pub fn key_name(k: Key) -> Option<&'static str> {
    Some(match k {
        Key::Enter => "enter",
        Key::Escape => "escape",
        Key::Tab => "tab",
        Key::Backspace => "backspace",
        Key::Delete => "delete",
        Key::Space => "space",
        Key::ArrowUp => "up",
        Key::ArrowDown => "down",
        Key::ArrowLeft => "left",
        Key::ArrowRight => "right",
        Key::Home => "home",
        Key::End => "end",
        Key::PageUp => "pageup",
        Key::PageDown => "pagedown",
        // `Key` is `#[non_exhaustive]`, and `Key::Unknown` carries a raw platform code that
        // has no place in GPUI's layout-independent vocabulary. Both go unnamed rather than
        // being guessed at, which would bind the wrong action.
        _ => return None,
    })
}

/// Builds the keystroke string GPUI parses, e.g. `"ctrl-shift-enter"`.
///
/// Modifier order is fixed and must match GPUI's parser, which is why this is one place
/// rather than a format string at each call site.
pub fn keystroke(k: Key, m: Modifiers) -> Option<String> {
    let name = key_name(k)?;
    let mut out = String::with_capacity(24);
    if m.ctrl {
        out.push_str("ctrl-");
    }
    if m.alt {
        out.push_str("alt-");
    }
    if m.shift {
        out.push_str("shift-");
    }
    if m.meta {
        out.push_str("cmd-");
    }
    out.push_str(name);
    Some(out)
}

/// Converts a logical point into GPUI's pixel space.
fn point(x: f64, y: f64) -> Point<gpui::Pixels> {
    gpui::point(px(x as f32), px(y as f32))
}

/// Translates one DAUx input event into the GPUI events it corresponds to.
///
/// Returns an empty vector for anything GPUI has no equivalent for, and up to two events for
/// the cases where DAUx carries in one event what GPUI splits in two: a DAUx event names the
/// modifiers held at the time, and GPUI expects modifier state to arrive as its own event
/// before the one that depends on it.
///
/// [`InputEvent::Text`] produces nothing: GPUI reconstructs text from keystrokes, and feeding
/// it host-composed text as well would double every character.
pub fn to_host_events(event: &InputEvent) -> Vec<HostEvent> {
    match event {
        InputEvent::PointerMoved { position, .. } => vec![HostEvent::MouseMoved {
            position: point(position.x, position.y),
        }],
        InputEvent::PointerButton {
            position,
            button: b,
            pressed,
            ..
        } => button(*b).map_or_else(Vec::new, |button| {
            let position = point(position.x, position.y);
            vec![if *pressed {
                HostEvent::MouseDown { button, position }
            } else {
                HostEvent::MouseUp { button, position }
            }]
        }),
        InputEvent::PointerLeft => vec![HostEvent::MouseExited {
            // GPUI wants a last-known position; the surface origin is the safe stand-in,
            // since the pointer is by definition no longer over anything.
            position: point(0.0, 0.0),
        }],
        InputEvent::Scroll {
            position,
            delta_x,
            delta_y,
            ..
        } => vec![HostEvent::Scroll {
            position: point(position.x, position.y),
            delta: ScrollDelta::Pixels(point(*delta_x, *delta_y)),
            touch_phase: gpui::TouchPhase::Moved,
        }],
        InputEvent::Key {
            key,
            pressed,
            repeat,
            modifiers: m,
        } => {
            let Some(text) = keystroke(*key, *m) else {
                return Vec::new();
            };
            let parsed = if *pressed {
                HostEvent::key_down(&text)
            } else {
                HostEvent::key_up(&text)
            };
            match parsed {
                Ok(HostEvent::KeyDown { keystroke, .. }) => vec![HostEvent::KeyDown {
                    keystroke,
                    is_held: *repeat,
                }],
                Ok(other) => vec![other],
                // An unparseable keystroke is dropped rather than guessed at.
                Err(_) => Vec::new(),
            }
        }
        InputEvent::Modifiers(m) => vec![HostEvent::ModifiersChanged {
            modifiers: modifiers(*m),
            capslock: Capslock { on: false },
        }],
        // GPUI derives text from keystrokes; forwarding both would type everything twice.
        InputEvent::Text(_) => Vec::new(),
        // GPUI tracks focus through its own window state, not through an input event.
        InputEvent::Focus(_) => Vec::new(),
        _ => Vec::new(),
    }
}

/// GPUI takes the scale factor as an `f32`.
pub fn scale(s: ScaleFactor) -> f32 {
    s.get() as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use daux_graphics::LogicalPoint;

    fn mods() -> Modifiers {
        Modifiers {
            ctrl: true,
            shift: true,
            ..Modifiers::NONE
        }
    }

    #[test]
    fn modifiers_map_meta_onto_platform() {
        let m = modifiers(Modifiers {
            meta: true,
            ..Modifiers::NONE
        });
        assert!(m.platform);
        assert!(!m.control);
        assert!(!m.function);

        let all = modifiers(Modifiers {
            shift: true,
            ctrl: true,
            alt: true,
            meta: true,
        });
        assert!(all.shift && all.control && all.alt && all.platform);
    }

    #[test]
    fn the_three_named_buttons_map_directly() {
        assert_eq!(button(PointerButton::Primary), Some(MouseButton::Left));
        assert_eq!(button(PointerButton::Secondary), Some(MouseButton::Right));
        assert_eq!(button(PointerButton::Middle), Some(MouseButton::Middle));
    }

    #[test]
    fn extra_buttons_become_navigation_then_nothing() {
        assert_eq!(
            button(PointerButton::Other(0)),
            Some(MouseButton::Navigate(NavigationDirection::Back))
        );
        assert_eq!(
            button(PointerButton::Other(1)),
            Some(MouseButton::Navigate(NavigationDirection::Forward))
        );
        // Better to drop a button GPUI cannot express than to report it as a left click.
        assert_eq!(button(PointerButton::Other(7)), None);
    }

    #[test]
    fn keystrokes_are_built_in_gpuis_modifier_order() {
        assert_eq!(keystroke(Key::Enter, Modifiers::NONE).unwrap(), "enter");
        assert_eq!(
            keystroke(Key::Enter, mods()).unwrap(),
            "ctrl-shift-enter",
            "ctrl before shift, matching GPUI's parser"
        );
        assert_eq!(
            keystroke(
                Key::ArrowUp,
                Modifiers {
                    shift: true,
                    ctrl: true,
                    alt: true,
                    meta: true,
                }
            )
            .unwrap(),
            "ctrl-alt-shift-cmd-up"
        );
    }

    #[test]
    fn every_named_key_produces_a_keystroke_gpui_can_parse() {
        for key in [
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
        ] {
            let text = keystroke(key, Modifiers::NONE)
                .unwrap_or_else(|| panic!("{key:?} must have a name"));
            assert!(
                HostEvent::key_down(&text).is_ok(),
                "GPUI must parse `{text}` for {key:?}"
            );
        }
    }

    #[test]
    fn an_unknown_key_is_dropped_rather_than_guessed_at() {
        assert_eq!(key_name(Key::Unknown(42)), None);
        assert_eq!(keystroke(Key::Unknown(42), Modifiers::NONE), None);
        let event = InputEvent::Key {
            key: Key::Unknown(42),
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        assert!(to_host_events(&event).is_empty());
    }

    #[test]
    fn pointer_events_carry_their_position() {
        let at = LogicalPoint::new(12.0, 34.0);
        let moved = to_host_events(&InputEvent::PointerMoved {
            position: at,
            modifiers: Modifiers::NONE,
        });
        assert!(matches!(moved.as_slice(), [HostEvent::MouseMoved { .. }]));

        let down = to_host_events(&InputEvent::PointerButton {
            position: at,
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        });
        assert!(matches!(
            down.as_slice(),
            [HostEvent::MouseDown {
                button: MouseButton::Left,
                ..
            }]
        ));

        let up = to_host_events(&InputEvent::PointerButton {
            position: at,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        });
        assert!(matches!(up.as_slice(), [HostEvent::MouseUp { .. }]));
    }

    #[test]
    fn repeat_survives_the_translation() {
        let held = to_host_events(&InputEvent::Key {
            key: Key::ArrowDown,
            pressed: true,
            repeat: true,
            modifiers: Modifiers::NONE,
        });
        match held.as_slice() {
            [HostEvent::KeyDown { is_held, .. }] => assert!(*is_held),
            other => panic!("expected one KeyDown, got {other:?}"),
        }
    }

    #[test]
    fn text_and_focus_produce_nothing() {
        // GPUI composes text from keystrokes; forwarding both would double every character.
        assert!(to_host_events(&InputEvent::Text("hello".into())).is_empty());
        assert!(to_host_events(&InputEvent::Focus(true)).is_empty());
        assert!(to_host_events(&InputEvent::Focus(false)).is_empty());
    }

    #[test]
    fn scroll_carries_pixel_deltas() {
        let events = to_host_events(&InputEvent::Scroll {
            position: LogicalPoint::new(1.0, 2.0),
            delta_x: -3.0,
            delta_y: 4.0,
            modifiers: Modifiers::NONE,
        });
        match events.as_slice() {
            [HostEvent::Scroll {
                delta: ScrollDelta::Pixels(d),
                ..
            }] => {
                assert!((f32::from(d.x) - -3.0).abs() < 1e-6);
                assert!((f32::from(d.y) - 4.0).abs() < 1e-6);
            }
            other => panic!("expected one pixel Scroll, got {other:?}"),
        }
    }

    #[test]
    fn scale_narrows_to_f32() {
        assert!((scale(ScaleFactor::ONE) - 1.0).abs() < f32::EPSILON);
        let two = ScaleFactor::new(2.0).expect("in range");
        assert!((scale(two) - 2.0).abs() < f32::EPSILON);
    }
}
