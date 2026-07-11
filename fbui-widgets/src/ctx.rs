//! The contexts handed to widgets during event handling and painting.
//!
//! Widgets never touch the [`Ui`](crate::Ui) directly — that would alias the very
//! tree the walk is borrowing. Instead a widget receives a context that exposes
//! *its own* bounds, the theme/fonts, and a set of **request** sinks: emit a
//! message, mark damage, ask for relayout/focus/pointer-capture. The Ui applies
//! those requests after the widget returns. This keeps the borrow graph simple
//! and the data-flow one-directional (see `DESIGN.md` §3).

use fbui_render::geom::{Point, Rect, Size};
use fbui_render::{FontContext, Painter};

use crate::event::Event;
use crate::theme::Theme;
use crate::tree::{PopupOptions, WidgetId};

/// A focus-movement request raised by a widget during event handling.
#[derive(Debug, Clone, Copy)]
pub(crate) enum FocusOp {
    /// Give focus to a specific widget (usually the one handling the event).
    Request(WidgetId),
    /// Move to the next / previous focusable widget in tab order.
    Next,
    Prev,
    /// Drop focus entirely.
    Clear,
}

/// A pointer-capture request.
#[derive(Debug, Clone, Copy)]
pub(crate) enum CaptureOp {
    Set(WidgetId),
    Clear,
}

/// A popup open/close request raised during event handling.
#[derive(Debug, Clone, Copy)]
pub(crate) enum PopupOp {
    Open(WidgetId, PopupOptions),
    Close(WidgetId),
}

/// The mutable side-effects a widget can request in one event pass. Owned by the
/// [`Ui`](crate::Ui) and lent to each [`EventCtx`] as a single `&mut`.
pub(crate) struct Outputs<Msg> {
    pub messages: Vec<Msg>,
    pub damage: Vec<Rect>,
    pub relayout: bool,
    /// A relayout whose pixel changes the widget fully accounts for itself
    /// (scroll-blit), so no implicit full-surface damage is added.
    pub scroll_layout: bool,
    pub focus: Option<FocusOp>,
    pub capture: Option<CaptureOp>,
    pub popup: Option<PopupOp>,
    pub handled: bool,
    /// A widget started a time-based animation; the Ui should keep ticking
    /// [`animate`](crate::Ui::animate) until it settles.
    pub animate: bool,
}

impl<Msg> Default for Outputs<Msg> {
    fn default() -> Self {
        Outputs {
            messages: Vec::new(),
            damage: Vec::new(),
            relayout: false,
            scroll_layout: false,
            focus: None,
            capture: None,
            popup: None,
            handled: false,
            animate: false,
        }
    }
}

impl<Msg> Outputs<Msg> {
    pub fn reset_for_event(&mut self) {
        self.focus = None;
        self.capture = None;
        self.popup = None;
        self.handled = false;
        self.animate = false;
    }
}

/// Context for [`Widget::event`](crate::Widget::event).
pub struct EventCtx<'a, Msg> {
    pub(crate) event: &'a Event,
    pub(crate) bounds: Rect,
    pub(crate) surface: Size,
    pub(crate) theme: &'a Theme,
    pub(crate) fonts: &'a mut FontContext,
    pub(crate) hovered: bool,
    pub(crate) focused: bool,
    pub(crate) self_id: WidgetId,
    pub(crate) out: &'a mut Outputs<Msg>,
}

impl<'a, Msg> EventCtx<'a, Msg> {
    /// The event being handled.
    pub fn event(&self) -> &Event {
        self.event
    }

    /// This widget's absolute logical bounds.
    pub fn bounds(&self) -> Rect {
        self.bounds
    }

    /// The full logical surface size — for widgets that place a floating
    /// overlay ([`Widget::overlay_rect`](crate::Widget::overlay_rect)) and need
    /// to damage or clamp it during event handling.
    pub fn surface_size(&self) -> Size {
        self.surface
    }

    pub fn theme(&self) -> &Theme {
        self.theme
    }

    /// Font context, for hit-testing against shaped text (e.g. caret placement).
    pub fn fonts(&mut self) -> &mut FontContext {
        self.fonts
    }

    /// Whether the pointer is currently over this widget.
    pub fn is_hovered(&self) -> bool {
        self.hovered
    }

    /// Whether this widget holds keyboard focus.
    pub fn is_focused(&self) -> bool {
        self.focused
    }

    /// Emit an application message.
    pub fn emit(&mut self, msg: Msg) {
        self.out.messages.push(msg);
    }

    /// Mark this widget's whole bounds as needing repaint.
    pub fn request_paint(&mut self) {
        let b = self.bounds;
        self.out.damage.push(b);
    }

    /// Mark a specific logical rectangle as needing repaint.
    pub fn request_paint_rect(&mut self, rect: Rect) {
        self.out.damage.push(rect);
    }

    /// Ask for a layout recompute (geometry may have changed).
    pub fn request_layout(&mut self) {
        self.out.relayout = true;
    }

    /// Request a relayout **without** the implicit full-surface repaint that
    /// [`request_layout`](Self::request_layout) carries — for the scroll-blit
    /// fast path, where the widget accounts for every changed pixel itself
    /// (the shifted region, the exposed strip, and explicitly damaged rects
    /// like a moved scrollbar thumb). Only valid when the layout change is
    /// confined to re-placing this widget's own children (a scroll offset):
    /// nothing outside its bounds may move.
    pub fn request_scroll_layout(&mut self) {
        self.out.scroll_layout = true;
    }

    /// Take keyboard focus.
    pub fn request_focus(&mut self) {
        self.out.focus = Some(FocusOp::Request(self.self_id));
    }

    /// Move focus to the next focusable widget.
    pub fn focus_next(&mut self) {
        self.out.focus = Some(FocusOp::Next);
    }

    /// Move focus to the previous focusable widget.
    pub fn focus_prev(&mut self) {
        self.out.focus = Some(FocusOp::Prev);
    }

    /// Drop keyboard focus.
    pub fn clear_focus(&mut self) {
        self.out.focus = Some(FocusOp::Clear);
    }

    /// Capture the pointer: keep receiving motion/release even outside bounds
    /// (drags, slider thumbs).
    pub fn capture_pointer(&mut self) {
        self.out.capture = Some(CaptureOp::Set(self.self_id));
    }

    /// Release a previously captured pointer.
    pub fn release_pointer(&mut self) {
        self.out.capture = Some(CaptureOp::Clear);
    }

    /// Register this widget's floating overlay as an interactive **popup**
    /// (see [`Ui::open_popup`](crate::Ui::open_popup)): pointer events inside
    /// its [`overlay_rect`](crate::Widget::overlay_rect) route to this widget
    /// ahead of the tree, and outside clicks dismiss it (per `opts`),
    /// delivering [`Event::PopupDismissed`](crate::Event::PopupDismissed).
    pub fn open_popup(&mut self, opts: PopupOptions) {
        self.out.popup = Some(PopupOp::Open(self.self_id, opts));
    }

    /// Close this widget's popup (the reverse of
    /// [`open_popup`](Self::open_popup)). No `PopupDismissed` is delivered —
    /// the widget is closing itself and already knows. No-op if not open.
    pub fn close_popup(&mut self) {
        self.out.popup = Some(PopupOp::Close(self.self_id));
    }

    /// Stop this event propagating to widgets behind this one.
    pub fn set_handled(&mut self) {
        self.out.handled = true;
    }

    /// Signal that this widget began a time-based animation, so the Ui keeps
    /// calling [`animate`](crate::Widget::animate) on the frame clock until the
    /// animation settles. Pair it with starting a [`Tween`](crate::anim::Tween)
    /// or kinetic coast in the same handler.
    pub fn request_anim(&mut self) {
        self.out.animate = true;
    }

    /// The pointer position in this widget's local space (origin at its top-left),
    /// if the event carries one.
    pub fn local_pointer(&self) -> Option<Point> {
        self.event
            .pointer_pos()
            .map(|p| Point::new(p.x - self.bounds.x, p.y - self.bounds.y))
    }
}

/// Context for [`Widget::animate_with`](crate::Widget::animate_with): the frame
/// `dt` plus the message sink, for the rare animation that must speak to the
/// app (the on-screen [`Keyboard`](crate::widgets::Keyboard)'s key auto-repeat).
/// Most animations only change pixels and should use the plain
/// [`animate`](crate::Widget::animate) instead.
pub struct AnimCtx<'a, Msg> {
    pub(crate) dt: f32,
    pub(crate) messages: &'a mut Vec<Msg>,
}

impl<'a, Msg> AnimCtx<'a, Msg> {
    /// Seconds elapsed since the previous animation tick.
    pub fn dt(&self) -> f32 {
        self.dt
    }

    /// Emit an application message from this tick.
    pub fn emit(&mut self, msg: Msg) {
        self.messages.push(msg);
    }
}

/// Context for [`Widget::paint`](crate::Widget::paint).
pub struct PaintCtx<'a, 'p> {
    pub(crate) painter: &'a mut Painter<'p>,
    pub(crate) fonts: &'a mut FontContext,
    pub(crate) theme: &'a Theme,
    pub(crate) bounds: Rect,
    pub(crate) region: Rect,
    pub(crate) hovered: bool,
    pub(crate) focused: bool,
}

impl<'a, 'p> PaintCtx<'a, 'p> {
    /// The painter to draw with (logical coordinates).
    pub fn painter(&mut self) -> &mut Painter<'p> {
        self.painter
    }

    /// The font context, for measuring/drawing text.
    pub fn fonts(&mut self) -> &mut FontContext {
        self.fonts
    }

    /// Both the painter and fonts at once — convenient when drawing text, which
    /// needs the two together.
    pub fn painter_and_fonts(&mut self) -> (&mut Painter<'p>, &mut FontContext) {
        (self.painter, self.fonts)
    }

    pub fn theme(&self) -> &Theme {
        self.theme
    }

    /// This widget's absolute logical bounds.
    pub fn bounds(&self) -> Rect {
        self.bounds
    }

    /// The logical region currently being repainted (the active clip). A widget
    /// that paints many independent pieces — list rows, grid cells — can skip the
    /// ones that don't intersect this, turning a small damage rect into
    /// proportionally small work.
    pub fn region(&self) -> Rect {
        self.region
    }

    pub fn is_hovered(&self) -> bool {
        self.hovered
    }

    pub fn is_focused(&self) -> bool {
        self.focused
    }
}
