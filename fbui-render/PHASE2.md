# Phase 2 — `fbui-render`: the rendering layer

This is the Phase 2 deliverable from [`PLAN.md`](../PLAN.md) §4: a **headless**
CPU software renderer. It sits above the Phase 1 platform layer and below the
(future) Phase 3 widgets, turning drawing commands into pixels in a normal-RAM
shadow buffer and copying only the damaged spans out to a scanout buffer.

Nothing here opens a device. The core crate depends on `fbui-platform` **only**
behind the off-by-default `platform` feature — exactly the "headless by design"
constraint PLAN §4 sets — so every primitive is snapshot-testable with no
hardware in the loop.

## What's here

```
fbui-render/
├── src/
│   ├── lib.rs          umbrella + re-exports; the frame cycle in the crate docs
│   ├── surface.rs      Surface: shadow Pixmap + Scale + DamageTracker; paint() + copy-out
│   ├── painter.rs      Painter: rects, rounded rects, paths, strokes, gradients,
│   │                   rect clipping, opacity groups, image blit (all logical coords)
│   ├── path.rs         Path / PathBuilder in logical coords (+ rounded-rect arcs)
│   ├── text/
│   │   ├── mod.rs      FontContext / TextStyle / TextLayout: cosmic-text shaping,
│   │   │               swash rasterization, source-over glyph compositing
│   │   └── atlas.rs    bounded glyph cache keyed by CacheKey, LRU byte budget
│   ├── damage.rs       DamageTracker: dirty-rect merge heuristics + buffer-age unioning
│   ├── scale.rs        Scale: fractional HiDPI, logical↔device, round-outward damage
│   ├── copyout.rs      damaged-span copy to Xrgb8888 / Argb8888 / Rgb565, kernel stride
│   ├── color.rs        straight-alpha Color (↔ tiny-skia premultiplied)
│   ├── geom.rs         logical f32 geometry + integer device IRect (mirrors platform Rect)
│   ├── image.rs        Image: PNG/JPEG decode → premultiplied Pixmap
│   ├── sample.rs       the "settings page" demo scene (shared by bench + examples)
│   └── platform_glue.rs  (feature `platform`) Surface::present / copy_into_frame
├── examples/
│   ├── settings_png.rs   headless: render the sample to a PNG (+ HiDPI arg)
│   └── present.rs        (feature `platform`) drive it onto a real Display via the loop
├── benches/repaint.rs    criterion: full-frame 1080p + small-damage toggle
└── tests/
    ├── snapshots.rs      golden-image tests for every painter primitive
    ├── snapshots/*.png   the committed goldens (font-free, deterministic)
    └── text.rs           structural text tests incl. CJK + RTL

fbui-testkit/            the snapshot harness (PLAN §4 deliverable): Pixmap vs PNG
                         with per-pixel tolerance, writes .actual/.diff on mismatch
```

## Design decisions worth knowing

- **Shadow buffer, copy out damaged spans only.** The painter draws into a
  normal-RAM `tiny_skia::Pixmap`; `present_to_buffer` blits only the rows/columns
  inside each damage rect into a caller-supplied byte slice with a
  **kernel-reported stride**. This is the Phase 0 NOTES discipline (never assume
  `width * bpp`, never random-write write-combined memory) realized one layer up.
- **tiny-skia is `[R,G,B,A]`; scanout is `0xXXRRGGBB`.** Even the 32-bit path is a
  red/blue swap, not a raw `memcpy` — still sequential and damage-bounded. The
  shadow is kept opaque so premultiplied equals straight and copy-out needs no
  unpremultiply.
- **Damage carries buffer age.** `DamageTracker` merges dirty rects (fuse when
  cheap, collapse to a bounding box past a cap) *and* keeps a short ring of recent
  frames, so a double-buffer of age *N* is brought current by unioning the last
  *N* frames' damage. Age 0 ⇒ repaint everything. Idle ⇒ no damage ⇒ no present.
- **Logical coordinates, fractional scale.** Everything the painter and text take
  is logical; `Scale` applies the device transform and rounds damage rectangles
  **outward** so anti-aliased edges are never clipped out of a repaint. Glyphs are
  rasterized at `size × scale` device pixels (crisp at 2×, not upscaled).
- **Text is delegated, the atlas is ours.** cosmic-text does segmentation, bidi,
  fallback, and shaping; swash rasterizes. The one thing we add is a *bounded*
  glyph cache (cosmic-text's own is unbounded) — an LRU byte budget keyed by
  `CacheKey`, which already folds in (font, size, subpixel bin).
- **One coupling point, feature-gated.** `platform_glue` adds
  `Surface::present(&mut dyn Display)` and `copy_into_frame(&mut Frame)` — the
  former for standalone use, the latter for inside a `PlatformHandler::render`.
  Core has zero device deps without it.

## Build & test

```sh
cargo test -p fbui-render -p fbui-testkit          # 50 headless tests, no devices
cargo clippy -p fbui-render --all-targets          # clean
cargo run -p fbui-render --example settings_png -- out.png       # headless PNG
cargo run -p fbui-render --example settings_png -- out@2x.png 2  # at 2× HiDPI
cargo run -p fbui-render --example present --features platform   # on a real VT
cargo bench -p fbui-render                          # repaint benchmarks
```

Regenerate the golden snapshots after an intentional rendering change:

```sh
FBUI_UPDATE_SNAPSHOTS=1 cargo test -p fbui-render --test snapshots
```

## Verification status against the Phase 2 exit criteria

| Exit criterion | Status |
|---|---|
| Snapshot tests cover every painter primitive | ✅ `tests/snapshots.rs` covers rects, rounded rects, fill/stroke paths, linear + radial gradients, rect clipping, opacity groups, image blit — golden PNGs committed |
| CJK + RTL sample text renders correctly | ✅ `tests/text.rs` shapes & rasterizes 你好世界 and Arabic/Hebrew via cosmic-text; ink-asserted where covering fonts exist (CI installs `fonts-noto-cjk`/`fonts-noto-core`) |
| Small-damage repaint **< 5 ms** on a Pi-class CPU | ⏳ benchmark in place (`small_damage_toggle`); the absolute Pi gate needs Pi-class hardware — see numbers below for this host |
| Full frame **< 16 ms** at 1080p | ⏳ benchmark in place (`full_frame_1080p`); same caveat |
| Headless: depends on platform only behind a feature | ✅ default build links no `fbui-platform`; `platform` glue is opt-in |
| Scale-factor (integer + fractional) end to end | ✅ `Scale` unit-tested; `settings_png … 2` renders crisp 2× |

### A note on the perf gate (matching Phase 0/1's honesty)

The `< 5 ms` / `< 16 ms` numbers in PLAN §4 are specified **on a Pi-class CPU**,
which this build host is not. The benchmarks exist and run anywhere; the property
they protect — *small-damage repaint is far cheaper than a full frame*, which is
what makes CPU rendering viable on weak hardware — is visible on any host. The
absolute Pi gate is the natural next step on real ARM hardware (or a Pi runner),
exactly as Phase 0 deferred its frame-rate matrix to real targets.

Measured on this host (x86-64, release, criterion):

| Benchmark | Median |
|---|---|
| `full_frame_1080p` (whole settings page, full copy-out) | ~5.9 ms |
| `small_damage_toggle` (one toggle, age-1 copy-out) | ~17.6 µs |

So a small-damage repaint is ~**340×** cheaper than a full frame here — that ratio,
not the absolute x86 numbers, is the regression we're guarding. Even the
full-frame case already clears the 16 ms budget on this host; the open question
is only whether the small-damage case stays under 5 ms on a Pi-class CPU, which
the bench will answer there.

## What Phase 3 consumes from here

`fbui-widgets` will own a retained tree and drive this layer: lay out with
`taffy`, then on each frame `Surface::paint(|p| …)` the dirty widgets through the
[`Painter`] and measure/draw text through [`FontContext`]. Damage flows out of
the painter automatically; the widget layer only decides *what* to repaint, never
*how* pixels reach the screen. The `platform` glue is what wires a finished
`Surface` to the Phase 1 event loop.
