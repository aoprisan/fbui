# Instrument widgets: streaming charts and gauges

fbui's target machines are HMI panels, kiosks, and dashboards — screens whose
job is showing *live readings*. The instrument widgets make that first-class:

- **`Chart`** — a multi-series strip chart: samples stream in, the trace
  scrolls left, the newest value stays pinned to the right edge.
  `Chart::sparkline()` is the chrome-free miniature for status rows.
- **`Gauge`** — a 270° radial dial: colored zone bands, tick marks, an accent
  value arc, an animated needle, and a numeric readout.

Both are ordinary retained widgets — headless, deterministic, snapshot-tested —
but they come with a damage story built for telemetry rates.

## Feeding data: `Ui::stream`

`Ui::with` is the general mutation path, and it deliberately assumes the worst:
full-bounds damage plus a relayout, because the closure may have changed the
widget's size. At 10–60 Hz per instrument that pessimism gets expensive.

`Ui::stream` is the high-rate path: the widget itself reports the cheapest
honest damage as a `StreamDamage` verdict —

```rust
// In App::update, on each new reading:
ui.stream(chart_id, |c: &mut Chart| c.push(&[temp, load]));
ui.stream(gauge_id, |g: &mut Gauge| g.update(temp));
```

- `Quiet` — nothing visible changed; the update costs nothing.
- `Shifted { extra }` — the widget recorded a scroll-blit; the next paint
  shifts its pixels in place and repaints only the exposed strip (plus the
  small `extra` seam).
- `Repaint` — repaint the widget's box, but skip the relayout.

Anything that can change a widget's *size* still goes through `Ui::with`.

## The strip-chart fast path

Phase 5's scroll-blit (`Surface::scroll_region`) moved list pixels vertically.
The chart generalizes it:

- `Surface::scroll_region_xy` shifts a region on either axis (a horizontal
  shift is a per-row overlapping `memmove`).
- `Widget::scroll_blit_xy` lets a widget shift a *sub-rect* of its box — the
  chart shifts only its plot area, never the axis gutter.

In the steady state (y-range unchanged) a pushed sample costs a `memmove` of
the plot plus re-rasterizing a few columns at the right edge, instead of
re-stroking every polyline in the plot. The invariant that governs all fbui
fast paths applies — **the fast path must never diverge from the slow one** —
and `chart_stream_blit_matches_a_full_repaint` pins the streamed frames
against a forced full repaint, frame by frame at two scales. (The comparison
allows single-pixel anti-aliasing wobble of a few code values: tiny-skia's
scanline coverage is float-accumulated, so an integer translation of a stroke
edge lying nearly tangent to a scanline can round one AA sample differently.
Any real divergence moves whole pixel runs and fails the test.)

What keeps a push an exact translation (preserve these when editing
`chart.rs`):

- the sample step is quantized to whole device pixels at layout (`placed`);
- sample x-positions are whole steps back from the plot's right edge;
- everything inside the plot is translation-invariant — uniform background,
  full-width horizontal gridlines, vertical time-gridlines phase-locked to
  absolute sample indices;
- text lives in the gutter, outside the blitted plot;
- a y-range move falls back to `Repaint` (auto-range is quantized to nice
  bounds with escape/shrink hysteresis, so it moves rarely).

## Auto-range behavior

`Chart::new()` tracks the data automatically: the band is fit to nice 1-2-5
bounds and then *held* until the data escapes it or shrinks to under ~a third
of it, so the steady state blits. Refits add ~10% headroom so a drifting
signal escapes in strides. `fixed_range(lo, hi)` pins the band — the right
choice for bounded quantities and the cheapest steady state.

## Gauges

```rust
Gauge::new(20.0, 110.0)
    .zone(80.0, GREEN).zone(95.0, AMBER).zone(110.0, RED)
    .label("°C coolant")
```

`set_value` retargets a `Tween`; the needle glides on the frame `dt` under the
normal animation contract — no wall clock, deterministic in tests, frozen (0%
CPU) when the app idles. `Gauge::update(v)` is the `Ui::stream` form, returning
`Quiet` for an unchanged, settled reading.

## Trying it

```sh
cargo run -p fbui --example telemetry --features platform   # needs a text VT
FBUI_HUD=1 cargo run -p fbui --example telemetry --features platform
```

The HUD's paint-cost readout shows the fast path directly: the big chart
streams at 10 Hz for a few milliwatts of paint, and forcing `Repaint` (resize
the window under `FBUI_BACKEND=term`, or push range escapes) shows the
difference.
