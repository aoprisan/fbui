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
use crate::geom::{IRect, Rect};
use crate::painter::Painter;
use crate::scale::Scale;

/// Encode straight-alpha RGBA8 rows (`width * 4` bytes each, no padding) as a
/// PNG. This is [`Surface::encode_png`] for pixels that already left the
/// surface — a frame snapshot shipped to another thread, say. Errors on a
/// buffer that isn't exactly `width * height * 4` bytes.
pub fn encode_png_rgba(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    if rgba.len() != width as usize * height as usize * 4 {
        return Err(format!(
            "rgba buffer is {} bytes, expected {} for {width}x{height}",
            rgba.len(),
            width as usize * height as usize * 4
        ));
    }
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::write_buffer_with_format(
        &mut bytes,
        rgba,
        width,
        height,
        image::ExtendedColorType::Rgba8,
        image::ImageFormat::Png,
    )
    .map_err(|e| e.to_string())?;
    Ok(bytes.into_inner())
}

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

    /// Register a device-pixel region as damaged without drawing into it.
    ///
    /// This is for overlays composited into the back buffer *after* copy-out — a
    /// software cursor, say — whose pixels never live in the shadow. Damaging the
    /// region the overlay last occupied makes the next copy-out refresh those
    /// pixels from the (clean) shadow, erasing the overlay's old position before
    /// it's redrawn elsewhere. Buffer-age history then carries the refresh across
    /// every back buffer.
    pub fn damage_device_rect(&mut self, r: IRect) {
        self.damage.add(r);
    }

    /// Borrow the shadow pixmap (for snapshot tests, PNG export, debugging).
    pub fn pixmap(&self) -> &tiny_skia::Pixmap {
        &self.shadow
    }

    /// The rendered pixels as straight-alpha RGBA8 rows, top-left origin — the
    /// screenshot form. The shadow is premultiplied internally (and kept
    /// opaque), so this un-premultiplies into the layout every image tool
    /// expects. Rows are `width() * 4` bytes, no padding.
    pub fn to_rgba(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.shadow.pixels().len() * 4);
        for px in self.shadow.pixels() {
            let c = px.demultiply();
            out.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
        }
        out
    }

    /// Encode the rendered pixels as a PNG — a screenshot of what's on (or
    /// about to be on) screen. The widget layer's `Ui::request_screenshot`
    /// pairs with this for capture from a running app; call it directly when
    /// you hold the surface (tests, tooling, a custom runner).
    pub fn encode_png(&self) -> Result<Vec<u8>, String> {
        encode_png_rgba(self.width(), self.height(), &self.to_rgba())
    }

    /// Write the rendered pixels to `path` as a PNG. See
    /// [`encode_png`](Self::encode_png). Blocking file I/O — fine for
    /// diagnostics; don't call it per frame.
    pub fn write_png(&self, path: impl AsRef<std::path::Path>) -> Result<(), String> {
        let png = self.encode_png()?;
        std::fs::write(path, png).map_err(|e| e.to_string())
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

    /// Vertically scroll the pixels inside `rect` (logical) by `dy` logical
    /// pixels, **reusing them instead of re-rasterizing** — the scroll-blit fast
    /// path (Phase 5). Positive `dy` moves content downward.
    ///
    /// The moved region is registered as damage (the scanout must receive the
    /// shifted pixels), and the method returns the newly-**exposed strip**
    /// (logical) that the caller still has to repaint — a fraction of the region.
    /// If the shift is zero or as large as the region (nothing to reuse), the
    /// whole `rect` is returned and no pixels are moved.
    ///
    /// This is what makes flick-scrolling a long list cheap: the expensive part
    /// (shaping and rasterizing every visible row) shrinks to just the one row
    /// band that scrolled into view; the rest is a sequential `memmove`.
    pub fn scroll_region(&mut self, rect: Rect, dy: f32) -> Rect {
        let dev = self
            .scale
            .to_device_rect(rect)
            .clamp_to(self.width(), self.height());
        let ddy = (dy * self.scale.factor()).round() as i32;
        if dev.is_empty() || ddy == 0 || ddy.unsigned_abs() >= dev.h {
            // Nothing reusable: the caller repaints the whole rect.
            if !dev.is_empty() {
                self.damage.add(dev);
            }
            return rect;
        }

        let stride = self.width() as usize;
        let bpp = 4usize;
        let x0 = dev.x as usize;
        let cols = dev.w as usize;
        let y0 = dev.y;
        let h = dev.h as i32;
        let data = self.shadow.data_mut();
        let row = |y: i32| -> usize { (y as usize * stride + x0) * bpp };

        if ddy > 0 {
            // Content moves down: dest row y copies from y-ddy. Walk bottom-up so a
            // source row is read before a later iteration overwrites it.
            for y in (y0 + ddy..y0 + h).rev() {
                let (s, d) = (row(y - ddy), row(y));
                data.copy_within(s..s + cols * bpp, d);
            }
        } else {
            // Content moves up: dest row y copies from y+|ddy|. Walk top-down.
            let s = (-ddy) as usize;
            for y in y0..y0 + h - s as i32 {
                let (src, dst) = (row(y + s as i32), row(y));
                data.copy_within(src..src + cols * bpp, dst);
            }
        }

        self.damage.add(dev);
        if dy > 0.0 {
            Rect::new(rect.x, rect.y, rect.w, dy)
        } else {
            Rect::new(rect.x, rect.bottom() + dy, rect.w, -dy)
        }
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

    fn row_color(s: &Surface, y: u32) -> (u8, u8, u8) {
        let px = s.pixmap().pixel(0, y).unwrap();
        (px.red(), px.green(), px.blue())
    }

    #[test]
    fn scroll_region_shifts_pixels_up() {
        // A 1×6 surface with a distinct colour per row.
        let mut s = Surface::new(1, 6, Scale::ONE);
        for y in 0..6u32 {
            s.paint(|p| {
                p.fill_rect(
                    Rect::new(0.0, y as f32, 1.0, 1.0),
                    Color::rgb(y as u8, 0, 0),
                )
            });
        }
        let _ = s.present_to_buffer(&mut [0u8; 6 * 4], 4, TargetFormat::Xrgb8888, 1);

        // Scroll content up by 2: row y now shows the old row y+2.
        let exposed = s.scroll_region(Rect::new(0.0, 0.0, 1.0, 6.0), -2.0);
        assert_eq!(
            exposed,
            Rect::new(0.0, 4.0, 1.0, 2.0),
            "bottom strip exposed"
        );
        assert_eq!(row_color(&s, 0), (2, 0, 0));
        assert_eq!(row_color(&s, 1), (3, 0, 0));
        assert_eq!(row_color(&s, 3), (5, 0, 0));
        // The moved region is damaged for copy-out.
        assert!(!s.is_clean());
    }

    #[test]
    fn scroll_region_shifts_pixels_down() {
        let mut s = Surface::new(1, 6, Scale::ONE);
        for y in 0..6u32 {
            s.paint(|p| {
                p.fill_rect(
                    Rect::new(0.0, y as f32, 1.0, 1.0),
                    Color::rgb(y as u8, 0, 0),
                )
            });
        }
        let _ = s.present_to_buffer(&mut [0u8; 6 * 4], 4, TargetFormat::Xrgb8888, 1);

        // Scroll content down by 2: row y now shows the old row y-2.
        let exposed = s.scroll_region(Rect::new(0.0, 0.0, 1.0, 6.0), 2.0);
        assert_eq!(exposed, Rect::new(0.0, 0.0, 1.0, 2.0), "top strip exposed");
        assert_eq!(row_color(&s, 2), (0, 0, 0));
        assert_eq!(row_color(&s, 5), (3, 0, 0));
    }

    #[test]
    fn scroll_region_too_far_moves_nothing() {
        let mut s = Surface::new(1, 4, Scale::ONE);
        for y in 0..4u32 {
            s.paint(|p| {
                p.fill_rect(
                    Rect::new(0.0, y as f32, 1.0, 1.0),
                    Color::rgb(y as u8, 0, 0),
                )
            });
        }
        let _ = s.present_to_buffer(&mut [0u8; 4 * 4], 4, TargetFormat::Xrgb8888, 1);
        // Shift >= height: nothing reusable, whole rect returned, pixels untouched.
        let whole = Rect::new(0.0, 0.0, 1.0, 4.0);
        assert_eq!(s.scroll_region(whole, 4.0), whole);
        assert_eq!(row_color(&s, 1), (1, 0, 0));
    }

    #[test]
    fn set_scale_damages_everything() {
        let mut s = Surface::new(8, 8, Scale::ONE);
        let _ = s.present_to_buffer(&mut [0u8; 8 * 8 * 4], 32, TargetFormat::Xrgb8888, 1);
        s.set_scale(Scale::new(2.0));
        let damage = s.present_to_buffer(&mut [0u8; 8 * 8 * 4], 32, TargetFormat::Xrgb8888, 1);
        assert_eq!(damage, vec![IRect::from_wh(8, 8)]);
    }

    #[test]
    fn injected_device_damage_presents_without_painting() {
        // The software-cursor contract: damaging a region the overlay vacated —
        // with no paint into the shadow — still surfaces it as a copy-out region,
        // so the next frame refreshes those pixels and erases the old arrow.
        let mut s = Surface::new(8, 8, Scale::ONE);
        let _ = s.present_to_buffer(&mut [0u8; 8 * 8 * 4], 32, TargetFormat::Xrgb8888, 1);
        assert!(s.is_clean());
        s.damage_device_rect(IRect::new(2, 3, 4, 4));
        assert!(!s.is_clean());
        let damage = s.present_to_buffer(&mut [0u8; 8 * 8 * 4], 32, TargetFormat::Xrgb8888, 1);
        assert_eq!(damage, vec![IRect::new(2, 3, 4, 4)]);
    }

    #[test]
    fn screenshot_roundtrips_through_png() {
        // Paint a red square on the black base, export, decode back with the
        // image crate, and check both a painted and an unpainted pixel.
        let mut s = Surface::new(8, 8, Scale::ONE);
        s.paint(|p| p.fill_rect(Rect::new(0.0, 0.0, 4.0, 4.0), Color::rgb(255, 0, 0)));

        let rgba = s.to_rgba();
        assert_eq!(rgba.len(), 8 * 8 * 4);

        let png = s.encode_png().unwrap();
        let img = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(img.dimensions(), (8, 8));
        assert_eq!(img.get_pixel(2, 2).0, [255, 0, 0, 255], "painted pixel");
        assert_eq!(img.get_pixel(6, 6).0, [0, 0, 0, 255], "opaque base pixel");
    }
}
