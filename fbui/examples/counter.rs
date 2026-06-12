//! A counter: two buttons and a label, the canonical Elm example.
//!
//! ```text
//! cargo run -p fbui --example counter --features platform
//! ```
//! Click +/− (or Tab to a button and press Space/Enter). Esc quits.

use fbui::widgets::{Align, Button, Container, Label};
use fbui::{App, Ui, WidgetId};

#[derive(Clone)]
enum Msg {
    Inc,
    Dec,
}

#[derive(Default)]
struct Counter {
    value: i32,
    label: Option<WidgetId>,
}

impl App for Counter {
    type Message = Msg;

    fn build(&mut self, ui: &mut Ui<Msg>) {
        let root = ui.set_root(
            Container::column()
                .fill()
                .padding(24.0)
                .gap(16.0)
                .align(Align::Center),
        );

        let title = ui.add_child(root, Label::new("Counter").size(28.0).bold());
        let _ = title;

        self.label = Some(ui.add_child(root, Label::new("0").size(48.0)));

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
        if let Some(id) = self.label {
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
