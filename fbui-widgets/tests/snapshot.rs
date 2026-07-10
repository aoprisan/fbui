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
