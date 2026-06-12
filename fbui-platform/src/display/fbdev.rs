//! Legacy fbdev fallback (`/dev/fb0`) behind the same [`Display`] trait.
//!
//! fbdev is deprecated kernel-side, but it's the universal lowest common
//! denominator — many simple panels and headless-ish setups expose only this.
//! Promotes the Phase 0 spike's `fbdev.rs`: stride comes from the kernel
//! (`fix.line_length`, never computed), and double-buffering — when the driver
//! gives us `yres_virtual >= 2*yres` and a pan step — is done by panning with
//! `FBIOPAN_DISPLAY` (which vsyncs on most drivers).
//!
//! Unlike DRM there's no fd to poll for completion: the pan ioctl blocks until
//! vblank itself, so [`present_fd`] is `None` and the event loop paces off its
//! timers instead. We hand the caller the *draw page* of the mmap as the back
//! buffer; the renderer's shadow + whole-row copy discipline still applies
//! because the mmap is the same write-combined device memory.
//!
//! [`present_fd`]: Display::present_fd

use std::os::unix::io::{AsRawFd, BorrowedFd, OwnedFd, RawFd};

use super::{BackendKind, Display, DisplayInfo, Frame};
use crate::error::{Error, Result};
use crate::format::PixelFormat;
use crate::geom::{Rect, Size};
use crate::ioctl::*;

struct FbMap {
    ptr: *mut u8,
    len: usize,
}

impl FbMap {
    fn map(fd: RawFd, len: usize) -> Result<Self> {
        // SAFETY: mmap the framebuffer's smem region, shared read/write.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(Error::last_os("mmap /dev/fb0"));
        }
        Ok(FbMap {
            ptr: ptr as *mut u8,
            len,
        })
    }

    fn slice(&mut self) -> &mut [u8] {
        // SAFETY: valid mapping of `len` bytes owned by self.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for FbMap {
    fn drop(&mut self) {
        // SAFETY: unmapping our own mapping.
        unsafe {
            libc::munmap(self.ptr as *mut libc::c_void, self.len);
        }
    }
}

/// The legacy framebuffer display.
pub struct FbdevDisplay {
    // Keep the node open for the life of the display; `_file` owns the fd.
    _file: OwnedFd,
    fd: RawFd,
    map: FbMap,
    var: FbVarScreeninfo,
    info: DisplayInfo,
    line_length: usize,
    page_stride: usize,
    /// Whether the driver lets us pan between two pages.
    can_pan: bool,
    /// Page currently being drawn into (0 or 1; always 0 if `!can_pan`).
    draw_page: usize,
}

impl FbdevDisplay {
    pub fn open(path: &str) -> Result<Self> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| Error::device(format!("open {path}"), e))?;
        let owned = OwnedFd::from(file);
        let fd = owned.as_raw_fd();

        let mut var = FbVarScreeninfo::default();
        let mut fix = FbFixScreeninfo::default();
        // SAFETY: correct struct types for these fb ioctls.
        unsafe {
            ioctl_ptr(fd, FBIOGET_VSCREENINFO, &mut var)
                .map_err(|e| Error::io("FBIOGET_VSCREENINFO", e))?;
            ioctl_ptr(fd, FBIOGET_FSCREENINFO, &mut fix)
                .map_err(|e| Error::io("FBIOGET_FSCREENINFO", e))?;
        }

        let format = pixel_format_from_var(&var)?;
        let size = Size::new(var.xres, var.yres);
        let line_length = fix.line_length as usize;
        let can_pan = var.yres_virtual >= var.yres * 2 && fix.ypanstep > 0;
        let page_stride = var.yres as usize * line_length;

        let map = FbMap::map(fd, fix.smem_len as usize)?;

        let info = DisplayInfo {
            size,
            format,
            refresh_mhz: 0, // fbdev doesn't report a reliable refresh
            buffers: if can_pan { 2 } else { 1 },
            backend: BackendKind::Fbdev,
        };

        Ok(FbdevDisplay {
            _file: owned,
            fd,
            map,
            var,
            info,
            line_length,
            page_stride,
            can_pan,
            draw_page: 0,
        })
    }
}

impl Display for FbdevDisplay {
    fn info(&self) -> DisplayInfo {
        self.info
    }

    fn begin_frame(&mut self) -> Result<Option<Frame<'_>>> {
        let size = self.info.size;
        let format = self.info.format;
        let stride = self.line_length;
        let base = self.draw_page * self.page_stride;
        let rows = size.h as usize * self.line_length;
        // Double-buffered fbdev never has trustworthy back-page contents after a
        // pan (we only ever drew the *other* page last), so age is always 0:
        // the render layer repaints the full surface. Quantifying real fbdev age
        // isn't worth it — DRM is the fast path.
        let buffer = &mut self.map.slice()[base..base + rows];
        Ok(Some(Frame {
            buffer,
            stride,
            size,
            format,
            age: 0,
        }))
    }

    fn present(&mut self, _damage: &[Rect]) -> Result<()> {
        if !self.can_pan {
            // Single-buffered: the draw already landed on screen. The event loop
            // paces frames via its timer; nothing to flip.
            return Ok(());
        }
        self.var.yoffset = (self.draw_page * self.info.size.h as usize) as u32;
        self.var.activate = FB_ACTIVATE_VBL;
        // SAFETY: FBIOPAN_DISPLAY takes a fb_var_screeninfo.
        unsafe {
            if ioctl_ptr(self.fd, FBIOPAN_DISPLAY, &mut self.var).is_err() {
                // Some drivers reject VBL; fall back to an immediate pan.
                self.var.activate = FB_ACTIVATE_NOW;
                ioctl_ptr(self.fd, FBIOPAN_DISPLAY, &mut self.var)
                    .map_err(|e| Error::io("FBIOPAN_DISPLAY", e))?;
            }
        }
        self.draw_page ^= 1;
        Ok(())
    }

    fn present_fd(&self) -> Option<BorrowedFd<'_>> {
        None
    }

    fn dispatch_present(&mut self) -> Result<bool> {
        Ok(false)
    }

    fn reconfigure(&mut self) -> Result<Option<DisplayInfo>> {
        // Re-read the kernel's screeninfo; a console mode change (or a panel that
        // re-trains) updates these. Cheap ioctls, safe to poll.
        let mut var = FbVarScreeninfo::default();
        let mut fix = FbFixScreeninfo::default();
        // SAFETY: correct struct types for these fb ioctls.
        unsafe {
            ioctl_ptr(self.fd, FBIOGET_VSCREENINFO, &mut var)
                .map_err(|e| Error::io("FBIOGET_VSCREENINFO", e))?;
            ioctl_ptr(self.fd, FBIOGET_FSCREENINFO, &mut fix)
                .map_err(|e| Error::io("FBIOGET_FSCREENINFO", e))?;
        }
        let format = pixel_format_from_var(&var)?;
        let size = Size::new(var.xres, var.yres);
        let line_length = fix.line_length as usize;
        if size == self.info.size && format == self.info.format && line_length == self.line_length {
            return Ok(None);
        }

        // Geometry changed: re-map at the new size and update our view of it.
        // Assigning a fresh map drops (munmaps) the old one.
        self.map = FbMap::map(self.fd, fix.smem_len as usize)?;
        self.can_pan = var.yres_virtual >= var.yres * 2 && fix.ypanstep > 0;
        self.page_stride = var.yres as usize * line_length;
        self.line_length = line_length;
        self.var = var;
        self.draw_page = 0;
        self.info.size = size;
        self.info.format = format;
        self.info.buffers = if self.can_pan { 2 } else { 1 };
        Ok(Some(self.info))
    }
}

/// Map the kernel's `var` bitfields to one of our [`PixelFormat`]s, or reject
/// layouts the platform doesn't pack for. We only accept the common cases the
/// spike validated; anything exotic is an explicit `Unsupported` rather than a
/// silently-wrong image.
fn pixel_format_from_var(var: &FbVarScreeninfo) -> Result<PixelFormat> {
    let bpp = var.bits_per_pixel;
    let is = |bf: &FbBitfield, off: u32, len: u32| bf.offset == off && bf.length == len;
    match bpp {
        32 if is(&var.red, 16, 8) && is(&var.green, 8, 8) && is(&var.blue, 0, 8) => {
            // Distinguish XRGB vs ARGB by whether the driver advertises alpha.
            if var.transp.length == 8 {
                Ok(PixelFormat::Argb8888)
            } else {
                Ok(PixelFormat::Xrgb8888)
            }
        }
        16 if is(&var.red, 11, 5) && is(&var.green, 5, 6) && is(&var.blue, 0, 5) => {
            Ok(PixelFormat::Rgb565)
        }
        other => Err(Error::unsupported(format!(
            "fbdev pixel layout: {other}bpp R@{}/{} G@{}/{} B@{}/{}",
            var.red.offset,
            var.red.length,
            var.green.offset,
            var.green.length,
            var.blue.offset,
            var.blue.length,
        ))),
    }
}
