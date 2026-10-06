//! The blitter the vendored hairline scan converter feeds when painting a
//! band: it receives every span in whole-screen coordinates, keeps the ones
//! in the band's rows, and blends them the way tiny-skia's
//! `RasterPipelineBlitter` (low-precision pipeline, solid colour,
//! source-over paint) would have on a whole-screen target.
//!
//! The arithmetic, per tiny-skia 0.12 `pipeline/{blitter,lowp}.rs`, on u16
//! lanes with `div255(v) = (v + 255) >> 8`:
//!
//! * An opaque colour with no clip mask is strength-reduced to `Source`:
//!   full coverage is a memset; partial coverage `c` is
//!   `lerp(d, s, c) = div255(d·(255−c) + s·c)`.
//! * Otherwise (translucent, or a clip mask present) it stays source-over
//!   with coverage pre-scaled: `s' = div255(s·c)`, then
//!   `s' + div255(d·(255 − s'.a))`. A binary clip mask multiplies by 255
//!   (identity) inside and skips the pixel outside.
//! * `blit_anti_h` passes its coverage through f32 (`c = (a/255·255 + 0.5)`),
//!   the `blit_mask` family (`blit_v`, `blit_anti_h2`, `blit_anti_v2`) uses
//!   the u8 directly.

use tiny_skia::{Color, PremultipliedColorU8};

use super::alpha_runs::AlphaRun;
use super::blitter::{Blitter, Mask};
use super::geom::ScreenIntRect;
use super::{AlphaU8, LengthU32};
use crate::geom::IRect;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// Opaque colour, no clip mask: memset / lerp.
    Source,
    /// Everything else: pre-scaled source-over.
    SourceOver,
}

pub(crate) struct BandBlitter<'a> {
    pixels: &'a mut [PremultipliedColorU8],
    width: i32,
    origin: (i32, i32),
    /// Where writes may land, in whole-screen coordinates: the band's rows
    /// (∩ the clip rect, when the whole-screen render had a clip mask).
    write: IRect,
    mode: Mode,
    /// The colour as the pipeline's uniform-colour stage holds it.
    src: [u16; 4],
    /// The memset colour (`Source` at full coverage).
    fill: PremultipliedColorU8,
}

impl<'a> BandBlitter<'a> {
    /// `color` is the paint colour (after any hairline opacity scaling);
    /// `masked` says whether the whole-screen render would have drawn with a
    /// clip mask, which disables tiny-skia's opaque strength reduction.
    pub(crate) fn new(
        pixmap: &'a mut tiny_skia::Pixmap,
        origin: (i32, i32),
        write: IRect,
        color: Color,
        masked: bool,
    ) -> Self {
        let pm = color.premultiply();
        let q = |v: f32| (v * 255.0 + 0.5) as u16;
        let src = [q(pm.red()), q(pm.green()), q(pm.blue()), q(pm.alpha())];
        let mode = if color.is_opaque() && !masked {
            Mode::Source
        } else {
            Mode::SourceOver
        };
        let width = pixmap.width() as i32;
        let (ox, oy) = origin;
        let held = IRect::new(ox, oy, pixmap.width(), pixmap.height());
        BandBlitter {
            pixels: pixmap.pixels_mut(),
            width,
            origin,
            write: write.intersect(held),
            mode,
            src,
            fill: pm.to_color_u8(),
        }
    }

    fn pixel(&mut self, x: i32, y: i32) -> Option<&mut PremultipliedColorU8> {
        if x < self.write.x
            || x >= self.write.right()
            || y < self.write.y
            || y >= self.write.bottom()
        {
            return None;
        }
        let (tx, ty) = (x - self.origin.0, y - self.origin.1);
        self.pixels.get_mut((ty * self.width + tx) as usize)
    }

    /// Full coverage (`blit_h` / `blit_rect`).
    fn full(&mut self, x: i32, y: i32) {
        let (mode, src, fill) = (self.mode, self.src, self.fill);
        let Some(d) = self.pixel(x, y) else { return };
        *d = match mode {
            Mode::Source => fill,
            Mode::SourceOver => over(*d, src),
        };
    }

    /// Partial coverage `c`, already in pipeline form.
    fn cover(&mut self, x: i32, y: i32, c: u16) {
        let (mode, src) = (self.mode, self.src);
        let Some(d) = self.pixel(x, y) else { return };
        *d = match mode {
            Mode::Source => {
                let l = |from: u8, to: u16| div255(from as u16 * (255 - c) + to * c);
                rgba(
                    l(d.red(), src[0]),
                    l(d.green(), src[1]),
                    l(d.blue(), src[2]),
                    l(d.alpha(), src[3]),
                )
            }
            Mode::SourceOver => {
                let s = src.map(|v| div255(v * c));
                over(*d, s)
            }
        };
    }

    /// `blit_anti_h`'s coverage: through f32, as `Lerp1Float` /
    /// `Scale1Float` see it.
    fn anti(&mut self, x: i32, y: i32, a: AlphaU8) {
        match a {
            0 => {}
            255 => self.full(x, y),
            a => {
                let c = ((a as f32 * (1.0 / 255.0)) * 255.0 + 0.5) as u16;
                self.cover(x, y, c);
            }
        }
    }
}

fn div255(v: u16) -> u16 {
    (v + 255) >> 8
}

fn rgba(r: u16, g: u16, b: u16, a: u16) -> PremultipliedColorU8 {
    PremultipliedColorU8::from_rgba(r as u8, g as u8, b as u8, a as u8)
        .unwrap_or(PremultipliedColorU8::TRANSPARENT)
}

/// Source-over of a (premultiplied, u16-lane) source onto `d`.
fn over(d: PremultipliedColorU8, s: [u16; 4]) -> PremultipliedColorU8 {
    let inv = 255 - s[3];
    rgba(
        s[0] + div255(d.red() as u16 * inv),
        s[1] + div255(d.green() as u16 * inv),
        s[2] + div255(d.blue() as u16 * inv),
        s[3] + div255(d.alpha() as u16 * inv),
    )
}

impl Blitter for BandBlitter<'_> {
    fn blit_h(&mut self, x: u32, y: u32, width: LengthU32) {
        for i in 0..width.get() {
            self.full((x + i) as i32, y as i32);
        }
    }

    fn blit_anti_h(&mut self, mut x: u32, y: u32, aa: &mut [AlphaU8], runs: &mut [AlphaRun]) {
        let mut offset = 0;
        let mut run_opt = runs[0];
        while let Some(run) = run_opt {
            let width = u32::from(run.get());
            let a = aa[offset];
            for i in 0..width {
                self.anti((x + i) as i32, y as i32, a);
            }
            x += width;
            offset += usize::from(run.get());
            run_opt = runs[offset];
        }
    }

    fn blit_v(&mut self, x: u32, y: u32, height: LengthU32, alpha: AlphaU8) {
        for i in 0..height.get() {
            self.cover(x as i32, (y + i) as i32, alpha as u16);
        }
    }

    fn blit_anti_h2(&mut self, x: u32, y: u32, alpha0: AlphaU8, alpha1: AlphaU8) {
        self.cover(x as i32, y as i32, alpha0 as u16);
        self.cover(x as i32 + 1, y as i32, alpha1 as u16);
    }

    fn blit_anti_v2(&mut self, x: u32, y: u32, alpha0: AlphaU8, alpha1: AlphaU8) {
        self.cover(x as i32, y as i32, alpha0 as u16);
        self.cover(x as i32, y as i32 + 1, alpha1 as u16);
    }

    fn blit_rect(&mut self, rect: &ScreenIntRect) {
        for y in rect.y()..rect.y() + rect.height() {
            for x in rect.x()..rect.x() + rect.width() {
                self.full(x as i32, y as i32);
            }
        }
    }

    fn blit_mask(&mut self, mask: &Mask, clip: &ScreenIntRect) {
        // Not reached from the hairline paths (they use the dedicated
        // methods above), but keep the trait total.
        let b = mask.bounds;
        for y in clip.y()..clip.y() + clip.height() {
            for x in clip.x()..clip.x() + clip.width() {
                let i = (x - b.x()) as usize + ((y - b.y()) * mask.row_bytes) as usize;
                let m = mask.image.get(i).copied().unwrap_or(0);
                self.cover(x as i32, y as i32, m as u16);
            }
        }
    }
}
