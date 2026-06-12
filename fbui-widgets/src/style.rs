//! Layout style — a thin layer over `taffy`.
//!
//! Widgets describe *layout* (flex direction, size, padding, gap, alignment) as a
//! [`taffy::Style`]; taffy does the actual box math. We re-export the type and add
//! a few terse constructors so widget code reads cleanly, plus the conversions
//! between taffy's geometry and `fbui-render`'s logical [`Rect`].

use fbui_render::geom::Rect;

/// The layout style a widget contributes to the tree. Alias for clarity and so
/// the rest of the toolkit imports it from one place.
pub type Style = taffy::Style;

/// A fixed length dimension.
pub fn length(v: f32) -> taffy::Dimension {
    taffy::Dimension::length(v)
}

/// A percentage (0–1) dimension.
pub fn percent(v: f32) -> taffy::Dimension {
    taffy::Dimension::percent(v)
}

/// The `auto` dimension.
pub fn auto() -> taffy::Dimension {
    taffy::Dimension::auto()
}

/// A `taffy::Size` of fixed lengths.
pub fn size(w: f32, h: f32) -> taffy::Size<taffy::Dimension> {
    taffy::Size {
        width: length(w),
        height: length(h),
    }
}

/// Uniform length padding/margin as a taffy rect.
pub fn uniform(v: f32) -> taffy::Rect<taffy::LengthPercentage> {
    let l = taffy::LengthPercentage::length(v);
    taffy::Rect {
        left: l,
        right: l,
        top: l,
        bottom: l,
    }
}

/// Convert a resolved taffy layout (relative to its parent) plus a parent origin
/// into an absolute logical [`Rect`].
pub fn layout_to_rect(layout: &taffy::Layout, origin_x: f32, origin_y: f32) -> Rect {
    Rect::new(
        origin_x + layout.location.x,
        origin_y + layout.location.y,
        layout.size.width,
        layout.size.height,
    )
}
