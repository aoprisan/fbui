# Changelog

All notable changes to **fbui** are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project aims to
follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Versioning policy

While the framework is pre-1.0 (`0.y.z`), a bump of **`y`** may carry breaking
API changes and a bump of **`z`** is reserved for backwards-compatible fixes and
additions. The workspace crates (`fbui`, `fbui-widgets`, `fbui-render`,
`fbui-platform`, `fbui-testkit`) are versioned **in lockstep** off the workspace
`version`, so a given `fbui` release pins exactly the sibling versions it was
built against. The MSRV is part of the contract: `fbui-platform` builds on Rust
**1.76**; the render/widget stack tracks its heavier dependencies (cosmic-text,
image) at **1.89**. An MSRV raise is a breaking change for the affected crate.

## [Unreleased]

### Phase 6: optional GPU path (in progress)

#### Added

- **Runtime display-backend selection**: a `Backend` preference
  (`Auto`/`DrmDumb`/`Fbdev`/`Gpu`) on `PlatformConfig`, also read from the
  `FBUI_BACKEND` environment variable, with a unit-tested `Backend::order`
  fallback policy. `Auto` keeps the software path the default and only it falls
  back; an explicit choice fails loudly. `open_display` walks the order and
  reports which backends are compiled in.

#### Changed

- `PlatformConfig.prefer_fbdev: bool` is replaced by `backend: Backend` (set
  `Backend::Fbdev` for the old `prefer_fbdev = true`).

#### Designed / gated

- The `drm-gbm-egl` GPU backend and GPU painter are specified in
  [`PHASE6.md`](PHASE6.md), including the one additive `Display`-trait extension
  (`FrameTarget`) the GPU path needs. The implementation links `libgbm`/`libEGL`
  and needs a GPU/EGL host, so it's gated like the DRM/libinput/libseat backends.

### Phase 5: performance & animation

#### Added

- **Animation API** (`fbui-widgets::anim`): `Easing` curves, a `Lerp` trait
  (`f32`, `Color`), and a `Tween<T>` advanced by the frame `dt` — pure and
  damage-aware. A new animated `Switch` widget is the worked example.
- **`Widget::animate` / `Ui` plumbing**: `EventCtx::request_anim` and
  `Ui::is_animating` so the runner advances animation only while something moves;
  Phase 4's kinetic coast now rides the same flag.
- **Scroll-blit fast path**: `Surface::scroll_region` shifts a region's pixels in
  place and reports the exposed strip; a new `Widget::scroll_blit` hook +
  `Anim::damage` + `PaintCtx::region` let `List` repaint only the strip that
  scrolled into view instead of the whole viewport. Benchmarked (~34% here) and
  pinned byte-for-byte against a full repaint.
- **`tracing` profiling** (`profile` feature on `fbui` / `fbui-widgets`): nested
  spans through input → update → layout → paint → present, with a flamegraph
  guide in [`docs/profiling.md`](docs/profiling.md). Zero-cost when off.
- A `scroll` benchmark (`fbui-widgets`) and CI gates for it and the `profile`
  feature.

#### Known gaps

- The DRM hardware **cursor-plane** overlay (cursor move without a widget
  repaint) is deferred — it needs a real DRM device.
- Scroll-blit is wired into `List`; `ScrollView` still full-repaints its region.
- On-device Pi-class refresh-rate / CPU-budget figures need ARM hardware; the
  blit-vs-full *ratio* is the CI gate.

## [0.1.0] — Phase 4: hardening & first release

The first tagged release. Phases 0–3 built the stack — kernel-facing spike,
platform layer, headless renderer, widget toolkit — and Phase 4 hardens it for
real devices.

### Added

- **Touch & pointer gestures** (`fbui-widgets`): a headless, deterministic
  `GestureRecognizer` that turns one contact stream (mouse or single-finger
  touch, unified) into `Tap`, `LongPress`, drag, and `Fling` gestures. New
  `Event::Tap` / `Event::LongPress` / `Event::Fling` variants carry them into
  the widget tree.
- **Kinetic ("flick to coast") scrolling**: `ScrollView` and `List` consume
  `Fling` and decay it through the new `Widget::animate(dt)` hook and
  `Ui::animate(dt)` frame-clock walk. `List` also gained touch drag-to-scroll
  (selecting only on a tap, so a drag scrolls instead).
- **RGB565 ordered dithering** (`fbui-render`): `copy_out_dithered` / a
  `Surface::set_dither` toggle apply 4×4 Bayer dithering on the 16-bit copy-out
  to suppress gradient banding on small panels. The runner enables it
  automatically when the display reports `Rgb565`.
- **Display hotplug / mode-change handling**: `Display::reconfigure` (implemented
  for the DRM and fbdev backends), a `PlatformHandler::on_display_changed` hook,
  and `InputSource::set_surface` rescaling. The event loop polls the connector's
  cached state on a low cadence; the runner rebuilds its surface and re-lays-out
  the tree on a change.

### Changed

- The raw-evdev parser is split into a pure `PacketState` state machine
  (`feed_raw`) with no `Device` handle, so it can be exercised directly.
- `AbsRange::scale` now clamps to the surface, so a device reporting
  out-of-range absolute values can never produce an off-surface coordinate.
- `List` selects on pointer **release** (a tap) rather than press, so a drag
  scrolls instead of selecting.

### Hardened

- Crash safety: `SIGQUIT` joins the signals whose handler restores the console;
  a regression test pins `restore_console`'s run-at-most-once idempotency.
- A deterministic fuzz test throws 300k arbitrary `(type, code, value)` tuples
  at the evdev parser and asserts it never panics, overflows, or emits an
  off-surface coordinate — the Phase 4 "fuzz the input parser" task, in plain
  `cargo test`.

### Docs & CI

- A "[running on your device](docs/running-on-your-device.md)" guide (permissions,
  seatd vs logind vs root, kernel config, troubleshooting), this changelog, and a
  versioning policy.
- CI gained a `cargo doc` (warnings-as-errors) gate and a benchmark-compile gate
  alongside the existing fmt/clippy/test, MSRV, and VKMS jobs.

### Known gaps (hardware / dependency gated, consistent with Phases 0–3)

- On-device verification of VT switching, DRM page-flip timing, multi-touch
  hardware input, and the Pi-class performance gates still needs real hardware /
  a VKMS CI runner with a non-writeback connector.
- Hotplug detection currently polls the connector's cached state; wiring a
  udev/uevent monitor as the trigger is the remaining on-device step.
- Publishing to crates.io is deferred: the crates carry `version = 0.1.0` and a
  lockstep policy, but the upload (flipping `publish`, a registry token) is a
  release-time action, not done from CI here.
- libinput's `set_surface` rescale-on-hotplug override is left to the
  feature-gated backend (not in the default/CI build).

[Unreleased]: https://github.com/aoprisan/fbui/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/aoprisan/fbui/releases/tag/v0.1.0
