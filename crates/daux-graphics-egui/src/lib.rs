//! egui editor backend for DAUx plug-ins.
//!
//! Wraps an [`egui::Context`] in [`DauxGraphic`](daux_graphics::DauxGraphic) and adds the
//! parameter-bound widgets a plug-in editor actually needs, so that an editor written against
//! `daux-graphics` can be drawn with egui without knowing anything about egui's frame
//! lifecycle.
//!
//! # The three pieces
//!
//! | Piece | What it does |
//! |---|---|
//! | [`InputTranslator`] | Turns host [`InputEvent`](daux_graphics::InputEvent)s into the [`egui::RawInput`] one frame is run with |
//! | [`EguiPainter`] | Turns the finished frame into pixels — supplied from outside, because this crate depends on no GPU API |
//! | [`ParamKnob`] and friends | Drive a [`ParamBinding`](daux_graphics::ParamBinding), so gesture bookkeeping is correct by construction |
//!
//! [`EguiEditor`] holds all three and implements the editor trait.
//!
//! # Why the painter is not in this crate
//!
//! egui produces shapes; something has to rasterise them, and that something needs a device.
//! Putting a renderer here would drag a GPU stack into every plug-in that uses egui, including
//! the ones running on a machine whose GPU driver is broken — which is the exact situation the
//! software fallback exists for. Instead, the renderer is a trait object supplied by the
//! plug-in: `daux-graphics-wgpu` on a working GPU, `daux-graphics-gl` where OpenGL is all
//! there is, or [`HeadlessPainter`] in a test.
//!
//! # Parameters
//!
//! The widgets never touch a [`Param`](daux_parameter::Param) directly. They go through
//! [`ParamBinding`](daux_graphics::ParamBinding), which owns the gesture state machine that
//! makes host automation record correctly — one `begin` per drag, one `end`, a value the host
//! hears *after* clamping, and an open gesture closed even if the editor is destroyed
//! mid-drag. See [`widgets`] for the interaction conventions.
//!
//! # Threading
//!
//! Everything here is `[main-thread]`. Nothing in this crate is `Send` or `Sync`, and nothing
//! in it is reachable from `process`: an editor communicates with the audio thread through the
//! parameter set and `daux-rt` channels, never by sharing a lock.
//!
//! # Example
//!
//! ```no_run
//! use daux_graphics::{DauxGraphic, LogicalSize, ParamBinding};
//! use daux_graphics_egui::{EguiEditor, HeadlessPainter, ParamKnob};
//! use daux_parameter::{FloatParam, ParamId, ParamRange};
//! use std::sync::Arc;
//!
//! let gain = Arc::new(FloatParam::new(
//!     ParamId(1),
//!     "Gain",
//!     0.0,
//!     ParamRange::Linear { min: -60.0, max: 12.0 },
//! ));
//!
//! let mut editor = EguiEditor::new(
//!     HeadlessPainter::new(),
//!     LogicalSize::new(320.0, 180.0),
//!     move |ui| {
//!         let binding = ParamBinding::new(gain.as_ref(), None);
//!         ui.add(ParamKnob::new(&binding).diameter(56.0));
//!     },
//! );
//! editor.tick();
//! ```

#![forbid(unsafe_code)]

mod convert;
mod editor;
mod input;
mod painter;
pub mod widgets;

pub use editor::EguiEditor;
pub use input::InputTranslator;
pub use painter::{EguiPainter, HeadlessPainter, profile};
pub use widgets::{
    ParamKnob, ParamSlider, ParamToggle, ParamValueEdit, param_knob, param_slider, param_toggle,
    param_value_edit,
};

/// The conversions between DAUx input and egui input.
///
/// Public because a plug-in that drives an [`egui::Context`] itself — bypassing [`EguiEditor`]
/// for a lifecycle this adapter does not cover — still needs them, and reimplementing the key
/// and modifier mapping is how `Ctrl` ends up doing nothing on macOS.
pub mod input_map {
    pub use crate::convert::{button, filter_text, key, modifiers, pos, scroll_delta};
}

/// Re-exported so a plug-in can name egui types without pinning its own copy of the crate.
///
/// A plug-in that depends on a *different* egui version than this backend gets two
/// incompatible `egui::Context` types and a confusing type error at the first `ui.add`; using
/// this re-export makes that impossible.
pub use egui;
