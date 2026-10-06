//! The VideoCore mailbox property interface (channel 8): how a bare-metal
//! program asks the GPU firmware for a framebuffer.

use core::ptr::{read_volatile, write_volatile};

use crate::mmu;

const MBOX_BASE: usize = 0x3F00_B880;
const READ: usize = MBOX_BASE;
const STATUS: usize = MBOX_BASE + 0x18;
const WRITE: usize = MBOX_BASE + 0x20;
const FULL: u32 = 0x8000_0000;
const EMPTY: u32 = 0x4000_0000;
const CHANNEL_PROPERTY: u32 = 8;

#[repr(C, align(16))]
struct Buffer([u32; 36]);

static mut BUF: Buffer = Buffer([0; 36]);

/// A framebuffer the firmware allocated.
pub struct Fb {
    pub base: usize,
    pub size: usize,
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
    /// 1 = RGB, 0 = BGR (byte order within the 32-bit pixel).
    pub rgb: bool,
}

fn call(buf: &mut [u32; 36]) -> bool {
    let addr = buf.as_ptr() as usize;
    mmu::clean(addr, core::mem::size_of_val(buf));
    unsafe {
        while read_volatile(STATUS as *const u32) & FULL != 0 {}
        // The low 4 bits carry the channel; the buffer is 16-byte aligned.
        write_volatile(WRITE as *mut u32, (addr as u32 & !0xF) | CHANNEL_PROPERTY);
        loop {
            while read_volatile(STATUS as *const u32) & EMPTY != 0 {}
            let v = read_volatile(READ as *const u32);
            if v & 0xF == CHANNEL_PROPERTY {
                break;
            }
        }
    }
    mmu::invalidate(addr, core::mem::size_of_val(buf));
    buf[1] == 0x8000_0000
}

impl Fb {
    /// The firmware kept RGB byte order despite our request: fbui's
    /// XRGB8888 output must have red and blue swapped on the way out.
    pub fn rgb_swap_needed(&self) -> bool {
        self.rgb
    }
}

/// Ask for a `width`×`height`, 32 bpp framebuffer.
pub fn framebuffer(width: u32, height: u32) -> Option<Fb> {
    // Through a raw pointer: references to a `static mut` are refused.
    #[allow(clippy::deref_addrof)]
    let buf = unsafe { &mut (*(&raw mut BUF)).0 };
    let msg: [u32; 35] = [
        35 * 4, // total size
        0,      // request
        0x48003,
        8,
        8,
        width,
        height, // physical size
        0x48004,
        8,
        8,
        width,
        height, // virtual size
        0x48009,
        8,
        8,
        0,
        0, // virtual offset
        0x48005,
        4,
        4,
        32, // depth
        0x48006,
        4,
        4,
        0, // pixel order: BGR (= XRGB8888 little-endian)
        0x40001,
        8,
        8,
        4096,
        0, // allocate buffer (align 4096) -> base, size
        0x40008,
        4,
        4,
        0, // get pitch
        0, // end tag
    ];
    buf[..35].copy_from_slice(&msg);
    if !call(buf) || buf[28] == 0 {
        return None;
    }
    Some(Fb {
        // The firmware returns a VideoCore bus address; strip the alias.
        base: (buf[28] & 0x3FFF_FFFF) as usize,
        size: buf[29] as usize,
        width: buf[5],
        height: buf[6],
        pitch: buf[33],
        rgb: buf[24] == 1,
    })
}
