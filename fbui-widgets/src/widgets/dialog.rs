//! [`Dialog`] — a modal barrier + centered content host.
//!
//! A dialog is *structure*, not magic: add it as the last child of a
//! [`Stack`](super::Stack) so it paints over the page, and add the card (a
//! [`Container`](super::Container) with a background) as its child — the
//! dialog centers whatever it holds. Being modal is the dialog's own behavior:
//! it fills the stack with a translucent scrim, swallows every pointer event
//! that isn't claimed by its children, traps Tab focus inside its subtree, and
//! emits `on_dismiss` for Esc or a click on the scrim.
//!
//! Opening and closing are tree operations owned by the app:
//!
//! ```ignore
//! // open: build the subtree, then move focus inside it
//! let dialog = ui.add_child(stack, Dialog::new().on_dismiss(|| Msg::CloseDialog));
//! let card = ui.add_child(dialog, Container::column().padding(20.0).gap(12.0)
//!     .background(theme_surface, 12.0));
//! ui.add_child(card, Label::new("Erase everything?"));
//! ui.add_child(card, Button::new("Cancel").secondary().on_press(|| Msg::CloseDialog));
//! ui.focus_first(dialog);
//!
//! // close (in App::update, on Msg::CloseDialog):
//! ui.remove(dialog);
//! ```
//!
//! Esc reaches the dialog by key *bubbling*: keys go to the focused widget and
//! climb the ancestor chain while unhandled, so any focus inside the card lets
//! the dialog see Esc. That's why opening should call
//! [`Ui::focus_first`](crate::Ui::focus_first).

use std::any::Any;

use fbui_render::Color;

use crate::ctx::{EventCtx, PaintCtx};
use crate::event::{Event, Key};
use crate::style::{self, Style};
use crate::theme::Theme;
use crate::widget::Widget;

/// A modal scrim that centers its children and blocks input to everything
/// beneath it (see the module docs for the open/close pattern).
pub struct Dialog<Msg> {
    on_dismiss: Option<Box<dyn Fn() -> Msg>>,
    scrim: Color,
    dismiss_on_scrim: bool,
}

impl<Msg> Dialog<Msg> {
    pub fn new() -> Self {
        Dialog {
            on_dismiss: None,
            scrim: Color::rgba(0, 0, 0, 140),
            dismiss_on_scrim: true,
        }
    }

    /// The message emitted when the user asks to dismiss (Esc, or a click on
    /// the scrim). Without it the dialog only closes when the app removes it.
    pub fn on_dismiss(mut self, f: impl Fn() -> Msg + 'static) -> Self {
        self.on_dismiss = Some(Box::new(f));
        self
    }

    /// Override the scrim color (default: translucent black).
    pub fn scrim(mut self, color: Color) -> Self {
        self.scrim = color;
        self
    }

    /// Whether a click on the scrim dismisses (default `true`). Turn it off
    /// for dialogs that must be answered through their buttons.
    pub fn dismiss_on_scrim(mut self, yes: bool) -> Self {
        self.dismiss_on_scrim = yes;
        self
    }

    fn dismiss(&self, ctx: &mut EventCtx<Msg>) {
        if let Some(f) = &self.on_dismiss {
            ctx.emit(f());
        }
    }
}

impl<Msg> Default for Dialog<Msg> {
    fn default() -> Self {
        Dialog::new()
    }
}

impl<Msg: 'static> Widget<Msg> for Dialog<Msg> {
    fn layout_style(&self, _theme: &Theme) -> Style {
        Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            // Center the card.
            align_items: Some(taffy::AlignItems::CENTER),
            justify_content: Some(taffy::JustifyContent::CENTER),
            size: taffy::Size {
                width: style::percent(1.0),
                height: style::percent(1.0),
            },
            ..Style::default()
        }
    }

    fn traps_focus(&self) -> bool {
        true
    }

    fn paint(&self, ctx: &mut PaintCtx) {
        let b = ctx.bounds();
        ctx.painter().fill_rect(b, self.scrim);
    }

    fn event(&mut self, ctx: &mut EventCtx<Msg>) {
        let ev = ctx.event().clone();
        match ev {
            // A press-and-release on the scrim itself (clicks on the card land
            // on its children and never reach here).
            Event::PointerUp { .. } => {
                if self.dismiss_on_scrim {
                    self.dismiss(ctx);
                }
                ctx.set_handled();
            }
            // The modal barrier: nothing that lands on the scrim, and no
            // scroll bubbling out of the card, gets past the dialog.
            Event::PointerDown { .. }
            | Event::PointerMove { .. }
            | Event::Scroll { .. }
            | Event::Tap { .. }
            | Event::LongPress { .. }
            | Event::Fling { .. } => {
                ctx.set_handled();
            }
            // Bubbled up from whatever is focused inside the card.
            Event::Key {
                key: Key::Escape,
                pressed: true,
                ..
            } => {
                self.dismiss(ctx);
                ctx.set_handled();
            }
            _ => {}
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
