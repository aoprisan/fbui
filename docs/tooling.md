# Working on an fbui app without seeing the screen

> For an author who cannot look at the display: an AI agent writing the app, a
> CI job, a developer on a laptop with the device in a rack. Everything here is
> additive — nothing changes how an app is written. The design this implements
> is `TOOLING.md`; the verified-vs-pending status is `PHASE-TOOLING.md`.

## The loop

```sh
cargo build -p myapp --features platform
FBUI_BACKEND=headless FBUI_REPLAY=flows/add-item.txt ./target/debug/myapp
#   fbui: flow: 12 step(s) from flows/add-item.txt
#   fbui: flow failed at step 9 of 12:
#   line 14: expect #count text "1 item"
#     #count text="0 items", expected "1 item"
#   tree written to flows/add-item.fail.txt, shot to flows/add-item.fail.png
#   exit 1
```

Read the tree dump; open the PNG only when geometry is in question; fix; rerun.
No display, no tty, no root.

## 1. The headless backend

`FBUI_BACKEND=headless` runs the **unmodified runner** against two RAM back
buffers that present nowhere. No display device, no tty, no seat, no input
devices, no privileges.

| Variable | Meaning |
|---|---|
| `FBUI_BACKEND=headless` | Select it. |
| `FBUI_HEADLESS_SIZE=WxH` | Surface size (default `1024x600`). |

It is a *backend*, not a test runner, on purpose: the frame clock, gesture
recognizer, timers, `Proxy`, power policy, record/replay, monkey and remote
console are the real ones, so a headless result is evidence about the real app.
Three details keep that honest:

* **Buffer ages follow the DRM double-buffered sequence exactly**, so the
  partial-redraw path runs headless, not just the `age = 0` repaint-everything
  path a single buffer would force.
* **Rows are padded to 64 bytes**, so `stride != width * bpp` and any code that
  recomputes the stride — the one thing the whole stack promises never to do —
  breaks loudly in CI instead of quietly on a device whose pitch is padded.
* **Presents complete synchronously**, so the loop needs no pacing timer and an
  idle headless app still blocks in `poll` at ~0% CPU.

`SIGUSR1` simulates a hotplug / mode change (the reported mode flips to its
portrait swap), which drives `on_display_changed` off-device for the first time.

The whole console works headless, so a device emulator for the web console is:

```sh
FBUI_BACKEND=headless FBUI_REMOTE=8433 ./myapp
```

## 2. Reading the screen as text

Every widget reports its user-visible content and state through
`Widget::describe`, and `Ui::inspect_text()` renders the laid-out tree one
widget per line:

```
Container #page [0,0 480x640] direction=column gap=8 padding=12
  Label #title [12,12 456x20] "Gallery"
  Container #actions [12,40 456x36] direction=row gap=8
    Button #save [12,40 65x36] "Save"
    Button #delete [85,40 77x36] "Delete" variant=danger
  Checkbox #agree [12,84 456x20] "I agree" checked=true
  Slider #volume [12,198 456x14] value=30 min=0 max=100
  TextInput #item [12,232 456x36] "milk" cursor=4 focused
```

`Kind #name [x,y wxh] "text" prop=value` then live state (`focused`,
`hovered`, `hidden`). This is the file to read in nine cases out of ten. Get
one with:

| Where | How |
|---|---|
| In a test | `ui.inspect_text()` |
| From a replay | `FBUI_REPLAY_TREE=path.txt` (written when the screen settles, beside `FBUI_REPLAY_SHOT`) |
| From a device | `fbui-ctl tree`, or `GET /tree.txt` |
| As JSON | `Ui::inspect()`, or `GET /tree` |

The shortest way to see what an app looks like with no screen at all:

```sh
FBUI_BACKEND=headless FBUI_REPLAY=/dev/null \
    FBUI_REPLAY_TREE=t.txt FBUI_REPLAY_SHOT=s.png ./myapp
```

An empty replay file is a valid, already-finished recording, so this builds the
tree, renders the first frame, writes both artifacts and exits.

### Names

```rust
let inc = ui.add_named(row, "inc", Button::new("+").on_press(|| Msg::Inc));
ui.name(inc, "inc");              // or afterwards
ui.find("inc");                   // -> Option<WidgetId>
ui.find("form/name");             // scoped by named ancestors
```

Names are optional, tree-unique, and die with their widget. They are what a
flow's `#name` resolves against — and they replace the `Option<WidgetId>`
fields an app would otherwise keep for its own `ui.with` calls. Claiming a name
twice is a `duplicate-name` lint, not a crash: the last claim wins.

## 3. Flow scripts (`fbui-rec 2`)

A flow is an interaction written as steps and expectations. The v1 recording
format (`@ms` + raw event lines) stays valid; the header picks the format, and
the two may be mixed in one file.

```
fbui-rec 2

# actions — each goes through the real input path
tap #inc                      # press + release at the widget's centre
tap Button "Submit"           # by kind and describe-text (first match, tree order)
tap @240,112                  # logical coordinates
press #knob / move #target / release       # a drag, step by step
drag #list dy=-200            # a swipe; `fast` makes it a fling
wheel #list -3                # scroll notches (negative = toward the user)
type "milk"                   # one key per character, UTF-8
key Enter | key Ctrl+C | key Shift+Tab
long-press #row3

# timing
wait settle                   # until nothing animates, bounded at 300 frames
wait 500ms                    # or 0.5s

# expectations — a failure ends the run with exit 1, a tree and a shot
expect #count text "1 item"
expect #done checked          # any describe flag; also `expect #vol value 50`
expect #dialog absent | present | visible | hidden | focused
expect Label "Thanks, Ann!"   # exists (kind + text)
expect no-lints               # §5

# artifacts (both wait for the screen to settle first)
shot end.png
tree end.txt
```

**References** are `#name`, `Kind "text"` (or a bare `Kind`), or `@x,y`. A
`#name` may be a path (`#form/name`) scoped by named ancestors.

**Resolution happens when the step runs**, against the live tree — so a flow
follows the layout: `tap #inc` lands on the button wherever it moved to. A
reference that matches nothing, or matches a widget that is present but not on
screen, **fails the step** and prints the tree. That is the most common
authoring mistake, so it gets the most useful error rather than a silent no-op.

### Three executors, one meaning

| Executor | Where | Input enters as | Use |
|---|---|---|---|
| **Harness** | `fbui_widgets::harness::run_text(&mut ui, flow, update)` | `Ui::event` (the behavior-test path) | `cargo test` on a `Ui` the test builds |
| **Runner** | `FBUI_REPLAY=flow.txt` | platform events through the gesture recognizer | headless CI, device runs, monkey reproducers |
| **Remote** | `fbui-ctl run flow.txt` | `POST /input`, references resolved via `GET /tree` | a live device in the field, or the headless emulator |

Everything that decides *meaning* — parsing, resolution, expectations — lives
in `fbui_widgets::script`, so the three can only differ in how they deliver an
already-resolved action.

The harness synthesizes `Event::Tap`/`LongPress`/`Fling` directly, because
there is no gesture recognizer below a `Ui`; the runner sends raw contacts and
lets the real recognizer classify them. Those two are pinned to each other by
`flow::tests::synthesized_input_recognizes_as_the_gesture_the_harness_fakes`,
which feeds the runner's synthesized input for each gesture step through the
real recognizer and asserts it produces the gesture the harness fakes.

```rust
#[test]
fn adding_an_item_updates_the_count() {
    let (mut ui, mut app) = build();
    harness::assert_flow(
        &mut ui,
        include_str!("../flows/add-item.txt"),
        |msg, ui| app.update(msg, ui),
    );
}
```

### Timing

Semantic steps carry no timestamps; the runner paces them on the replay clock,
`FBUI_REPLAY_STEP` ms apart (default 50). A `drag` synthesizes intermediate
moves so the recognizer sees a plausible swipe: `fast` packs them a frame apart
(a fling), and an ordinary drag spaces them past the recognizer's velocity
window and pauses before lifting, so it never flings however far it went.
Because the clock is the recording clock, `FBUI_REPLAY_SPEED=max` still honors
long-press thresholds and fling velocities.

### A directory of flows is a test suite

```sh
for f in flows/*.txt; do
    FBUI_BACKEND=headless FBUI_REPLAY_SPEED=max FBUI_REPLAY=$f ./myapp || exit 1
done
```

`fbui/flows/` holds one per shipped example; CI runs all of them.

## 4. Traces: reading *why*

`FBUI_TRACE=path` (or `-` for stderr) writes one line per notable event on the
replay/wall clock:

```
@0	start	Headless 1024x600 scale=1
@0	frame	paint=183.6ms rects=1 area=3686400
@0	expect	line 11: expect #title text "Counter"  ok
@0	input	button down at 538,169 → #inc
@40	input	button up at 538,169 → #inc
@40	msg	Inc
@40	mutate	1 op(s), damage 1 rect(s) / 1800 px²
@40	expect	line 15: expect #count text "1"  ok
```

That is the causal chain **input → message → mutation → damage → frame**, which
makes "the button did nothing" a one-line diagnosis:

| What you see | What it means |
|---|---|
| no `input` line | the tap missed — check the `→ #name` on nearby lines |
| `input` but no `msg` | the widget has no callback (`on_press` missing) |
| `msg` but `mutate nothing` | `update` matched an arm that touches no widget |
| `mutate` but no `frame` | nothing was damaged, or the app exited first |

Messages need a textual form, and `App::Message` has no `Debug` bound, so the
app supplies one:

```rust
fn describe_message(&self, msg: &Msg) -> Option<String> {
    Some(format!("{msg:?}"))          // with #[derive(Debug)] on Msg
}
```

Without it, messages trace as `<msg>`. The trace is buffered and flushed per
frame, and the remote console keeps the last 500 lines at `GET /trace`
(`fbui-ctl trace --follow`) even when `FBUI_TRACE` is unset.

### Counters

`Ui::diagnostics()` / `take_diagnostics()` report mutations, damage rects and
area, layouts, paints, messages and events. They cost a few integer increments
and are always on, so a test can assert *cost* rather than only pixels:

```rust
ui.take_diagnostics();
ui.paint(&mut surface);
ui.paint(&mut surface);
assert_eq!(ui.take_diagnostics().paints, 0, "a clean frame does no work");
```

## 5. Lints: what an eye would catch

`Ui::lint()` walks the laid-out tree for the mistakes a glance catches and a
tree dump never will.

| Rule | Fires when |
|---|---|
| `touch-target` | a focusable widget smaller than the tappable minimum on both axes |
| `truncated-text` | text needs more width than its box and does not wrap |
| `unreachable-focus` | a focusable widget with no area, or entirely clipped away |
| `off-surface` | a widget wholly outside the surface **and** with no clipping ancestor |
| `overflow` | children escape a container that neither clips nor stacks them |
| `empty-scroll` | a scroll viewport with no content |
| `stacked-modals` | two `Dialog`s in the tree at once |
| `duplicate-name` | a name claimed by two live widgets |
| `undescribed` | a focusable widget whose `describe` reports nothing |
| `ambiguous-ref` | a flow's `Kind "text"` reference matches more than one widget |

```sh
FBUI_LINT=1 ./myapp        # report each finding once, to stderr or the trace
```

```rust
ui.set_touch_target(44.0);           // default 24; a touch-only kiosk wants 44
ui.allow_lint(id, Rule::TouchTarget); // the deliberate case
```

Rules are conservative: a lint that cries wolf gets switched off, and then it
catches nothing. The acceptance test is that all ten shipped examples are
clean — which is also how the pass earned its keep, by finding two real bugs
(the `showcase` pushed content off a 1024x600 screen; `custom_widget`'s `Dot`
described nothing).

`expect no-lints` in a flow turns the pass into a test.

## 6. `fbui-ctl`: the console from a shell

```sh
export FBUI_CTL=http://kiosk-7.local:8433   # token via FBUI_REMOTE_TOKEN
fbui-ctl tree                # GET /tree.txt
fbui-ctl shot out.png        # GET /screen.png
fbui-ctl tap '#inc'          # resolve via /tree, then POST /input
fbui-ctl type "milk"; fbui-ctl key Enter
fbui-ctl run flows/add-item.txt
fbui-ctl trace --follow
fbui-ctl metrics
```

Built with `--features remote`; `std::net` only, like the console it talks to.
This is the field-support loop: reproduce on the device, run the same flow
headless in CI, commit it.

Two things it cannot do, and says so rather than passing vacuously:
`expect no-lints` (the lint pass runs inside the `Ui`) and raw `@ms` event
lines (only the device's runner can replay a platform event). Modifiers are not
expressible over `/input`, so `key Ctrl+C` warns and sends the bare key.

## 7. The workflow

1. **Build headless.** `cargo build -p myapp --features platform`.
2. **Look.** `FBUI_BACKEND=headless FBUI_REPLAY=/dev/null FBUI_REPLAY_TREE=t.txt
   FBUI_REPLAY_SHOT=s.png ./myapp` — read `t.txt`; open `s.png` only when
   geometry is in question.
3. **Interact.** Write `flows/<feature>.txt` with `tap`/`type`/`expect`; run it
   headless; read the failure's tree dump.
4. **Understand.** `FBUI_TRACE=-` on the same run when a step does something
   unexpected.
5. **Check.** `FBUI_LINT=1`, and `expect no-lints` in the flow.
6. **Commit the flow.** CI runs every flow headless, with a pinned monkey seed
   beside them.

## Variable reference

| Variable | Meaning |
|---|---|
| `FBUI_BACKEND=headless` | RAM buffers, no display/tty/seat |
| `FBUI_HEADLESS_SIZE=WxH` | headless surface size (default `1024x600`) |
| `FBUI_REPLAY=path` | play a v1 recording or a v2 flow (header picks) |
| `FBUI_REPLAY_SPEED=n\|max` | clock multiplier |
| `FBUI_REPLAY_STEP=ms` | gap between flow steps (default 50) |
| `FBUI_REPLAY_SHOT=path.png` | settled screenshot at the end |
| `FBUI_REPLAY_TREE=path.txt` | settled tree dump at the end |
| `FBUI_REPLAY_EXIT=0\|1` | stay running / exit when playback ends |
| `FBUI_TRACE=path\|-` | write the event trace |
| `FBUI_LINT=1` | run the lint pass after every layout |
| `FBUI_REMOTE=port` | the remote console (works headless) |
| `FBUI_CTL=http://host:port` | which console `fbui-ctl` talks to |
| `FBUI_CTL_STEP=ms` | gap between `fbui-ctl run` steps (default 120) |

See also: `docs/record-replay.md`, `docs/remote-console.md`,
`docs/monkey-testing.md`.
