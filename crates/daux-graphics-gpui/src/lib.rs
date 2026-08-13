//! GPUI editor backend for DAUx plug-ins.
//!
//! Wraps [`gpui_embedded::plugin::PluginEditor`] in [`DauxGraphic`](daux_graphics::DauxGraphic)
//! so a plug-in written against `daux-graphics` can render its editor with GPUI without
//! knowing anything about GPUI's lifecycle.
//!
//! # Why the fork
//!
//! This builds on [`futureboard/gpui-se`](https://github.com/futureboard/gpui-se) rather than
//! crates.io `gpui`. Upstream GPUI assumes it *is* the application: it creates the windows,
//! owns the event loop, keeps global state, and calls `exit` when the last window closes.
//! Every one of those is fatal in a plug-in, where the host owns the window, the host pumps
//! the events, a DAW routinely runs dozens of instances in one process, and `exit` takes the
//! user's session with it. The fork's `gpui_embedded` crate is a GPUI platform built for
//! exactly this: no windows, no event loop, no globals, no `exit`.
//!
//! # The three jobs the host takes on
//!
//! In exchange for GPUI not owning the loop, the editor's host — here, the DAW, through
//! `daux-graphics` — must:
//!
//! 1. **Tick.** [`DauxGraphic::tick`](daux_graphics::DauxGraphic::tick) drives
//!    [`PluginEditor::idle`](gpui_embedded::plugin::PluginEditor::idle). Nothing runs
//!    otherwise: no layout, no painting, no async tasks.
//! 2. **Report.** Input arrives through
//!    [`on_input`](daux_graphics::DauxGraphic::on_input), and size and scale changes through
//!    [`resize`](daux_graphics::DauxGraphic::resize) and
//!    [`scale_factor_changed`](daux_graphics::DauxGraphic::scale_factor_changed).
//! 3. **Serve.** Clipboard, cursor shape and URL requests reach the system through an
//!    [`EmbeddedHost`](gpui_embedded::EmbeddedHost), supplied via
//!    [`GpuiEditor::with_builder`].
//!
//! # The audio thread
//!
//! Never touches any of this. Move data across with the primitives in
//! [`gpui_embedded::audio`] or with a `daux-rt` channel; a `Mutex` shared with the editor is
//! a real-time bug even when it is uncontended.
//!
//! # Example
//!
//! ```no_run
//! use daux_graphics::LogicalSize;
//! use daux_graphics_gpui::GpuiEditor;
//! use gpui::{AppContext, Context, IntoElement, ParentElement, Render, Window, div};
//!
//! struct Panel;
//!
//! impl Render for Panel {
//!     fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
//!         div().child("hello from a plug-in editor")
//!     }
//! }
//!
//! let editor = GpuiEditor::new(LogicalSize::new(700.0, 400.0), |_window, cx| {
//!     cx.new(|_| Panel)
//! });
//! ```

#![deny(unsafe_op_in_unsafe_fn)]

mod convert;
mod editor;

pub use editor::{GpuiEditor, profile};

/// The conversions between DAUx input and GPUI input.
///
/// Public because a plug-in that drives `gpui_embedded` directly — bypassing [`GpuiEditor`]
/// for a lifecycle this adapter does not cover — still needs them, and reimplementing the
/// keystroke mapping is how the modifier order ends up wrong.
pub mod input {
    pub use crate::convert::{button, key_name, keystroke, modifiers, scale, to_host_events};
}

/// Re-exported so a plug-in can name GPUI types without pinning its own copy of the fork.
pub use {gpui, gpui_embedded};
