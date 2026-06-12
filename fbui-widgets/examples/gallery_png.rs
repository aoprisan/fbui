//! Headless: build a gallery of widgets and render it to a PNG — proof the
//! toolkit composes and paints with no device.
//!
//! ```text
//! cargo run -p fbui-widgets --example gallery_png -- /tmp/gallery.png
//! ```

use fbui_render::geom::Size;
use fbui_render::Scale;
use fbui_render::Surface;
use fbui_widgets::widgets::{Align, Button, Checkbox, Container, Label, List, Slider, TextInput};
use fbui_widgets::{Theme, Ui};

#[derive(Clone)]
enum Msg {}

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "gallery.png".into());
    let (w, h) = (520u32, 440u32);

    let mut ui = Ui::<Msg>::new(Size::new(w as f32, h as f32), Scale::ONE, Theme::dark());
    let root = ui.set_root(Container::column().fill().padding(20.0).gap(14.0));

    ui.add_child(root, Label::new("Widget gallery").size(26.0).bold());

    ui.add_child(
        root,
        Label::new("A text field:").color(ui.theme().palette.muted),
    );
    ui.add_child(root, TextInput::new().value("editable text"));

    let row = ui.add_child(root, Container::row().gap(12.0).align(Align::Center));
    ui.add_child(row, Checkbox::new("Wi-Fi", true));
    ui.add_child(row, Checkbox::new("Bluetooth", false));

    ui.add_child(root, Label::new("Volume:").color(ui.theme().palette.muted));
    ui.add_child(root, Slider::new(0.0, 100.0, 65.0));

    let buttons = ui.add_child(root, Container::row().gap(10.0));
    ui.add_child(buttons, Button::new("Cancel"));
    ui.add_child(buttons, Button::new("Save"));

    let panel = ui.add_child(
        root,
        Container::column()
            .grow(1.0)
            .background(ui.theme().palette.surface, 10.0),
    );
    let rows: Vec<String> = (0..200).map(|i| format!("List item {i}")).collect();
    ui.add_child(panel, List::new(rows));

    let mut surface = Surface::new(w, h, Scale::ONE);
    ui.paint(&mut surface);
    surface
        .pixmap()
        .save_png(&path)
        .unwrap_or_else(|e| panic!("write {path}: {e}"));
    eprintln!("wrote {path}");
}
