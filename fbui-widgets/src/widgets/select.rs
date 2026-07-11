//! [`Select`] — a dropdown: a closed field showing the current option, and an
//! open menu floating above everything else.
//!
//! The menu is a **floating overlay** ([`Widget::overlay_rect`]): it isn't part
//! of the layout flow, so it can extend over whatever sits below the field
//! (and flips above it when there's no room). While open it is registered as
//! a **popup** ([`Ui::open_popup`](crate::Ui::open_popup)): the `Ui` routes
//! pointer events inside the menu here, dismisses on click-away (consumed),
//! and swallows outside scrolls — the widget only hit-tests its own rows.

use std::any::Any;

use fbui_render::geom::{Point, Rect, Size};
use fbui_render::{FontContext, Painter, PathBuilder};

use crate::ctx::{EventCtx, PaintCtx};
use crate::event::{Event, Key, PointerButton};
use crate::popup::{place_anchored, AnchorSpec, Placement};
use crate::style::Style;
use crate::theme::Theme;
use crate::tree::PopupOptions;
use crate::util::{focus_ring, text_style, union};
use crate::widget::{AvailableSize, KnownDims, Widget};

/// Height of one menu row, logical px.
const ROW_H: f32 = 32.0;
/// Inner padding of the menu box.
const MENU_PAD: f32 = 4.0;
/// Vertical gap between the field and the menu.
const GAP: f32 = 2.0;
/// Field text padding.
const PAD_X: f32 = 12.0;
const PAD_Y: f32 = 8.0;
/// Space reserved for the chevron on the right of the field.
const CHEVRON_W: f32 = 24.0;

/// A single-choice dropdown. Emits `on_change(index)` when an option is
/// committed (click, or Enter on the keyboard-hovered row).
pub struct Select<Msg> {
    options: Vec<String>,
    selected: usize,
    open: bool,
    /// Row the pointer / arrow keys are on while open.
    hover: Option<usize>,
    on_change: Option<Box<dyn Fn(usize) -> Msg>>,
}

impl<Msg> Select<Msg> {
    pub fn new<I, S>(options: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Select {
            options: options.into_iter().map(Into::into).collect(),
            selected: 0,
            open: false,
            hover: None,
            on_change: None,
        }
    }

    /// Set the initially selected option index (clamped).
    pub fn selected(mut self, idx: usize) -> Self {
        self.selected = idx.min(self.options.len().saturating_sub(1));
        self
    }

    /// Message factory invoked when a different option is committed.
    pub fn on_change(mut self, f: impl Fn(usize) -> Msg + 'static) -> Self {
        self.on_change = Some(Box::new(f));
        self
    }

    /// The currently selected option index.
    pub fn selected_index(&self) -> usize {
        self.selected
    }

    /// Whether the menu is currently open.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Replace the options (call via [`Ui::with`](crate::Ui::with)); the
    /// selection is clamped and the menu closed.
    pub fn set_options(&mut self, options: Vec<String>) {
        self.options = options;
        self.selected = self.selected.min(self.options.len().saturating_sub(1));
        self.open = false;
        self.hover = None;
    }

    /// The floating menu rect for a field at `b`: below the field, flipped
    /// above when there's more room there, clamped to the surface.
    fn menu_rect(&self, b: Rect, surface: Size) -> Rect {
        let h = self.options.len() as f32 * ROW_H + 2.0 * MENU_PAD;
        place_anchored(
            b,
            Size::new(b.w, h),
            surface,
            AnchorSpec::new(Placement::Below).gap(GAP),
        )
    }

    /// The menu row index at `pos`, if `pos` is inside the menu.
    fn row_at(&self, pos: Point, menu: Rect) -> Option<usize> {
        if !menu.contains_point(pos) {
            return None;
        }
        let i = ((pos.y - menu.y - MENU_PAD) / ROW_H).floor();
        if i < 0.0 {
            return None;
        }
        let i = i as usize;
        (i < self.options.len()).then_some(i)
    }

    /// Damage both the field and the menu area (padded a pixel for the border
    /// stroke's anti-aliased halo just outside the rects).
    fn damage_all(&self, ctx: &mut EventCtx<Msg>) {
        let b = ctx.bounds();
        let menu = self.menu_rect(b, ctx.surface_size());
        ctx.request_paint_rect(union(b, menu).inset(-1.0));
    }

    fn open_menu(&mut self, ctx: &mut EventCtx<Msg>) {
        if self.options.is_empty() {
            return;
        }
        self.open = true;
        self.hover = Some(self.selected);
        // The field is the focus target itself, so no focus grab; the Ui's
        // click-away dismissal replaces the old pointer-capture routing.
        ctx.open_popup(PopupOptions {
            dismiss_on_outside_click: true,
            grab_focus: false,
        });
        ctx.request_focus();
        self.damage_all(ctx);
        ctx.set_handled();
    }

    fn close_menu(&mut self, ctx: &mut EventCtx<Msg>) {
        self.open = false;
        self.hover = None;
        ctx.close_popup();
        self.damage_all(ctx);
        ctx.set_handled();
    }

    fn commit(&mut self, idx: usize, ctx: &mut EventCtx<Msg>) {
        if idx < self.options.len() && idx != self.selected {
            self.selected = idx;
            if let Some(f) = &self.on_change {
                ctx.emit(f(idx));
            }
        }
        self.close_menu(ctx);
    }
}

impl<Msg: 'static> Widget<Msg> for Select<Msg> {
    fn layout_style(&self, _theme: &Theme) -> Style {
        Style::default()
    }

    fn measure(
        &mut self,
        fonts: &mut FontContext,
        theme: &Theme,
        _known: KnownDims,
        _available: AvailableSize,
    ) -> Option<Size> {
        // Wide enough for the widest option, so committing never resizes.
        let st = text_style(theme, theme.metrics.font_size, theme.palette.text);
        let mut w: f32 = 0.0;
        let mut h = theme.metrics.font_size;
        for opt in &self.options {
            let s = fonts.layout(opt, &st, None).size();
            w = w.max(s.w);
            h = h.max(s.h);
        }
        Some(Size::new(w + 2.0 * PAD_X + CHEVRON_W, h + 2.0 * PAD_Y))
    }

    fn focusable(&self) -> bool {
        true
    }

    fn overlay_rect(&self, bounds: Rect, surface: Size) -> Option<Rect> {
        self.open.then(|| self.menu_rect(bounds, surface))
    }

    fn paint(&self, ctx: &mut PaintCtx) {
        let b = ctx.bounds();
        let focused = ctx.is_focused();
        let theme = ctx.theme();
        let radius = theme.metrics.radius;
        let fw = theme.metrics.focus_width;
        let (surface, line, text, accent) = (
            theme.palette.surface,
            theme.palette.line,
            theme.palette.text,
            theme.palette.accent,
        );
        let st = text_style(theme, theme.metrics.font_size, text);
        let label = self.options.get(self.selected).cloned().unwrap_or_default();

        let (p, fonts) = ctx.painter_and_fonts();
        p.fill_rounded_rect(b, radius, surface);
        p.stroke_rounded_rect(b, radius, line, 1.0);
        let ts = fonts.layout(&label, &st, None).size();
        let ty = b.y + (b.h - ts.h) / 2.0;
        fonts.draw_text(p, &label, &st, Point::new(b.x + PAD_X, ty), None);
        chevron(p, b, if self.open { accent } else { line });
        if focused {
            focus_ring(p, b, radius, accent, fw);
        }
    }

    fn paint_overlay(&self, ctx: &mut PaintCtx) {
        let menu = ctx.bounds();
        let theme = ctx.theme();
        let radius = theme.metrics.radius;
        let (bg, line, text, accent, on_accent) = (
            theme.palette.surface_alt,
            theme.palette.line,
            theme.palette.text,
            theme.palette.accent,
            theme.palette.on_accent,
        );
        let st_normal = text_style(theme, theme.metrics.font_size, text);
        let st_hover = text_style(theme, theme.metrics.font_size, on_accent);
        let st_selected = text_style(theme, theme.metrics.font_size, accent);
        let font_size = theme.metrics.font_size;
        let (hover, selected) = (self.hover, self.selected);
        let options = &self.options;

        let (p, fonts) = ctx.painter_and_fonts();
        p.fill_rounded_rect(menu, radius, bg);
        p.stroke_rounded_rect(menu, radius, line, 1.0);
        p.push_clip(menu);
        for (i, opt) in options.iter().enumerate() {
            let ry = menu.y + MENU_PAD + i as f32 * ROW_H;
            let row = Rect::new(menu.x + MENU_PAD, ry, menu.w - 2.0 * MENU_PAD, ROW_H);
            let st = if hover == Some(i) {
                p.fill_rounded_rect(row, radius * 0.6, accent);
                &st_hover
            } else if selected == i {
                &st_selected
            } else {
                &st_normal
            };
            fonts.draw_text(
                p,
                opt,
                st,
                Point::new(
                    row.x + PAD_X - MENU_PAD,
                    ry + (ROW_H - font_size) / 2.0 - 1.0,
                ),
                None,
            );
        }
        p.pop_clip();
    }

    fn event(&mut self, ctx: &mut EventCtx<Msg>) {
        let b = ctx.bounds();
        let menu = self.menu_rect(b, ctx.surface_size());
        let ev = ctx.event().clone();
        match ev {
            Event::PointerDown {
                button: PointerButton::Left,
                pos,
            } => {
                if !self.open {
                    if b.contains_point(pos) {
                        self.open_menu(ctx);
                    }
                } else if let Some(i) = self.row_at(pos, menu) {
                    // Press highlights; commit happens on release, like a menu.
                    if self.hover != Some(i) {
                        self.hover = Some(i);
                        ctx.request_paint_rect(menu);
                    }
                    ctx.set_handled();
                } else {
                    // Inside the menu box but off every row (the padding ring).
                    // Clicks anywhere else never reach here: the Ui dismisses
                    // the popup and consumes them.
                    self.close_menu(ctx);
                }
            }
            Event::PointerMove { pos } => {
                if self.open {
                    let row = self.row_at(pos, menu);
                    if row.is_some() && row != self.hover {
                        self.hover = row;
                        ctx.request_paint_rect(menu);
                    }
                }
            }
            Event::PointerUp {
                button: PointerButton::Left,
                pos,
            } => {
                if self.open {
                    if let Some(i) = self.row_at(pos, menu) {
                        self.commit(i, ctx);
                    }
                    // Release on the field right after opening keeps it open.
                    ctx.set_handled();
                }
            }
            Event::PopupDismissed => {
                // Click-away (or a popup stacking over us): the Ui already
                // removed the popup entry and damaged the menu; sync state and
                // repaint the field (chevron highlight).
                self.open = false;
                self.hover = None;
                ctx.request_paint();
            }
            Event::Key {
                key, pressed: true, ..
            } if ctx.is_focused() => match key {
                Key::Enter | Key::Space => {
                    if self.open {
                        if let Some(i) = self.hover {
                            self.commit(i, ctx);
                        } else {
                            self.close_menu(ctx);
                        }
                    } else {
                        self.open_menu(ctx);
                    }
                }
                Key::Down | Key::Up => {
                    if self.open {
                        let n = self.options.len();
                        if n > 0 {
                            let cur = self.hover.unwrap_or(self.selected);
                            let next = if key == Key::Down {
                                (cur + 1).min(n - 1)
                            } else {
                                cur.saturating_sub(1)
                            };
                            self.hover = Some(next);
                            ctx.request_paint_rect(menu);
                        }
                        ctx.set_handled();
                    } else {
                        self.open_menu(ctx);
                    }
                }
                Key::Escape => {
                    if self.open {
                        self.close_menu(ctx);
                    }
                    // Closed: leave Esc unhandled so it bubbles (e.g. to a
                    // Dialog hosting this select).
                }
                _ => {}
            },
            _ => {}
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// Small downward chevron at the right edge of the field.
fn chevron(p: &mut Painter, b: Rect, color: fbui_render::Color) {
    let cx = b.right() - CHEVRON_W / 2.0 - 4.0;
    let cy = b.y + b.h / 2.0;
    let (w, h) = (8.0, 5.0);
    let mut pb = PathBuilder::new();
    pb.move_to(cx - w / 2.0, cy - h / 2.0)
        .line_to(cx + w / 2.0, cy - h / 2.0)
        .line_to(cx, cy + h / 2.0)
        .close();
    if let Some(path) = pb.finish() {
        p.fill_path(&path, color);
    }
}
