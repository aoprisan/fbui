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

/// The smallest 1/2/5×10ⁿ at or above `raw` — the tick-step ladder the
/// instrument widgets ([`Chart`](crate::widgets::Chart),
/// [`Gauge`](crate::widgets::Gauge)) quantize their axes with.
pub(crate) fn nice_step(raw: f32) -> f32 {
    let raw = raw.max(f32::MIN_POSITIVE);
    let mag = 10.0f32.powf(raw.log10().floor());
    let norm = raw / mag;
    let m = if norm <= 1.0 {
        1.0
    } else if norm <= 2.0 {
        2.0
    } else if norm <= 5.0 {
        5.0
    } else {
        10.0
    };
    m * mag
}

/// Quantize a data extent outward to "nice" bounds: tick-step multiples with
/// the step from the 1-2-5 decades, targeting ~4 divisions. Quantizing is what
/// keeps an auto-range *stable* — it only moves when the data escapes the
/// current nice bounds.
pub(crate) fn nice_range(min: f32, max: f32) -> (f32, f32) {
    if !min.is_finite() || !max.is_finite() {
        return (0.0, 1.0);
    }
    let mut span = max - min;
    if span <= max.abs().max(1.0) * 1e-4 {
        // Flat (or near-flat) signal: manufacture a visible band around it
        // rather than a microscopic one that jitters with float noise.
        span = max.abs().max(1.0) * 0.2;
    }
    let step = nice_step(span / 4.0);
    let mut lo = (min / step).floor() * step;
    let mut hi = (max / step).ceil() * step;
    if hi <= lo {
        lo -= step;
        hi += step;
    }
    (lo, hi)
}

/// Trim trailing zeros off a tick label (`12.50` → `12.5`, `3.00` → `3`).
pub(crate) fn tick_label(v: f32) -> String {
    let s = format!("{v:.2}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" {
        "0".into()
    } else {
        s.to_string()
    }
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
