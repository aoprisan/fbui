//! A widget snapshot test that is deliberately **text-free**, so it's
//! deterministic across hosts (no font dependency): nested containers with
//! backgrounds plus sliders at known values. This exercises the layout → paint →
//! damage path end to end and pins the geometry of the painted output.
//!
//! Regenerate after an intentional change:
//! `FBUI_UPDATE_SNAPSHOTS=1 cargo test -p fbui-widgets --test snapshot`

use fbui_render::geom::Size;
use fbui_render::{Color, Scale, Surface};
use fbui_testkit::{assert_snapshot_in, Tolerance};
use fbui_widgets::widgets::{Container, Keyboard, Slider, Spinner, Stack, TabBar};
use fbui_widgets::{Theme, Ui};

#[derive(Clone)]
enum Msg {}

#[test]
fn sliders_in_panels() {
    let (w, h) = (300u32, 200u32);
    let mut ui = Ui::<Msg>::new(Size::new(w as f32, h as f32), Scale::ONE, Theme::dark());

    let root = ui.set_root(Container::column().fill().padding(16.0).gap(12.0));
    for (i, v) in [10.0f32, 50.0, 90.0].into_iter().enumerate() {
        let panel = ui.add_child(
            root,
            Container::row()
                .padding(12.0)
                .grow(1.0)
                .background(Color::rgb(0x24 + i as u8 * 8, 0x28, 0x32), 8.0),
        );
        ui.add_child(panel, Slider::new(0.0, 100.0, v));
    }

    let mut surface = Surface::new(w, h, Scale::ONE);
    ui.paint(&mut surface);

    assert_snapshot_in(
        "tests/snapshots",
        "sliders_in_panels",
        surface.pixmap(),
        Tolerance::FUZZY,
    );
}

/// A `Stack` overlays three differently-sized, opaque panels at the same origin;
/// each later one is smaller, so the result is a set of nested rectangles —
/// proving children share a box and z-order by insertion (last on top). Text-free
/// for host determinism.
#[test]
fn stacked_panels_overlap_back_to_front() {
    let (w, h) = (240u32, 180u32);
    let mut ui = Ui::<Msg>::new(Size::new(w as f32, h as f32), Scale::ONE, Theme::dark());

    let stack = ui.set_root(Stack::new());
    // Back: fills the whole stack.
    ui.add_child(
        stack,
        Container::column()
            .fill()
            .background(Color::rgb(0x30, 0x36, 0x46), 0.0),
    );
    // Middle: a smaller centered-ish panel (sized, so it pins to the origin).
    ui.add_child(
        stack,
        Container::column()
            .width(160.0)
            .height(120.0)
            .background(Color::rgb(0x4c, 0x8d, 0xff), 12.0),
    );
    // Front: smaller still, drawn on top of the other two.
    ui.add_child(
        stack,
        Container::column()
            .width(80.0)
            .height(60.0)
            .background(Color::rgb(0xe5, 0x4b, 0x4b), 8.0),
    );

    let mut surface = Surface::new(w, h, Scale::ONE);
    ui.paint(&mut surface);

    assert_snapshot_in(
        "tests/snapshots",
        "stacked_panels",
        surface.pixmap(),
        Tolerance::FUZZY,
    );
}

/// The on-screen keyboard's key grid, docked to fill a filled column. Text-free
/// (the default `Ui` loads no font, so key labels don't render) — this pins the
/// per-key geometry and the theme-derived key colors across the layers' rows.
#[test]
fn keyboard_key_grid() {
    let (w, h) = (360u32, 232u32);
    let mut ui = Ui::<Msg>::new(Size::new(w as f32, h as f32), Scale::ONE, Theme::dark());

    let root = ui.set_root(Container::column().fill());
    ui.add_child(root, Keyboard::new().height(h as f32));

    let mut surface = Surface::new(w, h, Scale::ONE);
    ui.paint(&mut surface);

    assert_snapshot_in(
        "tests/snapshots",
        "keyboard_key_grid",
        surface.pixmap(),
        Tolerance::FUZZY,
    );
}

/// The Phase-5+ additions side by side: a three-segment `TabBar` with the
/// middle tab active, and a `Spinner` frozen at phase 0. This pins the segment
/// geometry, the active-segment fill, and the spinner's dot ring with its
/// brightness fade. Tab labels render with host fonts under the tolerant
/// compare, the same footing as `keyboard_key_grid`.
#[test]
fn tabbar_and_spinner() {
    let (w, h) = (260u32, 120u32);
    let mut ui = Ui::<Msg>::new(Size::new(w as f32, h as f32), Scale::ONE, Theme::dark());

    let root = ui.set_root(Container::column().fill().padding(12.0).gap(12.0));
    ui.add_child(root, TabBar::new(["one", "two", "three"]).selected(1));
    let row = ui.add_child(root, Container::row().grow(1.0));
    ui.add_child(row, Spinner::new().size(48.0));

    let mut surface = Surface::new(w, h, Scale::ONE);
    ui.paint(&mut surface);

    assert_snapshot_in(
        "tests/snapshots",
        "tabbar_and_spinner",
        surface.pixmap(),
        Tolerance::FUZZY,
    );
}

/// An open `Menu` on the popup layer: enabled items, a separator, a disabled
/// item, and the keyboard-hovered first row. Labels render with host fonts
/// under the tolerant compare (the `tabbar_and_spinner` footing); the box,
/// row highlight, separator rule, and dimmed disabled row are pinned.
#[test]
fn menu_open() {
    use fbui_render::geom::Point;
    use fbui_widgets::event::{Event, Key, Modifiers};
    use fbui_widgets::widgets::Menu;
    use fbui_widgets::PopupOptions;

    let (w, h) = (240u32, 200u32);
    let mut ui = Ui::<Msg>::new(Size::new(w as f32, h as f32), Scale::ONE, Theme::dark());

    let root = ui.set_root(Container::column().fill().padding(12.0));
    // Entries: 0 "Cut", 1 "Copy", 2 separator, 3 "Paste" (disabled).
    let menu = ui.add_child(
        root,
        Menu::new(["Cut", "Copy"])
            .separator()
            .item("Paste")
            .disable(3),
    );
    ui.with::<Menu<Msg>, _>(menu, |m| m.open_at(Point::new(24.0, 20.0)));
    ui.open_popup(menu, PopupOptions::default());
    // Arrow down: the first enabled row gets the hover highlight.
    ui.event(Event::Key {
        key: Key::Down,
        pressed: true,
        mods: Modifiers::default(),
    });

    let mut surface = Surface::new(w, h, Scale::ONE);
    ui.paint(&mut surface);

    assert_snapshot_in(
        "tests/snapshots",
        "menu_open",
        surface.pixmap(),
        Tolerance::FUZZY,
    );
}

/// An open `ContextMenu` near the bottom-right corner: the menu flips above
/// the anchor point and clamps inside the surface, over a filled content
/// region. Pins the flip/clamp geometry and the shared menu chrome.
#[test]
fn context_menu_open() {
    use fbui_render::geom::Point;
    use fbui_widgets::widgets::ContextMenu;
    use fbui_widgets::PopupOptions;

    let (w, h) = (240u32, 160u32);
    let mut ui = Ui::<Msg>::new(Size::new(w as f32, h as f32), Scale::ONE, Theme::dark());

    let root = ui.set_root(Container::column().fill().padding(8.0));
    let cm = ui.add_child(root, ContextMenu::new(["Rename", "Delete"]).fill());
    ui.add_child(
        cm,
        Container::column()
            .fill()
            .background(Color::rgb(0x2a, 0x30, 0x3e), 8.0),
    );
    // Anchor near the bottom-right: two rows don't fit below or to the right,
    // so the box flips up and clamps left.
    ui.with::<ContextMenu<Msg>, _>(cm, |m| m.open_at(Point::new(210.0, 140.0)));
    ui.open_popup(cm, PopupOptions::default());

    let mut surface = Surface::new(w, h, Scale::ONE);
    ui.paint(&mut surface);

    assert_snapshot_in(
        "tests/snapshots",
        "context_menu_open",
        surface.pixmap(),
        Tolerance::FUZZY,
    );
}

/// A visible tooltip pinned against the top edge: `Above` placement has no
/// headroom, so the tip flips below its owner and centers on it. Text renders
/// with host fonts under the tolerant compare; the box, border, and flip
/// geometry are pinned.
#[test]
fn tooltip_shown() {
    use fbui_render::geom::Point;
    use fbui_widgets::event::Event;
    use fbui_widgets::widgets::Button;
    use fbui_widgets::Tooltip;

    let (w, h) = (240u32, 120u32);
    let mut ui = Ui::<Msg>::new(Size::new(w as f32, h as f32), Scale::ONE, Theme::dark());

    let root = ui.set_root(Container::column().padding(6.0));
    let btn = ui.add_child(root, Button::new("Save"));
    ui.set_tooltip(btn, Tooltip::new("Write to disk"));
    ui.layout_now();

    // Hover and run out the dwell on the frame clock.
    let b = ui.bounds(btn).unwrap();
    ui.event(Event::PointerMove {
        pos: Point::new(b.x + b.w / 2.0, b.y + b.h / 2.0),
    });
    while ui.animate(0.2) {}

    let mut surface = Surface::new(w, h, Scale::ONE);
    ui.paint(&mut surface);

    assert_snapshot_in(
        "tests/snapshots",
        "tooltip_shown",
        surface.pixmap(),
        Tolerance::FUZZY,
    );
}

/// The HMI instrument pair: a streamed two-series strip chart (fill + grids)
/// beside gauges with zone bands. Deliberately text-free (readouts off, no
/// gutter labels) so the goldens stay host-independent; all the vector
/// geometry — dials, zones, needles, traces, grids — is pinned.
#[test]
fn instruments_chart_and_gauges() {
    use fbui_widgets::widgets::{Chart, Gauge};

    let (w, h) = (360u32, 220u32);
    let mut ui = Ui::<Msg>::new(Size::new(w as f32, h as f32), Scale::ONE, Theme::dark());

    let root = ui.set_root(Container::column().fill().padding(10.0).gap(10.0));
    let dials = ui.add_child(root, Container::row().gap(10.0));
    let g1 = ui.add_child(
        dials,
        Gauge::new(0.0, 100.0)
            .zone(60.0, Color::rgb(0x34, 0xd3, 0x99))
            .zone(85.0, Color::rgb(0xfb, 0xbf, 0x24))
            .zone(100.0, Color::rgb(0xef, 0x44, 0x44))
            .show_value(false)
            .animate_secs(0.0),
    );
    let g2 = ui.add_child(
        dials,
        Gauge::new(0.0, 8.0).show_value(false).animate_secs(0.0),
    );
    let chart = ui.add_child(
        root,
        Chart::new()
            .fixed_range(0.0, 100.0)
            .fill(true)
            .time_grid_every(10)
            .gutter(0.0)
            .sample_width(3.0),
    );
    ui.layout_now();

    ui.with(g1, |g: &mut Gauge| g.set_value(72.0));
    ui.with(g2, |g: &mut Gauge| g.set_value(3.6));
    for i in 0..120u32 {
        ui.stream(chart, |c: &mut Chart| {
            c.push(&[
                (i as f32 * 0.23).sin() * 30.0 + 55.0,
                (i as f32 * 0.09).cos() * 18.0 + 25.0,
            ])
        });
    }

    let mut surface = Surface::new(w, h, Scale::ONE);
    ui.paint(&mut surface);

    assert_snapshot_in(
        "tests/snapshots",
        "instruments_chart_and_gauges",
        surface.pixmap(),
        Tolerance::FUZZY,
    );
}

/// A bare sparkline: the chrome-free chart preset used inline in status rows.
#[test]
fn sparkline_inline() {
    use fbui_widgets::widgets::Chart;

    let (w, h) = (120u32, 40u32);
    let mut ui = Ui::<Msg>::new(Size::new(w as f32, h as f32), Scale::ONE, Theme::dark());
    let root = ui.set_root(Container::column().fill().padding(8.0));
    let spark = ui.add_child(root, Chart::sparkline());
    ui.layout_now();
    for i in 0..80u32 {
        ui.stream(spark, |c: &mut Chart| {
            c.push_one((i as f32 * 0.31).sin() * (i as f32 * 0.02) + 2.0)
        });
    }

    let mut surface = Surface::new(w, h, Scale::ONE);
    ui.paint(&mut surface);

    assert_snapshot_in(
        "tests/snapshots",
        "sparkline_inline",
        surface.pixmap(),
        Tolerance::FUZZY,
    );
}
