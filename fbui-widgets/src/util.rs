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

/// Scale a color's RGB toward black by `f` (0–1).
pub(crate) fn darken(c: Color, f: f32) -> Color {
    Color::rgba(
        (c.r as f32 * f) as u8,
        (c.g as f32 * f) as u8,
        (c.b as f32 * f) as u8,
        c.a,
    )
}
