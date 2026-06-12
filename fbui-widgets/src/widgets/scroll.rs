//! [`ScrollView`] — a clipping viewport that offsets its children vertically.
//!
//! The viewport clips and translates its subtree; the [`Ui`](crate::Ui) feeds it
//! its content vs. viewport extents after layout (via `set_scroll_metrics`), so it
//! can clamp scrolling without reaching into the tree. A [`Fling`](Event::Fling)
//! (Phase 4) starts a kinetic coast that decays in [`animate`](Widget::animate).

use std::any::Any;

use fbui_render::geom::{Point, Rect, Size};

use crate::ctx::{EventCtx, PaintCtx};
use crate::event::{Event, PointerButton};
use crate::kinetic::Kinetic;
use crate::style::{self, Style};
use crate::theme::Theme;
use crate::widget::{Anim, Widget};

/// A vertically scrolling container.
pub struct ScrollView {
    offset: f32,
    content_h: f32,
    viewport_h: f32,
    drag: Option<f32>,
    /// Momentum after a fling, in offset-pixels per second; 0 when at rest.
    kinetic: Kinetic,
}

impl ScrollView {
    pub fn new() -> Self {
        ScrollView {
            offset: 0.0,
            content_h: 0.0,
            viewport_h: 0.0,
            drag: None,
            kinetic: Kinetic::new(),
        }
    }

    fn max_offset(&self) -> f32 {
        (self.content_h - self.viewport_h).max(0.0)
    }

    /// Apply a scroll delta, clamped. Returns whether the offset actually moved.
    fn apply_scroll(&mut self, dy: f32) -> bool {
        let new = (self.offset + dy).clamp(0.0, self.max_offset());
        if (new - self.offset).abs() > f32::EPSILON {
            self.offset = new;
            true
        } else {
            false
        }
    }

    fn scroll_by<Msg>(&mut self, dy: f32, ctx: &mut EventCtx<Msg>) {
        if self.apply_scroll(dy) {
            // Children must be re-placed at the new offset.
            ctx.request_layout();
            ctx.request_paint();
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
                    ctx.request_paint();
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
        let moved = self.apply_scroll(dy);
        if !moved {
            // Hit a bound: nothing left to coast into.
            self.kinetic.stop();
        }
        if moved {
            Anim {
                repaint: true,
                relayout: true,
                running: self.kinetic.is_running(),
            }
        } else {
            Anim::IDLE
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
