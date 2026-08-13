//! What a plug-in can do, as a bitset.

use core::fmt;
use core::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign};

/// Declares the capability bits once and derives the constant, the builder-style
/// setter, the predicate and the `Debug` name table from one description.
///
/// Writing the bit number, the `with_*`, the `is_*` and the name table by hand
/// four times over twenty capabilities is exactly the sort of repetition where a
/// transposition hides for a year, so it is generated instead.
macro_rules! capabilities {
    ($(
        $(#[$meta:meta])*
        $konst:ident = $bit:literal, $with:ident, $has:ident;
    )*) => {
        impl Capabilities {
            $(
                $(#[$meta])*
                pub const $konst: Self = Self(1 << $bit);
            )*

            /// Every bit ABI v1 defines. Anything outside this mask is reserved.
            pub const ALL: Self = Self($((1u64 << $bit) |)* 0);

            /// The defined bits with their names, for [`fmt::Debug`] and for
            /// tooling that prints a descriptor.  `[main-thread]`
            pub const NAMES: &'static [(&'static str, Capabilities)] = &[
                $((stringify!($konst), Self(1 << $bit)),)*
            ];

            $(
                #[doc = concat!(
                    "Returns the set with [`", stringify!($konst),
                    "`](Capabilities::", stringify!($konst), ") added. `[any-thread]`"
                )]
                #[inline]
                #[must_use]
                pub const fn $with(self) -> Self {
                    self.union(Self::$konst)
                }

                #[doc = concat!(
                    "`true` when [`", stringify!($konst),
                    "`](Capabilities::", stringify!($konst), ") is set. `[any-thread]`"
                )]
                #[inline]
                #[must_use]
                pub const fn $has(self) -> bool {
                    self.contains(Self::$konst)
                }
            )*
        }
    };
}

/// The `DAUX_CAP_*` bitset a plug-in advertises. `[any-thread]`
///
/// The bit values are transcribed from `docs/specifications/abi-v1.md` §6.2 and
/// are part of the binary contract, so they can never be renumbered. Hosts use
/// them to decide where a plug-in may be instantiated before loading any DSP,
/// and the format adapters translate them into VST3 subcategories and CLAP
/// features.
///
/// Capabilities are declarative: setting [`Capabilities::HAS_GUI`] does not
/// create an editor, it *promises* one. Advertise only what is true — a host
/// that trusts a false promise is a host that shows an empty window.
///
/// ```
/// use daux_core::Capabilities;
///
/// let caps = Capabilities::NONE
///     .with_audio_effect()
///     .with_sidechain()
///     .with_has_gui();
///
/// assert!(caps.is_audio_effect());
/// assert!(!caps.is_instrument());
/// assert_eq!(caps.bits(), 0b1000_1000_0001);
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Capabilities(u64);

capabilities! {
    /// Processes audio. Mirrors `DAUX_CAP_AUDIO_EFFECT`.
    AUDIO_EFFECT = 0, with_audio_effect, is_audio_effect;
    /// Generates audio from notes. Mirrors `DAUX_CAP_INSTRUMENT`.
    INSTRUMENT = 1, with_instrument, is_instrument;
    /// Transforms events without touching audio. Mirrors `DAUX_CAP_MIDI_EFFECT`.
    MIDI_EFFECT = 2, with_midi_effect, is_midi_effect;
    /// Measures rather than alters. Mirrors `DAUX_CAP_ANALYZER`.
    ANALYZER = 3, with_analyzer, is_analyzer;
    /// Accepts note/MIDI input. Mirrors `DAUX_CAP_MIDI_INPUT`.
    MIDI_INPUT = 4, with_midi_input, is_midi_input;
    /// Emits note/MIDI output. Mirrors `DAUX_CAP_MIDI_OUTPUT`.
    MIDI_OUTPUT = 5, with_midi_output, is_midi_output;
    /// Understands MIDI 2.0 / UMP, not only MIDI 1.0. Mirrors `DAUX_CAP_MIDI2`.
    MIDI2 = 6, with_midi2, is_midi2;
    /// Has a sidechain input bus. Mirrors `DAUX_CAP_SIDECHAIN`.
    SIDECHAIN = 7, with_sidechain, is_sidechain;
    /// Buses can be added, removed or re-laid-out by the host. Mirrors
    /// `DAUX_CAP_DYNAMIC_BUSES`.
    DYNAMIC_BUSES = 8, with_dynamic_buses, is_dynamic_buses;
    /// Applies parameter automation at the exact sample offset of each event
    /// rather than once per block. Mirrors `DAUX_CAP_SAMPLE_ACCURATE_AUTO`.
    SAMPLE_ACCURATE_AUTO = 9, with_sample_accurate_auto, is_sample_accurate_auto;
    /// Responds to per-note expression. Mirrors `DAUX_CAP_NOTE_EXPRESSION`.
    NOTE_EXPRESSION = 10, with_note_expression, is_note_expression;
    /// Provides an editor. Mirrors `DAUX_CAP_HAS_GUI`.
    HAS_GUI = 11, with_has_gui, is_has_gui;
    /// Is unusable without its editor — vanishingly rare, and hostile to
    /// headless rendering. Mirrors `DAUX_CAP_REQUIRES_GUI`.
    REQUIRES_GUI = 12, with_requires_gui, is_requires_gui;
    /// Can present its editor as a shared GPU texture (abi-v1 §13). Mirrors
    /// `DAUX_CAP_SHARED_TEXTURE_GUI`.
    SHARED_TEXTURE_GUI = 13, with_shared_texture_gui, is_shared_texture_gui;
    /// Supports faster-than-real-time offline rendering. Mirrors
    /// `DAUX_CAP_OFFLINE_RENDER`.
    OFFLINE_RENDER = 14, with_offline_render, is_offline_render;
    /// Must run in real time to be correct — a hardware bridge, say — so the
    /// host must not render it offline. Mirrors `DAUX_CAP_HARD_REALTIME`.
    HARD_REALTIME = 15, with_hard_realtime, is_hard_realtime;
    /// Safe to run in a sandboxed child process: no undeclared filesystem,
    /// network or device access. Mirrors `DAUX_CAP_SANDBOX_SAFE`.
    SANDBOX_SAFE = 16, with_sandbox_safe, is_sandbox_safe;
    /// Only ever works in stereo. Mirrors `DAUX_CAP_STEREO_ONLY`.
    STEREO_ONLY = 17, with_stereo_only, is_stereo_only;
    /// Latency can change while loaded, so the host must re-read it. Mirrors
    /// `DAUX_CAP_LATENCY_DYNAMIC`.
    LATENCY_DYNAMIC = 18, with_latency_dynamic, is_latency_dynamic;
    /// Rings out forever — a freeze reverb, a feedback network. Mirrors
    /// `DAUX_CAP_TAIL_INFINITE`.
    TAIL_INFINITE = 19, with_tail_infinite, is_tail_infinite;
}

impl Capabilities {
    /// The empty set.
    pub const NONE: Self = Self(0);

    /// Wraps a raw `DAUX_CAP_*` bitset, preserving bits this version does not
    /// know. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    /// The raw bitset for `DauxPluginDescriptorV1::capabilities`.
    /// `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn bits(self) -> u64 {
        self.0
    }

    /// `true` when **every** bit of `other` is set. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// `true` when **any** bit of `other` is set. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    /// `true` when nothing is advertised. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The union of two sets. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// The intersection of two sets. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    /// `self` with every bit of `other` cleared. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// Adds or removes `other` depending on `on`. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn set(self, other: Self, on: bool) -> Self {
        if on {
            self.union(other)
        } else {
            self.without(other)
        }
    }

    /// Bits set but not defined by ABI v1 — a plug-in built against a newer
    /// SDK. Informational, never an error. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn unknown_bits(self) -> u64 {
        self.0 & !Self::ALL.0
    }

    /// Iterates the defined capabilities that are set, in bit order.
    /// `[main-thread]`
    pub fn iter(self) -> impl Iterator<Item = (&'static str, Capabilities)> {
        Self::NAMES
            .iter()
            .copied()
            .filter(move |(_, bit)| self.contains(*bit))
    }
}

impl BitOr for Capabilities {
    type Output = Self;
    #[inline]
    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

impl BitOrAssign for Capabilities {
    #[inline]
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl BitAnd for Capabilities {
    type Output = Self;
    #[inline]
    fn bitand(self, rhs: Self) -> Self {
        self.intersection(rhs)
    }
}

impl BitAndAssign for Capabilities {
    #[inline]
    fn bitand_assign(&mut self, rhs: Self) {
        self.0 &= rhs.0;
    }
}

impl fmt::Debug for Capabilities {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Capabilities(")?;
        let mut first = true;
        for (name, _) in self.iter() {
            if !first {
                f.write_str(" | ")?;
            }
            f.write_str(name)?;
            first = false;
        }
        let unknown = self.unknown_bits();
        if unknown != 0 {
            if !first {
                f.write_str(" | ")?;
            }
            write!(f, "{unknown:#x}")?;
            first = false;
        }
        if first {
            f.write_str("NONE")?;
        }
        f.write_str(")")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The numbers are the contract. Transcribed from
    /// `docs/specifications/abi-v1.md` §6.2 by hand, deliberately not derived
    /// from the macro, so a mistyped shift in one place cannot match a mistyped
    /// shift in the other.
    #[test]
    fn every_bit_matches_abi_v1_6_2() {
        assert_eq!(Capabilities::AUDIO_EFFECT.bits(), 1 << 0);
        assert_eq!(Capabilities::INSTRUMENT.bits(), 1 << 1);
        assert_eq!(Capabilities::MIDI_EFFECT.bits(), 1 << 2);
        assert_eq!(Capabilities::ANALYZER.bits(), 1 << 3);
        assert_eq!(Capabilities::MIDI_INPUT.bits(), 1 << 4);
        assert_eq!(Capabilities::MIDI_OUTPUT.bits(), 1 << 5);
        assert_eq!(Capabilities::MIDI2.bits(), 1 << 6);
        assert_eq!(Capabilities::SIDECHAIN.bits(), 1 << 7);
        assert_eq!(Capabilities::DYNAMIC_BUSES.bits(), 1 << 8);
        assert_eq!(Capabilities::SAMPLE_ACCURATE_AUTO.bits(), 1 << 9);
        assert_eq!(Capabilities::NOTE_EXPRESSION.bits(), 1 << 10);
        assert_eq!(Capabilities::HAS_GUI.bits(), 1 << 11);
        assert_eq!(Capabilities::REQUIRES_GUI.bits(), 1 << 12);
        assert_eq!(Capabilities::SHARED_TEXTURE_GUI.bits(), 1 << 13);
        assert_eq!(Capabilities::OFFLINE_RENDER.bits(), 1 << 14);
        assert_eq!(Capabilities::HARD_REALTIME.bits(), 1 << 15);
        assert_eq!(Capabilities::SANDBOX_SAFE.bits(), 1 << 16);
        assert_eq!(Capabilities::STEREO_ONLY.bits(), 1 << 17);
        assert_eq!(Capabilities::TAIL_INFINITE.bits(), 1 << 19);
        assert_eq!(Capabilities::LATENCY_DYNAMIC.bits(), 1 << 18);
        assert_eq!(Capabilities::ALL.bits(), (1 << 20) - 1);
        assert_eq!(Capabilities::NAMES.len(), 20);
    }

    #[test]
    fn the_setters_and_predicates_agree_bit_for_bit() {
        // Every generated `with_*` sets exactly the bit its `is_*` reports, and
        // touches nothing else.
        for (name, bit) in Capabilities::NAMES.iter().copied() {
            let set = Capabilities::NONE.union(bit);
            assert!(set.contains(bit), "{name}");
            assert_eq!(set.bits().count_ones(), 1, "{name} set more than one bit");
            assert_eq!(
                set.iter().map(|(n, _)| n).collect::<Vec<_>>(),
                [name],
                "{name} does not round-trip through iter()"
            );
        }

        let caps = Capabilities::NONE
            .with_audio_effect()
            .with_instrument()
            .with_midi_effect()
            .with_analyzer()
            .with_midi_input()
            .with_midi_output()
            .with_midi2()
            .with_sidechain()
            .with_dynamic_buses()
            .with_sample_accurate_auto()
            .with_note_expression()
            .with_has_gui()
            .with_requires_gui()
            .with_shared_texture_gui()
            .with_offline_render()
            .with_hard_realtime()
            .with_sandbox_safe()
            .with_stereo_only()
            .with_latency_dynamic()
            .with_tail_infinite();
        assert_eq!(caps, Capabilities::ALL);

        assert!(caps.is_audio_effect());
        assert!(caps.is_instrument());
        assert!(caps.is_midi_effect());
        assert!(caps.is_analyzer());
        assert!(caps.is_midi_input());
        assert!(caps.is_midi_output());
        assert!(caps.is_midi2());
        assert!(caps.is_sidechain());
        assert!(caps.is_dynamic_buses());
        assert!(caps.is_sample_accurate_auto());
        assert!(caps.is_note_expression());
        assert!(caps.is_has_gui());
        assert!(caps.is_requires_gui());
        assert!(caps.is_shared_texture_gui());
        assert!(caps.is_offline_render());
        assert!(caps.is_hard_realtime());
        assert!(caps.is_sandbox_safe());
        assert!(caps.is_stereo_only());
        assert!(caps.is_latency_dynamic());
        assert!(caps.is_tail_infinite());

        assert!(Capabilities::NONE.is_empty());
        assert!(!Capabilities::NONE.is_audio_effect());
    }

    #[test]
    fn set_algebra() {
        let a = Capabilities::AUDIO_EFFECT | Capabilities::SIDECHAIN;
        assert!(a.contains(Capabilities::AUDIO_EFFECT));
        assert!(!a.contains(Capabilities::ALL));
        assert!(a.intersects(Capabilities::SIDECHAIN | Capabilities::INSTRUMENT));
        assert_eq!(
            a.without(Capabilities::SIDECHAIN),
            Capabilities::AUDIO_EFFECT
        );
        assert_eq!(a & Capabilities::ALL, a);
        assert_eq!(
            a.set(Capabilities::HAS_GUI, true),
            a | Capabilities::HAS_GUI
        );
        assert_eq!(
            a.set(Capabilities::SIDECHAIN, false),
            Capabilities::AUDIO_EFFECT
        );

        let mut b = Capabilities::NONE;
        b |= Capabilities::ANALYZER;
        assert_eq!(b, Capabilities::ANALYZER);
        b &= Capabilities::AUDIO_EFFECT;
        assert!(b.is_empty());

        // `contains` of the empty set is vacuously true; `intersects` is not.
        assert!(a.contains(Capabilities::NONE));
        assert!(!a.intersects(Capabilities::NONE));
    }

    #[test]
    fn unknown_bits_survive_and_are_reported() {
        let raw = Capabilities::ALL.bits() | (1 << 40);
        let caps = Capabilities::from_bits(raw);
        assert_eq!(caps.bits(), raw);
        assert_eq!(caps.unknown_bits(), 1 << 40);
        assert!(caps.contains(Capabilities::ALL));
        assert_eq!(Capabilities::ALL.unknown_bits(), 0);
        assert_eq!(Capabilities::from_bits(u64::MAX).iter().count(), 20);
    }

    #[test]
    fn debug_lists_names_and_leftovers() {
        assert_eq!(format!("{:?}", Capabilities::NONE), "Capabilities(NONE)");
        assert_eq!(
            format!(
                "{:?}",
                Capabilities::AUDIO_EFFECT | Capabilities::TAIL_INFINITE
            ),
            "Capabilities(AUDIO_EFFECT | TAIL_INFINITE)"
        );
        assert_eq!(
            format!("{:?}", Capabilities::from_bits(1 << 40)),
            "Capabilities(0x10000000000)"
        );
        assert!(
            format!("{:?}", Capabilities::from_bits(1 | (1 << 40)))
                .contains("AUDIO_EFFECT | 0x10000000000")
        );
    }
}
