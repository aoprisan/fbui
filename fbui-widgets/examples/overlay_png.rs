//! Headless: the overlay layer in one frame — an open [`Select`] menu floating
//! over page content plus a [`Toasts`] pile (left panel), and a modal
//! [`Dialog`] over its page (right panel) — rendered to a PNG with no device.
//!
//! ```text
//! cargo run -p fbui-widgets --example overlay_png -- /tmp/overlay.png
//! ```

use fbui_render::geom::{Point, Size};
use fbui_render::{Scale, Surface};
use fbui_widgets::event::{Event, PointerButton};
use fbui_widgets::widgets::{Button, Container, Dialog, Label, Select, Stack, ToastKind, Toasts};
use fbui_widgets::{Theme, Ui};

#[derive(Clone)]
enum Msg {}

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "overlay.png".into());
    let (w, h) = (640u32, 400u32);

    let theme = Theme::dark();
    let surface_color = theme.palette.surface;
    let muted = theme.palette.muted;
    let mut ui = Ui::<Msg>::new(Size::new(w as f32, h as f32), Scale::ONE, theme);

    let root = ui.set_root(Container::row().fill());

    // Left panel: an open Select floating over content, and toasts.
    let left = ui.add_child(root, Stack::new());
    let page = ui.add_child(left, Container::column().padding(20.0).gap(12.0));
    ui.add_child(page, Label::new("Dropdown + toasts").size(20.0).bold());
    ui.add_child(page, Label::new("Render quality").color(muted));
    let select = ui.add_child(
        page,
        Select::new(["Low", "Medium", "High", "Ultra"]).selected(1),
    );
    for i in 0..5 {
        ui.add_child(
            page,
            Label::new(format!("Page content line {i}")).color(muted),
        );
    }
    let toasts = ui.add_child(left, Toasts::new());
    ui.with::<Toasts, _>(toasts, |t| {
        t.push(ToastKind::Success, "Settings saved");
        t.push(ToastKind::Error, "Disk almost full");
    });

    // Right panel: a modal dialog over its own page.
    let right = ui.add_child(root, Stack::new());
    let rpage = ui.add_child(right, Container::column().padding(20.0).gap(12.0));
    ui.add_child(rpage, Label::new("Modal dialog").size(20.0).bold());
    for i in 0..7 {
        ui.add_child(
            rpage,
            Label::new(format!("Dimmed page line {i}")).color(muted),
        );
    }
    let dialog = ui.add_child(right, Dialog::new());
    let card = ui.add_child(
        dialog,
        Container::column()
            .padding(20.0)
            .gap(12.0)
            .background(surface_color, 12.0),
    );
    ui.add_child(card, Label::new("Erase everything?").size(18.0).bold());
    let buttons = ui.add_child(card, Container::row().gap(10.0));
    ui.add_child(buttons, Button::new("Cancel").secondary());
    ui.add_child(buttons, Button::new("Erase").danger());
    ui.focus_first(dialog);

    // Click the select's field so its menu is open in the shot.
    ui.layout_now();
    let b = ui.bounds(select).expect("select laid out");
    ui.event(Event::PointerDown {
        pos: Point::new(b.x + 10.0, b.y + 10.0),
        button: PointerButton::Left,
    });

    let mut surface = Surface::new(w, h, Scale::ONE);
    ui.paint(&mut surface);
    surface
        .pixmap()
        .save_png(&path)
        .unwrap_or_else(|e| panic!("write {path}: {e}"));
    eprintln!("wrote {path}");
}
