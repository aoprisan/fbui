# Profiling fbui

fbui can emit a `tracing` span for every phase of a frame —
**input → update → layout → paint → present** — so you can see where the time
goes and capture a flamegraph. The instrumentation is behind a Cargo feature and
costs nothing when it's off.

## Turn it on

Build with the `profile` feature (it enables `fbui-widgets/profile` too):

```sh
cargo run -p fbui --example big_list --features "platform profile"
```

The spans emitted are:

| Span | Crate | Covers |
|---|---|---|
| `input` | `fbui` (runner) | translating a platform event and dispatching it |
| `tick` | `fbui` (runner) | per-frame animation advance (kinetic, tweens) |
| `present` | `fbui` (runner) | copying the damaged spans into the scanout buffer |
| `ui.event` | `fbui-widgets` | routing one event to a widget (+ lazy layout) |
| `ui.layout` | `fbui-widgets` | a `taffy` relayout pass |
| `ui.paint` | `fbui-widgets` | the damaged-region paint walk |
| `ui.animate` | `fbui-widgets` | advancing every widget's animation |

Spans nest (`ui.layout` and `ui.paint` show up inside `ui.event`/`present`), so a
collected trace reads as a frame timeline.

## Collect a trace

Spans only do something when a `tracing` subscriber is installed. Add one in your
app's `main` (behind the same feature so a normal build stays dependency-free):

### Quick: console timings

```toml
# Cargo.toml
[dependencies]
tracing-subscriber = { version = "0.3", optional = true }

[features]
profile = ["fbui/profile", "dep:tracing-subscriber"]
```

```rust,ignore
fn main() -> fbui::Result<()> {
    #[cfg(feature = "profile")]
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
        .init();
    fbui::run(MyApp::default())
}
```

Each span prints its wall-clock duration on close — enough to spot a slow phase.

### Flamegraph

For a visual flamegraph, use [`tracing-flame`]:

```rust,ignore
use tracing_flame::FlameLayer;
use tracing_subscriber::prelude::*;

let (flame, _guard) = FlameLayer::with_file("./tracing.folded").unwrap();
tracing_subscriber::registry().with(flame).init();
```

Then fold and render with Brendan Gregg's [`inferno`]:

```sh
cargo install inferno
inferno-flamegraph < tracing.folded > frame.svg
```

Open `frame.svg`: wide bars are the expensive phases. On a CPU-rendered target
the usual shape is `ui.paint` dominating, and within it, text shaping — which is
exactly what the Phase 5 scroll-blit fast path shrinks (it skips re-rasterizing
rows that merely scrolled).

[`tracing-flame`]: https://docs.rs/tracing-flame
[`inferno`]: https://github.com/jonhoo/inferno

## What to look for

- **`ui.paint` ≫ everything** on a static screen that shouldn't be repainting →
  something is over-damaging. Check that mutations damage only what changed.
- **`ui.layout` on every frame** → a relayout is being forced each frame (a style
  that changes every tick); cache it.
- **`present` large** relative to `ui.paint` → the damage region is bigger than
  the visual change; the copy-out is bandwidth-bound on write-combined memory.

The scroll benchmark (`cargo bench -p fbui-widgets --bench scroll`) is the
regression gate for the scroll path: `scroll_blit` must stay markedly cheaper
than `scroll_full_repaint`.
