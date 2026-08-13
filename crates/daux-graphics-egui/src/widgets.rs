//! Parameter-bound egui widgets.
//!
//! Every widget here drives a [`ParamBinding`], never a parameter directly. That is the whole
//! point: `ParamBinding` already implements the gesture state machine — one `begin` per drag,
//! one `end`, the host told the value the parameter *actually took* after clamping, text entry
//! bracketed in its own gesture, and an open gesture closed if the editor is torn down
//! mid-drag. Reimplementing any of that per widget is how automation lanes end up latched in
//! write mode.
//!
//! # Conventions these widgets share
//!
//! | Gesture | Effect |
//! |---|---|
//! | Drag | Changes the value; one host gesture per drag |
//! | Shift + drag | Fine adjustment ([`Modifiers::is_fine_adjust`](daux_graphics::Modifiers::is_fine_adjust)) |
//! | Alt + click | Restores the default ([`Modifiers::is_reset`](daux_graphics::Modifiers::is_reset)) |
//! | Double click | Restores the default |
//!
//! Reset is bracketed in its own gesture too, so a host records one automation point for it
//! rather than none.
//!
//! # Example
//!
//! ```
//! use daux_graphics::ParamBinding;
//! use daux_graphics_egui::{ParamKnob, ParamSlider};
//! use daux_parameter::{FloatParam, ParamId, ParamRange};
//!
//! # fn draw(ui: &mut egui::Ui, gain: &FloatParam) {
//! // `host` is `HostServices::params()`, or `None` in a preview harness.
//! let binding = ParamBinding::new(gain, None);
//! ui.add(ParamKnob::new(&binding).diameter(48.0));
//! ui.add(ParamSlider::new(&binding));
//! # }
//! ```

use daux_graphics::ParamBinding;
use egui::{Response, Sense, Ui, Vec2, Widget, pos2, vec2};

/// How much slower a shift-held drag moves. A quarter is the conventional feel.
const FINE_ADJUST: f64 = 0.25;

/// Logical pixels of vertical drag that sweep a knob through its whole range.
const DEFAULT_DRAG_LENGTH: f32 = 200.0;

/// Where a knob's zero and full positions sit, as an angle in degrees measured clockwise from
/// the `+x` axis in screen space (`y` grows downwards). 135° is the lower left; adding 270°
/// arrives at the lower right, having passed through straight up.
const KNOB_START_DEGREES: f32 = 135.0;
/// How far a knob sweeps, in degrees. 270° leaves a readable gap at the bottom.
const KNOB_SWEEP_DEGREES: f32 = 270.0;
/// Segments used to draw the knob's value arc. Enough to look round at any sane size.
const KNOB_ARC_SEGMENTS: usize = 48;

/// Restores the parameter's default inside a gesture of its own.
///
/// A reset that is not bracketed writes nothing to a host's automation lane, because the host
/// only records between `begin` and `end`. If a drag is already open — the user alt-clicked
/// without letting go — the existing gesture is left alone rather than being closed early.
fn reset_in_gesture(binding: &ParamBinding<'_>) {
    let already_open = binding.is_gesturing();
    if !already_open {
        binding.begin_gesture();
    }
    binding.reset();
    if !already_open {
        binding.end_gesture();
    }
}

/// Sets a normalised value inside a gesture of its own, for a click rather than a drag.
fn set_in_gesture(binding: &ParamBinding<'_>, normalized: f64) {
    let already_open = binding.is_gesturing();
    if !already_open {
        binding.begin_gesture();
    }
    binding.set_normalized(normalized);
    if !already_open {
        binding.end_gesture();
    }
}

/// Handles the reset gestures every widget shares, reporting whether the value moved.
fn handle_reset(binding: &ParamBinding<'_>, ui: &Ui, response: &Response) -> bool {
    let alt = ui.input(|i| i.modifiers.alt);
    if response.double_clicked() || (response.clicked() && alt) {
        reset_in_gesture(binding);
        return true;
    }
    false
}

/// A rotary control bound to a parameter.
///
/// Dragging **vertically** changes the value: up increases, down decreases, and a drag of
/// [`drag_length`](Self::drag_length) logical pixels covers the whole range. Vertical-only is
/// deliberate — a knob that also responds to horizontal motion feels unpredictable when the
/// user's hand drifts, and diagonal drags produce values nobody aimed for.
///
/// [main-thread]
pub struct ParamKnob<'a> {
    binding: &'a ParamBinding<'a>,
    diameter: f32,
    drag_length: f32,
    hover_text: bool,
}

impl<'a> ParamKnob<'a> {
    /// [main-thread] Binds a knob to a parameter.
    #[must_use]
    pub fn new(binding: &'a ParamBinding<'a>) -> Self {
        Self {
            binding,
            diameter: 40.0,
            drag_length: DEFAULT_DRAG_LENGTH,
            hover_text: true,
        }
    }

    /// [main-thread] Sets the knob's diameter in logical pixels.
    ///
    /// Clamped to a floor: a knob smaller than a few pixels cannot be aimed at, and one with a
    /// non-finite size would produce a `NaN` rectangle that swallows every later hit test.
    #[must_use]
    pub fn diameter(mut self, diameter: f32) -> Self {
        self.diameter = if diameter.is_finite() {
            diameter.max(4.0)
        } else {
            4.0
        };
        self
    }

    /// [main-thread] Sets how many logical pixels of vertical drag sweep the whole range.
    ///
    /// Larger is finer. Clamped to a floor so that a zero can never divide by itself.
    #[must_use]
    pub fn drag_length(mut self, pixels: f32) -> Self {
        self.drag_length = if pixels.is_finite() {
            pixels.max(1.0)
        } else {
            DEFAULT_DRAG_LENGTH
        };
        self
    }

    /// [main-thread] Whether hovering shows the formatted value. On by default.
    #[must_use]
    pub const fn hover_text(mut self, show: bool) -> Self {
        self.hover_text = show;
        self
    }
}

impl Widget for ParamKnob<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let (rect, mut response) =
            ui.allocate_exact_size(Vec2::splat(self.diameter), Sense::click_and_drag());
        let mut changed = false;

        if response.drag_started() {
            self.binding.begin_gesture();
        }
        if response.dragged() {
            let delta = response.drag_delta();
            let fine = ui.input(|i| i.modifiers.shift);
            let speed = if fine { FINE_ADJUST } else { 1.0 };
            // Screen `y` grows downwards, so dragging up is a negative delta and must raise
            // the value.
            let step = -f64::from(delta.y) / f64::from(self.drag_length) * speed;
            if step != 0.0 {
                self.binding.nudge_normalized(step);
                changed = true;
            }
        }
        if response.drag_stopped() {
            self.binding.end_gesture();
        }
        changed |= handle_reset(self.binding, ui, &response);

        if changed {
            response.mark_changed();
        }

        if ui.is_rect_visible(rect) {
            let visuals = *ui.style().interact(&response);
            let center = rect.center();
            let radius = rect.width().min(rect.height()) * 0.5 - visuals.bg_stroke.width;
            let t = self.binding.normalized().clamp(0.0, 1.0) as f32;

            ui.painter()
                .circle(center, radius, visuals.bg_fill, visuals.bg_stroke);

            // The value arc, drawn as short segments: enough for a smooth curve without
            // depending on a path primitive whose spelling changes between egui versions.
            let track = radius * 0.82;
            let steps = (KNOB_ARC_SEGMENTS as f32 * t).ceil() as usize;
            let mut previous = None;
            for i in 0..=steps {
                let along = if steps == 0 {
                    0.0
                } else {
                    t * (i as f32) / (steps as f32)
                };
                let angle = (KNOB_START_DEGREES + KNOB_SWEEP_DEGREES * along).to_radians();
                let point = center + vec2(angle.cos(), angle.sin()) * track;
                if let Some(from) = previous {
                    ui.painter().line_segment([from, point], visuals.fg_stroke);
                }
                previous = Some(point);
            }

            // The indicator, from the hub to the rim.
            let angle = (KNOB_START_DEGREES + KNOB_SWEEP_DEGREES * t).to_radians();
            let direction = vec2(angle.cos(), angle.sin());
            ui.painter().line_segment(
                [
                    center + direction * radius * 0.35,
                    center + direction * track,
                ],
                visuals.fg_stroke,
            );
        }

        if self.hover_text {
            response = response.on_hover_text(self.binding.display());
        }
        response
    }
}

/// A horizontal bar bound to a parameter.
///
/// Clicking or dragging positions the value **absolutely**: the value follows the pointer, as
/// on every fader in every DAW. Shift is a fine adjustment relative to where the drag started,
/// so a precise change does not require hitting a single pixel.
///
/// [main-thread]
pub struct ParamSlider<'a> {
    binding: &'a ParamBinding<'a>,
    size: Vec2,
    hover_text: bool,
}

impl<'a> ParamSlider<'a> {
    /// [main-thread] Binds a slider to a parameter.
    #[must_use]
    pub fn new(binding: &'a ParamBinding<'a>) -> Self {
        Self {
            binding,
            size: vec2(140.0, 16.0),
            hover_text: true,
        }
    }

    /// [main-thread] Sets the slider's size in logical pixels.
    ///
    /// Clamped to a floor in both axes so the track always has a width to divide by.
    #[must_use]
    pub fn size(mut self, size: Vec2) -> Self {
        let clamp = |v: f32| if v.is_finite() { v.max(4.0) } else { 4.0 };
        self.size = vec2(clamp(size.x), clamp(size.y));
        self
    }

    /// [main-thread] Whether hovering shows the formatted value. On by default.
    #[must_use]
    pub const fn hover_text(mut self, show: bool) -> Self {
        self.hover_text = show;
        self
    }
}

impl Widget for ParamSlider<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let (rect, mut response) = ui.allocate_exact_size(self.size, Sense::click_and_drag());
        let mut changed = false;
        let fine = ui.input(|i| i.modifiers.shift);

        if response.drag_started() {
            self.binding.begin_gesture();
        }
        if response.dragged() {
            if fine {
                // Relative while shift is held: absolute positioning has no fine mode, since
                // the value is a function of the pointer's position and nothing else.
                let step =
                    f64::from(response.drag_delta().x) / f64::from(rect.width()) * FINE_ADJUST;
                if step != 0.0 {
                    self.binding.nudge_normalized(step);
                    changed = true;
                }
            } else if let Some(pointer) = response.interact_pointer_pos() {
                let t = f64::from((pointer.x - rect.left()) / rect.width());
                self.binding.set_normalized(t.clamp(0.0, 1.0));
                changed = true;
            }
        }
        if response.drag_stopped() {
            self.binding.end_gesture();
        }

        let alt = ui.input(|i| i.modifiers.alt);
        if response.clicked() && !alt {
            // A click without a drag still positions the fader, and still has to be bracketed
            // or the host records nothing at all for it.
            if let Some(pointer) = response.interact_pointer_pos() {
                let t = f64::from((pointer.x - rect.left()) / rect.width());
                set_in_gesture(self.binding, t.clamp(0.0, 1.0));
                changed = true;
            }
        }
        changed |= handle_reset(self.binding, ui, &response);

        if changed {
            response.mark_changed();
        }

        if ui.is_rect_visible(rect) {
            let visuals = *ui.style().interact(&response);
            let t = self.binding.normalized().clamp(0.0, 1.0) as f32;
            ui.painter().rect(
                rect,
                visuals.corner_radius,
                visuals.bg_fill,
                visuals.bg_stroke,
                egui::StrokeKind::Inside,
            );
            let filled = egui::Rect::from_min_max(
                rect.min,
                pos2(rect.left() + rect.width() * t, rect.bottom()),
            );
            ui.painter()
                .rect_filled(filled, visuals.corner_radius, visuals.fg_stroke.color);
        }

        if self.hover_text {
            response = response.on_hover_text(self.binding.display());
        }
        response
    }
}

/// A checkbox bound to a parameter.
///
/// Drives the normalised value to `1.0` or `0.0`, which for a
/// [`BoolParam`](daux_parameter::BoolParam) is exactly on and off, and for any other parameter
/// is its maximum and minimum. Each click is one complete gesture, so a host records one
/// automation point per toggle.
///
/// [main-thread]
pub struct ParamToggle<'a> {
    binding: &'a ParamBinding<'a>,
    text: Option<&'a str>,
}

impl<'a> ParamToggle<'a> {
    /// [main-thread] Binds a toggle to a parameter, labelled with the parameter's own name.
    #[must_use]
    pub const fn new(binding: &'a ParamBinding<'a>) -> Self {
        Self {
            binding,
            text: None,
        }
    }

    /// [main-thread] Overrides the label.
    #[must_use]
    pub const fn text(mut self, text: &'a str) -> Self {
        self.text = Some(text);
        self
    }
}

impl Widget for ParamToggle<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let mut on = self.binding.normalized() >= 0.5;
        let label = match self.text {
            Some(text) => text.to_owned(),
            None => self.binding.param().info().name,
        };
        let mut response = ui.add(egui::Checkbox::new(&mut on, label));
        if response.changed() {
            set_in_gesture(self.binding, if on { 1.0 } else { 0.0 });
        }
        if handle_reset(self.binding, ui, &response) {
            response.mark_changed();
        }
        response
    }
}

/// A single-line text field bound to a parameter.
///
/// # Behaviour
///
/// * Not focused, it shows the parameter's own formatted value, so it tracks automation.
/// * Focusing it selects the whole value, so the first keystroke replaces rather than appends —
///   without that, typing `-6` into a field showing `0.00 dB` produces `0.00 dB-6`.
/// * Nothing is parsed until focus leaves, by Enter, Tab or a click elsewhere.
/// * Text the parameter rejects leaves the value untouched and the field snaps back to the
///   current value, so a typo cannot silently zero a control.
///
/// The in-progress text lives in egui's own temporary memory, keyed by the parameter id, and
/// is removed on commit. The widget therefore has no state of its own and can be created fresh
/// every frame like any other egui widget.
///
/// [main-thread]
pub struct ParamValueEdit<'a> {
    binding: &'a ParamBinding<'a>,
    width: f32,
}

impl<'a> ParamValueEdit<'a> {
    /// [main-thread] Binds a text field to a parameter.
    #[must_use]
    pub const fn new(binding: &'a ParamBinding<'a>) -> Self {
        Self {
            binding,
            width: 72.0,
        }
    }

    /// [main-thread] Sets the field's width in logical pixels.
    #[must_use]
    pub fn width(mut self, width: f32) -> Self {
        self.width = if width.is_finite() {
            width.max(8.0)
        } else {
            8.0
        };
        self
    }
}

impl Widget for ParamValueEdit<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let id = ui.id().with(("daux-param-value", self.binding.id().0));
        // The stored buffer is the single source of truth while an edit is in progress: focus
        // has usually already moved on by the frame the edit is committed, so asking egui
        // whether the field is focused is not enough to find the text the user typed.
        let stored = ui.data(|d| d.get_temp::<String>(id));
        let focused = ui.memory(|m| m.has_focus(id));
        let entering = focused && stored.is_none();
        let mut buffer = stored.unwrap_or_else(|| self.binding.display());

        let output = egui::TextEdit::singleline(&mut buffer)
            .id(id)
            .desired_width(self.width)
            .show(ui);
        let mut response = output.response.response;

        if entering {
            // This is the first frame with focus. Anything typed in it went to whatever cursor
            // position the field happened to have, so start again from the parameter's own
            // text and select all of it; the next keystroke replaces the value.
            buffer = self.binding.display();
            let mut state = output.state;
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::select_all(&output.galley)));
            state.store(ui.ctx(), id);
        }

        if response.lost_focus() {
            if self.binding.apply_text(&buffer) {
                response.mark_changed();
            }
            // Dropping the buffer is what makes the field snap back to the parameter's own
            // text — which, for a rejected entry, is the only way the user can tell it was
            // rejected.
            ui.data_mut(|d| d.remove::<String>(id));
        } else if focused {
            ui.data_mut(|d| d.insert_temp(id, buffer));
        }
        response
    }
}

/// [main-thread] A knob bound to `binding`, added to `ui`.
pub fn param_knob(ui: &mut Ui, binding: &ParamBinding<'_>) -> Response {
    ui.add(ParamKnob::new(binding))
}

/// [main-thread] A slider bound to `binding`, added to `ui`.
pub fn param_slider(ui: &mut Ui, binding: &ParamBinding<'_>) -> Response {
    ui.add(ParamSlider::new(binding))
}

/// [main-thread] A toggle bound to `binding`, added to `ui`.
pub fn param_toggle(ui: &mut Ui, binding: &ParamBinding<'_>) -> Response {
    ui.add(ParamToggle::new(binding))
}

/// [main-thread] A text field bound to `binding`, added to `ui`.
pub fn param_value_edit(ui: &mut Ui, binding: &ParamBinding<'_>) -> Response {
    ui.add(ParamValueEdit::new(binding))
}

#[cfg(test)]
mod tests {
    use super::*;
    use daux_host_services::{HostParams, ParamId, RescanFlags};
    use daux_parameter::{BoolParam, FloatParam, Param, ParamRange};
    use std::sync::Mutex;

    /// Records the gesture and value traffic a host would have seen.
    #[derive(Default)]
    struct SpyHost {
        calls: Mutex<Vec<String>>,
    }

    impl SpyHost {
        fn push(&self, call: String) {
            self.calls
                .lock()
                .expect("no test panics while holding this")
                .push(call);
        }

        fn calls(&self) -> Vec<String> {
            self.calls
                .lock()
                .expect("no test panics while holding this")
                .clone()
        }

        fn count(&self, prefix: &str) -> usize {
            self.calls()
                .iter()
                .filter(|c| c.starts_with(prefix))
                .count()
        }
    }

    impl HostParams for SpyHost {
        fn gesture_begin(&self, id: ParamId) {
            self.push(format!("begin {}", id.0));
        }
        fn gesture_end(&self, id: ParamId) {
            self.push(format!("end {}", id.0));
        }
        fn changed(&self, id: ParamId, plain: f64) {
            self.push(format!("changed {} {plain}", id.0));
        }
        fn rescan(&self, _flags: RescanFlags) {
            self.push("rescan".to_owned());
        }
    }

    fn gain() -> FloatParam {
        FloatParam::new(
            ParamId(7),
            "Gain",
            0.0,
            ParamRange::Linear {
                min: -60.0,
                max: 12.0,
            },
        )
        .with_unit("dB")
    }

    /// Positions a parameter without going through a binding.
    ///
    /// Setting up through the binding would send the host a `changed` call the test never
    /// meant to make, and then every assertion about what the host heard is about the setup
    /// rather than about the widget.
    fn preset(param: &FloatParam, normalized: f64) {
        param.set_normalized(normalized);
    }

    /// Drives an egui context through a sequence of frames, each with its own raw input.
    ///
    /// This is a real egui run: layout, hit testing, interaction state and all. A widget's
    /// gesture bookkeeping is only meaningful against egui's actual drag detection, so the
    /// tests below simulate pointer traffic rather than calling the widget's helpers.
    struct Harness {
        ctx: egui::Context,
        pointer: egui::Pos2,
        modifiers: egui::Modifiers,
        time: f64,
    }

    impl Harness {
        fn new() -> Self {
            Self {
                ctx: egui::Context::default(),
                pointer: egui::Pos2::ZERO,
                modifiers: egui::Modifiers::NONE,
                time: 0.0,
            }
        }

        fn raw(&mut self, events: Vec<egui::Event>) -> egui::RawInput {
            self.time += 1.0 / 60.0;
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 300.0),
                )),
                time: Some(self.time),
                focused: true,
                events,
                ..Default::default()
            }
        }

        fn frame(&mut self, events: Vec<egui::Event>, ui: impl FnMut(&mut egui::Ui)) {
            let raw = self.raw(events);
            let mut ui = ui;
            self.ctx
                .run_ui(raw, |root| ui(root))
                .drop_without_applying_deltas();
        }

        fn move_to(&mut self, x: f32, y: f32) -> egui::Event {
            self.pointer = pos2(x, y);
            egui::Event::PointerMoved(self.pointer)
        }

        fn press(&mut self) -> egui::Event {
            egui::Event::PointerButton {
                pos: self.pointer,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: self.modifiers,
            }
        }

        fn release(&mut self) -> egui::Event {
            egui::Event::PointerButton {
                pos: self.pointer,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: self.modifiers,
            }
        }
    }

    #[test]
    fn dragging_a_knob_produces_exactly_one_gesture() {
        let param = gain();
        preset(&param, 0.5);
        let host = SpyHost::default();
        let binding = ParamBinding::new(&param, Some(&host));
        let mut h = Harness::new();

        // Frame 1: lay the knob out so egui knows where it is.
        let hover = h.move_to(30.0, 30.0);
        h.frame(vec![hover], |ui| {
            ui.add(ParamKnob::new(&binding).diameter(60.0));
        });
        // Frame 2: press.
        let press = h.press();
        h.frame(vec![press], |ui| {
            ui.add(ParamKnob::new(&binding).diameter(60.0));
        });
        // Frames 3-5: drag upwards in steps.
        for y in [20.0, 10.0, 0.0] {
            let moved = h.move_to(30.0, y);
            h.frame(vec![moved], |ui| {
                ui.add(ParamKnob::new(&binding).diameter(60.0));
            });
        }
        // Frame 6: release.
        let release = h.release();
        h.frame(vec![release], |ui| {
            ui.add(ParamKnob::new(&binding).diameter(60.0));
        });

        let calls = host.calls();
        assert_eq!(
            host.count("begin"),
            1,
            "one drag must open exactly one gesture: {calls:?}"
        );
        assert_eq!(
            host.count("end"),
            1,
            "one drag must close exactly one gesture: {calls:?}"
        );
        assert_eq!(calls.first().map(String::as_str), Some("begin 7"));
        assert_eq!(calls.last().map(String::as_str), Some("end 7"));
        assert!(
            host.count("changed") > 0,
            "the drag produced no value changes at all: {calls:?}"
        );
        assert!(
            binding.normalized() > 0.5,
            "dragging upwards must raise the value, got {}",
            binding.normalized()
        );
        std::mem::forget(binding); // do not let Drop append another `end`
    }

    #[test]
    fn a_knob_drag_that_is_abandoned_mid_flight_still_closes_its_gesture() {
        // The editor is destroyed while the pointer is still down — a window closed by the
        // host, a crashed UI thread, a user hitting the DAW's "close all editors". If the
        // gesture stayed open the host would keep recording automation for ever.
        let param = gain();
        let host = SpyHost::default();
        {
            let binding = ParamBinding::new(&param, Some(&host));
            let mut h = Harness::new();
            let hover = h.move_to(30.0, 30.0);
            h.frame(vec![hover], |ui| {
                ui.add(ParamKnob::new(&binding).diameter(60.0));
            });
            let press = h.press();
            h.frame(vec![press], |ui| {
                ui.add(ParamKnob::new(&binding).diameter(60.0));
            });
            let moved = h.move_to(30.0, 5.0);
            h.frame(vec![moved], |ui| {
                ui.add(ParamKnob::new(&binding).diameter(60.0));
            });
            assert!(binding.is_gesturing(), "the drag must really be open here");
        }
        assert_eq!(host.count("begin"), 1);
        assert_eq!(host.count("end"), 1, "{:?}", host.calls());
        assert_eq!(host.calls().last().map(String::as_str), Some("end 7"));
    }

    #[test]
    fn shift_makes_a_knob_drag_finer_by_exactly_the_documented_factor() {
        let param = gain();
        preset(&param, 0.5);
        let coarse = ParamBinding::new(&param, None);

        let mut h = Harness::new();
        let drag = |h: &mut Harness, binding: &ParamBinding<'_>, modifiers: egui::Modifiers| {
            h.modifiers = modifiers;
            let hover = h.move_to(30.0, 40.0);
            h.frame(
                vec![hover, egui::Event::ModifiersChanged(modifiers)],
                |ui| {
                    ui.add(ParamKnob::new(binding).diameter(60.0).drag_length(100.0));
                },
            );
            let press = h.press();
            h.frame(vec![press], |ui| {
                ui.add(ParamKnob::new(binding).diameter(60.0).drag_length(100.0));
            });
            let moved = h.move_to(30.0, 20.0);
            h.frame(vec![moved], |ui| {
                ui.add(ParamKnob::new(binding).diameter(60.0).drag_length(100.0));
            });
            let release = h.release();
            h.frame(vec![release], |ui| {
                ui.add(ParamKnob::new(binding).diameter(60.0).drag_length(100.0));
            });
        };

        drag(&mut h, &coarse, egui::Modifiers::NONE);
        let coarse_travel = coarse.normalized() - 0.5;
        assert!(coarse_travel > 0.0, "the coarse drag did nothing");

        let param2 = gain();
        preset(&param2, 0.5);
        let fine = ParamBinding::new(&param2, None);
        let mut h2 = Harness::new();
        drag(&mut h2, &fine, egui::Modifiers::SHIFT);
        let fine_travel = fine.normalized() - 0.5;

        assert!(fine_travel > 0.0, "the fine drag did nothing");
        assert!(
            (fine_travel - coarse_travel * FINE_ADJUST).abs() < 1e-6,
            "fine travel {fine_travel} should be {FINE_ADJUST} of coarse travel {coarse_travel}"
        );
    }

    #[test]
    fn alt_clicking_a_knob_restores_the_default_inside_its_own_gesture() {
        let param = gain();
        param.set_plain(-30.0);
        let host = SpyHost::default();
        let binding = ParamBinding::new(&param, Some(&host));
        assert_eq!(param.plain(), -30.0);

        let mut h = Harness::new();
        h.modifiers = egui::Modifiers::ALT;
        let hover = h.move_to(30.0, 30.0);
        h.frame(
            vec![hover, egui::Event::ModifiersChanged(egui::Modifiers::ALT)],
            |ui| {
                ui.add(ParamKnob::new(&binding).diameter(60.0));
            },
        );
        let press = h.press();
        h.frame(vec![press], |ui| {
            ui.add(ParamKnob::new(&binding).diameter(60.0));
        });
        let release = h.release();
        h.frame(vec![release], |ui| {
            ui.add(ParamKnob::new(&binding).diameter(60.0));
        });

        assert_eq!(param.plain(), 0.0, "alt-click must restore the default");
        assert_eq!(host.count("begin"), 1, "{:?}", host.calls());
        assert_eq!(host.count("end"), 1, "{:?}", host.calls());
        std::mem::forget(binding);
    }

    #[test]
    fn clicking_a_slider_jumps_to_the_pointer_and_records_one_gesture() {
        let param = gain();
        preset(&param, 0.0);
        let host = SpyHost::default();
        let binding = ParamBinding::new(&param, Some(&host));

        let mut h = Harness::new();
        let widget = |ui: &mut egui::Ui, b: &ParamBinding<'_>| {
            ui.add(ParamSlider::new(b).size(vec2(100.0, 20.0)));
        };
        // The slider is laid out at the top-left of the root ui; click three quarters along.
        let hover = h.move_to(75.0, 10.0);
        h.frame(vec![hover], |ui| widget(ui, &binding));
        let press = h.press();
        h.frame(vec![press], |ui| widget(ui, &binding));
        let release = h.release();
        h.frame(vec![release], |ui| widget(ui, &binding));

        assert!(
            binding.normalized() > 0.5,
            "a click three quarters along should be past the middle, got {}",
            binding.normalized()
        );
        assert_eq!(host.count("begin"), 1, "{:?}", host.calls());
        assert_eq!(host.count("end"), 1, "{:?}", host.calls());
        std::mem::forget(binding);
    }

    #[test]
    fn a_toggle_flips_the_parameter_and_brackets_the_change() {
        let flip = BoolParam::new(ParamId(3), "Invert", false);
        let host = SpyHost::default();
        let binding = ParamBinding::new(&flip, Some(&host));
        assert_eq!(binding.normalized(), 0.0);

        let mut h = Harness::new();
        let widget = |ui: &mut egui::Ui, b: &ParamBinding<'_>| {
            ui.add(ParamToggle::new(b));
        };
        let hover = h.move_to(10.0, 10.0);
        h.frame(vec![hover], |ui| widget(ui, &binding));
        let press = h.press();
        h.frame(vec![press], |ui| widget(ui, &binding));
        let release = h.release();
        h.frame(vec![release], |ui| widget(ui, &binding));

        assert!(flip.value(), "the click must have toggled the parameter");
        assert_eq!(host.count("begin"), 1, "{:?}", host.calls());
        assert_eq!(host.count("end"), 1, "{:?}", host.calls());
        assert_eq!(host.count("changed"), 1, "{:?}", host.calls());
        std::mem::forget(binding);
    }

    #[test]
    fn a_toggle_defaults_to_the_parameters_own_name() {
        let flip = BoolParam::new(ParamId(3), "Invert", false);
        let binding = ParamBinding::new(&flip, None);
        assert_eq!(binding.param().info().name, "Invert");
        // Nothing to assert about pixels; what matters is that building the label reads the
        // parameter rather than requiring the caller to repeat it.
        let toggle = ParamToggle::new(&binding);
        assert!(toggle.text.is_none());
        assert_eq!(ParamToggle::new(&binding).text("Flip").text, Some("Flip"));
    }

    #[test]
    fn committed_text_is_applied_and_rejected_text_leaves_the_value_alone() {
        let param = gain();
        let host = SpyHost::default();
        let binding = ParamBinding::new(&param, Some(&host));
        let mut h = Harness::new();
        let field = |ui: &mut egui::Ui, b: &ParamBinding<'_>| {
            ui.add(ParamValueEdit::new(b));
        };

        // Frame 1: lay the field out and ask for focus, as a click would.
        h.frame(vec![], |ui| {
            ui.add(ParamValueEdit::new(&binding)).request_focus();
        });
        // Frame 2: the first frame with focus — the value is selected, ready to be replaced.
        h.frame(vec![], |ui| field(ui, &binding));
        // Frame 3: type over the selection.
        h.frame(vec![egui::Event::Text("-6".to_owned())], |ui| {
            field(ui, &binding)
        });
        // Frame 4: commit by moving focus away.
        h.frame(vec![tab()], |ui| field(ui, &binding));
        h.frame(vec![], |ui| field(ui, &binding));

        assert_eq!(param.plain(), -6.0, "{:?}", host.calls());
        assert_eq!(
            host.count("begin"),
            1,
            "text entry is one complete edit: {:?}",
            host.calls()
        );
        assert_eq!(host.count("end"), 1, "{:?}", host.calls());
        std::mem::forget(binding);
    }

    /// The Tab keystroke a test uses to move focus away and commit an edit.
    fn tab() -> egui::Event {
        egui::Event::Key {
            key: egui::Key::Tab,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn nonsense_text_is_refused_without_touching_the_parameter() {
        let param = gain();
        param.set_plain(-12.0);
        let before = param.plain();
        let host = SpyHost::default();
        let binding = ParamBinding::new(&param, Some(&host));
        let mut h = Harness::new();
        let field = |ui: &mut egui::Ui, b: &ParamBinding<'_>| {
            ui.add(ParamValueEdit::new(b));
        };

        h.frame(vec![], |ui| {
            ui.add(ParamValueEdit::new(&binding)).request_focus();
        });
        h.frame(vec![], |ui| field(ui, &binding));
        h.frame(vec![egui::Event::Text("wombat".to_owned())], |ui| {
            field(ui, &binding)
        });
        h.frame(vec![tab()], |ui| field(ui, &binding));

        assert_eq!(
            param.plain(),
            before,
            "a rejected entry must change nothing"
        );
        assert_eq!(
            host.count("begin"),
            0,
            "a rejected entry must not open a gesture: {:?}",
            host.calls()
        );
        std::mem::forget(binding);
    }

    #[test]
    fn focusing_a_value_field_selects_the_whole_value_so_typing_replaces_it() {
        // Without this, typing `-6` into a field showing `0.00 dB` commits `0.00 dB-6`, which
        // a lenient parser then reads as 0 dB — the change silently does the opposite of what
        // the user asked for.
        let param = gain();
        param.set_plain(-12.0);
        let binding = ParamBinding::new(&param, None);
        let mut h = Harness::new();
        let field = |ui: &mut egui::Ui, b: &ParamBinding<'_>| {
            ui.add(ParamValueEdit::new(b));
        };

        h.frame(vec![], |ui| {
            ui.add(ParamValueEdit::new(&binding)).request_focus();
        });
        h.frame(vec![], |ui| field(ui, &binding));
        h.frame(vec![egui::Event::Text("3".to_owned())], |ui| {
            field(ui, &binding)
        });
        h.frame(vec![tab()], |ui| field(ui, &binding));

        assert_eq!(
            param.plain(),
            3.0,
            "the typed value replaced the old one instead of being appended to it"
        );
    }

    #[test]
    fn an_abandoned_edit_leaves_the_field_showing_the_real_value() {
        let param = gain();
        param.set_plain(-12.0);
        let binding = ParamBinding::new(&param, None);
        let mut h = Harness::new();
        let field = |ui: &mut egui::Ui, b: &ParamBinding<'_>| {
            ui.add(ParamValueEdit::new(b));
        };

        h.frame(vec![], |ui| {
            ui.add(ParamValueEdit::new(&binding)).request_focus();
        });
        h.frame(vec![], |ui| field(ui, &binding));
        h.frame(vec![egui::Event::Text("nonsense".to_owned())], |ui| {
            field(ui, &binding)
        });
        h.frame(vec![tab()], |ui| field(ui, &binding));
        h.frame(vec![], |ui| field(ui, &binding));
        assert_eq!(param.plain(), -12.0, "the rejected entry changed the value");

        // Edit again. If the abandoned "nonsense" had survived in egui's memory, the next
        // edit would start from it and commit something the user never typed.
        h.frame(vec![], |ui| {
            ui.add(ParamValueEdit::new(&binding)).request_focus();
        });
        h.frame(vec![], |ui| field(ui, &binding));
        h.frame(vec![egui::Event::Text("3".to_owned())], |ui| {
            field(ui, &binding)
        });
        h.frame(vec![tab()], |ui| field(ui, &binding));

        assert_eq!(
            param.plain(),
            3.0,
            "a stale buffer from the abandoned edit leaked into the next one"
        );
    }

    #[test]
    fn builders_clamp_sizes_that_would_produce_nan_geometry() {
        let param = gain();
        let binding = ParamBinding::new(&param, None);

        assert_eq!(ParamKnob::new(&binding).diameter(f32::NAN).diameter, 4.0);
        assert_eq!(ParamKnob::new(&binding).diameter(-10.0).diameter, 4.0);
        assert_eq!(ParamKnob::new(&binding).diameter(64.0).diameter, 64.0);

        assert_eq!(
            ParamKnob::new(&binding).drag_length(0.0).drag_length,
            1.0,
            "a zero drag length would divide by zero"
        );
        assert_eq!(
            ParamKnob::new(&binding)
                .drag_length(f32::INFINITY)
                .drag_length,
            DEFAULT_DRAG_LENGTH
        );

        let slider = ParamSlider::new(&binding).size(vec2(f32::NAN, -3.0));
        assert_eq!(slider.size, vec2(4.0, 4.0));
        assert_eq!(ParamValueEdit::new(&binding).width(f32::NAN).width, 8.0);
    }

    #[test]
    fn a_widget_that_is_never_touched_opens_no_gesture_at_all() {
        // Laying a control out is not interacting with it. A plug-in whose editor merely
        // exists must not write automation.
        let param = gain();
        let host = SpyHost::default();
        {
            let binding = ParamBinding::new(&param, Some(&host));
            let mut h = Harness::new();
            for _ in 0..5 {
                h.frame(vec![], |ui| {
                    ui.add(ParamKnob::new(&binding));
                    ui.add(ParamSlider::new(&binding));
                    ui.add(ParamValueEdit::new(&binding));
                });
            }
        }
        assert!(
            host.calls().is_empty(),
            "an untouched editor talked to the host: {:?}",
            host.calls()
        );
        assert_eq!(param.plain(), 0.0);
    }

    #[test]
    fn the_free_functions_add_the_same_widgets() {
        let param = gain();
        let flip = BoolParam::new(ParamId(3), "Invert", false);
        let knob_binding = ParamBinding::new(&param, None);
        let toggle_binding = ParamBinding::new(&flip, None);
        let mut h = Harness::new();
        h.frame(vec![], |ui| {
            assert!(param_knob(ui, &knob_binding).rect.width() > 0.0);
            assert!(param_slider(ui, &knob_binding).rect.width() > 0.0);
            assert!(param_toggle(ui, &toggle_binding).rect.width() > 0.0);
            assert!(param_value_edit(ui, &knob_binding).rect.width() > 0.0);
        });
    }
}
