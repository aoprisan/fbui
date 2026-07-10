//! [`Spinner`] — an indeterminate activity indicator.

use std::any::Any;

use fbui_render::geom::Rect;

use crate::ctx::PaintCtx;
use crate::style::{self, Style};
use crate::theme::Theme;
use crate::widget::{Anim, Widget};

/// Default diameter, logical px.
const DEFAULT_SIZE: f32 = 28.0;
/// Seconds per revolution.
const PERIOD: f32 = 1.0;
/// Dots around the ring.
const DOTS: usize = 12;
/// The faintest trailing dot's alpha fraction.
const MIN_ALPHA: f32 = 0.15;

/// A ring of dots with a rotating brightness head — "something is happening,
/// duration unknown". Use [`ProgressBar`](crate::widgets::ProgressBar) instead
/// when you can measure the work.
///
/// Spins from the moment it's added; stop and restart it from `App::update`
/// via [`Ui::with`](crate::Ui::with) + [`set_running`](Self::set_running).
/// While running it repaints only its own bounds each frame, and while stopped
/// it costs nothing (the tree goes idle if nothing else animates) — the
/// idle-burns-0% rule. The rotation advances by the frame `dt`, never a wall
/// clock, so it stays deterministic under test.
pub struct Spinner {
    /// Revolution phase in `[0, 1)`.
    phase: f32,
    running: bool,
    size: f32,
}

impl Spinner {
    /// A running spinner at the default size.
    pub fn new() -> Self {
        Spinner {
            phase: 0.0,
            running: true,
            size: DEFAULT_SIZE,
        }
    }

    /// Override the diameter (logical px).
    pub fn size(mut self, size: f32) -> Self {
        self.size = size.max(1.0);
        self
    }

    /// Builder form of [`set_running`](Self::set_running).
    pub fn running(mut self, running: bool) -> Self {
        self.running = running;
        self
    }

    /// Start or stop spinning. Stopped, the dots freeze in place; restarting
    /// resumes from the same phase.
    pub fn set_running(&mut self, running: bool) {
        self.running = running;
    }

    /// Whether the spinner is currently spinning.
    pub fn is_running(&self) -> bool {
        self.running
    }
}

impl Default for Spinner {
    fn default() -> Self {
        Self::new()
    }
}

impl<Msg: 'static> Widget<Msg> for Spinner {
    fn layout_style(&self, _theme: &Theme) -> Style {
        Style {
            size: taffy::Size {
                width: style::length(self.size),
                height: style::length(self.size),
            },
            flex_grow: 0.0,
            flex_shrink: 0.0,
            ..Style::default()
        }
    }

    fn paint(&self, ctx: &mut PaintCtx) {
        let b = ctx.bounds();
        let accent = ctx.theme().palette.accent;
        let (cx, cy) = (b.x + b.w / 2.0, b.y + b.h / 2.0);
        let outer = b.w.min(b.h) / 2.0;
        let dot_r = (outer * 0.14).max(1.0);
        let ring_r = outer - dot_r;

        // The brightness head sweeps the ring; each dot fades with its distance
        // behind the head. Quantizing the head to whole dots makes the motion
        // read as a step per dot (the classic look) and keeps repaints honest.
        let head = (self.phase * DOTS as f32).floor();
        let p = ctx.painter();
        for i in 0..DOTS {
            let frac = ((i as f32 - head).rem_euclid(DOTS as f32)) / DOTS as f32;
            let alpha = 1.0 - (1.0 - MIN_ALPHA) * frac;
            let angle = std::f32::consts::TAU * i as f32 / DOTS as f32;
            let (dx, dy) = (cx + ring_r * angle.cos(), cy + ring_r * angle.sin());
            // A dot is a fully-rounded square (there is no circle primitive).
            let dot = Rect::new(dx - dot_r, dy - dot_r, 2.0 * dot_r, 2.0 * dot_r);
            p.fill_rounded_rect(dot, dot_r, accent.with_alpha((alpha * 255.0) as u8));
        }
    }

    fn animate(&mut self, dt: f32) -> Anim {
        if !self.running {
            return Anim::IDLE;
        }
        let before = (self.phase * DOTS as f32).floor();
        self.phase = (self.phase + dt / PERIOD).fract();
        // Only a head step changes pixels; skip the repaint between steps.
        let stepped = (self.phase * DOTS as f32).floor() != before;
        Anim {
            repaint: stepped,
            running: true,
            ..Anim::IDLE
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
