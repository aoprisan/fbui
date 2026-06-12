//! The display abstraction: one trait, ignorant of DRM vs fbdev.
//!
//! This is the stable foundation Phase 1 exists to produce. Everything above it
//! (`fbui-render`, `fbui-widgets`) talks only to [`Display`] and never learns
//! whether it is page-flipping KMS dumb buffers or panning a legacy framebuffer.
//!
//! ## The frame cycle
//!
//! ```text
//!   loop {
//!       let frame = display.begin_frame()?;   // mapped back buffer + stride + age
//!       render_into(frame.buffer, frame.stride, frame.age);  // sequential writes only
//!       display.present(&damage)?;            // queue flip / pan (vsynced)
//!       // ... wait for present-complete (event loop polls present_fd) ...
//!   }
//! ```
//!
//! Two design facts inherited verbatim from the Phase 0 spike NOTES:
//!
//! * **Stride is never computed.** [`Frame::stride`] is whatever the kernel
//!   reported (DRM `pitch`, fbdev `line_length`); callers must never assume
//!   `width * bpp`.
//! * **Write forward only.** The back buffer may be write-combined/uncached
//!   device memory, where scattered sub-word writes are murder. Callers keep
//!   their own normal-RAM shadow and copy whole damaged rows in — [`Frame`]
//!   hands out a `&mut [u8]` shaped to encourage exactly that.
//!
//! [`Frame::age`] is the EGL-style buffer-age hint that makes partial redraw
//! correct under double buffering: it says how many presents ago this buffer
//! last held valid contents (`0` = undefined, repaint everything).

use std::os::unix::io::BorrowedFd;

use crate::error::Result;
use crate::format::PixelFormat;
use crate::geom::{Rect, Size};

#[cfg(feature = "drm-backend")]
pub mod drm;
#[cfg(feature = "fbdev")]
pub mod fbdev;

/// Static properties of the scanout, fixed for the life of a [`Display`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayInfo {
    /// Active resolution in physical pixels.
    pub size: Size,
    /// Byte layout of presented pixels.
    pub format: PixelFormat,
    /// Refresh rate in millihertz (e.g. `60_000` for 60 Hz), `0` if unknown.
    pub refresh_mhz: u32,
    /// How many buffers the backend cycles through. `2` = double-buffered
    /// (flip/pan), `1` = single-buffered (the render layer must avoid tearing
    /// itself or accept it).
    pub buffers: u8,
    /// Which backend is driving this display, for logging/diagnostics.
    pub backend: BackendKind,
}

/// Which concrete backend a [`Display`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    /// DRM/KMS dumb buffers with page-flipping — the primary, vsynced path.
    DrmDumb,
    /// Legacy `/dev/fb0` mmap, optionally double-buffered by panning.
    Fbdev,
}

/// A back buffer borrowed for the duration of one frame.
///
/// The lifetime ties the mapping to `&mut Display`, so a `Frame` cannot outlive
/// the [`Display`] and two frames cannot be live at once.
pub struct Frame<'a> {
    /// The mapped back buffer. `stride * size.h` bytes; write **forward only**.
    pub buffer: &'a mut [u8],
    /// Bytes per row, kernel-reported — **not** `size.w * bpp`.
    pub stride: usize,
    /// Dimensions of the surface.
    pub size: Size,
    /// Pixel packing for this buffer.
    pub format: PixelFormat,
    /// Buffer-age hint: presents since this buffer last held defined contents.
    /// `0` means "contents undefined, repaint the whole surface"; `N` means the
    /// buffer still holds the frame from `N` presents ago, so the caller need
    /// only repaint the union of damage from the last `N` frames.
    pub age: u32,
}

impl Frame<'_> {
    /// Mutable view of row `y` (its first `size.w * bpp` bytes — the part that
    /// is actually scanned out, excluding any stride padding).
    pub fn row(&mut self, y: u32) -> &mut [u8] {
        let bpp = self.format.bytes_per_pixel();
        let off = y as usize * self.stride;
        &mut self.buffer[off..off + self.size.w as usize * bpp]
    }
}

/// A display surface fbui can render to and present.
///
/// Object-safe on purpose: the platform stores a `Box<dyn Display>` so the
/// backend is chosen at runtime (DRM first, fbdev fallback) without leaking the
/// choice into the type of everything above.
pub trait Display {
    /// Immutable scanout properties.
    fn info(&self) -> DisplayInfo;

    /// Acquire the back buffer for drawing.
    ///
    /// Returns [`Error::WouldBlock`](crate::Error) shape via `Ok(None)` if a
    /// previous [`present`](Display::present) is still in flight and no buffer
    /// is free yet — the caller should wait on [`present_fd`](Display::present_fd)
    /// and retry. Single-buffered backends always return a buffer.
    fn begin_frame(&mut self) -> Result<Option<Frame<'_>>>;

    /// Present the buffer drawn since the last [`begin_frame`](Display::begin_frame).
    ///
    /// `damage` is the set of rectangles that actually changed; backends may use
    /// it to limit copy-out, but must present a coherent full frame regardless.
    /// On double-buffered backends this queues a vsynced flip/pan and returns
    /// immediately; completion arrives via [`dispatch_present`](Display::dispatch_present).
    fn present(&mut self, damage: &[Rect]) -> Result<()>;

    /// Descriptor to poll for present-completion, if the backend has one.
    ///
    /// DRM returns its card fd (page-flip events arrive here); fbdev returns
    /// `None` because its pan ioctl blocks until vblank itself.
    fn present_fd(&self) -> Option<BorrowedFd<'_>>;

    /// Drain present-completion events after [`present_fd`](Display::present_fd)
    /// signalled readable. Returns `true` if at least one present completed
    /// (a buffer became free). No-op returning `false` for fd-less backends.
    fn dispatch_present(&mut self) -> Result<bool>;

    /// Release the scanout resources without dropping the backend, for a VT
    /// switch *away* (we lose DRM master). Rendering must stop until
    /// [`resume`](Display::resume). Default: nothing to do.
    fn suspend(&mut self) -> Result<()> {
        Ok(())
    }

    /// Re-acquire the scanout after a VT switch *back*: restore the mode/CRTC
    /// and force the next frame to be a full repaint. Default: nothing to do.
    fn resume(&mut self) -> Result<()> {
        Ok(())
    }
}
