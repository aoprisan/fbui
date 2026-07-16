# Running fbui apps in a terminal

fbui's reason to exist is owning a real Linux display — but the hardest part
of *developing* such an app has always been needing that display: a text VT,
root or `video`+`input` group membership, or a QEMU guest. The **terminal
backend** removes that wall. It is an ordinary display/input backend pair
behind the same `Display`/`InputSource` traits as DRM and evdev, so any
unmodified fbui app can run:

- in a terminal emulator on your dev machine,
- **over SSH** on the target device, before its panel even works,
- in CI, against a pty (that's how the backend's own tests run).

```sh
FBUI_BACKEND=term cargo run -p fbui --example showcase --features platform
```

No root, no device nodes, no VT. Esc quits (as on-device); the terminal is
restored on every exit path — drop, panic, and fatal signals — exactly like
the VT guard's console restore.

## Backend selection

`Platform::new` picks the backend at runtime:

1. `FBUI_BACKEND=term` (or `PlatformConfig::prefer_term`) forces the terminal.
   `FBUI_BACKEND=fbdev` and `drm` steer the device paths as before.
2. Otherwise DRM then fbdev are tried as usual. If **both** fail and the
   process is attached to a capable terminal (`/dev/tty` opens, and `$TERM` is
   neither empty, `dumb`, nor `linux`), the platform falls back to the
   terminal instead of dying — so `cargo run` in a desktop terminal or over
   SSH just works. On a real console (`TERM=linux`) the device error is
   reported instead, because there a DRM/fbdev failure is almost always a
   permissions problem you want to see.

## Pixel protocols

The backend speaks two protocols, chosen automatically
(`FBUI_TERM_PROTOCOL=kitty|cells` overrides):

**Kitty graphics protocol** — used when the terminal is known to support it
(kitty, Ghostty, WezTerm — detected via `KITTY_WINDOW_ID`, `$TERM`,
`$TERM_PROGRAM`, `$WEZTERM_PANE`). The frame is real pixels at the terminal's
full text-area resolution (cell size × grid, taken from `TIOCGWINSZ` or a
`CSI 14 t` query, falling back to 8×16 per cell). Damage is transmitted as
small *patch* images placed over the base frame at pixel offsets, so a
button's hover highlight costs bytes proportional to the button — the
difference between usable and unusable over SSH. When patches accumulate
(48) or a frame damages more than half the surface, the backend consolidates:
it retransmits the base (alternating between two image ids so the swap never
flickers) and deletes the patches.

**Half-block cells** — the universal fallback for every other terminal
(xterm, gnome-terminal, tmux, …). Each character cell shows two stacked
pixels using `▀` with 24-bit foreground/background colors, so an 80×24
terminal is an 80×48 surface — a *preview*, not a desktop. Zoom your terminal
font way out (or use a big window) to give the UI room; 200+ columns starts
to feel like a tiny panel. Damage maps to cell runs; unchanged cells are
never rewritten.

## Input

Keys arrive as UTF-8 and CSI/SS3 sequences and are normalized to the same
`KeyEvent`s evdev produces (keysym + text + modifiers, text on the press).
Terminals don't report key releases, so each key synthesizes a press
immediately followed by a release — the shape the widget layer already
handles. Note the app-level implications:

- **Esc exits** (the runner's convention), and a lone `0x1b` is only treated
  as Esc at the end of a read batch, so escape sequences never misfire it.
- Key *hold* interactions (Backspace auto-repeat aside — the terminal repeats
  for you) won't behave like a held hardware key.

The mouse uses SGR reporting (mode 1006 + all-motion 1003), upgraded to
**pixel coordinates** (mode 1016) in kitty mode; in cells mode a cell is 1×2
pixels anyway, so clicks are already pixel-accurate. Motion, buttons, and the
wheel map to the normal pointer events — hover states, tooltips, kinetic
scrolling and gestures all work.

## Resize is a hotplug

Resizing the terminal window flows through the same path as an HDMI
mode-change: the event loop's periodic `reconfigure()` poll notices the new
`TIOCGWINSZ`, reallocates the surface, and calls `on_display_changed`. Apps
that already handle hotplug (Phase 4) handle terminal resize for free, within
about a second.

## Limitations (v1, honest)

- `stderr` is not redirected: anything the app (or the platform's own
  diagnostics) prints lands on the alternate screen until the next repaint.
  Redirect it for a clean demo: `2>/tmp/fbui.log`.
- Sixel and the iTerm2 inline-image protocol are not implemented — kitty
  covers the modern graphics-capable set; everything else gets cells.
- Cell-mode resolution is what it is; it's a development preview, not a
  target environment.
- Presents are paced by the event loop's fbdev-style 16 ms timer, not vsync;
  refresh is reported as unknown.
- No key-release events and no multi-key chords beyond what modifiers encode.

## Testing

The backend is fully testable headless: the escape encoders and the input
parser are pure functions with unit tests, and `tests/term_pty.rs` drives the
public API end-to-end against a pty pair (the test plays the terminal
emulator). Nothing needs privileges, VKMS, or an actual terminal.
