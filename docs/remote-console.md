# The remote console

A kiosk in the field has no keyboard, no second screen, and nobody standing in
front of it — but it usually has a network. The **remote console** (feature
`remote` on the `fbui` crate) makes every fbui app remotely observable and
operable over plain HTTP:

- a **live view** of the screen in any browser,
- **remote control**: clicks, scrolls, and typing injected through the exact
  same code path as live input,
- a **widget-tree inspector** (names, bounds, focus — the `Ui::inspect` API),
- **Prometheus metrics** for fleet monitoring,
- and a scriptable input API (`curl` a tap onto a device in a rack).

There is no X11, no VNC server, and no new dependency behind this: the server
is hand-rolled over `std::net` and the console is a single embedded HTML file.
A release binary grows by a few tens of kilobytes.

## Quick start

```sh
# build with the features
cargo run -p fbui --example showcase --features platform,remote &

# on the device (or in front of it):
FBUI_REMOTE=8433 ./your-app
# fbui: remote console listening on http://127.0.0.1:8433/
```

Then from your workstation:

```sh
ssh -L 8433:localhost:8433 device
xdg-open http://localhost:8433
```

You see what the device shows, live. Click the screen to interact; click once
into the screen area and type to send keys. The right-hand panel shows the
widget tree (hover a node to highlight it on the screen) and the metrics.

`FBUI_REMOTE` accepts a bare port (binds `127.0.0.1`) or a full address:

```sh
FBUI_REMOTE=0.0.0.0:8433 FBUI_REMOTE_TOKEN=$(openssl rand -hex 16) ./your-app
```

## Security model

Input injection **is remote control of the device** — treat the port like an
SSH port:

- Nothing runs unless `FBUI_REMOTE` is set. There is no default-on behavior.
- A bare port binds **loopback only**. Reaching it remotely means an SSH
  tunnel — which is the recommended deployment.
- Binding wider (`0.0.0.0:…`) should always be paired with
  `FBUI_REMOTE_TOKEN`. When set, every request must carry the token
  (`?token=…` or `Authorization: Bearer …`); everything else is 401.
  The web console picks the token up from its own URL:
  `http://device:8433/?token=…`.
- The transport is plain HTTP. The token protects against a curious port
  scanner, not an on-path attacker: on an untrusted network, tunnel.
- A failed bind is a **hard startup error**: an operator who believes a kiosk
  is remotely reachable must not find out otherwise in the field.
- `Escape` exits the app, remotely as locally (the runner's global quit key).
  The web console requires a double-press so a stray Esc doesn't kill a
  kiosk; the raw API does what you tell it.

## Endpoints

| Endpoint | What |
|---|---|
| `GET /` | the embedded web console |
| `GET /screen.png` | the current frame as a PNG — works while the app is idle |
| `GET /stream` | `multipart/x-mixed-replace` PNG stream; parts arrive when frames are presented (damage-driven, throttled to ~15 fps per client) |
| `GET /tree` | widget-tree snapshot as JSON |
| `GET /metrics` | Prometheus text format |
| `POST /input` | inject input (parameters below) |

At most 16 concurrent connections are served; further ones get 503.

### `POST /input`

Parameters are query-string encoded; coordinates are **device pixels** in the
space of `/screen.png` (the console maps browser coordinates for you).

| `type` | parameters | meaning |
|---|---|---|
| `move` | `x`, `y` | pointer motion |
| `down` / `up` | `x`, `y`, `button`(`left`\|`middle`\|`right`, default left) | button press / release |
| `tap` | `x`, `y`, `button` | press + release |
| `wheel` | `x`, `y`, `dy` | scroll; positive `dy` scrolls content down (browser `deltaY` convention) |
| `key` | `key` | one press+release: a single character, or `Enter`, `Backspace`, `Tab`, `Delete`, `Home`, `End`, `Left`, `Right`, `Up`, `Down`, `Space`, `Escape` |
| `text` | `text` | type a string, one key per character |

```sh
curl -X POST 'http://localhost:8433/input?type=tap&x=240&y=112'
curl -X POST 'http://localhost:8433/input?type=text&text=hello%20kiosk'
curl -X POST 'http://localhost:8433/input?type=key&key=Enter'
```

Injected input flows through the **same single input path** as evdev events:
the gesture recognizer runs (a remote `down`+`up` is a tap, complete with
gesture semantics), focus moves, `App::update` sees the same messages — and
`FBUI_RECORD` captures it, so a remote-driven session produces a replayable
recording (see `docs/record-replay.md`). Together the two make a powerful
support loop: reproduce a field issue remotely, keep the recording as a CI
regression test.

### `GET /tree`

```json
{ "scale": 1, "tree": { "id": "WidgetId(1v1)", "name": "Container",
  "bounds": [0, 0, 640, 480], "focusable": false, "focused": false,
  "hovered": false, "children": [ … ] } }
```

`bounds` are logical pixels (`[x, y, w, h]`); multiply by `scale` for the
device pixels of `/screen.png`. Nodes report `overlay` when the widget
currently floats one (an open dropdown, a toast). This is
`fbui::Ui::inspect()` serialized with `fbui::remote::tree_json` — a custom
embedder can serve the same document.

### `GET /metrics`

Prometheus text format: `fbui_frames_total`, `fbui_input_events_total`,
`fbui_paint_milliseconds` (last frame's paint + copy-out cost),
`fbui_paint_milliseconds_max`, `fbui_uptime_seconds`, `fbui_surface_pixels`,
`fbui_remote_clients`, `fbui_remote_watchers`. Point a fleet scraper at it and
alert on a kiosk whose frame cost regresses or whose uptime resets.

## Cost when enabled

The accept loop and each connection are plain blocked threads — no polling, no
timers; an idle console costs nothing measurable. Frames are copied out of the
shadow surface (a memcpy) and PNG-encoded **only while a stream client is
connected**, on the connection's thread, throttled per client. With no clients
connected the per-frame overhead is one atomic check.

## The debug HUD

Related, purely local: `FBUI_HUD=1` composites a small fps / paint-cost
readout into the top-right corner of every presented frame (drawn like the
software cursor, after copy-out, using a built-in 3×5 pixel font — it works
even when text rendering is what's broken). When the app idles, no frames are
presented and the readout freezes with the app; the idle-burns-0% rule is
untouched.
