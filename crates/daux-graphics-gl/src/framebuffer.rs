//! The CPU pixel buffer the software path draws into.

use daux_graphics::{DauxGraphicResult, GraphicError, GraphicErrorKind, PhysicalSize};

/// Bytes per pixel. RGBA8 is the one format every GL context can upload without a conversion
/// and every CPU rasteriser can write without a swizzle.
pub const BYTES_PER_PIXEL: usize = 4;

/// The largest framebuffer this crate will allocate, in pixels.
///
/// 64 mega-pixels is 256 MiB at RGBA8 — more than a 8192×8192 editor, and far more than any
/// real one. The cap exists because a host's window size arrives as two `u32`s and a
/// misbehaving or hostile one can ask for `u32::MAX` on a side; without a ceiling that is a
/// request to allocate 64 exbibytes, which aborts the process rather than failing.
pub const MAX_PIXELS: u64 = 64 << 20;

/// An RGBA8 image in main memory, laid out top row first with no padding between rows.
///
/// # Why it is grow-only
///
/// [`resize`](Self::resize) reuses the existing allocation whenever the new size fits, so an
/// editor being dragged smaller and larger again does not reallocate on every mouse move — a
/// resize drag produces a resize event per frame, and reallocating a megabyte each time is
/// visible as stutter in the host's UI thread.
///
/// [main-thread]
pub struct SoftwareFramebuffer {
    pixels: Vec<u8>,
    size: PhysicalSize,
}

impl core::fmt::Debug for SoftwareFramebuffer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SoftwareFramebuffer")
            .field("size", &self.size)
            .field("capacity_bytes", &self.pixels.capacity())
            .finish()
    }
}

impl SoftwareFramebuffer {
    /// [main-thread] An empty framebuffer that has allocated nothing.
    ///
    /// What an editor holds before it is opened, so that constructing an editor that is never
    /// opened costs nothing.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            pixels: Vec::new(),
            size: PhysicalSize::ZERO,
        }
    }

    /// [main-thread] Allocates a transparent framebuffer of `size` pixels.
    ///
    /// # Errors
    ///
    /// [`GraphicErrorKind::CapacityExceeded`] when `size` is larger than [`MAX_PIXELS`].
    /// A zero-sized framebuffer is *not* an error: it is what a minimised window has, and
    /// refusing it would turn minimising a DAW into an editor failure.
    pub fn new(size: PhysicalSize) -> DauxGraphicResult<Self> {
        let mut fb = Self::empty();
        fb.resize(size)?;
        Ok(fb)
    }

    /// [main-thread] The framebuffer's size in pixels.
    #[must_use]
    pub const fn size(&self) -> PhysicalSize {
        self.size
    }

    /// [main-thread] Whether either dimension is zero, so there is nothing to draw into.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.size.is_empty()
    }

    /// [main-thread] Bytes per row, with no padding.
    #[must_use]
    pub const fn row_pitch(&self) -> usize {
        self.size.width as usize * BYTES_PER_PIXEL
    }

    /// [main-thread] The pixel data, top row first.
    #[must_use]
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// [main-thread] The pixel data, mutably. This is what a rasteriser writes into.
    #[must_use]
    pub fn pixels_mut(&mut self) -> &mut [u8] {
        &mut self.pixels
    }

    /// [main-thread] One row of pixels, or `None` when `y` is past the bottom.
    #[must_use]
    pub fn row_mut(&mut self, y: u32) -> Option<&mut [u8]> {
        if y >= self.size.height || self.is_empty() {
            return None;
        }
        let pitch = self.row_pitch();
        let start = y as usize * pitch;
        self.pixels.get_mut(start..start + pitch)
    }

    /// [main-thread] Overwrites every pixel with one RGBA colour.
    pub fn fill(&mut self, rgba: [u8; 4]) {
        for pixel in self.pixels.chunks_exact_mut(BYTES_PER_PIXEL) {
            pixel.copy_from_slice(&rgba);
        }
    }

    /// [main-thread] Writes one pixel, ignoring coordinates outside the framebuffer.
    ///
    /// Silently clipping rather than panicking is the right behaviour for a rasteriser: a
    /// shape that pokes over an edge is normal, and a bounds check per pixel at the call site
    /// is exactly the code that gets it wrong.
    pub fn set_pixel(&mut self, x: u32, y: u32, rgba: [u8; 4]) {
        if x >= self.size.width || y >= self.size.height {
            return;
        }
        let pitch = self.row_pitch();
        let offset = y as usize * pitch + x as usize * BYTES_PER_PIXEL;
        if let Some(pixel) = self.pixels.get_mut(offset..offset + BYTES_PER_PIXEL) {
            pixel.copy_from_slice(&rgba);
        }
    }

    /// [main-thread] Reads one pixel, or `None` outside the framebuffer.
    #[must_use]
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.size.width || y >= self.size.height {
            return None;
        }
        let offset = y as usize * self.row_pitch() + x as usize * BYTES_PER_PIXEL;
        let slice = self.pixels.get(offset..offset + BYTES_PER_PIXEL)?;
        Some([slice[0], slice[1], slice[2], slice[3]])
    }

    /// [main-thread] Changes the framebuffer's size, keeping the allocation when it fits.
    ///
    /// The contents afterwards are unspecified — a resize invalidates the picture, and every
    /// caller redraws immediately — except that they are always fully initialised, never
    /// stale memory from some other allocation.
    ///
    /// # Errors
    ///
    /// [`GraphicErrorKind::CapacityExceeded`] when `size` exceeds [`MAX_PIXELS`]. The
    /// framebuffer is left exactly as it was, so a host that sends a nonsense size gets its
    /// previous editor back rather than a blank one.
    pub fn resize(&mut self, size: PhysicalSize) -> DauxGraphicResult<()> {
        let pixels = size.area();
        if pixels > MAX_PIXELS {
            return Err(GraphicError::new(
                GraphicErrorKind::CapacityExceeded,
                format!(
                    "a {}x{} editor is {pixels} pixels, past the {MAX_PIXELS}-pixel ceiling",
                    size.width, size.height
                ),
            ));
        }
        // `pixels <= MAX_PIXELS` bounds this well inside `usize` on any platform this crate
        // builds for, so the multiplication cannot overflow.
        let bytes = pixels as usize * BYTES_PER_PIXEL;
        self.pixels.clear();
        self.pixels.resize(bytes, 0);
        self.size = size;
        Ok(())
    }
}

impl Default for SoftwareFramebuffer {
    /// [main-thread] [`SoftwareFramebuffer::empty`].
    fn default() -> Self {
        Self::empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size(w: u32, h: u32) -> PhysicalSize {
        PhysicalSize::new(w, h)
    }

    #[test]
    fn a_new_framebuffer_is_the_size_it_was_asked_for_and_fully_zeroed() {
        let fb = SoftwareFramebuffer::new(size(4, 3)).expect("small enough");
        assert_eq!(fb.size(), size(4, 3));
        assert_eq!(fb.row_pitch(), 16);
        assert_eq!(fb.pixels().len(), 48);
        assert!(fb.pixels().iter().all(|b| *b == 0));
        assert!(!fb.is_empty());
    }

    #[test]
    fn an_empty_framebuffer_allocates_nothing_and_reads_as_empty() {
        let fb = SoftwareFramebuffer::empty();
        assert!(fb.is_empty());
        assert_eq!(fb.pixels().len(), 0);
        assert_eq!(fb.row_pitch(), 0);
        assert_eq!(fb.pixel(0, 0), None);
        assert_eq!(SoftwareFramebuffer::default().size(), PhysicalSize::ZERO);
    }

    #[test]
    fn a_minimised_window_is_a_zero_sized_framebuffer_not_an_error() {
        let mut fb = SoftwareFramebuffer::new(size(10, 10)).expect("ok");
        fb.resize(size(0, 0)).expect("minimising is not a failure");
        assert!(fb.is_empty());
        assert!(fb.row_mut(0).is_none());
        fb.resize(size(2, 2)).expect("restoring works");
        assert_eq!(fb.pixels().len(), 16);
    }

    #[test]
    fn pixels_land_where_they_are_addressed() {
        let mut fb = SoftwareFramebuffer::new(size(3, 2)).expect("ok");
        fb.set_pixel(0, 0, [1, 2, 3, 4]);
        fb.set_pixel(2, 1, [9, 8, 7, 6]);
        assert_eq!(fb.pixel(0, 0), Some([1, 2, 3, 4]));
        assert_eq!(fb.pixel(2, 1), Some([9, 8, 7, 6]));
        assert_eq!(fb.pixel(1, 0), Some([0, 0, 0, 0]));

        // Row 0 is the top row and comes first in memory.
        assert_eq!(&fb.pixels()[0..4], &[1, 2, 3, 4]);
        assert_eq!(&fb.pixels()[20..24], &[9, 8, 7, 6]);
    }

    #[test]
    fn writes_outside_the_framebuffer_are_clipped_rather_than_panicking() {
        let mut fb = SoftwareFramebuffer::new(size(2, 2)).expect("ok");
        fb.set_pixel(2, 0, [255; 4]);
        fb.set_pixel(0, 2, [255; 4]);
        fb.set_pixel(u32::MAX, u32::MAX, [255; 4]);
        assert!(
            fb.pixels().iter().all(|b| *b == 0),
            "an out-of-bounds write reached the buffer"
        );
        assert_eq!(fb.pixel(2, 0), None);
        assert_eq!(fb.pixel(0, 2), None);
    }

    #[test]
    fn rows_are_contiguous_and_the_right_length() {
        let mut fb = SoftwareFramebuffer::new(size(5, 4)).expect("ok");
        for y in 0..4 {
            let row = fb.row_mut(y).unwrap_or_else(|| panic!("row {y} exists"));
            assert_eq!(row.len(), 20);
            row.fill(y as u8 + 1);
        }
        assert!(fb.row_mut(4).is_none());
        assert_eq!(fb.pixel(0, 0), Some([1, 1, 1, 1]));
        assert_eq!(fb.pixel(4, 3), Some([4, 4, 4, 4]));
    }

    #[test]
    fn filling_touches_every_pixel_and_nothing_more() {
        let mut fb = SoftwareFramebuffer::new(size(7, 3)).expect("ok");
        fb.fill([10, 20, 30, 255]);
        assert_eq!(fb.pixel(6, 2), Some([10, 20, 30, 255]));
        assert_eq!(fb.pixels().len(), 7 * 3 * 4);
        assert!(
            fb.pixels().chunks_exact(4).all(|p| p == [10, 20, 30, 255]),
            "fill left a pixel behind"
        );
    }

    #[test]
    fn a_hostile_size_is_refused_without_attempting_the_allocation() {
        // A host that reports `u32::MAX` on a side is asking for 64 EiB. Attempting it aborts
        // the process, which takes the DAW and the user's session with it.
        let mut fb = SoftwareFramebuffer::new(size(64, 64)).expect("ok");
        let before = fb.size();

        let err = fb
            .resize(size(u32::MAX, u32::MAX))
            .expect_err("that size must be refused");
        assert_eq!(err.kind(), GraphicErrorKind::CapacityExceeded);
        assert_eq!(
            fb.size(),
            before,
            "a refused resize must leave the framebuffer usable"
        );
        assert_eq!(fb.pixels().len(), 64 * 64 * 4);

        assert!(SoftwareFramebuffer::new(size(u32::MAX, 2)).is_err());
        assert!(SoftwareFramebuffer::new(size(2, u32::MAX)).is_err());
    }

    #[test]
    fn the_ceiling_is_exactly_where_it_is_documented() {
        // 8192x8192 is 64 Mpx: the largest allowed. One pixel more is refused.
        assert!(SoftwareFramebuffer::new(size(8192, 8192)).is_ok());
        assert_eq!(size(8192, 8192).area(), MAX_PIXELS);
        assert!(SoftwareFramebuffer::new(size(8193, 8192)).is_err());
    }

    #[test]
    fn shrinking_keeps_the_allocation_so_a_resize_drag_does_not_thrash() {
        let mut fb = SoftwareFramebuffer::new(size(400, 400)).expect("ok");
        let capacity = fb.pixels.capacity();
        assert!(capacity >= 400 * 400 * 4);

        fb.resize(size(100, 100)).expect("shrink");
        assert_eq!(fb.size(), size(100, 100));
        assert_eq!(fb.pixels().len(), 100 * 100 * 4);
        assert_eq!(
            fb.pixels.capacity(),
            capacity,
            "shrinking must not free and reallocate on the next frame"
        );

        fb.resize(size(400, 400)).expect("grow back");
        assert_eq!(
            fb.pixels.capacity(),
            capacity,
            "growing back within the allocation"
        );
    }

    #[test]
    fn a_resized_framebuffer_never_shows_stale_pixels() {
        let mut fb = SoftwareFramebuffer::new(size(4, 4)).expect("ok");
        fb.fill([0xAB; 4]);
        fb.resize(size(2, 2)).expect("shrink");
        fb.resize(size(4, 4)).expect("grow back");
        assert!(
            fb.pixels().iter().all(|b| *b == 0),
            "the old contents came back through the reused allocation"
        );
    }
}
