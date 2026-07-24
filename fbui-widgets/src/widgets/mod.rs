//! The v1 widget set (PLAN §3.3). Each widget is a small, self-contained
//! implementation of [`Widget`](crate::Widget); containers get their children
//! from the [`Ui`](crate::Ui) tree, the rest are leaves that paint themselves.
//!
//! The overlay layer builds on [`Stack`], the floating-overlay hooks, and the
//! popup layer ([`Ui::open_popup`](crate::Ui::open_popup)): [`Dialog`] (modal
//! scrim + focus trap), [`Select`] (dropdown menu), [`Menu`] / [`ContextMenu`]
//! (floating action menus), [`Toasts`] (transient notifications).

mod button;
mod chart;
mod checkbox;
mod container;
mod context_menu;
mod dialog;
mod gauge;
mod image;
mod keyboard;
mod label;
mod list;
mod menu;
mod progressbar;
mod radio;
mod scroll;
mod select;
mod slider;
mod spinner;
mod stack;
mod switch;
mod tabbar;
mod text_input;
mod toast;

pub use button::{Button, ButtonVariant};
pub use chart::Chart;
pub use checkbox::Checkbox;
pub use container::{Align, Container};
pub use context_menu::ContextMenu;
pub use dialog::Dialog;
pub use gauge::Gauge;
pub use image::ImageView;
pub use keyboard::Keyboard;
pub use label::Label;
pub use list::List;
pub use menu::{Menu, MenuItem};
pub use progressbar::ProgressBar;
pub use radio::RadioGroup;
pub use scroll::ScrollView;
pub use select::Select;
pub use slider::Slider;
pub use spinner::Spinner;
pub use stack::Stack;
pub use switch::Switch;
pub use tabbar::TabBar;
pub use text_input::TextInput;
pub use toast::{ToastKind, Toasts};
