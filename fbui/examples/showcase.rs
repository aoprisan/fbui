//! An interactive tour of every widget, each wired to a message so you can see
//! the full event → update → repaint loop. The status line at the bottom echoes
//! your last interaction.
//!
//! ```text
//! cargo run -p fbui --example showcase --features platform
//! ```
//! Tab moves focus; Space/Enter/arrows operate the focused widget. Esc quits.

use fbui::render::Image;
use fbui::widgets::{
    Align, Button, Checkbox, Container, ImageView, Label, List, ProgressBar, ScrollView, Slider,
    Switch, TextInput,
};
use fbui::{App, Ui, WidgetId};

#[derive(Clone)]
enum Msg {
    Press,
    Toggle(bool),
    Flip(bool),
    Volume(f32),
    Name(String),
    Select(usize),
}

#[derive(Default)]
struct Showcase {
    status: Option<WidgetId>,
    bar: Option<WidgetId>,
}

/// A procedural RGBA gradient, so ImageView has something to blit without
/// shipping an asset file.
fn gradient(w: u32, h: u32) -> Image {
    let mut img = image::RgbaImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let r = (x * 255 / w.max(1)) as u8;
            let g = (y * 255 / h.max(1)) as u8;
            img.put_pixel(x, y, image::Rgba([r, g, 180, 255]));
        }
    }
    Image::from_rgba(img)
}

impl App for Showcase {
    type Message = Msg;

    fn build(&mut self, ui: &mut Ui<Msg>) {
        let muted = ui.theme().palette.muted;
        let accent = ui.theme().palette.accent;
        let surface = ui.theme().palette.surface;

        let root = ui.set_root(Container::column().fill().padding(20.0).gap(12.0));

        ui.add_child(root, Label::new("Widget showcase").size(24.0).bold());

        // Button.
        ui.add_child(root, Button::new("Click me").on_press(|| Msg::Press));

        // Boolean toggles.
        let toggles = ui.add_child(root, Container::row().gap(16.0).align(Align::Center));
        ui.add_child(
            toggles,
            Checkbox::new("Enabled", true).on_toggle(Msg::Toggle),
        );
        ui.add_child(toggles, Switch::new("Dark mode", false).on_toggle(Msg::Flip));

        // Text entry.
        ui.add_child(root, Label::new("Name").color(muted));
        ui.add_child(
            root,
            TextInput::new().placeholder("your name").on_change(Msg::Name),
        );

        // Slider drives the progress bar below it. Each lives in a row so its
        // flex-grow fills horizontally rather than ballooning the column.
        ui.add_child(root, Label::new("Volume").color(muted));
        let slider_row = ui.add_child(root, Container::row());
        ui.add_child(slider_row, Slider::new(0.0, 100.0, 50.0).on_change(Msg::Volume));
        let bar_row = ui.add_child(root, Container::row().width(320.0));
        self.bar = Some(ui.add_child(bar_row, ProgressBar::new(0.5)));

        // A procedurally generated image.
        let media = ui.add_child(root, Container::row().gap(12.0).align(Align::Center));
        ui.add_child(media, ImageView::new(gradient(80, 80)));
        ui.add_child(media, Label::new("ImageView").color(muted));

        // A windowed List beside a ScrollView, each filling half the row.
        let panels = ui.add_child(root, Container::row().gap(12.0).grow(1.0));

        let list_panel = ui.add_child(
            panels,
            Container::column().grow(1.0).background(surface, 10.0),
        );
        let rows: Vec<String> = (0..50).map(|i| format!("Row #{i:02}")).collect();
        ui.add_child(list_panel, List::new(rows).on_select(Msg::Select));

        let scroll_panel = ui.add_child(
            panels,
            Container::column()
                .grow(1.0)
                .padding(8.0)
                .background(surface, 10.0),
        );
        let scroll = ui.add_child(scroll_panel, ScrollView::new());
        let scroll_col = ui.add_child(scroll, Container::column().gap(6.0));
        for i in 0..30 {
            ui.add_child(scroll_col, Label::new(format!("Scrolled line {i}")));
        }

        // Status line, updated by every interaction.
        self.status = Some(ui.add_child(
            root,
            Label::new("interact with a widget above").color(accent),
        ));
    }

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        let status = match msg {
            Msg::Press => "button pressed".to_string(),
            Msg::Toggle(on) => format!("checkbox {}", if on { "checked" } else { "unchecked" }),
            Msg::Flip(on) => format!("switch {}", if on { "on" } else { "off" }),
            Msg::Name(s) => format!("name = {s:?}"),
            Msg::Select(i) => format!("selected row #{i:02}"),
            Msg::Volume(v) => {
                if let Some(id) = self.bar {
                    ui.with::<ProgressBar, _>(id, |b| b.set_fraction(v / 100.0));
                }
                format!("volume = {v:.0}")
            }
        };
        if let Some(id) = self.status {
            ui.with::<Label, _>(id, |l| l.set_text(status));
        }
    }
}

fn main() {
    if let Err(e) = fbui::run(Showcase::default()) {
        eprintln!("showcase: {e}");
        std::process::exit(1);
    }
}
