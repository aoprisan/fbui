//! [`Chart`] — a streaming multi-series strip chart for live telemetry.
//!
//! Built for the HMI dashboard case: samples arrive at wire rate (a sensor
//! poll, a `Proxy` from a worker thread) and the trace scrolls leftward with
//! the newest value pinned to the right edge. Pushing goes through
//! [`Ui::stream`](crate::Ui::stream), and the chart answers with the cheapest
//! honest damage: when the y-range is unchanged, the plot's existing pixels
//! are **scroll-blitted** left ([`Widget::scroll_blit_xy`]) and only the strip
//! that scrolled into view — a few columns — is re-rasterized. A byte-for-byte
//! equivalence test pins the fast path against a full repaint, per the
//! framework invariant.
//!
//! What keeps the blit exact (and what to preserve when editing):
//! - The sample step is quantized to a whole number of **device** pixels in
//!   [`placed`](Widget::placed), so every shift is an integer memmove.
//! - Sample x-positions are measured from the plot's right edge in whole
//!   steps, so one push translates the whole trace by exactly one step.
//! - Everything painted inside the plot is translation-invariant: the
//!   background is a uniform fill, horizontal gridlines span the full width,
//!   and vertical time-gridlines are phase-locked to absolute sample indices,
//!   so they scroll with the data.
//! - Axis labels live in a left gutter *outside* the blit region.
//! - When the auto-range moves (quantized to "nice" bounds so it moves
//!   rarely), the chart falls back to a full repaint of its box.

use std::any::Any;
use std::collections::VecDeque;

use fbui_render::geom::{Point, Rect, Size};
use fbui_render::path::PathBuilder;
use fbui_render::{Color, FontContext, Scale};

use crate::ctx::PaintCtx;
use crate::style::Style;
use crate::theme::Theme;
use crate::tree::StreamDamage;
use crate::util::{nice_range, nice_step, text_style, tick_label};
use crate::widget::{AvailableSize, KnownDims, Widget};

/// Fallback series colors (series 0 uses the theme accent): readable on both
/// bundled themes, in the order extra series are created.
const SERIES_COLORS: [Color; 5] = [
    Color::rgb(0x34, 0xd3, 0x99), // green
    Color::rgb(0xfb, 0xbf, 0x24), // amber
    Color::rgb(0xa7, 0x8b, 0xfa), // violet
    Color::rgb(0x22, 0xd3, 0xee), // cyan
    Color::rgb(0xf4, 0x72, 0xb6), // pink
];

/// Ring-buffer cap used before the first layout tells us the real capacity.
const PRELAYOUT_CAP: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq)]
enum RangeMode {
    /// Track the data, quantized outward to "nice" bounds.
    Auto,
    Fixed(f32, f32),
}

struct Series {
    color: Option<Color>,
    data: VecDeque<f32>,
}

/// Geometry derived from layout, cached so [`Chart::push`] can make damage
/// decisions without a paint context.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Geo {
    bounds: Rect,
    /// Logical px per sample — a whole number of device px at this scale.
    step: f32,
    /// Ring capacity: visible samples plus stroke overhang.
    capacity: usize,
}

/// A live strip chart. See the module docs for the damage contract.
///
/// ```no_run
/// # use fbui_widgets::{Ui, StreamDamage};
/// # use fbui_widgets::widgets::Chart;
/// # let mut ui: Ui<()> = todo!();
/// # let chart_id: fbui_widgets::WidgetId = todo!();
/// # let reading = 0.0f32;
/// // In App::update, on each new reading:
/// ui.stream(chart_id, |c: &mut Chart| c.push(&[reading]));
/// ```
pub struct Chart {
    series: Vec<Series>,
    range_mode: RangeMode,
    /// Active painted range; meaningful once `has_range`.
    cur_range: (f32, f32),
    has_range: bool,
    grid: bool,
    /// Vertical gridline every N samples (scrolls with the data); 0 = none.
    time_grid: usize,
    fill: bool,
    stroke: f32,
    gutter: f32,
    /// Requested logical px per sample (quantized at layout).
    sample_width: f32,
    preferred: Size,
    geo: Option<Geo>,
    /// Pending leftward shift for the next paint's scroll-blit (≤ 0).
    blit_dx: f32,
    /// Samples ever pushed — the phase reference for the time grid.
    total: u64,
}

impl Chart {
    /// A framed chart with a y-axis gutter and gridlines, auto-ranged.
    pub fn new() -> Self {
        Chart {
            series: Vec::new(),
            range_mode: RangeMode::Auto,
            cur_range: (0.0, 1.0),
            has_range: false,
            grid: true,
            time_grid: 0,
            fill: false,
            stroke: 1.5,
            gutter: 36.0,
            sample_width: 2.0,
            preferred: Size::new(240.0, 96.0),
            geo: None,
            blit_dx: 0.0,
            total: 0,
        }
    }

    /// A chrome-free miniature: no gutter, no grid, filled trace — an inline
    /// activity/level indicator.
    pub fn sparkline() -> Self {
        Chart {
            grid: false,
            gutter: 0.0,
            fill: true,
            stroke: 1.0,
            preferred: Size::new(80.0, 24.0),
            ..Chart::new()
        }
    }

    /// Add an explicit series with its trace color. Series may also be created
    /// implicitly by [`push`](Chart::push), using the default color cycle.
    pub fn with_series(mut self, color: Color) -> Self {
        self.series.push(Series {
            color: Some(color),
            data: VecDeque::new(),
        });
        self
    }

    /// Fix the y-range instead of tracking the data. With a fixed range every
    /// push that doesn't move the range scroll-blits — the steady state for a
    /// bounded signal (a percentage, a temperature band).
    pub fn fixed_range(mut self, lo: f32, hi: f32) -> Self {
        let (lo, hi) = if hi > lo { (lo, hi) } else { (hi, lo + 1.0) };
        self.range_mode = RangeMode::Fixed(lo, hi);
        self.cur_range = (lo, hi);
        self.has_range = true;
        self
    }

    /// Toggle horizontal gridlines (at the y-axis ticks).
    pub fn grid(mut self, on: bool) -> Self {
        self.grid = on;
        self
    }

    /// Draw a vertical gridline every `samples` samples; it scrolls with the
    /// data, giving the trace a time reference. 0 disables.
    pub fn time_grid_every(mut self, samples: usize) -> Self {
        self.time_grid = samples;
        self
    }

    /// Fill under the trace with a translucent wash of the series color.
    pub fn fill(mut self, on: bool) -> Self {
        self.fill = on;
        self
    }

    /// Trace stroke width in logical px.
    pub fn stroke_width(mut self, w: f32) -> Self {
        self.stroke = w.max(0.1);
        self
    }

    /// Width of the left y-axis label gutter in logical px (0 = none).
    pub fn gutter(mut self, w: f32) -> Self {
        self.gutter = w.max(0.0);
        self
    }

    /// Logical px the trace advances per sample (quantized to whole device
    /// pixels at layout, minimum one).
    pub fn sample_width(mut self, w: f32) -> Self {
        self.sample_width = w.max(0.1);
        self
    }

    /// Preferred (intrinsic) size reported to layout.
    pub fn preferred_size(mut self, w: f32, h: f32) -> Self {
        self.preferred = Size::new(w, h);
        self
    }

    /// Append one sample per series, advancing the chart by one step, and
    /// report the precise damage — designed to be the body of a
    /// [`Ui::stream`](crate::Ui::stream) call. Series missing a value (or
    /// created later) receive a gap ([`f32::NAN`] breaks the trace). Extra
    /// values create new series on the default color cycle.
    pub fn push(&mut self, values: &[f32]) -> StreamDamage {
        self.total = self.total.wrapping_add(1);
        while self.series.len() < values.len() {
            self.series.push(Series {
                color: None,
                data: VecDeque::new(),
            });
        }
        let cap = self.geo.map(|g| g.capacity).unwrap_or(PRELAYOUT_CAP);
        for (i, s) in self.series.iter_mut().enumerate() {
            s.data.push_back(values.get(i).copied().unwrap_or(f32::NAN));
            while s.data.len() > cap {
                s.data.pop_front();
            }
        }

        let Some(geo) = self.geo else {
            // Not laid out yet: the first paint draws everything anyway.
            return StreamDamage::Repaint;
        };
        let plot = self.plot_rect(geo.bounds);
        if plot.w <= 0.0 || plot.h <= 0.0 {
            return StreamDamage::Quiet;
        }

        // Where does the y-range land after this sample? Auto mode holds the
        // current band with hysteresis — refit only when the data escapes it
        // or has shrunk to rattle around in it — so the steady state blits
        // instead of repainting on every extent wiggle.
        let range = match self.range_mode {
            RangeMode::Fixed(lo, hi) => (lo, hi),
            RangeMode::Auto => match self.data_extent() {
                Some((dmin, dmax)) => {
                    let (clo, chi) = self.cur_range;
                    let cspan = chi - clo;
                    let escaped = dmin < clo || dmax > chi;
                    let rattling = (dmax - dmin) < cspan * 0.35;
                    if self.has_range && !escaped && !rattling {
                        self.cur_range
                    } else {
                        // Refit with headroom so a drifting signal escapes in
                        // strides, not every sample.
                        let pad = (dmax - dmin) * 0.1;
                        nice_range(dmin - pad, dmax + pad)
                    }
                }
                None => return StreamDamage::Quiet, // no finite data yet
            },
        };
        if !self.has_range || range != self.cur_range {
            self.cur_range = range;
            self.has_range = true;
            self.blit_dx = 0.0;
            return StreamDamage::Repaint;
        }

        // Steady state: shift the plot left one step and repaint the seam.
        let dx = self.blit_dx - geo.step;
        if -dx >= plot.w {
            // Accumulated a whole plot width between paints — nothing reusable.
            self.blit_dx = 0.0;
            return StreamDamage::Repaint;
        }
        self.blit_dx = dx;
        // The exposed right strip is damaged when the blit is applied; the
        // extra seam covers the old trace endpoint (its cap + anti-aliasing)
        // that the new segment must overdraw.
        let seam = self.stroke * 0.5 + 3.0;
        let x = (plot.right() + dx - seam).max(plot.x);
        StreamDamage::Shifted {
            extra: Some(Rect::new(x, plot.y, seam, plot.h)),
        }
    }

    /// Single-series sugar for [`push`](Chart::push).
    pub fn push_one(&mut self, value: f32) -> StreamDamage {
        self.push(&[value])
    }

    /// Drop all samples (the next paint clears the traces).
    pub fn clear(&mut self) {
        for s in &mut self.series {
            s.data.clear();
        }
        self.blit_dx = 0.0;
        if self.range_mode == RangeMode::Auto {
            self.has_range = false;
        }
    }

    /// The y-range currently painted, once data (or a fixed range) set one.
    pub fn range(&self) -> Option<(f32, f32)> {
        self.has_range.then_some(self.cur_range)
    }

    /// Samples currently held by the longest series.
    pub fn len(&self) -> usize {
        self.series.iter().map(|s| s.data.len()).max().unwrap_or(0)
    }

    /// Whether no series holds any samples.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn plot_rect(&self, b: Rect) -> Rect {
        Rect::new(b.x + self.gutter, b.y, (b.w - self.gutter).max(0.0), b.h)
    }

    /// Min/max over every finite sample of every series.
    fn data_extent(&self) -> Option<(f32, f32)> {
        let mut ext: Option<(f32, f32)> = None;
        for s in &self.series {
            for &v in &s.data {
                if v.is_finite() {
                    let (lo, hi) = ext.get_or_insert((v, v));
                    *lo = lo.min(v);
                    *hi = hi.max(v);
                }
            }
        }
        ext
    }

    fn series_color(&self, idx: usize, theme: &Theme) -> Color {
        self.series[idx].color.unwrap_or_else(|| {
            if idx == 0 {
                theme.palette.accent
            } else {
                SERIES_COLORS[(idx - 1) % SERIES_COLORS.len()]
            }
        })
    }
}

impl Default for Chart {
    fn default() -> Self {
        Self::new()
    }
}

impl<Msg: 'static> Widget<Msg> for Chart {
    fn layout_style(&self, _theme: &Theme) -> Style {
        Style {
            flex_grow: 1.0,
            ..Style::default()
        }
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

    fn placed(&mut self, bounds: Rect, scale: Scale) {
        let factor = scale.factor();
        let plot = self.plot_rect(bounds);
        // A whole number of device pixels per sample: the property that makes
        // every push an exact integer blit.
        let step_dev = (self.sample_width * factor).round().max(1.0);
        let step = step_dev / factor;
        // Ring capacity: what fits across the plot, plus enough overhang that a
        // sample is only dropped once its stroke is fully off-screen (dropping
        // a still-visible sample would fork the fast path from ground truth).
        let visible = (plot.w * factor / step_dev).ceil() as usize + 1;
        let overhang = ((self.stroke * 0.5 + 3.0) * factor / step_dev).ceil() as usize + 2;
        let geo = Geo {
            bounds,
            step,
            capacity: visible + overhang,
        };
        if self.geo != Some(geo) {
            self.geo = Some(geo);
            // Stale pixels can't be reused across a geometry change; the
            // layout change already damaged the widget in full.
            self.blit_dx = 0.0;
            for s in &mut self.series {
                while s.data.len() > geo.capacity {
                    s.data.pop_front();
                }
            }
        }
    }

    fn scroll_blit_xy(&mut self, bounds: Rect) -> Option<(Rect, f32, f32)> {
        if self.blit_dx == 0.0 {
            return None;
        }
        let dx = std::mem::take(&mut self.blit_dx);
        Some((self.plot_rect(bounds), dx, 0.0))
    }

    fn paint(&self, ctx: &mut PaintCtx) {
        let b = ctx.bounds();
        let theme = ctx.theme();
        let plot = self.plot_rect(b);
        if plot.w <= 0.0 || plot.h <= 0.0 {
            return;
        }
        let (lo, hi) = self.cur_range;
        let span = (hi - lo).max(f32::EPSILON);
        let surface_alt = theme.palette.surface_alt;
        let line_color = theme.palette.line;
        let muted = theme.palette.muted;
        let n_series = self.series.len();
        let colors: Vec<Color> = (0..n_series).map(|i| self.series_color(i, theme)).collect();
        let step = self.geo.map(|g| g.step).unwrap_or(self.sample_width);
        let tick = if self.grid || self.gutter > 0.0 {
            Some(nice_step(span / 4.0))
        } else {
            None
        };
        let y_of = |v: f32| plot.bottom() - (v - lo) / span * plot.h;

        // ---- gutter (outside the blit region) ----
        if self.gutter > 0.0 {
            if let Some(ts) = tick {
                let style = text_style(theme, theme.metrics.font_size * 0.75, muted);
                let mut k = (lo / ts).ceil();
                while k * ts <= hi + ts * 0.001 {
                    let v = k * ts;
                    let y = y_of(v);
                    let label = tick_label(v);
                    let (p, fonts) = ctx.painter_and_fonts();
                    let w = fonts.layout(&label, &style, None).size().w;
                    let ly = (y - style.line_height * 0.5)
                        .clamp(plot.y, (plot.bottom() - style.line_height).max(plot.y));
                    fonts.draw_text(p, &label, &style, Point::new(plot.x - 6.0 - w, ly), None);
                    k += 1.0;
                }
            }
        }

        let p = ctx.painter();
        // ---- plot background: a uniform fill (translation-invariant) ----
        p.fill_rect(plot, surface_alt);
        p.push_clip(plot);

        // ---- horizontal gridlines: full-width, so a horizontal shift is a
        // no-op on them ----
        if self.grid {
            if let Some(ts) = tick {
                let mut k = (lo / ts).ceil();
                while k * ts <= hi + ts * 0.001 {
                    let y = y_of(k * ts);
                    p.fill_rect(
                        Rect::new(plot.x, y - 0.5, plot.w, 1.0),
                        line_color.with_alpha(96),
                    );
                    k += 1.0;
                }
            }
        }

        // ---- vertical time grid: phase-locked to absolute sample indices, so
        // it scrolls with the data ----
        let n = self.len();
        if self.time_grid > 0 && n > 0 {
            let g = self.time_grid as u64;
            let newest = self.total.saturating_sub(1);
            // Youngest gridline at or left of the newest sample.
            let mut a = newest - (newest % g);
            loop {
                let back = (newest - a) as f32;
                let x = plot.right() - back * step;
                if x < plot.x {
                    break;
                }
                p.fill_rect(
                    Rect::new(x - 0.5, plot.y, 1.0, plot.h),
                    line_color.with_alpha(64),
                );
                if a < g {
                    break;
                }
                a -= g;
            }
        }

        // ---- traces: newest sample anchored at the right edge; x measured in
        // whole steps from the right so a push is an exact translation ----
        for (si, s) in self.series.iter().enumerate() {
            let n = s.data.len();
            if n == 0 {
                continue;
            }
            let color = colors[si];
            let x_at = |i: usize| plot.right() - (n - 1 - i) as f32 * step;

            if self.fill {
                // One filled run per contiguous stretch of finite samples.
                let mut pb: Option<(PathBuilder, f32)> = None; // (path, run start x)
                let flush = |pb: &mut Option<(PathBuilder, f32)>,
                             end_x: f32,
                             p: &mut fbui_render::Painter| {
                    if let Some((mut path, start_x)) = pb.take() {
                        path.line_to(end_x, plot.bottom());
                        path.line_to(start_x, plot.bottom());
                        path.close();
                        if let Some(path) = path.finish() {
                            p.fill_path(&path, color.with_alpha(48));
                        }
                    }
                };
                let mut last_x = plot.x;
                for (i, &v) in s.data.iter().enumerate() {
                    let x = x_at(i);
                    if v.is_finite() {
                        let y = y_of(v);
                        match &mut pb {
                            Some((path, _)) => {
                                path.line_to(x, y);
                            }
                            None => {
                                let mut path = PathBuilder::new();
                                path.move_to(x, y);
                                pb = Some((path, x));
                            }
                        }
                        last_x = x;
                    } else {
                        flush(&mut pb, last_x, p);
                    }
                }
                flush(&mut pb, last_x, p);
            }

            // The stroked trace, with NaN gaps breaking the line.
            let mut pb = PathBuilder::new();
            let mut pen_down = false;
            let mut segments = false;
            for (i, &v) in s.data.iter().enumerate() {
                let x = x_at(i);
                if v.is_finite() {
                    let y = y_of(v);
                    if pen_down {
                        pb.line_to(x, y);
                        segments = true;
                    } else {
                        pb.move_to(x, y);
                        pen_down = true;
                    }
                } else {
                    pen_down = false;
                }
            }
            if segments {
                if let Some(path) = pb.finish() {
                    p.stroke_path(&path, color, self.stroke);
                }
            }
        }

        p.pop_clip();
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nice_step_picks_125_decades() {
        assert_eq!(nice_step(0.9), 1.0);
        assert_eq!(nice_step(1.1), 2.0);
        assert_eq!(nice_step(3.0), 5.0);
        assert_eq!(nice_step(7.0), 10.0);
        assert_eq!(nice_step(30.0), 50.0);
        assert!((nice_step(0.03) - 0.05).abs() < 1e-6);
    }

    #[test]
    fn nice_range_bounds_are_step_multiples() {
        let (lo, hi) = nice_range(3.2, 17.8);
        assert!(lo <= 3.2 && hi >= 17.8);
        let step = nice_step((17.8 - 3.2) / 4.0);
        assert!((lo / step).fract().abs() < 1e-3);
    }

    #[test]
    fn nice_range_survives_flat_data() {
        let (lo, hi) = nice_range(5.0, 5.0);
        assert!(lo <= 5.0 && hi >= 5.0 && hi > lo);
    }

    #[test]
    fn tick_labels_trim_zeros() {
        assert_eq!(tick_label(3.0), "3");
        assert_eq!(tick_label(12.5), "12.5");
        assert_eq!(tick_label(-0.0), "0");
    }
}
