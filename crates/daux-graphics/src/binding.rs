//! Parameter ↔ widget glue, written once so no backend has to get it right again.
//!
//! Every plug-in editor has to do the same four things when the user drags a knob: tell the
//! host a gesture began, push values as the pointer moves, tell the host the gesture ended,
//! and format the value for display. Getting the gesture bookkeeping wrong is what makes
//! automation recording produce either nothing or a single spike, and it is not obvious from
//! the outside that it is wrong.
//!
//! [`ParamBinding`] does it correctly and is reusable from egui, GPUI, OpenGL or a hand-rolled
//! renderer, because it knows nothing about any of them.

use core::cell::Cell;
use core::fmt;

use daux_host_services::{HostParams, ParamId};
use daux_parameter::Param;

/// One parameter, bound to one widget, for the lifetime of a frame or a drag.
///
/// # Gestures
///
/// A host records automation between [`begin_gesture`](Self::begin_gesture) and
/// [`end_gesture`](Self::end_gesture). The binding tracks whether a gesture is open, so:
///
/// - calling `begin_gesture` twice does not send two begins,
/// - calling `end_gesture` without a begin sends nothing,
/// - and dropping the binding mid-drag closes the gesture rather than leaving the host
///   recording forever.
///
/// That last one matters: an editor closed while the user is holding a control would
/// otherwise leave the host's automation lane open indefinitely.
///
/// [main-thread]
pub struct ParamBinding<'a> {
    param: &'a dyn Param,
    host: Option<&'a dyn HostParams>,
    id: ParamId,
    gesturing: Cell<bool>,
}

impl fmt::Debug for ParamBinding<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ParamBinding")
            .field("id", &self.id)
            .field("gesturing", &self.gesturing.get())
            .field("has_host", &self.host.is_some())
            .finish_non_exhaustive()
    }
}

impl<'a> ParamBinding<'a> {
    /// [main-thread] Binds a parameter, optionally to a host that records automation.
    ///
    /// `host` is `None` in a preview harness or a unit test. Everything still works; the
    /// value changes simply go nowhere but the parameter itself.
    pub fn new(param: &'a dyn Param, host: Option<&'a dyn HostParams>) -> Self {
        Self {
            id: param.id(),
            param,
            host,
            gesturing: Cell::new(false),
        }
    }

    /// [main-thread] The bound parameter's permanent id.
    pub const fn id(&self) -> ParamId {
        self.id
    }

    /// [main-thread] The bound parameter.
    pub const fn param(&self) -> &'a dyn Param {
        self.param
    }

    /// [main-thread] `true` while a gesture is open.
    pub fn is_gesturing(&self) -> bool {
        self.gesturing.get()
    }

    /// [main-thread] The user grabbed the control.
    ///
    /// Idempotent: a second call while a gesture is already open does nothing.
    pub fn begin_gesture(&self) {
        if self.gesturing.replace(true) {
            return;
        }
        if let Some(host) = self.host {
            host.gesture_begin(self.id);
        }
    }

    /// [main-thread] The user let go of the control.
    ///
    /// Idempotent: a call with no gesture open does nothing.
    pub fn end_gesture(&self) {
        if !self.gesturing.replace(false) {
            return;
        }
        if let Some(host) = self.host {
            host.gesture_end(self.id);
        }
    }

    /// [main-thread] The current value in real-world units.
    pub fn plain(&self) -> f64 {
        self.param.plain()
    }

    /// [main-thread] The current value as a `0..=1` position on the control.
    pub fn normalized(&self) -> f64 {
        self.param.normalized()
    }

    /// [main-thread] Sets a real-world value and tells the host.
    ///
    /// The host is notified with the value the parameter actually took, not the one it was
    /// asked for — the range may have clamped or quantised it, and a host told the
    /// unclamped value would draw its automation lane out of step with the audio.
    pub fn set_plain(&self, value: f64) {
        self.param.set_plain(value);
        self.notify();
    }

    /// [main-thread] Sets a `0..=1` position on the control and tells the host.
    pub fn set_normalized(&self, value: f64) {
        self.param.set_normalized(value);
        self.notify();
    }

    /// [main-thread] Nudges the normalised position by `delta`, clamped to `0..=1`.
    ///
    /// The right primitive for an arrow key or a scroll wheel.
    pub fn nudge_normalized(&self, delta: f64) {
        if !delta.is_finite() {
            return;
        }
        self.set_normalized((self.normalized() + delta).clamp(0.0, 1.0));
    }

    /// [main-thread] Restores the parameter's default and tells the host.
    ///
    /// This is what alt-click on a control should do — see
    /// [`Modifiers::is_reset`](crate::Modifiers::is_reset).
    pub fn reset(&self) {
        self.param.reset();
        self.notify();
    }

    /// [main-thread] The current value formatted for display, with its unit.
    pub fn display(&self) -> String {
        self.param.text(self.param.plain())
    }

    /// [main-thread] Formats the current value into a reused buffer.
    ///
    /// An editor that redraws every frame should use this rather than
    /// [`display`](Self::display), which allocates a fresh `String` each time.
    pub fn display_into(&self, out: &mut String) {
        self.param.to_text(self.param.plain(), out);
    }

    /// [main-thread] Applies text the user typed, returning whether it was accepted.
    ///
    /// Text entry is a complete edit, not a drag, so this brackets the change in its own
    /// gesture: the host records one automation point rather than none.
    pub fn apply_text(&self, text: &str) -> bool {
        let Some(plain) = self.param.from_text(text) else {
            return false;
        };
        let was_open = self.is_gesturing();
        if !was_open {
            self.begin_gesture();
        }
        self.set_plain(plain);
        if !was_open {
            self.end_gesture();
        }
        true
    }

    /// Tells the host the value moved, with whatever the parameter actually stored.
    fn notify(&self) {
        if let Some(host) = self.host {
            host.changed(self.id, self.param.plain());
        }
    }
}

impl Drop for ParamBinding<'_> {
    /// Closes an open gesture rather than leaving the host recording automation forever.
    fn drop(&mut self) {
        self.end_gesture();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use daux_host_services::RescanFlags;
    use daux_parameter::{FloatParam, ParamRange};
    use std::sync::Mutex;

    /// Records the calls a host would have seen.
    #[derive(Default)]
    struct SpyHost {
        calls: Mutex<Vec<String>>,
    }

    impl SpyHost {
        fn calls(&self) -> Vec<String> {
            self.calls
                .lock()
                .expect("no test panics while holding this")
                .clone()
        }

        fn push(&self, s: String) {
            self.calls
                .lock()
                .expect("no test panics while holding this")
                .push(s);
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

    #[test]
    fn a_drag_produces_one_begin_and_one_end() {
        let param = gain();
        let host = SpyHost::default();
        {
            let b = ParamBinding::new(&param, Some(&host));
            b.begin_gesture();
            b.set_plain(-6.0);
            b.set_plain(-12.0);
            b.end_gesture();
        }
        let calls = host.calls();
        assert_eq!(calls[0], "begin 7");
        assert_eq!(calls.last().unwrap(), "end 7");
        assert_eq!(calls.iter().filter(|c| c.starts_with("begin")).count(), 1);
        assert_eq!(calls.iter().filter(|c| c.starts_with("end")).count(), 1);
        assert_eq!(calls.iter().filter(|c| c.starts_with("changed")).count(), 2);
    }

    #[test]
    fn redundant_begins_and_ends_are_swallowed() {
        let param = gain();
        let host = SpyHost::default();
        let b = ParamBinding::new(&param, Some(&host));
        b.end_gesture(); // no gesture open: nothing at all
        b.begin_gesture();
        b.begin_gesture();
        assert!(b.is_gesturing());
        b.end_gesture();
        b.end_gesture();
        assert!(!b.is_gesturing());
        core::mem::forget(b); // do not let Drop add another end

        assert_eq!(host.calls(), ["begin 7", "end 7"]);
    }

    #[test]
    fn dropping_mid_drag_closes_the_gesture() {
        let param = gain();
        let host = SpyHost::default();
        {
            let b = ParamBinding::new(&param, Some(&host));
            b.begin_gesture();
            b.set_normalized(0.75);
            // The editor is torn down while the user is still holding the control.
        }
        assert_eq!(host.calls().last().unwrap(), "end 7");
    }

    #[test]
    fn the_host_hears_the_value_the_parameter_actually_took() {
        let param = gain();
        let host = SpyHost::default();
        let b = ParamBinding::new(&param, Some(&host));
        b.set_plain(1000.0); // far above the +12 dB ceiling
        core::mem::forget(b);

        assert_eq!(param.plain(), 12.0);
        assert_eq!(host.calls(), ["changed 7 12"]);
    }

    #[test]
    fn text_entry_is_bracketed_in_its_own_gesture() {
        let param = gain();
        let host = SpyHost::default();
        let b = ParamBinding::new(&param, Some(&host));
        assert!(b.apply_text("-6 dB"));
        core::mem::forget(b);

        let calls = host.calls();
        assert_eq!(calls[0], "begin 7");
        assert_eq!(calls[2], "end 7");
        assert_eq!(param.plain(), -6.0);
    }

    #[test]
    fn rejected_text_changes_nothing_and_opens_no_gesture() {
        let param = gain();
        let host = SpyHost::default();
        let b = ParamBinding::new(&param, Some(&host));
        assert!(!b.apply_text("not a number"));
        core::mem::forget(b);

        assert_eq!(param.plain(), 0.0);
        assert!(host.calls().is_empty());
    }

    #[test]
    fn text_entry_inside_an_open_drag_does_not_close_it() {
        let param = gain();
        let host = SpyHost::default();
        let b = ParamBinding::new(&param, Some(&host));
        b.begin_gesture();
        assert!(b.apply_text("3"));
        assert!(b.is_gesturing(), "the drag must still be open");
        core::mem::forget(b);

        assert_eq!(host.calls().iter().filter(|c| *c == "end 7").count(), 0);
    }

    #[test]
    fn nudging_clamps_and_ignores_nonsense() {
        let param = gain();
        let b = ParamBinding::new(&param, None);
        b.set_normalized(0.5);
        b.nudge_normalized(0.25);
        assert!((b.normalized() - 0.75).abs() < 1e-9);
        b.nudge_normalized(10.0);
        assert_eq!(b.normalized(), 1.0);
        b.nudge_normalized(-10.0);
        assert_eq!(b.normalized(), 0.0);

        let before = b.normalized();
        b.nudge_normalized(f64::NAN);
        assert_eq!(b.normalized(), before);
    }

    #[test]
    fn a_binding_works_with_no_host_at_all() {
        let param = gain();
        let b = ParamBinding::new(&param, None);
        b.begin_gesture();
        b.set_plain(-3.0);
        b.end_gesture();
        b.reset();
        assert_eq!(param.plain(), 0.0);
        assert_eq!(b.id(), ParamId(7));
    }

    #[test]
    fn display_and_display_into_agree() {
        let param = gain();
        let b = ParamBinding::new(&param, None);
        b.set_plain(-6.0);
        let mut buf = String::from("stale contents");
        b.display_into(&mut buf);
        assert_eq!(buf, b.display());
        assert!(buf.contains("dB"), "{buf}");
    }
}
