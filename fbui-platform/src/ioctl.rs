//! Raw Linux ioctl numbers and struct layouts not exposed by `libc`.
//!
//! Stable kernel ABI constants from `<linux/kd.h>`, `<linux/vt.h>`, and
//! `<linux/fb.h>`. The request numbers are literal constants in those headers
//! (not `_IO*`-encoded), so we hard-code them and call [`libc::ioctl`] directly.
//! This is the Phase 0 spike's `ioctl.rs` extended with the VT
//! mode-switching ioctls Phase 1 needs for cooperative VT handoff.

use std::io;
use std::os::unix::io::RawFd;

// ---- <linux/kd.h> ---------------------------------------------------------

/// Get the current console mode (text vs graphics).
pub const KDGETMODE: libc::c_ulong = 0x4B3B;
/// Set the console mode.
pub const KDSETMODE: libc::c_ulong = 0x4B3A;
pub const KD_TEXT: libc::c_int = 0x00;
pub const KD_GRAPHICS: libc::c_int = 0x01;

/// Get the current keyboard translation mode.
pub const KDGKBMODE: libc::c_ulong = 0x4B44;
/// Set the keyboard translation mode.
pub const KDSKBMODE: libc::c_ulong = 0x4B45;
pub const K_RAW: libc::c_int = 0x00;
pub const K_XLATE: libc::c_int = 0x01;
pub const K_MEDIUMRAW: libc::c_int = 0x02;
pub const K_UNICODE: libc::c_int = 0x03;
/// Fully mute the keyboard for the duration we own the VT.
pub const K_OFF: libc::c_int = 0x04;

// ---- <linux/vt.h> ---------------------------------------------------------

pub const VT_GETSTATE: libc::c_ulong = 0x5603;
pub const VT_SETMODE: libc::c_ulong = 0x5602;
pub const VT_RELDISP: libc::c_ulong = 0x5605;
// Kept for completeness / future explicit-VT-allocation use (Phase 4 hotplug).
#[allow(dead_code)]
pub const VT_OPENQRY: libc::c_ulong = 0x5600;
#[allow(dead_code)]
pub const VT_ACTIVATE: libc::c_ulong = 0x5606;
#[allow(dead_code)]
pub const VT_WAITACTIVE: libc::c_ulong = 0x5607;
#[allow(dead_code)]
pub const VT_GETMODE: libc::c_ulong = 0x5601;

/// `vt_mode.mode` values.
pub const VT_AUTO: libc::c_char = 0x00;
pub const VT_PROCESS: libc::c_char = 0x01;

/// `VT_RELDISP` arguments: ack a release request, or acknowledge an acquire.
pub const VT_RELDISP_RELEASE_OK: libc::c_int = 1;
pub const VT_ACKACQ: libc::c_int = 2;

/// `struct vt_mode` — how the kernel signals us on VT switch.
#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct VtMode {
    /// `VT_AUTO` (kernel switches freely) or `VT_PROCESS` (we mediate).
    pub mode: libc::c_char,
    /// If set, kernel waits for our ack before switching away.
    pub waitv: libc::c_char,
    /// Signal raised when we must release the VT.
    pub relsig: libc::c_short,
    /// Signal raised when we are given the VT back.
    pub acqsig: libc::c_short,
    /// Unused free-signal field.
    pub frsig: libc::c_short,
}

/// `struct vt_stat` — only the active VT field is interesting to us.
#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct VtStat {
    pub v_active: libc::c_ushort,
    pub v_signal: libc::c_ushort,
    pub v_state: libc::c_ushort,
}

// ---- <linux/fb.h> ---------------------------------------------------------
// Only the fbdev backend consumes these. They're ABI definitions, so allow them
// to sit unused when that backend isn't compiled in.
#[allow(unused_imports)]
pub use fb::*;

#[allow(dead_code)]
mod fb {
    pub const FBIOGET_VSCREENINFO: libc::c_ulong = 0x4600;
    #[allow(dead_code)]
    pub const FBIOPUT_VSCREENINFO: libc::c_ulong = 0x4601;
    pub const FBIOGET_FSCREENINFO: libc::c_ulong = 0x4602;
    pub const FBIOPAN_DISPLAY: libc::c_ulong = 0x4606;

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    pub struct FbBitfield {
        pub offset: u32,
        pub length: u32,
        pub msb_right: u32,
    }

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    pub struct FbVarScreeninfo {
        pub xres: u32,
        pub yres: u32,
        pub xres_virtual: u32,
        pub yres_virtual: u32,
        pub xoffset: u32,
        pub yoffset: u32,
        pub bits_per_pixel: u32,
        pub grayscale: u32,
        pub red: FbBitfield,
        pub green: FbBitfield,
        pub blue: FbBitfield,
        pub transp: FbBitfield,
        pub nonstd: u32,
        pub activate: u32,
        pub height: u32,
        pub width: u32,
        pub accel_flags: u32,
        pub pixclock: u32,
        pub left_margin: u32,
        pub right_margin: u32,
        pub upper_margin: u32,
        pub lower_margin: u32,
        pub hsync_len: u32,
        pub vsync_len: u32,
        pub sync: u32,
        pub vmode: u32,
        pub rotate: u32,
        pub colorspace: u32,
        pub reserved: [u32; 4],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct FbFixScreeninfo {
        pub id: [u8; 16],
        pub smem_start: libc::c_ulong,
        pub smem_len: u32,
        pub type_: u32,
        pub type_aux: u32,
        pub visual: u32,
        pub xpanstep: u16,
        pub ypanstep: u16,
        pub ywrapstep: u16,
        pub line_length: u32,
        pub mmio_start: libc::c_ulong,
        pub mmio_len: u32,
        pub accel: u32,
        pub capabilities: u16,
        pub reserved: [u16; 2],
    }

    impl Default for FbFixScreeninfo {
        fn default() -> Self {
            // SAFETY: an all-zero bit pattern is a valid `FbFixScreeninfo`.
            unsafe { std::mem::zeroed() }
        }
    }

    /// `activate` flag forcing the pan to take effect immediately.
    pub const FB_ACTIVATE_NOW: u32 = 0;
    /// `activate` flag requesting the driver vsync the pan.
    pub const FB_ACTIVATE_VBL: u32 = 0x10;
} // mod fb

// ---- thin ioctl wrappers --------------------------------------------------

/// `ioctl(fd, req, arg)` where `arg` is passed by value (e.g. an `int`).
pub fn ioctl_val(fd: RawFd, req: libc::c_ulong, arg: libc::c_int) -> io::Result<()> {
    // SAFETY: `fd` is a valid descriptor; `arg` matches the int-valued ioctls.
    let r = unsafe { libc::ioctl(fd, req as _, arg) };
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// `ioctl(fd, req, &mut arg)` for the int-out ioctls (KDGETMODE, KDGKBMODE).
pub fn ioctl_get_int(fd: RawFd, req: libc::c_ulong) -> io::Result<libc::c_int> {
    let mut out: libc::c_int = 0;
    // SAFETY: the kernel writes a single int through the pointer.
    let r = unsafe { libc::ioctl(fd, req as _, &mut out) };
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(out)
    }
}

/// `ioctl(fd, req, &mut arg)` for struct-valued ioctls.
///
/// # Safety
/// `T` must be the exact struct the kernel expects for `req`.
pub unsafe fn ioctl_ptr<T>(fd: RawFd, req: libc::c_ulong, arg: &mut T) -> io::Result<()> {
    let r = libc::ioctl(fd, req as _, arg as *mut T);
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
