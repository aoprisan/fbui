# Developer tooling — working on an fbui app without seeing the screen

The track designed in [`TOOLING.md`](TOOLING.md) and documented for users in
[`docs/tooling.md`](docs/tooling.md). Its audience is an author who cannot look
at the display: an AI agent writing the app, a CI job, a developer with the
device in a rack. It spans every crate, so the summary lives at the root, in
the style of `PHASE4.md`/`PHASE5.md`.

## What's here

| Step (TOOLING §10) | Where | Status |
|---|---|---|
| **1** Headless backend | `fbui-platform`: `display/headless.rs`, `BackendKind::Headless`, feature `headless` (default), `seat::NullSeat` | ✅ done & tested |
| `FBUI_REPLAY_TREE` | `fbui/src/run.rs` | ✅ done |
| **2** Names, `describe`, `inspect_text` | `fbui-widgets`: `describe.rs`, `Widget::describe`, `Ui::name`/`add_named`/`find`, `InspectNode { kind, name, text, props, visible }`, `Ui::inspect_text` | ✅ done & tested (golden dump) |
| **3** Flow scripts + harness + runner replay | `fbui-widgets`: `script.rs`, `harness.rs`; `fbui`: `flow.rs`, v2 detection in `run.rs`, `fbui/flows/*` | ✅ done & tested |
| **4** Traces, diagnostics, lints | `fbui`: `trace.rs`, `App::describe_message`, `FBUI_LINT`; `fbui-widgets`: `Ui::diagnostics`, `lint.rs` | ✅ done & tested |
| **5** `fbui-ctl`, `/tree.txt`, `/trace`, docs | `fbui/src/bin/fbui-ctl.rs`, `fbui/src/remote/{http,hub,json}.rs`, `docs/tooling.md` | ✅ done; flow-executor parity verified by hand (see below) |

## Exit criteria, verified vs pending

| Criterion | Status |
|---|---|
| CI runs the examples headless with `FBUI_MONKEY` and a committed flow, no `sudo`, no `modprobe` | ✅ the `headless` job in `.github/workflows/ci.yml` runs every flow, lints every example, and monkeys three of them |
| `Frame::age` exercises the partial-redraw path headless | ✅ `display::headless::tests` and `tests/headless.rs` pin the DRM double-buffered sequence — see the deviation note below |
| Every widget has `describe`, with a unit test on its `text`/`value` | ✅ `fbui-widgets/tests/describe.rs`, including `no_built_in_widget_is_silent` |
| `inspect_text` of a gallery is a committed golden | ✅ `fbui-widgets/tests/snapshots/gallery.tree.txt` (regenerate with `FBUI_UPDATE_SNAPSHOTS=1`) |
| The remote console shows names and props | ✅ `/tree` JSON gained `kind`/`name`/`text`/`props`/`visible`; the built-in console renders them |
| A flow per shipped example, run by CI through the headless runner | ✅ `fbui/flows/{counter,form,big_list}.txt` |
| A flow run by a `#[test]` through the harness | ✅ `fbui-widgets/tests/flow.rs` (against trees the tests build) |
| The §3.4 parity test | ⚠️ **partially**: `flow::tests::synthesized_input_recognizes_as_the_gesture_the_harness_fakes` pins the two input paths to each other, which is the property that could actually diverge. Running one flow end to end through *both* executors in one process is not automated — the runner takes over the process (env vars, signals), so it would have to shell out to a built example. Done by hand instead; see below. |
| `FBUI_TRACE` documented with the "button did nothing" walkthrough | ✅ `docs/tooling.md` §4 |
| `take_diagnostics` used by a no-op-damage test | ✅ `fbui-widgets/tests/lint.rs` |
| Lints produce zero findings on the shipped examples, and one intentional `touch-target` in a test | ✅ all ten examples are clean (enforced by CI); `a_tiny_button_is_a_touch_target_finding` |
| The same flow passes through all three executors against the headless emulator | ✅ verified by hand: `fbui/flows/counter.txt` passes through the harness (as `tests/flow.rs`'s equivalent tree), through `FBUI_REPLAY` headless, and through `fbui-ctl run` against `FBUI_BACKEND=headless FBUI_REMOTE=…`. Only the first two are automated. |

## Design decisions worth knowing

- **The headless backend is a backend, not a test runner.** It runs the
  *unmodified* `fbui::run` — frame clock, gestures, timers, `Proxy`, power
  policy, record/replay, monkey, remote console — with the display replaced, so
  a headless result is evidence about the real app. Three details keep that
  honest rather than merely convenient: buffer ages follow the DRM sequence
  (the partial-redraw path runs), rows are padded to 64 bytes (so
  `stride != width * bpp` and stride-recomputing code fails loudly in CI), and
  presents complete synchronously (so the loop still blocks in `poll` at ~0%
  CPU).
- **Meaning lives in one place.** Parsing a flow, resolving a reference and
  evaluating an expectation are all in `fbui_widgets::script`; each executor
  only translates an already-resolved `Act` into its own kind of input. Three
  executors that each re-implemented the semantics would drift, and then a
  green CI run would prove nothing about the device.
- **Resolution happens when a step runs**, never at parse time, so a flow
  follows the layout. A reference that resolves to nothing — or to a widget
  that is present but clipped off screen — *fails the step*: silently tapping
  a widget that isn't there is the failure mode that makes a passing flow
  worthless.
- **`describe` is default-empty**, so third-party widgets keep compiling; the
  `undescribed` lint makes the gap visible instead. It runs only from
  `inspect`, never on the paint or event path.
- **Lints are conservative by construction.** A lint that cries wolf gets
  switched off, and then it catches nothing — so the acceptance test is zero
  findings on the shipped examples, enforced in CI.

## Deviations from `TOOLING.md`, and why

- **Buffer ages are `0, 0, 2, 2, …`, not "1/2".** §1 says the headless backend
  should report "ages 1/2 like DRM does". DRM's actual accounting with two
  buffers yields `0` for each buffer's first use and `2` from then on, and
  matching DRM *exactly* is the point of the exercise, so the code and its test
  pin the real sequence.
- **A duplicate name is a lint, not a `debug_assert`.** §2.1 says both. An
  assert makes the rule untestable (tests run in debug) and turns a diagnostic
  aid into a crash, which is the wrong trade for a name. The collision is
  recorded, the last claim wins so `find` stays unambiguous, and
  `duplicate-name` reports it — which `FBUI_LINT=1` prints and
  `expect no-lints` fails a flow on.
- **The `touch-target` default is 24 logical px, not 44.** §5 names 44, which
  is the platform guideline; this toolkit's own controls are ~36 px tall by
  theme, so a 44 default would fire on nearly every well-built screen and the
  rule would be switched off. The default flags what is clearly too small to
  hit, and `Ui::set_touch_target(44.0)` is one line for a touch-only kiosk.
- **`off-surface` ignores widgets with a clipping ancestor.** §5 says "partly
  or fully outside the surface". Content scrolled below the fold is off the
  surface *legitimately* — that is what a scroll view is for — so the rule
  fires only for a widget wholly outside the surface with nothing clipping it.
- **`fbui-ctl` cannot run `expect no-lints`** (the lint pass runs inside the
  `Ui`, and the console has no endpoint for it) or raw `@ms` lines. It says so
  and exits non-zero rather than passing an expectation it never checked.
  Modifiers are not expressible over `/input`, so `key Ctrl+C` warns and sends
  the bare key.
- **`fbui-widgets` gained a default feature.** `harness` (the flow parser and
  in-process executor) is on by default rather than off: the umbrella runner
  needs it, and an off-by-default parser is an untested one. `cargo check -p
  fbui-widgets --no-default-features` is in the CI matrix so the minimal build
  stays honest.

## What the pass found

The lint rules earned their keep before they shipped, by finding two real bugs
in the shipped examples:

- **`showcase` pushed its content off a 1024x600 screen.** Its panels row could
  not shrink below the intrinsic height of a 50-row `List`, so the page was
  1232 px tall and the status line — and half the list — were unreachable. The
  fix needed a new `Container::shrink()` (the flexbox `min-size: 0` idiom), and
  the example now windows properly at 1024x600.
- **`custom_widget`'s `Dot` described nothing**, which is exactly what the
  `undescribed` rule is for — and a poor advertisement in the example that
  teaches how to write a widget.

## Still open

- **Automating the three-executor parity run.** It needs a test that shells out
  to a built example binary; the gesture-level parity test covers the part that
  could actually diverge.
- **Pixel goldens for flows.** `shot` writes a PNG, and `fbui-testkit`'s
  tolerant compare exists, but nothing wires a flow's shot to a golden
  automatically. Text metrics differ by host fonts, so a flow's `shot` is only
  comparable with `bundled-font` or a pinned font set (which the widget tests
  now use).
- **`FBUI_HEADLESS_SIZE` and hotplug in one flow.** `SIGUSR1` simulates a mode
  change, but no flow step drives it, so `on_display_changed` is exercised by a
  unit test rather than by a scripted scenario.
- **Multi-touch in flows.** The grammar addresses one contact; the gesture
  recognizer is single-contact too (a Phase 4 gap), so this waits on that.
