//! [`Menu`] — a floating action menu on the popup layer.
//!
//! Like [`Toasts`], the widget itself is a zero-size *host* added anywhere in
//! the tree; the visible menu is its floating overlay, opened as an
//! interactive popup so the [`Ui`](crate::Ui) routes pointer events into it,
//! dismisses it on click-away, and confines Tab while it's open. Opening is a
//! two-step (mirroring [`Dialog`]'s add-then-focus pattern): arm the widget,
//! then register the popup —
//!
//! ```ignore
//! // In App::update, e.g. reacting to a button press:
//! ui.with::<Menu<Msg>, _>(menu, |m| m.open_below(anchor_bounds));
//! ui.open_popup(menu, PopupOptions::default());
//! ```
//!
//! Items activate on release (like [`Select`] rows) or Enter on the
//! keyboard-hovered item; `on_activate(index)` fires and the menu closes.
//! Disabled items and separators are skipped by both pointer and arrows.
//! Submenus are out of scope for v1.
//!
//! [`Dialog`]: crate::widgets::Dialog
//! [`Select`]: crate::widgets::Select
//! [`Toasts`]: crate::widgets::Toasts

use std::any::Any;

use fbui_render::geom::{Point, Rect, Size};
use fbui_render::FontContext;

use crate::ctx::{EventCtx, PaintCtx};
use crate::event::{Event, Key, PointerButton};
use crate::popup::{place_anchored, AnchorSpec, Placement};
use crate::style::{self, Style};
use crate::theme::Theme;
use crate::util::text_style;
use crate::widget::Widget;

/// Height of one item row, logical px (matches [`Select`](crate::widgets::Select)).
const ROW_H: f32 = 32.0;
/// Height of a separator row.
const SEP_H: f32 = 9.0;
/// Inner padding of the menu box.
const MENU_PAD: f32 = 4.0;
/// Horizontal text padding inside a row.
const PAD_X: f32 = 12.0;
/// Minimum menu width, so single-character menus stay usable.
const MIN_W: f32 = 96.0;

/// One entry of a menu.
pub enum MenuItem {
    /// A selectable action.
    Item {
        label: String,
        /// Shown dimmed; unreachable by pointer and arrows.
        disabled: bool,
    },
    /// A thin horizontal rule between item groups.
    Separator,
}

impl MenuItem {
    /// An enabled action item.
    pub fn item(label: impl Into<String>) -> Self {
        MenuItem::Item {
            label: label.into(),
            disabled: false,
        }
    }

    fn enabled(&self) -> bool {
        matches!(
            self,
            MenuItem::Item {
                disabled: false,
                ..
            }
        )
    }

    fn height(&self) -> f32 {
        match self {
            MenuItem::Item { .. } => ROW_H,
            MenuItem::Separator => SEP_H,
        }
    }
}

/// The shared engine behind [`Menu`] and
/// [`ContextMenu`](crate::widgets::ContextMenu): items, hover, measurement,
/// hit-testing, keyboard navigation, and painting of the floating menu box.
pub(crate) struct MenuCore<Msg> {
    items: Vec<MenuItem>,
    /// Item the pointer / arrow keys are on.
    pub(crate) hover: Option<usize>,
    /// Menu box size, cached by [`prepare`](Self::prepare) (needs fonts).
    size: Size,
    on_activate: Option<Box<dyn Fn(usize) -> Msg>>,
}

impl<Msg> MenuCore<Msg> {
    pub(crate) fn new(items: Vec<MenuItem>) -> Self {
        MenuCore {
            items,
            hover: None,
            size: Size::new(0.0, 0.0),
            on_activate: None,
        }
    }

    pub(crate) fn set_on_activate(&mut self, f: impl Fn(usize) -> Msg + 'static) {
        self.on_activate = Some(Box::new(f));
    }

    pub(crate) fn disable(&mut self, index: usize) {
        if let Some(MenuItem::Item { disabled, .. }) = self.items.get_mut(index) {
            *disabled = true;
        }
    }

    /// Measure the widest label and total height; caches [`size`](Self::size).
    pub(crate) fn prepare(&mut self, fonts: &mut FontContext, theme: &Theme) {
        let st = text_style(theme, theme.metrics.font_size, theme.palette.text);
        let mut w: f32 = MIN_W;
        let mut h: f32 = 2.0 * MENU_PAD;
        for item in &self.items {
            if let MenuItem::Item { label, .. } = item {
                w = w.max(fonts.layout(label, &st, None).size().w + 2.0 * PAD_X + 2.0 * MENU_PAD);
            }
            h += item.height();
        }
        self.size = Size::new(w, h);
    }

    pub(crate) fn size(&self) -> Size {
        self.size
    }

    /// The rect of entry `i` inside a menu box at `menu`.
    pub(crate) fn row_rect(&self, menu: Rect, i: usize) -> Option<Rect> {
        if i >= self.items.len() {
            return None;
        }
        let mut y = menu.y + MENU_PAD;
        for item in &self.items[..i] {
            y += item.height();
        }
        Some(Rect::new(
            menu.x + MENU_PAD,
            y,
            menu.w - 2.0 * MENU_PAD,
            self.items[i].height(),
        ))
    }

    /// The *enabled* item under `pos`, if any.
    pub(crate) fn row_at(&self, pos: Point, menu: Rect) -> Option<usize> {
        if !menu.contains_point(pos) {
            return None;
        }
        for i in 0..self.items.len() {
            if let Some(r) = self.row_rect(menu, i) {
                if r.contains_point(pos) {
                    return self.items[i].enabled().then_some(i);
                }
            }
        }
        None
    }

    /// The next enabled item after (`forward`) / before `from`, saturating at
    /// the ends. `None` starts from the respective end.
    pub(crate) fn next_enabled(&self, from: Option<usize>, forward: bool) -> Option<usize> {
        let n = self.items.len();
        let range: Box<dyn Iterator<Item = usize>> = match (from, forward) {
            (Some(i), true) => Box::new(i + 1..n),
            (Some(i), false) => Box::new((0..i).rev()),
            (None, true) => Box::new(0..n),
            (None, false) => Box::new((0..n).rev()),
        };
        for i in range {
            if self.items[i].enabled() {
                return Some(i);
            }
        }
        // Saturate: stay on `from` if it's already the last enabled one.
        from.filter(|&i| self.items[i].enabled())
    }

    pub(crate) fn first_enabled(&self) -> Option<usize> {
        self.next_enabled(None, true)
    }

    pub(crate) fn last_enabled(&self) -> Option<usize> {
        self.next_enabled(None, false)
    }

    /// Paint the menu box; `menu` is the overlay rect.
    pub(crate) fn paint(&self, ctx: &mut PaintCtx, menu: Rect) {
        let theme = ctx.theme();
        let radius = theme.metrics.radius;
        let (bg, line, text, muted, accent, on_accent) = (
            theme.palette.surface_alt,
            theme.palette.line,
            theme.palette.text,
            theme.palette.muted,
            theme.palette.accent,
            theme.palette.on_accent,
        );
        let st_normal = text_style(theme, theme.metrics.font_size, text);
        let st_hover = text_style(theme, theme.metrics.font_size, on_accent);
        let st_disabled = text_style(theme, theme.metrics.font_size, muted);
        let font_size = theme.metrics.font_size;

        // Collect row geometry before grabbing the painter (borrow discipline).
        struct Row<'a> {
            rect: Rect,
            label: Option<(&'a str, bool, bool)>, // (text, hovered, disabled)
        }
        let rows: Vec<Row> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, item)| {
                let rect = self.row_rect(menu, i)?;
                Some(match item {
                    MenuItem::Item { label, disabled } => Row {
                        rect,
                        label: Some((label.as_str(), self.hover == Some(i), *disabled)),
                    },
                    MenuItem::Separator => Row { rect, label: None },
                })
            })
            .collect();

        let (p, fonts) = ctx.painter_and_fonts();
        p.fill_rounded_rect(menu, radius, bg);
        p.stroke_rounded_rect(menu, radius, line, 1.0);
        p.push_clip(menu);
        for row in rows {
            match row.label {
                Some((label, hovered, disabled)) => {
                    let st = if hovered && !disabled {
                        p.fill_rounded_rect(row.rect, radius * 0.6, accent);
                        &st_hover
                    } else if disabled {
                        &st_disabled
                    } else {
                        &st_normal
                    };
                    let ty = row.rect.y + (row.rect.h - font_size) / 2.0 - 1.0;
                    fonts.draw_text(
                        p,
                        label,
                        st,
                        Point::new(row.rect.x + PAD_X - MENU_PAD, ty),
                        None,
                    );
                }
                None => {
                    let y = row.rect.y + row.rect.h / 2.0;
                    p.fill_rect(Rect::new(row.rect.x, y, row.rect.w, 1.0), line);
                }
            }
        }
        p.pop_clip();
    }
}

/// Where an open [`Menu`] is anchored.
enum MenuAnchor {
    /// At a point (a context-menu position): top-left at the point.
    At(Point),
    /// Below an anchor rect (a menu-button's bounds), flipping above when
    /// there's no room.
    Below(Rect),
}

/// A floating action menu (see the module docs for the open pattern).
pub struct Menu<Msg> {
    core: MenuCore<Msg>,
    anchor: Option<MenuAnchor>,
    on_close: Option<Box<dyn Fn() -> Msg>>,
}

impl<Msg> Menu<Msg> {
    /// A menu of enabled items, one per label.
    pub fn new<I, S>(items: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Menu {
            core: MenuCore::new(items.into_iter().map(MenuItem::item).collect()),
            anchor: None,
            on_close: None,
        }
    }

    /// Append a separator after the items added so far.
    pub fn separator(mut self) -> Self {
        self.core.items.push(MenuItem::Separator);
        self
    }

    /// Append one more item.
    pub fn item(mut self, label: impl Into<String>) -> Self {
        self.core.items.push(MenuItem::item(label));
        self
    }

    /// Disable the entry at `index` (indexes count separators too).
    pub fn disable(mut self, index: usize) -> Self {
        self.core.disable(index);
        self
    }

    /// Message factory fired when an item is activated; `index` is the
    /// entry's position (separators included, but never activated).
    pub fn on_activate(mut self, f: impl Fn(usize) -> Msg + 'static) -> Self {
        self.core.set_on_activate(f);
        self
    }

    /// Message emitted whenever the menu closes without activating — Esc,
    /// click-away, or a programmatic dismissal.
    pub fn on_close(mut self, f: impl Fn() -> Msg + 'static) -> Self {
        self.on_close = Some(Box::new(f));
        self
    }

    // ---- runtime control (call via `Ui::with`, then `Ui::open_popup`) ----

    /// Arm the menu to open with its top-left at `pos` (surface-logical).
    pub fn open_at(&mut self, pos: Point) {
        self.anchor = Some(MenuAnchor::At(pos));
        self.core.hover = None;
    }

    /// Arm the menu to open below `anchor` (flipping above at the bottom edge).
    pub fn open_below(&mut self, anchor: Rect) {
        self.anchor = Some(MenuAnchor::Below(anchor));
        self.core.hover = None;
    }

    /// Close the menu (the popup registration prunes on the next event).
    pub fn close(&mut self) {
        self.anchor = None;
        self.core.hover = None;
    }

    pub fn is_open(&self) -> bool {
        self.anchor.is_some()
    }

    /// The keyboard/pointer-hovered entry, for tests.
    pub fn hovered(&self) -> Option<usize> {
        self.core.hover
    }

    /// The rect of entry `i` within a menu box at `menu` — geometry test hook
    /// (the same the widget paints and hit-tests with).
    pub fn row_rect(&self, menu: Rect, i: usize) -> Option<Rect> {
        self.core.row_rect(menu, i)
    }

    fn activate(&mut self, i: usize, ctx: &mut EventCtx<Msg>) {
        if let Some(f) = &self.core.on_activate {
            ctx.emit(f(i));
        }
        self.anchor = None;
        self.core.hover = None;
        ctx.close_popup();
        ctx.set_handled();
    }

    fn close_via(&mut self, ctx: &mut EventCtx<Msg>) {
        self.anchor = None;
        self.core.hover = None;
        ctx.close_popup();
        if let Some(f) = &self.on_close {
            ctx.emit(f());
        }
        ctx.set_handled();
    }
}

impl<Msg: 'static> Widget<Msg> for Menu<Msg> {
    fn layout_style(&self, _theme: &Theme) -> Style {
        // Out of the flow entirely (the Toasts pattern): the visible menu
        // lives in the overlay, placed against the stored anchor.
        Style {
            position: taffy::Position::Absolute,
            size: taffy::Size {
                width: style::length(0.0),
                height: style::length(0.0),
            },
            ..Style::default()
        }
    }

    fn paint(&self, _ctx: &mut PaintCtx) {}

    fn prepare_overlay(&mut self, fonts: &mut FontContext, theme: &Theme, _surface: Size) {
        self.core.prepare(fonts, theme);
    }

    fn overlay_rect(&self, _bounds: Rect, surface: Size) -> Option<Rect> {
        let anchor = self.anchor.as_ref()?;
        let (rect, spec) = match anchor {
            MenuAnchor::At(p) => (
                Rect::new(p.x, p.y, 0.0, 0.0),
                AnchorSpec::new(Placement::Below).gap(0.0),
            ),
            MenuAnchor::Below(r) => (*r, AnchorSpec::new(Placement::Below)),
        };
        Some(place_anchored(rect, self.core.size(), surface, spec))
    }

    fn paint_overlay(&self, ctx: &mut PaintCtx) {
        let menu = ctx.bounds();
        self.core.paint(ctx, menu);
    }

    fn event(&mut self, ctx: &mut EventCtx<Msg>) {
        if self.anchor.is_none() {
            return;
        }
        let Some(menu) = self.overlay_rect(ctx.bounds(), ctx.surface_size()) else {
            return;
        };
        let ev = ctx.event().clone();
        match ev {
            // Pointer events arrive here only while inside the menu rect
            // (routed by the popup layer).
            Event::PointerMove { pos } => {
                let row = self.core.row_at(pos, menu);
                if row.is_some() && row != self.core.hover {
                    self.core.hover = row;
                    ctx.request_paint_rect(menu);
                }
            }
            Event::PointerDown {
                button: PointerButton::Left,
                pos,
            } => {
                let row = self.core.row_at(pos, menu);
                if row.is_some() && row != self.core.hover {
                    self.core.hover = row;
                    ctx.request_paint_rect(menu);
                }
                ctx.set_handled();
            }
            Event::PointerUp {
                button: PointerButton::Left,
                pos,
            } => {
                // Activate on release, like Select rows; a release on a
                // disabled item / separator / the padding keeps the menu open.
                if let Some(i) = self.core.row_at(pos, menu) {
                    self.activate(i, ctx);
                } else {
                    ctx.set_handled();
                }
            }
            Event::Key {
                key, pressed: true, ..
            } if ctx.is_focused() => match key {
                Key::Down => {
                    let next = self.core.next_enabled(self.core.hover, true);
                    if next != self.core.hover {
                        self.core.hover = next;
                        ctx.request_paint_rect(menu);
                    }
                    ctx.set_handled();
                }
                Key::Up => {
                    let next = self.core.next_enabled(self.core.hover, false);
                    if next != self.core.hover {
                        self.core.hover = next;
                        ctx.request_paint_rect(menu);
                    }
                    ctx.set_handled();
                }
                Key::Home => {
                    self.core.hover = self.core.first_enabled();
                    ctx.request_paint_rect(menu);
                    ctx.set_handled();
                }
                Key::End => {
                    self.core.hover = self.core.last_enabled();
                    ctx.request_paint_rect(menu);
                    ctx.set_handled();
                }
                Key::Enter | Key::Space => {
                    if let Some(i) = self.core.hover {
                        self.activate(i, ctx);
                    } else {
                        ctx.set_handled();
                    }
                }
                Key::Escape => self.close_via(ctx),
                _ => {}
            },
            Event::PopupDismissed => {
                self.anchor = None;
                self.core.hover = None;
                if let Some(f) = &self.on_close {
                    ctx.emit(f());
                }
            }
            _ => {}
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
