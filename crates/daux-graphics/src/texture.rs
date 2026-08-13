//! Shared-texture presentation: the types and the negotiation, and nothing else.
//!
//! `com.futureboard.daux.shared-texture/1` (`abi-v1` §13) lets a plug-in render its
//! editor into a GPU surface the host imports and composites directly — no nested child
//! window, no second swapchain, no z-order fights. It is the one graphics capability
//! VST3 and CLAP cannot express.
//!
//! Three rules keep it from being a crash factory, and this module encodes all three:
//!
//! 1. **Negotiation is mandatory.** The host advertises what it can import
//!    ([`SharedTextureCaps`]); the plug-in picks a combination or declines
//!    ([`negotiate_shared_texture`]). Nothing is assumed.
//! 2. **Fallback is mandatory.** A failed negotiation is not an error, it is a fall back
//!    to [`PresentationMode::EmbeddedSurface`](crate::PresentationMode::EmbeddedSurface).
//!    A plug-in must never *require* shared textures to show a UI.
//! 3. **Synchronisation is explicit.** A shared texture without a fence is a race. The
//!    fence kind is negotiated like everything else, and a side that cannot provide one
//!    says so up front with [`SharedTextureCaps::requires_fence`].
//!
//! This module is pure description. It does not call a single GPU API — that lives in
//! the backend crates, where it can be tested against a real device.

use core::ffi::c_void;
use core::fmt;

use crate::PresentationMode;
use crate::size::PhysicalSize;

/// `[any-thread]` How a GPU surface is shared between two APIs or processes.
///
/// The numeric values are the `DAUX_TEXTURE_HANDLE_*` constants of `abi-v1` §13, spelled
/// out here because `daux-graphics` must not depend on `daux-abi`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u32)]
pub enum SharedTextureKind {
    /// Windows: a `HANDLE` obtained from `IDXGIResource1::CreateSharedHandle`.
    D3D11Shared = 1,
    /// Windows: a shared Direct3D 12 heap handle.
    D3D12Heap = 2,
    /// macOS: an `IOSurfaceRef` backing a Metal texture.
    IoSurface = 3,
    /// Linux: a DMA-BUF file descriptor.
    DmaBuf = 4,
    /// Vulkan external memory as a POSIX file descriptor.
    VulkanFd = 5,
    /// Vulkan external memory as a Win32 `HANDLE`.
    VulkanWin32 = 6,
}

impl SharedTextureKind {
    /// `[any-thread]` Every kind the ABI defines, in ascending numeric order.
    pub const ALL: [Self; 6] = [
        Self::D3D11Shared,
        Self::D3D12Heap,
        Self::IoSurface,
        Self::DmaBuf,
        Self::VulkanFd,
        Self::VulkanWin32,
    ];

    /// `[any-thread]` The `DAUX_TEXTURE_HANDLE_*` value.
    #[must_use]
    pub const fn as_bits(self) -> u32 {
        self as u32
    }

    /// `[any-thread]` Reads a `DAUX_TEXTURE_HANDLE_*` value.
    ///
    /// Unknown values return `None`, which the caller must treat as "decline", never as
    /// "guess" — that is what `abi-v1` §13 requires.
    #[must_use]
    pub const fn from_bits(bits: u32) -> Option<Self> {
        match bits {
            1 => Some(Self::D3D11Shared),
            2 => Some(Self::D3D12Heap),
            3 => Some(Self::IoSurface),
            4 => Some(Self::DmaBuf),
            5 => Some(Self::VulkanFd),
            6 => Some(Self::VulkanWin32),
            _ => None,
        }
    }

    /// `[any-thread]` A short, stable, lowercase name used in manifests and logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::D3D11Shared => "d3d11-shared",
            Self::D3D12Heap => "d3d12-heap",
            Self::IoSurface => "iosurface",
            Self::DmaBuf => "dmabuf",
            Self::VulkanFd => "vulkan-fd",
            Self::VulkanWin32 => "vulkan-win32",
        }
    }

    /// `[any-thread]` Whether this kind could exist on the platform this binary was
    /// built for.
    ///
    /// A convenience for building capability lists, not a security check: the answer
    /// still has to be confirmed by negotiation with the host.
    #[must_use]
    pub const fn is_plausible_on_this_platform(self) -> bool {
        let windows = cfg!(target_os = "windows");
        let apple = cfg!(target_vendor = "apple");
        match self {
            Self::D3D11Shared | Self::D3D12Heap | Self::VulkanWin32 => windows,
            Self::IoSurface => apple,
            Self::DmaBuf => !windows && !apple,
            Self::VulkanFd => !windows,
        }
    }
}

impl fmt::Display for SharedTextureKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `[any-thread]` The pixel format of a shared surface.
///
/// `abi-v1` §13 deliberately leaves `DauxSharedTextureV1::format` to negotiation rather
/// than freezing an enumeration into the binary contract. These are the values DAUx
/// negotiates with; both sides must treat an unrecognised value as "decline".
///
/// The set is small on purpose. Every entry is importable by D3D11/12, Metal and Vulkan
/// alike; exotic formats belong in a later revision, not in the first agreement two
/// unknown implementations ever make.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u32)]
pub enum TextureFormat {
    /// 8-bit RGBA, unsigned normalised, linear.
    Rgba8Unorm = 1,
    /// 8-bit RGBA, unsigned normalised, sRGB-encoded.
    Rgba8UnormSrgb = 2,
    /// 8-bit BGRA, unsigned normalised, linear. The Windows compositor's native order.
    Bgra8Unorm = 3,
    /// 8-bit BGRA, unsigned normalised, sRGB-encoded.
    Bgra8UnormSrgb = 4,
    /// 10-bit RGB with 2-bit alpha, for HDR-capable compositors.
    Rgb10a2Unorm = 5,
    /// 16-bit floating point RGBA, for wide-gamut and HDR editors.
    Rgba16Float = 6,
}

impl TextureFormat {
    /// `[any-thread]` Every format DAUx negotiates, in ascending numeric order.
    pub const ALL: [Self; 6] = [
        Self::Rgba8Unorm,
        Self::Rgba8UnormSrgb,
        Self::Bgra8Unorm,
        Self::Bgra8UnormSrgb,
        Self::Rgb10a2Unorm,
        Self::Rgba16Float,
    ];

    /// `[any-thread]` The wire value written into `DauxSharedTextureV1::format`.
    #[must_use]
    pub const fn as_bits(self) -> u32 {
        self as u32
    }

    /// `[any-thread]` Reads a wire value; unknown values mean "decline".
    #[must_use]
    pub const fn from_bits(bits: u32) -> Option<Self> {
        match bits {
            1 => Some(Self::Rgba8Unorm),
            2 => Some(Self::Rgba8UnormSrgb),
            3 => Some(Self::Bgra8Unorm),
            4 => Some(Self::Bgra8UnormSrgb),
            5 => Some(Self::Rgb10a2Unorm),
            6 => Some(Self::Rgba16Float),
            _ => None,
        }
    }

    /// `[any-thread]` A short, stable, lowercase name used in manifests and logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Rgba8Unorm => "rgba8unorm",
            Self::Rgba8UnormSrgb => "rgba8unorm-srgb",
            Self::Bgra8Unorm => "bgra8unorm",
            Self::Bgra8UnormSrgb => "bgra8unorm-srgb",
            Self::Rgb10a2Unorm => "rgb10a2unorm",
            Self::Rgba16Float => "rgba16float",
        }
    }

    /// `[any-thread]` Bytes one pixel occupies, used to sanity-check `row_pitch`.
    #[must_use]
    pub const fn bytes_per_pixel(self) -> u32 {
        match self {
            Self::Rgba8Unorm | Self::Rgba8UnormSrgb | Self::Bgra8Unorm | Self::Bgra8UnormSrgb => 4,
            Self::Rgb10a2Unorm => 4,
            Self::Rgba16Float => 8,
        }
    }

    /// `[any-thread]` Whether sampling this format applies the sRGB transfer function.
    ///
    /// Getting this wrong is the classic "the plug-in's UI is washed out" bug, so it is
    /// part of the negotiated agreement rather than an afterthought.
    #[must_use]
    pub const fn is_srgb(self) -> bool {
        matches!(self, Self::Rgba8UnormSrgb | Self::Bgra8UnormSrgb)
    }
}

impl fmt::Display for TextureFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `[any-thread]` The cross-API synchronisation primitive guarding a shared texture.
///
/// Written into `DauxSharedTextureV1::fence_kind`, where `0` means "no fence". The
/// values are DAUx's, negotiated exactly like [`TextureFormat`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u32)]
pub enum FenceKind {
    /// Direct3D 11 keyed mutex on the shared resource.
    D3D11KeyedMutex = 1,
    /// A Direct3D 12 fence shared as a Win32 `HANDLE`.
    D3D12Fence = 2,
    /// A Vulkan timeline semaphore exported as a POSIX file descriptor.
    VulkanSemaphoreFd = 3,
    /// A Vulkan timeline semaphore exported as a Win32 `HANDLE`.
    VulkanSemaphoreWin32 = 4,
    /// A Metal `MTLSharedEvent`.
    MetalSharedEvent = 5,
    /// A Linux sync-file file descriptor.
    SyncFileFd = 6,
}

impl FenceKind {
    /// `[any-thread]` Every fence kind DAUx negotiates, in ascending numeric order.
    pub const ALL: [Self; 6] = [
        Self::D3D11KeyedMutex,
        Self::D3D12Fence,
        Self::VulkanSemaphoreFd,
        Self::VulkanSemaphoreWin32,
        Self::MetalSharedEvent,
        Self::SyncFileFd,
    ];

    /// `[any-thread]` The wire value; never `0`, which the ABI reserves for "no fence".
    #[must_use]
    pub const fn as_bits(self) -> u32 {
        self as u32
    }

    /// `[any-thread]` Reads a wire value. `0` and unknown values return `None`.
    #[must_use]
    pub const fn from_bits(bits: u32) -> Option<Self> {
        match bits {
            1 => Some(Self::D3D11KeyedMutex),
            2 => Some(Self::D3D12Fence),
            3 => Some(Self::VulkanSemaphoreFd),
            4 => Some(Self::VulkanSemaphoreWin32),
            5 => Some(Self::MetalSharedEvent),
            6 => Some(Self::SyncFileFd),
            _ => None,
        }
    }

    /// `[any-thread]` A short, stable, lowercase name used in manifests and logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::D3D11KeyedMutex => "d3d11-keyed-mutex",
            Self::D3D12Fence => "d3d12-fence",
            Self::VulkanSemaphoreFd => "vulkan-semaphore-fd",
            Self::VulkanSemaphoreWin32 => "vulkan-semaphore-win32",
            Self::MetalSharedEvent => "metal-shared-event",
            Self::SyncFileFd => "sync-file-fd",
        }
    }
}

impl fmt::Display for FenceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `[main-thread]` What one side of the negotiation can produce or import.
///
/// Order is preference order. A host lists the handle kinds it can import best first; a
/// plug-in does the same for what it can produce. [`negotiate_shared_texture`] walks the
/// **plug-in's** order, because the plug-in knows which of its own paths is
/// best-tested — the host's role is to veto, not to rank.
///
/// An empty capability set is a valid answer meaning "I cannot do this at all", and it
/// is what [`none`](Self::none) returns.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SharedTextureCaps {
    /// Importable or producible handle kinds, most preferred first.
    pub kinds: Vec<SharedTextureKind>,
    /// Acceptable pixel formats, most preferred first.
    pub formats: Vec<TextureFormat>,
    /// Usable synchronisation primitives, most preferred first. May be empty.
    pub fences: Vec<FenceKind>,
    /// Whether this side refuses to proceed without a fence.
    ///
    /// A compositor that reads the texture on its own timeline must set this: an
    /// unsynchronised import is a tearing, flickering race. A side that sets it and
    /// finds no common fence declines, and both fall back to an embedded surface.
    pub requires_fence: bool,
}

impl SharedTextureCaps {
    /// `[any-thread]` An empty capability set: "I cannot share textures".
    #[must_use]
    pub const fn none() -> Self {
        Self {
            kinds: Vec::new(),
            formats: Vec::new(),
            fences: Vec::new(),
            requires_fence: false,
        }
    }

    /// `[main-thread]` An empty capability set to build on.
    #[must_use]
    pub fn new() -> Self {
        Self::none()
    }

    /// `[main-thread]` Appends a handle kind, ignoring duplicates.
    #[must_use]
    pub fn with_kind(mut self, kind: SharedTextureKind) -> Self {
        if !self.kinds.contains(&kind) {
            self.kinds.push(kind);
        }
        self
    }

    /// `[main-thread]` Appends a pixel format, ignoring duplicates.
    #[must_use]
    pub fn with_format(mut self, format: TextureFormat) -> Self {
        if !self.formats.contains(&format) {
            self.formats.push(format);
        }
        self
    }

    /// `[main-thread]` Appends a fence kind, ignoring duplicates.
    #[must_use]
    pub fn with_fence(mut self, fence: FenceKind) -> Self {
        if !self.fences.contains(&fence) {
            self.fences.push(fence);
        }
        self
    }

    /// `[main-thread]` Declares that this side will not proceed without a fence.
    #[must_use]
    pub fn requiring_fence(mut self, required: bool) -> Self {
        self.requires_fence = required;
        self
    }

    /// `[any-thread]` Whether this side can share textures at all.
    ///
    /// Both a handle kind and a format are needed: either list being empty makes an
    /// agreement impossible, so both count as "empty" here.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty() || self.formats.is_empty()
    }

    /// `[any-thread]` Whether the given handle kind is listed.
    #[must_use]
    pub fn supports_kind(&self, kind: SharedTextureKind) -> bool {
        self.kinds.contains(&kind)
    }

    /// `[any-thread]` Whether the given format is listed.
    #[must_use]
    pub fn supports_format(&self, format: TextureFormat) -> bool {
        self.formats.contains(&format)
    }

    /// `[any-thread]` Whether the given fence kind is listed.
    #[must_use]
    pub fn supports_fence(&self, fence: FenceKind) -> bool {
        self.fences.contains(&fence)
    }
}

/// `[any-thread]` The outcome of a successful [`negotiate_shared_texture`].
///
/// Everything both sides need to agree on before a single pixel is rendered.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SharedTextureAgreement {
    /// The handle kind both sides speak.
    pub kind: SharedTextureKind,
    /// The pixel format both sides accept.
    pub format: TextureFormat,
    /// The agreed fence, or `None` when both sides were content without one.
    pub fence: Option<FenceKind>,
}

impl fmt::Display for SharedTextureAgreement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.kind, self.format)?;
        match self.fence {
            Some(fence) => write!(f, " synchronised by {fence}"),
            None => f.write_str(" unsynchronised"),
        }
    }
}

/// `[main-thread]` Intersects two capability sets, in the plug-in's preference order.
///
/// Returns `None` — meaning "decline, fall back to an embedded surface" — when the two
/// sides share no handle kind, no format, or no fence while either side requires one.
/// Declining is a normal outcome, not an error: the caller falls back to
/// [`PresentationMode::EmbeddedSurface`].
///
/// ```
/// use daux_graphics::{
///     FenceKind, SharedTextureCaps, SharedTextureKind, TextureFormat,
///     negotiate_shared_texture,
/// };
///
/// let plugin = SharedTextureCaps::new()
///     .with_kind(SharedTextureKind::D3D12Heap)
///     .with_kind(SharedTextureKind::D3D11Shared)
///     .with_format(TextureFormat::Rgba8UnormSrgb)
///     .with_format(TextureFormat::Bgra8UnormSrgb)
///     .with_fence(FenceKind::D3D12Fence);
///
/// // The host cannot import a D3D12 heap and composites in BGRA.
/// let host = SharedTextureCaps::new()
///     .with_kind(SharedTextureKind::D3D11Shared)
///     .with_format(TextureFormat::Bgra8UnormSrgb)
///     .with_fence(FenceKind::D3D12Fence)
///     .requiring_fence(true);
///
/// let agreed = negotiate_shared_texture(&plugin, &host).expect("both sides can do this");
/// assert_eq!(agreed.kind, SharedTextureKind::D3D11Shared);
/// assert_eq!(agreed.format, TextureFormat::Bgra8UnormSrgb);
/// assert_eq!(agreed.fence, Some(FenceKind::D3D12Fence));
/// ```
#[must_use]
pub fn negotiate_shared_texture(
    plugin: &SharedTextureCaps,
    host: &SharedTextureCaps,
) -> Option<SharedTextureAgreement> {
    let kind = *plugin.kinds.iter().find(|k| host.supports_kind(**k))?;
    let format = *plugin.formats.iter().find(|f| host.supports_format(**f))?;
    let fence = plugin
        .fences
        .iter()
        .copied()
        .find(|f| host.supports_fence(*f));
    if fence.is_none() && (plugin.requires_fence || host.requires_fence) {
        return None;
    }
    Some(SharedTextureAgreement {
        kind,
        format,
        fence,
    })
}

/// `[main-thread]` A GPU surface handed to the host for compositing.
///
/// The Rust mirror of `DauxSharedTextureV1` (`abi-v1` §13). Like [`WindowTarget`], it
/// carries raw handles this crate never touches, so it is neither `Send` nor `Sync`: a
/// GPU handle belongs to the thread and device that made it.
///
/// [`WindowTarget`]: crate::WindowTarget
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SharedTexture {
    /// How `handle` is to be interpreted.
    pub kind: SharedTextureKind,
    /// The shared handle itself: a Win32 `HANDLE`, an `IOSurfaceRef`, or a file
    /// descriptor widened to pointer size.
    pub handle: *mut c_void,
    /// The surface's pixel format.
    pub format: TextureFormat,
    /// The surface's size in device pixels.
    pub size: PhysicalSize,
    /// Bytes per row, or `0` to let the importer derive it from `format` and `size`.
    pub row_pitch: u32,
    /// The synchronisation primitive guarding the surface, if one was agreed.
    pub fence: Option<*mut c_void>,
    /// How `fence` is to be interpreted. Always `Some` exactly when `fence` is.
    pub fence_kind: Option<FenceKind>,
}

impl SharedTexture {
    /// `[main-thread]` Describes a surface, rejecting a null handle or an empty size.
    #[must_use]
    pub fn new(
        kind: SharedTextureKind,
        handle: *mut c_void,
        format: TextureFormat,
        size: PhysicalSize,
    ) -> Option<Self> {
        (!handle.is_null() && !size.is_empty()).then_some(Self {
            kind,
            handle,
            format,
            size,
            row_pitch: 0,
            fence: None,
            fence_kind: None,
        })
    }

    /// `[main-thread]` Sets an explicit row pitch. `0` means "derive it".
    #[must_use]
    pub const fn with_row_pitch(mut self, row_pitch: u32) -> Self {
        self.row_pitch = row_pitch;
        self
    }

    /// `[main-thread]` Attaches the agreed fence, rejecting a null fence handle.
    ///
    /// Returns `None` rather than silently presenting an unsynchronised surface: a fence
    /// that was negotiated and then not delivered is exactly the race the negotiation
    /// existed to prevent.
    #[must_use]
    pub fn with_fence(mut self, kind: FenceKind, fence: *mut c_void) -> Option<Self> {
        if fence.is_null() {
            return None;
        }
        self.fence = Some(fence);
        self.fence_kind = Some(kind);
        Some(self)
    }

    /// `[any-thread]` The smallest row pitch this surface could have, in bytes.
    ///
    /// Saturates rather than overflowing for absurd sizes.
    #[must_use]
    pub const fn min_row_pitch(&self) -> u64 {
        self.size.width as u64 * self.format.bytes_per_pixel() as u64
    }

    /// `[any-thread]` Whether the description is internally consistent.
    ///
    /// Checked by the importing side before it touches the handle: a non-null handle, a
    /// non-empty size, a row pitch that is either "derive it" or at least one row's
    /// worth of bytes, and a fence handle that is present exactly when a fence kind is.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        if self.handle.is_null() || self.size.is_empty() {
            return false;
        }
        if self.row_pitch != 0 && u64::from(self.row_pitch) < self.min_row_pitch() {
            return false;
        }
        match (self.fence, self.fence_kind) {
            (Some(ptr), Some(_)) => !ptr.is_null(),
            (None, None) => true,
            _ => false,
        }
    }

    /// `[any-thread]` Whether this surface is what the negotiation agreed on.
    ///
    /// A presenter that hands over a texture in a different format than the one both
    /// sides agreed to is worse than one that declines, so importers check.
    #[must_use]
    pub fn matches(&self, agreement: &SharedTextureAgreement) -> bool {
        self.kind == agreement.kind
            && self.format == agreement.format
            && self.fence_kind == agreement.fence
    }
}

/// `[main-thread]` Implemented by editors that can hand the host a GPU surface.
///
/// The trait is small because the hard parts — creating the texture, exporting the
/// handle, signalling the fence — are backend work that needs a real device. What lives
/// here is the protocol: negotiate, then acquire a surface per frame, then release it.
///
/// Every implementation MUST work when negotiation fails. Returning `None` from
/// [`negotiate`](Self::negotiate) obliges the caller to fall back to
/// [`fallback_presentation`](Self::fallback_presentation), which is why that method
/// cannot return [`PresentationMode::SharedTexture`].
pub trait SharedTexturePresenter {
    /// `[main-thread]` What this presenter can produce, in its own preference order.
    fn caps(&self) -> SharedTextureCaps;

    /// `[main-thread]` Agrees on a handle kind with the host, or declines.
    ///
    /// Implementations normally delegate to [`negotiate_shared_texture`] and remember
    /// the whole [`SharedTextureAgreement`], since [`acquire`](Self::acquire) needs the
    /// format and fence too.
    fn negotiate(&mut self, host: &SharedTextureCaps) -> Option<SharedTextureKind>;

    /// `[main-thread]` The surface holding the most recently rendered frame.
    ///
    /// `None` means "nothing to show this frame" — not an error, and not a reason for
    /// the host to tear down the editor.
    fn acquire(&mut self) -> Option<SharedTexture>;

    /// `[main-thread]` Called once the host has finished with the acquired surface.
    ///
    /// The default does nothing, which is correct for a presenter that keeps a single
    /// persistent surface. Double-buffered presenters recycle here.
    fn release(&mut self) {}

    /// `[main-thread]` What to present instead when negotiation fails.
    ///
    /// Always an embedded surface unless an editor genuinely has a better fallback; it
    /// may never be [`PresentationMode::SharedTexture`], because that is the mode that
    /// just failed.
    fn fallback_presentation(&self) -> PresentationMode {
        PresentationMode::EmbeddedSurface
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_handle(v: usize) -> *mut c_void {
        v as *mut c_void
    }

    fn d3d11_caps() -> SharedTextureCaps {
        SharedTextureCaps::new()
            .with_kind(SharedTextureKind::D3D11Shared)
            .with_format(TextureFormat::Bgra8UnormSrgb)
            .with_fence(FenceKind::D3D11KeyedMutex)
    }

    #[test]
    fn handle_kind_values_match_the_abi_constants() {
        assert_eq!(SharedTextureKind::D3D11Shared.as_bits(), 1);
        assert_eq!(SharedTextureKind::D3D12Heap.as_bits(), 2);
        assert_eq!(SharedTextureKind::IoSurface.as_bits(), 3);
        assert_eq!(SharedTextureKind::DmaBuf.as_bits(), 4);
        assert_eq!(SharedTextureKind::VulkanFd.as_bits(), 5);
        assert_eq!(SharedTextureKind::VulkanWin32.as_bits(), 6);
        for kind in SharedTextureKind::ALL {
            assert_eq!(SharedTextureKind::from_bits(kind.as_bits()), Some(kind));
        }
        assert_eq!(SharedTextureKind::from_bits(0), None);
        assert_eq!(SharedTextureKind::from_bits(7), None);
        assert_eq!(SharedTextureKind::from_bits(u32::MAX), None);
    }

    #[test]
    fn formats_and_fences_round_trip_and_reject_unknown_values() {
        for format in TextureFormat::ALL {
            assert_eq!(TextureFormat::from_bits(format.as_bits()), Some(format));
            assert!(format.bytes_per_pixel() >= 4);
        }
        assert_eq!(TextureFormat::from_bits(0), None);
        assert_eq!(TextureFormat::from_bits(7), None);
        assert!(TextureFormat::Bgra8UnormSrgb.is_srgb());
        assert!(!TextureFormat::Bgra8Unorm.is_srgb());
        assert_eq!(TextureFormat::Rgba16Float.bytes_per_pixel(), 8);

        for fence in FenceKind::ALL {
            assert_eq!(FenceKind::from_bits(fence.as_bits()), Some(fence));
            assert_ne!(fence.as_bits(), 0, "0 is reserved for 'no fence'");
        }
        assert_eq!(FenceKind::from_bits(0), None);
        assert_eq!(FenceKind::from_bits(7), None);
    }

    #[test]
    fn platform_plausibility_is_mutually_exclusive_where_it_should_be() {
        assert_ne!(
            SharedTextureKind::D3D11Shared.is_plausible_on_this_platform(),
            SharedTextureKind::IoSurface.is_plausible_on_this_platform(),
            "no platform has both D3D11 and IOSurface"
        );
        #[cfg(target_os = "windows")]
        {
            assert!(SharedTextureKind::D3D11Shared.is_plausible_on_this_platform());
            assert!(!SharedTextureKind::DmaBuf.is_plausible_on_this_platform());
            assert!(!SharedTextureKind::VulkanFd.is_plausible_on_this_platform());
            assert!(SharedTextureKind::VulkanWin32.is_plausible_on_this_platform());
        }
    }

    #[test]
    fn capability_builders_ignore_duplicates() {
        let caps = d3d11_caps()
            .with_kind(SharedTextureKind::D3D11Shared)
            .with_format(TextureFormat::Bgra8UnormSrgb)
            .with_fence(FenceKind::D3D11KeyedMutex);
        assert_eq!(caps.kinds.len(), 1);
        assert_eq!(caps.formats.len(), 1);
        assert_eq!(caps.fences.len(), 1);
        assert!(caps.supports_kind(SharedTextureKind::D3D11Shared));
        assert!(!caps.supports_kind(SharedTextureKind::DmaBuf));
        assert!(caps.supports_format(TextureFormat::Bgra8UnormSrgb));
        assert!(!caps.supports_format(TextureFormat::Rgba16Float));
        assert!(caps.supports_fence(FenceKind::D3D11KeyedMutex));
        assert!(!caps.supports_fence(FenceKind::SyncFileFd));
    }

    #[test]
    fn a_half_filled_capability_set_counts_as_empty() {
        assert!(SharedTextureCaps::none().is_empty());
        let kinds_only = SharedTextureCaps::new().with_kind(SharedTextureKind::DmaBuf);
        assert!(kinds_only.is_empty(), "a handle kind with no format agrees on nothing");
        let formats_only = SharedTextureCaps::new().with_format(TextureFormat::Rgba8Unorm);
        assert!(formats_only.is_empty());
        assert!(!d3d11_caps().is_empty());
    }

    #[test]
    fn negotiation_follows_the_plug_ins_preference_order() {
        let plugin = SharedTextureCaps::new()
            .with_kind(SharedTextureKind::D3D12Heap)
            .with_kind(SharedTextureKind::D3D11Shared)
            .with_format(TextureFormat::Rgba8UnormSrgb)
            .with_format(TextureFormat::Bgra8UnormSrgb);
        // The host lists D3D11 first but supports both; the plug-in's order wins.
        let host = SharedTextureCaps::new()
            .with_kind(SharedTextureKind::D3D11Shared)
            .with_kind(SharedTextureKind::D3D12Heap)
            .with_format(TextureFormat::Bgra8UnormSrgb)
            .with_format(TextureFormat::Rgba8UnormSrgb);
        let agreed = negotiate_shared_texture(&plugin, &host).expect("agreement");
        assert_eq!(agreed.kind, SharedTextureKind::D3D12Heap);
        assert_eq!(agreed.format, TextureFormat::Rgba8UnormSrgb);
        assert_eq!(agreed.fence, None);
        assert!(agreed.to_string().ends_with("unsynchronised"));
    }

    #[test]
    fn negotiation_declines_when_nothing_is_shared() {
        let plugin = SharedTextureCaps::new()
            .with_kind(SharedTextureKind::DmaBuf)
            .with_format(TextureFormat::Rgba8Unorm);
        let wrong_kind = SharedTextureCaps::new()
            .with_kind(SharedTextureKind::D3D11Shared)
            .with_format(TextureFormat::Rgba8Unorm);
        assert_eq!(negotiate_shared_texture(&plugin, &wrong_kind), None);

        let wrong_format = SharedTextureCaps::new()
            .with_kind(SharedTextureKind::DmaBuf)
            .with_format(TextureFormat::Rgba16Float);
        assert_eq!(negotiate_shared_texture(&plugin, &wrong_format), None);

        assert_eq!(
            negotiate_shared_texture(&plugin, &SharedTextureCaps::none()),
            None
        );
        assert_eq!(
            negotiate_shared_texture(&SharedTextureCaps::none(), &plugin),
            None
        );
    }

    #[test]
    fn a_required_fence_that_is_not_shared_declines_the_whole_agreement() {
        let plugin = SharedTextureCaps::new()
            .with_kind(SharedTextureKind::DmaBuf)
            .with_format(TextureFormat::Rgba8Unorm)
            .with_fence(FenceKind::SyncFileFd);
        let host_needs_fence = SharedTextureCaps::new()
            .with_kind(SharedTextureKind::DmaBuf)
            .with_format(TextureFormat::Rgba8Unorm)
            .with_fence(FenceKind::VulkanSemaphoreFd)
            .requiring_fence(true);
        assert_eq!(
            negotiate_shared_texture(&plugin, &host_needs_fence),
            None,
            "no common fence and the host insists: decline"
        );

        // The same mismatch without the requirement is an unsynchronised agreement.
        let relaxed = host_needs_fence.clone().requiring_fence(false);
        let agreed = negotiate_shared_texture(&plugin, &relaxed).expect("agreement");
        assert_eq!(agreed.fence, None);

        // And a plug-in that insists is refused just as firmly.
        let strict_plugin = plugin.requiring_fence(true);
        assert_eq!(negotiate_shared_texture(&strict_plugin, &relaxed), None);
    }

    #[test]
    fn a_common_fence_is_reported_in_the_agreement() {
        let both = d3d11_caps().requiring_fence(true);
        let agreed = negotiate_shared_texture(&both, &both.clone()).expect("agreement");
        assert_eq!(agreed.fence, Some(FenceKind::D3D11KeyedMutex));
        assert!(agreed.to_string().contains("d3d11-keyed-mutex"));
    }

    #[test]
    fn surfaces_reject_impossible_descriptions() {
        assert!(
            SharedTexture::new(
                SharedTextureKind::D3D11Shared,
                core::ptr::null_mut(),
                TextureFormat::Bgra8Unorm,
                PhysicalSize::new(64, 64),
            )
            .is_none()
        );
        assert!(
            SharedTexture::new(
                SharedTextureKind::D3D11Shared,
                fake_handle(1),
                TextureFormat::Bgra8Unorm,
                PhysicalSize::new(64, 0),
            )
            .is_none()
        );
    }

    #[test]
    fn surface_validity_checks_row_pitch_and_fence_consistency() {
        let base = SharedTexture::new(
            SharedTextureKind::D3D11Shared,
            fake_handle(0x100),
            TextureFormat::Bgra8Unorm,
            PhysicalSize::new(64, 32),
        )
        .expect("valid");
        assert!(base.is_valid());
        assert_eq!(base.min_row_pitch(), 64 * 4);
        assert!(base.with_row_pitch(256).is_valid());
        assert!(base.with_row_pitch(512).is_valid(), "padding is allowed");
        assert!(!base.with_row_pitch(255).is_valid(), "a short row is not");

        let fenced = base
            .with_fence(FenceKind::D3D11KeyedMutex, fake_handle(0x200))
            .expect("non-null fence");
        assert!(fenced.is_valid());
        assert!(
            base.with_fence(FenceKind::D3D11KeyedMutex, core::ptr::null_mut())
                .is_none(),
            "a null fence handle is not a fence"
        );

        let mut half_fenced = base;
        half_fenced.fence_kind = Some(FenceKind::D3D12Fence);
        assert!(!half_fenced.is_valid(), "a fence kind with no fence handle");
        let mut other_half = base;
        other_half.fence = Some(fake_handle(0x300));
        assert!(!other_half.is_valid(), "a fence handle with no kind");

        let mut zero_size = base;
        zero_size.size = PhysicalSize::ZERO;
        assert!(!zero_size.is_valid());
    }

    #[test]
    fn a_surface_is_checked_against_the_agreement() {
        let agreement = SharedTextureAgreement {
            kind: SharedTextureKind::D3D11Shared,
            format: TextureFormat::Bgra8Unorm,
            fence: Some(FenceKind::D3D11KeyedMutex),
        };
        let surface = SharedTexture::new(
            SharedTextureKind::D3D11Shared,
            fake_handle(0x100),
            TextureFormat::Bgra8Unorm,
            PhysicalSize::new(8, 8),
        )
        .expect("valid")
        .with_fence(FenceKind::D3D11KeyedMutex, fake_handle(0x200))
        .expect("non-null");
        assert!(surface.matches(&agreement));

        let unfenced = SharedTexture::new(
            SharedTextureKind::D3D11Shared,
            fake_handle(0x100),
            TextureFormat::Bgra8Unorm,
            PhysicalSize::new(8, 8),
        )
        .expect("valid");
        assert!(
            !unfenced.matches(&agreement),
            "dropping the agreed fence is exactly the race negotiation prevents"
        );

        let wrong_format = SharedTexture::new(
            SharedTextureKind::D3D11Shared,
            fake_handle(0x100),
            TextureFormat::Rgba16Float,
            PhysicalSize::new(8, 8),
        )
        .expect("valid");
        assert!(!wrong_format.matches(&agreement));
    }

    /// A presenter that agrees to whatever the host and its own caps allow, used to
    /// prove the trait's default methods and the negotiation helper compose.
    struct TestPresenter {
        caps: SharedTextureCaps,
        agreement: Option<SharedTextureAgreement>,
        released: usize,
    }

    impl SharedTexturePresenter for TestPresenter {
        fn caps(&self) -> SharedTextureCaps {
            self.caps.clone()
        }

        fn negotiate(&mut self, host: &SharedTextureCaps) -> Option<SharedTextureKind> {
            self.agreement = negotiate_shared_texture(&self.caps, host);
            self.agreement.map(|a| a.kind)
        }

        fn acquire(&mut self) -> Option<SharedTexture> {
            let agreement = self.agreement?;
            SharedTexture::new(
                agreement.kind,
                fake_handle(0xF00D),
                agreement.format,
                PhysicalSize::new(16, 16),
            )
        }

        fn release(&mut self) {
            self.released += 1;
        }
    }

    #[test]
    fn a_presenter_that_declines_still_has_a_fallback() {
        let mut presenter = TestPresenter {
            caps: d3d11_caps(),
            agreement: None,
            released: 0,
        };
        let hostile_host = SharedTextureCaps::new()
            .with_kind(SharedTextureKind::DmaBuf)
            .with_format(TextureFormat::Rgba8Unorm);
        assert_eq!(presenter.negotiate(&hostile_host), None);
        assert!(
            presenter.acquire().is_none(),
            "nothing may be handed over before an agreement"
        );
        assert_eq!(
            presenter.fallback_presentation(),
            PresentationMode::EmbeddedSurface
        );
        assert!(!presenter.caps().is_empty());

        let good_host = d3d11_caps();
        assert_eq!(
            presenter.negotiate(&good_host),
            Some(SharedTextureKind::D3D11Shared)
        );
        let surface = presenter.acquire().expect("a surface after agreement");
        assert!(surface.is_valid());
        assert_eq!(surface.format, TextureFormat::Bgra8UnormSrgb);
        presenter.release();
        assert_eq!(presenter.released, 1);
    }
}
