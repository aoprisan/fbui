//! A form with validation: a text field, a multi-line notes box, a checkbox,
//! a slider, and a submit button that reports the entered values (or asks for
//! a name). The fields support selection (drag, Shift+arrows), word jumps
//! (Ctrl+arrows), and cut/copy/paste between each other (Ctrl+X/C/V) — see
//! `docs/text-editing.md`.
//!
//! ```text
//! cargo run -p fbui --example form --features platform
//! ```

use fbui::widgets::{Align, Button, Checkbox, Container, Label, Slider, TextArea, TextInput};
use fbui::{App, Ui, WidgetId};

#[derive(Clone)]
enum Msg {
    Name(String),
    Notes(String),
    Subscribe(bool),
    Volume(f32),
    Submit,
}

#[derive(Default)]
struct Form {
    name: String,
    notes: String,
    subscribe: bool,
    volume: f32,
    status: Option<WidgetId>,
}

impl App for Form {
    type Message = Msg;

    fn build(&mut self, ui: &mut Ui<Msg>) {
        let muted = ui.theme().palette.muted;
        let accent = ui.theme().palette.accent;

        let root = ui.set_root(Container::column().fill().padding(24.0).gap(14.0));

        ui.add_child(root, Label::new("Sign up").size(24.0).bold());

        ui.add_child(root, Label::new("Name").color(muted));
        ui.add_child(
            root,
            TextInput::new()
                .placeholder("your name")
                .on_change(Msg::Name),
        );

        ui.add_child(root, Label::new("Notes").color(muted));
        ui.add_child(
            root,
            TextArea::new()
                .rows(3)
                .placeholder("anything else? (multi-line)")
                .on_change(Msg::Notes),
        );

        let row = ui.add_child(root, Container::row().gap(10.0).align(Align::Center));
        ui.add_child(
            row,
            Checkbox::new("Email me updates", false).on_toggle(Msg::Subscribe),
        );

        ui.add_child(root, Label::new("Volume").color(muted));
        ui.add_child(root, Slider::new(0.0, 100.0, 50.0).on_change(Msg::Volume));

        ui.add_child(root, Button::new("Submit").on_press(|| Msg::Submit));

        self.status = Some(ui.add_child(root, Label::new(" ").color(accent)));
        self.volume = 50.0;
    }

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Name(s) => self.name = s,
            Msg::Notes(s) => self.notes = s,
            Msg::Subscribe(b) => self.subscribe = b,
            Msg::Volume(v) => self.volume = v,
            Msg::Submit => {
                let text = if self.name.trim().is_empty() {
                    "Please enter a name.".to_string()
                } else {
                    let lines = self.notes.lines().filter(|l| !l.trim().is_empty()).count();
                    format!(
                        "Thanks, {}! volume {:.0}{}{}",
                        self.name.trim(),
                        self.volume,
                        if self.subscribe { ", subscribed" } else { "" },
                        if lines > 0 {
                            format!(", {lines} line(s) of notes")
                        } else {
                            String::new()
                        }
                    )
                };
                if let Some(id) = self.status {
                    ui.with::<Label, _>(id, |l| l.set_text(text));
                }
            }
        }
    }
}

fn main() {
    if let Err(e) = fbui::run(Form::default()) {
        eprintln!("form: {e}");
        std::process::exit(1);
    }
}
