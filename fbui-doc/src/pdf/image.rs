//! Image XObjects and inline images (§8.9) → premultiplied pixmaps.

use alloc::vec::Vec;

use tiny_skia::{Pixmap, PremultipliedColorU8};

use super::color::ColorSpace;
use super::filter::Decoded;
use super::object::{Dict, Object, Stream};
use super::Document;

/// A decoded image ready to draw through the CTM.
pub struct DecodedImage {
    pub pixmap: Pixmap,
    /// Whether to sample smoothly (`/Interpolate`, or downscaling anyway).
    pub smooth: bool,
}

fn dim(doc: &Document, d: &Dict, long: &[u8], short: &[u8]) -> Option<u32> {
    let v = doc.get_in(d, long);
    let v = if v.is_null() { doc.get_in(d, short) } else { v };
    let n = v.as_i64()?;
    (1..=65_535).contains(&n).then_some(n as u32)
}

/// Pull `n` bits at bit offset `bit` (MSB first).
#[inline]
fn bits(data: &[u8], bit: usize, n: u32) -> u32 {
    if n == 8 {
        return data.get(bit / 8).copied().unwrap_or(0) as u32;
    }
    if n == 16 {
        let i = bit / 8;
        return (data.get(i).copied().unwrap_or(0) as u32) << 8
            | data.get(i + 1).copied().unwrap_or(0) as u32;
    }
    let mut v = 0;
    for k in 0..n as usize {
        let b = bit + k;
        v = v << 1 | ((data.get(b / 8).copied().unwrap_or(0) >> (7 - b % 8)) & 1) as u32;
    }
    v
}

/// Decode an image dict + its (byte-filter decoded) data. `fill` is the
/// current non-stroking colour (RGBA, straight) for stencil masks.
pub fn decode(
    doc: &Document,
    dict: &Dict,
    decoded: Decoded,
    resources: Option<&Dict>,
    fill: [u8; 4],
    max_pixels: u64,
) -> Option<DecodedImage> {
    let w = dim(doc, dict, b"Width", b"W")?;
    let h = dim(doc, dict, b"Height", b"H")?;
    if w as u64 * h as u64 > max_pixels {
        return None;
    }
    let smooth = doc.get_in(dict, b"Interpolate").as_bool().unwrap_or(false);
    let is_mask = doc.get_in(dict, b"ImageMask").as_bool().unwrap_or(false)
        || doc.get_in(dict, b"IM").as_bool().unwrap_or(false);
    let decode_arr: Vec<f32> = doc
        .get_in(dict, b"Decode")
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|o| doc.resolve(o).ok()?.as_f32())
                .collect()
        })
        .unwrap_or_default();

    let mut data = decoded.data;
    let mut bpc = doc.get_in(dict, b"BitsPerComponent").as_i64().unwrap_or(8) as u32;
    let mut cs_obj = doc.get_in(dict, b"ColorSpace");

    if let Some((codec, _)) = &decoded.image_filter {
        match codec.as_slice() {
            b"DCTDecode" | b"DCT" => {
                let (pixels, comps) = decode_jpeg(&data)?;
                data = pixels;
                bpc = 8;
                if cs_obj.is_null() {
                    cs_obj = Object::Name(match comps {
                        1 => b"DeviceGray".to_vec(),
                        4 => b"DeviceCMYK".to_vec(),
                        _ => b"DeviceRGB".to_vec(),
                    });
                }
            }
            // JPX / JBIG2 / CCITT: not in this subset. Draw a neutral
            // placeholder so the page layout still reads.
            _ => {
                let mut pm = Pixmap::new(1, 1)?;
                pm.fill(tiny_skia::Color::from_rgba8(0xd0, 0xd0, 0xd0, 0xff));
                return Some(DecodedImage {
                    pixmap: pm,
                    smooth: true,
                });
            }
        }
    }

    let mut pm = Pixmap::new(w, h)?;
    if is_mask {
        // Stencil: sample 0 paints (with Decode [0 1]); 1 leaves it.
        let invert = decode_arr.first().copied().unwrap_or(0.0) > 0.5;
        let row_bits = w as usize;
        let row_bytes = row_bits.div_ceil(8);
        let paint = PremultipliedColorU8::from_rgba(
            mul(fill[0], fill[3]),
            mul(fill[1], fill[3]),
            mul(fill[2], fill[3]),
            fill[3],
        )?;
        let clear = PremultipliedColorU8::TRANSPARENT;
        for (y, row) in pm.pixels_mut().chunks_exact_mut(w as usize).enumerate() {
            let base = y * row_bytes * 8;
            for (x, px) in row.iter_mut().enumerate() {
                let bit = bits(&data, base + x, 1) == 1;
                *px = if bit == invert { paint } else { clear };
            }
        }
        return Some(DecodedImage {
            pixmap: pm,
            smooth: true,
        });
    }

    let cs = ColorSpace::parse(doc, &cs_obj, resources);
    let n = cs.components();
    if !matches!(bpc, 1 | 2 | 4 | 8 | 16) {
        return None;
    }
    let max = ((1u32 << bpc) - 1) as f32;
    let ranges: Vec<(f32, f32)> = (0..n)
        .map(
            |i| match (decode_arr.get(2 * i), decode_arr.get(2 * i + 1)) {
                (Some(&a), Some(&b)) => (a, b),
                _ => cs.decode_range(i, bpc),
            },
        )
        .collect();
    let row_bytes = (w as usize * n * bpc as usize).div_ceil(8);
    let default_decode = decode_arr.is_empty();

    // Colour-key masking: raw sample ranges that become transparent.
    let key: Option<Vec<(u32, u32)>> = match doc.get_in(dict, b"Mask") {
        Object::Array(a) if a.len() >= 2 * n => Some(
            a.as_chunks::<2>()
                .0
                .iter()
                .map(|p| {
                    (
                        p[0].as_i64().unwrap_or(0) as u32,
                        p[1].as_i64().unwrap_or(0) as u32,
                    )
                })
                .collect(),
        ),
        _ => None,
    };

    // Lookup table for single-component 8-bit (or less) images: a byte →
    // colour table makes Indexed / Separation / Gray images one load each.
    let lut: Option<Vec<[u8; 3]>> = (n == 1 && bpc <= 8).then(|| {
        (0..=max as u32)
            .map(|v| {
                let (lo, hi) = ranges[0];
                let c = lo + v as f32 * (hi - lo) / max;
                to_u8(cs.to_rgb(&[c]))
            })
            .collect()
    });

    let mut comps = [0.0f32; 32];
    let mut raw = [0u32; 32];
    for (y, row) in pm.pixels_mut().chunks_exact_mut(w as usize).enumerate() {
        let row_start = y * row_bytes * 8;
        for (x, px) in row.iter_mut().enumerate() {
            let bit0 = row_start + x * n * bpc as usize;
            let rgb = if let Some(lut) = &lut {
                let v = bits(&data, bit0, bpc);
                raw[0] = v;
                lut[v as usize]
            } else if bpc == 8 && default_decode && matches!(cs, ColorSpace::Rgb) {
                let i = bit0 / 8;
                let s = data.get(i..i + 3).unwrap_or(&[0, 0, 0]);
                raw[..3].copy_from_slice(&[s[0] as u32, s[1] as u32, s[2] as u32]);
                [s[0], s[1], s[2]]
            } else {
                for i in 0..n.min(32) {
                    let v = bits(&data, bit0 + i * bpc as usize, bpc);
                    raw[i] = v;
                    let (lo, hi) = ranges[i];
                    comps[i] = lo + v as f32 * (hi - lo) / max;
                }
                to_u8(cs.to_rgb(&comps[..n.min(32)]))
            };
            let transparent = key.as_ref().is_some_and(|k| {
                k.iter()
                    .take(n)
                    .enumerate()
                    .all(|(i, &(lo, hi))| (lo..=hi).contains(&raw[i]))
            });
            *px = if transparent {
                PremultipliedColorU8::TRANSPARENT
            } else {
                PremultipliedColorU8::from_rgba(rgb[0], rgb[1], rgb[2], 255)
                    .unwrap_or(PremultipliedColorU8::TRANSPARENT)
            };
        }
    }

    // Soft mask (alpha channel) or explicit stencil mask.
    if let Some(s) = doc.get_in(dict, b"SMask").as_stream() {
        if let Some(alpha) = mask_plane(doc, s, false, max_pixels) {
            apply_alpha(&mut pm, &alpha);
        }
    } else if let Some(s) = doc.get_in(dict, b"Mask").as_stream() {
        if let Some(alpha) = mask_plane(doc, s, true, max_pixels) {
            apply_alpha(&mut pm, &alpha);
        }
    }
    Some(DecodedImage {
        pixmap: pm,
        smooth: smooth || w > 64,
    })
}

fn mul(c: u8, a: u8) -> u8 {
    ((c as u16 * a as u16 + 127) / 255) as u8
}

fn to_u8(rgb: [f32; 3]) -> [u8; 3] {
    let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    [c(rgb[0]), c(rgb[1]), c(rgb[2])]
}

/// An alpha plane: width, height, values.
struct Plane {
    w: u32,
    h: u32,
    a: Vec<u8>,
}

/// Decode an `SMask` (gray → alpha) or a stencil `Mask` (bit 1 = masked).
fn mask_plane(doc: &Document, s: &Stream, stencil: bool, max_pixels: u64) -> Option<Plane> {
    let d = &s.dict;
    let w = dim(doc, d, b"Width", b"W")?;
    let h = dim(doc, d, b"Height", b"H")?;
    if w as u64 * h as u64 > max_pixels {
        return None;
    }
    let decoded = doc.decode_stream(s).ok()?;
    let (data, bpc) = match &decoded.image_filter {
        Some((c, _)) if c == b"DCTDecode" || c == b"DCT" => (decode_jpeg(&decoded.data)?.0, 8),
        Some(_) => return None,
        None => (
            decoded.data,
            if stencil {
                1
            } else {
                doc.get_in(d, b"BitsPerComponent").as_i64().unwrap_or(8) as u32
            },
        ),
    };
    if !matches!(bpc, 1 | 2 | 4 | 8 | 16) {
        return None;
    }
    let decode_arr: Vec<f32> = doc
        .get_in(d, b"Decode")
        .as_array()
        .map(|a| a.iter().filter_map(Object::as_f32).collect())
        .unwrap_or_default();
    let (lo, hi) = (
        decode_arr.first().copied().unwrap_or(0.0),
        decode_arr.get(1).copied().unwrap_or(1.0),
    );
    let max = ((1u32 << bpc) - 1) as f32;
    let row_bytes = (w as usize * bpc as usize).div_ceil(8);
    let mut a = Vec::with_capacity((w * h) as usize);
    for y in 0..h as usize {
        for x in 0..w as usize {
            let v = bits(&data, (y * row_bytes) * 8 + x * bpc as usize, bpc) as f32 / max;
            let v = lo + v * (hi - lo);
            let alpha = if stencil { 1.0 - v } else { v };
            a.push((alpha.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
        }
    }
    Some(Plane { w, h, a })
}

fn apply_alpha(pm: &mut Pixmap, plane: &Plane) {
    let (w, h) = (pm.width(), pm.height());
    for (y, row) in pm.pixels_mut().chunks_exact_mut(w as usize).enumerate() {
        let sy = (y as u64 * plane.h as u64 / h as u64) as usize;
        for (x, px) in row.iter_mut().enumerate() {
            let sx = (x as u64 * plane.w as u64 / w as u64) as usize;
            let a = plane
                .a
                .get(sy * plane.w as usize + sx)
                .copied()
                .unwrap_or(255);
            let c = px.demultiply();
            let na = mul(c.alpha(), a);
            *px = PremultipliedColorU8::from_rgba(
                mul(c.red(), na),
                mul(c.green(), na),
                mul(c.blue(), na),
                na,
            )
            .unwrap_or(PremultipliedColorU8::TRANSPARENT);
        }
    }
}

/// Baseline/progressive JPEG → (interleaved 8-bit pixels, components).
pub fn decode_jpeg(data: &[u8]) -> Option<(Vec<u8>, usize)> {
    use zune_core::bytestream::ZCursor;
    use zune_core::colorspace::ColorSpace as Zcs;
    use zune_core::options::DecoderOptions;
    let mut probe = zune_jpeg::JpegDecoder::new(ZCursor::new(data));
    probe.decode_headers().ok()?;
    let input = probe.input_colorspace()?;
    let out = match input.num_components() {
        1 => Zcs::Luma,
        4 => Zcs::CMYK,
        _ => Zcs::RGB,
    };
    let opts = DecoderOptions::new_fast().jpeg_set_out_colorspace(out);
    let mut dec = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(data), opts);
    let pixels = dec.decode().ok()?;
    let comps = dec.output_colorspace()?.num_components();
    Some((pixels, comps))
}
