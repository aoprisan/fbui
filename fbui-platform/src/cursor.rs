//! A minimal software cursor.
//!
//! The platform layer doesn't own a cursor concept — that's the toolkit's job —
//! but a software pointer is needed to satisfy the Phase 1 demo (and is handy
//! for any bring-up before `fbui-render` exists). This is a tiny self-contained
//! arrow blitter that composites directly into a [`Frame`] buffer, honoring its
//! stride and pixel format. It tracks its own position from relative/absolute
//! motion and reports the damage it dirties so partial-present still works.
//!
//! Real cursors move to a hardware plane in Phase 5; this is deliberately dumb.

use crate::display::Frame;
use crate::geom::{Point, Rect, Size};

/// A 12×19 1-bpp arrow: `1` = opaque white, `2` = black outline, `0` = clear.
/// Hand-rolled so the demo needs no asset files.
const ARROW: [&[u8]; 19] = [
    b"2           ",
    b"22          ",
    b"212         ",
    b"2112        ",
    b"21112       ",
    b"211112      ",
    b"2111112     ",
    b"21111112    ",
    b"211111112   ",
    b"2111111112  ",
    b"21111111112 ",
    b"2111112222  ",
    b"211112      ",
    b"21 2112     ",
    b"2  2112     ",
    b"    2112    ",
    b"    2112    ",
    b"     22     ",
    b"            ",
];

const CW: u32 = 12;
const CH: u32 = 19;

/// A software pointer with a position and a fixed arrow sprite.
pub struct SoftwareCursor {
    pos: Point,
    bounds: Size,
    /// The rectangle painted last frame, so callers can damage-erase it.
    last_rect: Rect,
}

impl SoftwareCursor {
    /// Start centered on a `bounds`-sized surface.
    pub fn new(bounds: Size) -> Self {
        let pos = Point::new(bounds.w as i32 / 2, bounds.h as i32 / 2);
        SoftwareCursor {
            pos,
            bounds,
            last_rect: Rect::EMPTY,
        }
    }

    pub fn position(&self) -> Point {
        self.pos
    }

    /// Apply a relative motion delta, clamped to the surface.
    pub fn move_relative(&mut self, dx: f64, dy: f64) {
        self.pos.x = (self.pos.x + dx.round() as i32).clamp(0, self.bounds.w as i32 - 1);
        self.pos.y = (self.pos.y + dy.round() as i32).clamp(0, self.bounds.h as i32 - 1);
    }

    /// Jump to an absolute position, clamped to the surface.
    pub fn move_absolute(&mut self, p: Point) {
        self.pos.x = p.x.clamp(0, self.bounds.w as i32 - 1);
        self.pos.y = p.y.clamp(0, self.bounds.h as i32 - 1);
    }

    /// The rectangle the cursor currently occupies (its hotspot is the tip at
    /// the top-left, like a classic arrow).
    pub fn rect(&self) -> Rect {
        Rect::new(self.pos.x, self.pos.y, CW, CH).clamp_to(self.bounds)
    }

    /// Union of where the cursor was and where it is now — the damage a move
    /// produces, so the caller repaints the vacated *and* the freshly-covered
    /// pixels.
    pub fn damage(&self) -> Rect {
        self.rect().union(self.last_rect)
    }

    /// Composite the arrow into `frame` at the current position. Records the
    /// painted rect for the next [`damage`](SoftwareCursor::damage) call.
    pub fn paint(&mut self, frame: &mut Frame<'_>) {
        let bpp = frame.format.bytes_per_pixel();
        let stride = frame.stride;
        let (ox, oy) = (self.pos.x, self.pos.y);
        for (ry, row) in ARROW.iter().enumerate() {
            let y = oy + ry as i32;
            if y < 0 || y >= frame.size.h as i32 {
                continue;
            }
            for (rx, &cell) in row.iter().enumerate() {
                let x = ox + rx as i32;
                if x < 0 || x >= frame.size.w as i32 || cell == b' ' || cell == b'0' {
                    continue;
                }
                let (r, g, b) = if cell == b'1' {
                    (255, 255, 255)
                } else {
                    (0, 0, 0)
                };
                let off = y as usize * stride + x as usize * bpp;
                write_pixel(&mut frame.buffer[off..off + bpp], frame.format, r, g, b);
            }
        }
        self.last_rect = self.rect();
    }
}

/// Pack one pixel into `dst` in the frame's format.
fn write_pixel(dst: &mut [u8], format: crate::format::PixelFormat, r: u8, g: u8, b: u8) {
    use crate::format::PixelFormat::*;
    match format {
        Xrgb8888 | Argb8888 => {
            // Little-endian 0xAARRGGBB => [B, G, R, A].
            dst[0] = b;
            dst[1] = g;
            dst[2] = r;
            dst[3] = 0xFF;
        }
        Rgb565 => {
            let v: u16 = ((r as u16 >> 3) << 11) | ((g as u16 >> 2) << 5) | (b as u16 >> 3);
            dst[0..2].copy_from_slice(&v.to_le_bytes());
        }
    }
}
