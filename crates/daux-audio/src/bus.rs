//! Channel layouts and bus topology.
//!
//! These types are the Rust-side mirror of the `daux.audio-ports/1` extension
//! (`abi-v1` §11.1). They are plain owned data built on the main thread while a plug-in is
//! inactive; nothing here is meant to be touched from the audio thread.

use core::fmt;
use core::ops::{BitAnd, BitOr, BitOrAssign, Not};

use crate::error::{AudioError, AudioResult};

/// `DAUX_LAYOUT_*` codes (`abi-v1` §11.1).
pub mod layout_code {
    /// No layout information; interpret the channels as discrete.
    pub const UNKNOWN: u32 = 0;
    /// `MONO`.
    pub const MONO: u32 = 1;
    /// `STEREO`.
    pub const STEREO: u32 = 2;
    /// `L_R_C`.
    pub const L_R_C: u32 = 3;
    /// `QUAD`.
    pub const QUAD: u32 = 4;
    /// `SURROUND_2_1`.
    pub const SURROUND_2_1: u32 = 5;
    /// `SURROUND_5_1`.
    pub const SURROUND_5_1: u32 = 6;
    /// `SURROUND_7_1`.
    pub const SURROUND_7_1: u32 = 7;
    /// `ATMOS_7_1_4`.
    pub const ATMOS_7_1_4: u32 = 8;
    /// `AMBISONIC_1ST`.
    pub const AMBISONIC_1ST: u32 = 9;
    /// `AMBISONIC_2ND`.
    pub const AMBISONIC_2ND: u32 = 10;
    /// `AMBISONIC_3RD`.
    pub const AMBISONIC_3RD: u32 = 11;
    /// `DISCRETE`.
    pub const DISCRETE: u32 = 12;
    /// `CUSTOM`.
    pub const CUSTOM: u32 = 13;
}

/// `DAUX_PORT_PURPOSE_*` codes (`abi-v1` §11.1).
pub mod purpose_code {
    /// `MAIN`.
    pub const MAIN: u32 = 0;
    /// `AUX`.
    pub const AUX: u32 = 1;
    /// `SIDECHAIN`.
    pub const SIDECHAIN: u32 = 2;
    /// `MONITOR`.
    pub const MONITOR: u32 = 3;
    /// `ANALYSIS`.
    pub const ANALYSIS: u32 = 4;
    /// `REFERENCE`.
    pub const REFERENCE: u32 = 5;
    /// `CV`.
    pub const CV: u32 = 6;
    /// `CONTROL`.
    pub const CONTROL: u32 = 7;
}

/// Speaker arrangement of one bus. `[any-thread]`
///
/// The channel *order* of every named layout is fixed and given by [`channel_name`]; hosts
/// and adapters rely on it, so it is part of the contract, not an implementation detail.
///
/// [`channel_name`]: ChannelLayout::channel_name
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChannelLayout {
    /// 1 channel: `M`.
    Mono,
    /// 2 channels: `L R`.
    Stereo,
    /// 3 channels: `L R C`.
    #[allow(
        clippy::upper_case_acronyms,
        reason = "the cross-crate contract fixes this variant's spelling"
    )]
    LRC,
    /// 4 channels: `L R Ls Rs`.
    Quad,
    /// 3 channels: `L R LFE`.
    Surround2_1,
    /// 6 channels: `L R C LFE Ls Rs`.
    Surround5_1,
    /// 8 channels: `L R C LFE Ls Rs Lrs Rrs`.
    Surround7_1,
    /// 12 channels: 7.1 plus four height channels.
    Atmos7_1_4,
    /// 4 channels of first-order ambisonics (ACN/SN3D).
    Ambisonic1st,
    /// 9 channels of second-order ambisonics (ACN/SN3D).
    Ambisonic2nd,
    /// 16 channels of third-order ambisonics (ACN/SN3D).
    Ambisonic3rd,
    /// `n` unrelated channels with no spatial meaning.
    Discrete(u16),
    /// `n` channels whose arrangement is described out of band by the plug-in.
    Custom(u16),
}

impl ChannelLayout {
    /// Number of channels this layout carries. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn channel_count(self) -> u16 {
        match self {
            Self::Mono => 1,
            Self::Stereo => 2,
            Self::LRC | Self::Surround2_1 => 3,
            Self::Quad | Self::Ambisonic1st => 4,
            Self::Surround5_1 => 6,
            Self::Surround7_1 => 8,
            Self::Ambisonic2nd => 9,
            Self::Atmos7_1_4 => 12,
            Self::Ambisonic3rd => 16,
            Self::Discrete(n) | Self::Custom(n) => n,
        }
    }

    /// The `DAUX_LAYOUT_*` code for this layout. `[any-thread]`
    ///
    /// Named `as_bits` for symmetry with the rest of the SDK; the value is a small
    /// enumerated code, not a bit mask.
    #[inline]
    #[must_use]
    pub const fn as_bits(self) -> u32 {
        match self {
            Self::Mono => layout_code::MONO,
            Self::Stereo => layout_code::STEREO,
            Self::LRC => layout_code::L_R_C,
            Self::Quad => layout_code::QUAD,
            Self::Surround2_1 => layout_code::SURROUND_2_1,
            Self::Surround5_1 => layout_code::SURROUND_5_1,
            Self::Surround7_1 => layout_code::SURROUND_7_1,
            Self::Atmos7_1_4 => layout_code::ATMOS_7_1_4,
            Self::Ambisonic1st => layout_code::AMBISONIC_1ST,
            Self::Ambisonic2nd => layout_code::AMBISONIC_2ND,
            Self::Ambisonic3rd => layout_code::AMBISONIC_3RD,
            Self::Discrete(_) => layout_code::DISCRETE,
            Self::Custom(_) => layout_code::CUSTOM,
        }
    }

    /// Rebuilds a layout from an ABI `(layout, channel_count)` pair. `[any-thread]`
    ///
    /// The channel count is authoritative: a named code whose channel count disagrees with
    /// `channels` — and every code this ABI version does not define, including
    /// `DAUX_LAYOUT_UNKNOWN` — degrades to [`Discrete`], never to a wrong arrangement.
    ///
    /// [`Discrete`]: ChannelLayout::Discrete
    #[inline]
    #[must_use]
    pub const fn from_bits(bits: u32, channels: u16) -> Self {
        let named = match bits {
            layout_code::MONO => Self::Mono,
            layout_code::STEREO => Self::Stereo,
            layout_code::L_R_C => Self::LRC,
            layout_code::QUAD => Self::Quad,
            layout_code::SURROUND_2_1 => Self::Surround2_1,
            layout_code::SURROUND_5_1 => Self::Surround5_1,
            layout_code::SURROUND_7_1 => Self::Surround7_1,
            layout_code::ATMOS_7_1_4 => Self::Atmos7_1_4,
            layout_code::AMBISONIC_1ST => Self::Ambisonic1st,
            layout_code::AMBISONIC_2ND => Self::Ambisonic2nd,
            layout_code::AMBISONIC_3RD => Self::Ambisonic3rd,
            layout_code::CUSTOM => return Self::Custom(channels),
            _ => return Self::Discrete(channels),
        };
        if named.channel_count() == channels {
            named
        } else {
            Self::Discrete(channels)
        }
    }

    /// Short conventional name of channel `index`, or `None` when the layout has no fixed
    /// naming (ambisonic, discrete, custom) or the index is out of range. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn channel_name(self, index: u16) -> Option<&'static str> {
        let names: &'static [&'static str] = match self {
            Self::Mono => &["M"],
            Self::Stereo => &["L", "R"],
            Self::LRC => &["L", "R", "C"],
            Self::Quad => &["L", "R", "Ls", "Rs"],
            Self::Surround2_1 => &["L", "R", "LFE"],
            Self::Surround5_1 => &["L", "R", "C", "LFE", "Ls", "Rs"],
            Self::Surround7_1 => &["L", "R", "C", "LFE", "Ls", "Rs", "Lrs", "Rrs"],
            Self::Atmos7_1_4 => &[
                "L", "R", "C", "LFE", "Ls", "Rs", "Lrs", "Rrs", "Ltf", "Rtf", "Ltr", "Rtr",
            ],
            Self::Ambisonic1st
            | Self::Ambisonic2nd
            | Self::Ambisonic3rd
            | Self::Discrete(_)
            | Self::Custom(_) => return None,
        };
        if (index as usize) < names.len() {
            Some(names[index as usize])
        } else {
            None
        }
    }

    /// `true` for the ambisonic layouts. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn is_ambisonic(self) -> bool {
        matches!(
            self,
            Self::Ambisonic1st | Self::Ambisonic2nd | Self::Ambisonic3rd
        )
    }

    /// `true` when the layout has a documented speaker order. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn is_named(self) -> bool {
        !matches!(self, Self::Discrete(_) | Self::Custom(_))
    }
}

impl Default for ChannelLayout {
    #[inline]
    fn default() -> Self {
        Self::Stereo
    }
}

impl fmt::Display for ChannelLayout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Mono => f.write_str("mono"),
            Self::Stereo => f.write_str("stereo"),
            Self::LRC => f.write_str("L/R/C"),
            Self::Quad => f.write_str("quad"),
            Self::Surround2_1 => f.write_str("2.1"),
            Self::Surround5_1 => f.write_str("5.1"),
            Self::Surround7_1 => f.write_str("7.1"),
            Self::Atmos7_1_4 => f.write_str("7.1.4"),
            Self::Ambisonic1st => f.write_str("ambisonic-1"),
            Self::Ambisonic2nd => f.write_str("ambisonic-2"),
            Self::Ambisonic3rd => f.write_str("ambisonic-3"),
            Self::Discrete(n) => write!(f, "discrete-{n}"),
            Self::Custom(n) => write!(f, "custom-{n}"),
        }
    }
}

/// What a bus is for. `[any-thread]`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BusPurpose {
    /// The primary signal path.
    #[default]
    Main,
    /// An additional signal path of the same kind as the main bus.
    Aux,
    /// A key/detector input that is not mixed into the output.
    Sidechain,
    /// A cue/monitor path.
    Monitor,
    /// Metering or analysis only; may be ignored by the host.
    Analysis,
    /// A reference signal, e.g. for matching or A/B.
    Reference,
    /// Control voltage: audio-rate control data, not sound.
    Cv,
    /// Slow control data carried in an audio bus.
    Control,
}

impl BusPurpose {
    /// The `DAUX_PORT_PURPOSE_*` code. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn as_bits(self) -> u32 {
        match self {
            Self::Main => purpose_code::MAIN,
            Self::Aux => purpose_code::AUX,
            Self::Sidechain => purpose_code::SIDECHAIN,
            Self::Monitor => purpose_code::MONITOR,
            Self::Analysis => purpose_code::ANALYSIS,
            Self::Reference => purpose_code::REFERENCE,
            Self::Cv => purpose_code::CV,
            Self::Control => purpose_code::CONTROL,
        }
    }

    /// Parses a `DAUX_PORT_PURPOSE_*` code, returning `None` for values this ABI version
    /// does not define. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn from_bits(bits: u32) -> Option<Self> {
        match bits {
            purpose_code::MAIN => Some(Self::Main),
            purpose_code::AUX => Some(Self::Aux),
            purpose_code::SIDECHAIN => Some(Self::Sidechain),
            purpose_code::MONITOR => Some(Self::Monitor),
            purpose_code::ANALYSIS => Some(Self::Analysis),
            purpose_code::REFERENCE => Some(Self::Reference),
            purpose_code::CV => Some(Self::Cv),
            purpose_code::CONTROL => Some(Self::Control),
            _ => None,
        }
    }
}

/// `DAUX_PORT_FLAG_*` bit set for one bus. `[any-thread]`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct BusFlags(u32);

impl BusFlags {
    /// No flags.
    pub const NONE: Self = Self(0);
    /// `DAUX_PORT_FLAG_IS_MAIN` — the primary bus of its direction.
    pub const IS_MAIN: Self = Self(1 << 0);
    /// `DAUX_PORT_FLAG_OPTIONAL` — the host may deactivate this bus.
    pub const OPTIONAL: Self = Self(1 << 1);
    /// `DAUX_PORT_FLAG_CV` — carries control voltage rather than audio.
    pub const CV: Self = Self(1 << 2);
    /// `DAUX_PORT_FLAG_SUPPORTS_64` — this bus can be processed in `f64`.
    pub const SUPPORTS_64: Self = Self(1 << 3);
    /// Every flag defined by ABI v1.
    pub const ALL: Self = Self(0b1111);

    /// Raw `DAUX_PORT_FLAG_*` bits. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Builds a flag set from raw bits, dropping bits this ABI version does not define.
    /// `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn from_bits_truncate(bits: u32) -> Self {
        Self(bits & Self::ALL.0)
    }

    /// `true` when every flag in `other` is set. Always `true` for [`BusFlags::NONE`].
    /// `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// `true` when any flag in `other` is set. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    /// `true` when no flag is set. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Returns the set with `other` added. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Returns the set with `other` removed. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }
}

impl BitOr for BusFlags {
    type Output = Self;
    #[inline]
    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

impl BitOrAssign for BusFlags {
    #[inline]
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl BitAnd for BusFlags {
    type Output = Self;
    #[inline]
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

impl Not for BusFlags {
    type Output = Self;
    /// Complement within the flags ABI v1 defines; undefined bits stay clear.
    #[inline]
    fn not(self) -> Self {
        Self(!self.0 & Self::ALL.0)
    }
}

/// Description of one audio bus. `[main-thread]`
///
/// Constructing one allocates (the name is a `String`), so build bus layouts while the
/// plug-in is inactive and keep them out of `process`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BusInfo {
    /// Stable identifier, permanent across plug-in versions (`abi-v1` §11.1).
    pub id: u32,
    /// Human-readable name shown by the host.
    pub name: String,
    /// Speaker arrangement.
    pub layout: ChannelLayout,
    /// What the bus is for.
    pub purpose: BusPurpose,
    /// `DAUX_PORT_FLAG_*` bits.
    pub flags: BusFlags,
}

impl BusInfo {
    /// Creates a bus description. `[main-thread]` — allocates the name.
    #[must_use]
    pub fn new(id: u32, name: impl Into<String>, layout: ChannelLayout) -> Self {
        Self {
            id,
            name: name.into(),
            layout,
            purpose: BusPurpose::Main,
            flags: BusFlags::NONE,
        }
    }

    /// Convenience for the usual case: id `0`, main purpose, [`BusFlags::IS_MAIN`].
    /// `[main-thread]`
    #[must_use]
    pub fn main(name: impl Into<String>, layout: ChannelLayout) -> Self {
        Self::new(0, name, layout)
            .with_purpose(BusPurpose::Main)
            .with_flags(BusFlags::IS_MAIN)
    }

    /// Sets the purpose. `[main-thread]`
    #[must_use]
    pub fn with_purpose(mut self, purpose: BusPurpose) -> Self {
        self.purpose = purpose;
        self
    }

    /// Replaces the flags. `[main-thread]`
    #[must_use]
    pub fn with_flags(mut self, flags: BusFlags) -> Self {
        self.flags = flags;
        self
    }

    /// Adds flags to the existing set. `[main-thread]`
    #[must_use]
    pub fn add_flags(mut self, flags: BusFlags) -> Self {
        self.flags |= flags;
        self
    }

    /// Number of channels on this bus. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn channel_count(&self) -> u16 {
        self.layout.channel_count()
    }

    /// `true` when [`BusFlags::IS_MAIN`] is set. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn is_main(&self) -> bool {
        self.flags.contains(BusFlags::IS_MAIN)
    }

    /// `true` when the host may switch this bus off. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn is_optional(&self) -> bool {
        self.flags.contains(BusFlags::OPTIONAL)
    }
}

/// The complete input/output bus topology of a plug-in. `[main-thread]`
///
/// Bus order is significant and is the order the host sees; index `0` of each direction is
/// the main bus by convention, and the plug-in should also mark it [`BusFlags::IS_MAIN`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct BusLayout {
    /// Input buses, in host order.
    pub inputs: Vec<BusInfo>,
    /// Output buses, in host order.
    pub outputs: Vec<BusInfo>,
}

impl BusLayout {
    /// An empty topology. `[main-thread]`
    #[must_use]
    pub const fn new() -> Self {
        Self {
            inputs: Vec::new(),
            outputs: Vec::new(),
        }
    }

    /// One main input and one main output with the same layout — the shape of almost every
    /// effect. `[main-thread]` — allocates.
    #[must_use]
    pub fn effect(layout: ChannelLayout) -> Self {
        Self {
            inputs: vec![BusInfo::main("Input", layout)],
            outputs: vec![BusInfo::main("Output", layout)],
        }
    }

    /// Stereo in, stereo out. `[main-thread]` — allocates.
    #[must_use]
    pub fn stereo_effect() -> Self {
        Self::effect(ChannelLayout::Stereo)
    }

    /// Mono in, mono out. `[main-thread]` — allocates.
    #[must_use]
    pub fn mono_effect() -> Self {
        Self::effect(ChannelLayout::Mono)
    }

    /// No audio input, one main output — the shape of an instrument. `[main-thread]`
    #[must_use]
    pub fn instrument(layout: ChannelLayout) -> Self {
        Self {
            inputs: Vec::new(),
            outputs: vec![BusInfo::main("Output", layout)],
        }
    }

    /// Appends an input bus. `[main-thread]`
    #[must_use]
    pub fn with_input(mut self, bus: BusInfo) -> Self {
        self.inputs.push(bus);
        self
    }

    /// Appends an output bus. `[main-thread]`
    #[must_use]
    pub fn with_output(mut self, bus: BusInfo) -> Self {
        self.outputs.push(bus);
        self
    }

    /// The first input flagged [`BusFlags::IS_MAIN`], else input `0`. `[any-thread]`
    #[must_use]
    pub fn main_input(&self) -> Option<&BusInfo> {
        self.inputs
            .iter()
            .find(|b| b.is_main())
            .or_else(|| self.inputs.first())
    }

    /// The first output flagged [`BusFlags::IS_MAIN`], else output `0`. `[any-thread]`
    #[must_use]
    pub fn main_output(&self) -> Option<&BusInfo> {
        self.outputs
            .iter()
            .find(|b| b.is_main())
            .or_else(|| self.outputs.first())
    }

    /// Looks an input bus up by its stable id. `[any-thread]`
    #[must_use]
    pub fn input_by_id(&self, id: u32) -> Option<&BusInfo> {
        self.inputs.iter().find(|b| b.id == id)
    }

    /// Looks an output bus up by its stable id. `[any-thread]`
    #[must_use]
    pub fn output_by_id(&self, id: u32) -> Option<&BusInfo> {
        self.outputs.iter().find(|b| b.id == id)
    }

    /// Total number of input channels across all buses. `[any-thread]`
    #[must_use]
    pub fn total_input_channels(&self) -> usize {
        self.inputs
            .iter()
            .map(|b| usize::from(b.channel_count()))
            .sum()
    }

    /// Total number of output channels across all buses. `[any-thread]`
    #[must_use]
    pub fn total_output_channels(&self) -> usize {
        self.outputs
            .iter()
            .map(|b| usize::from(b.channel_count()))
            .sum()
    }

    /// `true` when there is no bus at all in either direction. `[any-thread]`
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inputs.is_empty() && self.outputs.is_empty()
    }

    /// Checks the invariants a host may rely on: ids are unique within a direction and at
    /// most one bus per direction is flagged [`BusFlags::IS_MAIN`]. `[main-thread]`
    ///
    /// # Errors
    ///
    /// [`AudioError::DuplicateBusId`] or [`AudioError::MultipleMainBuses`].
    pub fn validate(&self) -> AudioResult<()> {
        validate_direction(&self.inputs)?;
        validate_direction(&self.outputs)
    }
}

fn validate_direction(buses: &[BusInfo]) -> AudioResult<()> {
    let mut mains = 0usize;
    for (i, bus) in buses.iter().enumerate() {
        if bus.is_main() {
            mains += 1;
            if mains > 1 {
                return Err(AudioError::MultipleMainBuses);
            }
        }
        if buses[..i].iter().any(|b| b.id == bus.id) {
            return Err(AudioError::DuplicateBusId(bus.id));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_counts_are_exhaustive_and_correct() {
        let cases = [
            (ChannelLayout::Mono, 1),
            (ChannelLayout::Stereo, 2),
            (ChannelLayout::LRC, 3),
            (ChannelLayout::Surround2_1, 3),
            (ChannelLayout::Quad, 4),
            (ChannelLayout::Ambisonic1st, 4),
            (ChannelLayout::Surround5_1, 6),
            (ChannelLayout::Surround7_1, 8),
            (ChannelLayout::Ambisonic2nd, 9),
            (ChannelLayout::Atmos7_1_4, 12),
            (ChannelLayout::Ambisonic3rd, 16),
            (ChannelLayout::Discrete(0), 0),
            (ChannelLayout::Discrete(u16::MAX), u16::MAX),
            (ChannelLayout::Custom(64), 64),
        ];
        for (layout, count) in cases {
            assert_eq!(layout.channel_count(), count, "{layout}");
        }
    }

    #[test]
    fn layout_codes_match_the_abi_table() {
        assert_eq!(ChannelLayout::Mono.as_bits(), 1);
        assert_eq!(ChannelLayout::Stereo.as_bits(), 2);
        assert_eq!(ChannelLayout::LRC.as_bits(), 3);
        assert_eq!(ChannelLayout::Quad.as_bits(), 4);
        assert_eq!(ChannelLayout::Surround2_1.as_bits(), 5);
        assert_eq!(ChannelLayout::Surround5_1.as_bits(), 6);
        assert_eq!(ChannelLayout::Surround7_1.as_bits(), 7);
        assert_eq!(ChannelLayout::Atmos7_1_4.as_bits(), 8);
        assert_eq!(ChannelLayout::Ambisonic1st.as_bits(), 9);
        assert_eq!(ChannelLayout::Ambisonic2nd.as_bits(), 10);
        assert_eq!(ChannelLayout::Ambisonic3rd.as_bits(), 11);
        assert_eq!(ChannelLayout::Discrete(3).as_bits(), 12);
        assert_eq!(ChannelLayout::Custom(3).as_bits(), 13);
    }

    #[test]
    fn layout_round_trips_through_the_abi() {
        let all = [
            ChannelLayout::Mono,
            ChannelLayout::Stereo,
            ChannelLayout::LRC,
            ChannelLayout::Quad,
            ChannelLayout::Surround2_1,
            ChannelLayout::Surround5_1,
            ChannelLayout::Surround7_1,
            ChannelLayout::Atmos7_1_4,
            ChannelLayout::Ambisonic1st,
            ChannelLayout::Ambisonic2nd,
            ChannelLayout::Ambisonic3rd,
            ChannelLayout::Discrete(5),
            ChannelLayout::Custom(5),
        ];
        for layout in all {
            let back = ChannelLayout::from_bits(layout.as_bits(), layout.channel_count());
            assert_eq!(back, layout, "{layout}");
        }
    }

    #[test]
    fn from_bits_distrusts_a_lying_host() {
        // Named code with the wrong channel count degrades instead of lying.
        assert_eq!(
            ChannelLayout::from_bits(layout_code::STEREO, 5),
            ChannelLayout::Discrete(5)
        );
        // Unknown and reserved codes degrade too.
        assert_eq!(
            ChannelLayout::from_bits(layout_code::UNKNOWN, 2),
            ChannelLayout::Discrete(2)
        );
        assert_eq!(
            ChannelLayout::from_bits(u32::MAX, 0),
            ChannelLayout::Discrete(0)
        );
        // CUSTOM always keeps whatever count the host reported.
        assert_eq!(
            ChannelLayout::from_bits(layout_code::CUSTOM, 999),
            ChannelLayout::Custom(999)
        );
    }

    #[test]
    fn channel_names_cover_the_named_layouts() {
        assert_eq!(ChannelLayout::Stereo.channel_name(0), Some("L"));
        assert_eq!(ChannelLayout::Stereo.channel_name(1), Some("R"));
        assert_eq!(ChannelLayout::Stereo.channel_name(2), None);
        assert_eq!(ChannelLayout::Surround5_1.channel_name(3), Some("LFE"));
        assert_eq!(ChannelLayout::Atmos7_1_4.channel_name(11), Some("Rtr"));
        assert_eq!(ChannelLayout::Atmos7_1_4.channel_name(12), None);
        assert_eq!(ChannelLayout::Ambisonic1st.channel_name(0), None);
        assert_eq!(ChannelLayout::Discrete(4).channel_name(0), None);
        assert_eq!(ChannelLayout::Mono.channel_name(u16::MAX), None);

        // Every named layout names every one of its channels.
        for layout in [
            ChannelLayout::Mono,
            ChannelLayout::Stereo,
            ChannelLayout::LRC,
            ChannelLayout::Quad,
            ChannelLayout::Surround2_1,
            ChannelLayout::Surround5_1,
            ChannelLayout::Surround7_1,
            ChannelLayout::Atmos7_1_4,
        ] {
            for c in 0..layout.channel_count() {
                assert!(layout.channel_name(c).is_some(), "{layout} channel {c}");
            }
        }
    }

    #[test]
    fn layout_predicates() {
        assert!(ChannelLayout::Ambisonic2nd.is_ambisonic());
        assert!(!ChannelLayout::Stereo.is_ambisonic());
        assert!(ChannelLayout::Stereo.is_named());
        assert!(!ChannelLayout::Custom(2).is_named());
        assert_eq!(ChannelLayout::default(), ChannelLayout::Stereo);
    }

    #[test]
    fn purpose_round_trips() {
        let all = [
            BusPurpose::Main,
            BusPurpose::Aux,
            BusPurpose::Sidechain,
            BusPurpose::Monitor,
            BusPurpose::Analysis,
            BusPurpose::Reference,
            BusPurpose::Cv,
            BusPurpose::Control,
        ];
        for (i, purpose) in all.into_iter().enumerate() {
            assert_eq!(purpose.as_bits(), i as u32);
            assert_eq!(BusPurpose::from_bits(purpose.as_bits()), Some(purpose));
        }
        assert_eq!(BusPurpose::from_bits(8), None);
        assert_eq!(BusPurpose::from_bits(u32::MAX), None);
        assert_eq!(BusPurpose::default(), BusPurpose::Main);
    }

    #[test]
    fn flag_algebra() {
        let f = BusFlags::IS_MAIN | BusFlags::SUPPORTS_64;
        assert_eq!(f.bits(), 0b1001);
        assert!(f.contains(BusFlags::IS_MAIN));
        assert!(f.contains(BusFlags::NONE));
        assert!(!f.contains(BusFlags::CV));
        assert!(f.intersects(BusFlags::SUPPORTS_64 | BusFlags::CV));
        assert!(!f.intersects(BusFlags::OPTIONAL));
        assert!(BusFlags::NONE.is_empty());
        assert!(!f.is_empty());
        assert_eq!(f.without(BusFlags::IS_MAIN), BusFlags::SUPPORTS_64);
        assert_eq!(f & BusFlags::IS_MAIN, BusFlags::IS_MAIN);
        assert_eq!(!BusFlags::ALL, BusFlags::NONE);
        assert_eq!(!BusFlags::NONE, BusFlags::ALL);
        // Undefined bits never survive a round trip through the ABI.
        assert_eq!(BusFlags::from_bits_truncate(u32::MAX), BusFlags::ALL);
        assert_eq!(BusFlags::from_bits_truncate(1 << 31), BusFlags::NONE);
        let mut g = BusFlags::NONE;
        g |= BusFlags::CV;
        assert_eq!(g, BusFlags::CV);
    }

    #[test]
    fn bus_info_builders() {
        let bus = BusInfo::main("Output", ChannelLayout::Surround5_1)
            .add_flags(BusFlags::SUPPORTS_64)
            .with_purpose(BusPurpose::Main);
        assert_eq!(bus.id, 0);
        assert_eq!(bus.name, "Output");
        assert_eq!(bus.channel_count(), 6);
        assert!(bus.is_main());
        assert!(!bus.is_optional());
        assert!(bus.flags.contains(BusFlags::SUPPORTS_64));

        let side = BusInfo::new(1, "Sidechain", ChannelLayout::Stereo)
            .with_purpose(BusPurpose::Sidechain)
            .with_flags(BusFlags::OPTIONAL);
        assert!(!side.is_main());
        assert!(side.is_optional());
        assert_eq!(side.purpose, BusPurpose::Sidechain);
    }

    #[test]
    fn layout_helpers() {
        let l = BusLayout::stereo_effect();
        assert_eq!(l.total_input_channels(), 2);
        assert_eq!(l.total_output_channels(), 2);
        assert_eq!(l.main_input().unwrap().name, "Input");
        assert_eq!(l.main_output().unwrap().name, "Output");
        assert!(!l.is_empty());
        l.validate().unwrap();

        assert_eq!(BusLayout::mono_effect().total_input_channels(), 1);

        let inst = BusLayout::instrument(ChannelLayout::Stereo);
        assert!(inst.inputs.is_empty());
        assert!(inst.main_input().is_none());
        assert_eq!(inst.total_input_channels(), 0);
        inst.validate().unwrap();

        assert!(BusLayout::new().is_empty());
        assert_eq!(BusLayout::default(), BusLayout::new());
        BusLayout::new().validate().unwrap();
    }

    #[test]
    fn layout_lookup_and_validation() {
        let l = BusLayout::new()
            .with_input(BusInfo::main("In", ChannelLayout::Stereo))
            .with_input(
                BusInfo::new(1, "Side", ChannelLayout::Mono).with_purpose(BusPurpose::Sidechain),
            )
            .with_output(BusInfo::main("Out", ChannelLayout::Stereo));
        l.validate().unwrap();
        assert_eq!(l.input_by_id(1).unwrap().name, "Side");
        assert!(l.input_by_id(7).is_none());
        assert_eq!(l.output_by_id(0).unwrap().name, "Out");
        assert!(l.output_by_id(1).is_none());
        assert_eq!(l.total_input_channels(), 3);
        // The main input is the flagged one even though it is not first in the list.
        let reordered = BusLayout::new()
            .with_input(BusInfo::new(9, "Side", ChannelLayout::Mono))
            .with_input(BusInfo::main("In", ChannelLayout::Stereo));
        assert_eq!(reordered.main_input().unwrap().name, "In");
    }

    #[test]
    fn validation_rejects_broken_topologies() {
        let dup = BusLayout::new()
            .with_input(BusInfo::new(4, "a", ChannelLayout::Mono))
            .with_input(BusInfo::new(4, "b", ChannelLayout::Mono));
        assert_eq!(dup.validate(), Err(AudioError::DuplicateBusId(4)));

        let two_mains = BusLayout::new()
            .with_output(BusInfo::main("a", ChannelLayout::Mono))
            .with_output(BusInfo::new(1, "b", ChannelLayout::Mono).with_flags(BusFlags::IS_MAIN));
        assert_eq!(two_mains.validate(), Err(AudioError::MultipleMainBuses));

        // The same id in *different* directions is legal.
        let ok = BusLayout::new()
            .with_input(BusInfo::main("in", ChannelLayout::Mono))
            .with_output(BusInfo::main("out", ChannelLayout::Mono));
        ok.validate().unwrap();
    }
}
