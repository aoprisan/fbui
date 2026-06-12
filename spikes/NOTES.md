# Phase 0 spike — findings & test matrix

This is the deliverable the rest of Phase 0 exists to produce: a record of how
real hardware diverges from the documentation, so Phase 1's `Display` trait is
designed against reality instead of the man pages. Fill in the **TO MEASURE**
cells from real runs; the design-implication notes below are what those numbers
feed into.

The spike binary is `spikes/` (`cargo run -- [drm|fbdev] …`). See
`spikes/README.md` for how to run each target.

---

## Test matrix

Run on each target:

```
# vsynced shadow path (the real design)
fbui-spike drm --seconds 10
# naive direct-to-WC-memory path, for the timing comparison
fbui-spike drm --seconds 10 --direct
# legacy fallback
fbui-spike fbdev --seconds 10
# console-restore proof
fbui-spike drm --panic-after 120
```

| Target | DRM modeset | Tear-free flips | fbdev fallback | Console restores after panic/SIGTERM | Notes |
|---|---|---|---|---|---|
| QEMU `-vga std` (bochs-drm) | TO MEASURE | TO MEASURE | TO MEASURE | TO MEASURE | |
| VKMS (virtual KMS, CI) | TO MEASURE | n/a (no real scanout) | n/a | TO MEASURE | writeback connector only; flips complete but nothing is scanned out — used for CI correctness, not visual/tearing checks |
| Raspberry Pi (vc4-kms-v3d) | TO MEASURE | TO MEASURE | TO MEASURE | TO MEASURE | |
| Generic Intel laptop (i915) | TO MEASURE | TO MEASURE | TO MEASURE | TO MEASURE | |

## Timing record (fill from `---- timing ----` output)

| Target | Res | drm/shadow render+blit ms | drm/direct render ms | fbdev render+blit ms | achieved fps |
|---|---|---|---|---|---|
| QEMU | | | | | |
| Pi | | | | | |
| Intel | | | | | |

The headline number Phase 1/2 care about: **shadow render+blit** vs.
**direct**. The hypothesis from the plan is that writing straight into
write-combined dumb-buffer memory (the `--direct` path) is dramatically slower
than render-to-RAM + one streaming row-copy, because WC memory punishes the
scattered sub-word writes that rasterization does. Record the ratio per target.

---

## What the spike already establishes (no hardware needed)

These came out of writing the code against `drm-rs` 0.15 / the kernel ABI and
hold regardless of target:

- **Stride is never computed.** Both backends take the row stride from the
  kernel: DRM from `DumbBuffer::pitch()` (the driver rounds up — e.g. a
  1366-wide XRGB8888 line is *not* `1366*4`), fbdev from
  `fb_fix_screeninfo.line_length`. The `Display` trait in Phase 1 must surface
  stride as first-class data on every mapped frame; widgets/render must never
  assume `width * bpp`.
- **Map once, copy forward only.** The dumb mapping is mapped a single time and
  only ever written via whole-row `copy_from_slice`. No reads, no random-access
  writes into device memory. This is the shape the Phase 1 `frame()` API should
  encourage: hand out a `&mut [u8]` + stride that the caller fills sequentially.
- **Page-flip is the frame clock.** `page_flip(.., EVENT, ..)` + blocking in
  `poll()` on the DRM fd until the `PageFlip` event arrives gives vsync *and*
  idle-sleep for free. Phase 1's `calloop` loop multiplexes exactly this fd.
- **DRM master is required for modeset.** `set_crtc` / `page_flip` fail without
  it — you get it by being root, or by being the active VT's session via
  libseat. The spike surfaces this as an explicit error pointing at the cause.
  This is the seam Phase 1's libseat / `noseat` split plugs into.
- **fbdev pixel packing must honor the `var` bitfields.** We only take the
  whole-row fast path when the bitfields are exactly R@16/G@8/B@0 (the common
  XRGB case that matches our shadow's native little-endian byte order);
  otherwise we pack per-channel. Don't assume layout.
- **fbdev double-buffering is conditional.** Only available when
  `yres_virtual >= 2*yres` and `ypanstep > 0`; then `FBIOPAN_DISPLAY` with
  `FB_ACTIVATE_VBL` flips and (usually) vsyncs. Many simple framebuffers expose
  no pan room at all — fall back to single-buffered + manual pacing.

## Console-safety design (the part everyone gets wrong)

The VT guard restores `KD_TEXT` + the original keyboard mode on **every** exit
path, implemented in `src/vt.rs`:

- normal teardown → `Drop`
- `panic!` → panic hook (restores *before* printing the backtrace)
- `SIGSEGV/SIGABRT/SIGILL/SIGBUS/SIGFPE/SIGHUP` → async-signal-safe handler that
  restores and re-raises with the default disposition
- `SIGINT/SIGTERM` → soft-stop handler flips `RUNNING`, the run unwinds, `Drop`
  restores (so timing still prints)

All state the signal handler needs (tty fd, saved KB mode) lives in atomics; the
handler does nothing but two `ioctl`s. The `GUARD_ACTIVE` swap makes restoration
idempotent across the Drop + signal race.

**TO VERIFY on hardware:** that a `kill -9` of the process (which we *cannot*
trap) still leaves a usable console. Expectation per the plan: SIGKILL can't be
caught, so the kernel's own VT cleanup on process exit must suffice — confirm
the console is usable (may need a blind `reset`/Ctrl-Alt-Fn) and record whether
`KD_GRAPHICS` survives the killed process on each target. If it does, Phase 1
needs a watchdog/helper story; if the kernel restores on close, we're fine.

## Per-target surprises (fill in)

### QEMU `-vga std`
- TO MEASURE

### VKMS
- TO MEASURE (note: needs `modprobe vkms`; connector is writeback — validate
  modeset/flip-event plumbing, not pixels)

### Raspberry Pi
- TO MEASURE (vc4: check whether preferred mode matches the attached panel;
  HDMI hotplug timing)

### Generic Intel laptop (i915)
- TO MEASURE (often needs to run from a real VT with master; check stride
  rounding on odd widths)

---

## Exit criteria (Phase 0)

- [ ] Tear-free 60 fps bars on at least two real targets (`drm`, shadow path).
- [ ] Crash test (panic, Ctrl-C/SIGTERM, and a `kill -9`-adjacent case) never
      leaves the console dead.
- [ ] This document records the stride / format / master-lock surprises that
      feed Phase 1's `Display` trait design.
