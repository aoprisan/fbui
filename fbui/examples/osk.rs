//! An on-screen keyboard for a touch kiosk: two text fields and a docked
//! virtual keyboard. Tap a field to focus it, then type on the keyboard — the
//! keys never steal focus, so characters land in the field you last tapped.
//!
//! ```text
//! cargo run -p fbui --example osk --features platform
//! ```
//!
//! Drive it by finger on a touchscreen, or by mouse (touch and left-click share
//! the same pointer path). Shift toggles caps; `?123` switches to symbols.

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
            // Route the tapped key to whichever field currently holds focus.
            // The downcast is a no-op if the focused widget isn't a TextInput.
            Msg::Kbd(k) => {
                if let Some(id) = ui.focused() {
                    ui.with::<TextInput<Msg>, _>(id, |t| t.apply_key(k));
                }
            }
        }
    }
}

fn main() {
    if let Err(e) = fbui::run(Kiosk::default()) {
        eprintln!("osk: {e}");
        std::process::exit(1);
    }
}
