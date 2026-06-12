//! Tiny integer geometry: sizes, points, and pixel rectangles.
//!
//! The platform layer speaks in *physical device pixels* — there is no scale
//! factor here (that's `fbui-render`'s concern). `Rect` is used both for
//! pointer/touch coordinates and, crucially, for **damage**: the set of
//! rectangles a frame actually changed, which `present()` uses to copy out only
//! what moved.

/// Width/height in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Size {
    pub w: u32,
    pub h: u32,
}

impl Size {
    pub const fn new(w: u32, h: u32) -> Self {
        Size { w, h }
    }

    /// Total pixel count (`w * h`), widened to avoid overflow on large modes.
    pub const fn area(self) -> u64 {
        self.w as u64 * self.h as u64
    }
}

/// A point in physical pixels. Signed so relative pointer deltas fit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub const ORIGIN: Point = Point { x: 0, y: 0 };

    pub const fn new(x: i32, y: i32) -> Self {
        Point { x, y }
    }
}

/// A half-open rectangle `[x, x+w) × [y, y+h)` in physical pixels.
///
/// Stored as origin + size so an empty rect (`w == 0 || h == 0`) is
/// unambiguous and `union`/`intersect` stay branch-light.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub const EMPTY: Rect = Rect {
        x: 0,
        y: 0,
        w: 0,
        h: 0,
    };

    pub const fn new(x: i32, y: i32, w: u32, h: u32) -> Self {
        Rect { x, y, w, h }
    }

    /// The whole surface of `size`, anchored at the origin.
    pub const fn from_size(size: Size) -> Self {
        Rect {
            x: 0,
            y: 0,
            w: size.w,
            h: size.h,
        }
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

    /// Smallest rectangle covering both. The empty rect is the identity, so
    /// folding `union` over a damage list never has to special-case the seed.
    pub fn union(self, other: Rect) -> Rect {
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
        Rect {
            x,
            y,
            w: (right - x) as u32,
            h: (bottom - y) as u32,
        }
    }

    /// Overlap of the two rectangles, or [`Rect::EMPTY`] if they're disjoint.
    pub fn intersect(self, other: Rect) -> Rect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        if right <= x || bottom <= y {
            Rect::EMPTY
        } else {
            Rect {
                x,
                y,
                w: (right - x) as u32,
                h: (bottom - y) as u32,
            }
        }
    }

    /// Clamp this rectangle to lie within `[0,0]–[size]`, dropping any part that
    /// falls off the surface. Backends call this before using damage to index a
    /// mapping so out-of-range damage can never cause an OOB copy.
    pub fn clamp_to(self, size: Size) -> Rect {
        self.intersect(Rect::from_size(size))
    }

    pub const fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x < self.right() && p.y >= self.y && p.y < self.bottom()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn union_treats_empty_as_identity() {
        let r = Rect::new(2, 3, 4, 5);
        assert_eq!(r.union(Rect::EMPTY), r);
        assert_eq!(Rect::EMPTY.union(r), r);
        assert_eq!(Rect::EMPTY.union(Rect::EMPTY), Rect::EMPTY);
    }

    #[test]
    fn union_covers_both() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(20, 5, 10, 10);
        assert_eq!(a.union(b), Rect::new(0, 0, 30, 15));
    }

    #[test]
    fn intersect_disjoint_is_empty() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(10, 0, 10, 10); // touches edge, half-open => no overlap
        assert!(a.intersect(b).is_empty());
    }

    #[test]
    fn intersect_overlap() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(5, 5, 10, 10);
        assert_eq!(a.intersect(b), Rect::new(5, 5, 5, 5));
    }

    #[test]
    fn clamp_drops_offscreen() {
        let size = Size::new(100, 100);
        assert_eq!(
            Rect::new(-5, -5, 10, 10).clamp_to(size),
            Rect::new(0, 0, 5, 5)
        );
        assert_eq!(
            Rect::new(95, 95, 20, 20).clamp_to(size),
            Rect::new(95, 95, 5, 5)
        );
        assert!(Rect::new(200, 0, 10, 10).clamp_to(size).is_empty());
    }

    #[test]
    fn contains_is_half_open() {
        let r = Rect::new(0, 0, 10, 10);
        assert!(r.contains(Point::new(0, 0)));
        assert!(r.contains(Point::new(9, 9)));
        assert!(!r.contains(Point::new(10, 10)));
    }
}
