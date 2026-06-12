//! Colors, as straight (non-premultiplied) 8-bit RGBA.
//!
//! The render layer's public surface speaks straight alpha because that is how
//! humans and stylesheets think (`#RRGGBBAA`). tiny-skia stores *premultiplied*
//! pixels internally; the conversion happens at the boundary in [`Color::to_tiny`]
//! and friends, never in caller code.

/// A straight-alpha sRGB color, one byte per channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const TRANSPARENT: Color = Color::rgba(0, 0, 0, 0);
    pub const BLACK: Color = Color::rgb(0, 0, 0);
    pub const WHITE: Color = Color::rgb(255, 255, 255);

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Color { r, g, b, a }
    }

    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Color::rgba(r, g, b, 255)
    }

    /// From a packed `0xAARRGGBB` word — the convention the platform scanout and
    /// most palettes use.
    pub const fn from_argb(argb: u32) -> Self {
        Color::rgba(
            (argb >> 16) as u8,
            (argb >> 8) as u8,
            argb as u8,
            (argb >> 24) as u8,
        )
    }

    /// Pack into `0xAARRGGBB`.
    pub const fn to_argb(self) -> u32 {
        (self.a as u32) << 24 | (self.r as u32) << 16 | (self.g as u32) << 8 | self.b as u32
    }

    /// Whether scanout can ignore alpha for this color (fully opaque).
    pub const fn is_opaque(self) -> bool {
        self.a == 255
    }

    /// This color with its alpha replaced.
    pub const fn with_alpha(self, a: u8) -> Self {
        Color { a, ..self }
    }

    /// tiny-skia's straight-alpha color (it premultiplies internally on use).
    pub fn to_tiny(self) -> tiny_skia::Color {
        tiny_skia::Color::from_rgba8(self.r, self.g, self.b, self.a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argb_round_trips() {
        let c = Color::rgba(0x12, 0x34, 0x56, 0x78);
        assert_eq!(c.to_argb(), 0x78_12_34_56);
        assert_eq!(Color::from_argb(0x78_12_34_56), c);
    }

    #[test]
    fn opacity_helpers() {
        assert!(Color::WHITE.is_opaque());
        assert!(!Color::WHITE.with_alpha(0).is_opaque());
        assert_eq!(Color::BLACK.with_alpha(128).a, 128);
    }
}
