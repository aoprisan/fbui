//! Decoded raster images, ready to blit.
//!
//! An [`Image`] is a premultiplied-alpha [`tiny_skia::Pixmap`] — the form
//! [`crate::Painter::draw_image`] can composite directly. We decode PNG/JPEG via
//! the `image` crate, then premultiply into tiny-skia's layout once, up front,
//! so the hot blit path does no per-pixel conversion.
//!
//! Images are **device-pixel bitmaps**: an icon is N×N physical pixels and is
//! drawn 1:1. Scaling for HiDPI is the caller's choice: ship a 2× asset, or —
//! with the `svg` feature — rasterize a vector icon at exactly the size you
//! need via `Image::from_svg`.

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

    /// Rasterize SVG bytes at `width`×`height` device pixels (feature `svg`).
    ///
    /// The drawing is scaled to **fit** the box, preserving its aspect ratio,
    /// and centered; the rest stays transparent. Pick the box in device pixels
    /// for the size you'll draw at — this is where SVG earns its keep on HiDPI:
    /// rasterize the same asset at `24` or `48` from one file, instead of
    /// shipping a bitmap per scale.
    ///
    /// Text and embedded raster images inside the SVG are not supported (this
    /// is an *icon* path, and enabling them would drag in font/image stacks);
    /// such elements are skipped, the rest of the drawing renders.
    #[cfg(feature = "svg")]
    pub fn from_svg(bytes: &[u8], width: u32, height: u32) -> Result<Image, String> {
        let opts = resvg::usvg::Options::default();
        let tree = resvg::usvg::Tree::from_data(bytes, &opts).map_err(|e| e.to_string())?;
        let (w, h) = (width.max(1), height.max(1));
        let mut pixmap = tiny_skia::Pixmap::new(w, h).expect("svg pixmap alloc");

        let src = tree.size();
        if src.width() > 0.0 && src.height() > 0.0 {
            let scale = (w as f32 / src.width()).min(h as f32 / src.height());
            let tx = (w as f32 - src.width() * scale) / 2.0;
            let ty = (h as f32 - src.height() * scale) / 2.0;
            let t = tiny_skia::Transform::from_row(scale, 0.0, 0.0, scale, tx, ty);
            // resvg rasterizes with the same tiny-skia the painter uses, so it
            // draws straight into our (premultiplied) pixmap.
            resvg::render(&tree, t, &mut pixmap.as_mut());
        }
        Ok(Image { pixmap })
    }

    /// Rasterize an SVG file at `width`×`height` device pixels (feature `svg`).
    /// See [`from_svg`](Self::from_svg).
    #[cfg(feature = "svg")]
    pub fn from_svg_file(path: impl AsRef<Path>, width: u32, height: u32) -> Result<Image, String> {
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        Image::from_svg(&bytes, width, height)
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

    #[cfg(feature = "svg")]
    #[test]
    fn svg_rasterizes_at_the_requested_size() {
        // A 10x10 viewBox with a full-bleed red square: every pixel of the
        // target box is covered, at any raster size.
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10">
            <rect x="0" y="0" width="10" height="10" fill="#ff0000"/>
        </svg>"##;
        let img = Image::from_svg(svg, 24, 24).unwrap();
        assert_eq!((img.width(), img.height()), (24, 24));
        let px = img.pixmap.pixel(12, 12).unwrap();
        assert_eq!(
            (px.red(), px.green(), px.blue(), px.alpha()),
            (255, 0, 0, 255)
        );
    }

    #[cfg(feature = "svg")]
    #[test]
    fn svg_fit_preserves_aspect_and_centers() {
        // A wide 20x10 drawing into a square box: fits the width, letterboxes
        // vertically — the very top row stays transparent, the center paints.
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 10">
            <rect x="0" y="0" width="20" height="10" fill="#00ff00"/>
        </svg>"##;
        let img = Image::from_svg(svg, 40, 40).unwrap();
        assert_eq!(img.pixmap.pixel(20, 2).unwrap().alpha(), 0, "letterboxed");
        assert_eq!(img.pixmap.pixel(20, 20).unwrap().green(), 255, "centered");
    }

    #[cfg(feature = "svg")]
    #[test]
    fn svg_rejects_garbage() {
        assert!(Image::from_svg(b"not an svg", 8, 8).is_err());
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
