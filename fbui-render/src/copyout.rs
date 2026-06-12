//! Copying the shadow buffer out to a scanout back buffer — damaged spans only.
//!
//! This is the one place the headless renderer touches "the hardware shape" of a
//! frame: a destination byte slice with a **kernel-reported stride** and a target
//! [`TargetFormat`], exactly what `fbui_platform::Frame` hands out. We never
//! assume `stride == width * bpp`, and we copy only the rows and columns inside
//! each damage rect, because the destination may be write-combined memory where
//! touching an untouched pixel is wasted bandwidth.
//!
//! ### Byte order
//!
//! tiny-skia stores pixels premultiplied, in memory order `[R, G, B, A]`. The
//! scanout formats are named by their little-endian 32-bit word: `Xrgb8888` is
//! `0xXXRRGGBB`, i.e. memory `[B, G, R, X]`. So even the "32-bit" path is a
//! red/blue swap, not a raw `memcpy` — cheap, sequential, and still damage-bounded.
//!
//! The shadow is expected to be **opaque** (the surface clears to an opaque base
//! before painting), so premultiplied equals straight alpha and we can take the
//! channels as-is.

use crate::geom::IRect;

/// Byte layout of the destination scanout buffer. Mirrors
/// `fbui_platform::PixelFormat`; the `platform` glue maps between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetFormat {
    /// 32 bpp `0xXXRRGGBB`; alpha byte forced opaque.
    Xrgb8888,
    /// 32 bpp `0xAARRGGBB`; shadow alpha preserved.
    Argb8888,
    /// 16 bpp `0bRRRRRGGGGGGBBBBB`.
    Rgb565,
}

impl TargetFormat {
    pub const fn bytes_per_pixel(self) -> usize {
        match self {
            TargetFormat::Xrgb8888 | TargetFormat::Argb8888 => 4,
            TargetFormat::Rgb565 => 2,
        }
    }
}

/// Copy the damaged regions of `shadow` (a tiny-skia pixmap) into `dst`.
///
/// `dst` is `dst_stride * height` bytes; `damage` rects must already be clamped
/// to the surface (the painter does this). Rects are processed in order; passing
/// the full-surface rect performs a complete blit.
pub fn copy_out(
    shadow: &tiny_skia::Pixmap,
    dst: &mut [u8],
    dst_stride: usize,
    format: TargetFormat,
    damage: &[IRect],
) {
    let sw = shadow.width() as usize;
    let sh = shadow.height() as usize;
    let src = shadow.data();
    let bpp = format.bytes_per_pixel();

    for rect in damage {
        // Defensive clamp: never index outside either buffer.
        let r = rect.clamp_to(sw as u32, sh as u32);
        if r.is_empty() {
            continue;
        }
        let x0 = r.x as usize;
        let y0 = r.y as usize;
        let cols = r.w as usize;

        for y in y0..y0 + r.h as usize {
            let src_row = &src[(y * sw + x0) * 4..(y * sw + x0 + cols) * 4];
            let dst_off = y * dst_stride + x0 * bpp;
            let dst_row = &mut dst[dst_off..dst_off + cols * bpp];
            match format {
                TargetFormat::Xrgb8888 => convert_row_32(src_row, dst_row, 0xff),
                TargetFormat::Argb8888 => convert_row_argb(src_row, dst_row),
                TargetFormat::Rgb565 => convert_row_565(src_row, dst_row),
            }
        }
    }
}

/// `[R,G,B,A]` -> `[B,G,R,X]` with a fixed X byte (Xrgb8888 fast path).
fn convert_row_32(src: &[u8], dst: &mut [u8], x: u8) {
    for (s, d) in src.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
        d[0] = s[2];
        d[1] = s[1];
        d[2] = s[0];
        d[3] = x;
    }
}

/// `[R,G,B,A]` -> `[B,G,R,A]`, preserving alpha (Argb8888).
fn convert_row_argb(src: &[u8], dst: &mut [u8]) {
    for (s, d) in src.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
        d[0] = s[2];
        d[1] = s[1];
        d[2] = s[0];
        d[3] = s[3];
    }
}

/// `[R,G,B,A]` -> little-endian RGB565 (`[lo, hi]`).
fn convert_row_565(src: &[u8], dst: &mut [u8]) {
    for (s, d) in src.chunks_exact(4).zip(dst.chunks_exact_mut(2)) {
        let v: u16 =
            ((s[0] as u16 & 0xf8) << 8) | ((s[1] as u16 & 0xfc) << 3) | ((s[2] as u16) >> 3);
        d[0] = v as u8;
        d[1] = (v >> 8) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn red_shadow(w: u32, h: u32) -> tiny_skia::Pixmap {
        let mut p = tiny_skia::Pixmap::new(w, h).unwrap();
        p.fill(tiny_skia::Color::from_rgba8(200, 100, 50, 255));
        p
    }

    #[test]
    fn xrgb_swaps_red_and_blue() {
        let shadow = red_shadow(2, 1);
        let mut dst = vec![0u8; 2 * 4];
        copy_out(
            &shadow,
            &mut dst,
            8,
            TargetFormat::Xrgb8888,
            &[IRect::from_wh(2, 1)],
        );
        // memory [B, G, R, X] => [50, 100, 200, 255]
        assert_eq!(&dst[0..4], &[50, 100, 200, 0xff]);
        assert_eq!(&dst[4..8], &[50, 100, 200, 0xff]);
    }

    #[test]
    fn respects_stride_padding() {
        let shadow = red_shadow(1, 2);
        // Destination has 4 bytes of row padding beyond the 1px (4 byte) row.
        let stride = 8;
        let mut dst = vec![0u8; stride * 2];
        copy_out(
            &shadow,
            &mut dst,
            stride,
            TargetFormat::Xrgb8888,
            &[IRect::from_wh(1, 2)],
        );
        assert_eq!(&dst[0..4], &[50, 100, 200, 0xff]);
        assert_eq!(&dst[4..8], &[0, 0, 0, 0]); // padding untouched
        assert_eq!(&dst[8..12], &[50, 100, 200, 0xff]);
    }

    #[test]
    fn copies_only_damaged_span() {
        let mut shadow = tiny_skia::Pixmap::new(4, 1).unwrap();
        // Paint pixel (2,0) distinctly; leave the rest black.
        shadow.pixels_mut()[2] = tiny_skia::ColorU8::from_rgba(10, 20, 30, 255).premultiply();
        let mut dst = vec![0xAAu8; 4 * 4];
        copy_out(
            &shadow,
            &mut dst,
            16,
            TargetFormat::Xrgb8888,
            &[IRect::new(2, 0, 1, 1)],
        );
        // Only the third pixel's 4 bytes changed.
        assert_eq!(&dst[0..8], &[0xAA; 8]);
        assert_eq!(&dst[8..12], &[30, 20, 10, 0xff]);
        assert_eq!(&dst[12..16], &[0xAA; 4]);
    }

    #[test]
    fn rgb565_packs_to_two_bytes() {
        let shadow = red_shadow(1, 1); // (200,100,50)
        let mut dst = vec![0u8; 2];
        copy_out(
            &shadow,
            &mut dst,
            2,
            TargetFormat::Rgb565,
            &[IRect::from_wh(1, 1)],
        );
        let v = u16::from_le_bytes([dst[0], dst[1]]);
        assert_eq!((v >> 11) & 0x1f, 200 >> 3);
        assert_eq!((v >> 5) & 0x3f, 100 >> 2);
        assert_eq!(v & 0x1f, 50 >> 3);
    }
}
