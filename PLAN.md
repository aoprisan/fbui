# fbui — Plan: a framebuffer UI framework for Linux (no X11/Wayland)

Status: **plan / research only — no implementation yet**

The goal is a UI framework that draws directly to the display on Linux consoles
(TTY), embedded devices, and kiosks — no X server, no Wayland compositor.
This document surveys existing solutions, picks a language and stack, and lays
out an architecture and milestone plan.

---

## 1. Survey of existing solutions

### 1.1 Established non-Rust solutions (prior art to learn from)

| Project | Approach | Notes |
|---|---|---|
| **LVGL** (C) | fbdev + DRM backends in `lv_port_linux`, evdev input | The de-facto embedded GUI library. Software-rendered, damage-tracked, widget-rich. Best reference for a CPU-rendered widget pipeline. |
| **Qt for Embedded Linux** | `eglfs` (GPU via EGL/KMS) and `linuxfb` (software fbdev) platform plugins, evdev/libinput input | Proves the dual-path model: GPU when available, plain framebuffer otherwise. Heavyweight. |
| **SDL2/SDL3 `kmsdrm` backend** | DRM/KMS + GBM + EGL | Not a UI toolkit, but the most battle-tested "fullscreen app without a display server" plumbing. |
| **flutter-pi** | Flutter engine on DRM/KMS, no X | Shows a modern reactive toolkit can run straight on KMS. |
| **DirectFB / DirectFB2** | fbdev-era graphics + input + windowing library | Original DirectFB is dead; DirectFB2 is a revival. Cautionary tale: fbdev-centric designs age badly — build on DRM/KMS. |
| **Cage** (wlroots) | Kiosk Wayland compositor | The "don't build it" alternative: run one fullscreen Wayland app under a minimal compositor. Worth knowing, but it *is* a display server. |

### 1.2 Rust ecosystem

| Project | What it gives us | Gap |
|---|---|---|
| **Slint — LinuxKMS backend** ([docs](https://docs.slint.dev/latest/docs/slint/guide/backends-and-renderers/backend_linuxkms/)) | The most complete existing answer: renders via OpenGL/KMS, Vulkan KHR-display, **DRM dumb buffers (software)**, or legacy LinuxFB; input via libinput/libudev; seat handling via libseat (with a `noseat` variant). | It's a finished product, not a framework to build. Backend still labeled experimental. Its architecture (seat → KMS → software/GPU render paths) is the blueprint to follow. |
| **Smithay crates: [`drm-rs`](https://github.com/Smithay/drm-rs), [`gbm.rs`](https://smithay.github.io/smithay/gbm/index.html), [`input.rs`](https://github.com/Smithay/input.rs), `calloop`** | Safe, maintained bindings for DRM/KMS (incl. [dumb buffers](https://smithay.github.io/smithay/drm/buffer/index.html)), GBM, libinput, plus an event loop. Built for the Smithay compositor project, usable standalone. | Low-level only — exactly what we want for the platform layer. |
| **[easydrm](https://github.com/everything-os/easydrm)** | GLFW-like DRM/KMS+GBM+EGL wrapper: monitor discovery, page flips, atomic commits. | GPU-path only, young project. Good API reference for the display layer. |
| **`embedded-graphics`** | no_std 2D drawing primitives + `DrawTarget` abstraction. | Primitives only; no widgets, layout, or real text. |
| **`framebuffer` crate** | mmap of `/dev/fb0`. | Trivial legacy fallback; fbdev is deprecated kernel-side. |
| **egui** | Mature immediate-mode toolkit. No framebuffer backend exists ([discussion #2328](https://github.com/emilk/egui/discussions/2328)), but egui outputs tessellated triangles that *could* be software-rasterized onto a dumb buffer. | Backend would have to be written — which is most of this project anyway. |
| **iced** | Has a `tiny-skia` software renderer. | Windowing is winit-centric; winit has **no KMS backend** ([winit #1865](https://github.com/rust-windowing/winit/issues/1865), open since 2021). |
| **tiny-skia** | High-quality CPU 2D rasterizer (Skia's software pipeline ported to Rust). | Rendering only — perfect fit for our render layer. |
| **cosmic-text** | Text shaping, bidi, font fallback, layout (rustybuzz + swash + fontdb). | None — this solves the hardest UI subproblem. |
| **taffy** | Flexbox + CSS Grid layout engine (used by Bevy, Dioxus, Zed). | None — drop-in layout layer. |

### 1.3 Zig ecosystem

| Project | Notes |
|---|---|
| **[dvui](https://github.com/david-vanderson/dvui)** | Immediate-mode GUI, backend-pluggable (SDL, raylib, web). A DRM backend could be written for it, but none exists. |
| **[capy](https://capy-ui.org/)** | Wraps *native OS controls* — explicitly not for custom rendering. Not applicable. |
| **Mach** | GPU/game oriented (WebGPU). Wrong direction for a TTY/dumb-buffer target. |

Zig verdict: entirely *feasible* — Zig's C interop makes binding `libdrm`,
`libinput`, and `xkbcommon` painless, and dvui shows a viable widget model.
But you would hand-roll the platform layer **and** the text stack (FreeType +
HarfBuzz bindings, font fallback, bidi) yourself. In Rust all of that exists
as maintained pure-Rust crates.

### 1.4 Key takeaway from research

- **fbdev (`/dev/fb0`) is legacy.** Everything modern (Slint, LVGL's new port,
  SDL, flutter-pi) targets **DRM/KMS**. Dumb buffers give you software
  rendering with proper vsync via page-flips. Keep fbdev only as a fallback
  backend behind the same trait.
- Nobody in Rust offers a *framework* (as opposed to Slint's product) for this
  niche: a reusable platform layer + software renderer + widget toolkit.
  That's the gap fbui fills.

---

## 2. Decision: Rust

**Rust**, for these reasons:

1. The entire platform layer exists as maintained crates (Smithay's `drm`,
   `gbm`, `input`, `calloop`; `xkbcommon`; `evdev`; `libseat`).
2. The hardest UI subproblems — text shaping (`cosmic-text`), 2D rasterization
   (`tiny-skia`), layout (`taffy`) — are solved, pure-Rust, and embeddable.
3. Memory safety matters when you're mmap'ing kernel buffers and parsing raw
   input events as a privileged process on a seat.

Zig remains the documented alternative (Section 1.3) if the project's goal
shifts toward minimal-dependency/no_std purity, but it adds months of
plumbing work for no architectural benefit.

---

## 3. Architecture

Four layers, each its own workspace crate, each usable without the ones above it:

```
┌─────────────────────────────────────────────┐
│ fbui-widgets   widget tree, state, theming  │
├─────────────────────────────────────────────┤
│ fbui-render    tiny-skia scene painting,    │
│                cosmic-text, damage tracking │
├─────────────────────────────────────────────┤
│ fbui-platform  display backends, input,     │
│                seat/VT, event loop          │
├─────────────────────────────────────────────┤
│ Linux kernel   DRM/KMS · evdev · VT/seat    │
└─────────────────────────────────────────────┘
fbui (umbrella crate)  ·  examples/  ·  fbui-testkit
```

### 3.1 `fbui-platform`

**Display** — a `Display` trait with two initial backends:

- **`drm-dumb` (primary):** open the card via libseat (or directly when
  root/`video` group — `noseat` feature like Slint's), pick connector/CRTC/mode
  with `drm-rs`, allocate **two dumb buffers**, render → `drmModePageFlip`
  (atomic API where available) → swap on the flip event. Tear-free, vsynced.
- **`fbdev` (fallback):** mmap `/dev/fb0`, honor stride/bpp/padding, optional
  double buffer via `FBIOPAN_DISPLAY` when `yres_virtual` allows.
- *(later, optional)* **`drm-gbm-egl`** GPU path — the trait must not preclude
  it, but it is explicitly out of scope for v1.

Performance note baked into the design: dumb buffers are typically
write-combined/uncached memory. Widgets always render into a normal-RAM
**shadow buffer**; only damaged rows get `memcpy`'d into the dumb buffer.

**Input** — `input.rs` (libinput) as primary: keyboards, mice, touch,
multi-seat, hotplug via udev. Keymap translation via `xkbcommon`. A raw
`evdev` feature for systems without libinput. Output: a normalized
`InputEvent` enum (key with keysym+utf8, pointer abs/rel, touch
down/move/up, scroll).

**Session/VT** — the part everyone gets wrong, so it's first-class:
`KDSETMODE KD_GRAPHICS` + keyboard mute on the owned VT, `VT_SETMODE` with
release/acquire signals so Ctrl-Alt-Fn works (release the CRTC, stop
rendering, reacquire and force full redraw), and a panic/`SIGTERM` hook that
**always** restores `KD_TEXT` — a crashed fullscreen app must never leave the
console dead.

**Event loop** — `calloop`: one loop multiplexing the DRM fd (page-flip
events), libinput fd, timers (animation/blink), and user wakeups. Frame
pacing: render only when damaged *and* a buffer is free; otherwise sleep on
the fds (idle apps burn ~0% CPU).

### 3.2 `fbui-render`

- Target: `tiny-skia` `Pixmap` over the shadow buffer (ARGB8888 primary;
  RGB565 conversion path for small panels later).
- Painter API: rects/rounded-rects, paths, strokes, gradients, image blit
  (PNG/JPEG via `image` crate), clipping, per-widget opacity.
- **Text:** `cosmic-text` buffer per text run; glyphs rasterized via `swash`
  into an in-memory glyph atlas keyed by (font, size, subpixel offset).
- **Damage tracking:** widgets report dirty rects; renderer repaints only the
  damaged union and copies only those rows out. This is what makes CPU
  rendering viable at 1080p+ on weak hardware.
- HiDPI: integer + fractional scale factor plumbed from mode DPI / config.

### 3.3 `fbui-widgets`

- **Model: retained widget tree with explicit state** (Elm-ish:
  `update(msg) → mutate state → mark damage → paint`). Rationale: immediate
  mode (egui/dvui style) is simpler to ship but fights damage tracking —
  full-screen repaint every frame is exactly what a CPU-on-embedded target
  can't afford. Retained + damage is how LVGL wins on this hardware class.
- **Layout:** `taffy` (flexbox + grid). Widgets are taffy nodes; measure
  functions for text.
- v1 widget set: `Label`, `Button`, `Checkbox`, `Slider`, `TextInput`
  (cursor, selection, basic editing — explicitly **no IME** in v1),
  `Image`, `Row/Column/Stack`, `ScrollView` (kinetic for touch), `List`.
- Focus & keyboard navigation (Tab/arrows), pointer capture, theming via a
  simple style struct (colors, spacing, radius, font stack), light/dark.

### 3.4 Testing strategy (decided up front, it shapes the code)

- `fbui-render` is fully headless → **snapshot tests**: paint scenes into a
  `Pixmap`, compare PNGs.
- `fbui-platform` integration tests against **VKMS** (the kernel's virtual
  KMS driver) in CI, plus `evdev` uinput devices for synthetic input.
- `examples/` runnable on any real TTY or QEMU (`-vga std`) for manual checks.

---

## 4. Milestones

| # | Deliverable | Proves |
|---|---|---|
| **M0 — spike** | Binary that opens DRM, page-flips two dumb buffers showing animated color bars; clean VT restore on exit/panic; same demo on fbdev. | The risky kernel-facing plumbing, before any framework code. |
| **M1 — platform** | `fbui-platform` crate: `Display` trait (drm-dumb + fbdev), libinput→normalized events, xkbcommon keymaps, libseat + noseat, VT switching, calloop loop. Demo: cursor + keystroke echo. | Stable foundation API. |
| **M2 — renderer** | `fbui-render`: painter over tiny-skia, cosmic-text + glyph atlas, damage tracking, snapshot test harness. Demo: static "settings page" mock at <5 ms repaint on a Pi-class CPU. | Performance viability. |
| **M3 — widgets** | `fbui-widgets` + `fbui` umbrella: taffy layout, v1 widget set, focus/theme. Demo apps: counter, form, scrolling list. | The actual framework exists. |
| **M4 — hardening** | Touch gestures, RGB565, hotplug/mode-change, VKMS CI, docs + `cargo doc`, 0.1 release. | Usable by others. |
| **M5+ (backlog)** | GBM/EGL GPU backend, IME, accessibility (AccessKit), animations API, multi-display, declarative macro layer. | — |

M0 is deliberately tiny and should be the first code written: if dumb-buffer
page-flipping or VT handling surprises us on target hardware, it changes M1's
API and we want to know before the trait is designed.

---

## 5. Risks & mitigations

| Risk | Mitigation |
|---|---|
| CPU rendering too slow at high resolutions | Damage tracking from day one; shadow buffer + row-wise copy (never read/random-write WC memory); glyph atlas; M2 has an explicit perf gate. |
| VT/seat edge cases (switching, crashes, permissions) | First-class in M0/M1; libseat for logind/seatd systems, `noseat` for bare embedded; panic hook restores console unconditionally. |
| Driver quirks (no atomic API, odd strides, padded fbdev) | Legacy page-flip fallback; stride always taken from the kernel, never computed; test matrix incl. VKMS, QEMU, Pi, generic Intel. |
| Text complexity (bidi, fallback, CJK) | Delegated wholesale to cosmic-text rather than hand-rolling. IME explicitly deferred. |
| Scope creep toward "a compositor" | One process owns the screen, fullscreen only. Multi-window/multi-app is a non-goal; that's what Cage/Wayland is for. |

---

## 6. References

- Slint LinuxKMS backend docs — https://docs.slint.dev/latest/docs/slint/guide/backends-and-renderers/backend_linuxkms/
- Smithay drm-rs — https://github.com/Smithay/drm-rs (dumb buffers: https://smithay.github.io/smithay/drm/buffer/index.html)
- Smithay input.rs (libinput bindings) — https://github.com/Smithay/input.rs
- easydrm (GLFW-like DRM/KMS framework) — https://github.com/everything-os/easydrm
- winit KMS backend issue (why iced/winit apps can't do this today) — https://github.com/rust-windowing/winit/issues/1865
- egui framebuffer discussion — https://github.com/emilk/egui/discussions/2328
- LVGL Linux port (fbdev/DRM backends) — https://github.com/lvgl/lv_port_linux
- Qt for Embedded Linux (eglfs/linuxfb) — https://doc.qt.io/qt-6/embedded-linux.html
- dvui (Zig immediate-mode GUI) — https://github.com/david-vanderson/dvui
- capy (Zig native-controls toolkit) — https://capy-ui.org/
