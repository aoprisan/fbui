# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

**fbui** is a framework (in progress) for drawing UIs directly to a Linux
display with **no X11 and no Wayland** — for TTYs, embedded devices, and kiosks.
One process owns the screen, fullscreen only; multi-window/compositor behavior is
an explicit non-goal. `PLAN.md` is the authoritative design doc: a survey,
the language decision (Rust), the four-layer architecture, and a strictly-ordered
multiphase plan with per-phase exit criteria. **Read the relevant phase section of
`PLAN.md` before doing architectural work** — each phase freezes the API the next
one consumes, so the plan, not convenience, dictates layering.

## Current state

- **Phase 0** (`spikes/`) — a *throwaway* kernel-facing spike, deliberately
  outside the workspace with its own lockfile. Kept as the reference for
  DRM/dumb-buffer/VT plumbing. Don't grow it; new code goes in the framework
  crates.
- **Phase 1** (`fbui-platform/`) — **implemented**: the platform layer. Builds
  and tests green on pure-Rust backends.
- **Phases 2+** (`fbui-render`, `fbui-widgets`, `fbui` umbrella) — plan only,
  not yet created.

## Build, lint, test

The framework workspace excludes `spikes/`, so workspace commands run from the
repo root operate on `fbui-platform` only.

```sh
cargo test --workspace        # headless unit tests — no devices needed, run anywhere
cargo clippy --workspace --all-targets
cargo fmt --all -- --check    # CI enforces this; RUSTFLAGS=-D warnings in CI

# Run one test by name:
cargo test --workspace drm_vkms_present_cycle

cargo run -p fbui-platform --example echo   # software cursor + keystroke echo; needs a real VT, as root
```

The Phase 0 spike builds separately:

```sh
cd spikes && cargo build --release          # then run on a real VT — see spikes/README.md
```

### Device-backed tests

`fbui-platform/tests/integration.rs` drives real (virtual) kernel devices and is
entirely `#[ignore]`d so `cargo test` stays green on any host. It only runs with
privileges against VKMS + uinput:

```sh
sudo -E cargo test -p fbui-platform --test integration -- --ignored --nocapture
# FBUI_DRM_CARD overrides the card node (default /dev/dri/card0)
```

CI (`.github/workflows/ci.yml`) has three jobs: a fast `check` gate (fmt, clippy,
unit tests, **feature-matrix `cargo check` builds**), an **MSRV 1.76** build, and
a `vkms` job that `modprobe`s the virtual drivers and runs the ignored tests as
root.

## Running on Linux from a non-Linux host

`fbui-platform` is Linux-only (DRM/KMS, fbdev, evdev, VT ioctls). To exercise it
from macOS or any host, use a Linux guest under QEMU — see
`fbui-platform/docs/qemu.md` and the `scripts/qemu-test.sh` helper
(`probe` reports detected hardware, `smoke` loads vkms+uinput and runs the device
tests, `demo` runs the echo example). The demo needs a **real text VT** (e.g.
Ctrl-Alt-F2 in the QEMU window), not SSH/serial, because it takes `KD_GRAPHICS`.

## Feature flags (fbui-platform)

The **default set is everything that builds with no system C libraries**:
`drm-backend fbdev evdev noseat event-loop`. The C-library backends are gated
off and **have not been built or run in the dev environment** (the box lacks the
libs) — treat them as written-against-docs and validate on a host with the
headers installed:

| Feature | Default | Needs system lib | Provides |
|---|---|---|---|
| `drm-backend`, `fbdev` | ✅ | no | DRM dumb-buffer / legacy fbdev display |
| `evdev` | ✅ | no | raw input (pure Rust) |
| `noseat` | ✅ | no | direct device open (root / `video`+`input`) |
| `event-loop` | ✅ | no | `Platform::run` (calloop) |
| `libinput` | — | libinput | accelerated input, hotplug, gestures |
| `xkbcommon` | — | libxkbcommon | real keymaps/layouts (else built-in US-QWERTY) |
| `libseat` | — | libseat | logind/seatd unprivileged sessions |

Backends are chosen at **runtime** with fallback (DRM→fbdev, libinput→evdev,
libseat→noseat); see the `open_*` functions in `fbui-platform/src/lib.rs`. When
adding a backend, keep this pattern: feature-gate the impl, box it behind the
trait, and fall back gracefully.

## Architecture of the platform layer

The whole point of `fbui-platform` is that **everything above it is ignorant of
DRM vs fbdev**. Four trait-shaped subsystems plus an event loop, all in
`fbui-platform/src/`:

- **`display/`** — the `Display` trait: `begin_frame()` → mapped back buffer →
  `present(damage)`. Two backends: `drm.rs` (primary, vsynced page flips, DRM
  master suspend/resume) and `fbdev.rs` (fallback, pan-flip double buffering).
- **`input/`** — a normalized `InputEvent` enum + `InputSource` trait. `evdev.rs`
  (default), `libinput.rs` (feature), `keymap.rs` (keycode→keysym+UTF-8).
- **`seat/`** — `Seat` trait + `SessionEvent`. `noseat.rs` (direct open) or
  `libseat.rs` (brokered).
- **`vt.rs`** — `VtGuard`: graphics mode + keyboard mute, **restore on every exit
  path**, and cooperative VT switching.
- **`event_loop.rs`** — calloop loop multiplexing display/input/vt/seat fds;
  apps implement `PlatformHandler`. `lib.rs` assembles it all (`Platform`,
  `PlatformConfig`).

### Invariants that shape the code (do not violate)

These come from the Phase 0 spike's hardware findings and recur throughout:

- **Stride is never computed.** Always use `Frame::stride` (the kernel-reported
  `pitch`/`line_length`), never `width * bpp`.
- **Write the back buffer forward only.** It may be write-combined/uncached
  device memory where scattered sub-word writes are catastrophic. The render
  layer keeps a normal-RAM **shadow buffer** and copies whole damaged rows out;
  `Frame` is shaped to encourage sequential writes.
- **The console is restored on *every* exit path** — `Drop`, `panic!`, and fatal
  signals — back to text mode and `VT_AUTO`. A crashed fullscreen app must never
  leave the console dead. (`kill -9` is the one uncatchable case.)
- **Idle burns ~0% CPU**: render only when there's damage *and* a buffer is free;
  otherwise block in `poll` on the fds.
- `Frame::age` is the EGL-style buffer-age hint for correct partial redraw under
  double buffering (`0` = repaint everything).

When a phase is implemented it gets a `PHASEn.md` next to its crate
(`fbui-platform/PHASE1.md`) recording design decisions and the verified-vs-pending
status of its exit criteria.
