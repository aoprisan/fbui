//! Headless: build one of every widget and render it to a PNG — proof the whole
//! toolkit composes and paints with no device.
//!
//! Unlike `gallery_png` (a curated subset), this exercises *all* widgets:
//! Label, TextInput, Checkbox, Switch, Slider, ProgressBar, Button (every
//! variant), ImageView, List, ScrollView, and the Container/Align layout.
//!
//! ```text
//! cargo run -p fbui-widgets --example all_widgets -- /tmp/all_widgets.png
//! ```

use fbui_render::geom::Size;
use fbui_render::{Image, Scale, Surface};
use fbui_widgets::widgets::{
    Align, Button, Checkbox, Container, ImageView, Label, List, ProgressBar, ScrollView, Slider,
    Switch, TextInput,
};
use fbui_widgets::{Theme, Ui};

#[derive(Clone)]
enum Msg {}

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

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "all_widgets.png".into());
    let (w, h) = (560u32, 760u32);

    let mut ui = Ui::<Msg>::new(Size::new(w as f32, h as f32), Scale::ONE, Theme::dark());
    let muted = ui.theme().palette.muted;
    let surface_color = ui.theme().palette.surface;

    let root = ui.set_root(Container::column().fill().padding(20.0).gap(12.0));

    // Text.
    ui.add_child(root, Label::new("All widgets").size(26.0).bold());
    ui.add_child(
        root,
        Label::new("one of every fbui widget, painted headlessly").color(muted),
    );
    ui.add_child(root, TextInput::new().value("editable text"));

    // Toggles: checkboxes and animated switches (shown at their settled state).
    let toggles = ui.add_child(root, Container::row().gap(16.0).align(Align::Center));
    ui.add_child(toggles, Checkbox::new("Wi-Fi", true));
    ui.add_child(toggles, Checkbox::new("Bluetooth", false));
    ui.add_child(toggles, Switch::new("Dark mode", true));
    ui.add_child(toggles, Switch::new("Mute", false));

    // Range inputs. Each sits in its own row so its flex-grow fills the row
    // *horizontally* instead of ballooning the column vertically.
    ui.add_child(root, Label::new("Volume").color(muted));
    let slider_row = ui.add_child(root, Container::row());
    ui.add_child(slider_row, Slider::new(0.0, 100.0, 65.0));
    ui.add_child(root, Label::new("Download").color(muted));
    let bar_row = ui.add_child(root, Container::row());
    ui.add_child(bar_row, ProgressBar::new(0.4));

    // Buttons — one of each variant.
    let buttons = ui.add_child(root, Container::row().gap(10.0));
    ui.add_child(buttons, Button::new("Save"));
    ui.add_child(buttons, Button::new("Cancel").secondary());
    ui.add_child(buttons, Button::new("Delete").danger());

    // A procedurally generated image.
    let media = ui.add_child(root, Container::row().gap(12.0).align(Align::Center));
    ui.add_child(media, ImageView::new(gradient(96, 96)));
    ui.add_child(media, Label::new("ImageView\n(96×96 gradient)").color(muted));

    // Bottom: a windowed List beside a ScrollView, each filling half the row.
    let panels = ui.add_child(root, Container::row().gap(12.0).grow(1.0));

    let list_panel = ui.add_child(
        panels,
        Container::column()
            .grow(1.0)
            .background(surface_color, 10.0),
    );
    let list_rows: Vec<String> = (0..200).map(|i| format!("List item {i}")).collect();
    ui.add_child(list_panel, List::new(list_rows));

    let scroll_panel = ui.add_child(
        panels,
        Container::column()
            .grow(1.0)
            .padding(8.0)
            .background(surface_color, 10.0),
    );
    let scroll = ui.add_child(scroll_panel, ScrollView::new());
    let scroll_col = ui.add_child(scroll, Container::column().gap(6.0));
    for i in 0..40 {
        ui.add_child(scroll_col, Label::new(format!("Scrolled line {i}")));
    }

    let mut surface = Surface::new(w, h, Scale::ONE);
    ui.paint(&mut surface);
    surface
        .pixmap()
        .save_png(&path)
        .unwrap_or_else(|e| panic!("write {path}: {e}"));
    eprintln!("wrote {path}");
}
