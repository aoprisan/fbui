# Idle power management

Kiosks and embedded panels care about burn-in and power draw. fbui can dim the
backlight after a period with no input, turn the panel off entirely a while
later, and wake it the moment someone touches the screen — with the waking tap
swallowed, so it can't also press whatever it landed on.

## Enabling it

From the app, return a policy from `App::idle_policy`:

```rust
use fbui::IdlePolicy;

fn idle_policy(&self) -> IdlePolicy<Msg> {
    IdlePolicy::disabled()
        .dim_after_secs(60.0)     // backlight down after 1 min idle…
        .dim_percent(15)          // …to 15 %
        .blank_after_secs(300.0)  // panel off (DPMS / fb blank) after 5 min
        .on_idle(Msg::ScreenIdle) // e.g. switch to an attract screen
        .on_wake(Msg::ScreenWake)
}
```

Operators can override the timings per deployment without a rebuild:

| Variable | Meaning |
|---|---|
| `FBUI_IDLE_DIM` | seconds until dim; `0`/`off` disables dimming |
| `FBUI_IDLE_BLANK` | seconds until the panel powers off; `0`/`off` disables |
| `FBUI_IDLE_DIM_LEVEL` | backlight percent while dimmed (0–100) |

Junk values are a hard startup error, like the other `FBUI_*` toggles.

## What each stage does

- **Dim** writes `/sys/class/backlight/*/brightness` (the first device found;
  the previous level is restored on wake). Systems without a controllable
  backlight — most desktop GPUs with external monitors — skip dimming;
  blanking still works. Writing sysfs needs the same provisioning as the
  device nodes (root, or a udev rule for the `video` group — see
  `running-on-your-device.md`).
- **Blank** asks the display backend to power the panel off:
  the DRM backend sets the connector's standard **DPMS** property, fbdev
  issues `FBIOBLANK`, and the terminal backend ignores it. Both are
  best-effort: a panel that can't blank stays lit rather than failing the
  app.
- **Wake** happens on any input: brightness is restored, the panel powers
  back on, and `on_wake` is delivered. Input arriving while the screen is
  *blanked* is swallowed until the waking gesture ends (finger up / key
  released); input while merely *dimmed* is delivered normally after
  restoring brightness.

`on_idle` fires once, when the screen first leaves the active state (at dim,
or at blank if dimming is off) — the hook for switching to a screensaver or
attract loop.

## Idle cost

The tracker rides the runner's existing `next_timeout` plumbing: the event
loop sleeps in `poll` until the next stage boundary (or an fd), so idle
management adds no ticking. Once blanked, no deadline is armed at all — the
loop sleeps purely on input fds, preserving the ~0 % idle-CPU rule.
