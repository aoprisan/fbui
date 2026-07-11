//! The [`Widget`] trait: the one interface every node implements.
//!
//! Object-safe and generic over the application message type `Msg` (one concrete
//! type per [`Ui`](crate::Ui)), so the tree stores `Box<dyn Widget<Msg>>`. A
//! widget contributes a layout style, optionally an intrinsic measure (text,
//! images), paints itself, and handles events — emitting messages and damage
//! through the [`EventCtx`]. Tree *structure* (children) is owned by the `Ui`, not
//! the widget, so containers hold only their layout config.

use std::any::Any;

use fbui_render::geom::{Point, Rect, Size};
use fbui_render::FontContext;

use crate::ctx::{AnimCtx, EventCtx, PaintCtx};
use crate::style::Style;
use crate::theme::Theme;

/// What a widget's [`animate`](Widget::animate) step changed this frame, and
/// whether it wants to keep being ticked.
///
/// Returned each frame so the [`Ui`](crate::Ui) knows what to mark dirty and
/// whether the animation clock must keep running (kinetic scroll coasting, say).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Anim {
    /// The widget's appearance changed; repaint its bounds.
    pub repaint: bool,
    /// The widget's geometry changed (e.g. a scroll offset that re-places
    /// children); a relayout is needed before repaint.
    pub relayout: bool,
    /// The animation is still running; keep ticking next frame.
    pub running: bool,
    /// Repaint exactly this logical rect instead of the widget's whole bounds.
    /// Used by the scroll-blit fast path: the bulk was shifted by
    /// [`scroll_blit`](Widget::scroll_blit), so only this strip needs redrawing.
    pub damage: Option<Rect>,
}

impl Anim {
    /// Nothing animated this frame.
    pub const IDLE: Anim = Anim {
        repaint: false,
        relayout: false,
        running: false,
        damage: None,
    };

    /// A frame that repainted (whole bounds) and wants to continue.
    pub fn repaint() -> Anim {
        Anim {
            repaint: true,
            running: true,
            ..Anim::IDLE
        }
    }

    /// A frame that changed geometry and wants to continue.
    pub fn relayout() -> Anim {
        Anim {
            repaint: true,
            relayout: true,
            running: true,
            damage: None,
        }
    }
}

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
///
/// # Implementing a custom widget
///
/// This trait is the entire extension story: the built-in widgets get no
/// privileged API, so a downstream type that implements `Widget` drops into the
/// tree via [`Ui::add_child`](crate::Ui::add_child) exactly like a
/// [`Button`](crate::widgets::Button) or [`Label`](crate::widgets::Label). A leaf
/// with an intrinsic size needs only `layout_style`, `measure`,
/// `paint`, and `as_any_mut`:
///
/// ```
/// use std::any::Any;
///
/// use fbui_widgets::widget::{AvailableSize, KnownDims};
/// use fbui_widgets::{PaintCtx, Style, Theme, Widget};
/// use fbui_widgets::fbui_render::geom::Size;
/// use fbui_widgets::fbui_render::{Color, FontContext};
///
/// /// A fixed-size square of one solid color.
/// struct Swatch {
///     color: Color,
///     side: f32,
/// }
///
/// impl<Msg: 'static> Widget<Msg> for Swatch {
///     fn layout_style(&self, _theme: &Theme) -> Style {
///         Style::default()
///     }
///
///     // Report an intrinsic size so layout reserves a square for us.
///     fn measure(
///         &mut self,
///         _fonts: &mut FontContext,
///         _theme: &Theme,
///         _known: KnownDims,
///         _available: AvailableSize,
///     ) -> Option<Size> {
///         Some(Size::new(self.side, self.side))
///     }
///
///     fn paint(&self, ctx: &mut PaintCtx) {
///         let bounds = ctx.bounds();
///         ctx.painter().fill_rect(bounds, self.color);
///     }
///
///     fn as_any_mut(&mut self) -> &mut dyn Any {
///         self
///     }
/// }
/// ```
///
/// For interactive and animated widgets, override [`event`](Widget::event)
/// (emit messages and request repaints through the [`EventCtx`]),
/// [`animate`](Widget::animate) (advance a frame-clock [`Tween`](crate::Tween)),
/// and [`focusable`](Widget::focusable). The `custom_widget` example in the
/// `fbui` crate walks through all of these end to end.
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

    /// Advance any time-based animation by `dt` seconds, returning what changed
    /// and whether to keep ticking (see [`Anim`]). Called by
    /// [`Ui::animate`](crate::Ui::animate) on the frame clock. Default: nothing
    /// animates. Kinetic scrolling lives here.
    fn animate(&mut self, _dt: f32) -> Anim {
        Anim::IDLE
    }

    /// Like [`animate`](Self::animate), but able to emit application messages —
    /// for the rare time-driven behavior that must speak to the app, such as the
    /// on-screen [`Keyboard`](crate::widgets::Keyboard)'s key auto-repeat. This
    /// is what the [`Ui`](crate::Ui) actually calls each tick; the default
    /// forwards to `animate`, so a widget overrides exactly one of the two.
    fn animate_with(&mut self, ctx: &mut AnimCtx<'_, Msg>) -> Anim {
        self.animate(ctx.dt())
    }

    /// A pending vertical scroll-blit (logical px) to apply before repaint,
    /// consuming it. The [`Ui`](crate::Ui) shifts the widget's existing pixels by
    /// this much via [`Surface::scroll_region`](fbui_render::Surface::scroll_region)
    /// — reusing them instead of re-rasterizing — and the widget then repaints only
    /// the newly-exposed strip. Positive = content moves down. Default: none.
    fn scroll_blit(&mut self) -> Option<f32> {
        None
    }

    /// Whether this widget accepts keyboard focus (tab order, key events).
    fn focusable(&self) -> bool {
        false
    }

    /// Whether this widget is a **focus trap**: while focus is inside its
    /// subtree, Tab/Shift-Tab cycle only within it. This is what a modal
    /// [`Dialog`](crate::widgets::Dialog) reports so the page underneath drops
    /// out of the tab order. Default: no trap.
    fn traps_focus(&self) -> bool {
        false
    }

    /// A floating **overlay** this widget wants painted on top of the whole
    /// tree (an open dropdown menu, a toast), or `None`. `bounds` is the
    /// widget's own laid-out box and `surface` the full logical surface, so the
    /// overlay can be placed near the widget yet clamped on-screen. The
    /// [`Ui`](crate::Ui) paints all overlays after the normal tree walk (in
    /// tree order) via [`paint_overlay`](Widget::paint_overlay), and treats the
    /// rect as damage whenever the widget changes.
    ///
    /// By itself an overlay is paint-only (a toast). To make it
    /// **interactive** — pointer events inside the rect routed to this
    /// widget, outside clicks dismissing it — register it as a popup with
    /// [`Ui::open_popup`](crate::Ui::open_popup) /
    /// [`EventCtx::open_popup`](crate::EventCtx::open_popup). Default: none.
    fn overlay_rect(&self, _bounds: Rect, _surface: Size) -> Option<Rect> {
        None
    }

    /// Paint the overlay reported by [`overlay_rect`](Widget::overlay_rect).
    /// `ctx.bounds()` is the overlay rect. Default: nothing.
    fn paint_overlay(&self, _ctx: &mut PaintCtx) {}

    /// Prepare whatever [`overlay_rect`](Widget::overlay_rect) /
    /// [`paint_overlay`](Widget::paint_overlay) need — typically measuring
    /// text so the overlay can size itself. Called by the
    /// [`Ui`](crate::Ui) when this widget's popup opens and again on surface
    /// resize; `overlay_rect` itself never gets font access. Default: nothing.
    fn prepare_overlay(&mut self, _fonts: &mut FontContext, _theme: &Theme, _surface: Size) {}

    /// Whether this widget clips its children to its bounds (scroll viewports).
    fn clips(&self) -> bool {
        false
    }

    /// Whether this widget *overlays* its children: each child is positioned to
    /// fill this widget's box, so children overlap and z-order by insertion (the
    /// last child added paints on top and is hit-tested first). This is what a
    /// [`Stack`](crate::widgets::Stack) reports; the [`Ui`](crate::Ui) reads it
    /// when resolving each child's layout style. Default: children flow per their
    /// own layout style.
    fn stacks_children(&self) -> bool {
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
