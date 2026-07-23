//! The on-device debug HUD (`FBUI_HUD=1`): a small fps / paint-cost readout
//! composited into the top-right corner of every presented frame.
//!
//! It rides the software-cursor pattern: drawn into the back buffer *after*
//! copy-out, never into the shadow surface, with its device rect damaged each
//! render so the vacated pixels refresh from the clean shadow across every
//! back buffer. The text uses a tiny built-in 3×5 pixel font — no fonts, no
//! render stack, so the HUD works even when text rendering itself is what's
//! broken. When the app idles no frames are presented and the HUD simply
//! freezes with the app — the idle-burns-0% rule is untouched.

use std::time::Instant;

use fbui_platform::{Frame, PixelFormat};
use fbui_render::geom::IRect;

/// Glyph cell geometry: 3×5 pixels, doubled on screen, 1-cell letter gap.
const GW: usize = 3;
const GH: usize = 5;
const SCALE: usize = 2;
const ADV: usize = (GW + 1) * SCALE;
const PAD: usize = 4;

/// The rolling window for the fps figure, seconds.
const FPS_WINDOW: f32 = 0.5;

/// The 3×5 font: exactly the characters the readout needs.
fn glyph(c: char) -> Option<[u8; GH]> {
    // Each row is 3 bits, MSB = left pixel.
    Some(match c {
        '0' => [0b111, 0b101, 0b101, 0b101, 0b111],
        '1' => [0b010, 0b110, 0b010, 0b010, 0b111],
        '2' => [0b111, 0b001, 0b111, 0b100, 0b111],
        '3' => [0b111, 0b001, 0b011, 0b001, 0b111],
        '4' => [0b101, 0b101, 0b111, 0b001, 0b001],
        '5' => [0b111, 0b100, 0b111, 0b001, 0b111],
        '6' => [0b111, 0b100, 0b111, 0b101, 0b111],
        '7' => [0b111, 0b001, 0b010, 0b010, 0b010],
        '8' => [0b111, 0b101, 0b111, 0b101, 0b111],
        '9' => [0b111, 0b101, 0b111, 0b001, 0b111],
        '.' => [0b000, 0b000, 0b000, 0b000, 0b010],
        'f' => [0b011, 0b100, 0b110, 0b100, 0b100],
        'p' => [0b110, 0b101, 0b110, 0b100, 0b100],
        's' => [0b011, 0b100, 0b010, 0b001, 0b110],
        'm' => [0b000, 0b111, 0b111, 0b101, 0b101],
        ' ' => [0; GH],
        _ => return None,
    })
}

/// Render `text` as an opaque box of set/unset pixels: returns
/// `(width, height, mask)` where `mask[y * width + x]` is `true` for lit
/// pixels. Pure, so it's unit-testable without a display.
fn rasterize(text: &str) -> (usize, usize, Vec<bool>) {
    let glyphs: Vec<[u8; GH]> = text.chars().filter_map(glyph).collect();
    let w = glyphs.len() * ADV - if glyphs.is_empty() { 0 } else { SCALE } + 2 * PAD;
    let h = GH * SCALE + 2 * PAD;
    let mut mask = vec![false; w * h];
    for (i, g) in glyphs.iter().enumerate() {
        let ox = PAD + i * ADV;
        for (ry, row) in g.iter().enumerate() {
            for rx in 0..GW {
                if row & (0b100 >> rx) == 0 {
                    continue;
                }
                for sy in 0..SCALE {
                    for sx in 0..SCALE {
                        let x = ox + rx * SCALE + sx;
                        let y = PAD + ry * SCALE + sy;
                        mask[y * w + x] = true;
                    }
                }
            }
        }
    }
    (w, h, mask)
}

/// Frame-rate / paint-cost bookkeeping plus the compositor for the readout.
pub(crate) struct Hud {
    /// Presents inside the current fps window.
    frames_in_window: u32,
    window_start: Instant,
    /// The displayed figures (updated once per window / per frame).
    fps: f32,
    paint_ms: f32,
    /// Where the HUD painted last, so the runner can damage it each render.
    last_rect: IRect,
}

impl Hud {
    /// `FBUI_HUD=1` (or any truthy value) enables the overlay.
    pub fn from_env() -> Option<Hud> {
        match std::env::var("FBUI_HUD").ok().as_deref() {
            None | Some("") | Some("0") | Some("false") => None,
            Some(_) => Some(Hud {
                frames_in_window: 0,
                window_start: Instant::now(),
                fps: 0.0,
                paint_ms: 0.0,
                last_rect: IRect::new(0, 0, 0, 0),
            }),
        }
    }

    /// Record one presented frame and its paint + copy-out cost (an EMA, so a
    /// single hiccup doesn't flicker the readout).
    pub fn note_frame(&mut self, paint_ms: f32) {
        self.paint_ms = if self.paint_ms == 0.0 {
            paint_ms
        } else {
            self.paint_ms * 0.8 + paint_ms * 0.2
        };
        self.frames_in_window += 1;
        let elapsed = self.window_start.elapsed().as_secs_f32();
        if elapsed >= FPS_WINDOW {
            self.fps = self.frames_in_window as f32 / elapsed;
            self.frames_in_window = 0;
            self.window_start = Instant::now();
        }
    }

    /// The device rect the HUD occupied last frame — damage it before painting
    /// so copy-out refreshes those pixels from the clean shadow first.
    pub fn damage(&self) -> IRect {
        self.last_rect
    }

    fn text(&self) -> String {
        format!(
            "{:.0}fps {:.1}ms",
            self.fps.min(999.0),
            self.paint_ms.min(99.9)
        )
    }

    /// Composite the readout into the frame's top-right corner (after
    /// copy-out, like the cursor). Lit pixels are white, the box is black —
    /// readable on anything.
    pub fn paint(&mut self, frame: &mut Frame<'_>) {
        let (w, h, mask) = rasterize(&self.text());
        let fw = frame.size.w as usize;
        let fh = frame.size.h as usize;
        let (w, h) = (w.min(fw), h.min(fh));
        let ox = fw - w;
        let bpp = frame.format.bytes_per_pixel();
        for y in 0..h {
            let row_off = y * frame.stride + ox * bpp;
            let row = &mut frame.buffer[row_off..row_off + w * bpp];
            for x in 0..w {
                let lit = mask[y * w + x];
                let v = if lit { 255 } else { 16 };
                write_pixel(&mut row[x * bpp..(x + 1) * bpp], frame.format, v, v, v);
            }
        }
        self.last_rect = IRect::new(ox as i32, 0, w as u32, h as u32);
    }
}

/// Pack one pixel in the frame's format (mirrors the platform cursor's rule:
/// little-endian `0xAARRGGBB` for the 32-bit formats).
fn write_pixel(dst: &mut [u8], format: PixelFormat, r: u8, g: u8, b: u8) {
    match format {
        PixelFormat::Xrgb8888 | PixelFormat::Argb8888 => {
            dst[0] = b;
            dst[1] = g;
            dst[2] = r;
            dst[3] = 0xFF;
        }
        PixelFormat::Rgb565 => {
            let v: u16 = ((r as u16 >> 3) << 11) | ((g as u16 >> 2) << 5) | (b as u16 >> 3);
            dst[0..2].copy_from_slice(&v.to_le_bytes());
        }
        // A future format this build doesn't know: draw nothing rather than
        // corrupt the buffer.
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rasterizes_known_glyphs_and_skips_unknown() {
        let (w, h, mask) = rasterize("1.5ms");
        assert_eq!(h, GH * SCALE + 2 * PAD);
        assert_eq!(w, 5 * ADV - SCALE + 2 * PAD);
        assert!(mask.iter().any(|&b| b), "some pixels lit");
        // An unknown char contributes nothing rather than panicking.
        let (w2, _, _) = rasterize("1?5ms"); // '?' dropped -> one glyph fewer
        assert_eq!(w2, 4 * ADV - SCALE + 2 * PAD);
    }

    #[test]
    fn digits_are_distinct() {
        let a = rasterize("1").2;
        let b = rasterize("8").2;
        assert_ne!(a, b);
    }

    #[test]
    fn paints_into_a_frame_and_reports_its_rect() {
        let mut hud = Hud {
            frames_in_window: 0,
            window_start: Instant::now(),
            fps: 60.0,
            paint_ms: 1.5,
            last_rect: IRect::new(0, 0, 0, 0),
        };
        let (fw, fh, stride) = (200usize, 40usize, 200 * 4usize);
        let mut buf = vec![0u8; stride * fh];
        let mut frame = Frame {
            buffer: &mut buf,
            stride,
            size: fbui_platform::Size {
                w: fw as u32,
                h: fh as u32,
            },
            format: PixelFormat::Xrgb8888,
            age: 1,
        };
        hud.paint(&mut frame);
        let r = hud.damage();
        assert_eq!(r.y, 0);
        assert_eq!((r.x + r.w as i32) as usize, fw, "flush to the right edge");
        assert!(r.w > 0 && r.h > 0);
        // Something white landed inside the rect; outside stayed untouched.
        let inside =
            &buf[(PAD * stride + (r.x as usize + PAD) * 4)..][..r.w as usize * 4 - 2 * PAD * 4];
        assert!(inside.contains(&255));
        assert!(buf[..r.x as usize * 4].iter().all(|&b| b == 0));
    }

    #[test]
    fn fps_settles_over_a_window() {
        let mut hud = Hud {
            frames_in_window: 0,
            window_start: Instant::now() - std::time::Duration::from_secs(1),
            fps: 0.0,
            paint_ms: 0.0,
            last_rect: IRect::new(0, 0, 0, 0),
        };
        hud.note_frame(2.0);
        assert!(hud.fps > 0.0, "window elapsed -> fps computed");
        assert_eq!(hud.paint_ms, 2.0, "first sample seeds the EMA");
        hud.note_frame(4.0);
        assert!(hud.paint_ms > 2.0 && hud.paint_ms < 4.0, "EMA blends");
    }
}
