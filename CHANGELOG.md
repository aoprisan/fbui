# Changelog

All notable changes to **fbui** are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project aims to
follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Versioning policy

While the framework is pre-1.0 (`0.y.z`), a bump of **`y`** may carry breaking
API changes and a bump of **`z`** is reserved for backwards-compatible fixes and
additions. The workspace crates (`fbui`, `fbui-widgets`, `fbui-render`,
`fbui-platform`, `fbui-testkit`) are versioned **in lockstep** off the workspace
`version`, so a given `fbui` release pins exactly the sibling versions it was
built against. The MSRV is part of the contract: `fbui-platform` builds on Rust
**1.76**; the render/widget stack tracks its heavier dependencies (cosmic-text,
image) at **1.89**. An MSRV raise is a breaking change for the affected crate.

## [Unreleased]

### Added

- **Terminal backend** (`fbui-platform` feature `term`, in the default set) —
  run any unmodified fbui app *inside a terminal*: over SSH, in a terminal
  emulator on a dev machine, in CI. `FBUI_BACKEND=term` forces it; when DRM
  and fbdev both fail and the process is attached to a capable terminal
  (`$TERM` not empty/`dumb`/`linux`), the platform now falls back to it
  instead of dying. Two pixel protocols, auto-detected
  (`FBUI_TERM_PROTOCOL=kitty|cells` overrides): the **kitty graphics
  protocol** (kitty/Ghostty/WezTerm) shows real pixels at full resolution
  with damage sent as small patch images over a ping-ponged base frame — a
  button repaint costs bytes proportional to the button, which is what makes
  it usable over SSH; **half-block cells** (`▀` + 24-bit SGR, 2 px per cell)
  works in any truecolor terminal as a preview. Input parses the terminal
  byte stream into the normal normalized events: UTF-8 keys and CSI/SS3
  sequences with modifiers (each key synthesized as press+release — terminals
  report no release), and SGR mouse buttons/motion/wheel with
  pixel-precision coordinates (mode 1016) in kitty mode. Terminal resize
  rides the existing hotplug path (`reconfigure` → `on_display_changed`).
  The raw-mode/alt-screen/mouse/image state is restored on **every** exit
  path — drop, panic, fatal signals — mirroring the VT guard. Pure Rust, no
  new dependencies; tested headless against pty pairs
  (`fbui-platform/tests/term_pty.rs`). See `docs/terminal-backend.md`.
- **`BackendKind::Terminal`** (breaking for exhaustive matches on
  `BackendKind`) and **`PlatformConfig::prefer_term`** (breaking for struct
  literals not using `..Default::default()`). `PlatformConfig::default()` now
  also reads `FBUI_BACKEND` (`drm`/`fbdev`/`term`), so existing binaries gain
  backend selection without a rebuild.
- **Named keysyms** for PageUp/PageDown/Insert/F1–F12 in
  `fbui_platform::keysym` (the terminal parser emits them; evdev apps can now
  match them by name too).
- **Input session record & replay** (`fbui` runner) — set `FBUI_RECORD=path`
  to capture the normalized input stream of a live session (flushed per
  event, so a crashed session's recording survives — that's the artifact you
  wanted), and `FBUI_REPLAY=path` to play it back through *exactly* the live
  input path (gestures, focus, kinetic scrolling, `App::update`).
  `FBUI_REPLAY_SPEED=n|max` scales the clock; `FBUI_REPLAY_SHOT=end.png`
  captures the settled end state and exits (`FBUI_REPLAY_EXIT` overrides).
  The format is hand-editable line-oriented text (`fbui-rec` v1, timestamps
  clamped monotonic, unknown lines skipped). Together with the terminal
  backend this makes a recorded kiosk flow a headless CI regression test:
  replay at `max` speed on `FBUI_BACKEND=term`, screenshot, compare — end
  states verified byte-identical across runs. See `docs/record-replay.md`.

- **The popup layer** — floating overlays can now be *interactive*.
  `Ui::open_popup(owner, PopupOptions)` (or `EventCtx::open_popup` from a
  handler) promotes a widget's overlay into a popup: pointer events inside the
  overlay rect route to the owner ahead of capture and tree hit-testing,
  presses outside dismiss it (delivering the new `Event::PopupDismissed`) and
  are consumed, scrolls outside are swallowed, Tab is confined to the topmost
  popup owner's subtree, and grabbed focus is restored on close. Popups stack;
  entries are pruned when the owner vanishes or stops reporting an overlay.
  The new `Widget::prepare_overlay` hook (defaulted) gives overlay owners font
  access to measure themselves before first placement. See `DESIGN.md` §5.
- **`place_anchored`** (`fbui_widgets::popup`) — the shared placement rule for
  anything floating: preferred side (`Below`/`Above`/`Right`/`Left` +
  `Start`/`Center`/`End` alignment), flipped to the opposite side when it
  doesn't fit and the opposite has more room, clamped (and shrunk as a last
  resort) to the surface. `Select`'s menu flip is now this function.
- **`Menu`** — a floating action menu on the popup layer: a zero-size host
  widget (the `Toasts` pattern) armed via `Ui::with` (`open_at`/`open_below`)
  then registered with `Ui::open_popup`. Items activate on release or Enter;
  separators and disabled items are skipped by pointer and arrow keys
  (Up/Down/Home/End); Esc and click-away fire `on_close`. Submenus and
  scrolling menus are out of scope for v1.
- **`ContextMenu`** — a transparent wrapper widget: children lay out and paint
  normally; a right-click or long-press anywhere inside its bounds (bubbled,
  so gestures on nested interactive children count) opens the shared menu
  engine at the pointer. `on_select(index)` on activation, `on_close`
  otherwise.
- **Tooltips** — `Ui::set_tooltip(id, Tooltip::new("…"))` shows a tip after a
  hover dwell (default 0.6 s, `delay(..)`) or immediately on a long-press
  (touch), hidden on hover change, any press/release, or a key. The dwell
  counts down on the deterministic frame clock and the clock runs *only*
  during the dwell — a shown tip costs nothing (the idle-burns-0% rule).
  Placement prefers above (`placement(..)`), flipping at screen edges.
  (Deliberately *not* built on the app timer API below: tooltips stay inside
  the deterministic, headless-testable widget layer.)
- **App timers** — `Proxy::send_after(delay, msg)` delivers a message to
  `App::update` once after `delay`; `Proxy::send_every(period, msg)` repeats
  (fixed-delay: a stalled loop catches up with one message, never a burst).
  Both return a cancellable, `Send` `fbui::Timer` handle (dropping the handle
  detaches; the delivery still happens) and work from any thread. No threads,
  no ticking: the event loop sleeps in `poll` until the earliest deadline.
  See the new `timer` example.
- **`PlatformHandler::next_timeout`** (fbui-platform, defaulted — existing
  handlers unaffected) — the handler now bounds the event loop's poll
  timeout with its next deadline instead of the loop hard-coding 16 ms.
  Returning `None` lets the loop block until fd activity (bounded by the
  ~1 s hotplug-poll backstop). The `fbui` runner uses it: 16 ms only while
  animating or mid-gesture, the next timer deadline otherwise, else no
  time-based wakeups at all — a truly idle app now sleeps ~1 s per wakeup
  instead of spinning at 60 Hz.
- **`TabBar`** — a segmented tab strip for switching between views: equal-width
  segments in one tree node (self-painted, self-hit-tested), a single tab stop
  with Left/Right/Home/End moving the selection while focused. Emits
  `on_select(index)` only when the selection changes; `TabBar::tab_rect`
  exposes the segment geometry paint and hit-testing use.
- **`Spinner`** — an indeterminate activity indicator: a ring of dots with a
  rotating brightness head, spinning from the moment it's added and stopped /
  restarted via `set_running`. Driven by the frame `dt` (never a wall clock),
  damage-quantized to head steps, and free when stopped — the idle-burns-0%
  rule. To let a widget animate from birth, `Ui` now arms one conservative
  animation tick when a widget is inserted (cleared on the next `animate` if
  nothing is actually running), the same arm `Ui::with` already used.
- **SVG icons** (feature `svg`, off by default) — `Image::from_svg` /
  `Image::from_svg_file` rasterize a vector icon at any device-pixel size
  through resvg (which draws with the same tiny-skia the painter uses):
  one asset serves every scale, displayed via the existing `ImageView`.
  The drawing is fit to the requested box, aspect preserved, centered.
  Text and embedded raster images inside SVGs are deliberately not enabled.
- **Screenshot API** — `Surface::to_rgba` (straight-alpha RGBA8 rows),
  `Surface::encode_png`, and `Surface::write_png` export what's on screen;
  `Ui::request_screenshot(path)` + `Ui::take_screenshot_request` let an app
  ask for a capture from `App::update` (remote diagnostics for a device with
  no second screen). The runner fulfills a request after the next paint — the
  shot always includes what the requesting update changed.
- **`Keyboard`** — an on-screen (virtual) keyboard for touch kiosks with no
  physical keyboard. A docked, non-focusable key grid that paints all its keys
  itself and hit-tests taps internally (one tree node), with QWERTY, a Shift
  layer, and a `?123` symbols layer. It deliberately never takes focus (so the
  text field being edited keeps it) and emits each tapped `Key` via `on_key`.
  Shift is one-shot (reverts after the next character) and Backspace
  auto-repeats while held, driven by the frame `dt` — never a wall clock.
  `Keyboard::key_rect` exposes the key geometry paint and hit-testing use.
- **`Ui::send_key`** — replay a `Key` to the focused widget as a synthetic
  pressed key event, the routing for an on-screen `Keyboard`'s taps from
  `App::update`: it drives the same event path as hardware input, so
  `on_change` fires and `Tab` moves focus. See the `osk` example.
- **`TextInput::apply_key`** — apply a `Key` at the caret as if typed
  (insert/backspace/delete/cursor), for direct programmatic edits. The
  hardware-key path shares this same implementation. It does **not** fire
  `on_change`; use `Ui::send_key` when it should.
- **`Widget::animate_with` + `AnimCtx`** — a defaulted variant of
  `Widget::animate` that can emit application messages from an animation tick
  (what `Ui::animate` now calls); `Keyboard`'s Backspace auto-repeat rides on
  it. Existing `animate` implementations are unaffected.

### Changed

- **`Event` is now `#[non_exhaustive]`** (breaking): downstream matches need a
  wildcard arm. Added the `Event::PopupDismissed` variant, delivered to a
  popup's owner when the `Ui` dismisses it (click-away, or a press landing in
  a popup stacked below it).
- **Gesture bubbling** (breaking for custom widgets relying on non-bubbling):
  `Tap`, `LongPress`, `Fling`, and right-button `PointerDown` now bubble from
  the hit widget to its ancestors like `Scroll` and `Key` always did.
  Left-button presses/releases stay direct. In-tree audit: `Dialog` already
  swallowed the gesture set (pinned by a regression test); a fling landing on
  a child of a `ScrollView` now correctly coasts the view.
- **`Select` sits on the popup layer**: the open menu no longer captures the
  pointer or implements its own click-away/scroll-swallow — the `Ui` routes,
  dismisses, and consumes. Behavior is unchanged (the existing Select tests
  pass as-is); Tab while the menu is open now stays on the field instead of
  leaking to the page behind it.
- **`App::Message` now requires `Send`** (breaking): the timer queue shares
  messages across threads, as `Proxy` always did in practice (`Proxy<M>` was
  only ever `Send` when `M` was). UI message enums are `Send` in any
  realistic app; the bound just states it.

### Fixed

- A stray debug `eprintln!` in `Ui::paint` printed a line on every
  scroll-blit frame; removed.
- `Keyboard` no longer leaks pointer capture when a held (auto-repeating)
  Backspace is cancelled by sliding off the key: the slide-off cleared the
  pressed state, and the release of the capture taken on press was gated on
  it, so the keyboard kept routing **all** later pointer input to itself —
  buttons and fields elsewhere on screen stopped responding until a key tap
  completed on the keyboard. The capture is now released on every pointer-up.
- The runner now drains widget messages **until the queue is empty** after
  events, animation ticks, and `Proxy` wakes — a message queued from inside
  `App::update` itself (e.g. an `on_change` triggered via `Ui::send_key`) was
  previously stranded until the next input event arrived.

## [0.2.0] — 2026-07-10 — Phase 5: performance & animation, and the overlay layer

### Added

- **The overlay layer** — everything that floats above the page, built on
  `Stack` and a new pair of `Widget` hooks (`overlay_rect`/`paint_overlay`:
  a floating rect painted after the whole tree, treated as damage whenever the
  widget changes):
  - **`Dialog`**: a modal scrim that centers its children (the app adds the
    card as its subtree), swallows input to the page beneath, traps Tab focus
    inside itself (`Widget::traps_focus`), and emits `on_dismiss` on Esc or a
    scrim click. Opened by adding it to a `Stack`; closed with the new
    **`Ui::remove`** (subtree removal, clearing focus/hover/capture into it).
  - **`Select`**: a dropdown. The open menu floats above everything (flipping
    upward when out of room), the pointer is captured while open (click-away
    dismisses without activating what's underneath), and Up/Down/Enter/Esc
    navigate from the keyboard.
  - **`Toasts`**: a zero-size host for transient notifications, stacked
    bottom-center with kind-colored edges (`ToastKind::{Info, Success,
    Error}`), pushed via `Ui::with`, fading out on the frame clock (idle when
    empty).
  - Supporting engine work: **key-event bubbling** (unhandled keys climb from
    the focused widget up the ancestor chain — how Esc reaches a `Dialog`),
    **`Ui::focus_first`** (move focus into a subtree, e.g. a just-opened
    modal), `EventCtx::surface_size`, and the scroll-blit overlap guard
    understands floating overlays. Shown headlessly in the `overlay_png`
    example (see the widgets README) and interactively in the new `overlay`
    example (`cargo run -p fbui --example overlay --features platform`).
- **Incremental repaints are now pixel-exact**: the repaint region is snapped
  outward to whole device pixels before painting, so a region-edge pixel is
  fully owned by whichever repaint drew it last. Previously a fractional
  region edge anti-aliased against the prior frame and repeated partial
  repaints could drift a hair from what a full repaint produces (caught by the
  overlay equivalence tests).

- **Scroll-blit for `ScrollView`** (closing the gap 0.1.0 shipped with): a
  wheel/drag/kinetic scroll now shifts the viewport's pixels in place and
  repaints only the exposed strip, via a new
  `EventCtx::request_scroll_layout` — a relayout *without* the implicit
  full-surface repaint, for widgets that account for every changed pixel
  themselves. Children still re-place at the new offset; only those touching
  the strip re-rasterize. Benchmarked (`scrollview_blit` vs
  `scrollview_full_repaint`, ~40% cheaper on the dev host) and pinned
  byte-for-byte against a full repaint, like the `List` path.
- **Overlay-safety guard for the blit fast path**: the `Ui` now detects when
  something later in z-order (e.g. a `Stack` overlay) overlaps a scrolling
  widget and falls back to a full repaint — an in-place pixel shift would have
  dragged the overlay's pixels along. Covered by an equivalence test.
- **Wheel-scroll bubbling**: an unhandled `Event::Scroll` now bubbles from the
  deepest widget under the pointer up its ancestor chain, so a wheel over a
  `Label` *inside* a `ScrollView` scrolls the view (previously the event died
  at the leaf).
- **`Container::width`/`height` now pin the min size** so an explicitly-sized
  container can't be flex-shrunk below it — fixed-height rows in a
  `ScrollView` overflow (and scroll) instead of silently compressing.

- **`Stack` container**: a layout that *overlays* its children instead of flowing
  them. A new `Widget::stacks_children` hook lets the `Ui` give each child of a
  stack `position: absolute` filling the stack, so children share a box and
  z-order by insertion (last on top, hit-tested first). This is the overlay
  primitive for future modal scrims, toasts, and popovers. Covered by behavior
  tests (overlap geometry, topmost-child hit order) and a text-free snapshot.
- **`RadioGroup` widget**: a single-choice list of options as one widget and one
  tab stop, with arrow-key navigation within the group and click-to-select; emits
  `on_change(index)`, mirroring `Checkbox`'s `on_toggle`. Shown in the
  `gallery_png` example.

- **Custom-widget extension surface**: the `fbui` umbrella now re-exports the
  `widget`, `anim`, and `style` modules plus `Anim`, so a downstream crate can
  implement `Widget<Msg>` for its own type without reaching into the sub-crates.
  Documented with a compiler-checked doctest on the `Widget` trait, a new
  "Writing a custom widget" section in `fbui-widgets/DESIGN.md`, and a
  `custom_widget` example (a tappable, pulsing `Dot`) exercising `measure`,
  `paint`, `event`, `animate`, and `focusable` end to end.

- **Animation API** (`fbui-widgets::anim`): `Easing` curves, a `Lerp` trait
  (`f32`, `Color`), and a `Tween<T>` advanced by the frame `dt` — pure and
  damage-aware. A new animated `Switch` widget is the worked example.
- **`Widget::animate` / `Ui` plumbing**: `EventCtx::request_anim` and
  `Ui::is_animating` so the runner advances animation only while something moves;
  Phase 4's kinetic coast now rides the same flag.
- **Scroll-blit fast path**: `Surface::scroll_region` shifts a region's pixels in
  place and reports the exposed strip; a new `Widget::scroll_blit` hook +
  `Anim::damage` + `PaintCtx::region` let `List` repaint only the strip that
  scrolled into view instead of the whole viewport. Benchmarked (~34% here) and
  pinned byte-for-byte against a full repaint.
- **`tracing` profiling** (`profile` feature on `fbui` / `fbui-widgets`): nested
  spans through input → update → layout → paint → present, with a flamegraph
  guide in [`docs/profiling.md`](docs/profiling.md). Zero-cost when off.
- A `scroll` benchmark (`fbui-widgets`) and CI gates for it and the `profile`
  feature.
- **Size-tuned release profile**: the workspace `[profile.release]` now builds for
  shipping (LTO, one codegen unit, `panic = "abort"`, `strip`) instead of carrying
  debug info. `panic = "abort"` is safe because the console restore is a panic
  *hook* + signal handlers, which run before abort — not unwinding `Drop`. A
  fully-featured release binary (DRM + evdev + text + bundled font) is ~3.4 MB. A
  new "Small builds & fast boot" section in the device guide documents the profile,
  the pure-Rust default features (no libinput/seatd/dbus/mesa in the image), and
  font bundling.
- **uevent hotplug trigger**: a pure-libc `NETLINK_KOBJECT_UEVENT` monitor
  (`fbui-platform`, no libudev) watches for `SUBSYSTEM=drm` events and makes the
  event loop reconfigure *immediately* on connect/disconnect/mode change, instead
  of waiting for the ~1 s poll (which stays as a backstop). Best-effort: if the
  netlink socket can't open (a sandbox without it), the poll still covers hotplug.
  Closes the "wire a udev/uevent monitor" gap from 0.1.0.
- **`Button` variants**: `ButtonVariant::{Primary, Secondary, Danger}` with
  `Button::secondary()` / `danger()` shorthands, each pulling its fill from the
  theme — including a new `Palette::danger` color, so a destructive action (an
  "Erase" button) reads as dangerous. Runtime theme switching already worked via
  `Ui::set_theme`.
- **`ProgressBar` widget**: a read-only fraction indicator (`[0, 1]`) for
  long-running work — the missing complement to the interactive `Slider`. Drive
  it from `App::update` via `Ui::with` (e.g. from progress a worker posts through
  a `Proxy`); it reuses the theme's track/accent colors. `Container` also gained
  `width`/`height` builders to give an `auto`-sized child a definite length. The
  `progress` example now shows a real bar.
- **Cross-thread wakeup primitive**: a generic way to drive the UI from a
  background thread. `fbui_platform::Waker` (a clonable, `Send` handle backed by a
  `calloop` ping) wakes the event loop; new `PlatformHandler::on_start(waker)` /
  `on_wake()` hooks deliver it and service it. At the runner level, `Proxy<M>`
  pairs a message sender with the waker: `App::on_start(proxy)` hands one out, a
  worker calls `proxy.send(msg)`, and the runner runs it through `App::update`
  exactly like a widget-emitted message. The framework stays ignorant of what the
  work is (IPC, I/O, a timer) — a new `progress` example shows the pattern. This
  is the primitive an out-of-process backend's client uses to feed the UI.
- **Host-independent / bundled fonts**: `FontContext::with_fonts(bytes)` builds a
  text context from caller-supplied TTF/OTF with **no** dependence on the host's
  installed fonts (the first face becomes the default family, so a plain
  `TextStyle` resolves to it) — what a reproducible, fast-booting target needs.
  `App::fonts()` and `Ui::with_fonts` plumb a font set through the runner. An
  optional `bundled-font` feature compiles in a default font (Inter Regular, OFL;
  `FontContext::with_default_font`) for turnkey text with no asset files, off by
  default (~300 KB). A deterministic text test backs it. (Note: with cosmic-text
  0.19, `FontContext::new()` no longer scans system fonts — it starts empty.)
- **Visible mouse cursor in the runner**: the `fbui` app runner now composites a
  software arrow over each frame (its position mirrors the pointer), so a mouse
  is actually drawn — clicking already worked, the pointer just wasn't shown. The
  arrow is overlaid into the back buffer after copy-out and never touches the
  shadow; a new `Surface::damage_device_rect` refreshes the vacated pixels so the
  buffer-age history erases the old position across every back buffer. A bare
  pointer move now schedules a present even when no widget changed.

### Known gaps

- The DRM hardware **cursor-plane** overlay (cursor move without a widget
  repaint) is deferred — it needs a real DRM device.
- Scroll-blit is wired into `List`; `ScrollView` still full-repaints its region.
- On-device Pi-class refresh-rate / CPU-budget figures need ARM hardware; the
  blit-vs-full *ratio* is the CI gate.

## [0.1.0] — Phase 4: hardening & first release

The first tagged release. Phases 0–3 built the stack — kernel-facing spike,
platform layer, headless renderer, widget toolkit — and Phase 4 hardens it for
real devices.

### Added

- **Touch & pointer gestures** (`fbui-widgets`): a headless, deterministic
  `GestureRecognizer` that turns one contact stream (mouse or single-finger
  touch, unified) into `Tap`, `LongPress`, drag, and `Fling` gestures. New
  `Event::Tap` / `Event::LongPress` / `Event::Fling` variants carry them into
  the widget tree.
- **Kinetic ("flick to coast") scrolling**: `ScrollView` and `List` consume
  `Fling` and decay it through the new `Widget::animate(dt)` hook and
  `Ui::animate(dt)` frame-clock walk. `List` also gained touch drag-to-scroll
  (selecting only on a tap, so a drag scrolls instead).
- **RGB565 ordered dithering** (`fbui-render`): `copy_out_dithered` / a
  `Surface::set_dither` toggle apply 4×4 Bayer dithering on the 16-bit copy-out
  to suppress gradient banding on small panels. The runner enables it
  automatically when the display reports `Rgb565`.
- **Display hotplug / mode-change handling**: `Display::reconfigure` (implemented
  for the DRM and fbdev backends), a `PlatformHandler::on_display_changed` hook,
  and `InputSource::set_surface` rescaling. The event loop polls the connector's
  cached state on a low cadence; the runner rebuilds its surface and re-lays-out
  the tree on a change.

### Changed

- The raw-evdev parser is split into a pure `PacketState` state machine
  (`feed_raw`) with no `Device` handle, so it can be exercised directly.
- `AbsRange::scale` now clamps to the surface, so a device reporting
  out-of-range absolute values can never produce an off-surface coordinate.
- `List` selects on pointer **release** (a tap) rather than press, so a drag
  scrolls instead of selecting.

### Hardened

- Crash safety: `SIGQUIT` joins the signals whose handler restores the console;
  a regression test pins `restore_console`'s run-at-most-once idempotency.
- A deterministic fuzz test throws 300k arbitrary `(type, code, value)` tuples
  at the evdev parser and asserts it never panics, overflows, or emits an
  off-surface coordinate — the Phase 4 "fuzz the input parser" task, in plain
  `cargo test`.

### Docs & CI

- A "[running on your device](docs/running-on-your-device.md)" guide (permissions,
  seatd vs logind vs root, kernel config, troubleshooting), this changelog, and a
  versioning policy.
- CI gained a `cargo doc` (warnings-as-errors) gate and a benchmark-compile gate
  alongside the existing fmt/clippy/test, MSRV, and VKMS jobs.

### Known gaps (hardware / dependency gated, consistent with Phases 0–3)

- On-device verification of VT switching, DRM page-flip timing, multi-touch
  hardware input, and the Pi-class performance gates still needs real hardware /
  a VKMS CI runner with a non-writeback connector.
- Hotplug detection currently polls the connector's cached state; wiring a
  udev/uevent monitor as the trigger is the remaining on-device step.
- Publishing to crates.io is deferred: the crates carry `version = 0.1.0` and a
  lockstep policy, but the upload (flipping `publish`, a registry token) is a
  release-time action, not done from CI here.
- libinput's `set_surface` rescale-on-hotplug override is left to the
  feature-gated backend (not in the default/CI build).

[Unreleased]: https://github.com/aoprisan/fbui/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/aoprisan/fbui/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/aoprisan/fbui/releases/tag/v0.1.0
