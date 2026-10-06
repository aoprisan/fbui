//! # fbui-doc — documents and images for `no_std` fbui targets
//!
//! Decoders that turn file bytes into [`tiny_skia::Pixmap`]s — the pixel
//! format `fbui-render` blits — with nothing but `core` + `alloc`:
//!
//! * [`raster`] — PNG (every colour type, bit depth and interlace, via
//!   zune-png) and baseline/progressive JPEG (via zune-jpeg);
//! * [`pdf`] — a PDF subset renderer: parses the file structure, runs page
//!   content streams and rasterizes them with tiny-skia, including embedded
//!   TrueType/CFF/Type 1/Type 3 fonts. Scope and limits are listed in the
//!   module docs and in NOSTD.md.
//!
//! The crate depends on nothing from the fbui stack, so it is usable on its
//! own; `fbui_render::Image::from_pixmap` takes its output directly.
//!
//! ```
//! # fn main() -> Result<(), fbui_doc::pdf::Error> {
//! use fbui_doc::pdf::{Document, RenderOptions};
//!
//! let pdf = b"%PDF-1.4
//! 1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
//! 2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
//! 3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 100 50] /Contents 4 0 R >> endobj
//! 4 0 obj << /Length 31 >> stream
//! 1 0 0 rg 10 10 80 30 re f
//! endstream endobj
//! trailer << /Root 1 0 R >>
//! %%EOF";
//! let doc = Document::parse(&pdf[..])?;
//! let page = doc.render_page(0, &RenderOptions { zoom: 2.0, ..Default::default() })?;
//! assert_eq!((page.width(), page.height()), (200, 100));
//! let px = page.pixel(100, 50).unwrap();
//! assert_eq!((px.red(), px.green(), px.blue()), (255, 0, 0));
//! # Ok(()) }
//! ```

#![no_std]

extern crate alloc;

mod math;
pub mod pdf;
pub mod raster;

pub use tiny_skia;

/// What a byte buffer looks like, by magic number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Png,
    Jpeg,
    Pdf,
    Unknown,
}

/// Identify `bytes` by their leading signature.
pub fn sniff(bytes: &[u8]) -> Format {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Format::Png
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Format::Jpeg
    } else if pdf_header(bytes) {
        Format::Pdf
    } else {
        Format::Unknown
    }
}

fn pdf_header(bytes: &[u8]) -> bool {
    // Some writers put junk before the header; the spec tolerates 1 KiB.
    bytes[..bytes.len().min(1024)]
        .windows(5)
        .any(|w| w == b"%PDF-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_formats() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n...."), Format::Png);
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Format::Jpeg);
        assert_eq!(sniff(b"%PDF-1.7\n"), Format::Pdf);
        assert_eq!(sniff(b"hello"), Format::Unknown);
    }
}
