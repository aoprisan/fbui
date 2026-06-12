//! A sample scene — the "settings page" mock from the Phase 2 exit criteria.
//!
//! This is **demo scaffolding**, not part of the framework's widget model
//! (that's Phase 3). It exists so the benchmark, the headless PNG example, and
//! the on-device example all draw the *same* text-heavy, ~30-element scene, which
//! is exactly what PLAN §4's perf gate is specified against. Drawing it takes a
//! [`Painter`] and a [`FontContext`]; everything is plain painter calls.

use crate::color::Color;
use crate::geom::{Point, Rect};
use crate::painter::Painter;
use crate::text::{FontContext, TextStyle};

/// Palette for the sample, roughly a dark settings UI.
struct Palette {
    bg: Color,
    panel: Color,
    header: Color,
    text: Color,
    muted: Color,
    accent: Color,
    track: Color,
}

const DARK: Palette = Palette {
    bg: Color::rgb(0x14, 0x16, 0x1b),
    panel: Color::rgb(0x1e, 0x21, 0x29),
    header: Color::rgb(0x26, 0x2b, 0x36),
    text: Color::rgb(0xe8, 0xea, 0xf0),
    muted: Color::rgb(0x9a, 0xa0, 0xb0),
    accent: Color::rgb(0x4c, 0x8d, 0xff),
    track: Color::rgb(0x3a, 0x40, 0x4e),
};

/// One row's content.
struct Setting {
    label: &'static str,
    detail: &'static str,
    on: bool,
}

const SETTINGS: &[Setting] = &[
    Setting {
        label: "Wi-Fi",
        detail: "homenet-5G",
        on: true,
    },
    Setting {
        label: "Bluetooth",
        detail: "2 devices",
        on: true,
    },
    Setting {
        label: "Airplane Mode",
        detail: "Off",
        on: false,
    },
    Setting {
        label: "Notifications",
        detail: "Banners",
        on: true,
    },
    Setting {
        label: "Dark Appearance",
        detail: "Always",
        on: true,
    },
    Setting {
        label: "Location Services",
        detail: "While Using",
        on: false,
    },
];

/// Paint the settings page into `p` at logical size `w × h`.
///
/// `highlight` optionally tints one row's toggle (the on-device example uses it
/// to show selection); pass `None` for the static scene.
pub fn settings_page(
    p: &mut Painter<'_>,
    fonts: &mut FontContext,
    w: f32,
    h: f32,
    highlight: Option<usize>,
) {
    let pal = &DARK;
    p.fill_rect(Rect::new(0.0, 0.0, w, h), pal.bg);

    // Header bar with a subtle vertical gradient and the page title.
    let header_h = 56.0;
    p.fill_linear_gradient(
        Rect::new(0.0, 0.0, w, header_h),
        Point::new(0.0, 0.0),
        Point::new(0.0, header_h),
        &[(0.0, pal.header), (1.0, pal.panel)],
    );
    fonts.draw_text(
        p,
        "Settings",
        &TextStyle::new(22.0, pal.text).bold(),
        Point::new(20.0, 16.0),
        None,
    );

    // The settings list inside a rounded panel.
    let margin = 16.0;
    let panel = Rect::new(
        margin,
        header_h + margin,
        w - 2.0 * margin,
        h - header_h - 2.0 * margin,
    );
    p.fill_rounded_rect(panel, 12.0, pal.panel);

    let row_h = ((panel.h - 16.0) / SETTINGS.len() as f32).min(64.0);
    let label_style = TextStyle::new(16.0, pal.text);
    let detail_style = TextStyle::new(13.0, pal.muted);

    for (i, s) in SETTINGS.iter().enumerate() {
        let y = panel.y + 8.0 + i as f32 * row_h;
        // Separator between rows (not before the first).
        if i > 0 {
            p.fill_rect(Rect::new(panel.x + 12.0, y, panel.w - 24.0, 1.0), pal.track);
        }

        let text_x = panel.x + 16.0;
        fonts.draw_text(p, s.label, &label_style, Point::new(text_x, y + 10.0), None);
        fonts.draw_text(
            p,
            s.detail,
            &detail_style,
            Point::new(text_x, y + 30.0),
            None,
        );

        // Toggle switch on the right.
        let tw = 44.0;
        let th = 24.0;
        let tx = panel.right() - 16.0 - tw;
        let ty = y + (row_h - th) / 2.0;
        let on = s.on;
        let track_color = if on {
            if highlight == Some(i) {
                pal.text
            } else {
                pal.accent
            }
        } else {
            pal.track
        };
        p.fill_rounded_rect(Rect::new(tx, ty, tw, th), th / 2.0, track_color);
        // Knob.
        let knob = th - 6.0;
        let kx = if on { tx + tw - knob - 3.0 } else { tx + 3.0 };
        p.fill_rounded_rect(
            Rect::new(kx, ty + 3.0, knob, knob),
            knob / 2.0,
            Color::WHITE,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scale::Scale;
    use crate::surface::Surface;

    #[test]
    fn settings_page_paints_and_damages() {
        let mut fonts = FontContext::new();
        let mut surface = Surface::new(400, 300, Scale::ONE);
        surface.paint(|p| settings_page(p, &mut fonts, 400.0, 300.0, None));
        assert!(!surface.is_clean());
    }
}
