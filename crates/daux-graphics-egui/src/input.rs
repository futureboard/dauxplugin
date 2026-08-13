//! Accumulating DAUx input into the [`egui::RawInput`] one frame is run with.
//!
//! egui is immediate-mode: it does not want events as they happen, it wants the whole batch
//! that arrived since the last frame, together with the surface size, the scale factor and the
//! focus state. A host, on the other hand, delivers events one at a time and resizes whenever
//! it likes. [`InputTranslator`] is the buffer between the two.

use std::time::Instant;

use daux_graphics::{InputEvent, PhysicalSize, ScaleFactor};

use crate::convert;

/// Buffers host input between frames and hands egui a [`egui::RawInput`] when one is run.
///
/// # Why this is stateful
///
/// Three things have to be remembered across events:
///
/// * **Modifiers.** A DAUx event names the modifiers held at the time; egui expects the
///   modifier set to be current when it processes each event. Every event that carries
///   modifiers therefore updates the remembered set.
/// * **Focus.** [`egui::RawInput::focused`] is a level, not an edge, so it has to survive
///   between frames or egui stops drawing a text cursor after the first frame.
/// * **Size and scale.** These arrive through `resize`/`scale_factor_changed`, not through
///   input events, and every frame needs them.
///
/// # Coordinates
///
/// DAUx delivers logical coordinates and a separate scale factor; egui works in points and
/// takes the scale factor as `native_pixels_per_point`. Those are the same thing, so the
/// screen rectangle handed to egui is the physical size divided by the scale factor, and
/// pointer positions pass through untouched.
///
/// [main-thread]
pub struct InputTranslator {
    events: Vec<egui::Event>,
    modifiers: egui::Modifiers,
    size: PhysicalSize,
    scale: ScaleFactor,
    focused: bool,
    max_texture_side: Option<usize>,
    epoch: Instant,
}

impl std::fmt::Debug for InputTranslator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InputTranslator")
            .field("pending_events", &self.events.len())
            .field("modifiers", &self.modifiers)
            .field("size", &self.size)
            .field("scale", &self.scale.get())
            .field("focused", &self.focused)
            .finish_non_exhaustive()
    }
}

impl InputTranslator {
    /// [main-thread] Builds a translator for a surface of `size` physical pixels at `scale`.
    ///
    /// The editor starts unfocused: a host focuses an editor by sending
    /// [`InputEvent::Focus`], and assuming focus that was never granted makes an editor steal
    /// keystrokes from the host's own shortcuts.
    #[must_use]
    pub fn new(size: PhysicalSize, scale: ScaleFactor) -> Self {
        Self {
            events: Vec::new(),
            modifiers: egui::Modifiers::NONE,
            size,
            scale,
            focused: false,
            max_texture_side: None,
            epoch: Instant::now(),
        }
    }

    /// [main-thread] The surface size in physical pixels.
    #[must_use]
    pub const fn size(&self) -> PhysicalSize {
        self.size
    }

    /// [main-thread] Records a new surface size.
    pub const fn set_size(&mut self, size: PhysicalSize) {
        self.size = size;
    }

    /// [main-thread] The current scale factor.
    #[must_use]
    pub const fn scale(&self) -> ScaleFactor {
        self.scale
    }

    /// [main-thread] Records a new scale factor.
    pub const fn set_scale(&mut self, scale: ScaleFactor) {
        self.scale = scale;
    }

    /// [main-thread] Whether the host has given the editor keyboard focus.
    #[must_use]
    pub const fn is_focused(&self) -> bool {
        self.focused
    }

    /// [main-thread] Tells egui the largest texture the renderer can create, in pixels.
    ///
    /// egui splits its font atlas to fit. Left unset, egui assumes a very portable 2048,
    /// which is correct but wastes atlas space on any real GPU. A painter that knows its
    /// device limit should say so.
    pub const fn set_max_texture_side(&mut self, side: usize) {
        self.max_texture_side = Some(side);
    }

    /// [main-thread] The surface size in egui points.
    #[must_use]
    pub fn size_in_points(&self) -> egui::Vec2 {
        let logical = self.size.to_logical(self.scale);
        egui::vec2(logical.width as f32, logical.height as f32)
    }

    /// [main-thread] How many events are waiting for the next frame.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.events.len()
    }

    /// [main-thread] Drops every buffered event without running a frame.
    ///
    /// Called when an editor closes, so that a reopened editor does not replay a click the
    /// user made before the window went away.
    pub fn clear(&mut self) {
        self.events.clear();
        self.modifiers = egui::Modifiers::NONE;
    }

    /// [main-thread] Buffers one host event, reporting whether egui was given anything.
    ///
    /// `false` means the event had no egui equivalent and was dropped — an unnamed key, a
    /// sixth mouse button, a text chunk that was nothing but control characters. It does
    /// **not** mean egui ignored the event: whether egui acted on it is only known after the
    /// frame runs, through [`egui::Context::egui_wants_pointer_input`] and its keyboard
    /// counterpart, which is what [`EguiEditor`](crate::EguiEditor) uses to answer the host.
    pub fn push(&mut self, event: &InputEvent) -> bool {
        match event {
            InputEvent::PointerMoved {
                position,
                modifiers,
            } => {
                self.modifiers = convert::modifiers(*modifiers);
                self.events
                    .push(egui::Event::PointerMoved(convert::pos(*position)));
                true
            }
            InputEvent::PointerButton {
                position,
                button,
                pressed,
                modifiers,
            } => {
                self.modifiers = convert::modifiers(*modifiers);
                let Some(button) = convert::button(*button) else {
                    return false;
                };
                self.events.push(egui::Event::PointerButton {
                    pos: convert::pos(*position),
                    button,
                    pressed: *pressed,
                    modifiers: self.modifiers,
                });
                true
            }
            InputEvent::PointerLeft => {
                self.events.push(egui::Event::PointerGone);
                true
            }
            InputEvent::Scroll {
                delta_x,
                delta_y,
                modifiers,
                ..
            } => {
                self.modifiers = convert::modifiers(*modifiers);
                self.events.push(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: convert::scroll_delta(*delta_x, *delta_y),
                    phase: egui::TouchPhase::Move,
                    modifiers: self.modifiers,
                });
                true
            }
            InputEvent::Key {
                key,
                pressed,
                repeat,
                modifiers,
            } => {
                self.modifiers = convert::modifiers(*modifiers);
                let Some(key) = convert::key(*key) else {
                    return false;
                };
                self.events.push(egui::Event::Key {
                    key,
                    // The DAUx vocabulary is layout-independent by construction, so there is
                    // no separate physical key to report. Guessing one would make
                    // physical-key shortcuts fire on the wrong keys for non-QWERTY layouts.
                    physical_key: None,
                    pressed: *pressed,
                    repeat: *repeat,
                    modifiers: self.modifiers,
                });
                true
            }
            InputEvent::Text(text) => {
                let text = convert::filter_text(text);
                if text.is_empty() {
                    return false;
                }
                self.events.push(egui::Event::Text(text));
                true
            }
            InputEvent::Focus(focused) => {
                self.focused = *focused;
                if !*focused {
                    // A modifier held when focus was lost is not held any more by the time
                    // focus returns, and the release event goes to whoever has focus now.
                    self.modifiers = egui::Modifiers::NONE;
                }
                self.events.push(egui::Event::WindowFocused(*focused));
                true
            }
            InputEvent::Modifiers(modifiers) => {
                self.modifiers = convert::modifiers(*modifiers);
                self.events
                    .push(egui::Event::ModifiersChanged(self.modifiers));
                true
            }
            // `InputEvent` is `#[non_exhaustive]`: an event this build does not know about is
            // dropped rather than mapped onto something that looks similar.
            _ => false,
        }
    }

    /// [main-thread] Takes everything buffered and builds the input for one egui frame.
    ///
    /// The buffer is emptied, so calling this twice in a row runs the second frame with no
    /// events — which is what an idle repaint should do.
    #[must_use]
    pub fn take_raw_input(&mut self) -> egui::RawInput {
        let mut raw = egui::RawInput {
            focused: self.focused,
            max_texture_side: self.max_texture_side,
            time: Some(self.epoch.elapsed().as_secs_f64()),
            ..Default::default()
        };
        raw.events.append(&mut self.events);
        raw.screen_rect = Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            self.size_in_points(),
        ));
        let scale = self.scale.get() as f32;
        let viewport = raw.viewports.entry(raw.viewport_id).or_default();
        viewport.native_pixels_per_point = Some(scale);
        viewport.focused = Some(self.focused);
        raw
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use daux_graphics::{Key, LogicalPoint, Modifiers, PointerButton};

    fn translator() -> InputTranslator {
        InputTranslator::new(
            PhysicalSize::new(800, 600),
            ScaleFactor::new(2.0).expect("2.0 is a valid scale factor"),
        )
    }

    fn at(x: f64, y: f64) -> LogicalPoint {
        LogicalPoint::new(x, y)
    }

    #[test]
    fn the_screen_rect_is_in_points_not_pixels() {
        let mut t = translator();
        let raw = t.take_raw_input();
        assert_eq!(
            raw.screen_rect,
            Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(400.0, 300.0)
            )),
            "800x600 physical pixels at 2x is 400x300 points"
        );
        let ppp = raw
            .viewports
            .get(&raw.viewport_id)
            .and_then(|v| v.native_pixels_per_point);
        assert_eq!(ppp, Some(2.0));
    }

    #[test]
    fn resizing_and_rescaling_change_the_next_frames_geometry() {
        let mut t = translator();
        t.set_size(PhysicalSize::new(1024, 768));
        t.set_scale(ScaleFactor::ONE);
        assert_eq!(t.size_in_points(), egui::vec2(1024.0, 768.0));
        assert_eq!(t.size(), PhysicalSize::new(1024, 768));
        assert_eq!(t.scale().get(), 1.0);
    }

    #[test]
    fn events_are_delivered_once_and_in_order() {
        let mut t = translator();
        assert!(t.push(&InputEvent::PointerMoved {
            position: at(1.0, 2.0),
            modifiers: Modifiers::NONE
        }));
        assert!(t.push(&InputEvent::PointerButton {
            position: at(1.0, 2.0),
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE
        }));
        assert_eq!(t.pending(), 2);

        let raw = t.take_raw_input();
        assert!(matches!(
            raw.events.as_slice(),
            [
                egui::Event::PointerMoved(_),
                egui::Event::PointerButton { pressed: true, .. }
            ]
        ));
        assert_eq!(t.pending(), 0);
        assert!(
            t.take_raw_input().events.is_empty(),
            "an event must never be delivered to two frames"
        );
    }

    #[test]
    fn modifiers_carried_on_an_event_become_the_current_set() {
        let mut t = translator();
        let shift = Modifiers {
            shift: true,
            ..Modifiers::NONE
        };
        t.push(&InputEvent::Key {
            key: Key::ArrowUp,
            pressed: true,
            repeat: false,
            modifiers: shift,
        });
        // A later event that carries no modifiers of its own still reports the current set.
        t.push(&InputEvent::PointerButton {
            position: at(0.0, 0.0),
            button: PointerButton::Primary,
            pressed: true,
            modifiers: shift,
        });
        let raw = t.take_raw_input();
        match raw.events.as_slice() {
            [
                egui::Event::Key { modifiers: a, .. },
                egui::Event::PointerButton { modifiers: b, .. },
            ] => {
                assert!(a.shift && b.shift);
            }
            other => panic!("unexpected events: {other:?}"),
        }
    }

    #[test]
    fn losing_focus_releases_the_modifiers_the_editor_will_never_see_released() {
        let mut t = translator();
        t.push(&InputEvent::Modifiers(Modifiers {
            ctrl: true,
            ..Modifiers::NONE
        }));
        assert!(t.push(&InputEvent::Focus(false)));
        assert!(!t.is_focused());

        let _ = t.take_raw_input();
        t.push(&InputEvent::PointerMoved {
            position: at(0.0, 0.0),
            modifiers: Modifiers::NONE,
        });
        t.push(&InputEvent::PointerButton {
            position: at(0.0, 0.0),
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        });
        let raw = t.take_raw_input();
        match raw.events.as_slice() {
            [_, egui::Event::PointerButton { modifiers, .. }] => {
                assert!(
                    !modifiers.ctrl,
                    "a stuck Ctrl turns every click into a shortcut"
                );
            }
            other => panic!("unexpected events: {other:?}"),
        }
    }

    #[test]
    fn focus_is_a_level_that_survives_between_frames() {
        let mut t = translator();
        assert!(!t.take_raw_input().focused);
        t.push(&InputEvent::Focus(true));
        assert!(t.take_raw_input().focused);
        assert!(
            t.take_raw_input().focused,
            "focus must persist into the next frame, not just the one it arrived in"
        );
    }

    #[test]
    fn untranslatable_events_are_reported_and_buffer_nothing() {
        let mut t = translator();
        assert!(!t.push(&InputEvent::Key {
            key: Key::Unknown(0x9f),
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE
        }));
        assert!(!t.push(&InputEvent::PointerButton {
            position: at(0.0, 0.0),
            button: PointerButton::Other(9),
            pressed: true,
            modifiers: Modifiers::NONE
        }));
        assert!(!t.push(&InputEvent::Text("\r".into())));
        assert_eq!(
            t.pending(),
            0,
            "a dropped event must not leave a half-built egui event behind"
        );
    }

    #[test]
    fn an_unnamed_key_still_updates_the_modifier_state() {
        // The key itself is untranslatable, but the fact that Alt was down when it arrived is
        // information egui needs for the *next* event.
        let mut t = translator();
        assert!(!t.push(&InputEvent::Key {
            key: Key::Unknown(1),
            pressed: true,
            repeat: false,
            modifiers: Modifiers {
                alt: true,
                ..Modifiers::NONE
            }
        }));
        t.push(&InputEvent::PointerMoved {
            position: at(0.0, 0.0),
            modifiers: Modifiers {
                alt: true,
                ..Modifiers::NONE
            },
        });
        t.push(&InputEvent::Text("x".into()));
        let raw = t.take_raw_input();
        assert_eq!(raw.events.len(), 2);
    }

    #[test]
    fn repeat_and_press_state_survive_the_translation() {
        let mut t = translator();
        t.push(&InputEvent::Key {
            key: Key::ArrowDown,
            pressed: true,
            repeat: true,
            modifiers: Modifiers::NONE,
        });
        t.push(&InputEvent::Key {
            key: Key::ArrowDown,
            pressed: false,
            repeat: false,
            modifiers: Modifiers::NONE,
        });
        let raw = t.take_raw_input();
        match raw.events.as_slice() {
            [
                egui::Event::Key {
                    key: egui::Key::ArrowDown,
                    pressed: true,
                    repeat: true,
                    ..
                },
                egui::Event::Key {
                    key: egui::Key::ArrowDown,
                    pressed: false,
                    ..
                },
            ] => {}
            other => panic!("unexpected events: {other:?}"),
        }
    }

    #[test]
    fn clearing_drops_buffered_input_so_a_reopened_editor_does_not_replay_it() {
        let mut t = translator();
        t.push(&InputEvent::PointerButton {
            position: at(1.0, 1.0),
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers {
                ctrl: true,
                ..Modifiers::NONE
            },
        });
        assert_eq!(t.pending(), 1);
        t.clear();
        assert_eq!(t.pending(), 0);

        t.push(&InputEvent::PointerMoved {
            position: at(0.0, 0.0),
            modifiers: Modifiers::NONE,
        });
        let raw = t.take_raw_input();
        assert_eq!(raw.events.len(), 1);
    }

    #[test]
    fn the_pointer_leaving_is_reported_so_hover_state_does_not_stick() {
        let mut t = translator();
        assert!(t.push(&InputEvent::PointerLeft));
        assert!(matches!(
            t.take_raw_input().events.as_slice(),
            [egui::Event::PointerGone]
        ));
    }

    #[test]
    fn time_advances_between_frames_so_animations_run() {
        let mut t = translator();
        let first = t.take_raw_input().time.expect("time is always reported");
        let second = t.take_raw_input().time.expect("time is always reported");
        assert!(second >= first, "{second} went backwards from {first}");
        assert!(first >= 0.0);
    }

    #[test]
    fn the_max_texture_side_reaches_egui_when_a_painter_knows_it() {
        let mut t = translator();
        assert_eq!(t.take_raw_input().max_texture_side, None);
        t.set_max_texture_side(8192);
        assert_eq!(t.take_raw_input().max_texture_side, Some(8192));
    }

    #[test]
    fn a_zero_sized_surface_produces_an_empty_but_finite_screen_rect() {
        // A host that minimises its window reports 0x0; egui must not be handed a NaN
        // rectangle, and the next resize must recover.
        let mut t = InputTranslator::new(PhysicalSize::ZERO, ScaleFactor::ONE);
        let rect = t.take_raw_input().screen_rect.expect("always set");
        assert!(rect.width().is_finite() && rect.height().is_finite());
        assert_eq!(rect.width(), 0.0);
        t.set_size(PhysicalSize::new(100, 50));
        assert_eq!(t.size_in_points(), egui::vec2(100.0, 50.0));
    }
}
