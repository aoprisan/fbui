# fbui

A Rust framework for drawing UIs **directly to a Linux display — no X11, no
Wayland**. For text consoles (TTYs), embedded devices, and kiosks: one process
owns the screen, fullscreen.

- **Display**: DRM/KMS dumb buffers, falling back to legacy fbdev, then to a
  terminal backend (kitty graphics / half-blocks) — chosen at runtime.
- **Input**: raw evdev (pure Rust) or libinput; mouse and touch unified into
  tap / long-press / drag / fling gestures, with kinetic scrolling.
- **Rendering**: a damage-tracked CPU renderer (tiny-skia + cosmic-text) into a
  normal-RAM shadow buffer, copied out row-by-row (XRGB8888 or dithered RGB565).
- **Widgets**: a retained tree with an Elm-ish `update(msg)` loop, taffy
  flexbox/grid layout, focus, theming, animation, and a v1 widget set.
- **Console safety**: the VT is restored to text mode on every exit path —
  `Drop`, `panic!`, and fatal signals.
- **Tooling with no screen**: a headless backend, widget-tree dumps, flow
  scripts with expectations, traces, lints, and a remote console.

## Crates

| Crate | What it is |
|---|---|
| [`fbui`](https://crates.io/crates/fbui) | The umbrella: re-exports the render layer and widgets, plus the app runner (`platform` feature). **Start here.** |
| [`fbui-widgets`](https://crates.io/crates/fbui-widgets) | Retained widget tree, layout, focus, theming, gestures, animation. |
| [`fbui-render`](https://crates.io/crates/fbui-render) | Headless CPU painter, damage tracking, text, copy-out. |
| [`fbui-platform`](https://crates.io/crates/fbui-platform) | Display / input / seat / VT / event loop — everything kernel-facing. |
| [`fbui-testkit`](https://crates.io/crates/fbui-testkit) | Golden-PNG snapshot assertions (a dev-dependency). |

The crates are versioned in lockstep; depend on the same version of each.

## Quick start

```toml
[dependencies]
fbui = { version = "0.4", features = ["platform", "bundled-font"] }
```

```rust,ignore
use fbui::widgets::{Align, Button, Container, Label};
use fbui::{App, Ui};

#[derive(Clone, Debug)]
enum Msg { Inc, Dec }

#[derive(Default)]
struct Counter { value: i32 }

impl App for Counter {
    type Message = Msg;

    fn build(&mut self, ui: &mut Ui<Msg>) {
        let root = ui.set_root(Container::column().fill().padding(24.0).gap(16.0).align(Align::Center));
        ui.add_named(root, "count", Label::new("0").size(48.0));
        let row = ui.add_child(root, Container::row().gap(12.0));
        ui.add_child(row, Button::new("−").on_press(|| Msg::Dec));
        ui.add_child(row, Button::new("+").on_press(|| Msg::Inc));
    }

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Inc => self.value += 1,
            Msg::Dec => self.value -= 1,
        }
        let text = self.value.to_string();
        if let Some(id) = ui.find("count") {
            ui.with::<Label, _>(id, |l| l.set_text(text));
        }
    }
}

fn main() {
    if let Err(e) = fbui::run(Counter::default()) {
        eprintln!("counter: {e}");
        std::process::exit(1);
    }
}
```

Run it from a real text VT (Ctrl-Alt-F2), as root or a member of the `video`
and `input` groups — or with no screen at all via `FBUI_BACKEND=headless` or
`FBUI_BACKEND=term`.

## Documentation

- API docs: [docs.rs/fbui](https://docs.rs/fbui)
- [Running on your device](https://github.com/aoprisan/fbui/blob/main/docs/running-on-your-device.md)
- [Developer tooling](https://github.com/aoprisan/fbui/blob/main/docs/tooling.md) — seeing and testing a UI with no screen
- [Design plan](https://github.com/aoprisan/fbui/blob/main/PLAN.md) and the [changelog](https://github.com/aoprisan/fbui/blob/main/CHANGELOG.md)

## MSRV

`fbui-platform` builds on Rust **1.76**; the render/widget stack (and so the
umbrella) needs **1.89**.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option. The bundled Inter font
(`bundled-font` feature) is under the SIL Open Font License; see
`fbui-render/fonts/Inter-LICENSE.txt`.
