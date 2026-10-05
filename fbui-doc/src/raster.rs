//! PNG and JPEG → premultiplied [`Pixmap`].

use core::fmt;

use tiny_skia::{Pixmap, PremultipliedColorU8};
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Not a format this module decodes.
    UnknownFormat,
    /// The codec rejected the data.
    Decode,
    /// Bigger than the caller's pixel budget.
    TooLarge,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Error::UnknownFormat => "not a PNG or JPEG",
            Error::Decode => "image data is corrupt or unsupported",
            Error::TooLarge => "image exceeds the pixel budget",
        })
    }
}

impl core::error::Error for Error {}

/// Decode a PNG or JPEG (sniffed), refusing images over `max_pixels`.
pub fn decode(bytes: &[u8], max_pixels: u64) -> Result<Pixmap, Error> {
    match crate::sniff(bytes) {
        crate::Format::Png => decode_png(bytes, max_pixels),
        crate::Format::Jpeg => decode_jpeg(bytes, max_pixels),
        _ => Err(Error::UnknownFormat),
    }
}

fn limits(max_pixels: u64) -> DecoderOptions {
    // Per-side caps keep the decoder from even allocating for a bomb.
    let side = max_pixels.min(1 << 30) as usize;
    DecoderOptions::new_fast()
        .set_max_width(side)
        .set_max_height(side)
}

/// Decode a PNG: any colour type and bit depth (16-bit is reduced to 8),
/// palette and `tRNS` transparency, Adam7 interlacing.
pub fn decode_png(bytes: &[u8], max_pixels: u64) -> Result<Pixmap, Error> {
    let opts = limits(max_pixels)
        .png_set_strip_to_8bit(true)
        .png_set_add_alpha_channel(true);
    let mut dec = zune_png::PngDecoder::new_with_options(ZCursor::new(bytes), opts);
    dec.decode_headers().map_err(|_| Error::Decode)?;
    let (w, h) = dec.dimensions().ok_or(Error::Decode)?;
    if w as u64 * h as u64 > max_pixels {
        return Err(Error::TooLarge);
    }
    let pixels = match dec.decode().map_err(|_| Error::Decode)? {
        zune_core::result::DecodingResult::U8(v) => v,
        _ => return Err(Error::Decode),
    };
    let cs = dec.colorspace().ok_or(Error::Decode)?;
    to_pixmap(w, h, &pixels, cs)
}

/// Decode a baseline or progressive JPEG (grayscale, YCbCr, CMYK).
pub fn decode_jpeg(bytes: &[u8], max_pixels: u64) -> Result<Pixmap, Error> {
    let opts = limits(max_pixels).jpeg_set_out_colorspace(ColorSpace::RGBA);
    let mut dec = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(bytes), opts);
    dec.decode_headers().map_err(|_| Error::Decode)?;
    let (w, h) = dec.dimensions().ok_or(Error::Decode)?;
    if w as u64 * h as u64 > max_pixels {
        return Err(Error::TooLarge);
    }
    let pixels = dec.decode().map_err(|_| Error::Decode)?;
    let cs = dec.output_colorspace().ok_or(Error::Decode)?;
    to_pixmap(w, h, &pixels, cs)
}

fn to_pixmap(w: usize, h: usize, px: &[u8], cs: ColorSpace) -> Result<Pixmap, Error> {
    let mut pm = Pixmap::new(w as u32, h as u32).ok_or(Error::TooLarge)?;
    let n = cs.num_components();
    if px.len() < w * h * n {
        return Err(Error::Decode);
    }
    let rgba = |s: &[u8]| -> [u8; 4] {
        match cs {
            ColorSpace::Luma => [s[0], s[0], s[0], 255],
            ColorSpace::LumaA => [s[0], s[0], s[0], s[1]],
            ColorSpace::RGB => [s[0], s[1], s[2], 255],
            ColorSpace::RGBA => [s[0], s[1], s[2], s[3]],
            ColorSpace::BGR => [s[2], s[1], s[0], 255],
            ColorSpace::BGRA => [s[2], s[1], s[0], s[3]],
            _ => [
                s[0],
                s.get(1).copied().unwrap_or(s[0]),
                s.get(2).copied().unwrap_or(s[0]),
                255,
            ],
        }
    };
    for (dst, src) in pm.pixels_mut().iter_mut().zip(px.chunks_exact(n)) {
        let [r, g, b, a] = rgba(src);
        *dst = tiny_skia::ColorU8::from_rgba(r, g, b, a).premultiply();
    }
    Ok(pm)
}

/// Build a premultiplied pixmap from straight RGBA8 (a helper for callers
/// with pixels from elsewhere).
pub fn pixmap_from_rgba(w: u32, h: u32, rgba: &[u8]) -> Option<Pixmap> {
    if rgba.len() != w as usize * h as usize * 4 {
        return None;
    }
    let mut pm = Pixmap::new(w, h)?;
    for (dst, s) in pm.pixels_mut().iter_mut().zip(rgba.chunks_exact(4)) {
        *dst = PremultipliedColorU8::from_rgba(
            ((s[0] as u16 * s[3] as u16 + 127) / 255) as u8,
            ((s[1] as u16 * s[3] as u16 + 127) / 255) as u8,
            ((s[2] as u16 * s[3] as u16 + 127) / 255) as u8,
            s[3],
        )?;
    }
    Some(pm)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// A minimal PNG encoder for tests (stored deflate via miniz).
    pub(crate) fn encode_png(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
        fn crc(data: &[u8]) -> u32 {
            let mut c = 0xFFFF_FFFFu32;
            for &b in data {
                c ^= b as u32;
                for _ in 0..8 {
                    c = if c & 1 != 0 {
                        0xEDB8_8320 ^ (c >> 1)
                    } else {
                        c >> 1
                    };
                }
            }
            !c
        }
        let mut raw = Vec::new();
        for row in rgba.chunks_exact(w as usize * 4) {
            raw.push(0);
            raw.extend_from_slice(row);
        }
        let z = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6);
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut chunk = |ty: &[u8], data: &[u8]| {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            let mut c = ty.to_vec();
            c.extend_from_slice(data);
            out.extend_from_slice(&c);
            out.extend_from_slice(&crc(&c).to_be_bytes());
        };
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&w.to_be_bytes());
        ihdr.extend_from_slice(&h.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        chunk(b"IHDR", &ihdr);
        chunk(b"IDAT", &z);
        chunk(b"IEND", &[]);
        out
    }

    #[test]
    fn png_roundtrip_premultiplies() {
        let png = encode_png(2, 1, &[255, 0, 0, 255, 0, 0, 255, 128]);
        let pm = decode(&png, 1 << 20).unwrap();
        assert_eq!((pm.width(), pm.height()), (2, 1));
        let p0 = pm.pixel(0, 0).unwrap();
        assert_eq!((p0.red(), p0.alpha()), (255, 255));
        let p1 = pm.pixel(1, 0).unwrap();
        assert_eq!(p1.alpha(), 128);
        assert!(p1.blue() <= 128, "premultiplied");
    }

    #[test]
    fn budget_and_garbage_are_errors() {
        let png = encode_png(4, 4, &[0; 64]);
        assert_eq!(decode(&png, 8), Err(Error::TooLarge));
        assert_eq!(decode(b"nope", 8), Err(Error::UnknownFormat));
        assert_eq!(decode(&png[..20], 1 << 20).map(|_| ()), Err(Error::Decode));
    }
}
