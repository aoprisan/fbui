//! An on-screen keyboard for a touch kiosk: two text fields and a docked
//! virtual keyboard. Tap a field to focus it, then type on the keyboard — the
//! keys never steal focus, so characters land in the field you last tapped.
//!
//! ```text
//! cargo run -p fbui --example osk --features platform
//! ```
//!
//! Drive it by finger on a touchscreen, or by mouse (touch and left-click share
//! the same pointer path). Shift capitalizes the next letter; `?123` switches
//! to symbols; holding Backspace auto-repeats; Enter jumps to the next field.

use fbui::widgets::{Container, Keyboard, Label, TextInput};
use fbui::{App, Key, Ui, WidgetId};

#[derive(Clone)]
enum Msg {
    /// A key was tapped on the on-screen keyboard.
    Kbd(Key),
}

#[derive(Default)]
struct Kiosk {
    root: Option<WidgetId>,
}

impl App for Kiosk {
    type Message = Msg;

    fn build(&mut self, ui: &mut Ui<Msg>) {
        let muted = ui.theme().palette.muted;
        let root = ui.set_root(Container::column().fill());

        // The content area grows to fill everything above the docked keyboard.
        let content = ui.add_child(root, Container::column().grow(1.0).padding(24.0).gap(12.0));
        ui.add_child(content, Label::new("Check in").size(24.0).bold());

        ui.add_child(content, Label::new("Name").color(muted));
        ui.add_child(content, TextInput::new().placeholder("your name"));

        ui.add_child(content, Label::new("Email").color(muted));
        ui.add_child(content, TextInput::new().placeholder("you@example.com"));

        // The keyboard docks at the bottom (fixed height, added last).
        ui.add_child(root, Keyboard::new().on_key(Msg::Kbd));

        // Start with the first field focused so typing works immediately.
        ui.focus_first(root);
        self.root = Some(root);
    }

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            // Enter hops to the next field: replayed as Tab, which the Ui
            // intercepts for focus movement (a TextInput would swallow Enter).
            Msg::Kbd(Key::Enter) => ui.send_key(Key::Tab),
            // Route every other tapped key to whichever widget holds focus.
            // `send_key` replays it through the real event path, so the field
            // edits, repaints, and fires `on_change` exactly like hardware input.
            Msg::Kbd(k) => ui.send_key(k),
        }
    }
}

fn main() {
    if let Err(e) = fbui::run(Kiosk::default()) {
        eprintln!("osk: {e}");
        std::process::exit(1);
    }
}
