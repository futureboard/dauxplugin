//! Example DAUx plugin in Rust: **"DAUx Gain (Rust)"**.
//!
//! Demonstrates the `daux-plugin` safe SDK end-to-end:
//!   - implements [`DauxPlugin`],
//!   - one automatable gain parameter in dB,
//!   - real (if minimal) DSP: planar float32 gain input -> output,
//!   - state round-trips the parameter,
//!   - exports the C ABI via `daux_export_plugin!`.
//!
//! Planned GUI: `DAUxGuiFramework::GPUI` with `DAUX_EDITOR_MODE_EXTERNAL`
//! (see Docs/plugin-gui-framework.md). Editor wiring is not implemented yet.

use daux_plugin::*;

const PARAM_GAIN: u32 = 1;
const GAIN_MIN_DB: f64 = -60.0;
const GAIN_MAX_DB: f64 = 12.0;
const GAIN_DEF_DB: f64 = 0.0;
const STATE_MAGIC: u32 = 0x4752_4144; // "DARG"

fn db_to_linear(db: f64) -> f32 {
    10f64.powf(db / 20.0) as f32
}

pub struct GainPlugin {
    gain_db: f64,
}

impl DauxPlugin for GainPlugin {
    fn descriptor() -> PluginDescriptor {
        PluginDescriptor {
            id: "com.daux.examples.gain.rust".to_string(),
            name: "DAUx Gain (Rust)".to_string(),
            vendor: "DAUx".to_string(),
            version: (0, 1, 0),
            category: Category::Effect,
            // STATE today; EDITOR will be added once the GPUI editor lands.
            caps: Caps::STATE,
            audio_input_buses: 1,
            audio_output_buses: 1,
            default_channels_per_bus: 2,
        }
    }

    fn parameters() -> Vec<ParamInfo> {
        vec![ParamInfo::new(
            PARAM_GAIN,
            "Gain",
            "dB",
            GAIN_MIN_DB,
            GAIN_MAX_DB,
            GAIN_DEF_DB,
        )]
    }

    fn new() -> Self {
        GainPlugin {
            gain_db: GAIN_DEF_DB,
        }
    }

    fn process(&mut self, _ctx: &ProcessContext, b: &mut Buffers) {
        let g = db_to_linear(self.gain_db);
        let frames = b.frames();
        for bus in 0..b.output_bus_count() {
            for ch in 0..b.output_channels(bus) {
                // Snapshot the matching input channel (immutable borrow) before
                // taking the mutable output borrow.
                let input: Option<Vec<f32>> = b.input(bus, ch).map(|s| s.to_vec());
                if let Some(out) = b.output(bus, ch) {
                    match &input {
                        Some(src) => {
                            for i in 0..frames {
                                out[i] = src.get(i).copied().unwrap_or(0.0) * g;
                            }
                        }
                        None => {
                            for v in out.iter_mut() {
                                *v = 0.0;
                            }
                        }
                    }
                }
            }
        }
    }

    fn get_param(&self, id: u32) -> f64 {
        if id == PARAM_GAIN {
            self.gain_db
        } else {
            0.0
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if id == PARAM_GAIN {
            self.gain_db = value.clamp(GAIN_MIN_DB, GAIN_MAX_DB);
        }
    }

    fn get_state(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(12);
        out.extend_from_slice(&STATE_MAGIC.to_le_bytes());
        out.extend_from_slice(&self.gain_db.to_le_bytes());
        out
    }

    fn set_state(&mut self, data: &[u8]) -> bool {
        if data.len() < 12 {
            return false;
        }
        let magic = u32::from_le_bytes(data[0..4].try_into().unwrap());
        if magic != STATE_MAGIC {
            return false;
        }
        let g = f64::from_le_bytes(data[4..12].try_into().unwrap());
        self.set_param(PARAM_GAIN, g);
        true
    }
}

// Export the C ABI for this plugin.
daux_export_plugin!(GainPlugin);

/// TODO: GPUI/DAUx editor scaffold.
///
/// The plan is for this module to implement a `daux_editor_vtable` backed by a
/// GPUI view that renders the gain knob and reports parameter changes back via
/// the host callbacks. Hosting/embedding happens through DAUxHost --mode=editor
/// (cross-process window reparenting). Until then the plugin advertises no
/// editor capability and runs headless.
pub mod editor {
    // pub struct GainEditor { /* gpui view, host callbacks ... */ }
    // impl GainEditor { /* attach/show/hide/set_bounds/idle */ }
}
