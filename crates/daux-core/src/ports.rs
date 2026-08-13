//! Event port topology, the event-side counterpart of [`BusLayout`](daux_audio::BusLayout).

use core::fmt;

/// One event port: a named stream of notes, controllers and parameter changes.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct EventPortInfo {
    /// Display name, e.g. `"MIDI In"`.
    pub name: String,
    /// `true` when this is the port a host should connect by default.
    pub is_main: bool,
    /// `true` when the port speaks MIDI 2.0 / UMP as well as MIDI 1.0.
    ///
    /// Every DAUx event port accepts MIDI 1.0; this only advertises the extended vocabulary
    /// (abi-v1 §9), so a host that only speaks MIDI 1.0 can ignore the flag entirely.
    pub supports_midi2: bool,
}

impl EventPortInfo {
    /// [main-thread] A main port with the given name, MIDI 1.0 only.
    pub fn main(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            is_main: true,
            supports_midi2: false,
        }
    }

    /// [main-thread] A secondary port with the given name, MIDI 1.0 only.
    pub fn aux(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            is_main: false,
            supports_midi2: false,
        }
    }

    /// Marks this port as also speaking MIDI 2.0 / UMP.
    #[must_use]
    pub fn with_midi2(mut self) -> Self {
        self.supports_midi2 = true;
        self
    }
}

impl fmt::Display for EventPortInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)?;
        if self.is_main {
            f.write_str(" (main)")?;
        }
        if self.supports_midi2 {
            f.write_str(" [midi2]")?;
        }
        Ok(())
    }
}

/// A plug-in's event ports, inputs and outputs.
///
/// The default is no ports at all, which is right for a pure audio effect. An instrument
/// declares one input; a MIDI effect declares one of each.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct EventPortLayout {
    /// Ports the host writes events into, in index order.
    pub inputs: Vec<EventPortInfo>,
    /// Ports the plug-in writes events out of, in index order.
    pub outputs: Vec<EventPortInfo>,
}

impl EventPortLayout {
    /// [main-thread] No event ports: a pure audio effect.
    pub fn none() -> Self {
        Self::default()
    }

    /// [main-thread] One main input and nothing else: the instrument layout.
    pub fn instrument() -> Self {
        Self {
            inputs: vec![EventPortInfo::main("Event In")],
            outputs: Vec::new(),
        }
    }

    /// [main-thread] One main input and one main output: the MIDI-effect layout.
    pub fn midi_effect() -> Self {
        Self {
            inputs: vec![EventPortInfo::main("Event In")],
            outputs: vec![EventPortInfo::main("Event Out")],
        }
    }

    /// Appends an input port.
    #[must_use]
    pub fn with_input(mut self, port: EventPortInfo) -> Self {
        self.inputs.push(port);
        self
    }

    /// Appends an output port.
    #[must_use]
    pub fn with_output(mut self, port: EventPortInfo) -> Self {
        self.outputs.push(port);
        self
    }

    /// [main-thread] `true` when the plug-in consumes events.
    pub fn has_input(&self) -> bool {
        !self.inputs.is_empty()
    }

    /// [main-thread] `true` when the plug-in produces events.
    pub fn has_output(&self) -> bool {
        !self.outputs.is_empty()
    }

    /// [main-thread] The first input marked main, or the first input.
    pub fn main_input(&self) -> Option<&EventPortInfo> {
        self.inputs
            .iter()
            .find(|p| p.is_main)
            .or_else(|| self.inputs.first())
    }

    /// [main-thread] The first output marked main, or the first output.
    pub fn main_output(&self) -> Option<&EventPortInfo> {
        self.outputs
            .iter()
            .find(|p| p.is_main)
            .or_else(|| self.outputs.first())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_layout_has_no_ports() {
        let l = EventPortLayout::default();
        assert_eq!(l, EventPortLayout::none());
        assert!(!l.has_input());
        assert!(!l.has_output());
        assert!(l.main_input().is_none());
        assert!(l.main_output().is_none());
    }

    #[test]
    fn the_instrument_layout_consumes_but_does_not_produce() {
        let l = EventPortLayout::instrument();
        assert!(l.has_input());
        assert!(!l.has_output());
        assert_eq!(l.main_input().unwrap().name, "Event In");
    }

    #[test]
    fn the_midi_effect_layout_does_both() {
        let l = EventPortLayout::midi_effect();
        assert!(l.has_input());
        assert!(l.has_output());
        assert_eq!(l.main_output().unwrap().name, "Event Out");
    }

    #[test]
    fn ports_can_be_added_and_flagged() {
        let l = EventPortLayout::none()
            .with_input(EventPortInfo::main("A").with_midi2())
            .with_input(EventPortInfo::aux("B"))
            .with_output(EventPortInfo::aux("C"));
        assert_eq!(l.inputs.len(), 2);
        assert!(l.inputs[0].supports_midi2);
        assert!(!l.inputs[1].is_main);
        // With no main output declared, the first output stands in for it.
        assert_eq!(l.main_output().unwrap().name, "C");
        assert_eq!(l.inputs[0].to_string(), "A (main) [midi2]");
    }
}
