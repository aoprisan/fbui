//! Geometry for the render layer: floating-point *logical* coordinates for
//! drawing, and integer *device-pixel* rectangles for damage.
//!
//! The split is deliberate. Widgets and the painter work in logical units (a
//! "12pt button" should mean the same thing on a 1× laptop panel and a 2× HiDPI
//! screen); damage and copy-out work in physical device pixels, because that is
//! what gets memcpy'd into the scanout buffer. [`crate::scale::Scale`] converts
//! between them, rounding device rects *outward* so anti-aliased edges are never
//! clipped off a repaint.

/// A point in logical coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const fn new(x: f32, y: f32) -> Self {
        Point { x, y }
    }
}

/// A size in logical coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Size {
    pub w: f32,
    pub h: f32,
}

impl Size {
    pub const fn new(w: f32, h: f32) -> Self {
        Size { w, h }
    }
}

/// A logical-coordinate rectangle, origin + size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Rect { x, y, w, h }
    }

    pub const fn from_xy_size(p: Point, s: Size) -> Self {
        Rect::new(p.x, p.y, s.w, s.h)
    }

    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    pub fn is_empty(&self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }

    /// Shrink (positive) or grow (negative) by `d` on every side.
    pub fn inset(&self, d: f32) -> Rect {
        Rect::new(
            self.x + d,
            self.y + d,
            (self.w - 2.0 * d).max(0.0),
            (self.h - 2.0 * d).max(0.0),
        )
    }

    /// tiny-skia rect, or `None` if degenerate (zero/negative extent).
    pub fn to_tiny(&self) -> Option<tiny_skia::Rect> {
        tiny_skia::Rect::from_xywh(self.x, self.y, self.w, self.h)
    }
}

/// A half-open device-pixel rectangle `[x, x+w) × [y, y+h)`.
///
/// Mirrors `fbui_platform::geom::Rect` exactly (signed origin, unsigned extent)
/// so damage produced here drops straight into `Display::present` with no
/// translation. The union/intersect/clamp semantics match too — empty is the
/// union identity, so folding a damage list never special-cases the seed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IRect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl IRect {
    pub const EMPTY: IRect = IRect {
        x: 0,
        y: 0,
        w: 0,
        h: 0,
    };

    pub const fn new(x: i32, y: i32, w: u32, h: u32) -> Self {
        IRect { x, y, w, h }
    }

    /// The whole `w × h` surface, anchored at the origin.
    pub const fn from_wh(w: u32, h: u32) -> Self {
        IRect { x: 0, y: 0, w, h }
    }

    pub const fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }

    pub const fn right(&self) -> i32 {
        self.x + self.w as i32
    }

    pub const fn bottom(&self) -> i32 {
        self.y + self.h as i32
    }

    pub const fn area(&self) -> u64 {
        self.w as u64 * self.h as u64
    }

    /// Smallest rectangle covering both; empty is the identity.
    pub fn union(self, other: IRect) -> IRect {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = self.right().max(other.right());
        let bottom = self.bottom().max(other.bottom());
        IRect::new(x, y, (right - x) as u32, (bottom - y) as u32)
    }

    /// Overlap, or [`IRect::EMPTY`] if disjoint.
    pub fn intersect(self, other: IRect) -> IRect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        if right <= x || bottom <= y {
            IRect::EMPTY
        } else {
            IRect::new(x, y, (right - x) as u32, (bottom - y) as u32)
        }
    }

    /// Clamp to `[0,0]–[w,h]`, dropping any part off the surface. Always call
    /// this before using a rect to index a mapping, so stray damage can't OOB.
    pub fn clamp_to(self, w: u32, h: u32) -> IRect {
        self.intersect(IRect::from_wh(w, h))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_inset() {
        let r = Rect::new(10.0, 10.0, 100.0, 50.0);
        assert_eq!(r.inset(5.0), Rect::new(15.0, 15.0, 90.0, 40.0));
        // Over-inset clamps to zero extent, never negative.
        assert_eq!(r.inset(40.0).h, 0.0);
    }

    #[test]
    fn irect_union_identity() {
        let r = IRect::new(2, 3, 4, 5);
        assert_eq!(r.union(IRect::EMPTY), r);
        assert_eq!(IRect::EMPTY.union(r), r);
    }

    #[test]
    fn irect_intersect_and_clamp() {
        let a = IRect::new(0, 0, 10, 10);
        let b = IRect::new(5, 5, 10, 10);
        assert_eq!(a.intersect(b), IRect::new(5, 5, 5, 5));
        assert_eq!(
            IRect::new(-5, -5, 10, 10).clamp_to(100, 100),
            IRect::new(0, 0, 5, 5)
        );
        assert!(IRect::new(200, 0, 10, 10).clamp_to(100, 100).is_empty());
    }
}
