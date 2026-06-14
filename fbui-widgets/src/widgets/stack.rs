//! [`Stack`] — a container whose children overlap, z-ordered by insertion.
//!
//! Where [`Container`](super::Container) flows its children in a row or column, a
//! `Stack` places every child in the *same* box: the [`Ui`](crate::Ui) gives each
//! child of a stack `position: absolute` filling the stack, so they paint
//! back-to-front in insertion order (the last child added is on top) and are
//! hit-tested front-to-back. A child keeps its own size if it sets one (pinned to
//! the top-left), or fills the stack if its size is `auto`.
//!
//! This is the primitive the overlay widgets build on: a full-screen `Stack` with
//! the page as child 0 and a floating layer (a modal scrim, a toast, a popover)
//! as child 1 — the layer draws over the page and intercepts pointer events that
//! land on it first.
//!
//! Because absolutely-positioned children don't contribute to their parent's
//! content size, a `Stack` is sized by *its own* box, not its children: it fills
//! its parent by default (the overlay-host case). Give it a definite size through
//! the parent's layout if you need something smaller.

use std::any::Any;

use fbui_render::Color;

use crate::ctx::PaintCtx;
use crate::style::{self, Style};
use crate::theme::Theme;
use crate::widget::Widget;

/// A container that overlays its children (see module docs). Build with
/// [`Stack::new`] and add children with [`Ui::add_child`](crate::Ui::add_child);
/// later children draw on top.
pub struct Stack {
    fill: bool,
    background: Option<Color>,
    radius: f32,
}

impl Stack {
    /// A stack that fills its parent on both axes — the common case, an overlay
    /// host spanning the whole surface.
    pub fn new() -> Self {
        Stack {
            fill: true,
            background: None,
            radius: 0.0,
        }
    }

    /// Don't fill the parent; take the size the parent's layout assigns (e.g. a
    /// flex child with a fixed length). Children still fill *this* stack.
    pub fn loose(mut self) -> Self {
        self.fill = false;
        self
    }

    /// Paint a rounded background behind the children.
    pub fn background(mut self, color: Color, radius: f32) -> Self {
        self.background = Some(color);
        self.radius = radius;
        self
    }
}

impl Default for Stack {
    fn default() -> Self {
        Stack::new()
    }
}

impl<Msg: 'static> Widget<Msg> for Stack {
    fn layout_style(&self, _theme: &Theme) -> Style {
        let size = if self.fill {
            taffy::Size {
                width: style::percent(1.0),
                height: style::percent(1.0),
            }
        } else {
            taffy::Size {
                width: style::auto(),
                height: style::auto(),
            }
        };
        Style {
            display: taffy::Display::Flex,
            // A positioned containing block, so children's `inset` resolves
            // against this stack's box.
            position: taffy::Position::Relative,
            size,
            ..Style::default()
        }
    }

    fn stacks_children(&self) -> bool {
        true
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
