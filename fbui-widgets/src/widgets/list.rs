//! [`List`] — a windowed list that lays out and paints only visible rows.
//!
//! Rows are data, not child widgets, so a 10 000-row list costs the same to
//! paint as a 10-row one: only the rows intersecting the viewport are drawn. This
//! is the windowing the Phase 3 exit criteria call for. Phase 5 adds two
//! sharpenings: a paint only re-rasterizes the rows intersecting the damage
//! region, and a wheel/drag/kinetic scroll uses the **scroll-blit** fast path —
//! shifting the already-drawn rows in place and repainting just the strip that
//! scrolled into view.

use std::any::Any;

use fbui_render::geom::{Point, Rect};

use crate::ctx::{EventCtx, PaintCtx};
use crate::event::{Event, Key, PointerButton};
use crate::kinetic::Kinetic;
use crate::style::{self, Style};
use crate::theme::Theme;
use crate::util::text_style;
use crate::widget::{Anim, Widget};

const ROW_H: f32 = 40.0;

/// Movement (logical px) past which a press becomes a scroll-drag rather than a
/// row selection.
const DRAG_SLOP: f32 = 6.0;

/// In-progress pointer drag over the list.
struct Drag {
    /// Where the press began (to tell a tap from a drag).
    start_y: f32,
    /// Last seen y, for incremental scrolling.
    last_y: f32,
    /// Whether it has crossed the slop and become a scroll.
    moved: bool,
}

/// A scrollable, single-selection list of text rows.
pub struct List<Msg> {
    rows: Vec<String>,
    row_h: f32,
    offset: f32,
    selected: Option<usize>,
    on_select: Option<Box<dyn Fn(usize) -> Msg>>,
    /// Last bounds seen, so kinetic [`animate`](Widget::animate) can clamp and
    /// place the scrollbar without a layout context.
    bounds: Rect,
    drag: Option<Drag>,
    kinetic: Kinetic,
    /// Pending content shift (logical px) for the next [`scroll_blit`](Widget::scroll_blit).
    blit_dy: f32,
}

impl<Msg> List<Msg> {
    pub fn new(rows: Vec<String>) -> Self {
        List {
            rows,
            row_h: ROW_H,
            offset: 0.0,
            selected: None,
            on_select: None,
            bounds: Rect::new(0.0, 0.0, 0.0, 0.0),
            drag: None,
            kinetic: Kinetic::new(),
            blit_dy: 0.0,
        }
    }

    pub fn row_height(mut self, h: f32) -> Self {
        self.row_h = h;
        self
    }

    pub fn on_select(mut self, f: impl Fn(usize) -> Msg + 'static) -> Self {
        self.on_select = Some(Box::new(f));
        self
    }

    /// Replace the rows (call via [`Ui::with`](crate::Ui::with)).
    pub fn set_rows(&mut self, rows: Vec<String>) {
        self.rows = rows;
        self.selected = None;
        self.offset = 0.0;
        self.kinetic.stop();
        self.drag = None;
        self.blit_dy = 0.0;
    }

    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    fn total_h(&self) -> f32 {
        self.rows.len() as f32 * self.row_h
    }

    fn max_offset(&self, viewport_h: f32) -> f32 {
        (self.total_h() - viewport_h).max(0.0)
    }

    /// The scrollbar thumb rect at a given offset (padded for clean damage), or
    /// `None` when there's no overflow.
    fn thumb_rect(&self, offset: f32, b: Rect) -> Option<Rect> {
        let max_off = self.max_offset(b.h);
        if max_off <= 0.0 {
            return None;
        }
        let frac = (b.h / self.total_h()).clamp(0.0, 1.0);
        let thumb_h = (b.h * frac).max(24.0);
        let t = (offset / max_off).clamp(0.0, 1.0);
        let thumb_y = b.y + t * (b.h - thumb_h);
        // A hair wider/taller than the 4px bar so the moved thumb is fully covered.
        Some(Rect::new(
            b.right() - 7.0,
            thumb_y - 1.0,
            7.0,
            thumb_h + 2.0,
        ))
    }

    /// Scroll by `dy` offset-pixels using the blit fast path: move the offset,
    /// record the content shift for `scroll_blit`, and return the rect to damage
    /// (the moved thumb), or `None` if nothing moved.
    fn scroll_blit_by(&mut self, dy: f32, b: Rect) -> Option<Rect> {
        let old = self.offset;
        let new = (old + dy).clamp(0.0, self.max_offset(b.h));
        if (new - old).abs() <= f32::EPSILON {
            return None;
        }
        self.offset = new;
        // Content shifts opposite the offset change (offset up ⇒ content up).
        self.blit_dy += -(new - old);
        let old_thumb = self.thumb_rect(old, b);
        let new_thumb = self.thumb_rect(new, b);
        match (old_thumb, new_thumb) {
            (Some(a), Some(c)) => Some(union(a, c)),
            (a, c) => a.or(c),
        }
    }

    fn select(&mut self, idx: usize, ctx: &mut EventCtx<Msg>) {
        if idx >= self.rows.len() {
            return;
        }
        self.selected = Some(idx);
        if let Some(f) = &self.on_select {
            ctx.emit(f(idx));
        }
        // Keep the selection in view (a jump — full repaint, not a blit).
        let b = ctx.bounds();
        let top = idx as f32 * self.row_h;
        let bottom = top + self.row_h;
        if top < self.offset {
            self.offset = top;
        } else if bottom > self.offset + b.h {
            self.offset = bottom - b.h;
        }
        self.offset = self.offset.clamp(0.0, self.max_offset(b.h));
        ctx.request_paint();
    }
}

impl<Msg: 'static> Widget<Msg> for List<Msg> {
    fn layout_style(&self, _theme: &Theme) -> Style {
        Style {
            size: taffy::Size {
                width: style::percent(1.0),
                height: style::percent(1.0),
            },
            flex_grow: 1.0,
            ..Style::default()
        }
    }

    fn focusable(&self) -> bool {
        true
    }

    fn paint(&self, ctx: &mut PaintCtx) {
        let b = ctx.bounds();
        let region = ctx.region();
        let theme = ctx.theme();
        let (surface, accent, on_accent, line) = (
            theme.palette.surface,
            theme.palette.accent,
            theme.palette.on_accent,
            theme.palette.line,
        );
        // Two prebuilt styles (normal + on-selection) so we hold no theme borrow
        // once we start painting.
        let st_normal = text_style(theme, theme.metrics.font_size, theme.palette.text);
        let st_selected = text_style(theme, theme.metrics.font_size, on_accent);
        let font_size = theme.metrics.font_size;

        // Visible row window, further bounded to the rows touching the damage
        // region — so a strip repaint (scroll-blit) only rasterizes a few rows.
        let first = (self.offset / self.row_h).floor().max(0.0) as usize;
        let last = (((self.offset + b.h) / self.row_h).ceil() as usize).min(self.rows.len());
        let max_off = self.max_offset(b.h);
        let offset = self.offset;
        let row_h = self.row_h;
        let selected = self.selected;
        let rows = &self.rows;

        let (p, fonts) = ctx.painter_and_fonts();
        p.fill_rect(b, surface);
        p.push_clip(b);
        for (n, row) in rows[first..last].iter().enumerate() {
            let i = first + n;
            let ry = b.y + i as f32 * row_h - offset;
            // Skip rows outside the repaint region (the win on a strip repaint).
            if ry + row_h <= region.y || ry >= region.bottom() {
                continue;
            }
            let row_rect = Rect::new(b.x, ry, b.w, row_h);
            let row_style = if selected == Some(i) {
                p.fill_rect(row_rect, accent);
                &st_selected
            } else {
                if i > first {
                    p.fill_rect(Rect::new(b.x + 8.0, ry, b.w - 16.0, 1.0), line);
                }
                &st_normal
            };
            fonts.draw_text(
                p,
                row,
                row_style,
                Point::new(b.x + 12.0, ry + (row_h - font_size) / 2.0 - 1.0),
                None,
            );
        }
        // Scrollbar (cheap; always drawn, clipped to the region).
        if max_off > 0.0 {
            let frac = (b.h / self.total_h()).clamp(0.0, 1.0);
            let thumb_h = (b.h * frac).max(24.0);
            let t = offset / max_off;
            let thumb_y = b.y + t * (b.h - thumb_h);
            p.fill_rounded_rect(Rect::new(b.right() - 6.0, thumb_y, 4.0, thumb_h), 2.0, line);
        }
        p.pop_clip();
    }

    fn event(&mut self, ctx: &mut EventCtx<Msg>) {
        let b = ctx.bounds();
        self.bounds = b;
        let ev = ctx.event().clone();
        match ev {
            Event::Scroll { delta_y, .. } => {
                if let Some(dmg) = self.scroll_blit_by(delta_y, b) {
                    ctx.request_paint_rect(dmg);
                }
                ctx.set_handled();
            }
            Event::PointerDown {
                button: PointerButton::Left,
                pos,
            } => {
                // Start a press: any coast halts, and we decide tap-vs-scroll on
                // move/release so a drag scrolls instead of selecting.
                self.kinetic.stop();
                ctx.request_focus();
                self.drag = Some(Drag {
                    start_y: pos.y,
                    last_y: pos.y,
                    moved: false,
                });
                ctx.capture_pointer();
                ctx.set_handled();
            }
            Event::PointerMove { pos } => {
                if let Some(drag) = &mut self.drag {
                    let dy = drag.last_y - pos.y;
                    drag.last_y = pos.y;
                    if (pos.y - drag.start_y).abs() > DRAG_SLOP {
                        drag.moved = true;
                    }
                    if let Some(dmg) = self.scroll_blit_by(dy, b) {
                        ctx.request_paint_rect(dmg);
                    }
                }
            }
            Event::PointerUp {
                button: PointerButton::Left,
                pos,
            } => {
                if let Some(drag) = self.drag.take() {
                    ctx.release_pointer();
                    // A press that didn't wander is a tap: select the row under it.
                    if !drag.moved {
                        let idx = ((pos.y - b.y + self.offset) / self.row_h).floor() as i64;
                        if idx >= 0 {
                            self.select(idx as usize, ctx);
                        }
                    }
                }
            }
            Event::Fling { velocity_y, .. } => {
                if self.max_offset(b.h) > 0.0 {
                    self.kinetic.start(-velocity_y);
                    ctx.request_anim();
                    ctx.set_handled();
                }
            }
            Event::Key {
                key: Key::Down,
                pressed: true,
                ..
            } if ctx.is_focused() => {
                let next = self.selected.map(|s| s + 1).unwrap_or(0);
                self.select(next, ctx);
                ctx.set_handled();
            }
            Event::Key {
                key: Key::Up,
                pressed: true,
                ..
            } if ctx.is_focused() => {
                let prev = self.selected.unwrap_or(0).saturating_sub(1);
                self.select(prev, ctx);
                ctx.set_handled();
            }
            _ => {}
        }
    }

    fn animate(&mut self, dt: f32) -> Anim {
        if !self.kinetic.is_running() {
            return Anim::IDLE;
        }
        let dy = self.kinetic.step(dt);
        let b = self.bounds;
        match self.scroll_blit_by(dy, b) {
            Some(dmg) => Anim {
                repaint: false,
                relayout: false,
                running: self.kinetic.is_running(),
                damage: Some(dmg),
            },
            None => {
                // Hit a bound: stop coasting.
                self.kinetic.stop();
                Anim::IDLE
            }
        }
    }

    fn scroll_blit(&mut self) -> Option<f32> {
        if self.blit_dy.abs() < f32::EPSILON {
            None
        } else {
            Some(std::mem::take(&mut self.blit_dy))
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// Bounding box of two logical rects (empty is the identity).
fn union(a: Rect, b: Rect) -> Rect {
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
