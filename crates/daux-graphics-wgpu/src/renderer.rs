//! The device, the swapchain and the present loop.

use daux_graphics::{
    DauxGraphicResult, GraphicError, GraphicErrorKind, PhysicalSize, WindowTarget,
};

use crate::{SurfaceConfig, select, target::surface_target};

/// An adapter, a device and a queue, with the instance that produced them.
///
/// Kept as one value because they have to be: a `Device` outliving its `Instance` is a use
/// after free in some backends, and separating them makes that possible to write. Everything
/// derived from a `GpuContext` — a surface, a texture, a pipeline — must be dropped before it.
///
/// [main-thread]
pub struct GpuContext {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
}

impl core::fmt::Debug for GpuContext {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let info = self.adapter.get_info();
        f.debug_struct("GpuContext")
            .field("adapter", &info.name)
            .field("backend", &info.backend)
            .field("device_type", &info.device_type)
            .finish_non_exhaustive()
    }
}

impl GpuContext {
    /// [main-thread] Creates a device with no surface attached, for offscreen rendering.
    ///
    /// Useful for rendering an editor to a texture — a preview thumbnail, a screenshot, a
    /// shared-texture presenter — and for tests that want to know whether this machine has a
    /// usable GPU at all.
    ///
    /// # Errors
    ///
    /// [`GraphicErrorKind::Renderer`] when no adapter matches, which on a headless machine or
    /// in a container without a GPU driver is the normal answer rather than a bug.
    ///
    /// # Panics
    ///
    /// `wgpu` itself panics if the crate was built with no backend for this platform. That is
    /// a build configuration error, not a runtime condition, and cannot be recovered from
    /// here.
    pub fn headless(config: &SurfaceConfig) -> DauxGraphicResult<Self> {
        let instance = new_instance(config);
        let adapter = request_adapter(&instance, config, None)?;
        let (device, queue) = request_device(&adapter, config)?;
        Ok(Self {
            instance,
            adapter,
            device,
            queue,
        })
    }

    /// [main-thread] The wgpu instance.
    #[must_use]
    pub const fn instance(&self) -> &wgpu::Instance {
        &self.instance
    }

    /// [main-thread] The chosen adapter.
    #[must_use]
    pub const fn adapter(&self) -> &wgpu::Adapter {
        &self.adapter
    }

    /// [main-thread] The logical device.
    #[must_use]
    pub const fn device(&self) -> &wgpu::Device {
        &self.device
    }

    /// [main-thread] The command queue.
    #[must_use]
    pub const fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// [main-thread] What the adapter says it is: name, backend, driver.
    ///
    /// Worth logging once when an editor opens. "It is black on my machine" is answerable when
    /// this line is in the log and unanswerable when it is not.
    #[must_use]
    pub fn info(&self) -> wgpu::AdapterInfo {
        self.adapter.get_info()
    }
}

/// One acquired swapchain image, with a view ready to be used as a render attachment.
///
/// Dropping a frame without calling [`present`](Self::present) discards it: wgpu returns the
/// image to the swapchain unpresented, which is exactly right for a frame abandoned because
/// something failed halfway through drawing it.
///
/// [main-thread]
pub struct Frame {
    texture: wgpu::SurfaceTexture,
    view: wgpu::TextureView,
    /// `true` when the surface no longer matches its configuration and should be reconfigured
    /// once this frame is out of the way.
    suboptimal: bool,
}

impl core::fmt::Debug for Frame {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Frame")
            .field("suboptimal", &self.suboptimal)
            .finish_non_exhaustive()
    }
}

impl Frame {
    /// [main-thread] The view to attach to a render pass.
    #[must_use]
    pub const fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    /// [main-thread] The swapchain texture behind the view.
    #[must_use]
    pub const fn texture(&self) -> &wgpu::Texture {
        &self.texture.texture
    }

    /// [main-thread] Whether the surface has drifted from its configuration.
    ///
    /// The frame is still perfectly drawable; the renderer reconfigures before the next one.
    #[must_use]
    pub const fn is_suboptimal(&self) -> bool {
        self.suboptimal
    }

    /// [main-thread] Puts the frame on screen.
    ///
    /// Submit every command buffer that draws into it *before* calling this. Prefer
    /// [`WgpuRenderer::present`], which already has the queue to hand.
    pub fn present(self, queue: &wgpu::Queue) {
        queue.present(self.texture);
    }
}

/// A wgpu device and swapchain attached to a host window.
///
/// # What it is for
///
/// This is a renderer, not a framework. It creates a surface from a
/// [`WindowTarget`](daux_graphics::WindowTarget), keeps it configured as the host resizes, and
/// hands out frames. What is drawn into those frames is somebody else's business — a
/// `daux-graphics-egui` painter, a plug-in's own pipelines, or a shared-texture presenter.
///
/// # Window lifetime
///
/// The surface holds raw pointers to the host's window. `GraphicContext` documents that window
/// as valid from `open` until `close` returns, so a renderer created in `open` **must** be
/// dropped no later than `close`. Keeping one alive afterwards is a use-after-free that
/// usually shows up as a crash inside the graphics driver on the next present.
///
/// # Losing the surface
///
/// Swapchains are not permanent. A display mode change, a laptop switching GPUs, a monitor
/// being unplugged, a driver update — all of them make the surface outdated or lost.
/// [`acquire`](Self::acquire) reconfigures and retries once on its own, so a caller only sees
/// an error when the surface is genuinely unusable.
///
/// [main-thread]
pub struct WgpuRenderer {
    gpu: GpuContext,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    size: PhysicalSize,
    /// Set when the swapchain needs reconfiguring but a frame was in flight at the time.
    /// Reconfiguring while a `SurfaceTexture` is alive panics inside wgpu.
    needs_reconfigure: bool,
}

impl core::fmt::Debug for WgpuRenderer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("WgpuRenderer")
            .field("size", &self.size)
            .field("format", &self.config.format)
            .field("present_mode", &self.config.present_mode)
            .field("alpha_mode", &self.config.alpha_mode)
            .field("gpu", &self.gpu)
            .finish_non_exhaustive()
    }
}

impl WgpuRenderer {
    /// [main-thread] Creates a device and a swapchain on the host's window.
    ///
    /// A zero-sized `size` is accepted and leaves the renderer dormant — that is what a
    /// minimised host window reports, and it recovers on the next
    /// [`resize`](Self::resize). While dormant, [`acquire`](Self::acquire) returns `None`.
    ///
    /// # Errors
    ///
    /// * [`GraphicErrorKind::WindowApi`] for a window handle wgpu cannot be given.
    /// * [`GraphicErrorKind::Renderer`] when no adapter can present to the window, when the
    ///   device cannot be created, or when the surface and adapter turn out to be
    ///   incompatible.
    ///
    /// # Safety of the window
    ///
    /// See the type documentation: the returned renderer must be dropped before the host
    /// destroys the window it was created from.
    pub fn new(
        target: WindowTarget,
        size: PhysicalSize,
        config: &SurfaceConfig,
    ) -> DauxGraphicResult<Self> {
        let surface_target = surface_target(target)?;
        let instance = new_instance(config);

        // SAFETY: `SurfaceTargetUnsafe::RawHandle` requires the window and display handles to
        // name a live window that outlives the surface. `surface_target` has already rejected
        // null and unrepresentable handles; the window itself is the host's, and this type's
        // documentation makes dropping the renderer before the host destroys its window part
        // of the contract every caller signs. We do not retain the handles anywhere else.
        let surface = unsafe { instance.create_surface_unsafe(surface_target) }.map_err(|e| {
            GraphicError::new(
                GraphicErrorKind::WindowApi,
                format!("wgpu could not create a surface on the host's window: {e}"),
            )
        })?;

        let adapter = request_adapter(&instance, config, Some(&surface))?;
        let capabilities = surface.get_capabilities(&adapter);
        if capabilities.formats.is_empty() {
            return Err(GraphicError::new(
                GraphicErrorKind::Renderer,
                format!(
                    "the adapter '{}' cannot present to the host's window",
                    adapter.get_info().name
                ),
            ));
        }
        let (device, queue) = request_device(&adapter, config)?;

        let mut renderer = Self {
            config: wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format: select::choose_format(&capabilities, config.format),
                color_space: wgpu::SurfaceColorSpace::Auto,
                width: size.width,
                height: size.height,
                present_mode: select::choose_present_mode(&capabilities, config.vsync),
                desired_maximum_frame_latency: config.effective_frame_latency(),
                alpha_mode: select::choose_alpha_mode(&capabilities, config.alpha),
                view_formats: Vec::new(),
            },
            gpu: GpuContext {
                instance,
                adapter,
                device,
                queue,
            },
            surface,
            size,
            needs_reconfigure: false,
        };
        renderer.reconfigure();
        Ok(renderer)
    }

    /// [main-thread] The GPU objects behind this renderer.
    #[must_use]
    pub const fn gpu(&self) -> &GpuContext {
        &self.gpu
    }

    /// [main-thread] The logical device.
    #[must_use]
    pub const fn device(&self) -> &wgpu::Device {
        self.gpu.device()
    }

    /// [main-thread] The command queue.
    #[must_use]
    pub const fn queue(&self) -> &wgpu::Queue {
        self.gpu.queue()
    }

    /// [main-thread] The swapchain surface.
    #[must_use]
    pub const fn surface(&self) -> &wgpu::Surface<'static> {
        &self.surface
    }

    /// [main-thread] The swapchain's colour format, which every render pipeline drawing into
    /// it must declare as its target.
    #[must_use]
    pub const fn format(&self) -> wgpu::TextureFormat {
        self.config.format
    }

    /// [main-thread] The full swapchain configuration.
    #[must_use]
    pub const fn configuration(&self) -> &wgpu::SurfaceConfiguration {
        &self.config
    }

    /// [main-thread] The surface size in physical pixels.
    #[must_use]
    pub const fn size(&self) -> PhysicalSize {
        self.size
    }

    /// [main-thread] Whether the renderer is dormant because the surface has no area.
    #[must_use]
    pub const fn is_dormant(&self) -> bool {
        self.size.is_empty()
    }

    /// [main-thread] Resizes the swapchain.
    ///
    /// A zero-sized surface — a minimised host window — makes the renderer dormant instead of
    /// failing: `wgpu::Surface::configure` rejects a zero dimension, and a minimised window is
    /// a normal thing for a host to do, not an editor failure. The next non-empty resize
    /// brings it back.
    ///
    /// Resizing to the size it already has does nothing, which matters because several hosts
    /// send a resize on every idle callback.
    pub fn resize(&mut self, size: PhysicalSize) {
        if size == self.size && !self.needs_reconfigure {
            return;
        }
        self.size = size;
        self.config.width = size.width;
        self.config.height = size.height;
        self.reconfigure();
    }

    /// [main-thread] Applies the current configuration to the surface.
    ///
    /// Does nothing while dormant. Never called with a swapchain image outstanding — wgpu
    /// panics if a [`Frame`] is alive — which is why a suboptimal frame sets a flag instead of
    /// reconfiguring on the spot.
    fn reconfigure(&mut self) {
        if self.is_dormant() {
            self.needs_reconfigure = true;
            return;
        }
        self.surface.configure(self.gpu.device(), &self.config);
        self.needs_reconfigure = false;
    }

    /// [main-thread] Acquires the next swapchain image.
    ///
    /// Returns `Ok(None)` for the conditions that mean "skip this frame and try again": the
    /// renderer is dormant, the acquisition timed out, or the host's window is occluded. Those
    /// are not errors, and treating them as errors makes a minimised DAW fill its log.
    ///
    /// An outdated or lost surface is reconfigured and retried once. Only a surface that is
    /// still unusable afterwards produces an error.
    ///
    /// # Errors
    ///
    /// [`GraphicErrorKind::Renderer`] when the surface could not be acquired even after being
    /// reconfigured, and [`GraphicErrorKind::Resource`] for a validation failure inside the
    /// acquisition, which means the configuration itself is wrong.
    pub fn acquire(&mut self) -> DauxGraphicResult<Option<Frame>> {
        if self.is_dormant() {
            return Ok(None);
        }
        if self.needs_reconfigure {
            self.reconfigure();
        }

        // One retry: the first attempt may report a surface that went outdated while the
        // editor was idle, which reconfiguring fixes. A second failure is real.
        for attempt in 0..2 {
            match self.surface.get_current_texture() {
                wgpu::CurrentSurfaceTexture::Success(texture) => {
                    return Ok(Some(self.frame(texture, false)));
                }
                wgpu::CurrentSurfaceTexture::Suboptimal(texture) => {
                    // Reconfiguring now would panic: this image is alive. Draw it — it is
                    // still correct, just not ideal — and reconfigure before the next one.
                    self.needs_reconfigure = true;
                    return Ok(Some(self.frame(texture, true)));
                }
                wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                    return Ok(None);
                }
                wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                    if attempt == 0 {
                        self.reconfigure();
                        continue;
                    }
                    return Err(GraphicError::new_static(
                        GraphicErrorKind::Renderer,
                        "the swapchain was still lost after being reconfigured",
                    ));
                }
                wgpu::CurrentSurfaceTexture::Validation => {
                    return Err(GraphicError::new_static(
                        GraphicErrorKind::Resource,
                        "acquiring a swapchain image raised a validation error",
                    ));
                }
            }
        }
        // The loop returns on every path; this is unreachable and exists so the function has
        // one obvious exit rather than an `unreachable!` that could become a panic in a host.
        Ok(None)
    }

    /// [main-thread] Puts an acquired frame on screen.
    ///
    /// Submit every command buffer that draws into it *before* calling this.
    pub fn present(&self, frame: Frame) {
        frame.present(self.queue());
    }

    /// Wraps an acquired swapchain image with the view a render pass attaches to.
    fn frame(&self, texture: wgpu::SurfaceTexture, suboptimal: bool) -> Frame {
        let view = texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        Frame {
            texture,
            view,
            suboptimal,
        }
    }
}

/// Builds the wgpu instance a plug-in should use.
///
/// No display handle: a plug-in is handed a window by the host and never owns the connection.
fn new_instance(config: &SurfaceConfig) -> wgpu::Instance {
    wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: config.backends,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    })
}

/// Asks for an adapter, turning "there is no GPU here" into an error rather than a panic.
fn request_adapter(
    instance: &wgpu::Instance,
    config: &SurfaceConfig,
    compatible_surface: Option<&wgpu::Surface<'static>>,
) -> DauxGraphicResult<wgpu::Adapter> {
    let options = wgpu::RequestAdapterOptions {
        power_preference: config.power_preference,
        force_fallback_adapter: false,
        compatible_surface,
        ..Default::default()
    };
    pollster::block_on(instance.request_adapter(&options)).map_err(|e| {
        GraphicError::new(
            GraphicErrorKind::Renderer,
            format!("no GPU adapter matched what this editor asked for: {e}"),
        )
    })
}

/// Creates a device with exactly what the adapter supports.
///
/// Asking for the adapter's own limits rather than a fixed set means the request cannot be
/// refused for wanting too much, and a weak GPU produces a working editor with small limits
/// rather than no editor at all.
fn request_device(
    adapter: &wgpu::Adapter,
    config: &SurfaceConfig,
) -> DauxGraphicResult<(wgpu::Device, wgpu::Queue)> {
    let descriptor = wgpu::DeviceDescriptor {
        label: Some(config.label),
        required_features: wgpu::Features::empty(),
        required_limits: adapter.limits(),
        ..Default::default()
    };
    pollster::block_on(adapter.request_device(&descriptor)).map_err(|e| {
        GraphicError::new(
            GraphicErrorKind::Renderer,
            format!("the GPU device could not be created: {e}"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a headless GPU context, or reports why the test is skipping.
    ///
    /// Every GPU-dependent test below goes through this. On a machine with no adapter — CI in
    /// a container, a build box with no display, a VM with a broken driver — the test prints
    /// what happened and passes. It must never fail there: the whole reason
    /// `daux-graphics-wgpu` is not a default member is that plain `cargo test` must work
    /// without a GPU, and a test that fails on a headless machine would defeat that.
    fn gpu_or_skip(what: &str) -> Option<GpuContext> {
        gpu_or_skip_with(what, &SurfaceConfig::new())
    }

    /// The body of [`gpu_or_skip`], with the configuration exposed so that the skip path
    /// itself can be tested on a machine that does have a GPU.
    fn gpu_or_skip_with(what: &str, config: &SurfaceConfig) -> Option<GpuContext> {
        match GpuContext::headless(config) {
            Ok(gpu) => Some(gpu),
            Err(e) => {
                eprintln!("skipping `{what}`: no usable GPU adapter here ({e})");
                None
            }
        }
    }

    #[test]
    fn the_gpu_gate_skips_rather_than_failing_when_there_is_no_adapter() {
        // This is the mechanism that keeps a headless build machine green, so it needs a test
        // of its own — on a developer's machine every other GPU test takes the *other* branch
        // and the skip path would otherwise never run. Asking for an empty backend set is the
        // one way to reproduce "no GPU here" on a machine that has one.
        let none = gpu_or_skip_with(
            "the_gpu_gate_skips_rather_than_failing_when_there_is_no_adapter",
            &SurfaceConfig::new().with_backends(wgpu::Backends::empty()),
        );
        assert!(
            none.is_none(),
            "an empty backend set must not somehow produce an adapter"
        );
    }

    #[test]
    fn a_headless_device_can_be_created_and_describes_itself() {
        let Some(gpu) = gpu_or_skip("a_headless_device_can_be_created_and_describes_itself") else {
            return;
        };
        let info = gpu.info();
        assert!(
            !info.name.is_empty(),
            "an adapter with no name is not a usable adapter"
        );
        // The limits a device was created with are the adapter's own, so they must be at
        // least what a device is guaranteed to provide.
        let limits = gpu.device().limits();
        assert!(
            limits.max_texture_dimension_2d >= 2048,
            "an adapter that cannot make a 2048px texture cannot hold egui's font atlas"
        );
        assert!(
            format!("{gpu:?}").contains(&info.name),
            "the debug form should name the adapter for logs"
        );
    }

    #[test]
    fn a_device_created_from_a_context_can_actually_render() {
        // Creating a device proves very little on its own — some broken drivers hand one back
        // and then fail on the first submission. Rendering a single clear to an offscreen
        // texture and waiting for it is what proves the context is usable.
        let Some(gpu) = gpu_or_skip("a_device_created_from_a_context_can_actually_render") else {
            return;
        };
        let device = gpu.device();
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("daux test target"),
            size: wgpu::Extent3d {
                width: 16,
                height: 16,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("daux test clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }
        gpu.queue().submit(Some(encoder.finish()));
        gpu.device()
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("the queue drained");
    }

    #[test]
    fn asking_for_a_backend_the_machine_does_not_have_fails_instead_of_hanging() {
        // A plug-in that restricts backends to one this machine cannot provide must get a
        // clear error at open time, not a device that fails later.
        let config = SurfaceConfig::new().with_backends(wgpu::Backends::empty());
        let err = GpuContext::headless(&config).expect_err("no backends means no adapter");
        assert_eq!(err.kind(), GraphicErrorKind::Renderer);
    }

    #[test]
    fn a_renderer_cannot_be_created_on_a_null_window() {
        // This one needs no GPU: the window handle is rejected before wgpu is asked for
        // anything, which is the point — a bad handle must never reach a driver.
        let err = WgpuRenderer::new(
            WindowTarget::Win32 {
                hwnd: core::ptr::null_mut(),
            },
            PhysicalSize::new(100, 100),
            &SurfaceConfig::new(),
        )
        .expect_err("a null HWND must be refused");
        assert_eq!(err.kind(), GraphicErrorKind::WindowApi);
    }

    #[test]
    fn a_renderer_cannot_be_created_on_a_wayland_surface_with_no_display() {
        let err = WgpuRenderer::new(
            WindowTarget::Wayland {
                surface: 0x10 as *mut core::ffi::c_void,
                display: core::ptr::null_mut(),
            },
            PhysicalSize::new(100, 100),
            &SurfaceConfig::new(),
        )
        .expect_err("a surface without its display must be refused");
        assert_eq!(err.kind(), GraphicErrorKind::WindowApi);
    }
}
