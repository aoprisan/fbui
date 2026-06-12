//! [`Label`] — static, measured text.

use std::any::Any;

use fbui_render::geom::{Point, Size};
use fbui_render::{Color, FontContext};

use crate::ctx::PaintCtx;
use crate::style::Style;
use crate::theme::Theme;
use crate::util::text_style;
use crate::widget::{AvailableSize, KnownDims, Widget};

/// A run of text. Sizes itself to its content (wrapping if a width is imposed).
pub struct Label {
    text: String,
    size: Option<f32>,
    color: Option<Color>,
    bold: bool,
    wrap: bool,
}

impl Label {
    pub fn new(text: impl Into<String>) -> Self {
        Label {
            text: text.into(),
            size: None,
            color: None,
            bold: false,
            wrap: false,
        }
    }

    /// Override the font size (default: theme body size).
    pub fn size(mut self, size: f32) -> Self {
        self.size = Some(size);
        self
    }

    /// Override the color (default: theme text color).
    pub fn color(mut self, color: Color) -> Self {
        self.color = Some(color);
        self
    }

    pub fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    /// Allow wrapping to the available width.
    pub fn wrap(mut self) -> Self {
        self.wrap = true;
        self
    }

    /// Replace the text (call via [`Ui::with`](crate::Ui::with)).
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
    }

    fn style_for(&self, theme: &Theme) -> fbui_render::TextStyle {
        let size = self.size.unwrap_or(theme.metrics.font_size);
        let color = self.color.unwrap_or(theme.palette.text);
        let s = text_style(theme, size, color);
        if self.bold {
            s.bold()
        } else {
            s
        }
    }
}

impl<Msg: 'static> Widget<Msg> for Label {
    fn layout_style(&self, _theme: &Theme) -> Style {
        Style::default()
    }

    fn measure(
        &mut self,
        fonts: &mut FontContext,
        theme: &Theme,
        known: KnownDims,
        available: AvailableSize,
    ) -> Option<Size> {
        let max_w = if self.wrap {
            known.width.or(match available.width {
                taffy::AvailableSpace::Definite(w) => Some(w),
                _ => None,
            })
        } else {
            None
        };
        let layout = fonts.layout(&self.text, &self.style_for(theme), max_w);
        Some(layout.size())
    }

    fn paint(&self, ctx: &mut PaintCtx) {
        let style = self.style_for(ctx.theme());
        let b = ctx.bounds();
        let max_w = if self.wrap { Some(b.w) } else { None };
        let (p, fonts) = ctx.painter_and_fonts();
        fonts.draw_text(p, &self.text, &style, Point::new(b.x, b.y), max_w);
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
