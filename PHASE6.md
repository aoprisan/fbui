# Phase 6 — Optional GPU path

The Phase 6 deliverable from [`PLAN.md`](PLAN.md) §4: an **optional** GPU display
backend (DRM + GBM + EGL) and GPU painter, behind the same traits, with the
**software path remaining the default and fully supported**. The plan is explicit
that this comes late on purpose — *"a GPU path added early tends to become the
only well-tested path, and this framework's reason to exist is excellent software
rendering."*

This phase is, by nature, GPU/EGL-hardware work: the backend links `libgbm`,
`libEGL`, and a GL/GLES driver, and can only be exercised on a host with a GPU
and a KMS-capable DRM device. It therefore splits, like every prior phase split
its hardware parts, into **what's landed and verifiable now** and **what's gated
on a GPU host**.

## What's landed (verifiable, in this build)

**Runtime backend selection** — the seam a GPU backend plugs into.

- A [`Backend`](fbui-platform/src/backend.rs) preference (`Auto` / `DrmDumb` /
  `Fbdev` / `Gpu`), chosen from `PlatformConfig` **or the `FBUI_BACKEND`
  environment variable**, so the backend is picked at runtime without
  recompiling — the exit criterion's "selected at runtime."
- `Backend::order()` is a pure function (unit-tested): `Auto` tries DRM dumb
  buffers then fbdev; an **explicit** choice yields exactly one attempt, so a
  misconfiguration fails loudly instead of silently degrading. The **software
  path is the default** (`Auto` never picks GPU implicitly).
- `open_display` walks that order, skipping backends not compiled in and
  surfacing the right error. `Backend::Gpu` returns a clear "requires the `gpu`
  feature and a GPU/EGL host" error today — the reserved slot the implementation
  below fills.

```sh
FBUI_BACKEND=fbdev cargo run -p fbui --example form --features platform   # force software fbdev
FBUI_BACKEND=gpu   cargo run -p fbui --example form --features platform   # -> clear "not built" error
```

This is the part that proves the architecture admits a GPU backend without
disturbing the software stack: selection, fallback, and diagnostics are real and
tested; nothing above the platform layer changed.

## The validation finding (why the trait needs one extension)

Task 1's real purpose is to *check the Phase 1 trait didn't bake in software-only
assumptions.* Walking the GPU backend against it surfaces exactly one place that
did:

> [`Display::begin_frame`](fbui-platform/src/display/mod.rs) hands back a **mapped
> CPU byte buffer** (`Frame { buffer: &mut [u8], stride, .. }`). That is perfect
> for dumb buffers and fbdev, but a GPU backend renders into a GBM/EGL surface and
> *swaps* it — there is no CPU buffer to hand out, and forcing one (render on GPU,
> read back to a dumb buffer) would throw away the entire point.

The minimal, backward-compatible fix is to make the frame target an enum rather
than always a CPU slice:

```rust
pub enum FrameTarget<'a> {
    /// CPU dumb buffer / fbdev mmap — today's path, unchanged.
    Cpu { buffer: &'a mut [u8], stride: usize },
    /// A bound GL framebuffer the GPU painter draws into; present == eglSwapBuffers.
    Gl { /* fbo handle, gl context ref */ },
}
```

The software backends keep returning `Cpu` (so `fbui-render`'s copy-out path is
untouched and stays the default); a GPU backend returns `Gl`. The `present(damage)`
contract is unchanged — dumb buffers page-flip, GBM/EGL `eglSwapBuffers`. This is
a small, additive trait change staged for when the GPU backend lands, not a
redesign — the dual-path model (CPU default, GPU optional) the survey took from
Qt/Slint holds.

## The GPU backend design (gated on a GPU host)

### `drm-gbm-egl` Display backend (`display::gpu`, behind a `gpu` feature)

Bring-up, mirroring the well-trodden SDL `kmsdrm` / Slint LinuxKMS path:

1. Open the DRM card (through the seat, as the dumb backend already does) and pick
   connector + CRTC + mode with the existing `drm-rs` code.
2. `gbm_create_device(fd)` → `gbm_surface_create(GBM_FORMAT_XRGB8888,
   SCANOUT | RENDERING)`.
3. EGL: `eglGetDisplay(gbm)`, `eglInitialize`, choose an `XRGB8888` config,
   `eglCreateWindowSurface(gbm_surface)`, `eglCreateContext` (GLES2/3).
4. Per frame: render → `eglSwapBuffers` → `gbm_surface_lock_front_buffer` →
   `drmModeAddFB2` (cache per BO) → `drmModePageFlip(EVENT)` → on the flip event
   `gbm_surface_release_buffer` the previous BO. The flip fd is the same one the
   event loop already multiplexes, so frame pacing and `suspend`/`resume`
   (master drop/reacquire on VT switch) carry over verbatim.

Optional deps, gated exactly like libinput/libseat are today: `gbm`,
`khronos-egl`, and a GL loader (`glow`). Default builds pull none of it.

### GPU painter (behind the Phase 2 painter trait)

`fbui-render`'s `Painter` is currently a concrete tiny-skia type. Slotting a GPU
painter behind it needs a painter **trait** with the vector ops widgets already
call (`fill_rect`, `fill_path`, `stroke_path`, gradients, `push_clip`/`pop_clip`,
`push_opacity`/`pop_opacity`, `draw_image`) plus the text seam. Two routes, to be
chosen when the backend is built and can be measured:

- **`femtovg`** (a NanoVG-style GLES canvas) as the GPU painter impl — closest to
  the existing API surface, handles paths/gradients/AA and glyph atlases on the
  GPU.
- **A thin GL renderer** tessellating tiny-skia's path output — more work, less
  dependency.

The text seam is the subtle part: today `FontContext::draw_text` composites swash
glyph coverage straight into the CPU pixmap. Behind the trait it becomes
`canvas.fill_glyph(mask, pos, color)`, which the CPU painter implements as today's
coverage blit and the GPU painter implements as a textured quad from a GPU glyph
atlas. Introducing that trait is deferred deliberately: defining it against only
the CPU impl risks baking CPU assumptions into the "abstraction" (the exact
failure mode Phase 1 warned about), so it lands **with** the GPU painter that
validates it, not before.

## Verification status against the Phase 6 exit criteria

> *"Examples run unchanged on both backends, selected at runtime; software path
> remains the default and fully supported."*

| Exit criterion | Status |
|---|---|
| Backend selected at runtime | ✅ `Backend` + `FBUI_BACKEND`, `open_display` ordering; unit-tested |
| Software path is the default & fully supported | ✅ `Auto` never picks GPU; the entire render/widget stack is unchanged |
| Examples run unchanged on both backends | ⏳ the GPU backend needs a GPU/EGL host to build and run; the *selection* that would choose it is in place |
| `drm-gbm-egl` backend behind the `Display` trait | ⏳ designed above; surfaced the one trait extension (`FrameTarget`) the GPU path needs; implementation gated on a GPU host |
| GPU painter behind the Phase 2 painter trait | ⏳ designed (femtovg/thin-GL + the `fill_glyph` text seam); the painter trait lands with the impl that validates it |

### Honestly gated (consistent with Phases 0–5)

- The GPU backend links `libgbm`/`libEGL`/a GL driver and needs a GPU + KMS
  device; it can't be built (the `gbm` crate's build script probes for `libgbm`)
  or run in the headless CI container, the same way DRM/VKMS, libinput, and
  libseat were gated in Phases 1–4.
- The `FrameTarget` trait extension and the painter trait are **staged designs**:
  they land together with the GPU code that exercises them, so the abstraction is
  validated by a second implementation rather than guessed at — the sequencing
  rationale the plan set out for exactly this phase.

## What this leaves for Phase 7

The ecosystem backlog (IME, AccessKit, multi-display, a declarative UI macro
layer, a Vulkan KHR-display backend, the no_std/`embedded-graphics` bridge) is
unchanged and unblocked; none of it depends on the GPU path.
