//! An identity map with caches on. Not optional: with the MMU off every
//! access is Device memory, where the unaligned loads compiled Rust freely
//! emits fault. RAM is Normal write-back; the peripheral window is Device.

use core::arch::asm;

/// Level-1 table (1 GiB per entry) and the level-2 table (2 MiB per entry)
/// that splits the first GiB into RAM and the peripheral window.
#[repr(C, align(4096))]
struct Table([u64; 512]);

static mut L1: Table = Table([0; 512]);
static mut L2: Table = Table([0; 512]);

/// Peripherals start here on the BCM2837.
const PERIPHERAL_BASE: u64 = 0x3F00_0000;

const VALID: u64 = 1;
const TABLE: u64 = 1 << 1;
const BLOCK: u64 = 0;
const AF: u64 = 1 << 10;
const INNER_SHAREABLE: u64 = 3 << 8;
const OUTER_SHAREABLE: u64 = 2 << 8;
const ATTR_NORMAL: u64 = 0 << 2; // MAIR index 0
const ATTR_DEVICE: u64 = 1 << 2; // MAIR index 1
const PXN_UXN: u64 = (1 << 53) | (1 << 54);

/// Build the tables and turn on the MMU and caches (EL1).
///
/// # Safety
/// Call once, early, on the boot core, before anything else touches memory
/// through the cache.
pub unsafe fn enable() {
    let l1 = &raw mut L1;
    let l2 = &raw mut L2;
    for i in 0..512u64 {
        let addr = i << 21;
        let entry = if addr >= PERIPHERAL_BASE {
            addr | VALID | BLOCK | AF | ATTR_DEVICE | OUTER_SHAREABLE | PXN_UXN
        } else {
            addr | VALID | BLOCK | AF | ATTR_NORMAL | INNER_SHAREABLE
        };
        (*l2).0[i as usize] = entry;
    }
    (*l1).0[0] = (l2 as u64) | VALID | TABLE;
    // 1-2 GiB: the ARM-local peripherals (0x4000_0000: timers, mailboxes).
    (*l1).0[1] = (1u64 << 30) | VALID | BLOCK | AF | ATTR_DEVICE | OUTER_SHAREABLE | PXN_UXN;

    // MAIR: attr0 = Normal WB RW-allocate, attr1 = Device-nGnRnE.
    let mair: u64 = 0xFF; // attr1 (bits 8..16) = 0x00
    // TCR: T0SZ=25 (39-bit VA, walks start at L1), 4 KiB granule,
    // inner/outer WB cacheable walks, inner shareable, TTBR1 walks off,
    // 32-bit physical addresses.
    let tcr: u64 = 25 | (1 << 8) | (1 << 10) | (3 << 12) | (1 << 23);
    asm!(
        "msr mair_el1, {mair}",
        "msr tcr_el1, {tcr}",
        "msr ttbr0_el1, {ttbr}",
        "dsb ish",
        "isb",
        "tlbi vmalle1",
        "dsb ish",
        "isb",
        mair = in(reg) mair,
        tcr = in(reg) tcr,
        ttbr = in(reg) l1 as u64,
    );
    // SCTLR_EL1: M (MMU), C (data cache), I (instruction cache); A=0 so
    // unaligned Normal accesses are fine.
    let mut sctlr: u64;
    asm!("mrs {}, sctlr_el1", out(reg) sctlr);
    sctlr |= (1 << 0) | (1 << 2) | (1 << 12);
    sctlr &= !(1 << 1);
    asm!("msr sctlr_el1, {}", "isb", in(reg) sctlr);
}

/// Clean the data cache for `[start, start + len)` to the point of
/// coherency, so a non-coherent observer (the display engine scanning the
/// framebuffer, the VideoCore reading a mailbox buffer) sees the writes.
pub fn clean(start: usize, len: usize) {
    const LINE: usize = 64;
    let mut a = start & !(LINE - 1);
    let end = start + len;
    while a < end {
        unsafe { asm!("dc cvac, {}", in(reg) a) };
        a += LINE;
    }
    unsafe { asm!("dsb sy") };
}

/// Invalidate the data cache for a range (after the VideoCore wrote it).
pub fn invalidate(start: usize, len: usize) {
    const LINE: usize = 64;
    let mut a = start & !(LINE - 1);
    let end = start + len;
    while a < end {
        unsafe { asm!("dc civac, {}", in(reg) a) };
        a += LINE;
    }
    unsafe { asm!("dsb sy") };
}
