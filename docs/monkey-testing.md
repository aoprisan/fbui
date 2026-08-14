# Monkey testing: seeded input chaos with a built-in reproducer

Every fbui app (run through the `fbui::run` runner) has a monkey tester built
in. Set `FBUI_MONKEY=<seed>` and the runner synthesizes a pseudo-random input
session — taps, drags, flings, long-presses, cancelled touches, mouse clicks,
wheel scrolls, focus/navigation keys, text (including multibyte UTF-8) — and
feeds it through **exactly the same replay path a real user's input takes**:
gesture recognition, kinetic scrolling, focus traversal, `App::update`, paint.
No app code changes, no test harness to write:

```sh
FBUI_MONKEY=42 ./kiosk-app                       # 1000 events of seeded chaos
FBUI_MONKEY=42 FBUI_MONKEY_EVENTS=20000 ./kiosk-app   # a longer beating
FBUI_MONKEY=random ./kiosk-app                   # explore; the seed is printed
```

The session is **fully determined by the seed** — same seed, same screen size,
same event stream — and, crucially, the whole script is written to disk as an
ordinary [recording](record-replay.md) *before the first event fires*. When
the monkey finds a panic, the reproducer already exists:

```sh
FBUI_BACKEND=term FBUI_MONKEY=7 FBUI_MONKEY_EVENTS=50000 ./kiosk-app
# fbui: monkey: seed 7, 50000 events on 1280x800; script saved to
#   fbui-monkey-7.rec — reproduce with FBUI_REPLAY=fbui-monkey-7.rec
# … thread 'main' panicked at src/cart.rs:88 …

FBUI_BACKEND=term FBUI_REPLAY=fbui-monkey-7.rec ./kiosk-app   # same panic
```

Because the `.rec` file — not the generator — is the ground truth, the
reproduction doesn't depend on the generator staying stable across fbui
versions, and the file is plain text: bisect it down to a minimal reproducer
by deleting lines in an editor, then check the trimmed flow in as a
[replay regression test](record-replay.md).

## Variables

| Variable | Meaning |
|---|---|
| `FBUI_MONKEY=seed\|random` | Enable the monkey with a `u64` seed. `random` picks (and prints) one — for exploration; CI should pin seeds. Mutually exclusive with `FBUI_REPLAY`. |
| `FBUI_MONKEY_EVENTS=n` | Minimum number of input events to generate (default `1000`; the closing gesture finishes past the budget). |
| `FBUI_MONKEY_OUT=path` | Where to save the script (default `fbui-monkey-<seed>.rec` in the working directory). |
| `FBUI_REPLAY_SPEED=n\|max` | Same as replay, but the monkey defaults to `max` — it is an unattended stress run. Set `1` to watch it live on a device. |
| `FBUI_REPLAY_SHOT=path.png` | PNG of the settled end state, as in replay — pin it against a golden to catch *rendering* corruption, not just panics. |
| `FBUI_REPLAY_EXIT=0` | Hand the app back interactively after the run instead of exiting (default: exit — unattended). |

## What the monkey does (and deliberately doesn't)

The generator is touch-heavy (kiosks are), and every action is *physically
plausible*: one slot-0 finger, every down matched by an up (or an explicit
`TouchCancel` — palm rejection and hotplug happen in the field, so ~3% of
drags end cancelled), keys pressed then released, positions always on screen.
Long-presses hold through the recognizer's threshold; fast drags register as
flings and set kinetic scrolling coasting. Text entry includes Latin-1, CJK,
and emoji to stress shaping. Timestamps ride the replay clock, so long-press
timing and fling velocities are honored even at `FBUI_REPLAY_SPEED=max`.

Two things it will never do:

- **Press Escape.** That's the runner's quit key; a monkey that quits the app
  isn't stress-testing it, and a saved script containing Escape would end a
  plain `FBUI_REPLAY` reproduction early.
- **Leave a device mid-gesture.** A session ends at rest — no held contact,
  button, or key — so the end-state screenshot is comparable across runs.

## CI recipe

Headless chaos smoke test on the [terminal backend](terminal-backend.md) — no
hardware, no root, fails the job on any panic:

```sh
FBUI_BACKEND=term FBUI_MONKEY=1 FBUI_MONKEY_EVENTS=10000 \
FBUI_MONKEY_OUT=artifacts/monkey.rec ./kiosk-app
```

Run a handful of pinned seeds (`1 2 3 …`) rather than `random`: a red CI run
must reproduce on a laptop. Upload `artifacts/monkey.rec` as a job artifact —
if the app dies, the artifact *is* the bug report.
