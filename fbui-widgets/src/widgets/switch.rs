//! [`Switch`] — an animated on/off toggle (Phase 5).
//!
//! Functionally a checkbox, but it demonstrates the [`anim`](crate::anim) tween
//! API: flipping it slides the thumb and cross-fades the track over a fraction of
//! a second instead of snapping. The animation rides the
//! [`animate`](Widget::animate) frame-clock hook and damages only the switch, so
//! it costs a few small repaints and then stops.

use std::any::Any;

use fbui_render::geom::{Point, Rect, Size};
use fbui_render::FontContext;

use crate::anim::{Easing, Lerp, Tween};
use crate::ctx::{EventCtx, PaintCtx};
use crate::event::{Event, Key, PointerButton};
use crate::style::Style;
use crate::theme::Theme;
use crate::util::{focus_ring, text_style};
use crate::widget::{Anim, AvailableSize, KnownDims, Widget};

const TRACK_W: f32 = 44.0;
const TRACK_H: f32 = 24.0;
const PAD: f32 = 3.0;
const GAP: f32 = 8.0;
/// Toggle transition duration in seconds.
const DURATION: f32 = 0.18;

/// An animated boolean toggle with a trailing label. Emits `on_toggle(new)`.
pub struct Switch<Msg> {
    label: String,
    on: bool,
    /// Thumb position / track blend, 0 (off) → 1 (on).
    pos: Tween<f32>,
    on_toggle: Option<Box<dyn Fn(bool) -> Msg>>,
}

impl<Msg> Switch<Msg> {
    pub fn new(label: impl Into<String>, on: bool) -> Self {
        let target = if on { 1.0 } else { 0.0 };
        Switch {
            label: label.into(),
            on,
            pos: Tween::settled(target, DURATION, Easing::EaseInOut),
            on_toggle: None,
        }
    }

    pub fn on_toggle(mut self, f: impl Fn(bool) -> Msg + 'static) -> Self {
        self.on_toggle = Some(Box::new(f));
        self
    }

    /// Current state.
    pub fn is_on(&self) -> bool {
        self.on
    }

    /// Set state, animating to it (call via [`Ui::with`](crate::Ui::with)).
    pub fn set_on(&mut self, on: bool) {
        if on != self.on {
            self.on = on;
            self.pos.retarget(if on { 1.0 } else { 0.0 });
        }
    }

    fn toggle(&mut self, ctx: &mut EventCtx<Msg>) {
        self.on = !self.on;
        self.pos.retarget(if self.on { 1.0 } else { 0.0 });
        if let Some(f) = &self.on_toggle {
            ctx.emit(f(self.on));
        }
        ctx.request_paint();
        ctx.request_anim();
        ctx.set_handled();
    }
}

impl<Msg: 'static> Widget<Msg> for Switch<Msg> {
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
        let st = text_style(theme, theme.metrics.font_size, theme.palette.text);
        let t = fonts.layout(&self.label, &st, None).size();
        let w = if self.label.is_empty() {
            TRACK_W
        } else {
            TRACK_W + GAP + t.w
        };
        Some(Size::new(w, TRACK_H.max(t.h)))
    }

    fn focusable(&self) -> bool {
        true
    }

    fn paint(&self, ctx: &mut PaintCtx) {
        let theme = ctx.theme();
        let b = ctx.bounds();
        let focused = ctx.is_focused();
        let track = Rect::new(b.x, b.y + (b.h - TRACK_H) / 2.0, TRACK_W, TRACK_H);
        let st = text_style(theme, theme.metrics.font_size, theme.palette.text);
        let (accent, off_track, on_accent, surface) = (
            theme.palette.accent,
            theme.palette.line,
            theme.palette.on_accent,
            theme.palette.surface,
        );
        let fw = theme.metrics.focus_width;

        // Eased blend value drives both the track colour and the thumb position.
        let v = self.pos.value();
        let track_color = off_track.lerp(accent, v);
        let thumb_color = surface.lerp(on_accent, v);
        let r = (TRACK_H - 2.0 * PAD) / 2.0;
        let cx = track.x + PAD + r + v * (TRACK_W - 2.0 * (PAD + r));
        let cy = track.y + TRACK_H / 2.0;
        let thumb = Rect::new(cx - r, cy - r, 2.0 * r, 2.0 * r);

        let (p, fonts) = ctx.painter_and_fonts();
        p.fill_rounded_rect(track, TRACK_H / 2.0, track_color);
        p.fill_rounded_rect(thumb, r, thumb_color);
        if !self.label.is_empty() {
            let ty = b.y + (b.h - st.size) / 2.0 - 1.0;
            fonts.draw_text(
                p,
                &self.label,
                &st,
                Point::new(b.x + TRACK_W + GAP, ty),
                None,
            );
        }
        if focused {
            focus_ring(p, b, 6.0, accent, fw);
        }
    }

    fn event(&mut self, ctx: &mut EventCtx<Msg>) {
        let ev = ctx.event().clone();
        match ev {
            Event::PointerDown {
                button: PointerButton::Left,
                ..
            } => {
                ctx.request_focus();
                self.toggle(ctx);
            }
            Event::Key {
                key: Key::Space,
                pressed: true,
                ..
            } if ctx.is_focused() => {
                self.toggle(ctx);
            }
            _ => {}
        }
    }

    fn animate(&mut self, dt: f32) -> Anim {
        if self.pos.advance(dt) {
            Anim::repaint()
        } else {
            Anim::IDLE
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
