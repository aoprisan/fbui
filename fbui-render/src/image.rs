//! Decoded raster images, ready to blit.
//!
//! An [`Image`] is a premultiplied-alpha [`tiny_skia::Pixmap`] — the form
//! [`crate::Painter::draw_image`] can composite directly. We decode PNG/JPEG via
//! the `image` crate, then premultiply into tiny-skia's layout once, up front,
//! so the hot blit path does no per-pixel conversion.
//!
//! Images are **device-pixel bitmaps**: an icon is N×N physical pixels and is
//! drawn 1:1. Scaling for HiDPI is the caller's choice (ship a 2× asset, or draw
//! a vector instead).

use std::path::Path;

use crate::geom::Size;

/// A decoded, premultiplied image.
#[derive(Debug, Clone)]
pub struct Image {
    pub(crate) pixmap: tiny_skia::Pixmap,
}

impl Image {
    /// Decode PNG/JPEG bytes (format sniffed from content).
    pub fn from_encoded(bytes: &[u8]) -> Result<Image, String> {
        let dynimg = image::load_from_memory(bytes).map_err(|e| e.to_string())?;
        Ok(Image::from_rgba(dynimg.to_rgba8()))
    }

    /// Decode a PNG/JPEG file.
    pub fn open(path: impl AsRef<Path>) -> Result<Image, String> {
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        Image::from_encoded(&bytes)
    }

    /// Build from raw straight-alpha RGBA8 pixels, premultiplying into tiny-skia
    /// layout.
    pub fn from_rgba(img: image::RgbaImage) -> Image {
        let (w, h) = img.dimensions();
        let mut pixmap = tiny_skia::Pixmap::new(w.max(1), h.max(1)).expect("image pixmap alloc");
        for (dst, src) in pixmap.pixels_mut().iter_mut().zip(img.pixels()) {
            let [r, g, b, a] = src.0;
            // tiny-skia stores premultiplied; ColorU8 + premultiply does it for us.
            *dst = tiny_skia::ColorU8::from_rgba(r, g, b, a).premultiply();
        }
        Image { pixmap }
    }

    pub fn size(&self) -> Size {
        Size::new(self.pixmap.width() as f32, self.pixmap.height() as f32)
    }

    pub fn width(&self) -> u32 {
        self.pixmap.width()
    }

    pub fn height(&self) -> u32 {
        self.pixmap.height()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_rgba_premultiplies() {
        // Half-transparent red: premultiplied red channel halves.
        let mut img = image::RgbaImage::new(1, 1);
        img.put_pixel(0, 0, image::Rgba([255, 0, 0, 128]));
        let out = Image::from_rgba(img);
        let px = out.pixmap.pixel(0, 0).unwrap();
        assert_eq!(px.alpha(), 128);
        assert!(
            px.red() < 200,
            "expected premultiplied red, got {}",
            px.red()
        );
    }

    #[test]
    fn decode_roundtrip_png() {
        // Encode a 2x2 PNG with the image crate, then decode through Image.
        let mut img = image::RgbaImage::new(2, 2);
        img.put_pixel(0, 0, image::Rgba([10, 20, 30, 255]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        img.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        let decoded = Image::from_encoded(bytes.get_ref()).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (2, 2));
    }
}
