//! Vector paths in logical coordinates.
//!
//! Thin newtypes over tiny-skia's path types so callers build in *logical* units
//! and never touch the device transform — the painter applies the scale when it
//! draws. The one value-add is [`Path::rounded_rect`], which tiny-skia's builder
//! doesn't provide: rounded corners are approximated with the standard cubic
//! Bézier arc (control-point distance `κ·r`).

use crate::geom::Rect;

/// Quarter-circle Bézier control-point ratio: `4/3·tan(π/8)`.
const KAPPA: f32 = 0.552_285;

/// An immutable, finished path in logical coordinates.
#[derive(Debug, Clone)]
pub struct Path(pub(crate) tiny_skia::Path);

impl Path {
    /// The logical-space bounding box of the path.
    pub fn bounds(&self) -> Rect {
        let b = self.0.bounds();
        Rect::new(b.x(), b.y(), b.width(), b.height())
    }

    /// An axis-aligned rectangle path, or `None` if degenerate.
    pub fn rect(r: Rect) -> Option<Path> {
        let mut pb = tiny_skia::PathBuilder::new();
        pb.push_rect(r.to_tiny()?);
        pb.finish().map(Path)
    }

    /// A rounded rectangle. `radius` is clamped to half the shorter side so an
    /// over-large radius yields a stadium/circle rather than self-intersecting.
    /// `None` if the rect is degenerate.
    pub fn rounded_rect(r: Rect, radius: f32) -> Option<Path> {
        if r.is_empty() {
            return None;
        }
        let radius = radius.clamp(0.0, (r.w.min(r.h)) / 2.0);
        if radius <= 0.0 {
            return Path::rect(r);
        }
        let (l, t, rt, b) = (r.x, r.y, r.right(), r.bottom());
        let c = radius * KAPPA; // control-point offset

        let mut pb = tiny_skia::PathBuilder::new();
        pb.move_to(l + radius, t);
        pb.line_to(rt - radius, t);
        pb.cubic_to(rt - radius + c, t, rt, t + radius - c, rt, t + radius);
        pb.line_to(rt, b - radius);
        pb.cubic_to(rt, b - radius + c, rt - radius + c, b, rt - radius, b);
        pb.line_to(l + radius, b);
        pb.cubic_to(l + radius - c, b, l, b - radius + c, l, b - radius);
        pb.line_to(l, t + radius);
        pb.cubic_to(l, t + radius - c, l + radius - c, t, l + radius, t);
        pb.close();
        pb.finish().map(Path)
    }
}

/// Builder for an open/closed path in logical coordinates.
#[derive(Debug, Default)]
pub struct PathBuilder(tiny_skia::PathBuilder);

impl PathBuilder {
    pub fn new() -> Self {
        PathBuilder(tiny_skia::PathBuilder::new())
    }

    pub fn move_to(&mut self, x: f32, y: f32) -> &mut Self {
        self.0.move_to(x, y);
        self
    }

    pub fn line_to(&mut self, x: f32, y: f32) -> &mut Self {
        self.0.line_to(x, y);
        self
    }

    pub fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) -> &mut Self {
        self.0.quad_to(cx, cy, x, y);
        self
    }

    pub fn cubic_to(
        &mut self,
        c1x: f32,
        c1y: f32,
        c2x: f32,
        c2y: f32,
        x: f32,
        y: f32,
    ) -> &mut Self {
        self.0.cubic_to(c1x, c1y, c2x, c2y, x, y);
        self
    }

    pub fn close(&mut self) -> &mut Self {
        self.0.close();
        self
    }

    /// Finish into an immutable [`Path`], or `None` if no usable geometry was
    /// added (e.g. a single `move_to`).
    pub fn finish(self) -> Option<Path> {
        self.0.finish().map(Path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounded_rect_bounds_match_input() {
        let r = Rect::new(10.0, 20.0, 100.0, 60.0);
        let p = Path::rounded_rect(r, 12.0).unwrap();
        let b = p.bounds();
        assert!((b.x - 10.0).abs() < 0.5 && (b.y - 20.0).abs() < 0.5);
        assert!((b.w - 100.0).abs() < 0.5 && (b.h - 60.0).abs() < 0.5);
    }

    #[test]
    fn zero_radius_is_a_rect() {
        assert!(Path::rounded_rect(Rect::new(0.0, 0.0, 10.0, 10.0), 0.0).is_some());
    }

    #[test]
    fn degenerate_is_none() {
        assert!(Path::rounded_rect(Rect::new(0.0, 0.0, 0.0, 10.0), 4.0).is_none());
    }

    #[test]
    fn builder_needs_geometry() {
        let mut pb = PathBuilder::new();
        pb.move_to(1.0, 1.0);
        assert!(pb.finish().is_none());
    }
}
