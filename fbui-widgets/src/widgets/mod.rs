//! The v1 widget set (PLAN §3.3). Each widget is a small, self-contained
//! implementation of [`Widget`](crate::Widget); containers get their children
//! from the [`Ui`](crate::Ui) tree, the rest are leaves that paint themselves.
//!
//! The overlay layer builds on [`Stack`], the floating-overlay hooks, and the
//! popup layer ([`Ui::open_popup`](crate::Ui::open_popup)): [`Dialog`] (modal
//! scrim + focus trap), [`Select`] (dropdown menu), [`Menu`] / [`ContextMenu`]
//! (floating action menus), [`Toasts`] (transient notifications).
//!
//! Text editing — [`TextInput`] (single line) and [`TextArea`] (multi-line) —
//! shares one editing core and the [`Ui`](crate::Ui)'s process clipboard; see
//! `docs/text-editing.md` for the key table.

#[allow(unused_imports)]
use crate::prelude::*;

// The minimal set — layout, text, images, a button, scrolling, a list and a
// progress bar — is always compiled. Everything else is behind the
// `all-widgets` feature (on in the hosted default set), so a `no_std`
// firmware build with `default-features = false` links only what a small
// appliance UI needs. See NOSTD.md.

mod button;
#[cfg(feature = "all-widgets")]
mod calendar;
#[cfg(feature = "all-widgets")]
mod chart;
#[cfg(feature = "all-widgets")]
mod checkbox;
mod container;
#[cfg(feature = "all-widgets")]
mod context_menu;
#[cfg(feature = "all-widgets")]
mod dialog;
#[cfg(feature = "all-widgets")]
mod edit;
#[cfg(feature = "all-widgets")]
mod gauge;
mod image;
#[cfg(feature = "all-widgets")]
mod keyboard;
mod label;
mod list;
#[cfg(feature = "all-widgets")]
mod menu;
#[cfg(feature = "all-widgets")]
mod navigator;
mod progressbar;
#[cfg(feature = "all-widgets")]
mod radio;
mod scroll;
#[cfg(feature = "all-widgets")]
mod select;
#[cfg(feature = "all-widgets")]
mod slider;
#[cfg(feature = "all-widgets")]
mod spinner;
mod stack;
#[cfg(feature = "all-widgets")]
mod switch;
#[cfg(feature = "all-widgets")]
mod tabbar;
#[cfg(feature = "all-widgets")]
mod text_area;
#[cfg(feature = "all-widgets")]
mod text_input;
#[cfg(feature = "all-widgets")]
mod toast;
#[cfg(feature = "all-widgets")]
mod tree_view;
#[cfg(feature = "all-widgets")]
mod video;

pub use button::{Button, ButtonVariant};
#[cfg(feature = "all-widgets")]
pub use calendar::{Calendar, Date};
#[cfg(feature = "all-widgets")]
pub use chart::Chart;
#[cfg(feature = "all-widgets")]
pub use checkbox::Checkbox;
pub use container::{Align, Container};
#[cfg(feature = "all-widgets")]
pub use context_menu::ContextMenu;
#[cfg(feature = "all-widgets")]
pub use dialog::Dialog;
#[cfg(feature = "all-widgets")]
pub use gauge::Gauge;
pub use image::ImageView;
#[cfg(feature = "all-widgets")]
pub use keyboard::Keyboard;
pub use label::Label;
pub use list::List;
#[cfg(feature = "all-widgets")]
pub use menu::{Menu, MenuItem};
#[cfg(feature = "all-widgets")]
pub use navigator::Navigator;
pub use progressbar::ProgressBar;
#[cfg(feature = "all-widgets")]
pub use radio::RadioGroup;
pub use scroll::ScrollView;
#[cfg(feature = "all-widgets")]
pub use select::Select;
#[cfg(feature = "all-widgets")]
pub use slider::Slider;
#[cfg(feature = "all-widgets")]
pub use spinner::Spinner;
pub use stack::Stack;
#[cfg(feature = "all-widgets")]
pub use switch::Switch;
#[cfg(feature = "all-widgets")]
pub use tabbar::TabBar;
#[cfg(feature = "all-widgets")]
pub use text_area::TextArea;
#[cfg(feature = "all-widgets")]
pub use text_input::TextInput;
#[cfg(feature = "all-widgets")]
pub use toast::{ToastKind, Toasts};
#[cfg(feature = "all-widgets")]
pub use tree_view::{NodeId, TreeNode, TreeView};
#[cfg(feature = "all-widgets")]
pub use video::{fit_rect, VideoFit, VideoView};
