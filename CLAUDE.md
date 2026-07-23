# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

**fbui** is a framework for drawing UIs directly to a Linux display with
**no X11 and no Wayland** — for TTYs, embedded devices, and kiosks. One process
owns the screen, fullscreen only; multi-window/compositor behavior is an
explicit non-goal. `PLAN.md` is the authoritative design doc: a survey, the
language decision (Rust), the four-layer architecture, and a strictly-ordered
multiphase plan with per-phase exit criteria. **Read the relevant phase section
of `PLAN.md` before doing architectural work** — each phase freezes the API the
next one consumes, so the plan, not convenience, dictates layering.

## Current state

Phases 0–5 are implemented; v0.1.0 is tagged. Each implemented phase has a
`PHASEn.md` recording design decisions and the verified-vs-pending status of its
exit criteria (cross-crate phases live at the repo root):

- **Phase 0** (`spikes/`) — a *throwaway* kernel-facing spike, deliberately
  outside the workspace with its own lockfile. Kept as the reference for
  DRM/dumb-buffer/VT plumbing. Don't grow it; new code goes in the framework
  crates.
- **Phase 1** (`fbui-platform/`, `PHASE1.md`) — the platform layer: display,
  input, seat, VT, event loop. Everything above it is ignorant of DRM vs fbdev.
- **Phase 2** (`fbui-render/`, `PHASE2.md`) — headless CPU renderer:
  shadow-surface painter (tiny-skia), damage tracking, text via cosmic-text
  (glyph atlas), images, RGB565 copy-out with ordered dithering, bundled fonts.
- **Phase 3** (`fbui-widgets/` + `fbui/` umbrella, `fbui-widgets/PHASE3.md`,
  `fbui-widgets/DESIGN.md`) — retained widget tree generic over an app `Msg`
  type: layout, focus, theming, and the v1 widget set (Button, Checkbox,
  RadioGroup, Switch, Slider, ProgressBar, TextInput, Label, ImageView, List,
  ScrollView, Container, Stack). `fbui-testkit/` provides golden-PNG snapshot
  testing (`FBUI_UPDATE_SNAPSHOTS=1` regenerates goldens).
- **Phase 4** (`PHASE4.md`, tagged **0.1.0**) — hardening: unified mouse/touch
  gestures (`GestureRecognizer`), kinetic scrolling, hotplug/mode-change without
  restart, evdev-parser fuzz test, crash-safety audit, device bring-up guide
  (`docs/running-on-your-device.md`), `CHANGELOG.md`.
- **Phase 5** (`PHASE5.md`) — performance & animation: `anim::Tween`/`Easing`
  damage-aware animation API, scroll-blit fast path (`Surface::scroll_region` +
  `Widget::scroll_blit`), `tracing` spans behind the `profile` feature
  (`docs/profiling.md`), cross-thread `Waker`/`Proxy`, uevent hotplug trigger.
- **Phases 6+** (GPU path, ecosystem backlog) — plan only.

Remaining known gaps are tracked honestly in each `PHASEn.md` and
`CHANGELOG.md`; most are hardware-gated (DRM cursor plane, on-device Pi-class
perf numbers, multi-finger gestures).

## Build, lint, test

The framework workspace excludes `spikes/`, so workspace commands run from the
repo root operate on the five framework crates.

```sh
cargo test --workspace        # headless unit + snapshot tests — no devices needed
cargo clippy --workspace --all-targets
cargo fmt --all -- --check    # CI enforces this; RUSTFLAGS=-D warnings in CI

# Run one test by name:
cargo test --workspace drm_vkms_present_cycle

cargo bench -p fbui-widgets --bench scroll   # scroll-blit vs full-repaint gate
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace   # CI gate

# On-device examples need a real text VT (root, or video+input groups):
cargo run -p fbui --example showcase --features platform
cargo run -p fbui-platform --example echo   # platform-layer smoke test
```

The Phase 0 spike builds separately (`cd spikes && cargo build --release`).

MSRV: `fbui-platform` is **1.76**; the render/widget stack is **1.89** (tracks
cosmic-text/image). An MSRV raise is a breaking change for the affected crate.

### Device-backed tests

`fbui-platform/tests/integration.rs` drives real (virtual) kernel devices and is
entirely `#[ignore]`d so `cargo test` stays green on any host. It only runs with
privileges against VKMS + uinput:

```sh
sudo -E cargo test -p fbui-platform --test integration -- --ignored --nocapture
# FBUI_DRM_CARD overrides the card node (default /dev/dri/card0)
```

CI (`.github/workflows/ci.yml`): a fast `check` gate (fmt, clippy, unit tests,
feature-matrix `cargo check`, rustdoc, bench-compile), an MSRV build, and a
`vkms` job that `modprobe`s the virtual drivers and runs the ignored tests as
root.

### Running on Linux from a non-Linux host

Use a Linux guest under QEMU — see `fbui-platform/docs/qemu.md` and
`scripts/qemu-test.sh` (`probe` reports hardware, `smoke` runs the device tests,
`demo` runs an example). Demos need a **real text VT** (e.g. Ctrl-Alt-F2 in the
QEMU window), not SSH/serial, because they take `KD_GRAPHICS`.

## Crate layering (dependencies point down; never skip a layer)

```
fbui           umbrella: re-exports render+widgets, app runner (`platform` feature)
fbui-widgets   retained tree, focus, theming, gestures, animation   [PHASE3/DESIGN.md]
fbui-render    headless painter, damage, text, copy-out             [PHASE2.md]
fbui-platform  Display / InputSource / Seat traits, VT, event loop  [PHASE1.md]
fbui-testkit   golden-PNG snapshot harness (dev-dependency only)
```

- **`fbui-platform/src/`** — `display/` (`drm.rs` primary, `fbdev.rs` fallback),
  `input/` (`evdev.rs` default, `libinput.rs` feature, `keymap.rs`), `seat/`
  (`noseat.rs`, `libseat.rs`), `vt.rs` (`VtGuard` — restore on every exit path),
  `term/` (terminal backend: kitty-graphics/half-block display + ANSI input,
  `FBUI_BACKEND=term`, see `docs/terminal-backend.md`), `event_loop.rs`
  (calloop; apps implement `PlatformHandler`), `uevent.rs`
  (netlink hotplug trigger). Backends are chosen at **runtime** with fallback
  (DRM→fbdev→terminal, libinput→evdev, libseat→noseat); see the `open_*`
  functions in `lib.rs`. When adding a backend, keep this pattern: feature-gate
  the impl, box it behind the trait, and fall back gracefully.
- **`fbui-render/src/`** — `surface.rs` (shadow buffer + damage + `copy_out` +
  `scroll_region`), `painter.rs`, `text/` (cosmic-text + glyph atlas),
  `copyout.rs` (XRGB/RGB565+dither), `platform_glue.rs` (the only
  render↔platform coupling, behind the `platform` feature).
- **`fbui-widgets/src/`** — `tree.rs` (`Ui`: event→update→layout→paint→animate),
  `widget.rs` (the `Widget<Msg>` trait — `measure`/`paint`/`event`/`animate`/
  `scroll_blit`), `ctx.rs`, `gesture.rs`, `kinetic.rs`, `anim.rs`, `theme.rs`,
  `widgets/*`. Widgets are headless and deterministic; tests live in
  `tests/behavior.rs` and `tests/snapshot.rs`.
- **`fbui/src/run.rs`** — the app runner: implements `PlatformHandler`, owns the
  frame clock, feeds gestures, composites the software cursor, handles
  `on_display_changed`, and delivers `Proxy<Msg>` messages from worker threads.

## Feature flags

`fbui` (umbrella): headless by default; `platform` pulls in fbui-platform and
the runner (examples require it), `bundled-font` compiles in Inter (~300 KB),
`profile` emits `tracing` spans, `remote` adds the remote console (an embedded
HTTP server — live screen view, input injection, widget-tree inspector,
Prometheus metrics — activated by `FBUI_REMOTE`; see `docs/remote-console.md`;
the module is headless-testable, the runner wiring needs `platform`).

`fbui-platform`: the **default set is everything that builds with no system C
libraries**: `drm-backend fbdev evdev noseat event-loop term`. The C-library backends
(`libinput`, `xkbcommon`, `libseat`) are gated off and **have not been built or
run in the dev environment** (the box lacks the libs) — treat them as
written-against-docs and validate on a host with the headers installed.

## Invariants that shape the code (do not violate)

These come from the Phase 0 spike's hardware findings and recur throughout:

- **Stride is never computed.** Always use `Frame::stride` (the kernel-reported
  `pitch`/`line_length`), never `width * bpp`.
- **Write the back buffer forward only.** It may be write-combined/uncached
  device memory where scattered sub-word writes are catastrophic. The render
  layer keeps a normal-RAM **shadow buffer** and copies whole damaged rows out;
  `Frame` is shaped to encourage sequential writes.
- **The console is restored on *every* exit path** — `Drop`, `panic!`, and fatal
  signals — back to text mode and `VT_AUTO`. A crashed fullscreen app must never
  leave the console dead. (`kill -9` is the one uncatchable case; `panic =
  "abort"` in release is safe because the restore is a panic *hook*, not an
  unwinding `Drop`.)
- **Idle burns ~0% CPU**: render only when there's damage *and* a buffer is
  free; otherwise block in `poll` on the fds. Animation follows the same rule:
  the runner walks `Ui::animate` only while `is_animating` — never tick a wall
  clock. Animations take the frame `dt`, so they stay deterministic and
  unit-testable.
- **The fast path must never diverge from the slow one.** Scroll-blit is pinned
  byte-for-byte against a full repaint (`scroll_blit_matches_a_full_repaint`);
  any new fast path needs the same equivalence test.
- `Frame::age` is the EGL-style buffer-age hint for correct partial redraw under
  double buffering (`0` = repaint everything).

## Conventions

- Widgets hold no platform types and no wall clock — pure state machines fed
  events, `dt`, and paint contexts, so everything is testable headless.
- Snapshot tests: tolerant compare (`fbui-testkit`); on intentional visual
  change run with `FBUI_UPDATE_SNAPSHOTS=1`, review the PNG, commit it.
- Workspace crates version in **lockstep** off `workspace.version`; changelog
  follows Keep a Changelog (see `CHANGELOG.md` for the pre-1.0 semver policy).
