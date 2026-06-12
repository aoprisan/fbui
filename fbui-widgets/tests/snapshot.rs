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
use fbui_widgets::widgets::{Container, Slider};
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
