# Phase 1 — `fbui-platform`: the platform layer

This is the Phase 1 deliverable from [`PLAN.md`](../PLAN.md) §4: the stable
foundation API. **Everything above it is ignorant of DRM vs fbdev.** It takes the
proven plumbing from the Phase 0 spike (`spikes/`) and turns it into four
trait-shaped subsystems plus an event loop and a demo.

## What's here

```
fbui-platform/
├── src/
│   ├── lib.rs            Platform / PlatformConfig umbrella + backend selection
│   ├── display/
│   │   ├── mod.rs        the `Display` trait — begin_frame → present, stride + buffer age
│   │   ├── drm.rs        drm-dumb backend (primary, vsynced page flips, master suspend/resume)
│   │   └── fbdev.rs      legacy /dev/fb0 fallback (pan-flip double buffering)
│   ├── input/
│   │   ├── mod.rs        normalized `InputEvent` enum + `InputSource` trait
│   │   ├── evdev.rs      raw-evdev backend (pure Rust, default)
│   │   ├── libinput.rs   libinput backend (feature `libinput`)
│   │   └── keymap.rs     evdev keycode → keysym + UTF-8 (xkbcommon or built-in US)
│   ├── seat/
│   │   ├── mod.rs        `Seat` trait + `SessionEvent`
│   │   ├── noseat.rs     direct-open (root / video+input groups), default
│   │   └── libseat.rs    logind/seatd brokered (feature `libseat`)
│   ├── vt.rs             console guard (graphics mode + restore-on-every-exit) + cooperative switching
│   ├── event_loop.rs     calloop loop multiplexing display/input/vt/seat fds + `PlatformHandler`
│   ├── cursor.rs         minimal software cursor (for the demo / bring-up)
│   ├── geom.rs           Size / Point / Rect (+ damage union/intersect)
│   ├── format.rs         PixelFormat (XRGB8888 / ARGB8888 / RGB565)
│   └── ioctl.rs          raw KD/VT/FB ioctl ABI (extended from the spike)
├── examples/echo.rs      software cursor + keystroke echo (the exit-criterion demo)
└── tests/integration.rs  VKMS present cycle + uinput keystroke (#[ignore], CI-only)
```

## Design decisions worth knowing

- **`Display::begin_frame` hands out the real back buffer + kernel stride + a
  buffer-age hint.** Stride is never computed (Phase 0 NOTES); age is the
  EGL-style "presents since this buffer last held valid contents", which is what
  makes partial redraw correct under double buffering. The render layer keeps the
  shadow and does the whole-row copy-out — the platform only hands out the
  mapping shaped to encourage sequential writes.
- **`present` is fire-and-forget; completion arrives on `present_fd`.** DRM
  returns its card fd (page-flip events); fbdev returns `None` and the loop paces
  it with a timer. The loop renders only when there's damage *and* a buffer is
  free, so idle = blocked in `poll` at ~0% CPU.
- **VT/session is split cleanly.** With `noseat`, `vt.rs` mediates Ctrl-Alt-Fn
  itself (`VT_PROCESS` + async-signal-safe self-pipe → `VtEvent`); with
  `libseat`, the manager does it and we react to `SessionEvent`. Both funnel to
  `Display::suspend`/`resume` (drop/re-acquire master, force full repaint).
- **The console is restored on *every* exit path** — drop, `panic!`, and fatal
  signals — carried over verbatim from the Phase 0 spike, now also returning the
  VT to `VT_AUTO`.

## Build & test

```sh
cargo test --workspace          # 17 headless unit tests, no devices needed
cargo clippy --workspace        # clean
cargo run -p fbui-platform --example echo   # from a real text VT, as root
```

Default features are the pure-Rust set (`drm-backend fbdev evdev noseat
event-loop`); they build and test anywhere. See below for the C-library backends.

To exercise the real DRM/input/VT paths on a non-Linux host (e.g. **QEMU on
macOS**) or any Linux VM, see [`docs/qemu.md`](docs/qemu.md) and the
`scripts/qemu-test.sh` helper (`smoke` runs the device integration tests against
VKMS; `demo` runs the echo example).

## Feature flags

| Feature | Default | Needs | Provides |
|---|---|---|---|
| `drm-backend` | ✅ | — (pure Rust `drm`) | DRM/KMS dumb-buffer display |
| `fbdev` | ✅ | — | legacy `/dev/fb0` display |
| `evdev` | ✅ | — (pure Rust `evdev`) | raw input |
| `noseat` | ✅ | — | direct device open |
| `event-loop` | ✅ | — (pure Rust `calloop`) | `Platform::run` |
| `libinput` | — | system **libinput** | accelerated input, gestures, hotplug |
| `xkbcommon` | — | system **libxkbcommon** | real keyboard layouts / compose |
| `libseat` | — | system **libseat** | logind/seatd unprivileged sessions |

## Verification status against the Phase 1 exit criteria

The build environment for this work has **no DRM card, no `/dev/uinput`, and none
of the libinput/libseat/xkbcommon system libraries**, so — exactly as Phase 0
did with its `TO MEASURE` matrix — the criteria split into *verified here* and
*pending hardware/CI*:

| Exit criterion | Status |
|---|---|
| Demo shows a software cursor and echoes keystrokes | ✅ code-complete (`examples/echo.rs`); runs on a real VT |
| VT switching away/back works repeatedly without artifacts | ⏳ implemented (`vt.rs` + `Display::suspend/resume`); needs hardware to validate |
| Runs as root (`noseat`) and as an unprivileged seat user (`libseat`) | ⏳ `noseat` complete; `libseat` implemented behind its feature, **unbuilt here** (no system lib) |
| CI green on VKMS | ⏳ harness + workflow in place (`tests/integration.rs`, `.github/workflows/ci.yml`); runs once on a VKMS-capable runner |
| Headless logic (geom/format/keymap/age) | ✅ 17 unit tests pass |

### Caveat on the C-library backends

`libinput.rs`, `libseat.rs`, and the xkbcommon path in `keymap.rs` link system
libraries that aren't present in this environment, so they are **compiled only
behind their features and have not been built or run here**. They are written
against the documented crate APIs and kept deliberately small; treat them as
*to be validated on a host that provides the libraries* (a CI job with
`libinput-dev libseat-dev libxkbcommon-dev` installed is the natural next step).
The pure-Rust `evdev` + `noseat` path is the verified default and is what the
integration tests exercise.

## What Phase 2 consumes from here

`fbui-render` depends on `fbui-platform` only for `Display` (the back buffer it
copies its shadow into) and `Frame::{stride, age, format}`. The `InputEvent`
stream and the `PlatformHandler` loop are what `fbui-widgets` will drive in
Phase 3. None of those consumers see DRM or fbdev — which was the whole point.
