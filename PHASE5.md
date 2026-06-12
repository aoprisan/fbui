# Phase 5 — Performance & animation

The Phase 5 deliverable from [`PLAN.md`](PLAN.md) §4: make motion cheap. It builds
directly on the Phase 4 frame-clock hook (`Widget::animate` / `Ui::animate`),
adding a real animation API, a scroll-blit fast path, and a `tracing` profiling
story. Like Phase 4 it spans crates, so the summary lives at the root.

## What's here

| Task (PLAN §4, Phase 5) | Where | Status |
|---|---|---|
| Animations API (timeline + easing), damage-aware | `fbui-widgets`: `anim.rs`, `widgets/switch.rs`, `ctx.rs`/`tree.rs` (`request_anim`/`is_animating`) | ✅ done & tested |
| Scroll-blit fast path (move pixels, repaint the exposed strip) | `fbui-render/src/surface.rs` (`scroll_region`), `fbui-widgets`: `widget.rs` (`scroll_blit`, `Anim::damage`), `tree.rs`, `ctx.rs` (`PaintCtx::region`), `widgets/list.rs` | ✅ done, tested & benchmarked |
| Cursor overlay without widget repaint | — | ⏳ needs the DRM hardware cursor plane (hardware-gated) |
| Profiling: `tracing` spans input→update→layout→paint→present | `fbui-widgets`/`fbui` `profile` feature, `docs/profiling.md` | ✅ done |

## Design decisions worth knowing

- **Tweens are pure and damage-aware.** [`anim::Tween`](fbui-widgets/src/anim.rs)
  moves a [`Lerp`] value (`f32`, `Color`) from `from` to `to` over a duration,
  shaped by an [`Easing`], advanced by the frame `dt`. It takes a `dt`, never a
  wall clock, so it's deterministic and unit-testable. A widget owning a tween
  ticks it in [`animate`](fbui-widgets/src/widget.rs) and repaints **only
  itself**; the new [`Switch`] widget is the worked example (the thumb slides and
  the track cross-fades).
- **The Ui knows when something is animating.** A handler that starts an animation
  calls `EventCtx::request_anim`; the `Ui` flips an `is_animating` flag the runner
  checks each frame, so it walks the tree to advance animation **only while
  something moves** — idle stays ~0% CPU. Phase 4's kinetic coast was retrofitted
  onto the same flag.
- **Scroll-blit reuses pixels instead of re-rasterizing them.**
  [`Surface::scroll_region`](fbui-render/src/surface.rs) shifts a rectangle's
  pixels vertically with a sequential `memmove` and reports the exposed strip; the
  expensive part — shaping and drawing every visible row — shrinks to just the row
  band that scrolled into view. The `Ui` applies a widget's pending
  [`scroll_blit`](fbui-widgets/src/widget.rs) before the clipped paint walk, and
  [`PaintCtx::region`] lets `List` skip the rows outside the repaint region. A
  wheel/drag/kinetic scroll on a long list now repaints a strip, not the viewport.
- **Correctness is pinned by an equivalence test.** `scroll_blit_matches_a_full_repaint`
  scrolls one list via the blit path and another with a forced full repaint of the
  same offset, and asserts the two surfaces are **byte-for-byte identical** — so
  the fast path can never silently diverge from the slow one.
- **Profiling is a zero-cost feature.** Under `profile`, the runner and `Ui` emit
  nested `tracing` spans (`input`/`tick`/`present`/`ui.event`/`ui.layout`/
  `ui.paint`/`ui.animate`); with the feature off the spans compile to nothing. See
  [`docs/profiling.md`](docs/profiling.md) for capturing a flamegraph.

## Build, test & measure

```sh
cargo test -p fbui-widgets                          # anim + switch + scroll-blit equivalence
cargo test --workspace                              # all headless tests green
cargo bench -p fbui-widgets --bench scroll          # scroll_full_repaint vs scroll_blit
cargo run -p fbui --example big_list --features "platform profile"   # spans on
```

Measured on the CI-class dev host (480×800, a 5 000-row list, one scroll step):

| Benchmark | Time |
|---|---|
| `scroll_full_repaint` (re-rasterize every visible row) | ~3.97 ms |
| `scroll_blit` (shift + repaint the exposed strip) | ~2.61 ms |

A ~34% drop on x86 where the surface is small; the gap widens on a Pi-class CPU
where per-row text shaping dominates and the strip is a larger share of the saved
work. The benchmark is the regression gate: `scroll_blit` must stay the cheaper of
the two.

## Verification status against the Phase 5 exit criteria

> *"Animated transitions at refresh rate on Pi-class hardware with measured CPU
> budget documented; scrolling CPU usage drops measurably vs the Phase 3
> baseline."*

| Exit criterion | Status |
|---|---|
| Animation API (timeline + easing), damage-aware | ✅ `Easing`/`Tween`/`Lerp`, the `animate` hook, `Switch` demo; 7 anim + 1 behavior test |
| Animated transitions at refresh rate | ✅ tween + kinetic ride the frame clock and damage only themselves; ⏳ the absolute Pi-refresh figure needs ARM hardware |
| Scrolling CPU drops measurably vs baseline | ✅ scroll-blit benchmarked at ~34% here, byte-exact vs a full repaint; the ratio is the documented gate |
| Profiling story (tracing spans, flamegraph docs) | ✅ `profile` feature + [`docs/profiling.md`](docs/profiling.md) |
| Cursor overlay without widget repaint | ⏳ deferred to the DRM hardware cursor plane (needs hardware), consistent with the gated items in Phases 1–4 |

### Gaps called out honestly (consistent with Phases 0–4)

- **Cursor plane**: the software cursor still repaints through the normal path;
  the hardware-cursor-plane overlay needs a real DRM device to build and verify.
- **Pi-class absolute numbers**: the *ratio* (blit ≪ full) is gated in CI; the
  on-device refresh-rate and CPU-budget figures need ARM hardware, the same
  caveat the perf gates carried since Phase 2.
- **Scroll-blit scope**: wired into `List` (the windowed, self-painting case).
  `ScrollView` (which re-places child widgets) still does a full damaged-region
  repaint; extending the blit to it is future work.

## What later phases build on this

The GPU backend (Phase 6) swaps only the Phase 2 painter and the `Display`
backend; the animation API, `scroll_region`, and the profiling spans are all
backend-agnostic and carry over unchanged.
