//! Framework-neutral input, as it arrives from a host.
//!
//! Every backend translates these into its own toolkit's events. The vocabulary is
//! deliberately small: it covers what a host can reliably deliver across Win32, Cocoa, X11
//! and Wayland, and nothing more. Gestures, IME composition and drag-and-drop are the
//! backend's business, reconstructed from these primitives.

use core::fmt;

use crate::LogicalPoint;

/// Which pointer button an event refers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PointerButton {
    /// The primary button — left on a right-handed mouse.
    Primary,
    /// The secondary button, which conventionally opens a context menu.
    Secondary,
    /// The middle button or wheel click.
    Middle,
    /// Any further button, numbered from 0 as the host reports it.
    Other(u8),
}

impl PointerButton {
    /// [main-thread] `true` for the button that begins a parameter gesture.
    pub const fn is_primary(self) -> bool {
        matches!(self, PointerButton::Primary)
    }
}

/// The keyboard modifiers held when an event was produced.
///
/// [`Modifiers::command`] is the platform's "primary" modifier: Command on macOS, Control
/// everywhere else. Backends should branch on `command` rather than on `ctrl` for shortcuts,
/// so the same code does the right thing on every platform.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Modifiers {
    /// Shift is down.
    pub shift: bool,
    /// Control is down.
    pub ctrl: bool,
    /// Alt / Option is down.
    pub alt: bool,
    /// The Windows / Command / Super key is down.
    pub meta: bool,
}

impl Modifiers {
    /// Nothing held.
    pub const NONE: Self = Self {
        shift: false,
        ctrl: false,
        alt: false,
        meta: false,
    };

    /// [main-thread] The platform's shortcut modifier: Command on macOS, Control elsewhere.
    pub const fn command(self) -> bool {
        if cfg!(target_os = "macos") {
            self.meta
        } else {
            self.ctrl
        }
    }

    /// [main-thread] `true` when no modifier at all is held.
    pub const fn is_empty(self) -> bool {
        !(self.shift || self.ctrl || self.alt || self.meta)
    }

    /// [main-thread] The conventional "fine adjustment" modifier for a knob or slider.
    pub const fn is_fine_adjust(self) -> bool {
        self.shift
    }

    /// [main-thread] The conventional "reset to default" modifier for a control.
    pub const fn is_reset(self) -> bool {
        self.alt
    }
}

/// A named, layout-independent key.
///
/// Only the keys an editor plausibly needs are named. Anything else arrives as
/// [`Key::Unknown`] with the host's raw code, and as an [`InputEvent::Text`] if it produced
/// a character — text entry must never be reconstructed from key codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Key {
    /// Enter / Return.
    Enter,
    /// Escape.
    Escape,
    /// Tab.
    Tab,
    /// Backspace.
    Backspace,
    /// Delete (forward).
    Delete,
    /// Space.
    Space,
    /// Arrow up.
    ArrowUp,
    /// Arrow down.
    ArrowDown,
    /// Arrow left.
    ArrowLeft,
    /// Arrow right.
    ArrowRight,
    /// Home.
    Home,
    /// End.
    End,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
    /// A key this vocabulary does not name, with the host's raw code.
    Unknown(u32),
}

impl Key {
    /// [main-thread] `true` for the keys a host normally reserves for transport control.
    ///
    /// A host that gives the editor keyboard focus still usually wants the space bar back for
    /// start/stop. An editor that returns [`InputResponse::Ignored`] for these lets that keep
    /// working; one that swallows them will surprise the user.
    pub const fn is_host_reserved(self) -> bool {
        matches!(self, Key::Space)
    }

    /// [main-thread] `true` for keys that move a focus ring or a value.
    pub const fn is_navigation(self) -> bool {
        matches!(
            self,
            Key::ArrowUp
                | Key::ArrowDown
                | Key::ArrowLeft
                | Key::ArrowRight
                | Key::Home
                | Key::End
                | Key::PageUp
                | Key::PageDown
                | Key::Tab
        )
    }
}

/// One input event delivered to an editor.
///
/// Coordinates are **logical** and relative to the editor's own top-left corner: the host has
/// already divided out the scale factor, so an editor never has to know what the display's
/// DPI is in order to hit-test a widget.
///
/// [main-thread]
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum InputEvent {
    /// The pointer moved to `position`.
    PointerMoved {
        /// Where the pointer now is.
        position: LogicalPoint,
        /// Modifiers held at the time.
        modifiers: Modifiers,
    },
    /// A pointer button went down or up.
    PointerButton {
        /// Where the pointer was.
        position: LogicalPoint,
        /// Which button.
        button: PointerButton,
        /// `true` on press, `false` on release.
        pressed: bool,
        /// Modifiers held at the time.
        modifiers: Modifiers,
    },
    /// The pointer left the editor's area. No position: it is outside.
    PointerLeft,
    /// A scroll wheel or trackpad gesture, in logical units.
    Scroll {
        /// Where the pointer was.
        position: LogicalPoint,
        /// Horizontal scroll; positive is rightward.
        delta_x: f64,
        /// Vertical scroll; positive is upward.
        delta_y: f64,
        /// Modifiers held at the time.
        modifiers: Modifiers,
    },
    /// A key went down or up.
    Key {
        /// Which key.
        key: Key,
        /// `true` on press, `false` on release.
        pressed: bool,
        /// `true` when the platform generated this from auto-repeat.
        repeat: bool,
        /// Modifiers held at the time.
        modifiers: Modifiers,
    },
    /// Text was committed, already composed by the platform's IME.
    ///
    /// This is the only correct source of characters. A single event may carry more than one
    /// character, and a character may be more than one `char`.
    Text(String),
    /// The editor gained (`true`) or lost (`false`) keyboard focus.
    Focus(bool),
    /// The modifier state changed without any other event.
    Modifiers(Modifiers),
}

impl InputEvent {
    /// [main-thread] Where the pointer was, for the events that carry a position.
    pub fn position(&self) -> Option<LogicalPoint> {
        match self {
            InputEvent::PointerMoved { position, .. }
            | InputEvent::PointerButton { position, .. }
            | InputEvent::Scroll { position, .. } => Some(*position),
            _ => None,
        }
    }

    /// [main-thread] The modifier state this event was produced under, when it carries one.
    pub fn modifiers(&self) -> Option<Modifiers> {
        match self {
            InputEvent::PointerMoved { modifiers, .. }
            | InputEvent::PointerButton { modifiers, .. }
            | InputEvent::Scroll { modifiers, .. }
            | InputEvent::Key { modifiers, .. }
            | InputEvent::Modifiers(modifiers) => Some(*modifiers),
            _ => None,
        }
    }

    /// [main-thread] `true` for events a host may reasonably want back if the editor does not
    /// use them — chiefly the space bar.
    pub const fn is_host_reserved(&self) -> bool {
        matches!(self, InputEvent::Key { key, .. } if key.is_host_reserved())
    }
}

/// Whether an editor consumed an event.
///
/// This is not a formality. When an editor returns [`InputResponse::Ignored`] for a key, the
/// host forwards it to its own shortcuts; when it returns [`InputResponse::Consumed`], the
/// host must not. Swallowing everything is the reason some plug-in editors break the space
/// bar, and it is avoidable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum InputResponse {
    /// The editor did not use the event; the host should handle it.
    #[default]
    Ignored,
    /// The editor used the event; the host must not handle it.
    Consumed,
}

impl InputResponse {
    /// [main-thread] Builds a response from "did I use it?".
    pub const fn consumed_if(consumed: bool) -> Self {
        if consumed {
            InputResponse::Consumed
        } else {
            InputResponse::Ignored
        }
    }

    /// [main-thread] `true` when the host must not act on the event.
    pub const fn is_consumed(self) -> bool {
        matches!(self, InputResponse::Consumed)
    }

    /// [main-thread] Combines two responses: consumed wins.
    ///
    /// Useful when an editor dispatches one event to several widgets.
    pub const fn or(self, other: Self) -> Self {
        if self.is_consumed() { self } else { other }
    }
}

impl fmt::Display for InputResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            InputResponse::Ignored => "ignored",
            InputResponse::Consumed => "consumed",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_modifier_follows_the_platform() {
        let meta = Modifiers {
            meta: true,
            ..Modifiers::NONE
        };
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::NONE
        };
        if cfg!(target_os = "macos") {
            assert!(meta.command());
            assert!(!ctrl.command());
        } else {
            assert!(ctrl.command());
            assert!(!meta.command());
        }
    }

    #[test]
    fn empty_modifiers_are_the_default() {
        assert_eq!(Modifiers::default(), Modifiers::NONE);
        assert!(Modifiers::NONE.is_empty());
        assert!(
            !Modifiers {
                shift: true,
                ..Modifiers::NONE
            }
            .is_empty()
        );
    }

    #[test]
    fn the_conventional_control_modifiers_are_named() {
        let shift = Modifiers {
            shift: true,
            ..Modifiers::NONE
        };
        let alt = Modifiers {
            alt: true,
            ..Modifiers::NONE
        };
        assert!(shift.is_fine_adjust());
        assert!(!shift.is_reset());
        assert!(alt.is_reset());
        assert!(!alt.is_fine_adjust());
    }

    #[test]
    fn positioned_events_report_their_position() {
        let p = LogicalPoint::new(10.0, 20.0);
        let m = Modifiers::NONE;
        assert_eq!(
            InputEvent::PointerMoved {
                position: p,
                modifiers: m
            }
            .position(),
            Some(p)
        );
        assert_eq!(
            InputEvent::Scroll {
                position: p,
                delta_x: 0.0,
                delta_y: 1.0,
                modifiers: m
            }
            .position(),
            Some(p)
        );
        assert_eq!(InputEvent::PointerLeft.position(), None);
        assert_eq!(InputEvent::Focus(true).position(), None);
        assert_eq!(InputEvent::Text("a".into()).position(), None);
    }

    #[test]
    fn modifier_carrying_events_report_their_modifiers() {
        let m = Modifiers {
            shift: true,
            ..Modifiers::NONE
        };
        assert_eq!(
            InputEvent::Key {
                key: Key::Enter,
                pressed: true,
                repeat: false,
                modifiers: m
            }
            .modifiers(),
            Some(m)
        );
        assert_eq!(InputEvent::Modifiers(m).modifiers(), Some(m));
        assert_eq!(InputEvent::PointerLeft.modifiers(), None);
    }

    #[test]
    fn the_space_bar_is_reserved_so_transport_control_keeps_working() {
        let space = InputEvent::Key {
            key: Key::Space,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        assert!(space.is_host_reserved());

        let enter = InputEvent::Key {
            key: Key::Enter,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        assert!(!enter.is_host_reserved());
        assert!(!InputEvent::Focus(true).is_host_reserved());
    }

    #[test]
    fn navigation_keys_are_classified() {
        assert!(Key::ArrowUp.is_navigation());
        assert!(Key::Tab.is_navigation());
        assert!(!Key::Enter.is_navigation());
        assert!(!Key::Unknown(42).is_navigation());
    }

    #[test]
    fn responses_default_to_ignored_and_combine_with_consumed_winning() {
        assert_eq!(InputResponse::default(), InputResponse::Ignored);
        assert_eq!(
            InputResponse::consumed_if(true),
            InputResponse::Consumed
        );
        assert_eq!(
            InputResponse::Ignored.or(InputResponse::Consumed),
            InputResponse::Consumed
        );
        assert_eq!(
            InputResponse::Consumed.or(InputResponse::Ignored),
            InputResponse::Consumed
        );
        assert_eq!(
            InputResponse::Ignored.or(InputResponse::Ignored),
            InputResponse::Ignored
        );
        assert_eq!(InputResponse::Consumed.to_string(), "consumed");
    }

    #[test]
    fn only_the_primary_button_starts_a_gesture() {
        assert!(PointerButton::Primary.is_primary());
        assert!(!PointerButton::Secondary.is_primary());
        assert!(!PointerButton::Other(3).is_primary());
    }
}
