//! [`ScrollView`] — a clipping viewport that offsets its children vertically.
//!
//! The viewport clips and translates its subtree; the [`Ui`](crate::Ui) feeds it
//! its content vs. viewport extents after layout (via `set_scroll_metrics`), so it
//! can clamp scrolling without reaching into the tree. A [`Fling`](Event::Fling)
//! (Phase 4) starts a kinetic coast that decays in [`animate`](Widget::animate).
//!
//! Scrolling rides the **scroll-blit** fast path (Phase 5, extended here from
//! [`List`](super::List)): the already-drawn viewport pixels are shifted in
//! place via [`scroll_blit`](Widget::scroll_blit) and only the exposed strip is
//! damaged. Unlike `List` (whose rows are data), the children are real widgets,
//! so a scroll still requests a relayout to re-place them at the new offset —
//! the saving is in *paint*: only the children intersecting the strip
//! re-rasterize instead of the whole viewport.

use std::any::Any;

use fbui_render::geom::{Point, Rect, Size};

use crate::ctx::{EventCtx, PaintCtx};
use crate::event::{Event, PointerButton};
use crate::kinetic::Kinetic;
use crate::style::{self, Style};
use crate::theme::Theme;
use crate::util::union;
use crate::widget::{Anim, Widget};

/// A vertically scrolling container.
pub struct ScrollView {
    offset: f32,
    content_h: f32,
    viewport_h: f32,
    drag: Option<f32>,
    /// Momentum after a fling, in offset-pixels per second; 0 when at rest.
    kinetic: Kinetic,
    /// Last bounds seen, so kinetic [`animate`](Widget::animate) can compute
    /// thumb damage without a layout context.
    bounds: Rect,
    /// Pending content shift (logical px) for the next [`scroll_blit`](Widget::scroll_blit).
    blit_dy: f32,
}

impl ScrollView {
    pub fn new() -> Self {
        ScrollView {
            offset: 0.0,
            content_h: 0.0,
            viewport_h: 0.0,
            drag: None,
            kinetic: Kinetic::new(),
            bounds: Rect::new(0.0, 0.0, 0.0, 0.0),
            blit_dy: 0.0,
        }
    }

    fn max_offset(&self) -> f32 {
        (self.content_h - self.viewport_h).max(0.0)
    }

    /// The scrollbar thumb rect at a given offset (padded for clean damage), or
    /// `None` when there's no overflow.
    fn thumb_rect(&self, offset: f32, b: Rect) -> Option<Rect> {
        let max_off = self.max_offset();
        if max_off <= 0.0 {
            return None;
        }
        let frac_visible = (self.viewport_h / self.content_h).clamp(0.0, 1.0);
        let thumb_h = (b.h * frac_visible).max(24.0);
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
        let new = (old + dy).clamp(0.0, self.max_offset());
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

    fn scroll_by<Msg>(&mut self, dy: f32, ctx: &mut EventCtx<Msg>) {
        if let Some(dmg) = self.scroll_blit_by(dy, ctx.bounds()) {
            // Children must be re-placed at the new offset; the pixels
            // themselves are shifted by the blit, so only the thumb needs
            // explicit damage (the exposed strip is damaged when the `Ui`
            // applies the blit).
            ctx.request_scroll_layout();
            ctx.request_paint_rect(dmg);
        }
    }
}

impl Default for ScrollView {
    fn default() -> Self {
        Self::new()
    }
}

impl<Msg: 'static> Widget<Msg> for ScrollView {
    fn layout_style(&self, _theme: &Theme) -> Style {
        Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: taffy::Size {
                width: style::percent(1.0),
                height: style::percent(1.0),
            },
            flex_grow: 1.0,
            overflow: taffy::Point {
                x: taffy::Overflow::Hidden,
                y: taffy::Overflow::Scroll,
            },
            ..Style::default()
        }
    }

    fn clips(&self) -> bool {
        true
    }

    fn content_offset(&self) -> Point {
        Point::new(0.0, -self.offset)
    }

    fn set_scroll_metrics(&mut self, content: Size, viewport: Size) {
        self.content_h = content.h;
        self.viewport_h = viewport.h;
        self.offset = self.offset.clamp(0.0, self.max_offset());
    }

    fn paint(&self, ctx: &mut PaintCtx) {
        // A thin scrollbar thumb when there's overflow.
        if self.max_offset() <= 0.0 {
            return;
        }
        let b = ctx.bounds();
        let line = ctx.theme().palette.line;
        let frac_visible = (self.viewport_h / self.content_h).clamp(0.0, 1.0);
        let thumb_h = (b.h * frac_visible).max(24.0);
        let t = self.offset / self.max_offset();
        let thumb_y = b.y + t * (b.h - thumb_h);
        let bar = Rect::new(b.right() - 6.0, thumb_y, 4.0, thumb_h);
        ctx.painter().fill_rounded_rect(bar, 2.0, line);
    }

    fn event(&mut self, ctx: &mut EventCtx<Msg>) {
        self.bounds = ctx.bounds();
        let ev = ctx.event().clone();
        match ev {
            Event::Scroll { delta_y, .. } => {
                self.scroll_by(delta_y, ctx);
                ctx.set_handled();
            }
            Event::PointerDown {
                button: PointerButton::Left,
                pos,
            } => {
                // A new touch stops any coast and starts a drag.
                self.kinetic.stop();
                self.drag = Some(pos.y);
                ctx.capture_pointer();
            }
            Event::PointerMove { pos } => {
                if let Some(last) = self.drag {
                    self.scroll_by(last - pos.y, ctx);
                    self.drag = Some(pos.y);
                }
            }
            Event::PointerUp {
                button: PointerButton::Left,
                ..
            } => {
                if self.drag.take().is_some() {
                    ctx.release_pointer();
                }
            }
            Event::Fling { velocity_y, .. } => {
                // Finger moving up (negative velocity_y) coasts content upward,
                // i.e. a positive offset velocity — matching the drag mapping.
                if self.max_offset() > 0.0 {
                    self.kinetic.start(-velocity_y);
                    ctx.request_anim();
                    ctx.set_handled();
                }
            }
            _ => {}
        }
    }

    fn animate(&mut self, dt: f32) -> Anim {
        if !self.kinetic.is_running() {
            return Anim::IDLE;
        }
        let dy = self.kinetic.step(dt);
        match self.scroll_blit_by(dy, self.bounds) {
            Some(dmg) => Anim {
                repaint: false,
                // Children still re-place at the new offset; the strip is
                // damaged when the blit is applied.
                relayout: true,
                running: self.kinetic.is_running(),
                damage: Some(dmg),
            },
            None => {
                // Hit a bound: nothing left to coast into.
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
