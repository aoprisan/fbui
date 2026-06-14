//! [`Container`] — a flexbox row or column. Its children are managed by the
//! [`Ui`](crate::Ui) tree, so the widget itself is pure layout configuration.
//!
//! For *overlapping* (z-stacked) children, see [`Stack`](super::Stack).

use std::any::Any;

use fbui_render::Color;

use crate::ctx::PaintCtx;
use crate::style::{self, Style};
use crate::theme::Theme;
use crate::widget::Widget;

/// Cross-axis alignment of children.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Start,
    Center,
    End,
    Stretch,
}

/// A flex container. Build with [`Container::row`] or [`Container::column`].
pub struct Container {
    column: bool,
    gap: f32,
    padding: f32,
    align: Align,
    grow: f32,
    background: Option<Color>,
    radius: f32,
    fill: bool,
    width: Option<f32>,
    height: Option<f32>,
}

impl Container {
    fn base(column: bool) -> Self {
        Container {
            column,
            gap: 0.0,
            padding: 0.0,
            align: Align::Stretch,
            grow: 0.0,
            background: None,
            radius: 0.0,
            fill: false,
            width: None,
            height: None,
        }
    }

    /// A horizontal row.
    pub fn row() -> Self {
        Container::base(false)
    }

    /// A vertical column.
    pub fn column() -> Self {
        Container::base(true)
    }

    /// Spacing between children, in logical pixels.
    pub fn gap(mut self, gap: f32) -> Self {
        self.gap = gap;
        self
    }

    /// Uniform inner padding.
    pub fn padding(mut self, padding: f32) -> Self {
        self.padding = padding;
        self
    }

    pub fn align(mut self, align: Align) -> Self {
        self.align = align;
        self
    }

    /// Flex-grow weight (share of leftover space along the parent's main axis).
    pub fn grow(mut self, grow: f32) -> Self {
        self.grow = grow;
        self
    }

    /// Fill the parent on both axes (100% width/height).
    pub fn fill(mut self) -> Self {
        self.fill = true;
        self
    }

    /// Fix the container's width, in logical pixels (overridden by
    /// [`fill`](Self::fill)). Gives an `auto`-sized child a definite length to
    /// lay out within.
    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    /// Fix the container's height, in logical pixels (overridden by
    /// [`fill`](Self::fill)).
    pub fn height(mut self, height: f32) -> Self {
        self.height = Some(height);
        self
    }

    /// Paint a rounded background behind the children.
    pub fn background(mut self, color: Color, radius: f32) -> Self {
        self.background = Some(color);
        self.radius = radius;
        self
    }
}

impl<Msg: 'static> Widget<Msg> for Container {
    fn layout_style(&self, _theme: &Theme) -> Style {
        let align = match self.align {
            Align::Start => taffy::AlignItems::START,
            Align::Center => taffy::AlignItems::CENTER,
            Align::End => taffy::AlignItems::END,
            Align::Stretch => taffy::AlignItems::STRETCH,
        };
        let size = if self.fill {
            taffy::Size {
                width: style::percent(1.0),
                height: style::percent(1.0),
            }
        } else {
            taffy::Size {
                width: self.width.map(style::length).unwrap_or_else(style::auto),
                height: self.height.map(style::length).unwrap_or_else(style::auto),
            }
        };
        Style {
            display: taffy::Display::Flex,
            flex_direction: if self.column {
                taffy::FlexDirection::Column
            } else {
                taffy::FlexDirection::Row
            },
            gap: taffy::Size {
                width: taffy::LengthPercentage::length(self.gap),
                height: taffy::LengthPercentage::length(self.gap),
            },
            padding: style::uniform(self.padding),
            align_items: Some(align),
            flex_grow: self.grow,
            size,
            ..Style::default()
        }
    }

    fn paint(&self, ctx: &mut PaintCtx) {
        if let Some(bg) = self.background {
            let b = ctx.bounds();
            ctx.painter().fill_rounded_rect(b, self.radius, bg);
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
