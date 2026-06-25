//! Safe, owned descriptor / parameter types that a plugin author builds. These
//! are converted into the raw `#[repr(C)]` POD structs by `export.rs` when the
//! plugin is exported across the C ABI.

use daux_plugin_sys as sys;

/// Plugin category. Mirrors `daux_plugin_category`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Effect,
    Instrument,
    MidiFx,
    Analyzer,
}

impl Category {
    pub fn to_raw(self) -> sys::daux_plugin_category {
        match self {
            Category::Effect => sys::DAUX_CATEGORY_EFFECT,
            Category::Instrument => sys::DAUX_CATEGORY_INSTRUMENT,
            Category::MidiFx => sys::DAUX_CATEGORY_MIDI_FX,
            Category::Analyzer => sys::DAUX_CATEGORY_ANALYZER,
        }
    }
}

/// Capability flags. OR them together with `|`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Caps(pub i32);

impl Caps {
    pub const NONE: Caps = Caps(sys::DAUX_CAP_NONE);
    pub const EDITOR: Caps = Caps(sys::DAUX_CAP_EDITOR);
    pub const MIDI_INPUT: Caps = Caps(sys::DAUX_CAP_MIDI_INPUT);
    pub const MIDI_OUTPUT: Caps = Caps(sys::DAUX_CAP_MIDI_OUTPUT);
    pub const STATE: Caps = Caps(sys::DAUX_CAP_STATE);
    pub const PRESETS: Caps = Caps(sys::DAUX_CAP_PRESETS);
}

impl core::ops::BitOr for Caps {
    type Output = Caps;
    fn bitor(self, rhs: Caps) -> Caps {
        Caps(self.0 | rhs.0)
    }
}

/// A plugin's static descriptor (one per plugin type).
#[derive(Clone, Debug)]
pub struct PluginDescriptor {
    pub id: String,
    pub name: String,
    pub vendor: String,
    pub version: (u32, u32, u32),
    pub category: Category,
    pub caps: Caps,
    pub audio_input_buses: u32,
    pub audio_output_buses: u32,
    pub default_channels_per_bus: u32,
}

impl Default for PluginDescriptor {
    fn default() -> Self {
        PluginDescriptor {
            id: String::new(),
            name: String::new(),
            vendor: String::new(),
            version: (0, 1, 0),
            category: Category::Effect,
            caps: Caps::NONE,
            audio_input_buses: 1,
            audio_output_buses: 1,
            default_channels_per_bus: 2,
        }
    }
}

/// Parameter flags.
#[derive(Clone, Copy, Debug, Default)]
pub struct ParamFlags(pub i32);

impl ParamFlags {
    pub const NONE: ParamFlags = ParamFlags(sys::DAUX_PARAM_NONE);
    pub const AUTOMATABLE: ParamFlags = ParamFlags(sys::DAUX_PARAM_AUTOMATABLE);
    pub const STEPPED: ParamFlags = ParamFlags(sys::DAUX_PARAM_STEPPED);
    pub const READ_ONLY: ParamFlags = ParamFlags(sys::DAUX_PARAM_READ_ONLY);
    pub const IS_BYPASS: ParamFlags = ParamFlags(sys::DAUX_PARAM_IS_BYPASS);
}

impl core::ops::BitOr for ParamFlags {
    type Output = ParamFlags;
    fn bitor(self, rhs: ParamFlags) -> ParamFlags {
        ParamFlags(self.0 | rhs.0)
    }
}

/// Description of one parameter. Values are in plain (not normalized) units.
#[derive(Clone, Debug)]
pub struct ParamInfo {
    pub id: u32,
    pub name: String,
    pub short_name: String,
    pub unit: String,
    pub default: f64,
    pub min: f64,
    pub max: f64,
    pub step_count: i32,
    pub flags: ParamFlags,
}

impl ParamInfo {
    /// Convenience constructor for a continuous, automatable parameter.
    pub fn new(id: u32, name: &str, unit: &str, min: f64, max: f64, default: f64) -> Self {
        ParamInfo {
            id,
            name: name.to_string(),
            short_name: name.to_string(),
            unit: unit.to_string(),
            default,
            min,
            max,
            step_count: 0,
            flags: ParamFlags::AUTOMATABLE,
        }
    }
}
