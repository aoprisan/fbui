//! Color-bar scene + shadow-buffer discipline.
//!
//! The scene is deliberately chosen to make tearing obvious: classic vertical
//! color bars that scroll horizontally every frame, plus a 2px white sentinel
//! line that sweeps across the screen. If a page flip isn't synced to vblank,
//! the scroll produces a visible diagonal tear and the sentinel line breaks.
//!
//! The "shadow-buffer discipline" from the plan: never render directly into
//! the framebuffer's write-combined / uncached mapping (random writes there
//! are murder). Render into a normal-RAM `Shadow`, then `memcpy` whole rows
//! into the device mapping. We instrument both phases so Phase 0 can record
//! the cost of each path on real hardware.

use std::time::{Duration, Instant};

/// SMPTE-ish bar palette, top byte ignored (XRGB). 0x00RRGGBB.
const BARS: [u32; 8] = [
    0x00FF_FFFF, // white
    0x00FF_FF00, // yellow
    0x0000_FFFF, // cyan
    0x0000_FF00, // green
    0x00FF_00FF, // magenta
    0x00FF_0000, // red
    0x0000_00FF, // blue
    0x0000_0000, // black
];

/// Normal-RAM scratch buffer, one `u32` (0x00RRGGBB) per pixel.
pub struct Shadow {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u32>,
}

impl Shadow {
    pub fn new(w: usize, h: usize) -> Self {
        Shadow {
            w,
            h,
            px: vec![0; w * h],
        }
    }

    /// Paint the animated bars for the given frame index into the shadow RAM.
    /// Returns the time spent painting.
    pub fn render_bars(&mut self, frame: u64) -> Duration {
        let t = Instant::now();
        let w = self.w;
        let h = self.h;
        let bar_w = (w / 8).max(1);
        // Scroll 2px/frame; sentinel sweeps the full width on its own period.
        let scroll = (frame as usize * 2) % w;
        let sentinel = (frame as usize * 7) % w;

        for y in 0..h {
            let row = &mut self.px[y * w..y * w + w];
            for (x, px) in row.iter_mut().enumerate() {
                let sx = (x + scroll) % w;
                let mut c = BARS[(sx / bar_w) % 8];
                // Vertical brightness ramp in the bottom quarter, so a static
                // region coexists with the moving one (exercises partial damage
                // intuition for later phases).
                if y > h * 3 / 4 {
                    let shade = ((y - h * 3 / 4) * 255 / (h / 4).max(1)) as u32;
                    c = shade << 16 | shade << 8 | shade;
                }
                if x.abs_diff(sentinel) < 2 {
                    c = 0x00FF_FFFF;
                }
                *px = c;
            }
        }
        t.elapsed()
    }

    /// Row-wise copy of the shadow into a device mapping of `dst_pitch` bytes
    /// per row, 4 bytes/pixel (XRGB8888). Returns the time spent copying.
    ///
    /// This is the only place that touches the (potentially write-combined)
    /// device memory, and it only ever does forward, whole-row `copy_from_slice`
    /// — never a read-modify-write.
    pub fn blit_xrgb8888(&self, dst: &mut [u8], dst_pitch: usize) -> Duration {
        let t = Instant::now();
        let row_bytes = self.w * 4;
        for y in 0..self.h {
            let src = &self.px[y * self.w..y * self.w + self.w];
            let off = y * dst_pitch;
            let drow = &mut dst[off..off + row_bytes];
            // Reinterpret the source row as bytes. On little-endian a
            // 0x00RRGGBB u32 lays out as [BB, GG, RR, 00] which is exactly
            // XRGB8888 memory order.
            let sbytes = bytemuck_cast(src);
            drow.copy_from_slice(sbytes);
        }
        t.elapsed()
    }
}

/// Render the bars *directly* into a device mapping, skipping the shadow, so we
/// can measure the penalty of writing straight into write-combined memory.
pub fn render_bars_direct_xrgb8888(
    dst: &mut [u8],
    dst_pitch: usize,
    w: usize,
    h: usize,
    frame: u64,
) -> Duration {
    let t = Instant::now();
    let bar_w = (w / 8).max(1);
    let scroll = (frame as usize * 2) % w;
    let sentinel = (frame as usize * 7) % w;
    for y in 0..h {
        let off = y * dst_pitch;
        for x in 0..w {
            let sx = (x + scroll) % w;
            let mut c = BARS[(sx / bar_w) % 8];
            if y > h * 3 / 4 {
                let shade = ((y - h * 3 / 4) * 255 / (h / 4).max(1)) as u32;
                c = shade << 16 | shade << 8 | shade;
            }
            if x.abs_diff(sentinel) < 2 {
                c = 0x00FF_FFFF;
            }
            let p = off + x * 4;
            dst[p..p + 4].copy_from_slice(&c.to_le_bytes());
        }
    }
    t.elapsed()
}

/// Cast a `&[u32]` to `&[u8]` without a dependency on `bytemuck`.
fn bytemuck_cast(s: &[u32]) -> &[u8] {
    // SAFETY: `u32` has no padding/invalid bit patterns as bytes, and the
    // resulting slice covers exactly the same bytes (alignment of u8 is 1).
    unsafe { std::slice::from_raw_parts(s.as_ptr() as *const u8, std::mem::size_of_val(s)) }
}

/// Rolling timing accumulator, summarized at the end of the run.
#[derive(Default)]
pub struct Timing {
    pub frames: u64,
    pub render: Duration,
    pub blit: Duration,
    pub started: Option<Instant>,
}

impl Timing {
    pub fn start(&mut self) {
        self.started = Some(Instant::now());
    }

    pub fn record(&mut self, render: Duration, blit: Duration) {
        self.frames += 1;
        self.render += render;
        self.blit += blit;
    }

    pub fn report(&self, label: &str) {
        let wall = self.started.map(|s| s.elapsed()).unwrap_or_default();
        let fps = if wall.as_secs_f64() > 0.0 {
            self.frames as f64 / wall.as_secs_f64()
        } else {
            0.0
        };
        let n = self.frames.max(1) as f64;
        eprintln!("---- timing ({label}) ----");
        eprintln!("  frames      : {}", self.frames);
        eprintln!("  wall        : {:.3} s", wall.as_secs_f64());
        eprintln!("  achieved fps: {fps:.1}");
        eprintln!(
            "  avg render  : {:.3} ms/frame",
            self.render.as_secs_f64() * 1e3 / n
        );
        eprintln!(
            "  avg blit    : {:.3} ms/frame",
            self.blit.as_secs_f64() * 1e3 / n
        );
    }
}
