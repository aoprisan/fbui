//! Legacy fbdev fallback (`/dev/fb0`).
//!
//! fbdev is deprecated kernel-side, but it's the universal lowest common
//! denominator, so Phase 0 validates its quirks: the **stride comes from the
//! kernel** (`fix.line_length`), never computed as `xres * bpp/8`; pixel
//! packing honors the `var` bitfields; and double-buffering, when the driver
//! exposes `yres_virtual >= 2*yres`, is done by panning with `FBIOPAN_DISPLAY`
//! (which vsyncs on most drivers).

use std::io;
use std::os::unix::io::{AsRawFd, RawFd};
use std::time::Duration;

use crate::ioctl::*;
use crate::scene::{Shadow, Timing};
use crate::Args;
use crate::RUNNING;

/// Pack one channel value (0..=255) into its bitfield position.
#[inline]
fn pack(val: u32, bf: &FbBitfield) -> u32 {
    let v = if bf.length >= 8 {
        val << (bf.length - 8)
    } else {
        val >> (8 - bf.length)
    };
    (v & ((1 << bf.length) - 1)) << bf.offset
}

struct FbMap {
    ptr: *mut u8,
    len: usize,
}

impl FbMap {
    fn map(fd: RawFd, len: usize) -> io::Result<Self> {
        // SAFETY: mmap the framebuffer's smem region for shared read/write.
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
            return Err(io::Error::last_os_error());
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

/// Blit the shadow into the framebuffer at a page offset, packing per the
/// kernel-reported bitfields and honoring `line_length` stride.
fn blit_fb(
    shadow: &Shadow,
    dst: &mut [u8],
    page_base: usize,
    line_length: usize,
    bytes_pp: usize,
    var: &FbVarScreeninfo,
) -> Duration {
    use std::time::Instant;
    let t = Instant::now();
    // Fast path: the common 32bpp BGRX/XRGB layout (red@16, green@8, blue@0)
    // is exactly our shadow's native byte order, so we can copy whole rows.
    let native_xrgb = bytes_pp == 4
        && var.red.offset == 16
        && var.green.offset == 8
        && var.blue.offset == 0
        && var.red.length == 8
        && var.green.length == 8
        && var.blue.length == 8;

    for y in 0..shadow.h {
        let src = &shadow.px[y * shadow.w..y * shadow.w + shadow.w];
        let row_off = page_base + y * line_length;
        if native_xrgb {
            let row_bytes = shadow.w * 4;
            // SAFETY: same-size reinterpret of u32 row as bytes.
            let sb = unsafe {
                std::slice::from_raw_parts(src.as_ptr() as *const u8, row_bytes)
            };
            dst[row_off..row_off + row_bytes].copy_from_slice(sb);
        } else {
            for (x, &c) in src.iter().enumerate() {
                let r = (c >> 16) & 0xFF;
                let g = (c >> 8) & 0xFF;
                let b = c & 0xFF;
                let packed =
                    pack(r, &var.red) | pack(g, &var.green) | pack(b, &var.blue);
                let p = row_off + x * bytes_pp;
                dst[p..p + bytes_pp].copy_from_slice(&packed.to_le_bytes()[..bytes_pp]);
            }
        }
    }
    t.elapsed()
}

pub fn run(args: &Args) -> io::Result<()> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&args.device)?;
    let fd = file.as_raw_fd();

    let mut var = FbVarScreeninfo::default();
    let mut fix = FbFixScreeninfo::default();
    // SAFETY: correct struct types for these fb ioctls.
    unsafe {
        ioctl_ptr(fd, FBIOGET_VSCREENINFO, &mut var)?;
        ioctl_ptr(fd, FBIOGET_FSCREENINFO, &mut fix)?;
    }

    let w = var.xres as usize;
    let h = var.yres as usize;
    let bytes_pp = (var.bits_per_pixel / 8) as usize;
    let line_length = fix.line_length as usize;
    if bytes_pp == 0 || (bytes_pp != 2 && bytes_pp != 4) {
        return Err(io::Error::other(format!(
            "unsupported bpp {} (spike handles 16/32)",
            var.bits_per_pixel
        )));
    }

    // Double-buffer only if the driver gives us a tall enough virtual area.
    let can_pan = var.yres_virtual >= var.yres * 2 && fix.ypanstep > 0;
    let page_stride = h * line_length;

    let id = String::from_utf8_lossy(&fix.id);
    eprintln!(
        "[fbdev] {} \"{}\": {}x{} {}bpp, stride {} B (kernel line_length), {}",
        args.device,
        id.trim_end_matches('\0').trim(),
        w,
        h,
        var.bits_per_pixel,
        line_length,
        if can_pan {
            "double-buffered via FBIOPAN_DISPLAY"
        } else {
            "single-buffered (no pan room / driver support)"
        }
    );
    eprintln!(
        "[fbdev] bitfields: R off{} len{}, G off{} len{}, B off{} len{}",
        var.red.offset, var.red.length, var.green.offset, var.green.length, var.blue.offset, var.blue.length,
    );

    let mut fbmap = FbMap::map(fd, fix.smem_len as usize)?;
    let mut shadow = Shadow::new(w, h);
    let mut timing = Timing::default();
    timing.start();

    let mut page = 0usize; // page currently being drawn into
    let mut frame: u64 = 0;

    while RUNNING.load(std::sync::atomic::Ordering::Relaxed) {
        if let Some(secs) = args.seconds {
            let elapsed = timing.started.map(|s| s.elapsed().as_secs_f64()).unwrap_or(0.0);
            if elapsed >= secs {
                break;
            }
        }

        let r = shadow.render_bars(frame);
        let draw_page = if can_pan { page } else { 0 };
        let b = blit_fb(
            &shadow,
            fbmap.slice(),
            draw_page * page_stride,
            line_length,
            bytes_pp,
            &var,
        );
        timing.record(r, b);

        if let Some(pf) = args.panic_after {
            if frame >= pf {
                panic!("[fbdev] deliberate panic after {pf} frames (console-restore test)");
            }
        }

        if can_pan {
            // Flip: show the page we just drew, vsync via VBL activate.
            var.yoffset = (draw_page * h) as u32;
            var.activate = FB_ACTIVATE_VBL;
            // SAFETY: FBIOPAN_DISPLAY takes a fb_var_screeninfo.
            unsafe {
                if ioctl_ptr(fd, FBIOPAN_DISPLAY, &mut var).is_err() {
                    // Some drivers reject VBL; fall back to NOW.
                    var.activate = FB_ACTIVATE_NOW;
                    let _ = ioctl_ptr(fd, FBIOPAN_DISPLAY, &mut var);
                }
            }
            page ^= 1;
        } else {
            // No vsync available; pace ourselves to ~60 Hz so the scroll is
            // smooth-ish and we don't spin the CPU.
            std::thread::sleep(Duration::from_micros(16_666));
        }
        frame += 1;
    }

    timing.report("fbdev");
    Ok(())
}
