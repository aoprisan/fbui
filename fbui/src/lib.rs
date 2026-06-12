//! # fbui — a framebuffer UI framework for Linux
//!
//! The umbrella crate. It re-exports the rendering layer ([`render`]) and the
//! widget toolkit ([`widgets`]), and — behind the `platform` feature — provides
//! [`run`], which drives a widget [`Ui`] on a real display (DRM/KMS or fbdev)
//! through the Phase 1 platform layer.
//!
//! ```no_run
//! # #[cfg(feature = "platform")]
//! # fn main() -> fbui::Result<()> {
//! use fbui::widgets::{Container, Label};
//! use fbui::{App, Ui};
//!
//! struct Counter;
//! #[derive(Clone)]
//! enum Msg {}
//! impl App for Counter {
//!     type Message = Msg;
//!     fn build(&mut self, ui: &mut Ui<Msg>) {
//!         let root = ui.set_root(Container::column().padding(16.0).fill());
//!         ui.add_child(root, Label::new("hello from a TTY"));
//!     }
//!     fn update(&mut self, _msg: Msg, _ui: &mut Ui<Msg>) {}
//! }
//! fbui::run(Counter)?;
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "platform"))] fn main() {}
//! ```

pub use fbui_render as render;
pub use fbui_widgets as widgets_crate;

// Flatten the most-used names to the crate root.
pub use fbui_widgets::{
    ctx, event, theme, tree, widgets, Event, Key, Modifiers, PaintCtx, PointerButton, Style, Theme,
    Ui, Widget, WidgetId,
};

#[cfg(feature = "platform")]
mod run;
#[cfg(feature = "platform")]
pub use fbui_platform::Result;
#[cfg(feature = "platform")]
pub use run::{run, App};
