//! OpenGL and software fallback rendering for DAUx plug-in editors.
//!
//! This is the path that works when nothing else does. A DAW runs on whatever machine the user
//! has: an old laptop with a GL 3.3 driver, a remote desktop session with no GPU acceleration
//! at all, a VM whose Vulkan loader is broken. An editor that only knows how to render through
//! a modern GPU API is invisible on all of them, which is why `daux-graphics` makes an
//! embedded surface the mandatory fallback and why this crate exists to fill it.
//!
//! # The two profiles
//!
//! | Profile | What it needs | What it does |
//! |---|---|---|
//! | `custom+opengl on embedded-surface` | A GL 3.1 / ES 3.0 context from the host | Uploads the frame and blits it with one draw call |
//! | `custom+software on embedded-surface` | Nothing | Hands the frame to whatever the host supplies |
//!
//! Both draw the same pixels: an editor rasterises into a [`SoftwareFramebuffer`] and a
//! [`Presenter`] puts it on screen. Swapping the presenter changes nothing about the editor.
//!
//! # What this crate does not do
//!
//! **It does not create GL contexts.** `glow` loads entry points; creating a context is
//! platform work — WGL, CGL, GLX, EGL — that needs the host's window handle and a pile of
//! platform crates. A host or plug-in supplies that through [`GlSurface`], typically over
//! `glutin`, and [`GlPresenter`] does everything downstream of it.
//!
//! **It is not a drawing library.** There is no path rasteriser here, no text layout, no
//! widgets. What goes into the framebuffer is the plug-in's business; `daux-graphics-egui`
//! is one thing that can fill it.
//!
//! # Testing without a GPU
//!
//! [`NullPresenter`] runs the whole editor lifecycle and discards the pixels, so editor logic
//! is testable on a headless machine — which is what the tests in this crate do. The GL calls
//! themselves need a current context and cannot be exercised that way; what *is* checked is
//! everything around them: the version parsing that picks a GLSL dialect, the viewport
//! arithmetic, the framebuffer's bounds and its ceiling, and the shader text.
//!
//! # Threading
//!
//! Everything is `[main-thread]`. A GL context belongs to one thread, and every method here
//! assumes it is the one the host calls editors on. Nothing in this crate is reachable from
//! `process`.

#![deny(unsafe_op_in_unsafe_fn)]

mod blit;
mod editor;
mod framebuffer;
mod present;
mod version;
mod viewport;

pub use blit::{GlBlitter, shader_source};
pub use editor::{FrameInfo, SoftwareEditor};
pub use framebuffer::{BYTES_PER_PIXEL, MAX_PIXELS, SoftwareFramebuffer};
pub use present::{GlPresenter, GlSurface, NullPresenter, Presenter, profile};
pub use version::GlVersion;
pub use viewport::{Scaling, Viewport};

/// Re-exported so a plug-in can name `glow` types — chiefly [`glow::Context`], which a
/// [`GlSurface`] implementation must hand back — without pinning its own copy of the crate.
///
/// A plug-in that depends on a *different* `glow` version gets a second, incompatible
/// `Context` type and a type error at the first `impl GlSurface`; using this re-export makes
/// that impossible.
pub use glow;
