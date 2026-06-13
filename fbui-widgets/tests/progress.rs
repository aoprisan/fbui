//! ProgressBar fill behavior — font-free and deterministic (tiny-skia), so it's
//! a structural pixel assertion rather than a committed golden image: the accent
//! fill must grow monotonically with the fraction.

use fbui_render::geom::Size;
use fbui_render::{Color, Scale, Surface};
use fbui_widgets::widgets::{Container, ProgressBar};
use fbui_widgets::{Theme, Ui};

#[derive(Clone)]
enum Msg {}

/// Count pixels exactly equal to `accent` — the solid fill interior (opaque, so
/// premultiplied == straight); AA edges differ but the interior is exact.
fn accent_pixels(surface: &Surface, accent: Color) -> u32 {
    let want = [accent.r, accent.g, accent.b];
    surface
        .pixmap()
        .pixels()
        .iter()
        .filter(|px| [px.red(), px.green(), px.blue()] == want)
        .count() as u32
}

fn fill_at(fraction: f32) -> u32 {
    let theme = Theme::dark();
    let accent = theme.palette.accent;
    let (w, h) = (200u32, 40u32);
    let mut ui = Ui::<Msg>::new(Size::new(w as f32, h as f32), Scale::ONE, theme);
    let root = ui.set_root(Container::column().fill().padding(8.0));
    ui.add_child(root, ProgressBar::new(fraction));
    let mut surface = Surface::new(w, h, Scale::ONE);
    ui.paint(&mut surface);
    accent_pixels(&surface, accent)
}

#[test]
fn fill_grows_with_fraction() {
    let empty = fill_at(0.0);
    let half = fill_at(0.5);
    let full = fill_at(1.0);
    assert_eq!(empty, 0, "an empty bar paints no accent fill");
    assert!(
        half > empty,
        "half fills more than empty: {half} vs {empty}"
    );
    assert!(full > half, "full fills more than half: {full} vs {half}");
}

#[test]
fn fraction_is_clamped() {
    assert_eq!(ProgressBar::new(2.0).fraction(), 1.0);
    assert_eq!(ProgressBar::new(-1.0).fraction(), 0.0);
    let mut b = ProgressBar::new(0.5);
    b.set_fraction(9.0);
    assert_eq!(b.fraction(), 1.0);
}
