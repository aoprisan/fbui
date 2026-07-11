//! [`ContextMenu`] — a transparent wrapper that opens a menu at the pointer.
//!
//! Host it like a [`Container`](crate::widgets::Container): its children lay
//! out and paint normally. A **right-click** or **long-press** (touch)
//! anywhere inside its bounds — including on interactive children, since
//! those events bubble — opens the menu at the pointer, registered as a
//! popup so the [`Ui`](crate::Ui) routes menu events, dismisses on
//! click-away, and confines Tab. The menu engine (rows, separators,
//! disabled items, keyboard navigation, painting) is shared with
//! [`Menu`](crate::widgets::Menu).
//!
//! ```ignore
//! let cm = ui.add_child(
//!     root,
//!     ContextMenu::new(["Rename", "Duplicate", "Delete"]).on_select(Msg::RowAction),
//! );
//! ui.add_child(cm, Label::new("right-click or long-press me"));
//! ```

use std::any::Any;

use fbui_render::geom::{Point, Rect, Size};
use fbui_render::FontContext;

use crate::ctx::{EventCtx, PaintCtx};
use crate::event::{Event, PointerButton};
use crate::popup::{place_anchored, AnchorSpec, Placement};
use crate::style::{self, Style};
use crate::theme::Theme;
use crate::tree::PopupOptions;
use crate::widget::Widget;

use super::menu::{MenuAction, MenuCore, MenuItem};

/// A context-menu region (see the module docs).
pub struct ContextMenu<Msg> {
    core: MenuCore<Msg>,
    /// Pointer position the open menu is anchored to.
    open_at: Option<Point>,
    on_close: Option<Box<dyn Fn() -> Msg>>,
    fill: bool,
}

impl<Msg> ContextMenu<Msg> {
    /// A region whose menu holds one enabled item per label.
    pub fn new<I, S>(items: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        ContextMenu {
            core: MenuCore::new(items.into_iter().map(MenuItem::item).collect()),
            open_at: None,
            on_close: None,
            fill: false,
        }
    }

    /// Append a separator after the items added so far.
    pub fn separator(mut self) -> Self {
        self.core.push(MenuItem::Separator);
        self
    }

    /// Append one more item.
    pub fn item(mut self, label: impl Into<String>) -> Self {
        self.core.push(MenuItem::item(label));
        self
    }

    /// Disable the entry at `index` (indexes count separators too).
    pub fn disable(mut self, index: usize) -> Self {
        self.core.disable(index);
        self
    }

    /// Message factory fired when an item is selected; `index` is the entry's
    /// position (separators included, but never selected).
    pub fn on_select(mut self, f: impl Fn(usize) -> Msg + 'static) -> Self {
        self.core.set_on_activate(f);
        self
    }

    /// Message emitted when the menu closes without selecting — Esc or
    /// click-away.
    pub fn on_close(mut self, f: impl Fn() -> Msg + 'static) -> Self {
        self.on_close = Some(Box::new(f));
        self
    }

    /// Fill the parent (both axes) instead of wrapping the children.
    pub fn fill(mut self) -> Self {
        self.fill = true;
        self
    }

    // ---- runtime control ---------------------------------------------------

    /// Arm the menu at `pos` programmatically (then call
    /// [`Ui::open_popup`](crate::Ui::open_popup)); the gesture path does this
    /// itself.
    pub fn open_at(&mut self, pos: Point) {
        self.open_at = Some(pos);
        self.core.hover = None;
    }

    pub fn is_open(&self) -> bool {
        self.open_at.is_some()
    }

    /// The keyboard/pointer-hovered entry, for tests.
    pub fn hovered(&self) -> Option<usize> {
        self.core.hover
    }

    /// The rect of entry `i` within a menu box at `menu` — geometry test hook.
    pub fn row_rect(&self, menu: Rect, i: usize) -> Option<Rect> {
        self.core.row_rect(menu, i)
    }

    fn close(&mut self) {
        self.open_at = None;
        self.core.hover = None;
    }
}

impl<Msg: 'static> Widget<Msg> for ContextMenu<Msg> {
    fn layout_style(&self, _theme: &Theme) -> Style {
        // A pass-through box: children flow per their own styles.
        if self.fill {
            Style {
                size: taffy::Size {
                    width: style::percent(1.0),
                    height: style::percent(1.0),
                },
                ..Style::default()
            }
        } else {
            Style::default()
        }
    }

    fn paint(&self, _ctx: &mut PaintCtx) {}

    fn prepare_overlay(&mut self, fonts: &mut FontContext, theme: &Theme, _surface: Size) {
        self.core.prepare(fonts, theme);
    }

    fn overlay_rect(&self, _bounds: Rect, surface: Size) -> Option<Rect> {
        let p = self.open_at?;
        Some(place_anchored(
            Rect::new(p.x, p.y, 0.0, 0.0),
            self.core.size(),
            surface,
            AnchorSpec::new(Placement::Below).gap(0.0),
        ))
    }

    fn paint_overlay(&self, ctx: &mut PaintCtx) {
        let menu = ctx.bounds();
        self.core.paint(ctx, menu);
    }

    fn event(&mut self, ctx: &mut EventCtx<Msg>) {
        if self.open_at.is_none() {
            // Closed: watch for the opening gesture inside our bounds. These
            // bubble up from children, so a right-click on a nested button
            // still lands here.
            let open_pos = match *ctx.event() {
                Event::PointerDown {
                    button: PointerButton::Right,
                    pos,
                } => Some(pos),
                Event::LongPress { pos } => Some(pos),
                _ => None,
            };
            if let Some(pos) = open_pos.filter(|p| ctx.bounds().contains_point(*p)) {
                self.open_at = Some(pos);
                self.core.hover = None;
                ctx.open_popup(PopupOptions::default());
                ctx.set_handled();
            }
            return;
        }
        let Some(menu) = self.overlay_rect(ctx.bounds(), ctx.surface_size()) else {
            return;
        };
        match self.core.handle(ctx, menu) {
            MenuAction::Activate(i) => {
                self.core.emit_activate(i, ctx);
                self.close();
                ctx.close_popup();
                ctx.set_handled();
            }
            MenuAction::Close => {
                self.close();
                ctx.close_popup();
                if let Some(f) = &self.on_close {
                    ctx.emit(f());
                }
                ctx.set_handled();
            }
            MenuAction::Dismissed => {
                self.close();
                if let Some(f) = &self.on_close {
                    ctx.emit(f());
                }
            }
            MenuAction::None => {}
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
