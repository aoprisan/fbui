//! # fbui-render — the CPU rendering layer
//!
//! Phase 2 of [`fbui`](https://github.com/aoprisan/fbui): a **headless** software
//! renderer that turns drawing commands into pixels in a normal-RAM shadow
//! buffer, then copies only the damaged spans out to a scanout buffer. It is the
//! middle layer of the stack — below the widgets, above the platform:
//!
//! ```text
//!   fbui-widgets   (Phase 3)  retained tree, layout, theming
//!   fbui-render    (here)      painter, text, damage, copy-out
//!   fbui-platform  (Phase 1)   Display / input / seat / VT
//! ```
//!
//! ## What's headless about it
//!
//! Nothing here opens a device. A [`Surface`] is a [`tiny_skia::Pixmap`] plus a
//! [`Scale`] and a [`DamageTracker`]; you [`paint`](Surface::paint) into it with a
//! [`Painter`], then [`present_to_buffer`](Surface::present_to_buffer) blits the
//! changed regions into any byte slice with a stride and a [`TargetFormat`]. That
//! makes every primitive snapshot-testable with no hardware (see `fbui-testkit`).
//!
//! The single coupling to the platform layer — driving a real
//! `fbui_platform::Display` — lives behind the off-by-default **`platform`**
//! feature, so the core crate has zero device dependencies, exactly as PLAN §4
//! requires ("headless by design — depends on `fbui-platform` only in examples").
//!
//! ## The pieces
//!
//! * [`Painter`] — rects, rounded rects, arbitrary paths, strokes, linear/radial
//!   gradients, rectangular clipping, opacity groups, and image blits, all in
//!   logical coordinates with damage reported in device pixels.
//! * [`text`] — cosmic-text shaping/bidi/fallback + swash rasterization through a
//!   bounded [glyph atlas](text::FontContext); CJK and RTL work given covering
//!   fonts.
//! * [`DamageTracker`] — dirty-rect merge heuristics and buffer-age unioning, so
//!   an aged double-buffer is brought fully current and idle frames cost nothing.
//! * [`Scale`] — fractional HiDPI plumbed end to end.
//!
//! ## A minimal frame
//!
//! ```
//! use fbui_render::{Surface, Scale, Color, TargetFormat, geom::Rect};
//!
//! let mut surface = Surface::new(320, 240, Scale::ONE);
//! surface.paint(|p| {
//!     p.fill_rect(Rect::new(10.0, 10.0, 100.0, 40.0), Color::rgb(0x33, 0x88, 0xff));
//!     p.fill_rounded_rect(Rect::new(10.0, 60.0, 100.0, 40.0), 8.0, Color::WHITE);
//! });
//!
//! // Copy the damaged spans into a scanout-shaped buffer (here, a plain Vec).
//! let stride = 320 * 4;
//! let mut scanout = vec![0u8; stride * 240];
//! let damage = surface.present_to_buffer(&mut scanout, stride, TargetFormat::Xrgb8888, 0);
//! assert!(!damage.is_empty());
//! ```

pub mod color;
pub mod copyout;
pub mod damage;
pub mod geom;
pub mod image;
pub mod painter;
pub mod path;
pub mod sample;
pub mod scale;
pub mod surface;
pub mod text;

#[cfg(feature = "platform")]
mod platform_glue;

pub use color::Color;
pub use copyout::TargetFormat;
pub use damage::DamageTracker;
pub use geom::{IRect, Point, Rect, Size};
pub use image::Image;
pub use painter::Painter;
pub use path::{Path, PathBuilder};
pub use scale::Scale;
pub use surface::{encode_png_rgba, Surface};
pub use text::{FontContext, FontFamily, TextLayout, TextStyle};
