# Phase 3 design: the retained widget tree

> PLAN §4 makes this document Phase 3's *first* task — "the design doc for this is
> the phase's first task and gets reviewed before code." This is that doc. It
> fixes the data model, the update/damage/paint loop, layout integration, and the
> focus/input model before any widget is written.

## 1. Why retained, not immediate

PLAN §3.3 already decided this and it shapes everything: **retained tree with
explicit state**, not immediate mode. Immediate-mode toolkits (egui, dvui)
rebuild the whole view every frame and repaint everything; that is exactly what a
CPU-on-embedded target cannot afford. A retained tree lets a widget say "only *I*
changed" and have the renderer repaint a 44×24 toggle instead of a 1080p screen —
the property Phase 2's damage tracker exists to exploit. LVGL wins on this
hardware class for the same reason.

So widgets are **long-lived objects that own their state**. The loop is Elm-ish in
its *control flow* — `update(msg) → mutate state → mark damage → paint` — but it
is not Elm's "rebuild the view tree each update". State lives in the tree.

## 2. The tree

```
Ui<Msg>
├── taffy: TaffyTree            layout engine (flexbox/grid), one node per widget
├── nodes: SlotMap<WidgetId, Node<Msg>>
├── root: WidgetId
├── focus: Option<WidgetId>     keyboard focus
├── hover / capture: Option<WidgetId>
├── theme: Theme
├── fonts: FontContext          owned here; text measure + paint need it
└── damage: Vec<Rect>           logical dirty rects accumulated since last paint

Node<Msg>
├── widget: Box<dyn Widget<Msg>>
├── taffy: taffy::NodeId
├── parent / children: WidgetId links
├── layout: Rect                resolved absolute logical bounds (post compute)
└── flags: NEEDS_LAYOUT | NEEDS_PAINT
```

`WidgetId` is a generational `SlotMap` key, so a stale id from a removed widget is
detected, never silently aliasing a new one. The tree mirrors a taffy tree
1:1 — every widget node is a taffy node — so layout is taffy's job and we never
hand-roll flex math.

### Generic over `Msg`

`Ui<Msg>` and `Widget<Msg>` are parameterized by the application's message type.
A `Button<Msg>` holds `on_press: Option<Msg>` (with `Msg: Clone`); when clicked it
pushes that message to the Ui's output queue. The application implements:

```rust
trait App {
    type Message: Clone;
    fn update(&mut self, msg: Self::Message, ui: &mut Ui<Self::Message>);
}
```

`update` is where state changes happen: it mutates application state *and* the
retained widgets (e.g. `ui.with::<Label, _>(id, |l| l.set_text(...))`), marking
damage as it goes. This keeps the data-flow one-directional (events → messages →
update → widget mutations → damage → paint) without `Rc<RefCell>` soup, and keeps
`Widget` object-safe (the message type is fixed per Ui, not per call).

Trait objects over a generic param are fine because `Msg` is one concrete type for
the whole tree; `Box<dyn Widget<Msg>>` is a normal object.

## 3. The frame loop

```
            input (platform InputEvent)
                  │  translate to widget Event (logical coords)
                  ▼
   ┌────────► dispatch ──► widget.event(EventCtx) ──► emits Msg(s) + marks damage
   │              │
   │              ▼
   │         drain messages ──► App::update(msg, ui) ──► mutates widgets + damage
   │              │
   │              ▼
   │         relayout if any NEEDS_LAYOUT  (taffy compute on dirty subtrees)
   │              │
   │              ▼
   │         paint damaged nodes into Surface  (Phase 2 Painter)
   │              │
   │              ▼
   └───────── present damaged spans  (Phase 2 copy-out → Phase 1 Display)
```

Idle = no events = no messages = no damage = no paint = no present, so an idle UI
sleeps in `poll` at ~0% CPU (the property inherited from Phases 1–2).

### Damage propagation rules

These are the crux and are stated explicitly so widgets can't get them wrong:

1. **A widget marks itself dirty** by calling `ctx.request_paint()`. That unions
   the widget's *absolute layout rect* into `Ui::damage`.
2. **Geometry changes request layout.** If a mutation can change size/position
   (text length, visibility, a slider thumb that resizes), the widget calls
   `ctx.request_layout()`. Relayout recomputes the subtree; because a child's new
   size can move siblings, layout damage is taken as the **union of the node's old
   and new rects, walked up to the nearest clip/scroll boundary**.
3. **Painting a node repaints its subtree** within its bounds (children draw over
   the parent). Damage is per-node-rect, then merged by Phase 2's tracker.
4. **Opaque-background optimization (later):** a node with an opaque background can
   stop upward damage propagation — repainting it fully covers what's beneath. v1
   conservatively repaints the damaged union; the hook is there.
5. **Overlays (and popups, which are promoted overlays) damage both where the
   overlay is now and where it last painted**, padded one logical pixel for
   border ink that straddles the rect edge. The `Ui` applies this on any
   change to an overlay-bearing widget — including popup open/close/dismissal
   and tooltip show/hide — so appearance, movement, and disappearance always
   repaint cleanly.

The renderer already merges overlapping dirty rects and bounds copy-out, so widget
code over-reporting damage is safe (just less efficient), while under-reporting is
a bug — hence "when unsure, request_paint".

## 4. Layout

`taffy` owns layout. Each widget contributes a `taffy::Style` (display, flex
direction, size, padding, margin, gap, alignment). Leaf widgets with intrinsic
size (text, image) register a **measure function**: taffy calls back with the
available space, and the widget measures via `FontContext` (shaping a `TextLayout`
to get its size) or its image dimensions.

Relayout is incremental: only nodes flagged `NEEDS_LAYOUT` (and their ancestors,
since a child resize can change the parent) are recomputed. taffy caches
unaffected subtrees internally.

Coordinates are **logical**; the Ui is told the device size and scale once and
lays out in logical pixels. The painter applies the device transform (Phase 2).

## 5. Input & focus

Platform `InputEvent`s (physical pixels) are translated to widget `Event`s in
logical coordinates:

* **Pointer** (motion / button / scroll): hit-tested top-down through the tree to
  find the deepest node containing the point; hover state transitions emit
  enter/leave; press establishes a *capture* so the owning widget keeps receiving
  motion/release even if the pointer leaves it (drag, slider).
* **Touch**: mapped onto the same pointer path (down=press, move=motion+capture,
  up=release); gesture recognition (tap/long-press/fling) is Phase 4.
* **Keyboard**: routed to the focused widget. If unhandled, **Tab / Shift-Tab**
  move focus along the tab order (tree order over focusable widgets), and arrows
  can be widget-specific (slider step, list selection).

Focus is a single `Option<WidgetId>`. A focus change damages both the old and new
focused widgets (focus rings repaint). Pointer capture is a single
`Option<WidgetId>` too — one pointer in v1 (multi-touch capture is Phase 4).

**Bubbling:** scrolls, keys, recognized gestures (tap / long-press / fling), and
right-button presses bubble from the hit widget to its ancestors until one marks
the event handled — a wheel or fling over a label inside a `ScrollView` scrolls
the view, Esc inside a `Dialog` dismisses it, a right-click on any child reaches
an enclosing `ContextMenu`. Left-button presses/releases stay direct: two widgets
arming on one press would double-activate.

### The popup layer

A floating overlay (`Widget::overlay_rect` + `paint_overlay`) is paint-only by
itself (a toast). Registering it with `Ui::open_popup(owner, PopupOptions)` (or
`EventCtx::open_popup` from an event handler) promotes it into an interactive
**popup**:

* **Routing.** Pointer events inside a popup's overlay rect are dispatched to
  its owner *ahead of* capture and tree hit-testing, front-to-back through the
  popup stack (most recently opened on top). The owner hit-tests its own
  content (menu rows), exactly like the compound widgets do.
* **Dismissal.** A press outside every popup dismisses the dismissable ones
  top-down — each owner receives `Event::PopupDismissed` to sync its state —
  and the press is **consumed**, so a click-away can't activate whatever sits
  underneath. Scrolls outside are swallowed while a dismissable popup is open.
  Moves and releases fall through. An explicit `close_popup` delivers no event:
  the closer already knows.
* **Focus.** With `grab_focus` (default) the owner takes focus on open and the
  previous focus is restored on close/dismissal. While any popup is open, Tab
  is confined to the topmost owner's subtree — a menu can't tab out to the page
  under it. Widgets that are themselves the focus target (`Select`) turn the
  grab off.
* **Lifecycle.** Removing the owner drops its popup entry; entries whose owner
  stops reporting an overlay (closed by direct mutation, e.g. `set_options`
  while open) are pruned on the next event; `set_root` clears the stack; a
  surface resize re-runs `prepare_overlay` (the fonts-in-hand measuring hook)
  for open popups.
* **Damage.** Opening, closing, and dismissal all go through the existing
  overlay damage rule (below); no popup-specific damage machinery exists.

`Select`'s dropdown, `Menu`, and `ContextMenu` all sit on this layer; a capture
grab is no longer part of the overlay story. `place_anchored` (in `popup.rs`)
is the one placement rule they share: preferred side, flip when it doesn't fit
and the opposite side has more room, clamp (and shrink as a last resort) to the
surface.

### Tooltips

`Ui::set_tooltip(id, Tooltip)` attaches a hover tip to any widget. This is a
**Ui facility**, not a wrapper widget: hover is delivered only to the deepest
hit widget, so a wrapper around content would never see hover over its
children — the Ui, which already owns hover transitions, runs the state
machine. The dwell delay counts down in `Ui::animate` on the frame `dt`
(deterministic, no wall clock; the frame clock runs only while a dwell is
pending — a shown tip costs nothing). A long-press shows the tip immediately
(the touch path); any press, release, or key hides it. The tip paints above
overlays and participates in the scroll-blit fallback check like one.

## 6. Theming

A `Theme` is a plain value: palette (bg/surface/text/muted/accent/…), spacing
unit, corner radius, and a font stack, plus a `light`/`dark` selector. Widgets
read the theme from `PaintCtx`; switching theme at runtime damages the whole root.
No global mutable state — the theme lives in the `Ui`.

## 7. v1 widget set (PLAN §3.3)

| Widget | v1 scope |
|---|---|
| `Label` | text, wrap, alignment, color from theme; measured via FontContext |
| `Button` | label, hover/pressed/focused visuals, emits `on_press` Msg |
| `Checkbox` | bool state, toggles on click/Space, emits `on_toggle(bool)` |
| `Slider` | min/max/value, drag thumb + arrow keys, emits `on_change(f32)` |
| `TextInput` | single line, cursor + selection, basic editing; **no IME** |
| `Container` | `Row` / `Column` via flex direction; gap, padding, align |
| `Stack` | overlapping children, z-ordered by insertion — the overlay primitive |
| `ScrollView` | clips content, vertical offset via wheel/drag; kinetic = Phase 4 |
| `List` | windowed: only visible rows are laid out/painted (10k-row target) |
| `Image` | blits a decoded `fbui_render::Image`, object-fit contain |

Editing niceties (clipboard, multi-line, kinetic fling) are explicitly out of v1
per PLAN; the structure leaves room for them.

### Beyond the v1 set

The set above is not closed (§10). Widgets added since:

* **`Stack`** — a container that overlays its children instead of flowing them.
  The [`Ui`](crate::Ui) gives each child of a stacking container (one that
  reports `Widget::stacks_children`) `position: absolute` filling the stack, so
  children share a box and z-order by insertion (last on top, hit-tested first).
  This is the primitive overlays — modal scrims, toasts, popovers — build on. It
  does **not** by itself contain keyboard focus or grab input; a true *modal*
  layered on top needs focus containment, deferred to its own change.
* **`RadioGroup`** — a single-choice list of options as one widget and one tab
  stop, with arrow-key navigation within the group (Tab moves *between* groups).
  Emits `on_change(index)`, mirroring `Checkbox`'s `on_toggle`.
* **`Keyboard`** — an on-screen keyboard for touch kiosks: a docked,
  non-focusable key grid that paints its keys and hit-tests taps itself (one
  node, like `List`/`Select`), with QWERTY / Shift / `?123` symbols layers
  (Shift is one-shot; Backspace auto-repeats off the frame `dt` via
  `Widget::animate_with`). Two constraints shape it: it **never takes focus**
  (so the edited `TextInput` keeps it), and — since a widget can only emit a
  `Msg`, not inject a key event — it emits each tapped `Key` via `on_key`,
  which the app replays to the focused widget with **`Ui::send_key`**: the
  same event path as hardware input, so `on_change` and repaint behave
  identically. (`TextInput::apply_key` remains for direct programmatic edits;
  it shares the hardware path's implementation but fires no `on_change`.)
* **`TabBar`** — a segmented tab strip; exactly one tab is active. One node
  that paints equal-width segments and hit-tests taps itself (the
  `Keyboard`/`Select` pattern), a single tab stop with Left/Right/Home/End
  moving the selection. Emits `on_select(index)` only when the selection
  *changes*; the app swaps the content below in `update`. Equal widths keep
  the geometry independent of label text and host fonts.
* **`Spinner`** — an indeterminate activity indicator: a dot ring with a
  rotating brightness head. Runs from birth (inserting a widget arms one
  conservative animation tick, the same arm `Ui::with` uses) and is stopped /
  restarted via `set_running`. The head is quantized to whole dots, so it
  repaints only when a step actually changes pixels, and a stopped spinner
  costs nothing — the idle-burns-0% rule.

## 8. The `fbui` umbrella

`fbui` re-exports `fbui-render` + `fbui-widgets` (+ `fbui-platform` behind a
`platform` feature) and provides the runner that glues the loop to Phase 1:
`fbui::run(app, ui)` implements `PlatformHandler`, translating `InputEvent`s,
driving `update`, and presenting the damaged `Surface` each frame. Headless use
(tests, snapshots) drives the same `Ui` without the platform feature.

## 9. What this buys Phase 4+

Animation (Phase 5) hangs off `request_paint` on a frame clock; the opaque-damage
and scroll-blit fast paths slot into rule §3.4 and the ScrollView; GPU (Phase 6)
swaps the Phase 2 painter without touching this layer. Nothing here assumes
software rendering or a particular display — it only speaks `Painter`, `Event`,
and damage.

## 10. Writing a custom widget

The widget set in §7 is not a closed enum — it is just the first set of types to
implement the public `Widget<Msg>` trait. **A downstream crate adds a widget by
implementing that same trait**, with no privileged or internal API: the built-in
widgets and a third-party one are indistinguishable to the `Ui`, which stores
both as `Box<dyn Widget<Msg>>` and drives them through the same trait methods.

What a custom widget overrides depends on what it is:

| Concern | Hook(s) | Default |
|---|---|---|
| Box model (flex, size, padding) | `layout_style` | required |
| Intrinsic content size (text, image, a disc) | `measure` | `None` (size from style) |
| Drawing | `paint` | required |
| Input → messages + damage | `event` (via `EventCtx`) | ignore |
| Frame-clock animation | `animate` (drive a `Tween`) | `Anim::IDLE` |
| Keyboard focus / tab order | `focusable` | `false` |
| Clipping + scrolling | `clips`, `content_offset`, `set_scroll_metrics`, `scroll_blit` | non-clipping, no offset |
| App mutation by id (`Ui::with`) | `as_any_mut` | required |

A leaf with a fixed size is `layout_style` + `measure` + `paint` + `as_any_mut`;
an interactive, animated one adds `event`, `animate`, and `focusable`. The two
data-flow rules from §3 still hold: a widget never touches the `Ui` directly, and
mutation flows one way — events emit `Msg`s and request damage through the
`EventCtx`, the app folds those into state in `update`, and the next frame
repaints. The doctest on the `Widget` trait shows the minimal leaf; the
`custom_widget` example in the `fbui` crate (a tappable, pulsing `Dot`) walks
through the interactive and animated hooks end to end.

Because the trait is the whole contract, the umbrella `fbui` crate re-exports the
pieces an external implementor needs — the `widget`, `anim`, and `style` modules
alongside `Widget`, `Anim`, `PaintCtx`, and `EventCtx` — so a custom widget never
has to reach past `fbui` into the sub-crates.
