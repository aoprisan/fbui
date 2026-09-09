# Tooling design: working on an fbui app without seeing the screen

> The mini-plan for the developer-tooling track. Its audience is an author who
> cannot look at the display: an AI agent writing the app, a CI job, a
> developer on a laptop with the device in a rack. Everything here is
> additive to the existing runner and toolkit; nothing changes how an app is
> written. Status: **implemented** — see [`docs/tooling.md`](docs/tooling.md)
> for how to use it and [`PHASE-TOOLING.md`](PHASE-TOOLING.md) for what is
> verified, what is still open, and where the implementation deliberately
> deviates from this design. (An Elm-architecture layer was designed first and
> set aside: verification tooling turned out to be the actual need, and it
> works with the retained API as it is.)

## 0. The loop this exists to close

An author who cannot see the screen still has to answer "what is on it now?"
after every change. Today the honest answer is: build for a device, or run
under a terminal emulator with a real tty, look with your eyes. Neither is
available to an agent or a CI job. What closes the loop is a sequence that
needs no display, no tty, no root, and produces **text and files**:

```sh
cargo build -p myapp
FBUI_BACKEND=headless FBUI_REPLAY=flows/add-item.txt ./target/debug/myapp
#   tap #add            ok
#   type "milk"         ok
#   key Enter           ok
#   expect #count text "1 item"     FAILED: text="0 items"
#   tree written to flows/add-item.fail.txt, shot to flows/add-item.fail.png
#   exit 1
```

Then read the tree dump (a widget per line with its text, value and state),
look at the PNG if geometry matters, fix, rerun. Every piece of that loop is
the subject of one section below:

| Piece | Section | Crate |
|---|---|---|
| Run the real runner with no display or tty | §1 headless backend | `fbui-platform` |
| Address widgets by name and see their content | §2 names + `describe` | `fbui-widgets` |
| Write a flow as steps and expectations, not coordinates | §3 script v2 | `fbui-widgets` + `fbui` |
| Run the same flow in `cargo test`, headless, or on a device | §3.4 three executors | all three |
| Read *why* something happened | §4 traces + diagnostics | `fbui` + `fbui-widgets` |
| Catch what an eye would catch | §5 lints | `fbui-widgets` |
| Drive a live device from a shell | §6 `fbui-ctl` | `fbui` (`remote`) |

What already exists and is reused rather than rebuilt: input record/replay
(`docs/record-replay.md`), the monkey tester, `Ui::inspect` and the remote
console's `/tree`, `/screen.png` and `/input` endpoints, `Ui::request_screenshot`,
`Surface::write_png`, and `fbui-testkit`'s tolerant PNG compare.

Three rules hold across all of it:

1. **Tooling never changes app behavior.** Scripted input enters through the
   same path as evdev events (gestures, focus, `App::update`); screenshots
   come from the shadow surface; the headless backend runs the unmodified
   runner. If a flow passes headless it passes on the device, modulo fonts.
2. **Zero cost when unused.** Names are a side map, `describe` runs only on
   inspect, traces and lints only when asked. The idle-0% rule is untouched.
3. **One grammar, three executors.** A flow file means the same thing in an
   in-process test, under the headless runner, and against a device over the
   remote console.

## 1. The headless backend

`FBUI_BACKEND=headless` selects a `Display` that owns two RAM back buffers
and presents nothing, plus an empty input-source list, no VT, no seat.
`FBUI_HEADLESS_SIZE=1024x600` (default) and `FBUI_SCALE` fix the surface. It
is the fourth `BackendKind` and lives in `fbui-platform/src/display/headless.rs`
behind a feature `headless` that is **on by default** (it needs nothing from
the system, like the other default backends).

Why a platform backend and not a separate "test runner": the point is to run
the *same* `fbui::run` — frame clock, gesture recognizer, timers, `Proxy`,
power policy, record/replay, monkey, remote console — so a headless run is
evidence about the real app, not about a stand-in. The terminal backend was
meant to be this path but `TtyGuard::acquire` refuses a non-tty (correctly:
it must restore termios), so CI has never actually run a replay.

Details that matter:

* **Two buffers, real `Frame::age`.** Presenting flips between the two RAM
  buffers and reports ages `1`/`2` like DRM does, so the partial-redraw path
  is exercised headless, not just the `age = 0` repaint-everything path.
* **Idle blocks.** `wait_events` blocks on the waker fd with the runner's
  timeout, exactly like the device backends; a headless app burns 0% while
  idle, and replay pacing works because the loop is the same loop.
* **Frames are observable.** The remote console (`FBUI_REMOTE`) works
  headless — `/screen.png` serves the last presented buffer — so a device
  emulator for the web console is `FBUI_BACKEND=headless FBUI_REMOTE=8433`.
* **Hotplug can be simulated.** `SIGUSR1` (or a remote `/display?size=`)
  swaps the reported mode and fires `on_display_changed`, which today can
  only be tested on VKMS.

Exit criterion: `.github/workflows/ci.yml` gains a job that builds the
examples with `platform` and runs `FBUI_BACKEND=headless FBUI_MONKEY=1` and a
committed flow with `FBUI_REPLAY_SHOT` on the standard runner, no `sudo`, no
`modprobe`.

## 2. Names and `describe`: the tree as text

### 2.1 Names

```rust
let add = ui.add_child(row, Button::new("+").on_press(|| Msg::Inc));
ui.name(add, "inc");                      // or:
let add = ui.add_named(row, "inc", Button::new("+").on_press(|| Msg::Inc));
```

A name is an app-assigned, tree-unique string stored in a
`SecondaryMap<WidgetId, Name>` with a reverse `HashMap<Name, WidgetId>`.
`Ui::find(name) -> Option<WidgetId>` resolves it; removal clears both maps. A
duplicate is a `debug_assert` and a lint (§5). Names are optional: nothing
existing changes, and flows can address unnamed widgets by kind and text
(§3.2), so the shipped examples work with zero edits. Names replace the
`label: Option<WidgetId>` fields apps keep today for their own `ui.with`
calls, which is a small ergonomic win on its own.

### 2.2 `describe`

`InspectNode` today carries the type name, bounds, and focus/hover flags. It
cannot say what a label reads or what an input holds, which is the single
biggest gap for a reader of text. One new trait hook:

```rust
pub trait Widget<Msg> {
    /// Report this widget's user-visible content and state for inspectors,
    /// scripts, and traces. Called only on `Ui::inspect`. Default: nothing.
    fn describe(&self, out: &mut Describe) {}
}

pub struct Describe { /* ordered (key, value) pairs */ }
impl Describe {
    pub fn text(&mut self, s: &str);          // the primary visible text
    pub fn value(&mut self, v: impl Display); // the primary value
    pub fn flag(&mut self, key: &'static str, on: bool);
    pub fn prop(&mut self, key: &'static str, v: impl Display);
}
```

Every built-in widget implements it, with `text` reserved for what a person
would read off the widget (this is what `expect … text` and `Button "Submit"`
addressing match against):

| Widget | `text` | `value` / props |
|---|---|---|
| `Label` | the text | `wrap`, `bold` |
| `Button` | the label | `variant` |
| `Checkbox` / `Switch` | the label | `checked` / `on` |
| `RadioGroup` | – | `selected`, `options=n` |
| `Slider` / `ProgressBar` / `Gauge` | – | `value`, `min`, `max` |
| `TextInput` / `TextArea` | the current text | `placeholder`, `cursor`, `selection` |
| `List` / `TreeView` | – | `rows`, `selected`, `first_visible` |
| `ScrollView` | – | `offset`, `content=WxH`, `viewport=WxH` |
| `Select` / `TabBar` | selected option label | `selected`, `open` |
| `Navigator` | – | `depth`, `top`, `transitioning` |
| `Toasts` | latest toast text | `count` |
| `Dialog` / `Stack` / `Container` | – | `direction`, `gap` (containers) |
| `Spinner` / `Chart` / `VideoView` / `Calendar` / `Keyboard` | – | `running` / `samples` / `date` / `layer` |

`InspectNode` grows `name: Option<String>` (the app-assigned name; the type
name field becomes `kind` — a rename the remote JSON and console pick up in
the same change), `props: Vec<(String, String)>`, and `visible: bool` (false
when fully clipped by a scroll ancestor or off-surface — what "can I tap it"
needs). A `Ui::inspect_text() -> String` renders the tree one node per line:

```
Container [0,0 1024x600] direction=column
  Label [24,24 976x34] "Counter"
  Label #count [24,74 976x58] "3"
  Container [24,148 976x40] direction=row
    Button #dec [24,148 60x40] "−"
    Button #inc [96,148 60x40] "+" focused
```

This is the file an agent reads instead of the PNG in nine cases out of ten.
The remote console serves it at `GET /tree.txt`; `FBUI_REPLAY_TREE=path`
writes it next to `FBUI_REPLAY_SHOT`; the harness returns it as a string.

## 3. Flow scripts: `fbui-rec` v2

### 3.1 Grammar

The v1 recording format (`@ms` + raw event lines) stays valid and unchanged;
v2 adds **semantic lines** with no timestamp, resolved at execution time.
Header `fbui-rec 2`; the size field becomes optional for semantic-only files.

```
fbui-rec 2

# actions — each goes through the real input path
tap #inc                      # press + release at the widget's center
tap Button "Submit"           # by kind and describe-text (first match, tree order)
tap @240,112                  # logical coordinates
press #knob / move #target / release      # a drag, step by step
drag #list dy=-200            # a swipe: press, N moves, release (fling if fast=)
wheel #list -3                # scroll notches
type "milk"                   # one key per char, UTF-8
key Enter | key Ctrl+C | key Shift+Tab
long-press #row3

# timing
wait settle                   # until !is_animating, bounded (300 frames) like a shot
wait 500ms

# expectations — a failure ends the run with exit 1 and a tree + shot
expect #count text "1 item"
expect #done checked          # any describe flag / prop:  expect #vol value 50
expect #dialog absent | present | visible | focused
expect Label "Thanks, Ann!"   # exists (kind + text)
expect no-lints               # §5

# artifacts
shot end.png                  # settled screenshot (waits like `wait settle`)
tree end.txt
```

Widget references are `#name`, `Kind "text"`, or `@x,y`; `#name` may be a
path (`#form/name`) when names are scoped inside a `Navigator` screen or a
`Dialog` (§2.1 keeps names unique per tree; scoping is sugar for
`screen-name/field`). A reference that resolves to nothing, or to a widget
that is not `visible`, fails the step with the tree dump — the most common
authoring mistake gets the most useful error.

### 3.2 Resolution

A semantic line resolves against `Ui::inspect()` *at the moment it runs*, so
a flow follows the layout: `tap #inc` lands on the button wherever it moved.
Kind + text matches on `InspectNode.kind` and the `text` from `describe`,
first in tree order; ambiguity is fine for authoring (`tap Button "OK"`) but
the lint pass warns when a `Kind "text"` reference in a committed flow is
ambiguous, since the next layout change may flip which one wins.

Actions become a small `Action` enum (`Tap(Point)`, `Press`, `Release`,
`Move`, `Wheel`, `Key`, `Text`, `Wait`, `Expect`, `Shot`, `Tree`) with the
reference already resolved to logical coordinates. The parser and resolver
live in `fbui-widgets/src/script.rs` — they need only `InspectNode` and
`Event` — and are unit-tested there against hand-built trees.

### 3.3 Timing

Semantic lines have no timestamps, so the executor paces them on the replay
clock: a fixed step (`FBUI_REPLAY_STEP`, default 50 ms) between actions, and
`drag` synthesizing intermediate moves at the step so the gesture recognizer
sees a plausible swipe (`fast=` makes it a fling). The clock is the existing
replay clock, so `FBUI_REPLAY_SPEED=max` still honors long-press thresholds
and fling velocities (the property `docs/record-replay.md` already
guarantees). Mixed files — raw v1 lines with `@ms` and semantic lines — run
in file order, the semantic ones taking the clock from where the last raw
line left it.

### 3.4 Three executors, one meaning

| Executor | Where | Input enters as | Use |
|---|---|---|---|
| **In-process harness** | `fbui_widgets::harness::run(&mut ui, script)` | `Ui::event` (widget `Event`s, the behavior-test path) | `cargo test` on a `Ui` built in the test; snapshot via `fbui-testkit` |
| **Runner replay** | `FBUI_REPLAY=flow.txt` (v2 detected by header) | `InputEvent`s through `Runner::handle_input`, like a recording | headless CI, device runs, monkey reproducers |
| **Remote** | `fbui-ctl run flow.txt` (§6) | `POST /input`, references resolved via `GET /tree` | a live device in the field or the headless emulator |

The harness path skips the gesture recognizer (there is none below the
runner) and therefore synthesizes `Event::Tap`/`LongPress`/`Fling` directly,
the way `tests/behavior.rs` does; the other two go through recognition. A
test in `fbui` runs the same flow through the harness and through the
headless runner and asserts identical `expect` outcomes and tree dumps, which
pins the two paths to each other — the "fast path never diverges" rule
applied to test infrastructure.

`expect` failures produce: the failing line, the actual value, the tree dump
and a shot written beside the flow (`<flow>.fail.txt` / `.fail.png`), exit
code 1 under the runner, a `panic!` with the same text in the harness. A
passing headless run exits 0, which makes `FBUI_REPLAY` a test runner and a
directory of flows a test suite: `for f in flows/*.txt; do FBUI_BACKEND=headless
FBUI_REPLAY=$f ./app || exit 1; done`, or the equivalent `#[test]` per flow.

Recording gets a small upgrade in the same change: when a live tap lands on a
named widget the recorder appends `# tap #name` as a comment on the raw line,
so a recorded session is easy to convert into a semantic flow by hand.

## 4. Traces and diagnostics: reading why

### 4.1 `FBUI_TRACE`

`FBUI_TRACE=path` (or `-` for stderr) makes the runner write one line per
notable event, on the replay/wall clock:

```
@0      start   headless 1024x600 scale=1
@120    input   tap 96,148 → Button #inc
@120    msg     Inc                          # App::describe_message
@121    mutate  Label #count with            # Ui::with / stream / add / remove
@121    damage  [24,74 976x58]
@121    focus   Button #inc
@137    frame   paint=0.8ms rects=1 area=56608
@1000   timer   Tick                          # Proxy timers, send_after/every
@1002   proxy   Progress(42)                  # cross-thread messages
@2000   lint    touch-target Button #tiny 28x20 < 44
@2500   expect  #count text "3"  ok
```

Messages need a textual form: `App::Message` has no `Debug` bound and adding
one would be a breaking change, so `App` gets
`fn describe_message(&self, msg: &Self::Message) -> Option<String>` with a
default of `None` (traced as `<msg>`); an app with `#[derive(Debug)]` returns
`Some(format!("{msg:?}"))`. This is what lets an author follow the causal
chain input → message → mutation → damage → frame without a debugger, and
what makes "the button did nothing" a one-line diagnosis (no `msg` line after
the `input` line: the callback is missing; a `msg` but no `mutate`: `update`
matched the wrong arm).

The trace is text so it can be grepped and diffed; it is written from the
runner thread with a buffered writer flushed per frame, and it is off unless
the variable is set. The remote console exposes the tail at `GET /trace`.

### 4.2 `Ui::diagnostics()`

A plain counter struct the `Ui` maintains always (a few integer increments
per operation): mutations by kind, damage rects and area, layout passes,
paint calls, messages emitted. `Ui::take_diagnostics()` resets it. The
harness exposes it so tests can assert cost, not just pixels:

```rust
let d = ui.take_diagnostics();
assert_eq!(d.damage_area, 0, "a no-op message must not repaint");
```

This is the general form of the "unchanged render produces no damage"
invariant, usable by any app or widget test today.

## 5. Lints: what an eye would catch

`Ui::lint() -> Vec<Lint>` walks the laid-out tree and reports the things a
person notices at a glance and a text reader never will:

| Lint | Rule |
|---|---|
| `duplicate-name` | two widgets share a name |
| `touch-target` | a focusable/tappable widget smaller than 44×44 logical px (kiosks are touch) |
| `unreachable-focus` | a focusable widget with zero area or fully clipped |
| `truncated-text` | a `Label`/`Button` whose measured text exceeds its bounds without `wrap` |
| `off-surface` | a widget partly or fully outside the surface |
| `overflow` | a non-stacking container whose children overlap or exceed its box |
| `empty-scroll` | a `ScrollView` with no content (usually a forgotten `add_child`) |
| `ambiguous-ref` | a `Kind "text"` flow reference matching more than one widget (from the script resolver) |
| `stacked-modals` | two `Dialog`s active at once |

Each `Lint` carries the rule, the widget (`kind`, name, bounds) and a
one-line message. The runner runs the pass after every layout when
`FBUI_LINT=1` and writes to the trace/stderr; `expect no-lints` in a flow
turns it into a test; the harness returns the list. Rules are conservative
(no false positives on the shipped examples is the acceptance test) and each
has an allow-list hook (`Ui::allow_lint(id, rule)`) for deliberate cases.

## 6. `fbui-ctl`: the remote console from a shell

A small binary in the `fbui` crate (`[[bin]] name = "fbui-ctl"`,
`required-features = ["remote"]`, `std::net` only like the console itself)
wrapping the HTTP API:

```sh
export FBUI_CTL=http://localhost:8433   # token via FBUI_REMOTE_TOKEN
fbui-ctl tree                # GET /tree.txt
fbui-ctl shot out.png        # GET /screen.png
fbui-ctl tap '#inc'          # resolve via /tree, POST /input
fbui-ctl type "milk" ; fbui-ctl key Enter
fbui-ctl run flows/add-item.txt      # the remote executor (§3.4)
fbui-ctl trace --follow      # GET /trace
```

This is the third executor and the field-support loop: reproduce on the
device, `fbui-ctl run` the same flow headless in CI, commit it. No new
server-side capability is needed beyond `/tree.txt` and `/trace`.

## 7. What the author's workflow becomes

Written down because it is the acceptance test for the whole track, and it
goes into `CLAUDE.md` when it works:

1. Build headless: `cargo build -p app --features platform`.
2. Look: `FBUI_BACKEND=headless FBUI_REPLAY_TREE=t.txt FBUI_REPLAY_SHOT=s.png
   FBUI_REPLAY=/dev/null ./app` renders the initial screen and exits; read
   `t.txt`, open `s.png` only when geometry is in question.
3. Interact: write `flows/<feature>.txt` with `tap`/`type`/`expect`; run it
   headless; read the failure's tree dump.
4. Understand: `FBUI_TRACE=-` on the same run when a step does something
   unexpected.
5. Check: `FBUI_LINT=1` and `expect no-lints` before committing.
6. Commit the flow; CI runs every flow headless with a pinned monkey seed
   beside them.

For in-process tests the same flow text drives `harness::run` on a `Ui` the
test builds, with `fbui-testkit` goldens for the pixels.

## 8. Changes by crate

`fbui-platform`: `display/headless.rs` + `BackendKind::Headless`, feature
`headless` (default), `FBUI_BACKEND=headless`, `FBUI_HEADLESS_SIZE`, simulated
mode change.

`fbui-widgets`: `Ui::name/add_named/find`, `Widget::describe` + `Describe`,
`InspectNode { kind, name, props, visible }` + `inspect_text`, `script.rs`
(parser, resolver, `Action`), `harness.rs` (in-process executor, `run`,
`expect` evaluation, diagnostics access), `Ui::diagnostics/take_diagnostics`,
`lint.rs` + `Ui::lint/allow_lint`, `describe` on every widget.

`fbui`: v2 detection in `record.rs` and the runner executor with exit codes,
`FBUI_REPLAY_TREE`, `FBUI_REPLAY_STEP`, recorder name comments,
`App::describe_message`, `FBUI_TRACE` writer, `FBUI_LINT`, remote `/tree.txt`
+ `/trace` + console updates for `kind`/`name`/`props`, `fbui-ctl`.

`fbui-testkit`: unchanged (the harness snapshots through it as today).

Docs: `docs/tooling.md` (grammar, variables, the workflow), updates to
`record-replay.md`, `remote-console.md`, `CLAUDE.md` §"Build, lint, test".
`CHANGELOG.md` entries per step.

## 9. Non-goals and open questions

**Non-goals.** A new crate or a new app API; a DSL beyond the flow grammar;
pixel-level assertions in flows (that is what goldens are for — `shot` plus
`fbui-testkit` covers it); an accessibility API (though `describe` is the
data one needs, and AccessKit can consume it later); Windows/macOS hosts (the
headless backend is still Linux-only because the runner is).

**Open questions, with the lean.**

1. *Should `describe` be required on `Widget`?* Lean: default-empty, so
   third-party widgets keep compiling; the lint pass flags a focusable widget
   with no `describe` output as `undescribed` so the gap is visible.
2. *Name scoping.* Lean: names are tree-unique and `#screen/field` is sugar
   resolved by walking ancestors' names. Revisit if apps with many screens
   want true per-screen scopes.
3. *`InspectNode.name` rename to `kind`.* Lean: do it now, pre-1.0, with a
   changelog entry; the only consumer is the remote console in this repo.
4. *Harness location.* Lean: in `fbui-widgets` (it needs `Ui` internals for
   diagnostics and the same `Event` path as the behavior tests), gated by a
   `harness` feature that `fbui` and tests enable, keeping the default build
   free of the parser.
5. *Fonts headless.* Text metrics differ by host fonts, so `expect … text`
   is stable but `shot` goldens are only comparable with `bundled-font` or a
   pinned font set (CI installs one today). Lean: document it; flows assert
   text, goldens pin pixels only where the test bundles the font.

## 10. Plan

Ordered so each step is useful alone and the next builds on it.

**Step 1 — headless backend.** `fbui-platform` backend + runner selection;
`FBUI_REPLAY_TREE`. *Exit:* CI job runs `counter`, `form`, `showcase` headless
with `FBUI_MONKEY=1` and takes a settled shot of each on the stock runner;
`Frame::age` alternates 1/2 in a unit test.

**Step 2 — names, `describe`, `inspect_text`.** *Exit:* every widget has
`describe` with a unit test asserting its `text`/`value`; `inspect_text` of
the showcase is a committed golden text file (diffs in review show exactly
what changed on screen); remote console shows names and props.

**Step 3 — flow scripts and the two local executors.** Parser/resolver,
harness, runner v2 replay, exit codes, fail artifacts. *Exit:* a flow per
shipped example under `fbui/flows/`, run by a `#[test]` through the harness
and by CI through the headless runner, with the parity test of §3.4; the
monkey's reproducer doc gains "trim to a semantic flow".

**Step 4 — traces, diagnostics, lints.** *Exit:* `FBUI_TRACE` documented with
the "button did nothing" diagnosis walkthrough; `take_diagnostics` used by at
least the no-op-damage test in `fbui-widgets`; lints produce zero findings on
the shipped examples and one intentional `touch-target` in a test.

**Step 5 — `fbui-ctl` and docs.** Remote executor, `/tree.txt`, `/trace`,
`docs/tooling.md`, the §7 workflow in `CLAUDE.md`. *Exit:* the same flow
passes through all three executors against the headless emulator; a
`PHASE-TOOLING.md` records verified-vs-pending criteria in the style of
`PHASE5.md`.
