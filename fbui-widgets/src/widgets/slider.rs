//! [`Slider`] — a draggable value in a range.

use std::any::Any;

use fbui_render::geom::Rect;

use crate::ctx::{EventCtx, PaintCtx};
use crate::event::{Event, Key, PointerButton};
use crate::style::{self, Style};
use crate::theme::Theme;
use crate::util::focus_ring;
use crate::widget::Widget;

const HEIGHT: f32 = 28.0;
const THUMB: f32 = 18.0;
const TRACK_H: f32 = 4.0;

/// A horizontal slider over `[min, max]`. Drag the thumb or use Left/Right;
/// emits `on_change(value)`.
pub struct Slider<Msg> {
    min: f32,
    max: f32,
    value: f32,
    step: f32,
    on_change: Option<Box<dyn Fn(f32) -> Msg>>,
    dragging: bool,
}

impl<Msg> Slider<Msg> {
    pub fn new(min: f32, max: f32, value: f32) -> Self {
        Slider {
            min,
            max,
            value: value.clamp(min, max),
            step: (max - min) / 20.0,
            on_change: None,
            dragging: false,
        }
    }

    pub fn on_change(mut self, f: impl Fn(f32) -> Msg + 'static) -> Self {
        self.on_change = Some(Box::new(f));
        self
    }

    /// Set the keyboard/step increment.
    pub fn step(mut self, step: f32) -> Self {
        self.step = step;
        self
    }

    pub fn value(&self) -> f32 {
        self.value
    }

    /// Set the value (call via [`Ui::with`](crate::Ui::with)).
    pub fn set_value(&mut self, value: f32) {
        self.value = value.clamp(self.min, self.max);
    }

    fn fraction(&self) -> f32 {
        if self.max > self.min {
            (self.value - self.min) / (self.max - self.min)
        } else {
            0.0
        }
    }

    fn track_rect(&self, b: Rect) -> Rect {
        let inset = THUMB / 2.0;
        Rect::new(b.x + inset, b.y, b.w - 2.0 * inset, b.h)
    }

    fn set_from_x(&mut self, b: Rect, x: f32, ctx: &mut EventCtx<Msg>) {
        let track = self.track_rect(b);
        let t = if track.w > 0.0 {
            ((x - track.x) / track.w).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let v = self.min + t * (self.max - self.min);
        self.update(v, ctx);
    }

    fn update(&mut self, v: f32, ctx: &mut EventCtx<Msg>) {
        let v = v.clamp(self.min, self.max);
        if (v - self.value).abs() > f32::EPSILON {
            self.value = v;
            if let Some(f) = &self.on_change {
                ctx.emit(f(v));
            }
            ctx.request_paint();
        }
    }
}

impl<Msg: 'static> Widget<Msg> for Slider<Msg> {
    fn layout_style(&self, _theme: &Theme) -> Style {
        Style {
            size: taffy::Size {
                width: style::auto(),
                height: style::length(HEIGHT),
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
        let focused = ctx.is_focused();
        let theme = ctx.theme();
        let (line, accent, on_accent, fw) = (
            theme.palette.line,
            theme.palette.accent,
            theme.palette.on_accent,
            theme.metrics.focus_width,
        );

        let track = self.track_rect(b);
        let cy = b.y + b.h / 2.0;
        let track_rect = Rect::new(track.x, cy - TRACK_H / 2.0, track.w, TRACK_H);
        let t = self.fraction();
        let thumb_cx = track.x + t * track.w;
        let filled = Rect::new(track.x, cy - TRACK_H / 2.0, t * track.w, TRACK_H);

        let p = ctx.painter();
        p.fill_rounded_rect(track_rect, TRACK_H / 2.0, line);
        p.fill_rounded_rect(filled, TRACK_H / 2.0, accent);
        let thumb = Rect::new(thumb_cx - THUMB / 2.0, cy - THUMB / 2.0, THUMB, THUMB);
        p.fill_rounded_rect(thumb, THUMB / 2.0, on_accent);
        p.stroke_rounded_rect(thumb, THUMB / 2.0, accent, 2.0);
        if focused {
            focus_ring(p, b, b.h / 2.0, accent, fw);
        }
    }

    fn event(&mut self, ctx: &mut EventCtx<Msg>) {
        let b = ctx.bounds();
        let ev = ctx.event().clone();
        match ev {
            Event::PointerDown {
                button: PointerButton::Left,
                pos,
            } => {
                self.dragging = true;
                ctx.capture_pointer();
                ctx.request_focus();
                self.set_from_x(b, pos.x, ctx);
                ctx.set_handled();
            }
            Event::PointerMove { pos } if self.dragging => {
                self.set_from_x(b, pos.x, ctx);
                ctx.set_handled();
            }
            Event::PointerUp {
                button: PointerButton::Left,
                ..
            } => {
                self.dragging = false;
                ctx.release_pointer();
                ctx.set_handled();
            }
            Event::Key {
                key: Key::Left,
                pressed: true,
                ..
            } if ctx.is_focused() => {
                let v = self.value - self.step;
                self.update(v, ctx);
                ctx.set_handled();
            }
            Event::Key {
                key: Key::Right,
                pressed: true,
                ..
            } if ctx.is_focused() => {
                let v = self.value + self.step;
                self.update(v, ctx);
                ctx.set_handled();
            }
            _ => {}
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
