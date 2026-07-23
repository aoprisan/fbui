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

// `run`, `Proxy`, and friends live behind the `platform` feature; intra-doc
// links to them only resolve when it's on. CI documents this crate with the
// feature enabled (full strictness); a headless doc build shouldn't fail on
// links to items it deliberately compiled out.
#![cfg_attr(not(feature = "platform"), allow(rustdoc::broken_intra_doc_links))]

pub use fbui_render as render;
pub use fbui_widgets as widgets_crate;

/// Enter a `tracing` span for the rest of the scope under the `profile` feature;
/// nothing otherwise. Lets the runner tag each frame phase with no cost when off.
/// Only defined with `platform`, since the runner is its sole user.
#[cfg(all(feature = "platform", feature = "profile"))]
macro_rules! span {
    ($name:expr) => {
        let _guard = tracing::info_span!($name).entered();
    };
}
#[cfg(all(feature = "platform", not(feature = "profile")))]
macro_rules! span {
    ($name:expr) => {};
}
#[cfg(feature = "platform")]
pub(crate) use span;

// Flatten the most-used names to the crate root. The `anim`, `style`, and
// `widget` modules are surfaced so downstream crates can implement the
// [`Widget`] trait — and size/animate their own widgets — without reaching past
// the umbrella into `fbui_widgets`.
pub use fbui_widgets::{
    anim, ctx, event, style, theme, tree, widget, widgets, Anim, AnimCtx, Event, InspectNode, Key,
    Modifiers, PaintCtx, PointerButton, Style, Theme, Ui, Widget, WidgetId,
};

#[cfg(feature = "remote")]
pub mod remote;

#[cfg(feature = "platform")]
mod hud;
#[cfg(feature = "platform")]
mod record;
#[cfg(feature = "platform")]
mod run;
#[cfg(feature = "platform")]
pub use fbui_platform::Result;
#[cfg(feature = "platform")]
pub use run::{run, App, Proxy};

// The timer queue is std-only and headless (its consumer, `Proxy`, is
// platform-gated) so its unit tests run everywhere.
#[cfg_attr(not(feature = "platform"), allow(dead_code))]
mod timer;
pub use timer::Timer;
