//! Button variants paint from the right theme role — font-free and
//! deterministic (the fill is a rounded rect, drawn with no glyphs), so we can
//! assert the fill color directly.

use fbui_render::geom::Size;
use fbui_render::{Color, Scale, Surface};
use fbui_widgets::widgets::{Button, ButtonVariant, Container};
use fbui_widgets::{Theme, Ui};

#[derive(Clone)]
enum Msg {}

fn count(surface: &Surface, c: Color) -> u32 {
    let want = [c.r, c.g, c.b];
    surface
        .pixmap()
        .pixels()
        .iter()
        .filter(|px| [px.red(), px.green(), px.blue()] == want)
        .count() as u32
}

/// Render a single button of `variant`; return `(danger_px, accent_px)`.
fn fills(variant: ButtonVariant) -> (u32, u32) {
    let theme = Theme::dark();
    let (danger, accent) = (theme.palette.danger, theme.palette.accent);
    let (w, h) = (160u32, 60u32);
    let mut ui = Ui::<Msg>::new(Size::new(w as f32, h as f32), Scale::ONE, theme);
    let root = ui.set_root(Container::column().fill().padding(8.0));
    ui.add_child(root, Button::new("Go").variant(variant));
    let mut surface = Surface::new(w, h, Scale::ONE);
    ui.paint(&mut surface);
    (count(&surface, danger), count(&surface, accent))
}

#[test]
fn danger_fills_with_danger_not_accent() {
    let (danger_px, accent_px) = fills(ButtonVariant::Danger);
    assert!(danger_px > 0, "danger button paints the danger fill");
    assert_eq!(accent_px, 0, "danger button never uses the accent fill");
}

#[test]
fn primary_fills_with_accent_not_danger() {
    let (danger_px, accent_px) = fills(ButtonVariant::Primary);
    assert!(accent_px > 0, "primary button paints the accent fill");
    assert_eq!(danger_px, 0, "primary button never uses the danger fill");
}

#[test]
fn secondary_uses_neither_accent_nor_danger_fill() {
    // Secondary fills with `surface_alt`; its only accent is the focus ring, which
    // an unfocused button doesn't draw.
    let (danger_px, accent_px) = fills(ButtonVariant::Secondary);
    assert_eq!(danger_px, 0);
    assert_eq!(accent_px, 0);
}
