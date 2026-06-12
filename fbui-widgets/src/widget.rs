//! The [`Widget`] trait: the one interface every node implements.
//!
//! Object-safe and generic over the application message type `Msg` (one concrete
//! type per [`Ui`](crate::Ui)), so the tree stores `Box<dyn Widget<Msg>>`. A
//! widget contributes a layout style, optionally an intrinsic measure (text,
//! images), paints itself, and handles events — emitting messages and damage
//! through the [`EventCtx`]. Tree *structure* (children) is owned by the `Ui`, not
//! the widget, so containers hold only their layout config.

use std::any::Any;

use fbui_render::geom::{Point, Size};
use fbui_render::FontContext;

use crate::ctx::{EventCtx, PaintCtx};
use crate::style::Style;
use crate::theme::Theme;

/// Available space for a measure, re-exported from taffy.
pub use taffy::AvailableSpace;
/// Known dimensions for a measure (`Some` = constrained), re-exported from taffy.
pub type KnownDims = taffy::Size<Option<f32>>;
/// Available space along both axes.
pub type AvailableSize = taffy::Size<AvailableSpace>;

/// A node in the widget tree.
///
/// `Msg` is the application's message type. Default method bodies make the common
/// case (a non-focusable leaf with no intrinsic size) a two-method impl: `paint`
/// and `as_any`.
pub trait Widget<Msg>: Any {
    /// The layout style (flex, size, padding, …) contributed to taffy.
    fn layout_style(&self, theme: &Theme) -> Style;

    /// Intrinsic content size for leaf widgets (text, image). Return `None` for
    /// widgets whose size is purely a function of their layout style. `known`
    /// carries any axis already constrained by layout; `available` is the space
    /// offered along each axis.
    fn measure(
        &mut self,
        _fonts: &mut FontContext,
        _theme: &Theme,
        _known: KnownDims,
        _available: AvailableSize,
    ) -> Option<Size> {
        None
    }

    /// Paint this widget within `ctx.bounds()`.
    fn paint(&self, ctx: &mut PaintCtx);

    /// Handle one event. Default: ignore.
    fn event(&mut self, _ctx: &mut EventCtx<Msg>) {}

    /// Whether this widget accepts keyboard focus (tab order, key events).
    fn focusable(&self) -> bool {
        false
    }

    /// Whether this widget clips its children to its bounds (scroll viewports).
    fn clips(&self) -> bool {
        false
    }

    /// A translation applied to this widget's children's positions (scroll
    /// offset). Default: none.
    fn content_offset(&self) -> Point {
        Point::new(0.0, 0.0)
    }

    /// Inform a scrolling widget of its content vs. viewport size after layout,
    /// so it can clamp its scroll offset. Called by the `Ui` only for widgets
    /// that [`clips`](Widget::clips). Default: ignore.
    fn set_scroll_metrics(&mut self, _content: Size, _viewport: Size) {}

    /// Downcast hook so the application can mutate a concrete widget by id via
    /// [`Ui::with`](crate::Ui::with). Implementors return `self`.
    fn as_any_mut(&mut self) -> &mut dyn Any;
}
