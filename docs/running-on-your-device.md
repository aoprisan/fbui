# Running fbui on your device

This guide takes you from a blank Linux text console (TTY) to a running fbui app
on real hardware — a Raspberry Pi with a touchscreen, an x86 kiosk, a generic
laptop. fbui draws **straight to the display** (DRM/KMS dumb buffers, or legacy
fbdev), with **no X server and no Wayland compositor**.

> One process owns the whole screen, fullscreen. fbui is not a compositor; if you
> need multiple windowed apps, run one fbui app under a kiosk compositor instead.

## 1. What the kernel must provide

- **DRM/KMS** — the primary, vsynced path. You need a card node at
  `/dev/dri/card0` whose driver supports **dumb buffers** and **page flips** (all
  the mainstream drivers do: i915, amdgpu, vc4 on the Pi, simpledrm, …). Check:

  ```sh
  ls /dev/dri/        # expect card0 (and renderD128)
  cat /sys/class/drm/card0-*/status   # "connected" on the output you'll use
  ```

- **fbdev** — the fallback. If `/dev/fb0` exists fbui can use it, panning between
  two pages when the driver allows (`yres_virtual >= 2*yres`). fbdev is
  deprecated kernel-side; prefer DRM and keep fbdev for boards where DRM is
  absent or flaky (`PlatformConfig::prefer_fbdev`).

- **evdev input** — keyboards, mice, touchscreens appear as `/dev/input/event*`.
  fbui's default input backend reads these directly (pure Rust, no libinput).

- **VT/console** — fbui takes the active VT into graphics mode and mutes its
  keyboard while running, restoring it on every exit path.

Useful kernel config if you build your own: `CONFIG_DRM`, your GPU's DRM driver
(or `CONFIG_DRM_SIMPLEDRM`), `CONFIG_DRM_VKMS` (for CI/testing), `CONFIG_FB` +
`CONFIG_FB_*` for the fallback, `CONFIG_INPUT_EVDEV`, and `CONFIG_VT`.

## 2. Permissions: root, `video`+`input`, or a seat manager

fbui needs to open the card, the input nodes, and the TTY. There are three ways
to be allowed to, mirroring Slint's `seat`/`noseat` split:

| Setup | How fbui opens devices | When to use |
|---|---|---|
| **root** | Direct open (`noseat`) | Quick bring-up, embedded images where the app is PID 1-ish. |
| **`video` + `input` groups** | Direct open (`noseat`) | Unprivileged but trusted: add your user to `video` (DRM/fb) and `input` (evdev). The cleanest bare-metal option. |
| **logind / seatd** | Brokered through libseat (`libseat` feature) | Desktop-ish systems where a session manager owns the seat; lets an ordinary session user run without group hacks. |

For the group route:

```sh
sudo usermod -aG video,input "$USER"   # log out / back in to take effect
```

The default build uses the direct-open (`noseat`) path with the pure-Rust evdev
backend, so it needs **no system libraries**. The `libseat`, `libinput`, and
`xkbcommon` backends are opt-in Cargo features for hosts that provide those C
libraries.

## 3. Run an example from a TTY

Switch to a text console (`Ctrl-Alt-F3`), log in, then:

```sh
# Build with the on-device runner (the `platform` feature pulls in fbui-platform).
cargo run -p fbui --example counter  --features platform
cargo run -p fbui --example form     --features platform
cargo run -p fbui --example big_list --features platform   # 10,000-row list
```

If you took the group route, no `sudo` is needed. As root:

```sh
sudo -E cargo run -p fbui --example form --features platform
```

Controls: mouse and touch drive the widgets; **Tab**/**Shift-Tab** move focus,
arrows/Enter/Space activate, and **Esc** quits (restoring the console). On a
touchscreen, tap to activate, **flick to coast** (kinetic scroll), and
**long-press** is delivered to widgets that want it.

## 4. Writing your own app

Implement the `App` trait and hand it to `fbui::run`:

```rust,ignore
use fbui::{App, Ui, Theme, widgets::{Container, Label, Button}};

struct Counter { n: i32, label: fbui::WidgetId }

#[derive(Clone)]
enum Msg { Inc }

impl App for Counter {
    type Message = Msg;
    fn build(&mut self, ui: &mut Ui<Msg>) {
        let root = ui.set_root(Container::column().padding(16.0).gap(8.0).fill());
        self.label = ui.add_child(root, Label::new("0"));
        ui.add_child(root, Button::new("+1").on_press(|| Msg::Inc));
    }
    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Inc => {
                self.n += 1;
                let n = self.n;
                ui.with::<Label, _>(self.label, |l| l.set_text(n.to_string()));
            }
        }
    }
}

fn main() -> fbui_platform::Result<()> {
    fbui::run(Counter { n: 0, label: Default::default() })
}
```

## 5. Configuration knobs

`PlatformConfig` (passed by the runner; construct your own for a custom loop):

- `card` / `fb` / `tty` — device nodes (defaults: `/dev/dri/card0`, `/dev/fb0`,
  `/dev/tty`).
- `prefer_fbdev` — skip DRM and go straight to fbdev.
- `prefer_libinput` — use libinput instead of raw evdev (needs the `libinput`
  feature; falls back to evdev if it can't initialize).
- `vt_guard` — take over the console. **Disable** it for serial/pty/SSH bring-up,
  where the console-mode ioctls would fail.

16-bit (RGB565) panels get ordered dithering on the copy-out automatically, to
suppress gradient banding.

## 6. Troubleshooting

- **"no connected connector"** — no output is `connected`. Check
  `/sys/class/drm/card*/status`; plug in HDMI before launching, or use
  `prefer_fbdev` if your panel only shows up as `/dev/fb0`.
- **`NotMaster` / `EACCES` on modeset** — another process holds DRM master
  (a running compositor, getty in KMS console). Switch to a free VT, or stop the
  display manager.
- **Permission denied opening `/dev/dri/card0` or `/dev/input/event*`** — you're
  not root and not in `video`/`input`. See §2.
- **Console comes back as a black/garbled screen after a crash** — fbui restores
  `KD_TEXT` on panic and on `SIGINT`/`SIGTERM`/`SIGQUIT`/`SIGHUP` and the fatal
  signals; if you `kill -9` it (uncatchable), run `reset` or switch VTs to
  recover.
- **Keyboard does nothing in the app** — fbui mutes the owning VT's keyboard
  (`K_OFF`) and reads evdev directly; make sure you launched from the **active**
  VT and have read access to the input nodes.
- **VT switching (`Ctrl-Alt-Fn`) leaves artifacts** — expected to be seamless on
  DRM (master is dropped/reacquired with a full redraw); on fbdev the fallback is
  best-effort. File a report with your driver name.

## 7. Testing without hardware

- The renderer and widgets are **headless** — `cargo test` covers them with no
  device.
- The platform layer's device path is exercised in CI against **VKMS** (the
  kernel's virtual KMS driver) plus **uinput** synthetic input; see
  [`fbui-platform/docs/qemu.md`](../fbui-platform/docs/qemu.md) for a QEMU recipe.
