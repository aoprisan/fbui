//! The fbui document viewer on a Raspberry Pi 3 with no operating system.
//!
//! Everything a Linux build gets from the kernel, this file does by hand —
//! and it's short, because `fbui-bare` asks so little of a board:
//!
//! * **memory** — an identity-mapped MMU with caches ([`mmu`]) and a heap
//!   for `alloc` ([`Heap`]);
//! * **display** — a 32 bpp framebuffer from the VideoCore firmware
//!   ([`mailbox`]), handed to fbui as a [`Framebuffer`] whose `flush`
//!   cleans the data cache over the damaged rows;
//! * **input** — the PL011 UART, decoding a terminal's keys ([`uart`]);
//! * **time and sleep** — the generic timer, and `wfi` woken by the UART or
//!   a timer compare ([`timer`]).
//!
//! The documents are compiled in. Run it with `scripts/run.sh` (QEMU
//! `raspi3b`) or copy `kernel8.img` to a Pi 3's SD card (README.md).

#![no_std]
#![no_main]

extern crate alloc;

mod boot;
mod mailbox;
mod mmu;
mod timer;
#[macro_use]
mod uart;

use alloc::collections::VecDeque;
use alloc::vec;
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::panic::PanicInfo;
use core::ptr::NonNull;

use fbui_bare::{Board, FbInfo, Framebuffer, Input};
use fbui_doc_viewer::{Entry, Viewer};
use fbui_render::geom::IRect;
use fbui_render::TargetFormat;

/// Screen mode asked of the firmware.
const WIDTH: u32 = 1024;
const HEIGHT: u32 = 768;
/// Heap size: generous — a page raster is width × height × 4 bytes, plus
/// clip masks and decoded images.
const HEAP_SIZE: usize = 384 * 1024 * 1024;

const FONT: &[u8] = include_bytes!("../../../fbui-render/fonts/Inter-Regular.ttf");

// ---- heap -------------------------------------------------------------

/// A single-core global allocator: no lock, because nothing else runs
/// (one core, no interrupt handlers).
struct Heap(UnsafeCell<linked_list_allocator::Heap>);

unsafe impl Sync for Heap {}

unsafe impl GlobalAlloc for Heap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        (*self.0.get())
            .allocate_first_fit(layout)
            .map_or(core::ptr::null_mut(), |p| p.as_ptr())
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if let Some(p) = NonNull::new(ptr) {
            (*self.0.get()).deallocate(p, layout);
        }
    }
}

#[global_allocator]
static HEAP: Heap = Heap(UnsafeCell::new(linked_list_allocator::Heap::empty()));

extern "C" {
    static __heap_start: u8;
}

// ---- display ----------------------------------------------------------

struct Screen {
    fb: mailbox::Fb,
}

impl Framebuffer for Screen {
    fn info(&self) -> FbInfo {
        FbInfo {
            width: self.fb.width,
            height: self.fb.height,
            // The firmware's pitch — never width * 4.
            stride: self.fb.pitch as usize,
            // Little-endian 0x00RRGGBB = bytes B, G, R, X: the firmware's
            // "BGR" order, which we ask for. (If it answers RGB anyway,
            // `flush` swaps the damaged pixels.)
            format: TargetFormat::Xrgb8888,
        }
    }

    fn pixels(&mut self) -> &mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self.fb.base as *mut u8, self.fb.size) }
    }

    fn flush(&mut self, damage: &[IRect]) {
        // The display engine reads RAM, not our cache.
        let pitch = self.fb.pitch as usize;
        let (w, h) = (self.fb.width as usize, self.fb.height as usize);
        for r in damage {
            let y0 = (r.y.max(0) as usize).min(h);
            let y1 = ((r.y + r.h as i32).max(0) as usize).min(h);
            if !self.fb.rgb_swap_needed() {
                mmu::clean(self.fb.base + y0 * pitch, (y1 - y0) * pitch);
                continue;
            }
            let x0 = (r.x.max(0) as usize).min(w);
            let x1 = ((r.x + r.w as i32).max(0) as usize).min(w);
            let base = self.fb.base;
            for y in y0..y1 {
                let row = unsafe {
                    core::slice::from_raw_parts_mut((base + y * pitch) as *mut u8, w * 4)
                };
                for px in row[x0 * 4..x1 * 4].chunks_exact_mut(4) {
                    px.swap(0, 2);
                }
            }
            mmu::clean(base + y0 * pitch, (y1 - y0) * pitch);
        }
    }
}

// ---- input + time -----------------------------------------------------

struct Pi3 {
    keys: uart::Keys,
    queue: VecDeque<Input>,
}

impl Board for Pi3 {
    fn poll_input(&mut self) -> Option<Input> {
        let now = timer::now_ms();
        while let Some(b) = uart::get() {
            if let Some(k) = self.keys.feed(b, now) {
                self.queue.push_back(Input::KeyTap(k));
            }
        }
        if let Some(k) = self.keys.tick(now) {
            self.queue.push_back(Input::KeyTap(k));
        }
        self.queue.pop_front()
    }

    fn now_ms(&self) -> u64 {
        timer::now_ms()
    }

    fn wait(&mut self, deadline_ms: Option<u64>) {
        let mut deadline = deadline_ms;
        if self.keys.pending_escape() {
            let esc = timer::now_ms() + uart::ESC_TIMEOUT_MS;
            deadline = Some(deadline.map_or(esc, |d| d.min(esc)));
        }
        timer::sleep_until(deadline, uart::has_data);
    }
}

// ---- entry ------------------------------------------------------------

#[no_mangle]
pub extern "C" fn kernel_main() -> ! {
    unsafe { mmu::enable() };
    uart::init();
    println!("\nfbui doc-viewer: bare-metal Raspberry Pi 3");

    unsafe {
        let start = core::ptr::addr_of!(__heap_start) as *mut u8;
        (*HEAP.0.get()).init(start, HEAP_SIZE);
        println!("heap: {:#x} + {} MiB", start as usize, HEAP_SIZE >> 20);
    }

    let fb = match mailbox::framebuffer(WIDTH, HEIGHT) {
        Some(fb) => fb,
        None => panic!("the firmware refused a {WIDTH}x{HEIGHT} framebuffer"),
    };
    println!(
        "framebuffer: {}x{} pitch {} at {:#x} ({} order)",
        fb.width,
        fb.height,
        fb.pitch,
        fb.base,
        if fb.rgb { "RGB" } else { "BGR" }
    );
    timer::init_wake();

    let fixtures = |name: &str, data: &'static [u8]| Entry::new(name, data);
    let entries = vec![
        fixtures(
            "fpdf2.pdf",
            include_bytes!("../../../fbui-doc/tests/fixtures/fpdf2.pdf"),
        ),
        fixtures(
            "reportlab.pdf",
            include_bytes!("../../../fbui-doc/tests/fixtures/reportlab.pdf"),
        ),
        fixtures(
            "swatch.png",
            include_bytes!("../../../fbui-doc/tests/fixtures/swatch.png"),
        ),
    ];
    println!(
        "{} documents; keys: arrows, PgUp/PgDn, space, +/-, w, p, Esc, Enter",
        entries.len()
    );

    let mut screen = Screen { fb };
    let mut board = Pi3 {
        keys: uart::Keys::default(),
        queue: VecDeque::new(),
    };
    fbui_bare::run(Viewer::new(entries, FONT), &mut screen, &mut board, 1.0)
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("\nPANIC: {info}");
    loop {
        unsafe { core::arch::asm!("wfi") };
    }
}
