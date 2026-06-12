# Phase 3 — `fbui-widgets` + `fbui`: the widget toolkit

The Phase 3 deliverable from [`PLAN.md`](../PLAN.md) §4: the framework proper. A
**retained** widget tree with an Elm-ish control loop — `update(msg) → mutate
state → mark damage → paint` — laid out by [`taffy`](https://docs.rs/taffy) and
drawn through the Phase 2 [`Painter`]. The design was fixed first in
[`DESIGN.md`](DESIGN.md) (PLAN makes that the phase's first task); this file is
the build summary.

`fbui-widgets` is **headless**: a `Ui` runs and paints into a
`fbui_render::Surface` with no device in the loop, which is what makes widgets
snapshot- and behavior-testable. The umbrella **`fbui`** crate re-exports the
render + widget layers and, behind the `platform` feature, adds the runner that
drives a `Ui` on a real DRM/fbdev display through Phase 1.

## What's here

```
fbui-widgets/
├── DESIGN.md           the reviewed design (model, damage rules, input/focus)
├── src/
│   ├── tree.rs         Ui: the retained tree — build, layout (taffy), event
│   │                   routing, focus, and the damaged-region paint walk
│   ├── widget.rs       the object-safe Widget<Msg> trait
│   ├── ctx.rs          EventCtx / PaintCtx — the request sinks widgets talk to
│   ├── event.rs        logical Event / Key / Modifiers / PointerButton
│   ├── style.rs        thin taffy::Style helpers + layout↔Rect conversions
│   ├── theme.rs        Theme: palette + metrics + font, light/dark
│   ├── util.rs         shared paint helpers (focus ring, text style, darken)
│   └── widgets/        Label, Button, Checkbox, Slider, TextInput,
│                       Container (Row/Column), ScrollView, List, ImageView
├── examples/gallery_png.rs   headless: render a widget gallery to a PNG
└── tests/
    ├── behavior.rs           layout, click→message, focus/tab, list select, damage
    ├── snapshot.rs           a text-free golden (deterministic across hosts)
    └── snapshots/*.png       the committed golden

fbui/  (umbrella)
├── src/lib.rs          re-exports render + widgets; flattens common names
├── src/run.rs          (feature `platform`) App trait + run(): input translation,
│                       update loop, present — the only code that knows both halves
└── examples/           counter, form, big_list  (feature `platform`)
```

## Design decisions worth knowing

- **Retained tree, generic over `Msg`.** Widgets are long-lived objects that own
  their state; the tree is `SlotMap<WidgetId, Node>` mirrored 1:1 onto a taffy
  tree. `Ui<Msg>` / `Widget<Msg>` are parameterized by the app's message type, so
  a `Button<Msg>` emits `Msg` and the app's `update(msg, ui)` pushes new state
  back into widgets via [`Ui::with`] — one-directional data flow, no `RefCell`.
- **Widgets never touch the `Ui`.** They get an [`EventCtx`]/[`PaintCtx`] exposing
  their own bounds + theme + fonts and a set of *request* sinks (emit a message,
  request paint/layout/focus/capture). The Ui applies the requests after the
  widget returns. The borrow trick throughout is to **destructure `&mut self`**
  into disjoint field references so a walk holds `&mut nodes` and `&mut fonts` at
  once (`DESIGN.md` §3).
- **Damage-bounded repaint.** Every mutation/event records a logical dirty rect;
  paint repaints the *union region* (clipped), skipping subtrees that don't
  intersect it, so a toggled checkbox repaints a checkbox-sized span, not the
  screen. Phase 2's tracker bounds the copy-out from there. Idle ⇒ no damage ⇒ no
  paint ⇒ ~0% CPU.
- **Layout is taffy.** Each widget contributes a `taffy::Style`; leaves with
  intrinsic size (text, image) register a measure function that shapes through
  `FontContext`. Scroll viewports use taffy `overflow: scroll` and are fed their
  content/viewport extents after layout so they can clamp without tree access.
- **One coupling point.** Only `fbui::run` (feature-gated) knows both the platform
  and the widgets; it translates physical `InputEvent`s into logical `Event`s,
  tracks the pointer position (the platform tracks none), and presents.

## Build & test

```sh
cargo test -p fbui-widgets                    # 9 behavior + 1 snapshot + doctest
cargo clippy --workspace --all-targets        # clean
cargo run -p fbui-widgets --example gallery_png -- out.png   # headless render

# On a real text VT (as root, or video+input groups):
cargo run -p fbui --example counter  --features platform
cargo run -p fbui --example form     --features platform
cargo run -p fbui --example big_list --features platform     # 10,000-row list
```

Regenerate the widget golden after an intentional rendering change:

```sh
FBUI_UPDATE_SNAPSHOTS=1 cargo test -p fbui-widgets --test snapshot
```

## Verification status against the Phase 3 exit criteria

| Exit criterion | Status |
|---|---|
| Design doc reviewed before code | ✅ [`DESIGN.md`](DESIGN.md) is the phase's first artifact |
| The v1 widgets exist and compose | ✅ Label, Button, Checkbox, Slider, TextInput, Container (Row/Column), ScrollView, List, ImageView — see `gallery_png` |
| Widget snapshot tests pass | ✅ a deterministic text-free golden (`tests/snapshot.rs`); plus 9 behavioral tests (layout, click→message, tab focus, keyboard activation, list selection, damage) |
| 10k-row list scrolls at refresh on Pi-class HW | ⏳ windowed `List` paints only visible rows (`big_list` example, 10 000 rows); the absolute Pi refresh gate needs ARM hardware |
| All three examples run from a TTY with kbd/mouse/touch | ⏳ `counter`/`form`/`big_list` build against the runner; on-device run needs a real VT (same hardware caveat as Phases 1–2) |
| Dogfood: a Wi-Fi settings panel | ⏳ the Phase 2 `settings_png` scene is the static dogfood; a live widget version is the natural next check on hardware |

### v1 gaps, called out honestly (consistent with Phases 0–2)

- **`Stack`** (overlapping children) is deferred — Row/Column cover the layouts
  the examples need; `DESIGN.md` §7 notes the approach.
- **Kinetic/fling scrolling** and **touch gesture recognition** (tap/long-press)
  are Phase 4 per PLAN; `ScrollView`/`List` do wheel + drag now.
- **`TextInput`** is single-line, cursor + selection + basic editing, **no IME /
  clipboard** (explicitly v1 scope). Caret hit-testing measures substring widths.
- The Pi-class **performance** and **on-device input** criteria await ARM
  hardware / a real VT, exactly as Phase 1's VT/DRM and Phase 2's perf gate did.

## What Phase 4+ builds on this

Animation (Phase 5) hangs off `request_paint` on a frame clock; the scroll-blit
and opaque-damage fast paths slot into the paint walk and `ScrollView`; the GPU
path (Phase 6) swaps the Phase 2 painter without touching this layer. Nothing
here assumes software rendering or a particular display — widgets speak only
`Painter`, `Event`, and damage.

[`Painter`]: fbui_render::Painter
[`Ui::with`]: crate::Ui::with
[`EventCtx`]: crate::EventCtx
[`PaintCtx`]: crate::PaintCtx
