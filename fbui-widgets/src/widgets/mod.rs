//! The v1 widget set (PLAN §3.3). Each widget is a small, self-contained
//! implementation of [`Widget`](crate::Widget); containers get their children
//! from the [`Ui`](crate::Ui) tree, the rest are leaves that paint themselves.
//!
//! The overlay layer builds on [`Stack`] and the floating-overlay hooks:
//! [`Dialog`] (modal scrim + focus trap), [`Select`] (dropdown menu),
//! [`Toasts`] (transient notifications).

mod button;
mod checkbox;
mod container;
mod dialog;
mod image;
mod keyboard;
mod label;
mod list;
mod progressbar;
mod radio;
mod scroll;
mod select;
mod slider;
mod stack;
mod switch;
mod text_input;
mod toast;

pub use button::{Button, ButtonVariant};
pub use checkbox::Checkbox;
pub use container::{Align, Container};
pub use dialog::Dialog;
pub use image::ImageView;
pub use keyboard::Keyboard;
pub use label::Label;
pub use list::List;
pub use progressbar::ProgressBar;
pub use radio::RadioGroup;
pub use scroll::ScrollView;
pub use select::Select;
pub use slider::Slider;
pub use stack::Stack;
pub use switch::Switch;
pub use text_input::TextInput;
pub use toast::{ToastKind, Toasts};
