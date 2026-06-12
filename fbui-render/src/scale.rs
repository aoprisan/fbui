//! HiDPI scale-factor plumbing: logical ↔ device-pixel conversion.
//!
//! One number, threaded end to end. The painter multiplies every logical
//! coordinate by it before handing geometry to tiny-skia, and damage is reported
//! in device pixels so copy-out and `Display::present` see physical regions.
//! Fractional factors (1.25, 1.5, …) are supported, not just integers — cheap
//! panels report odd DPIs and users set fractional UI scale.
//!
//! The cardinal rule is **round device rectangles outward**. A logical edge at
//! 10.4px covers device pixel 10; if we truncated the *width* we could clip the
//! last anti-aliased column out of a repaint and leave stale pixels on screen.
//! [`Scale::to_device_rect`] floors the origin and ceils the far edge.

use crate::geom::{IRect, Rect};

/// Device pixels per logical pixel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Scale(f32);

impl Scale {
    /// 1 device pixel per logical pixel — no scaling.
    pub const ONE: Scale = Scale(1.0);

    /// Build a scale, clamping to a sane positive range so a bogus DPI can't
    /// produce a zero/NaN transform that silently paints nothing.
    pub fn new(factor: f32) -> Self {
        let f = if factor.is_finite() && factor > 0.0 {
            factor.clamp(0.25, 16.0)
        } else {
            1.0
        };
        Scale(f)
    }

    pub fn factor(self) -> f32 {
        self.0
    }

    /// Convert a logical length to device pixels (no rounding — for the
    /// tiny-skia transform, which is itself floating point).
    pub fn to_device(self, logical: f32) -> f32 {
        logical * self.0
    }

    /// Convert a device length back to logical units (e.g. a pointer position
    /// arriving from the platform layer in physical pixels).
    pub fn to_logical(self, device: f32) -> f32 {
        device / self.0
    }

    /// Convert a logical rectangle to the smallest device-pixel rectangle that
    /// fully contains it: origin floored, far edge ceiled. This is what damage
    /// uses, so partially-covered edge pixels are always repainted.
    pub fn to_device_rect(self, r: Rect) -> IRect {
        if r.is_empty() {
            return IRect::EMPTY;
        }
        let x0 = (r.x * self.0).floor();
        let y0 = (r.y * self.0).floor();
        let x1 = (r.right() * self.0).ceil();
        let y1 = (r.bottom() * self.0).ceil();
        IRect::new(x0 as i32, y0 as i32, (x1 - x0) as u32, (y1 - y0) as u32)
    }

    /// The tiny-skia transform that maps logical drawing coords to device space.
    pub fn transform(self) -> tiny_skia::Transform {
        tiny_skia::Transform::from_scale(self.0, self.0)
    }
}

impl Default for Scale {
    fn default() -> Self {
        Scale::ONE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_scale_is_exact() {
        let s = Scale::new(2.0);
        assert_eq!(
            s.to_device_rect(Rect::new(1.0, 2.0, 3.0, 4.0)),
            IRect::new(2, 4, 6, 8)
        );
    }

    #[test]
    fn fractional_scale_rounds_outward() {
        let s = Scale::new(1.5);
        // x: floor(10*1.5)=15, right: ceil(10.4*1.5=15.6)=16 -> width 1 -> but
        // here use a rect that lands mid-pixel to prove the ceil.
        let r = Rect::new(10.0, 10.0, 0.4, 0.4); // right/bottom = 10.4
        let d = s.to_device_rect(r);
        assert_eq!(d.x, 15);
        assert_eq!(d.right(), 16); // ceil(15.6)
        assert_eq!(d.w, 1);
    }

    #[test]
    fn bogus_factors_clamp() {
        assert_eq!(Scale::new(f32::NAN).factor(), 1.0);
        assert_eq!(Scale::new(-3.0).factor(), 1.0);
        assert_eq!(Scale::new(1000.0).factor(), 16.0);
    }

    #[test]
    fn logical_round_trip() {
        let s = Scale::new(1.25);
        assert!((s.to_logical(s.to_device(40.0)) - 40.0).abs() < 1e-4);
    }
}
