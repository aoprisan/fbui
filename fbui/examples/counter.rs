//! A counter: two buttons and a label, the canonical Elm example.
//!
//! ```text
//! cargo run -p fbui --example counter --features platform
//! ```
//! Click +/− (or Tab to a button and press Space/Enter). Esc quits.
//!
//! The widgets are **named** (`ui.add_named(.., "count", ..)`), which is what
//! lets `flows/counter.txt` say `tap #inc` and `expect #count text "1"` — and
//! what lets `update` find the label without the app keeping a `WidgetId`
//! field of its own. Run the flow with no screen at all:
//!
//! ```text
//! FBUI_BACKEND=headless FBUI_REPLAY=fbui/flows/counter.txt ./counter
//! ```

use fbui::widgets::{Align, Button, Container, Label};
use fbui::{App, Ui};

#[derive(Clone, Debug)]
enum Msg {
    Inc,
    Dec,
}

#[derive(Default)]
struct Counter {
    value: i32,
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

        ui.add_named(root, "title", Label::new("Counter").size(28.0).bold());
        ui.add_named(root, "count", Label::new("0").size(48.0));

        let row = ui.add_named(root, "buttons", Container::row().gap(12.0));
        ui.add_named(row, "dec", Button::new("−").on_press(|| Msg::Dec));
        ui.add_named(row, "inc", Button::new("+").on_press(|| Msg::Inc));
    }

    // Give `FBUI_TRACE` the app's own vocabulary, so a trace reads
    // `msg  Inc` rather than `msg  <msg>`. One line, and the causal chain
    // input → message → mutation becomes legible.
    fn describe_message(&self, msg: &Msg) -> Option<String> {
        Some(format!("{msg:?}"))
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
