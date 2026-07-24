//! # fbui-widgets — the widget toolkit
//!
//! Phase 3 of [`fbui`](https://github.com/aoprisan/fbui): a **retained** widget
//! tree with an Elm-ish control loop — `update(msg) → mutate state → mark damage
//! → paint` — laid out by `taffy` and drawn through the Phase 2
//! [`Painter`](fbui_render::Painter). It
//! is headless: a [`Ui`] runs and paints into a [`fbui_render::Surface`] with no
//! device in the loop, which is what makes widgets snapshot-testable. The
//! umbrella `fbui` crate wires a `Ui` to a real display via Phase 1.
//!
//! See `DESIGN.md` for the data model, the damage-propagation rules, and the
//! input/focus model.
//!
//! ## A minimal tree
//!
//! ```
//! use fbui_widgets::{Ui, Theme, widgets::{Container, Label, Button}};
//! use fbui_render::geom::Size;
//! use fbui_render::Scale;
//!
//! #[derive(Clone)]
//! enum Msg { Clicked }
//!
//! let mut ui = Ui::<Msg>::new(Size::new(320.0, 240.0), Scale::ONE, Theme::dark());
//! let root = ui.set_root(Container::column().padding(16.0).gap(8.0).fill());
//! ui.add_child(root, Label::new("Hello, fbui"));
//! ui.add_child(root, Button::new("Click me").on_press(|| Msg::Clicked));
//! ```

/// Enter a `tracing` span for the rest of the current scope when the `profile`
/// feature is on; expands to nothing otherwise (zero cost in normal builds).
/// Defined before the modules so they can all use it.
#[cfg(feature = "profile")]
macro_rules! span {
    ($name:expr) => {
        let _guard = tracing::info_span!($name).entered();
    };
}
#[cfg(not(feature = "profile"))]
macro_rules! span {
    ($name:expr) => {};
}
pub(crate) use span;

pub mod anim;
pub mod ctx;
pub mod event;
pub mod gesture;
pub mod kinetic;
pub mod popup;
pub mod style;
pub mod theme;
pub mod tree;
mod util;
pub mod widget;
pub mod widgets;

pub use anim::{Easing, Lerp, Tween};
pub use ctx::{AnimCtx, EventCtx, PaintCtx};
pub use event::{Event, Key, Modifiers, PointerButton};
pub use gesture::{Gesture, GestureConfig, GestureRecognizer};
pub use popup::{place_anchored, Alignment, AnchorSpec, Placement};
pub use style::Style;
pub use theme::{Metrics, Palette, Theme};
pub use tree::{InspectNode, PopupOptions, StreamDamage, Tooltip, Ui, WidgetId};
pub use widget::{Anim, Widget};

// Re-export the render layer so downstreams need only depend on the toolkit.
pub use fbui_render;
