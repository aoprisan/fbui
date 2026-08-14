//! Display rotation: painting a portrait UI on a landscape panel (and every
//! other quarter-turn combination) without the widgets knowing.
//!
//! Portrait-mounted screens are the norm in signage and kiosks, but panels
//! almost always scan out landscape. Rather than teach layout and painting
//! about rotated coordinates, the **surface renders in UI orientation** (the
//! rotated dimensions) and the rotation is applied at the one place pixels
//! leave the shadow buffer: copy-out. Input travels the other way — panel
//! coordinates map back into UI space with [`Rotation::map_panel_point`].
//!
//! [`Rotation`] is the amount the UI appears turned **clockwise** on the
//! unrotated panel. For a panel physically mounted on its side, pick the
//! rotation that makes the UI upright: a landscape panel stood on its left
//! edge shows an upright portrait UI at [`Rotation::Rot90`].
//!
//! Everything here is pure integer/float geometry, testable headless. The
//! rotated copy-out itself lives in [`crate::copyout`]; the runner applies the
//! input mapping.

use crate::geom::IRect;

/// A quarter-turn rotation of the UI on the panel, clockwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Rotation {
    /// UI and panel agree; copy-out is the plain row-copy fast path.
    #[default]
    Rot0,
    /// UI turned 90° clockwise on the panel (portrait UI on a landscape
    /// panel stood on its left edge).
    Rot90,
    /// Upside down.
    Rot180,
    /// UI turned 270° clockwise (= 90° counter-clockwise).
    Rot270,
}

impl Rotation {
    /// Parse a rotation from degrees — the `FBUI_ROTATE` values. Accepts the
    /// four quarter turns only.
    pub fn from_degrees(deg: u32) -> Option<Rotation> {
        match deg % 360 {
            0 => Some(Rotation::Rot0),
            90 => Some(Rotation::Rot90),
            180 => Some(Rotation::Rot180),
            270 => Some(Rotation::Rot270),
            _ => None,
        }
    }

    /// The rotation in degrees (0/90/180/270).
    pub fn degrees(self) -> u32 {
        match self {
            Rotation::Rot0 => 0,
            Rotation::Rot90 => 90,
            Rotation::Rot180 => 180,
            Rotation::Rot270 => 270,
        }
    }

    /// The rotation that undoes this one.
    pub fn inverse(self) -> Rotation {
        match self {
            Rotation::Rot0 => Rotation::Rot0,
            Rotation::Rot90 => Rotation::Rot270,
            Rotation::Rot180 => Rotation::Rot180,
            Rotation::Rot270 => Rotation::Rot90,
        }
    }

    /// Whether this rotation swaps width and height (the quarter turns).
    pub fn swaps_axes(self) -> bool {
        matches!(self, Rotation::Rot90 | Rotation::Rot270)
    }

    /// The UI-orientation (surface) size for a panel of `(panel_w, panel_h)`
    /// physical pixels: swapped for the quarter turns.
    pub fn surface_size(self, panel_w: u32, panel_h: u32) -> (u32, u32) {
        if self.swaps_axes() {
            (panel_h, panel_w)
        } else {
            (panel_w, panel_h)
        }
    }

    /// Map a **pixel** (integer cell) from surface space into panel space.
    /// `(sw, sh)` are the surface (UI-orientation) dimensions.
    #[inline]
    pub fn map_pixel(self, x: u32, y: u32, sw: u32, sh: u32) -> (u32, u32) {
        match self {
            Rotation::Rot0 => (x, y),
            // Panel is (sh × sw): surface column x becomes panel row x, and
            // surface row y lands at panel column (sh - 1 - y).
            Rotation::Rot90 => (sh - 1 - y, x),
            Rotation::Rot180 => (sw - 1 - x, sh - 1 - y),
            Rotation::Rot270 => (y, sw - 1 - x),
        }
    }

    /// Map a device-pixel rect from surface space into panel space. `(sw, sh)`
    /// are the surface dimensions; the result is in panel dimensions
    /// (`surface_size` swapped back).
    pub fn map_rect(self, r: IRect, sw: u32, sh: u32) -> IRect {
        let (sw, sh) = (sw as i32, sh as i32);
        let (w, h) = (r.w as i32, r.h as i32);
        match self {
            Rotation::Rot0 => r,
            Rotation::Rot90 => IRect::new(sh - (r.y + h), r.x, r.h, r.w),
            Rotation::Rot180 => IRect::new(sw - (r.x + w), sh - (r.y + h), r.w, r.h),
            Rotation::Rot270 => IRect::new(r.y, sw - (r.x + w), r.h, r.w),
        }
    }

    /// Map a continuous point from **panel** space into surface (UI) space —
    /// the input direction. `(panel_w, panel_h)` are the panel's physical
    /// dimensions.
    pub fn map_panel_point(self, x: f32, y: f32, panel_w: f32, panel_h: f32) -> (f32, f32) {
        match self {
            Rotation::Rot0 => (x, y),
            Rotation::Rot90 => (y, panel_w - x),
            Rotation::Rot180 => (panel_w - x, panel_h - y),
            Rotation::Rot270 => (panel_h - y, x),
        }
    }

    /// Rotate a relative pointer delta from panel space into surface space.
    pub fn map_delta(self, dx: f32, dy: f32) -> (f32, f32) {
        match self {
            Rotation::Rot0 => (dx, dy),
            Rotation::Rot90 => (dy, -dx),
            Rotation::Rot180 => (-dx, -dy),
            Rotation::Rot270 => (-dy, dx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn degrees_roundtrip() {
        for deg in [0u32, 90, 180, 270] {
            assert_eq!(Rotation::from_degrees(deg).unwrap().degrees(), deg);
        }
        assert_eq!(Rotation::from_degrees(360), Some(Rotation::Rot0));
        assert_eq!(Rotation::from_degrees(45), None);
    }

    #[test]
    fn surface_size_swaps_for_quarter_turns() {
        assert_eq!(Rotation::Rot0.surface_size(1920, 1080), (1920, 1080));
        assert_eq!(Rotation::Rot90.surface_size(1920, 1080), (1080, 1920));
        assert_eq!(Rotation::Rot180.surface_size(1920, 1080), (1920, 1080));
        assert_eq!(Rotation::Rot270.surface_size(1920, 1080), (1080, 1920));
    }

    /// Walk every surface pixel of a small surface: the panel image must be a
    /// permutation (each panel pixel hit exactly once, in bounds).
    #[test]
    fn map_pixel_is_a_bijection() {
        let (sw, sh) = (3u32, 5u32);
        for rot in [
            Rotation::Rot0,
            Rotation::Rot90,
            Rotation::Rot180,
            Rotation::Rot270,
        ] {
            // Axis swap is symmetric, so this also converts surface -> panel.
            let (pw, ph) = rot.surface_size(sw, sh);
            let mut seen = vec![false; (pw * ph) as usize];
            for y in 0..sh {
                for x in 0..sw {
                    let (px, py) = rot.map_pixel(x, y, sw, sh);
                    assert!(px < pw && py < ph, "{rot:?}: ({x},{y}) -> ({px},{py})");
                    let i = (py * pw + px) as usize;
                    assert!(!seen[i], "{rot:?}: panel pixel ({px},{py}) hit twice");
                    seen[i] = true;
                }
            }
            assert!(seen.iter().all(|&s| s), "{rot:?}: panel fully covered");
        }
    }

    /// The corner pixels land where a clockwise turn of the image puts them.
    #[test]
    fn map_pixel_corners() {
        // Surface 4×2 (w×h). Rot90 => panel 2×4.
        let r = Rotation::Rot90;
        assert_eq!(r.map_pixel(0, 0, 4, 2), (1, 0)); // top-left -> top-right
        assert_eq!(r.map_pixel(3, 0, 4, 2), (1, 3)); // top-right -> bottom-right
        assert_eq!(r.map_pixel(0, 1, 4, 2), (0, 0)); // bottom-left -> top-left
                                                     // Rot180 keeps dims, flips both.
        let r = Rotation::Rot180;
        assert_eq!(r.map_pixel(0, 0, 4, 2), (3, 1));
        // Rot270: top-left -> bottom-left.
        let r = Rotation::Rot270;
        assert_eq!(r.map_pixel(0, 0, 4, 2), (0, 3));
        assert_eq!(r.map_pixel(3, 0, 4, 2), (0, 0)); // top-right -> top-left
    }

    /// A rect maps to exactly the bounding box of its pixels' images.
    #[test]
    fn map_rect_matches_pixel_map() {
        let (sw, sh) = (7u32, 4u32);
        let r = IRect::new(1, 2, 3, 2);
        for rot in [
            Rotation::Rot0,
            Rotation::Rot90,
            Rotation::Rot180,
            Rotation::Rot270,
        ] {
            let mapped = rot.map_rect(r, sw, sh);
            let mut min = (u32::MAX, u32::MAX);
            let mut max = (0u32, 0u32);
            for y in r.y as u32..(r.y + r.h as i32) as u32 {
                for x in r.x as u32..(r.x + r.w as i32) as u32 {
                    let (px, py) = rot.map_pixel(x, y, sw, sh);
                    min = (min.0.min(px), min.1.min(py));
                    max = (max.0.max(px), max.1.max(py));
                }
            }
            assert_eq!(
                (mapped.x as u32, mapped.y as u32),
                min,
                "{rot:?} rect origin"
            );
            assert_eq!(
                (mapped.w, mapped.h),
                (max.0 - min.0 + 1, max.1 - min.1 + 1),
                "{rot:?} rect size"
            );
        }
    }

    /// Panel-point mapping inverts the pixel mapping (continuous form): a
    /// panel point at a pixel's center maps into that pixel's cell in surface
    /// space.
    #[test]
    fn panel_point_inverts_pixel_map() {
        let (sw, sh) = (6u32, 3u32);
        for rot in [
            Rotation::Rot0,
            Rotation::Rot90,
            Rotation::Rot180,
            Rotation::Rot270,
        ] {
            let (pw, ph) = if rot.swaps_axes() { (sh, sw) } else { (sw, sh) };
            for y in 0..sh {
                for x in 0..sw {
                    let (px, py) = rot.map_pixel(x, y, sw, sh);
                    let (ux, uy) =
                        rot.map_panel_point(px as f32 + 0.5, py as f32 + 0.5, pw as f32, ph as f32);
                    assert_eq!(
                        (ux.floor() as u32, uy.floor() as u32),
                        (x, y),
                        "{rot:?}: panel center of ({px},{py})"
                    );
                }
            }
        }
    }

    #[test]
    fn deltas_rotate_with_the_ui() {
        // Physical "right" on the panel… (UI top edge faces panel-right at
        // Rot90, so moving right physically moves toward the UI's top)
        let (dx, dy) = (10.0, 0.0);
        assert_eq!(Rotation::Rot90.map_delta(dx, dy), (0.0, -10.0));
        // "left" upside down,
        assert_eq!(Rotation::Rot180.map_delta(dx, dy), (-10.0, 0.0));
        // and "down" at 270°.
        assert_eq!(Rotation::Rot270.map_delta(dx, dy), (0.0, 10.0));
    }

    #[test]
    fn inverse_composes_to_identity() {
        for rot in [
            Rotation::Rot0,
            Rotation::Rot90,
            Rotation::Rot180,
            Rotation::Rot270,
        ] {
            let (sw, sh) = (5u32, 9u32);
            let (pw, ph) = rot.surface_size(sw, sh); // panel dims for this surface
            let (x, y) = rot.map_pixel(2, 7, sw, sh);
            // Mapping the panel pixel through the inverse (treating the panel
            // as a surface of its own dims) returns the original.
            assert_eq!(rot.inverse().map_pixel(x, y, pw, ph), (2, 7), "{rot:?}");
        }
    }
}
