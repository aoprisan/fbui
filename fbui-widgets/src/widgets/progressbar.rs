//! [`ProgressBar`] — a read-only fraction indicator for long-running work.

use std::any::Any;

use fbui_render::geom::Rect;

use crate::ctx::PaintCtx;
use crate::style::{self, Style};
use crate::theme::Theme;
use crate::widget::Widget;

const HEIGHT: f32 = 8.0;

/// A horizontal progress bar showing a fraction in `[0, 1]`.
///
/// Non-interactive: it has no events and isn't focusable. Drive it from
/// `App::update` via [`Ui::with`](crate::Ui::with) — typically from progress a
/// background worker posts through a [`Proxy`](../../fbui/struct.Proxy.html). It
/// reuses the theme's track (`line`) and `accent` colors so it sits naturally
/// next to a [`Slider`](crate::widgets::Slider).
pub struct ProgressBar {
    fraction: f32,
}

impl ProgressBar {
    /// A bar at `fraction`, clamped to `[0, 1]`.
    pub fn new(fraction: f32) -> Self {
        ProgressBar {
            fraction: fraction.clamp(0.0, 1.0),
        }
    }

    /// The current fraction, in `[0, 1]`.
    pub fn fraction(&self) -> f32 {
        self.fraction
    }

    /// Set the fraction (clamped to `[0, 1]`). Call via
    /// [`Ui::with`](crate::Ui::with), which marks the bar for repaint.
    pub fn set_fraction(&mut self, fraction: f32) {
        self.fraction = fraction.clamp(0.0, 1.0);
    }
}

impl<Msg: 'static> Widget<Msg> for ProgressBar {
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

    fn paint(&self, ctx: &mut PaintCtx) {
        let b = ctx.bounds();
        let theme = ctx.theme();
        let (line, accent) = (theme.palette.line, theme.palette.accent);
        let r = b.h / 2.0;

        let p = ctx.painter();
        // The track, then the filled portion (a pill) on top.
        p.fill_rounded_rect(b, r, line);
        let w = b.w * self.fraction;
        if w > 0.0 {
            // Cap the radius so a sliver of fill stays a sane rounded shape.
            p.fill_rounded_rect(Rect::new(b.x, b.y, w, b.h), r.min(w / 2.0), accent);
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
