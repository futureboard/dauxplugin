//! wgpu rendering backend for DAUx plug-in editors.
//!
//! A **renderer, not a framework**. This crate creates a GPU device and a swapchain on the
//! window a host handed over, keeps them configured as that window is resized and moved
//! between displays, and hands out frames. What is drawn into those frames is somebody else's
//! business: a `daux-graphics-egui` painter, a plug-in's own pipelines, or a future
//! shared-texture presenter. There is no widget, no layout and no input handling here.
//!
//! # The three pieces
//!
//! | Piece | What it does |
//! |---|---|
//! | [`SurfaceConfig`] | What the plug-in asks for: adapter, vsync, format, alpha |
//! | [`GpuContext`] | Instance, adapter, device and queue, kept together because they must be |
//! | [`WgpuRenderer`] | A swapchain on the host's window, plus the acquire/present loop |
//!
//! # Everything is a preference
//!
//! A surface reports what it can do; a plug-in says what it would like. This crate picks the
//! nearest supported answer rather than failing, because an editor that refuses to open on a
//! machine whose swapchain has no `Immediate` present mode is worse than one that runs at
//! `Fifo`. The choosing is pure — see [`choose_format`], [`choose_present_mode`] and
//! [`choose_alpha_mode`] — which is why it is tested on machines with no GPU.
//!
//! # Window lifetime is the dangerous part
//!
//! A wgpu surface holds raw pointers into the host's window. `GraphicContext` documents that
//! window as valid from `open` until `close` returns, so a [`WgpuRenderer`] created in `open`
//! must be dropped no later than `close`. A renderer that outlives the window it was made from
//! crashes inside the graphics driver on the next present, with a stack trace that blames the
//! plug-in.
//!
//! # Building without a GPU
//!
//! This crate is a workspace member but **not** a default member, so plain `cargo build` and
//! `cargo test` never pull in a GPU stack. Its own GPU-dependent tests skip cleanly when no
//! adapter is present rather than failing, so a headless build machine stays green.
//!
//! # Threading
//!
//! Everything is `[main-thread]`. A swapchain belongs to the thread the host calls editors on,
//! and nothing here is reachable from `process`.
//!
//! # Example
//!
//! ```no_run
//! use daux_graphics::{PhysicalSize, WindowTarget};
//! use daux_graphics_wgpu::{SurfaceConfig, WgpuRenderer};
//!
//! # fn open(target: WindowTarget) -> Result<(), daux_graphics::GraphicError> {
//! let mut renderer = WgpuRenderer::new(target, PhysicalSize::new(800, 600), &SurfaceConfig::new())?;
//!
//! // Each frame, driven by the host's idle callback:
//! if let Some(frame) = renderer.acquire()? {
//!     // … record and submit commands that draw into `frame.view()` …
//!     renderer.present(frame);
//! }
//! # Ok(())
//! # }
//! ```

#![deny(unsafe_op_in_unsafe_fn)]

mod config;
mod renderer;
mod select;
mod target;

pub use config::{AlphaPreference, FormatPreference, SurfaceConfig, Vsync, capabilities, profile};
pub use renderer::{Frame, GpuContext, WgpuRenderer};
pub use select::{choose_alpha_mode, choose_format, choose_present_mode};
pub use target::surface_target;

/// Re-exported so a plug-in can name wgpu types — a `Device`, a `TextureFormat`, a
/// `RenderPipeline` — without pinning its own copy of the crate.
///
/// A plug-in that depends on a *different* wgpu version gets a second, incompatible `Device`
/// type and a type error at the first pipeline it tries to create with this renderer's device;
/// using this re-export makes that impossible.
pub use wgpu;
