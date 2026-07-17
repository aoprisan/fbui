# Input session record & replay

Every fbui app (run through the `fbui::run` runner) can **record** the input
it receives and **replay** a recording later — through exactly the same code
path as live input, so gesture recognition, focus, kinetic scrolling, and
`App::update` all happen again. No app code changes; it's all environment
variables:

```sh
FBUI_RECORD=flow.rec ./kiosk-app          # record a live session
FBUI_REPLAY=flow.rec ./kiosk-app          # replay it, real time
FBUI_REPLAY=flow.rec FBUI_REPLAY_SPEED=4 ./kiosk-app   # 4x faster
```

Combined with the [terminal backend](terminal-backend.md), a recorded flow
becomes a headless end-to-end regression test that runs anywhere `cargo test`
does — no hardware, no root:

```sh
FBUI_BACKEND=term FBUI_REPLAY=flow.rec FBUI_REPLAY_SPEED=max \
    FBUI_REPLAY_SHOT=end.png ./kiosk-app
# compare end.png against a golden image
```

Replaying the same recording twice produces byte-identical end-state
screenshots (settled UI; see "Determinism" below).

## Variables

| Variable | Meaning |
|---|---|
| `FBUI_RECORD=path` | Append every input event to `path` (created fresh; flushed per event, so a crash session's recording survives — that's the artifact you wanted). |
| `FBUI_REPLAY=path` | Load and play a recording. Live input still works during playback (Esc still quits). |
| `FBUI_REPLAY_SPEED=n\|max` | Wall-clock multiplier (default `1`). `max` delivers everything as fast as frames render. |
| `FBUI_REPLAY_SHOT=path.png` | After the last event, wait for animations to settle, then write a PNG of the end state. |
| `FBUI_REPLAY_EXIT=0\|1` | What happens when playback ends. Unset: *as recorded* — a replayed Esc exits exactly as it did live (unless a shot is requested, which implies `1`). `1`: the replayer owns the ending — the recording's quit keystroke is swallowed, the shot (if any) is captured, then the app exits. `0`: same swallow, but the app stays running interactively after playback. |

A recording notes the surface size it was made on; replaying on a different
size logs a warning — absolute coordinates may land on different widgets.

## File format (`fbui-rec` v1)

Line-oriented text, written to be hand-editable and reviewable in a diff:

```
fbui-rec 1 1024x600
# comments and unknown lines are skipped
@0    m 512 300          # absolute pointer motion to (512, 300)
@210  b l p              # left button press   (l|m|r|<code> ; p|r)
@290  b l r              # ... release
@800  k 0x61 p 0 u61     # key: keysym, state (p|r|a), modifier bits, utf8 hex
@1200 s 0 -1 w           # scroll: horizontal, vertical, source (w|f|c)
@1500 d 4 -2.5           # relative pointer motion (mice)
@2000 td 0 100 200       # touch down: slot, x, y   (tm / tu <slot> / tc)
```

Timestamps are milliseconds from session start and are clamped to be
monotonic on load, so hand-edited files can't play out of order. Writing a
test flow by hand is entirely reasonable — that's why it's text.

## Determinism, honestly

Events are delivered at recorded positions through the deterministic widget
layer, so end states settle identically (the CI example above relies on
that, and replays are verified byte-identical). During playback the gesture
recognizer runs on the **recording's own timeline**, not the wall clock —
long-press holds and fling velocities replay identically at any
`FBUI_REPLAY_SPEED`, including `max`. What is *not* frame-exact between
runs: animations advance by real frame `dt`, so mid-flight frames differ —
only settled states are comparable, which is why the shot waits for
animations to finish.

Record at the platform-event level means recordings capture *intent* (what
the user did), not widget identities — a recording survives refactors that
keep the layout, and breaks (loudly, visibly) when the layout moves.
