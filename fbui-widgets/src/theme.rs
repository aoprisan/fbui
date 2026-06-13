//! Theming: a plain value, no globals.
//!
//! A [`Theme`] is palette + metrics + font choices. It lives in the [`Ui`] and is
//! handed to widgets through the paint/layout contexts; switching it at runtime
//! just damages the root. Light and dark are built in; a custom theme is any
//! `Theme` value.
//!
//! [`Ui`]: crate::Ui

use fbui_render::text::FontFamily;
use fbui_render::Color;

/// The semantic color roles widgets paint with.
#[derive(Debug, Clone)]
pub struct Palette {
    /// Window background.
    pub bg: Color,
    /// Raised surfaces (panels, cards, inputs).
    pub surface: Color,
    /// A surface one step further raised / pressed.
    pub surface_alt: Color,
    /// Primary text.
    pub text: Color,
    /// Secondary / disabled text.
    pub muted: Color,
    /// Accent (selected toggles, focus ring, primary buttons).
    pub accent: Color,
    /// Text drawn on top of `accent`.
    pub on_accent: Color,
    /// Destructive/danger actions (a "delete"/"erase" button); `on_accent` text
    /// reads on top of it.
    pub danger: Color,
    /// Hairlines, separators, inactive tracks.
    pub line: Color,
}

/// Spacing/sizing metrics, in logical pixels.
#[derive(Debug, Clone)]
pub struct Metrics {
    /// Base spacing unit (padding/gap are multiples of this).
    pub unit: f32,
    /// Default corner radius.
    pub radius: f32,
    /// Body font size.
    pub font_size: f32,
    /// Focus-ring stroke width.
    pub focus_width: f32,
}

/// A complete visual theme.
#[derive(Debug, Clone)]
pub struct Theme {
    pub palette: Palette,
    pub metrics: Metrics,
    pub font: FontFamily,
}

impl Theme {
    /// The default dark theme.
    pub fn dark() -> Self {
        Theme {
            palette: Palette {
                bg: Color::rgb(0x14, 0x16, 0x1b),
                surface: Color::rgb(0x1e, 0x21, 0x29),
                surface_alt: Color::rgb(0x2a, 0x2f, 0x3a),
                text: Color::rgb(0xe8, 0xea, 0xf0),
                muted: Color::rgb(0x9a, 0xa0, 0xb0),
                accent: Color::rgb(0x4c, 0x8d, 0xff),
                on_accent: Color::WHITE,
                danger: Color::rgb(0xe5, 0x4b, 0x4b),
                line: Color::rgb(0x3a, 0x40, 0x4e),
            },
            metrics: default_metrics(),
            font: FontFamily::SansSerif,
        }
    }

    /// The default light theme.
    pub fn light() -> Self {
        Theme {
            palette: Palette {
                bg: Color::rgb(0xf5, 0xf6, 0xf8),
                surface: Color::WHITE,
                surface_alt: Color::rgb(0xe7, 0xe9, 0xee),
                text: Color::rgb(0x1a, 0x1c, 0x22),
                muted: Color::rgb(0x6a, 0x70, 0x80),
                accent: Color::rgb(0x1f, 0x6f, 0xff),
                on_accent: Color::WHITE,
                danger: Color::rgb(0xd6, 0x3a, 0x3a),
                line: Color::rgb(0xd2, 0xd6, 0xde),
            },
            metrics: default_metrics(),
            font: FontFamily::SansSerif,
        }
    }
}

fn default_metrics() -> Metrics {
    Metrics {
        unit: 8.0,
        radius: 8.0,
        font_size: 16.0,
        focus_width: 2.0,
    }
}

impl Default for Theme {
    fn default() -> Self {
        Theme::dark()
    }
}
