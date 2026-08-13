//! Putting a [`SoftwareFramebuffer`] on screen with OpenGL.
//!
//! Everything here needs a **current** GL context on the calling thread. `glow` does not
//! create contexts — that is platform work (WGL, CGL, GLX, EGL, or a crate like `glutin`) —
//! so this module takes one and uses it, and the crate's [`GlSurface`](crate::GlSurface) trait
//! is where a host says how to make it current and how to swap buffers.
//!
//! # Why a fullscreen triangle
//!
//! One triangle that overhangs the viewport covers the same pixels as two, with no shared
//! edge down the diagonal to produce a seam, and its vertices are computed from `gl_VertexID`
//! — so there is no vertex buffer, no attribute layout and nothing to keep in sync between
//! the Rust side and the shader. The whole blit is one draw call with three vertices.

use daux_graphics::{DauxGraphicResult, GraphicError, GraphicErrorKind, PhysicalSize};
use glow::HasContext as _;

use crate::{GlVersion, SoftwareFramebuffer, Viewport};

/// The vertex shader body, appended to the version header [`GlVersion::glsl_header`] chose.
///
/// `v` walks `(0,0) → (2,0) → (0,2)` in clip-space units, giving a triangle that covers the
/// whole `[-1,1]` square with room to spare. The texture coordinate flips `y`, because a
/// [`SoftwareFramebuffer`] is stored top row first and OpenGL samples from the bottom up.
const VERTEX_BODY: &str = "
out vec2 v_uv;
void main() {
    vec2 v = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
    v_uv = vec2(v.x, 1.0 - v.y);
    gl_Position = vec4(v * 2.0 - 1.0, 0.0, 1.0);
}
";

/// The fragment shader body: one texture fetch, no filtering decisions, no colour management.
const FRAGMENT_BODY: &str = "
in vec2 v_uv;
out vec4 o_color;
uniform sampler2D u_image;
void main() {
    o_color = texture(u_image, v_uv);
}
";

/// The name of the sampler uniform the blit program declares.
const SAMPLER_UNIFORM: &str = "u_image";

/// [main-thread] The shader source for one stage, with the right `#version` for `version`.
///
/// Public so a host can log or pre-validate exactly what will be compiled, which is the
/// difference between a bug report that says "the editor is black" and one that says which
/// line of GLSL the driver rejected.
///
/// # Errors
///
/// [`GraphicErrorKind::Unsupported`] when the context is too old for these shaders; see
/// [`GlVersion::glsl_header`].
pub fn shader_source(version: GlVersion, fragment: bool) -> DauxGraphicResult<String> {
    let header = version.glsl_header().ok_or_else(|| {
        GraphicError::new(
            GraphicErrorKind::Unsupported,
            format!("{version} is older than the GLSL 1.40 / ES 3.00 floor this backend needs"),
        )
    })?;
    let body = if fragment { FRAGMENT_BODY } else { VERTEX_BODY };
    Ok(format!("{header}{body}"))
}

/// The GL objects that draw a [`SoftwareFramebuffer`] over a viewport.
///
/// # Ownership
///
/// A blitter holds a program, a vertex array object and a texture, all belonging to the
/// context it was created with. Dropping it does **not** free them: GL objects can only be
/// deleted from a thread with that context current, and `Drop` has no way to get one. Call
/// [`destroy`](Self::destroy) instead; failing to is a leak that lasts as long as the context,
/// which for a plug-in editor means until the host's window goes away.
///
/// [main-thread]
pub struct GlBlitter {
    program: glow::Program,
    vertex_array: Option<glow::VertexArray>,
    texture: glow::Texture,
    sampler: Option<glow::UniformLocation>,
    /// The size the texture was last allocated at, so an unchanged size uploads with
    /// `glTexSubImage2D` instead of reallocating the whole texture every frame.
    allocated: PhysicalSize,
}

impl core::fmt::Debug for GlBlitter {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("GlBlitter")
            .field("allocated", &self.allocated)
            .field("has_vertex_array", &self.vertex_array.is_some())
            .finish_non_exhaustive()
    }
}

impl GlBlitter {
    /// [main-thread] Compiles the blit program and creates its texture.
    ///
    /// # Errors
    ///
    /// [`GraphicErrorKind::Unsupported`] for a context too old to run the shaders, and
    /// [`GraphicErrorKind::Resource`] when the driver refuses to create or link something —
    /// with the driver's own info log in the message, which is the only useful thing to put
    /// in a bug report about a shader that will not compile on one particular GPU.
    ///
    /// # Safety
    ///
    /// `gl` must be a live context that is **current on the calling thread**, and must stay
    /// current for every later call on the returned blitter.
    pub unsafe fn new(gl: &glow::Context, version: GlVersion) -> DauxGraphicResult<Self> {
        let vertex_source = shader_source(version, false)?;
        let fragment_source = shader_source(version, true)?;

        // SAFETY: the caller guarantees `gl` is current on this thread. Every object created
        // below is either handed to the returned blitter or deleted before the early return
        // that abandoned it, so nothing is leaked on a failure path.
        unsafe {
            let program = gl
                .create_program()
                .map_err(|e| resource_error("could not create a GL program", &e))?;

            let stages = [
                (glow::VERTEX_SHADER, vertex_source),
                (glow::FRAGMENT_SHADER, fragment_source),
            ];
            let mut shaders = Vec::with_capacity(stages.len());
            for (kind, source) in stages {
                let shader = match gl.create_shader(kind) {
                    Ok(shader) => shader,
                    Err(e) => {
                        delete_shaders(gl, &shaders);
                        gl.delete_program(program);
                        return Err(resource_error("could not create a GL shader", &e));
                    }
                };
                gl.shader_source(shader, &source);
                gl.compile_shader(shader);
                if !gl.get_shader_compile_status(shader) {
                    let log = gl.get_shader_info_log(shader);
                    gl.delete_shader(shader);
                    delete_shaders(gl, &shaders);
                    gl.delete_program(program);
                    return Err(resource_error("the blit shader did not compile", &log));
                }
                gl.attach_shader(program, shader);
                shaders.push(shader);
            }

            gl.link_program(program);
            // Detaching before deleting is what actually releases the shader objects; a
            // shader that is still attached lives as long as the program does.
            for shader in &shaders {
                gl.detach_shader(program, *shader);
            }
            delete_shaders(gl, &shaders);

            if !gl.get_program_link_status(program) {
                let log = gl.get_program_info_log(program);
                gl.delete_program(program);
                return Err(resource_error("the blit program did not link", &log));
            }

            let texture = match gl.create_texture() {
                Ok(texture) => texture,
                Err(e) => {
                    gl.delete_program(program);
                    return Err(resource_error("could not create the blit texture", &e));
                }
            };
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::LINEAR as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                glow::LINEAR as i32,
            );
            // Clamping matters: with the default `GL_REPEAT`, the linear filter samples across
            // the wrap at the outer half-pixel and draws a line of the opposite edge's colour
            // along every side of the editor.
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_S,
                glow::CLAMP_TO_EDGE as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_T,
                glow::CLAMP_TO_EDGE as i32,
            );
            gl.bind_texture(glow::TEXTURE_2D, None);

            // A core-profile context refuses to draw without a vertex array object bound,
            // even for a draw that reads no attributes at all.
            let vertex_array = if version.has_vertex_array_objects() {
                match gl.create_vertex_array() {
                    Ok(vao) => Some(vao),
                    Err(e) => {
                        gl.delete_texture(texture);
                        gl.delete_program(program);
                        return Err(resource_error("could not create a vertex array", &e));
                    }
                }
            } else {
                None
            };

            let sampler = gl.get_uniform_location(program, SAMPLER_UNIFORM);

            Ok(Self {
                program,
                vertex_array,
                texture,
                sampler,
                allocated: PhysicalSize::ZERO,
            })
        }
    }

    /// [main-thread] Copies `frame` into the blit texture.
    ///
    /// Reallocates the texture only when the size changed; an unchanged size is a
    /// `glTexSubImage2D` into the existing storage, which is what keeps a 60 Hz editor from
    /// churning GPU memory.
    ///
    /// # Errors
    ///
    /// [`GraphicErrorKind::InvalidArgument`] for a frame with a dimension past what OpenGL can
    /// address. An empty frame is not an error — it uploads nothing and leaves the texture
    /// alone, which is what a minimised window should do.
    ///
    /// # Safety
    ///
    /// The context this blitter was created with must be current on the calling thread.
    pub unsafe fn upload(
        &mut self,
        gl: &glow::Context,
        frame: &SoftwareFramebuffer,
    ) -> DauxGraphicResult<()> {
        if frame.is_empty() {
            return Ok(());
        }
        let size = frame.size();
        let (width, height) = (
            i32::try_from(size.width).map_err(|_| too_wide(size))?,
            i32::try_from(size.height).map_err(|_| too_wide(size))?,
        );

        // SAFETY: the caller guarantees the context is current. `frame.pixels()` is a slice of
        // exactly `width * height * 4` bytes — `SoftwareFramebuffer` maintains that invariant
        // — which is what `GL_RGBA`/`GL_UNSIGNED_BYTE` at this size reads, and the driver
        // copies out of it before the call returns, so the borrow does not outlive it.
        unsafe {
            gl.bind_texture(glow::TEXTURE_2D, Some(self.texture));
            // Rows are tightly packed, so the default four-byte row alignment happens to be
            // right for RGBA8 — but saying so explicitly costs nothing and stops an unrelated
            // change of format from silently skewing the image.
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);

            if size == self.allocated {
                gl.tex_sub_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    0,
                    0,
                    width,
                    height,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(Some(frame.pixels())),
                );
            } else {
                gl.tex_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    glow::RGBA8 as i32,
                    width,
                    height,
                    0,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(Some(frame.pixels())),
                );
                self.allocated = size;
            }
            gl.bind_texture(glow::TEXTURE_2D, None);
        }
        Ok(())
    }

    /// [main-thread] Clears the surface and draws the uploaded frame into `viewport`.
    ///
    /// `clear` is the colour the letterbox bars get when the viewport does not cover the whole
    /// surface. An empty viewport, or a blitter with nothing uploaded yet, clears and returns.
    ///
    /// # Safety
    ///
    /// The context this blitter was created with must be current on the calling thread.
    pub unsafe fn draw(
        &self,
        gl: &glow::Context,
        surface: PhysicalSize,
        viewport: Viewport,
        clear: [f32; 4],
    ) {
        // SAFETY: the caller guarantees the context is current. Every object named here was
        // created by `new` on this same context and has not been destroyed — `destroy`
        // consumes the blitter, so a destroyed one cannot be drawn with.
        unsafe {
            let full = Viewport::fill(surface);
            gl.disable(glow::SCISSOR_TEST);
            gl.disable(glow::DEPTH_TEST);
            gl.disable(glow::CULL_FACE);
            gl.disable(glow::BLEND);
            gl.viewport(full.x, full.y, full.width, full.height);
            gl.clear_color(clear[0], clear[1], clear[2], clear[3]);
            gl.clear(glow::COLOR_BUFFER_BIT);

            if viewport.is_empty() || self.allocated.is_empty() {
                return;
            }

            gl.viewport(viewport.x, viewport.y, viewport.width, viewport.height);
            gl.use_program(Some(self.program));
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.texture));
            gl.uniform_1_i32(self.sampler.as_ref(), 0);
            gl.bind_vertex_array(self.vertex_array);
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
            gl.bind_vertex_array(None);
            gl.bind_texture(glow::TEXTURE_2D, None);
            gl.use_program(None);
        }
    }

    /// [main-thread] Deletes every GL object this blitter owns.
    ///
    /// Consuming `self` is deliberate: it makes "destroyed, then used" unrepresentable rather
    /// than something to check for.
    ///
    /// # Safety
    ///
    /// The context this blitter was created with must be current on the calling thread.
    pub unsafe fn destroy(self, gl: &glow::Context) {
        // SAFETY: the caller guarantees the context is current, and it is the same context
        // these objects were created on — a blitter cannot be moved between contexts, because
        // nothing here hands out its object names.
        unsafe {
            if let Some(vao) = self.vertex_array {
                gl.delete_vertex_array(vao);
            }
            gl.delete_texture(self.texture);
            gl.delete_program(self.program);
        }
    }
}

/// Deletes shader objects that are no longer needed.
///
/// # Safety
///
/// `gl` must be current on the calling thread and must be the context the shaders came from.
unsafe fn delete_shaders(gl: &glow::Context, shaders: &[glow::Shader]) {
    // SAFETY: forwarded from this function's own contract; `shaders` holds only names this
    // module created on `gl` and has not yet deleted.
    unsafe {
        for shader in shaders {
            gl.delete_shader(*shader);
        }
    }
}

/// Builds the error for a driver that refused to make something, keeping its own message.
fn resource_error(what: &str, detail: &str) -> GraphicError {
    let detail = detail.trim();
    if detail.is_empty() {
        GraphicError::new(GraphicErrorKind::Resource, what.to_owned())
    } else {
        GraphicError::new(GraphicErrorKind::Resource, format!("{what}: {detail}"))
    }
}

/// Builds the error for a frame OpenGL cannot address.
fn too_wide(size: PhysicalSize) -> GraphicError {
    GraphicError::new(
        GraphicErrorKind::InvalidArgument,
        format!(
            "a {}x{} frame is larger than OpenGL can address",
            size.width, size.height
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // The GL calls in this module cannot be exercised without a real, current context, which a
    // headless test machine does not have and this crate deliberately cannot create. What *is*
    // testable — and what actually breaks — is the shader text: a mismatched `#version`, a
    // varying declared in one stage and not the other, or a uniform whose name drifted apart
    // from the string passed to `get_uniform_location`.

    #[test]
    fn each_stage_gets_the_version_header_the_context_needs() {
        let desktop = GlVersion::new(3, 3, false);
        let vertex = shader_source(desktop, false).expect("3.3 is supported");
        let fragment = shader_source(desktop, true).expect("3.3 is supported");
        assert!(vertex.starts_with("#version 330 core\n"));
        assert!(fragment.starts_with("#version 330 core\n"));

        let es = shader_source(GlVersion::new(3, 0, true), true).expect("ES 3.0 is supported");
        assert!(es.starts_with("#version 300 es\n"));
        assert!(
            es.contains("precision"),
            "an ES fragment shader without a precision qualifier does not compile"
        );
    }

    #[test]
    fn a_context_that_cannot_run_the_shaders_is_reported_before_anything_is_created() {
        let err = shader_source(GlVersion::new(2, 1, false), false)
            .expect_err("GL 2.1 has no gl_VertexID");
        assert_eq!(err.kind(), GraphicErrorKind::Unsupported);
        assert!(
            err.message().contains("2.1"),
            "the message must name the version that was found: {}",
            err.message()
        );
        assert!(shader_source(GlVersion::new(2, 0, true), true).is_err());
    }

    #[test]
    fn the_two_stages_agree_on_the_varying_they_pass_between_them() {
        // A varying written by the vertex shader and not read by the fragment shader — or
        // named differently in the two — links on some drivers and fails on others.
        assert!(VERTEX_BODY.contains("out vec2 v_uv;"));
        assert!(FRAGMENT_BODY.contains("in vec2 v_uv;"));
        assert_eq!(
            VERTEX_BODY.matches("v_uv").count(),
            2,
            "the vertex stage should declare and write v_uv exactly once each"
        );
    }

    #[test]
    fn the_sampler_uniform_is_named_the_same_in_the_shader_and_in_the_lookup() {
        // `get_uniform_location` returns `None` for a name that is not there, and a `None`
        // location silently draws black instead of failing.
        assert!(
            FRAGMENT_BODY.contains(&format!("uniform sampler2D {SAMPLER_UNIFORM};")),
            "the fragment shader does not declare `{SAMPLER_UNIFORM}`"
        );
    }

    #[test]
    fn the_fullscreen_triangle_needs_no_vertex_buffer() {
        // The whole reason the blit is one draw call with no buffers: the positions come from
        // `gl_VertexID`. If that ever changed to an attribute, the vertex array setup in `new`
        // would silently become insufficient.
        assert!(VERTEX_BODY.contains("gl_VertexID"));
        assert!(
            !VERTEX_BODY.contains("in vec"),
            "the vertex stage must not read any attribute"
        );
    }

    #[test]
    fn resource_errors_keep_the_drivers_own_message() {
        let with_log = resource_error("the blit shader did not compile", "  0:12 syntax error  ");
        assert_eq!(with_log.kind(), GraphicErrorKind::Resource);
        assert_eq!(
            with_log.message(),
            "the blit shader did not compile: 0:12 syntax error"
        );

        // Some drivers return an empty info log; the message must still say what failed.
        let bare = resource_error("could not create a GL program", "   ");
        assert_eq!(bare.message(), "could not create a GL program");
    }
}
