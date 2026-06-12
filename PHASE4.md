# Phase 4 — Hardening & the 0.1 release

The Phase 4 deliverable from [`PLAN.md`](PLAN.md) §4: take the Phase 0–3 stack
from "builds and tests green" to **a thing you can run on a device**. Unlike the
earlier phases this one spans all four crates rather than introducing a new one,
so its summary lives at the workspace root. The version is cut: every crate now
carries `0.1.0` off the workspace `version`.

## What's here

| Task (PLAN §4, Phase 4) | Where | Status |
|---|---|---|
| Touch gestures (tap/long-press/drag/fling) unified with pointer events | `fbui-widgets/src/gesture.rs`, `event.rs`, `fbui/src/run.rs` | ✅ done & unit-tested |
| Kinetic / fling scrolling | `fbui-widgets`: `kinetic.rs`, `widget.rs` (`animate`), `tree.rs` (`Ui::animate`), `widgets/{scroll,list}.rs` | ✅ done & tested |
| RGB565 output conversion path for small panels | `fbui-render/src/copyout.rs`, `surface.rs` | ✅ path complete (Phases 2–3); Phase 4 adds ordered dithering |
| Display hotplug & mode-change without restart | `fbui-platform`: `display/{mod,drm,fbdev}.rs`, `event_loop.rs`, `input/mod.rs`; `fbui/src/run.rs` | ✅ end-to-end path; ⏳ udev trigger + on-device verify |
| Crash-safety audit; fuzz the input parser | `fbui-platform/src/vt.rs`, `input/evdev.rs` | ✅ done & tested |
| Docs: `cargo doc` clean, running guide, CHANGELOG, versioning | `docs/`, `CHANGELOG.md`, crate docs, `.github/workflows/ci.yml` | ✅ done |
| CI matrix; publish 0.1 | `.github/workflows/ci.yml`, crate manifests | ✅ CI extended, version cut; ⏳ crates.io upload |

## Design decisions worth knowing

- **Gestures are a pure state machine.** `GestureRecognizer`
  (`fbui-widgets/src/gesture.rs`) takes one contact stream — down/move/up with a
  caller-supplied millisecond timestamp — and emits `Tap`, `LongPress`, drag, and
  `Fling`. It holds no timers and no platform types, so it's deterministic and
  unit-testable; the runner ticks `poll()` on the frame clock for long-press. The
  umbrella runner feeds it both mouse-button drags and the primary touch contact,
  which is what "unified with pointer events" means in practice — mouse and touch
  produce the same gestures.
- **Kinetic scroll rides a new `Widget::animate(dt)` hook.** A `Fling` seeds a
  shared `Kinetic` velocity that decays exponentially; `Ui::animate(dt)` walks the
  tree each frame, accumulating damage and reporting whether anything still
  coasts, so the runner only keeps the clock spinning while something moves. Idle
  ⇒ no animation ⇒ the loop sleeps, preserving the ~0% idle the whole stack is
  built around. `List` additionally gained touch drag-to-scroll (and now selects
  on a *tap*, so a drag scrolls instead of selecting).
- **RGB565 was already plumbed end to end** (fbdev detects it, `PixelFormat`
  carries it, `copy_out` converts it). Phase 4's contribution is **ordered (4×4
  Bayer) dithering** on that path — the real-world reason 16-bit panels are
  painful is gradient banding — exposed as `copy_out_dithered` / a
  `Surface::set_dither` toggle the runner flips on automatically for `Rgb565`
  displays. It's position-stable, so damaged-span copies don't shimmer.
- **The evdev parser is now fuzzable.** The packet-coalescing logic was split
  into a `PacketState` state machine (`feed_raw`) that holds no `Device` handle,
  so a deterministic test throws 300k arbitrary `(type, code, value)` tuples at it
  and asserts it never panics, overflows, or — thanks to `AbsRange::scale`
  clamping — emits an off-surface coordinate.
- **Crash safety stayed first-class.** `SIGQUIT` joined the signal set whose
  handler restores the console, and a test pins the `restore_console`
  run-at-most-once invariant that protects against a concurrent `Drop` and signal
  handler both issuing the restore ioctls.
- **Hotplug is one trait method.** `Display::reconfigure` re-reads the output and,
  on a resolution change, re-allocates DRM dumb buffers (or re-maps fbdev) and
  returns the new `DisplayInfo`; the loop polls the connector's *cached* state on
  a 1 Hz cadence (no reprobe), rescales input via `InputSource::set_surface`, and
  hands the runner an `on_display_changed` so it rebuilds its surface and
  re-lays-out the tree. No restart.

## Build & test

```sh
cargo test --workspace                              # all headless tests
cargo test -p fbui-widgets --lib                    # gesture + kinetic state machines
cargo test -p fbui-platform --lib                   # evdev fuzz + VT idempotency
cargo clippy --workspace --all-targets              # clean
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps \
  -p fbui-platform -p fbui-render -p fbui-widgets -p fbui-testkit
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p fbui --features platform

# On a real text VT (root, or video+input groups) — now with touch gestures:
cargo run -p fbui --example big_list --features platform   # flick to coast
```

See **[docs/running-on-your-device.md](docs/running-on-your-device.md)** for the
full bring-up guide (permissions, seatd/logind/root, kernel config,
troubleshooting) and **[CHANGELOG.md](CHANGELOG.md)** for the release notes and
versioning policy.

## Verification status against the Phase 4 exit criteria

> *"An external tester can take a Pi + touchscreen from a blank TTY to running the
> form example using only the docs; 0.1.0 published."*

| Exit criterion | Status |
|---|---|
| Gestures unified across mouse & touch | ✅ one `GestureRecognizer`, fed by both; 9 unit tests |
| Kinetic scrolling on real list/scroll content | ✅ `List`/`ScrollView` coast; end-to-end `Ui` test |
| RGB565 path for small panels | ✅ end-to-end, now dithered; 3 copy-out tests |
| Hotplug/mode-change without restart | ✅ full path (trait → loop → runner); ⏳ udev trigger + on-device verify need hardware |
| Every exit path restores the console; parser fuzzed | ✅ panic/signal hooks (incl. SIGQUIT), idempotency test, 300k-tuple parser fuzz |
| Docs: a tester can bring up a device from the guide | ✅ [running-on-your-device.md](docs/running-on-your-device.md); `cargo doc` is a CI gate |
| CI matrix (doc, benches, MSRV, VKMS) | ✅ added rustdoc + bench-compile gates to the existing fmt/clippy/test, MSRV, VKMS jobs |
| 0.1.0 published to crates.io | ⏳ version cut workspace-wide; the upload (flip `publish`, registry token) is a release-time action, gated like the prior phases' hardware steps |

### Gaps called out honestly (consistent with Phases 0–3)

- **On-device** verification of hotplug, multi-touch hardware, VT timing, and the
  Pi-class performance gate still needs real hardware / a non-writeback VKMS
  connector — the same caveat Phases 1–3 carried.
- **Hotplug detection** polls the connector's cached state; a udev/uevent monitor
  as the trigger is the remaining wiring (and a new dependency), deferred.
- **Multi-finger** gestures (pinch/rotate) are out of v1 scope; the recognizer
  tracks one contact (a mouse or single finger).
- **libinput's** `set_surface` rescale-on-hotplug is left to the feature-gated
  backend, not in the default/CI build.

## What Phase 5+ builds on this

The animation timeline (Phase 5) generalizes the `Widget::animate(dt)` /
`Ui::animate` hook kinetic scrolling introduced here; the scroll-blit fast path
slots into the same `List`/`ScrollView` coast. The GPU backend (Phase 6) still
swaps only the Phase 2 painter and the `Display` backend — `reconfigure`,
gestures, and kinetic scroll are all backend-agnostic.
