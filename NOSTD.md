# fbui without an operating system — `no_std` design and status

fbui was built for Linux with no X11 and no Wayland. This track takes it one
step further down: **no OS at all**. The same widget tree, layout, painter
and copy-out run on a bare-metal target, and the sample app — a PDF/PNG/JPEG
viewer — boots straight into Rust on a Raspberry Pi 3.

This document is both the design (what was decided and why) and the status
record (what is verified, what is pending), in the style of the `PHASEn.md`
files.

## 1. Goal and non-goals

**Goal.** Everything above `fbui-platform` builds as `#![no_std]` + `alloc`,
so a board with a linear framebuffer, some key/touch input, a millisecond
clock and a heap can run an fbui app. Ship a non-trivial sample app — a
document viewer — and prove it on a bare-metal target.

**Non-goals.**

- A second, "lite" UI stack. The `no_std` build is the *same code* as the
  Linux build (see §2).
- `no_alloc`. The retained tree, taffy layout, cosmic-text and tiny-skia all
  allocate; a heap is a requirement (§6 gives the numbers).
- An RTOS integration layer. `fbui-bare` is a polling loop with a sleep hook;
  wrapping it in an RTOS task is the board's business.

## 2. The decision: make the existing crates `no_std`-capable

Two shapes were possible: fork a cut-down "embedded fbui", or teach the real
crates to build without `std`. We did the second:

- **The fast path must never diverge from the slow one** (an fbui invariant).
  A forked painter or layout would drift from the one the snapshot tests pin.
  With one code path, every golden PNG and behaviour test written for Linux
  covers the bare-metal build too.
- The dependencies already cooperate: tiny-skia (`no-std-float`), cosmic-text
  (`no_std`), taffy (`alloc`), slotmap, swash, ttf-parser, and the zune
  codecs all build without `std`.
- `std` was used thinly: a few `HashMap`s, `f32` math, file paths for
  screenshots, the `image` crate. Each has a clean seam (§4).

So `fbui-render` and `fbui-widgets` gained a **default `std` feature**. Off,
they are `no_std`. The Linux build (`fbui`, `fbui-platform`) is unchanged.

## 3. Architecture

```
                 Linux                           bare metal
  ┌───────────────────────────────┐   ┌────────────────────────────────┐
  │ fbui        (runner, remote…) │   │ your app      (e.g. doc-viewer)│
  │ fbui-platform (DRM, evdev, VT)│   │ fbui-bare     (runner, no OS)  │
  └───────────────┬───────────────┘   │ board crate   (boot, fb, input)│
                  │                   └───────────────┬────────────────┘
                  └────────────┬──────────────────────┘
          fbui-widgets   tree, layout, focus, gestures, animation   [std optional]
          fbui-render    painter, text, damage, copy-out            [std optional]
          fbui-doc       PNG/JPEG + PDF subset → pixmaps            [always no_std]
```

| crate | role | `no_std` |
|---|---|---|
| `fbui-render` | painter, text, damage, copy-out, rotation | yes, with `default-features = false` |
| `fbui-widgets` | retained tree and widget set | yes, with `default-features = false` |
| `fbui-bare` (new) | the bare-metal runner | always |
| `fbui-doc` (new) | PNG/JPEG decode, PDF subset renderer | always |
| `apps/doc-viewer` (new) | the sample app, board-independent | always |
| `apps/doc-viewer-rpi3` (new) | the Pi 3 board crate (outside the workspace) | always |
| `fbui`, `fbui-platform` | Linux runner and platform | Linux only (unchanged) |

### 3.1 The board contract — `fbui-bare`

A board implements two small traits; an app implements `App` (the same
`build`/`update` shape as `fbui::App`, minus threads and files, plus an
`on_key` hook for app-wide shortcuts):

```rust
pub trait Framebuffer {
    fn info(&self) -> FbInfo;          // width, height, stride, TargetFormat
    fn pixels(&mut self) -> &mut [u8]; // the scanout memory
    fn flush(&mut self, damage: &[IRect]) {} // SPI push / cache clean / flip
}

pub trait Board {
    fn poll_input(&mut self) -> Option<Input>; // keys, pointer, scroll — never blocks
    fn now_ms(&self) -> u64;
    fn wait(&mut self, deadline_ms: Option<u64>) {} // wfi / wfe / spin
}
```

`fbui_bare::run(app, &mut fb, &mut board, scale) -> !` is the whole main
loop: drain input → (gestures, animation) → paint if damaged → copy damaged
spans out → `flush` → `wait(next_deadline)`. `Runner` exposes the same steps
one at a time for tests and for boards that own their loop.

The fbui invariants carry over:

- **Stride is never computed** — `FbInfo::stride` is the controller's pitch.
  The viewer tests run on a padded-stride RAM framebuffer to keep this honest.
- **Forward-only writes** — the copy-out is the same damaged-span code, so
  write-combined or uncached scanout memory is fine to hand over directly.
- **Idle burns ~0% CPU** — with no damage, no animation and no pending
  gesture, `next_deadline` is `None` and the board sleeps until input. On the
  Pi this is a `wfi` woken by the UART interrupt (measured: 0 CPU ticks over
  5 s in QEMU, §8).
- **Buffer age** — a single-buffered scanout is age 1 after the first frame
  (age 0); `Runner::invalidate` resets it when something else drew there.

Raw input is in device pixels; the runner converts to logical coordinates and
runs the same `GestureRecognizer` the Linux runner does (tap, long-press,
fling), so touch boards get kinetic scrolling for free.

## 4. How the port works (the seams)

| `std` thing | `no_std` replacement |
|---|---|
| implicit prelude (`Vec`, `String`, `Box`, `format!`, …) | a private `prelude` module per crate, glob-imported by every module |
| `f32::floor/sin/sqrt/powf/…` | `fbui_render::math::F32Ext`, backed by `libm`. Under `std` the inherent methods shadow it, so hosted numerics are unchanged (a unit test pins the shim to `std`) |
| `std::collections::HashMap` | `hashbrown` (glyph atlas, widget names) |
| `std::rc`, `core::any`, `fmt`, `mem` | `alloc::rc`, `core::*` |
| `image` crate (decode, PNG encode) | `std`-only; `no_std` gets `Image::from_rgba_bytes` and the new `Image::from_pixmap` — `fbui-doc` decodes into tiny-skia pixmaps directly |
| file paths (`Image::open`, `Surface::write_png`, `Ui::request_screenshot`) | `std`-only |
| flow harness, `profile` tracing | `std`-only (`harness`/`profile` imply `std`) |
| cosmic-text system fonts | none in `no_std`; fonts come from bytes (`App::fonts`). `FontContext::layout` no longer panics on an empty font database — it lays out nothing — so a firmware that forgot a font shows no text rather than crashing |
| taffy `std` | `alloc` + the default layout algorithms; `detailed_layout_info` is `std`-only (taffy 0.11 doesn't build it without `std`; fbui never read it) |

### 4.1 Minimal widget set

`fbui-widgets` without its new **`all-widgets`** feature (on in the hosted
defaults, off with `default-features = false`) compiles only:

`Label`, `Button`, `Container`, `Stack`, `ScrollView`, `List`, `ImageView`,
`ProgressBar`.

Everything else — text editors, menus, selects, calendar, charts, gauge,
keyboard, dialogs, toasts, tree view, tabs, navigator, video view, switch,
slider, checkbox, radio, spinner — needs `all-widgets`. It all builds
`no_std` too; this is a size choice, not a capability one. `fbui-bare`
forwards the feature. Apps add their own widgets exactly as on Linux — the
viewer's `PageView` is one.

## 5. `fbui-doc` — documents without an OS

A new crate, always `no_std`, independent of the fbui stack (it produces
`tiny_skia::Pixmap`s, which `Image::from_pixmap` wraps with no copy).

**Raster:** PNG (all colour types, bit depths, palette/`tRNS`, Adam7) and
JPEG (baseline/progressive, gray/YCbCr/CMYK) via `zune-png`/`zune-jpeg`, with
a pixel budget checked before allocation.

**PDF subset** — chosen as "what a document viewer on a small device needs":

| area | supported | not supported |
|---|---|---|
| file structure | xref tables, xref streams, incremental updates (`/Prev`, hybrid `/XRefStm`), object streams; a reconstructing scan when the xref is broken | encryption (reported as `Error::Encrypted`) |
| filters | Flate and LZW (+ PNG/TIFF predictors), ASCIIHex, ASCII85, RunLength, DCT | JBIG2, JPX, CCITT (drawn as a neutral placeholder) |
| graphics | all path and paint operators, clipping (`W`/`W*`, text clip modes), line width/cap/join/miter/dash, `ExtGState` alpha, form XObjects, annotation appearance streams | blend modes, soft-mask groups, knockout/isolated groups, overprint |
| colour | Gray, RGB, CMYK, Lab, ICCBased (by component count), Indexed, Separation/DeviceN through PDF functions (sampled, exponential, stitching, **PostScript calculator**) | ICC colour management |
| images | 1–16 bpc, `Decode`, stencil masks, `SMask`, colour-key and stencil `Mask`, inline images | — |
| shading | axial and radial (`sh` and shading patterns), tiling patterns | shading types 1 and 4–7 (free-form meshes) |
| text | all text operators and render modes; embedded **TrueType/OpenType**, **CFF** (Type1C, CID-keyed), **Type 1** (own eexec decryption and charstring interpreter with flex, hint replacement, `seac`), **Type 3**; Type 0 with Identity-H or embedded CMaps; ToUnicode | predefined CJK CMaps; vertical writing. Non-embedded fonts (the standard 14) use a caller-supplied fallback face, stretched to the PDF's widths so line lengths hold |

**Robustness:** every input is hostile. Nesting, reference chains, forms,
Type 3 glyphs, calculator programs, operand stacks and decoded sizes are
bounded; errors are values. A deterministic mutation test (truncations and
byte bursts over the fixtures, 180 files per run) must not panic.

**Fidelity:** fixtures come from real producers (reportlab, fpdf2, pikepdf —
generators committed in `fbui-doc/tests/fixtures/`), and were compared by eye
against poppler's `pdftoppm`: vector pages, gradients, CID TrueType text,
images and soft masks match; differences are font substitution for
non-embedded faces (by design) and poppler's thicker text strokes. Tests
assert the colours the source document fixes.

## 6. Memory budget (measured)

Counting allocators in the tests and the viewer's `shots` example give:

| scenario | live heap | peak heap |
|---|---|---|
| 320×240 RGB565, minimal widgets, one font (`fbui-bare/tests/footprint.rs`, gated < 2 MiB) | 718 KiB | 875 KiB |
| viewer at 1024×768, library screen | 3.9 MiB | 6.3 MiB |
| viewer, a PDF page at fit-width | 9.9 MiB | 12.5–15.3 MiB |
| viewer, zoomed to ~280% | 16.6 MiB | 25.1 MiB |

Where it goes: the shadow surface is 4 bytes/pixel (3 MiB at 1024×768); a
rendered page is another 4 bytes/pixel of page; clip masks are 1 byte/pixel
while active; the glyph atlas is budgeted at 4 MiB; `App::fonts` copies each
font into the heap (Inter is ~300 KiB). The board sizes its heap
(`RenderOptions::max_pixels` and the viewer's `max_pixels` cap page rasters
so a huge page fails cleanly instead of exhausting it).

**Target classes this implies:** a Cortex-A or Cortex-M7 with external
SDRAM/PSRAM (STM32H7 + SDRAM, i.MX RT, ESP32-S3 + PSRAM, any Pi) runs the
viewer comfortably; a 320×240 control panel fits in ~1 MiB. Parts with
≤ 512 KiB of RAM and no external memory are out of reach without the
follow-ups in §9.

## 7. The sample app

**`apps/doc-viewer`** (`no_std`, board-independent): a library screen
(`List`), page view, paging, zoom steps and fit-width/fit-page, panning, a
progress strip; key map for keypads and terminals (→/PgDn/Space next, ←/PgUp
previous, ↑↓ scroll, Home/End, `+`/`-`, `w`, `p`, Esc, Enter). Built from the
minimal widget set plus one custom widget (`PageView`, a 2-D pannable
bitmap). Host tests drive it through the same `fbui_bare::Runner` a board
uses; `cargo run -p fbui-doc-viewer --features std --example shots -- DIR`
saves what the screen shows.

**`apps/doc-viewer-rpi3`**: the Pi 3 board crate, ~500 lines — reset vector
(park cores, EL3/EL2→EL1, FPU, stack, `.bss`), identity-mapped MMU with
caches (*required*: with the MMU off every access is Device memory, where the
unaligned loads Rust emits fault), VideoCore mailbox framebuffer, PL011 UART
keys (ANSI escapes decoded), generic timer, and `wfi` sleep woken by the UART
receive interrupt or a timer compare with IRQs masked (no vector table
needed). See its README for building `kernel8.img` and running in QEMU.

## 8. Status — verified vs pending

Verified in the development environment (CI's new `nostd` job runs the
same commands; its first run is pending at the time of writing):

- [x] `fbui-render`, `fbui-widgets` (minimal and `all-widgets`), `fbui-bare`,
      `fbui-doc` and `fbui-doc-viewer` build for `thumbv7em-none-eabihf` (no
      `std` exists there) on stable and on the 1.89 MSRV.
- [x] The `no_std` unit tests of `fbui-render` and `fbui-widgets` pass on the
      host (`--no-default-features --lib`); the hosted suite is unchanged.
- [x] `fbui-doc`: 18 unit tests, 7 fixture tests, the mutation pass, a doctest.
- [x] The viewer: 5 host tests through `Runner` (library, paging, zoom, Space
      read-on, broken documents reported not crashed, idle = no frames).
- [x] The Pi 3 image boots in QEMU `raspi3b` as ELF and as raw
      `kernel8.img`; `qemu_drive.py` types keys over the UART and screendumps:
      library, PDF pages (CID TrueType, embedded Type 1 and TrueType,
      standard-14 fallback, vector graphics, gradients, images, soft masks),
      zoom, fit-page, the PNG document — no panic, no blank screen.
- [x] Idle on bare metal: 0 CPU ticks over 5 s with the UI idle (QEMU).

Pending (hardware-gated or out of scope here):

- [ ] **A physical Raspberry Pi 3.** The real-board paths — data-cache
      cleaning of the framebuffer, the firmware's pixel order (handled
      either way), the UART's GPIO/baud setup — are written to the documents
      and exercised only as far as QEMU models them (QEMU does not model
      caches).
- [ ] **A physical MCU.** `thumbv7em` is compile-verified only; no
      Cortex-M board crate exists yet.
- [ ] **Bare-metal touch/mouse input.** `Input` has pointer events and the
      gesture path is shared with Linux, but the Pi port reads only the UART
      (no USB stack).

## 9. Follow-ups

- **Smaller targets.** A banded/tiled render mode (paint the damage in
  strips through a small RGB565 shadow) would lift the 4 bytes/pixel shadow
  requirement; `FontContext` from `&'static` font data would drop the font
  copy. Both would bring a 320×240 UI under ~300 KiB.
- **More boards.** UEFI (GOP framebuffer, `wfi`-free event waits), a
  Cortex-M7 + SPI panel (exercising `Framebuffer::flush` as a DMA push), the
  Pi 4 (GIC-400 instead of the BCM2836 local controller).
- **Storage.** A FAT reader so the viewer lists an SD card instead of
  compiled-in documents.
- **PDF.** Encryption (RC4/AES with an empty user password covers most
  "protected" documents), JPX via a `no_std` JPEG 2000 decoder, mesh
  shadings, blend modes.
