//! [`Gauge`] — a radial meter for a bounded quantity: pressure, temperature,
//! load, RPM.
//!
//! The other half of the HMI instrument pair (with the streaming
//! [`Chart`](super::Chart)): a 270° dial with a colored-zone track, tick
//! marks, an accent value arc, a needle, and a numeric readout. The needle is
//! animated — [`set_value`](Gauge::set_value) retargets a [`Tween`], and the
//! dial glides on the deterministic frame clock (`dt`-driven, so it's
//! headless-testable and freezes when the app idles, per the 0%-idle rule).

use std::any::Any;
use std::f32::consts::PI;

use fbui_render::geom::{Point, Size};
use fbui_render::path::{Path, PathBuilder};
use fbui_render::{Color, FontContext};

use crate::anim::{Easing, Tween};
use crate::ctx::PaintCtx;
use crate::style::Style;
use crate::theme::Theme;
use crate::tree::StreamDamage;
use crate::util::{nice_step, text_style, tick_label};
use crate::widget::{Anim, AvailableSize, KnownDims, Widget};

/// Dial start angle in screen coordinates (y down): 135°, lower-left.
const START: f32 = 0.75 * PI;
/// Dial sweep: 270°, ending lower-right.
const SWEEP: f32 = 1.5 * PI;

/// A radial gauge over `[min, max]`.
///
/// Update it with [`set_value`](Gauge::set_value) via
/// [`Ui::with`](crate::Ui::with), or at telemetry rate with
/// [`update`](Gauge::update) via [`Ui::stream`](crate::Ui::stream) (a gauge
/// always repaints its whole box — it's small — but `stream` skips the
/// relayout `with` schedules).
pub struct Gauge {
    min: f32,
    max: f32,
    value: f32,
    /// The needle's animated position, in value units.
    shown: Tween<f32>,
    /// Zone bands as `(upper_bound, color)`, ascending; each band runs from
    /// the previous bound (or `min`) up to its own.
    zones: Vec<(f32, Color)>,
    label: Option<String>,
    show_value: bool,
    decimals: usize,
    anim_secs: f32,
    preferred: Size,
}

impl Gauge {
    /// A gauge spanning `min..max`, needle at `min`.
    pub fn new(min: f32, max: f32) -> Self {
        let (min, max) = if max > min {
            (min, max)
        } else {
            (max, min + 1.0)
        };
        Gauge {
            min,
            max,
            value: min,
            shown: Tween::settled(min, 0.25, Easing::EaseOut),
            zones: Vec::new(),
            label: None,
            show_value: true,
            decimals: 0,
            anim_secs: 0.25,
            preferred: Size::new(140.0, 120.0),
        }
    }

    /// Append a zone band from the previous bound (or `min`) up to `upper`,
    /// painted on the track in `color`. Add zones in ascending order — a
    /// green/amber/red split is three calls.
    pub fn zone(mut self, upper: f32, color: Color) -> Self {
        self.zones.push((upper, color));
        self
    }

    /// A caption under the readout (typically the unit: `"°C"`, `"PSI"`).
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Show or hide the numeric readout (default: shown).
    pub fn show_value(mut self, on: bool) -> Self {
        self.show_value = on;
        self
    }

    /// Decimal places in the readout (default 0).
    pub fn decimals(mut self, n: usize) -> Self {
        self.decimals = n;
        self
    }

    /// Needle glide time in seconds for a full retarget; 0 disables animation.
    pub fn animate_secs(mut self, secs: f32) -> Self {
        self.anim_secs = secs.max(0.0);
        self
    }

    /// Preferred (intrinsic) size reported to layout.
    pub fn preferred_size(mut self, w: f32, h: f32) -> Self {
        self.preferred = Size::new(w, h);
        self
    }

    /// Initial value (builder form of [`set_value`](Gauge::set_value), without
    /// animation).
    pub fn value(mut self, v: f32) -> Self {
        let v = v.clamp(self.min, self.max);
        self.value = v;
        self.shown = Tween::settled(v, self.anim_secs.max(0.01), Easing::EaseOut);
        self
    }

    /// The current target value.
    pub fn current(&self) -> f32 {
        self.value
    }

    /// Retarget the needle to `v` (clamped to the range). The glide runs on
    /// the frame clock; with `animate_secs(0.0)` the needle jumps.
    pub fn set_value(&mut self, v: f32) {
        let v = v.clamp(self.min, self.max);
        if v == self.value {
            return;
        }
        self.value = v;
        if self.anim_secs <= 0.0 {
            self.shown = Tween::settled(v, 0.01, Easing::EaseOut);
        } else {
            self.shown.retarget(v);
        }
    }

    /// [`set_value`](Gauge::set_value) shaped for
    /// [`Ui::stream`](crate::Ui::stream): reports [`StreamDamage::Repaint`]
    /// when the reading changed and [`StreamDamage::Quiet`] when it didn't.
    pub fn update(&mut self, v: f32) -> StreamDamage {
        let before = self.value;
        self.set_value(v);
        if self.value == before && self.shown.is_done() {
            StreamDamage::Quiet
        } else {
            StreamDamage::Repaint
        }
    }

    fn angle_of(&self, v: f32) -> f32 {
        START + (v - self.min) / (self.max - self.min) * SWEEP
    }
}

impl<Msg: 'static> Widget<Msg> for Gauge {
    fn layout_style(&self, _theme: &Theme) -> Style {
        Style::default()
    }

    fn measure(
        &mut self,
        _fonts: &mut FontContext,
        _theme: &Theme,
        _known: KnownDims,
        _available: AvailableSize,
    ) -> Option<Size> {
        Some(self.preferred)
    }

    fn animate(&mut self, dt: f32) -> Anim {
        if self.shown.is_done() {
            return Anim::IDLE;
        }
        let running = self.shown.advance(dt);
        Anim {
            repaint: true,
            relayout: false,
            running,
            damage: None,
        }
    }

    fn paint(&self, ctx: &mut PaintCtx) {
        let b = ctx.bounds();
        let theme = ctx.theme();
        if b.w <= 8.0 || b.h <= 8.0 {
            return;
        }

        // Dial geometry: centre sits low so the 270° arc fills the box.
        let cx = b.x + b.w / 2.0;
        let cy = b.y + b.h * 0.54;
        let r = (b.w / 2.0 - 4.0).min(b.h * 0.5).max(8.0);
        let track_w = (r * 0.18).clamp(3.0, 12.0);
        let arc_r = r - track_w / 2.0;

        let accent = theme.palette.accent;
        let line = theme.palette.line;
        let muted = theme.palette.muted;
        let text = theme.palette.text;
        let font_size = theme.metrics.font_size;
        let value_style = text_style(theme, font_size * 1.15, text).bold();
        let label_style = text_style(theme, font_size * 0.75, muted);

        let shown = self.shown.value().clamp(self.min, self.max);
        let shown_angle = self.angle_of(shown);

        let p = ctx.painter();

        // Track: zone bands if configured, else the plain line color.
        if self.zones.is_empty() {
            let mut pb = PathBuilder::new();
            pb.arc(cx, cy, arc_r, START, SWEEP);
            if let Some(path) = pb.finish() {
                p.stroke_path(&path, line, track_w);
            }
        } else {
            let mut from = self.min;
            for &(upper, color) in &self.zones {
                let to = upper.clamp(self.min, self.max);
                if to > from {
                    let (a0, a1) = (self.angle_of(from), self.angle_of(to));
                    let mut pb = PathBuilder::new();
                    pb.arc(cx, cy, arc_r, a0, a1 - a0);
                    if let Some(path) = pb.finish() {
                        p.stroke_path(&path, color.with_alpha(120), track_w);
                    }
                    from = to;
                }
            }
            // Anything past the last zone stays neutral track.
            if from < self.max {
                let a0 = self.angle_of(from);
                let mut pb = PathBuilder::new();
                pb.arc(cx, cy, arc_r, a0, self.angle_of(self.max) - a0);
                if let Some(path) = pb.finish() {
                    p.stroke_path(&path, line, track_w);
                }
            }
        }

        // Value arc: min → needle, on top of the track.
        if shown > self.min {
            let mut pb = PathBuilder::new();
            pb.arc(cx, cy, arc_r, START, shown_angle - START);
            if let Some(path) = pb.finish() {
                p.stroke_path(&path, accent, track_w);
            }
        }

        // Major ticks on the nice-step ladder, just inside the track.
        let ts = nice_step((self.max - self.min) / 5.0);
        let mut k = (self.min / ts).ceil();
        while k * ts <= self.max + ts * 0.001 {
            let a = self.angle_of(k * ts);
            let (dx, dy) = (a.cos(), a.sin());
            let r0 = arc_r - track_w / 2.0 - 2.0;
            let r1 = r0 - (r * 0.08).max(3.0);
            let mut pb = PathBuilder::new();
            pb.move_to(cx + dx * r0, cy + dy * r0);
            pb.line_to(cx + dx * r1, cy + dy * r1);
            if let Some(path) = pb.finish() {
                p.stroke_path(&path, muted, 1.0);
            }
            k += 1.0;
        }

        // Needle: a slim triangle from the hub to just short of the track.
        let tip_r = arc_r - track_w / 2.0 - 3.0;
        if tip_r > 6.0 {
            let (dx, dy) = (shown_angle.cos(), shown_angle.sin());
            let (nx, ny) = (-dy, dx); // perpendicular
            let half = (r * 0.035).clamp(1.5, 3.0);
            let tip = Point::new(cx + dx * tip_r, cy + dy * tip_r);
            let mut pb = PathBuilder::new();
            pb.move_to(tip.x, tip.y);
            pb.line_to(cx + nx * half, cy + ny * half);
            pb.line_to(cx - nx * half, cy - ny * half);
            pb.close();
            if let Some(path) = pb.finish() {
                p.fill_path(&path, text);
            }
        }
        if let Some(hub) = Path::circle(cx, cy, (r * 0.06).clamp(2.5, 5.0)) {
            p.fill_path(&hub, text);
        }

        // Readout and label, centred under the hub.
        if self.show_value {
            let s = if self.decimals == 0 {
                tick_label(self.value)
            } else {
                format!("{:.*}", self.decimals, self.value)
            };
            let (p, fonts) = ctx.painter_and_fonts();
            let w = fonts.layout(&s, &value_style, None).size().w;
            fonts.draw_text(
                p,
                &s,
                &value_style,
                Point::new(cx - w / 2.0, cy + r * 0.28),
                None,
            );
        }
        if let Some(label) = &self.label {
            let (p, fonts) = ctx.painter_and_fonts();
            let w = fonts.layout(label, &label_style, None).size().w;
            fonts.draw_text(
                p,
                label,
                &label_style,
                Point::new(cx - w / 2.0, cy + r * 0.28 + value_style.line_height),
                None,
            );
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
