//! Small painting helpers shared by the widget set.

use fbui_render::geom::Rect;
use fbui_render::{Color, Painter, TextStyle};

use crate::theme::Theme;

/// A theme-derived text style at `size` in `color`.
pub(crate) fn text_style(theme: &Theme, size: f32, color: Color) -> TextStyle {
    TextStyle::new(size, color).family(theme.font.clone())
}

/// Draw the standard focus ring just inside `bounds`. Takes plain values (not a
/// `&Theme`) so callers can drop their theme borrow before grabbing the painter.
pub(crate) fn focus_ring(p: &mut Painter, bounds: Rect, radius: f32, accent: Color, width: f32) {
    p.stroke_rounded_rect(bounds.inset(width / 2.0), radius, accent, width);
}

/// Bounding box of two logical rects (empty is the identity).
pub(crate) fn union(a: Rect, b: Rect) -> Rect {
    if a.is_empty() {
        return b;
    }
    if b.is_empty() {
        return a;
    }
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    let right = a.right().max(b.right());
    let bottom = a.bottom().max(b.bottom());
    Rect::new(x, y, right - x, bottom - y)
}

/// Scale a color's RGB toward black by `f` (0–1).
pub(crate) fn darken(c: Color, f: f32) -> Color {
    Color::rgba(
        (c.r as f32 * f) as u8,
        (c.g as f32 * f) as u8,
        (c.b as f32 * f) as u8,
        c.a,
    )
}
