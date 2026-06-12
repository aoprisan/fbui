//! The render surface: a normal-RAM shadow buffer, a scale factor, and a damage
//! tracker — the three things every paint pass needs.
//!
//! [`Surface`] owns the [`tiny_skia::Pixmap`] the painter draws into and copies
//! out of. It is deliberately ignorant of where pixels end up: [`present_to_buffer`]
//! takes a raw destination slice + stride + format, so the same surface serves a
//! snapshot test, a PNG export, or a real `fbui_platform::Display` (the latter via
//! the `platform` feature glue).
//!
//! The shadow is kept **opaque**: it's cleared to an opaque base on creation and
//! on any full repaint, so copy-out can treat premultiplied pixels as straight
//! and the scanout never shows through to garbage.
//!
//! [`present_to_buffer`]: Surface::present_to_buffer

use crate::color::Color;
use crate::copyout::{self, TargetFormat};
use crate::damage::DamageTracker;
use crate::geom::IRect;
use crate::painter::Painter;
use crate::scale::Scale;

/// A CPU render target plus its damage bookkeeping.
pub struct Surface {
    shadow: tiny_skia::Pixmap,
    scale: Scale,
    damage: DamageTracker,
    base: Color,
    /// Apply ordered dithering when copying out to a 16-bit (RGB565) target.
    dither_565: bool,
}

impl Surface {
    /// Create a `width × height` device-pixel surface at `scale`, cleared to an
    /// opaque black base.
    pub fn new(width: u32, height: u32, scale: Scale) -> Self {
        Surface::with_base(width, height, scale, Color::BLACK)
    }

    /// As [`new`](Self::new) but with a chosen opaque base color. A non-opaque
    /// base is forced opaque — the scanout has no alpha to blend against.
    pub fn with_base(width: u32, height: u32, scale: Scale, base: Color) -> Self {
        let base = base.with_alpha(255);
        let mut shadow =
            tiny_skia::Pixmap::new(width.max(1), height.max(1)).expect("shadow buffer allocation");
        shadow.fill(base.to_tiny());
        Surface {
            shadow,
            scale,
            damage: DamageTracker::new(),
            base,
            dither_565: false,
        }
    }

    /// Enable (or disable) ordered dithering on the RGB565 copy-out path. Off by
    /// default; turn it on for 16-bit panels to suppress gradient banding. A
    /// no-op for 32-bit targets. The runner enables it automatically when the
    /// display reports an [`Rgb565`](crate::copyout::TargetFormat::Rgb565) format.
    pub fn set_dither(&mut self, on: bool) {
        self.dither_565 = on;
    }

    /// Whether RGB565 dithering is enabled.
    pub fn dither(&self) -> bool {
        self.dither_565
    }

    pub fn width(&self) -> u32 {
        self.shadow.width()
    }

    pub fn height(&self) -> u32 {
        self.shadow.height()
    }

    pub fn scale(&self) -> Scale {
        self.scale
    }

    /// Change the scale factor. Marks the whole surface damaged, since existing
    /// pixels were rendered at the old factor.
    pub fn set_scale(&mut self, scale: Scale) {
        self.scale = scale;
        self.damage.add(IRect::from_wh(self.width(), self.height()));
    }

    /// True if nothing has been drawn since the last present — the caller can
    /// skip presenting entirely.
    pub fn is_clean(&self) -> bool {
        self.damage.is_clean()
    }

    /// Borrow the shadow pixmap (for snapshot tests, PNG export, debugging).
    pub fn pixmap(&self) -> &tiny_skia::Pixmap {
        &self.shadow
    }

    /// Run a paint pass. The closure gets a [`Painter`] bound to this surface's
    /// shadow buffer and scale; whatever it draws accumulates damage.
    pub fn paint(&mut self, f: impl FnOnce(&mut Painter<'_>)) {
        let mut painter = Painter::new(&mut self.shadow, &mut self.damage, self.scale);
        f(&mut painter);
    }

    /// Repaint the whole surface from scratch: reset to the opaque base and draw,
    /// marking everything damaged. Use after a resume/mode-change where the back
    /// buffers hold unknown contents.
    pub fn repaint_full(&mut self, f: impl FnOnce(&mut Painter<'_>)) {
        let full = IRect::from_wh(self.shadow.width(), self.shadow.height());
        self.shadow.fill(self.base.to_tiny());
        let mut painter = Painter::new(&mut self.shadow, &mut self.damage, self.scale);
        painter.add_damage(full);
        f(&mut painter);
    }

    /// Flush accumulated damage for a back buffer of the given `age`, copy the
    /// damaged spans into `dst`, and return the device-pixel regions that were
    /// written (so the caller can hand them to `present`).
    ///
    /// `dst` is `stride * height` bytes; `stride` is the kernel-reported pitch,
    /// never assumed to be `width * bpp`.
    pub fn present_to_buffer(
        &mut self,
        dst: &mut [u8],
        stride: usize,
        format: TargetFormat,
        age: u32,
    ) -> Vec<IRect> {
        let (w, h) = (self.shadow.width(), self.shadow.height());
        let damage = self.damage.flush(age, w, h);
        if self.dither_565 && format == TargetFormat::Rgb565 {
            copyout::copy_out_dithered(&self.shadow, dst, stride, format, &damage);
        } else {
            copyout::copy_out(&self.shadow, dst, stride, format, &damage);
        }
        damage
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Rect;

    #[test]
    fn fresh_surface_is_opaque_base() {
        let s = Surface::with_base(4, 4, Scale::ONE, Color::rgb(10, 20, 30));
        let px = s.pixmap().pixel(0, 0).unwrap();
        assert_eq!(
            (px.red(), px.green(), px.blue(), px.alpha()),
            (10, 20, 30, 255)
        );
    }

    #[test]
    fn painting_accumulates_damage_and_copies_out() {
        let mut s = Surface::new(10, 10, Scale::ONE);
        let _ = s.present_to_buffer(&mut [0u8; 10 * 10 * 4], 40, TargetFormat::Xrgb8888, 1);
        assert!(s.is_clean());

        s.paint(|p| p.fill_rect(Rect::new(2.0, 2.0, 3.0, 3.0), Color::WHITE));
        assert!(!s.is_clean());

        let mut dst = vec![0u8; 10 * 10 * 4];
        let damage = s.present_to_buffer(&mut dst, 40, TargetFormat::Xrgb8888, 1);
        assert_eq!(damage, vec![IRect::new(2, 2, 3, 3)]);
        // The painted pixel made it across (white -> [B,G,R,X]=255,255,255).
        let off = 2 * 40 + 2 * 4;
        assert_eq!(&dst[off..off + 4], &[255, 255, 255, 0xff]);
    }

    #[test]
    fn set_scale_damages_everything() {
        let mut s = Surface::new(8, 8, Scale::ONE);
        let _ = s.present_to_buffer(&mut [0u8; 8 * 8 * 4], 32, TargetFormat::Xrgb8888, 1);
        s.set_scale(Scale::new(2.0));
        let damage = s.present_to_buffer(&mut [0u8; 8 * 8 * 4], 32, TargetFormat::Xrgb8888, 1);
        assert_eq!(damage, vec![IRect::from_wh(8, 8)]);
    }
}
