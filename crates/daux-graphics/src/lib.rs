//! Framework-neutral plug-in editor abstraction.
//!
//! This crate describes what a plug-in editor *is* without knowing how it draws. It depends
//! on no GUI toolkit and no GPU API, and it never will: `egui`, GPUI, `wgpu` and OpenGL live
//! behind the `daux-graphics-*` backend crates, each of which implements [`DauxGraphic`] in
//! its own way. A plug-in that only ever names types from this crate can change backends
//! without changing a line of its editor logic.
//!
//! # Three orthogonal axes
//!
//! A DAUx editor is described by three independent choices, not one:
//!
//! | Axis | Type | Examples |
//! |---|---|---|
//! | Which toolkit lays out and draws widgets | [`GraphicFramework`] | egui, GPUI, custom |
//! | What turns those widgets into pixels | [`GraphicRenderer`] | wgpu, OpenGL, software |
//! | How those pixels reach the host | [`PresentationMode`] | embedded surface, shared texture, … |
//!
//! One combination is a [`GraphicProfile`]; an editor advertises several as
//! [`GraphicCapabilities`] and the host picks one. Keeping the axes separate is what allows a
//! GPU-composited editor and a software-rendered fallback to be the same editor, and it is
//! why [`PresentationMode::EmbeddedSurface`] exists as a floor every host can meet.
//!
//! # Lifetime
//!
//! An editor's lifetime is independent of the processor's. It may be opened and closed many
//! times, or never opened at all, while audio runs throughout. See [`DauxGraphic`].
//!
//! # What is *not* here
//!
//! No widgets, no layout, no styling, no font handling. This crate is a boundary, not a
//! toolkit.

#![deny(unsafe_op_in_unsafe_fn)]

mod bitset;

mod binding;
mod capability;
mod descriptor;
mod error;
mod graphic;
mod input;
mod size;
mod texture;
mod window;

pub use binding::ParamBinding;
pub use capability::{
    GraphicCapabilities, GraphicFramework, GraphicFrameworkSet, GraphicProfile, GraphicRenderer,
    GraphicRendererSet, HostGraphicCaps, MAX_GRAPHIC_PROFILES, PresentationMode,
    PresentationModeSet,
};
pub use descriptor::GraphicDescriptor;
pub use error::{DauxGraphicResult, GraphicError, GraphicErrorKind};
pub use graphic::{DauxGraphic, GraphicContext};
pub use input::{InputEvent, InputResponse, Key, Modifiers, PointerButton};
pub use size::{
    LogicalPoint, LogicalSize, LogicalVector, PhysicalPoint, PhysicalSize, ScaleFactor,
};
pub use texture::{
    FenceKind, SharedTexture, SharedTextureAgreement, SharedTextureCaps, SharedTextureKind,
    SharedTexturePresenter, TextureFormat, negotiate_shared_texture,
};
pub use window::{WindowApi, WindowApiSet, WindowTarget};

/// Re-exported so a backend crate can name the handle types in [`WindowTarget`]'s
/// conversions without taking its own dependency on a matching version.
pub use raw_window_handle;
