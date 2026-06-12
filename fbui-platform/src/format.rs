//! Pixel formats the platform layer can present.
//!
//! Phase 0 established that the back buffer is handed out as raw bytes with a
//! kernel-reported stride; the *format* tells the layer above how to pack into
//! those bytes. We deliberately keep this list short — the render layer's native
//! buffer is little-endian `0xAARRGGBB`, so [`PixelFormat::Xrgb8888`] is the
//! zero-conversion fast path and everything else is a conversion the backend or
//! render layer opts into.

/// Byte layout of one pixel in a presented frame.
///
/// Names follow DRM `fourcc` convention: the letters are channels in
/// **memory order is little-endian of the named word**, i.e. `Xrgb8888` is the
/// 32-bit word `0xXXRRGGBB`, which on a little-endian machine lands in memory as
/// `[BB, GG, RR, XX]`. That is exactly how a `u32` of `0x00RRGGBB` serializes,
/// which is why it's our copy-`memcpy` fast path (see the Phase 0 NOTES).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PixelFormat {
    /// 32 bpp, `0xXXRRGGBB`. Alpha/X byte ignored on scanout. The native path.
    Xrgb8888,
    /// 32 bpp, `0xAARRGGBB`. Same packing as `Xrgb8888`; alpha is meaningful to
    /// the renderer but ignored by the scanout hardware.
    Argb8888,
    /// 16 bpp, `0bRRRRRGGGGGGBBBBB`. Common on small/cheap panels; needs a
    /// down-convert from the renderer's 32-bit buffer.
    Rgb565,
}

impl PixelFormat {
    /// Bytes occupied by one pixel.
    pub const fn bytes_per_pixel(self) -> usize {
        match self {
            PixelFormat::Xrgb8888 | PixelFormat::Argb8888 => 4,
            PixelFormat::Rgb565 => 2,
        }
    }

    /// Whether the renderer's little-endian `0xAARRGGBB` buffer can be copied
    /// row-for-row into this format with no per-pixel conversion.
    pub const fn is_native_copy(self) -> bool {
        matches!(self, PixelFormat::Xrgb8888 | PixelFormat::Argb8888)
    }

    /// The DRM `fourcc` 32-bit code, for backends that talk to `drm-rs`.
    pub const fn drm_fourcc(self) -> u32 {
        // fourcc('X','R','2','4') etc. — little-endian packing of four ASCII
        // bytes, matching `drm::buffer::DrmFourcc`.
        const fn cc(a: u8, b: u8, c: u8, d: u8) -> u32 {
            (a as u32) | ((b as u32) << 8) | ((c as u32) << 16) | ((d as u32) << 24)
        }
        match self {
            PixelFormat::Xrgb8888 => cc(b'X', b'R', b'2', b'4'),
            PixelFormat::Argb8888 => cc(b'A', b'R', b'2', b'4'),
            PixelFormat::Rgb565 => cc(b'R', b'G', b'1', b'6'),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bpp() {
        assert_eq!(PixelFormat::Xrgb8888.bytes_per_pixel(), 4);
        assert_eq!(PixelFormat::Rgb565.bytes_per_pixel(), 2);
    }

    #[test]
    fn fourcc_matches_drm_ascii() {
        // "XR24" little-endian.
        assert_eq!(PixelFormat::Xrgb8888.drm_fourcc(), 0x3432_5258);
        assert_eq!(PixelFormat::Argb8888.drm_fourcc(), 0x3432_5241);
        assert_eq!(PixelFormat::Rgb565.drm_fourcc(), 0x3631_4752);
    }

    #[test]
    fn native_copy_only_for_32bpp() {
        assert!(PixelFormat::Xrgb8888.is_native_copy());
        assert!(!PixelFormat::Rgb565.is_native_copy());
    }
}
