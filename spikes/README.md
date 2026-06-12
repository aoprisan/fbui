# fbui Phase 0 spike

A **throwaway** binary that de-risks the kernel-facing plumbing before any
framework API is designed (see `../PLAN.md` § Phase 0). It is intentionally not
part of a workspace and not published — it exists to be run on real hardware and
to produce `NOTES.md`.

What it exercises:

1. **DRM/KMS dumb buffers** — open the card, find the connected connector and
   its preferred mode, resolve encoder → CRTC, allocate **two** XRGB8888 dumb
   buffers, modeset, and animate color bars by page-flipping with
   `PageFlipFlags::EVENT`, blocking on the DRM fd for vsync.
2. **Shadow-buffer discipline** — render into normal RAM, then row-copy into the
   write-combined dumb mapping. `--direct` renders straight into the mapping
   instead, so you can measure the penalty. Both paths print timing.
3. **VT guard** — `KD_GRAPHICS` + keyboard mute (`K_OFF`), restored
   unconditionally via `Drop`, a panic hook, and signal handlers.
4. **fbdev fallback** — the same demo over `/dev/fb0`, honoring the
   kernel-reported stride and pixel bitfields, with `FBIOPAN_DISPLAY`
   double-buffering when the driver allows.

## Build

```sh
cd spikes
cargo build --release
```

Pure-Rust deps (`drm`, `libc`) — no system libraries needed to build.

## Run

Run from a **real Linux text VT** (Ctrl-Alt-F3, log in), as root or as the
active seat's session — modeset needs DRM master.

```sh
# Primary path: vsynced DRM dumb-buffer page flips, shadow → mapping copy
sudo ./target/release/fbui-spike drm

# Same, but render straight into write-combined memory (timing comparison)
sudo ./target/release/fbui-spike drm --direct

# Legacy fallback
sudo ./target/release/fbui-spike fbdev

# Prove the console always comes back
sudo ./target/release/fbui-spike drm --panic-after 120
```

Options:

| Flag | Meaning |
|---|---|
| `drm` \| `fbdev` | backend (default `drm`) |
| `--device <path>` | node (default `/dev/dri/card0` or `/dev/fb0`) |
| `--seconds <n>` | run n seconds then exit cleanly (default 8; `0` = forever) |
| `--direct` | (drm) skip the shadow, render into the WC mapping |
| `--no-vt-guard` | don't touch console mode (serial console / pty / SSH) |
| `--panic-after <n>` | panic after n frames — console-restore test |

Over SSH or a serial console (where the KD ioctls would `ENXIO`/`ENOTTY`), pass
`--no-vt-guard`; the spike also degrades to a no-op guard automatically if it
can't open `/dev/tty` as a console.

## Output

Stops cleanly on `--seconds`, `SIGINT` (`kill -INT`), or `SIGTERM`, printing a
timing summary:

```
---- timing (drm/shadow) ----
  frames      : 480
  wall        : 8.001 s
  achieved fps: 60.0
  avg render  : 1.2 ms/frame
  avg blit    : 0.4 ms/frame
```

Record results in `NOTES.md`.
