//! Text-editing behavior: the single-line `TextInput` and the multi-line
//! `TextArea` driven through the real event path — caret placement by
//! pointer, drag selection, the clipboard chords against the `Ui` clipboard,
//! line-aware navigation, and caret-following scroll.
//!
//! Glyph geometry matters here, so unlike `behavior.rs` these load the
//! bundled Inter face straight from the repo (no host fonts, no feature flag).

use fbui_render::geom::{Point, Size};
use fbui_render::{FontContext, Scale, Surface};
use fbui_widgets::event::{Event, Key, Modifiers, PointerButton};
use fbui_widgets::widgets::{Container, ScrollView, TextArea, TextInput};
use fbui_widgets::{Theme, Ui, WidgetId};

#[derive(Clone, Debug, PartialEq)]
enum Msg {
    Changed(String),
    Area(String),
}

fn fonts() -> FontContext {
    FontContext::with_fonts([include_bytes!("../../fbui-render/fonts/Inter-Regular.ttf").to_vec()])
}

fn ui() -> Ui<Msg> {
    Ui::with_fonts(Size::new(400.0, 300.0), Scale::ONE, Theme::dark(), fonts())
}

fn press(ui: &mut Ui<Msg>, key: Key, mods: Modifiers) {
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

fn ctrl(c: char) -> (Key, Modifiers) {
    (
        Key::Char(c),
        Modifiers {
            ctrl: true,
            ..Modifiers::default()
        },
    )
}

fn shift() -> Modifiers {
    Modifiers {
        shift: true,
        ..Modifiers::default()
    }
}

fn none() -> Modifiers {
    Modifiers::default()
}

fn type_str(ui: &mut Ui<Msg>, s: &str) {
    for c in s.chars() {
        let key = match c {
            ' ' => Key::Space,
            '\n' => Key::Enter,
            c => Key::Char(c),
        };
        press(ui, key, none());
    }
}

fn click(ui: &mut Ui<Msg>, at: Point) {
    ui.event(Event::PointerDown {
        pos: at,
        button: PointerButton::Left,
    });
    ui.event(Event::PointerUp {
        pos: at,
        button: PointerButton::Left,
    });
}

fn drag(ui: &mut Ui<Msg>, from: Point, to: Point) {
    ui.event(Event::PointerDown {
        pos: from,
        button: PointerButton::Left,
    });
    ui.event(Event::PointerMove { pos: to });
    ui.event(Event::PointerUp {
        pos: to,
        button: PointerButton::Left,
    });
}

fn input_text(ui: &mut Ui<Msg>, id: WidgetId) -> String {
    ui.with::<TextInput<Msg>, _>(id, |t| t.text().to_string())
        .unwrap()
}

fn area_text(ui: &mut Ui<Msg>, id: WidgetId) -> String {
    ui.with::<TextArea<Msg>, _>(id, |t| t.text().to_string())
        .unwrap()
}

fn area_cursor(ui: &mut Ui<Msg>, id: WidgetId) -> usize {
    ui.with::<TextArea<Msg>, _>(id, |t| t.cursor()).unwrap()
}

/// Logical x of byte `idx` inside the field, using the same shaping the widget uses.
fn caret_x(ui: &mut Ui<Msg>, text: &str, idx: usize) -> f32 {
    let style = fbui_render::TextStyle::new(ui.theme().metrics.font_size, ui.theme().palette.text)
        .family(ui.theme().font.clone());
    let mut f = fonts();
    f.layout(text, &style, None).caret(idx).x
}

#[test]
fn text_input_clipboard_chords_use_the_ui_clipboard() {
    let mut ui = ui();
    let root = ui.set_root(Container::column().fill().padding(10.0));
    let field = ui.add_child(root, TextInput::new().on_change(Msg::Changed));
    ui.layout_now();
    let b = ui.bounds(field).unwrap();
    click(&mut ui, Point::new(b.x + 20.0, b.y + b.h / 2.0));
    assert_eq!(ui.focused(), Some(field));

    type_str(&mut ui, "hello world");
    let _ = ui.take_messages();
    let (k, m) = ctrl('a');
    press(&mut ui, k, m);
    let (k, m) = ctrl('c');
    press(&mut ui, k, m);
    assert_eq!(ui.clipboard(), "hello world");
    assert!(ui.take_messages().is_empty(), "copy is not a change");

    let (k, m) = ctrl('x');
    press(&mut ui, k, m);
    assert_eq!(input_text(&mut ui, field), "");
    assert_eq!(ui.take_messages(), vec![Msg::Changed(String::new())]);

    let (k, m) = ctrl('v');
    press(&mut ui, k, m);
    press(&mut ui, k, m);
    assert_eq!(input_text(&mut ui, field), "hello worldhello world");

    // The app can install clipboard text (bridged from elsewhere) and it pastes.
    ui.set_clipboard("multi\nline");
    let (k, m) = ctrl('a');
    press(&mut ui, k, m);
    let (k, m) = ctrl('v');
    press(&mut ui, k, m);
    assert_eq!(
        input_text(&mut ui, field),
        "multi line",
        "single-line flattens breaks"
    );

    // A Ctrl chord the field doesn't know is not typed as a character.
    let (k, m) = ctrl('q');
    press(&mut ui, k, m);
    assert_eq!(input_text(&mut ui, field), "multi line");
}

#[test]
fn text_input_word_navigation_and_shift_selection() {
    let mut ui = ui();
    let root = ui.set_root(Container::column().fill().padding(10.0));
    let field = ui.add_child(root, TextInput::new().value("alpha beta gamma"));
    ui.layout_now();
    let c = {
        let b = ui.bounds(field).unwrap();
        Point::new(b.x + 5.0, b.y + b.h / 2.0)
    };
    click(&mut ui, c);
    // Clicked at the very start.
    assert_eq!(ui.with::<TextInput<Msg>, _>(field, |t| t.cursor()), Some(0));
    let ctrl_right = (
        Key::Right,
        Modifiers {
            ctrl: true,
            ..Modifiers::default()
        },
    );
    press(&mut ui, ctrl_right.0, ctrl_right.1);
    assert_eq!(ui.with::<TextInput<Msg>, _>(field, |t| t.cursor()), Some(6));
    press(
        &mut ui,
        Key::End,
        Modifiers {
            shift: true,
            ..Modifiers::default()
        },
    );
    assert_eq!(
        ui.with::<TextInput<Msg>, _>(field, |t| t.selection()),
        Some(6..16)
    );
    press(&mut ui, Key::Backspace, none());
    assert_eq!(input_text(&mut ui, field), "alpha ");
}

#[test]
fn text_input_drag_selects_by_glyph_position() {
    let mut ui = ui();
    let root = ui.set_root(Container::column().fill().padding(10.0));
    let text = "hello world";
    let field = ui.add_child(root, TextInput::new().value(text));
    ui.layout_now();
    let b = ui.bounds(field).unwrap();
    let pad = 8.0;
    let x0 = b.x + pad + caret_x(&mut ui, text, 0) + 0.5;
    let x5 = b.x + pad + caret_x(&mut ui, text, 5) + 0.5;
    let y = b.y + b.h / 2.0;
    drag(&mut ui, Point::new(x0, y), Point::new(x5, y));
    assert_eq!(
        ui.with::<TextInput<Msg>, _>(field, |t| t.selection()),
        Some(0..5)
    );
    // Typing replaces the selection.
    press(&mut ui, Key::Char('J'), none());
    assert_eq!(input_text(&mut ui, field), "J world");

    // A long-press (touch) selects the word under it.
    let xw = b.x + pad + caret_x(&mut ui, "J world", 4) + 0.5;
    ui.event(Event::LongPress {
        pos: Point::new(xw, y),
    });
    assert_eq!(
        ui.with::<TextInput<Msg>, _>(field, |t| t.selection()),
        Some(2..7)
    );
}

#[test]
fn text_input_scrolls_to_keep_the_caret_visible() {
    let mut ui = ui();
    let root = ui.set_root(Container::row().fill().padding(10.0));
    let narrow = ui.add_child(root, Container::column().width(140.0));
    let field = ui.add_child(narrow, TextInput::new());
    ui.layout_now();
    let b = ui.bounds(field).unwrap();
    assert!(b.w <= 140.0);
    click(&mut ui, Point::new(b.x + 5.0, b.y + b.h / 2.0));
    type_str(&mut ui, "a value far wider than the box it lives in");
    let scroll = ui
        .with::<TextInput<Msg>, _>(field, |t| t.scroll_offset())
        .unwrap();
    assert!(scroll > 0.0, "scrolled to follow the caret: {scroll}");
    press(&mut ui, Key::Home, none());
    let scroll = ui
        .with::<TextInput<Msg>, _>(field, |t| t.scroll_offset())
        .unwrap();
    assert_eq!(scroll, 0.0, "Home scrolls back to the start");

    // Painting a scrolled field must not panic and stays inside its box.
    let mut surface = Surface::new(400, 300, Scale::ONE);
    ui.paint(&mut surface);
}

#[test]
fn text_area_enter_makes_lines_and_arrows_move_between_them() {
    let mut ui = ui();
    let root = ui.set_root(Container::column().fill().padding(10.0));
    let area = ui.add_child(root, TextArea::new().on_change(Msg::Area));
    ui.layout_now();
    let b = ui.bounds(area).unwrap();
    click(&mut ui, Point::new(b.x + 20.0, b.y + 20.0));
    assert_eq!(ui.focused(), Some(area));

    type_str(&mut ui, "ab\ncdef\ngh");
    assert_eq!(area_text(&mut ui, area), "ab\ncdef\ngh");
    assert_eq!(
        ui.take_messages().last(),
        Some(&Msg::Area("ab\ncdef\ngh".into()))
    );
    assert_eq!(area_cursor(&mut ui, area), 10);

    // Up from the end of "gh" lands on the middle line near column 2.
    press(&mut ui, Key::Up, none());
    let c = area_cursor(&mut ui, area);
    assert!((3..=7).contains(&c), "on line 2: {c}");
    assert_eq!(c, 5, "column kept: {c}");
    press(&mut ui, Key::Up, none());
    assert_eq!(area_cursor(&mut ui, area), 2, "line 1, clamped to its end");
    press(&mut ui, Key::Up, none());
    assert_eq!(
        area_cursor(&mut ui, area),
        0,
        "Up on the first line goes home"
    );

    // Down twice remembers the goal column across the shorter line? Start at
    // column 2 of line 1, go down through line 2 to line 3 (2 chars).
    press(&mut ui, Key::End, none());
    press(&mut ui, Key::Down, none());
    press(&mut ui, Key::Down, none());
    assert_eq!(area_cursor(&mut ui, area), 10, "end of gh");
    press(&mut ui, Key::Down, none());
    assert_eq!(
        area_cursor(&mut ui, area),
        10,
        "Down on the last line stays at end"
    );

    // Home/End are per visual line; Ctrl+Home/End are document-wide.
    press(&mut ui, Key::Up, none());
    press(&mut ui, Key::Home, none());
    assert_eq!(area_cursor(&mut ui, area), 3);
    press(&mut ui, Key::End, none());
    assert_eq!(area_cursor(&mut ui, area), 7);
    press(
        &mut ui,
        Key::Home,
        Modifiers {
            ctrl: true,
            ..Modifiers::default()
        },
    );
    assert_eq!(area_cursor(&mut ui, area), 0);
    press(
        &mut ui,
        Key::End,
        Modifiers {
            ctrl: true,
            ..Modifiers::default()
        },
    );
    assert_eq!(area_cursor(&mut ui, area), 10);

    // Shift+Up selects back across the line break, and Ctrl+X cuts it.
    press(&mut ui, Key::Up, shift());
    assert_eq!(
        ui.with::<TextArea<Msg>, _>(area, |t| t.selection()),
        Some(5..10)
    );
    let (k, m) = ctrl('x');
    press(&mut ui, k, m);
    assert_eq!(area_text(&mut ui, area), "ab\ncd");
    assert_eq!(ui.clipboard(), "ef\ngh");
    let (k, m) = ctrl('v');
    press(&mut ui, k, m);
    assert_eq!(
        area_text(&mut ui, area),
        "ab\ncdef\ngh",
        "multi-line paste keeps breaks"
    );
}

#[test]
fn text_area_click_places_the_caret_on_the_clicked_line() {
    let mut ui = ui();
    let root = ui.set_root(Container::column().fill().padding(10.0));
    let area = ui.add_child(root, TextArea::new().value("first\nsecond\nthird"));
    ui.layout_now();
    let b = ui.bounds(area).unwrap();
    let lh = ui.theme().metrics.font_size * 1.25;
    let pad = 8.0;
    // Far right on the second line → its end.
    click(&mut ui, Point::new(b.right() - 20.0, b.y + pad + lh * 1.5));
    assert_eq!(area_cursor(&mut ui, area), 12);
    // Far left on the third line → its start.
    click(&mut ui, Point::new(b.x + pad + 0.5, b.y + pad + lh * 2.5));
    assert_eq!(area_cursor(&mut ui, area), 13);
    // Drag from line 1 start to line 2 end selects across the break.
    drag(
        &mut ui,
        Point::new(b.x + pad + 0.5, b.y + pad + lh * 0.5),
        Point::new(b.right() - 20.0, b.y + pad + lh * 1.5),
    );
    assert_eq!(
        ui.with::<TextArea<Msg>, _>(area, |t| t.selection()),
        Some(0..12)
    );
}

#[test]
fn text_area_wraps_and_down_moves_within_a_paragraph() {
    let mut ui = ui();
    let root = ui.set_root(Container::row().fill().padding(10.0));
    let narrow = ui.add_child(root, Container::column().width(160.0));
    let long = "one two three four five six seven eight nine ten eleven twelve";
    let area = ui.add_child(narrow, TextArea::new().value(long).rows(6));
    ui.layout_now();
    let b = ui.bounds(area).unwrap();
    click(&mut ui, Point::new(b.x + 9.0, b.y + 9.0));
    assert_eq!(area_cursor(&mut ui, area), 0);
    press(&mut ui, Key::Down, none());
    let c = area_cursor(&mut ui, area);
    assert!(
        c > 0 && c < long.len(),
        "moved to the next wrapped line: {c}"
    );
    press(&mut ui, Key::End, none());
    let e = area_cursor(&mut ui, area);
    assert!(e > c && e < long.len(), "end of a wrapped visual line: {e}");
}

#[test]
fn text_area_scrolls_to_follow_the_caret_and_wheel_bubbles_at_bounds() {
    let mut ui = ui();
    let root = ui.set_root(Container::column().fill().padding(10.0));
    let sv = ui.add_child(root, ScrollView::new());
    let col = ui.add_child(sv, Container::column().gap(8.0));
    let area = ui.add_child(col, TextArea::new().rows(2));
    // Filler so the ScrollView itself has overflow.
    for _ in 0..12 {
        ui.add_child(col, TextInput::new());
    }
    ui.layout_now();
    let b = ui.bounds(area).unwrap();
    click(&mut ui, Point::new(b.x + 20.0, b.y + 12.0));
    let offset = |ui: &mut Ui<Msg>| {
        ui.with::<TextArea<Msg>, _>(area, |t| t.scroll_offset())
            .unwrap()
    };
    assert_eq!(offset(&mut ui), 0.0);
    type_str(&mut ui, "1\n2\n3\n4\n5\n6");
    assert!(
        offset(&mut ui) > 0.0,
        "caret on line 6 pulled the view down"
    );
    press(
        &mut ui,
        Key::Home,
        Modifiers {
            ctrl: true,
            ..Modifiers::default()
        },
    );
    assert_eq!(offset(&mut ui), 0.0, "caret back at the top scrolls up");

    // Wheel down inside the area scrolls the area, not the page.
    let wheel = |ui: &mut Ui<Msg>, dy: f32| {
        ui.event(Event::Scroll {
            pos: Point::new(b.x + 20.0, b.y + 12.0),
            delta_x: 0.0,
            delta_y: dy,
        });
    };
    let page_before = ui.bounds(area).unwrap().y;
    wheel(&mut ui, 10.0);
    assert!(offset(&mut ui) > 0.0, "wheel scrolled the area");
    ui.layout_now();
    assert_eq!(ui.bounds(area).unwrap().y, page_before, "page untouched");
    // Wheel up at the top bubbles to the ScrollView (which is already at its
    // top, so nothing moves) — and wheel down past the bottom bubbles too.
    wheel(&mut ui, -1000.0);
    assert_eq!(offset(&mut ui), 0.0);
    wheel(&mut ui, 1000.0);
    wheel(&mut ui, 50.0);
    ui.layout_now();
    assert!(
        ui.bounds(area).unwrap().y < page_before,
        "past the area's bottom the wheel moved the page"
    );
}

#[test]
fn text_area_paints_selection_caret_and_scrollbar_without_panicking() {
    let mut ui = ui();
    let root = ui.set_root(Container::column().fill().padding(10.0));
    let area = ui.add_child(root, TextArea::new().rows(2).value("a\nb\nc\nd\ne\nf"));
    ui.layout_now();
    let b = ui.bounds(area).unwrap();
    click(&mut ui, Point::new(b.x + 20.0, b.y + 12.0));
    ui.with::<TextArea<Msg>, _>(area, |t| t.select_all());
    press(
        &mut ui,
        Key::End,
        Modifiers {
            ctrl: true,
            shift: true,
            alt: false,
        },
    );
    let mut surface = Surface::new(400, 300, Scale::ONE);
    ui.paint(&mut surface);
    let mut surface2 = Surface::new(400, 300, Scale::new(2.0));
    ui.set_size(Size::new(200.0, 150.0), Scale::new(2.0));
    ui.paint(&mut surface2);
}
