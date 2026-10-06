//! The widget set on bitmap fonts: the same screens, input and text editing,
//! with no shaping engine underneath.

use fbui_render::geom::{Point, Size};
use fbui_render::{FontContext, Scale, Surface};
use fbui_widgets::event::{Event, Key, Modifiers, PointerButton};
use fbui_widgets::widgets::{Button, Container, Label, List, TextArea, TextInput};
use fbui_widgets::{Theme, Ui, WidgetId};

#[derive(Clone)]
enum Msg {}

/// The bundled bitmap Inter (12/16/20/24 px), via the dev-dependency's
/// `bundled-bitmap-font`.
fn bitmap_fonts() -> FontContext {
    let fc = FontContext::with_default_bitmap_fonts();
    assert!(fc.uses_bitmap_fonts());
    fc
}

struct Screen {
    ui: Ui<Msg>,
    input: WidgetId,
    area: WidgetId,
}

fn screen() -> Screen {
    let mut ui = Ui::with_fonts(
        Size::new(320.0, 300.0),
        Scale::ONE,
        Theme::dark(),
        bitmap_fonts(),
    );
    let root = ui.set_root(Container::column().fill().padding(10.0).gap(8.0));
    ui.add_child(root, Label::new("Settings").size(20.0));
    let input = ui.add_child(root, TextInput::new().placeholder("Name"));
    let area = ui.add_child(root, TextArea::new().rows(3));
    ui.add_child(root, Button::new("Apply"));
    ui.add_child(
        root,
        List::new((0..8).map(|i| format!("Row {i}")).collect::<Vec<_>>()),
    );
    ui.name(input, "name");
    ui.name(area, "notes");
    ui.layout_now();
    Screen { ui, input, area }
}

fn click(ui: &mut Ui<Msg>, at: Point) {
    let button = PointerButton::Left;
    ui.event(Event::PointerDown { pos: at, button });
    ui.event(Event::PointerUp { pos: at, button });
    ui.event(Event::Tap { pos: at });
}

fn typ(ui: &mut Ui<Msg>, text: &str) {
    for ch in text.chars() {
        let key = if ch == ' ' { Key::Space } else { Key::Char(ch) };
        ui.event(Event::Key {
            key,
            pressed: true,
            mods: Modifiers::default(),
        });
        ui.event(Event::Key {
            key,
            pressed: false,
            mods: Modifiers::default(),
        });
    }
}

fn key(ui: &mut Ui<Msg>, key: Key, mods: Modifiers) {
    ui.event(Event::Key {
        key,
        pressed: true,
        mods,
    });
    ui.event(Event::Key {
        key,
        pressed: false,
        mods,
    });
}

#[test]
fn a_screen_lays_out_and_paints_with_bitmap_fonts() {
    let mut s = screen();
    let mut surface = Surface::new(320, 300, Scale::ONE);
    s.ui.paint(&mut surface);
    // Text has real extents: the label is as tall as its line, the button
    // wide enough for its caption.
    let tree = s.ui.inspect_text();
    assert!(
        tree.contains("Settings") && tree.contains("Apply"),
        "{tree}"
    );
    // And actually reaches the pixels: the label's box holds bright ink.
    let label = s.ui.bounds(s.ui.find("name").unwrap()).unwrap();
    let px = surface.pixmap();
    let ink = (0..label.y as u32)
        .flat_map(|y| (0..320).map(move |x| (x, y)))
        .filter(|&(x, y)| px.pixel(x, y).unwrap().red() > 180)
        .count();
    assert!(ink > 40, "the title drew ({ink} bright pixels)");
    assert!(s.ui.lint().is_empty(), "{:?}", s.ui.lint());
}

#[test]
fn text_input_edits_with_bitmap_fonts() {
    let mut s = screen();
    let b = s.ui.bounds(s.input).unwrap();
    click(&mut s.ui, Point::new(b.x + 8.0, b.y + b.h / 2.0));
    typ(&mut s.ui, "hello world");
    let text = |ui: &mut Ui<Msg>| {
        ui.with::<TextInput<Msg>, _>(s.input, |t| t.text().to_string())
            .unwrap()
    };
    assert_eq!(text(&mut s.ui), "hello world");
    // Select the last word with shift+arrows, replace it.
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    for _ in 0..5 {
        key(&mut s.ui, Key::Left, shift);
    }
    assert_eq!(
        s.ui.with::<TextInput<Msg>, _>(s.input, |t| t.selection())
            .unwrap(),
        6..11
    );
    typ(&mut s.ui, "there");
    assert_eq!(text(&mut s.ui), "hello there");
    // Clicking near the start of the field puts the caret before the text.
    click(&mut s.ui, Point::new(b.x + 2.0, b.y + b.h / 2.0));
    typ(&mut s.ui, ">");
    assert_eq!(text(&mut s.ui), ">hello there");
    let mut surface = Surface::new(320, 300, Scale::ONE);
    s.ui.paint(&mut surface);
}

#[test]
fn text_area_wraps_with_bitmap_fonts() {
    let mut s = screen();
    let b = s.ui.bounds(s.area).unwrap();
    click(&mut s.ui, Point::new(b.x + 8.0, b.y + 8.0));
    typ(
        &mut s.ui,
        "a line long enough to wrap inside the box several times over",
    );
    key(&mut s.ui, Key::Enter, Modifiers::default());
    typ(&mut s.ui, "next");
    let t =
        s.ui.with::<TextArea<Msg>, _>(s.area, |t| t.text().to_string())
            .unwrap();
    assert!(t.ends_with("\nnext"), "{t:?}");
    let mut surface = Surface::new(320, 300, Scale::ONE);
    s.ui.paint(&mut surface);
    assert!(s.ui.lint().is_empty(), "{:?}", s.ui.lint());
}
