//! Phase 1 demo: a software cursor that follows the mouse/touch and a strip of
//! cells that echoes typed keys. Quit with `Esc`.
//!
//! This is the Phase 1 exit-criterion artifact: it proves the whole platform
//! layer end to end — display bring-up (DRM or fbdev), normalized input, the VT
//! guard, and the event loop — without any of `fbui-render`/`fbui-widgets`
//! existing yet. There's no text rasterizer at this layer, so a "typed key"
//! shows up as a colored cell derived from its character; the point is that
//! keystrokes and pointer motion flow through and the screen updates.
//!
//! Run it from a real text VT (Ctrl-Alt-F3, log in) as root or with
//! `video`+`input` group access:
//!
//! ```text
//! cargo run --example echo
//! ```

use fbui_platform::cursor::SoftwareCursor;
use fbui_platform::{
    keysym, Flow, Frame, InputEvent, KeyState, PixelFormat, Platform, PlatformConfig,
    PlatformHandler, Rect, Size,
};

/// Recent keystrokes, newest last, capped to what fits across the screen.
struct Echo {
    cursor: SoftwareCursor,
    size: Size,
    typed: Vec<char>,
    exit: bool,
}

impl Echo {
    fn new(size: Size) -> Self {
        Echo {
            cursor: SoftwareCursor::new(size),
            size,
            typed: Vec::new(),
            exit: false,
        }
    }
}

/// A stable color per character, so the same key always shows the same hue.
fn color_for(c: char) -> (u8, u8, u8) {
    let h = (c as u32).wrapping_mul(2654435761);
    (
        ((h >> 16) & 0xFF) as u8,
        ((h >> 8) & 0xFF) as u8,
        (h & 0xFF) as u8 | 0x40,
    )
}

impl PlatformHandler for Echo {
    fn on_input(&mut self, event: InputEvent) -> Flow {
        match event {
            InputEvent::Key(k) if k.state == KeyState::Pressed => {
                if k.keysym == keysym::ESCAPE {
                    self.exit = true;
                    return Flow::Exit;
                }
                if k.keysym == keysym::BACKSPACE {
                    self.typed.pop();
                    return Flow::Redraw;
                }
                if let Some(text) = k.utf8 {
                    self.typed.extend(text.chars());
                    // Keep only what fits as 24px cells across the screen.
                    let max = (self.size.w / 24).max(1) as usize;
                    if self.typed.len() > max {
                        let drop = self.typed.len() - max;
                        self.typed.drain(0..drop);
                    }
                }
                Flow::Redraw
            }
            InputEvent::PointerMotion { dx, dy } => {
                self.cursor.move_relative(dx, dy);
                Flow::Redraw
            }
            InputEvent::PointerMotionAbsolute { position } => {
                self.cursor.move_absolute(position);
                Flow::Redraw
            }
            InputEvent::TouchDown { position, .. } | InputEvent::TouchMotion { position, .. } => {
                self.cursor.move_absolute(position);
                Flow::Redraw
            }
            _ => Flow::Continue,
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>) -> Vec<Rect> {
        // Repaint the whole surface: simplest correct thing without the render
        // layer's shadow/age machinery. (Partial-present is exercised by the
        // backend trait itself; the toolkit will do real damage tracking.)
        let bg = (0x12, 0x14, 0x1A);
        clear(frame, bg);

        // The echo strip: one 24px cell per recent character along the top.
        for (i, &c) in self.typed.iter().enumerate() {
            let x = i as i32 * 24 + 8;
            fill_rect(frame, Rect::new(x, 8, 20, 32), color_for(c));
        }

        // The cursor on top.
        self.cursor.paint(frame);

        vec![Rect::from_size(frame.size)]
    }

    fn on_session(&mut self, active: bool) {
        eprintln!(
            "[echo] session {}",
            if active { "activated" } else { "deactivated" }
        );
    }
}

/// Fill the whole frame with a solid color.
fn clear(frame: &mut Frame<'_>, (r, g, b): (u8, u8, u8)) {
    fill_rect(frame, Rect::from_size(frame.size), (r, g, b));
}

/// Fill `rect` (clamped to the surface) with a solid color, writing whole rows.
fn fill_rect(frame: &mut Frame<'_>, rect: Rect, (r, g, b): (u8, u8, u8)) {
    let rect = rect.clamp_to(frame.size);
    if rect.is_empty() {
        return;
    }
    let bpp = frame.format.bytes_per_pixel();
    let stride = frame.stride;
    let pixel = pack(frame.format, r, g, b);
    for y in rect.y..rect.bottom() {
        let row_off = y as usize * stride + rect.x as usize * bpp;
        let row = &mut frame.buffer[row_off..row_off + rect.w as usize * bpp];
        for chunk in row.chunks_exact_mut(bpp) {
            chunk.copy_from_slice(&pixel[..bpp]);
        }
    }
}

/// Pack a color into up to 4 bytes for the frame's format.
fn pack(format: PixelFormat, r: u8, g: u8, b: u8) -> [u8; 4] {
    match format {
        PixelFormat::Xrgb8888 | PixelFormat::Argb8888 => [b, g, r, 0xFF],
        PixelFormat::Rgb565 => {
            let v: u16 = ((r as u16 >> 3) << 11) | ((g as u16 >> 2) << 5) | (b as u16 >> 3);
            let [lo, hi] = v.to_le_bytes();
            [lo, hi, 0, 0]
        }
        // `PixelFormat` is non-exhaustive; treat anything new as 32bpp native.
        _ => [b, g, r, 0xFF],
    }
}

fn main() {
    let config = PlatformConfig::default();
    let platform = match Platform::new(&config) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("echo: could not bring up the platform: {e}");
            eprintln!("      (run from a real text VT as root or with video+input groups)");
            std::process::exit(1);
        }
    };
    let size = platform.info().size;
    let mut app = Echo::new(size);
    eprintln!(
        "[echo] running at {}x{} — type to echo, move the mouse, Esc to quit",
        size.w, size.h
    );
    if let Err(e) = platform.run(&mut app) {
        eprintln!("echo: run error: {e}");
        std::process::exit(1);
    }
}
