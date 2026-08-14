//! [`VideoView`] — a damage-aware surface for externally produced frames:
//! a camera preview, a decoded video, a screen share.
//!
//! fbui deliberately doesn't decode video — users bring V4L2, ffmpeg, or
//! GStreamer on a worker thread. What the framework provides is the zero-fuss
//! presentation path: hand each decoded frame to the widget and only the video
//! rect repaints, at the frame rate the producer sets, while the rest of the
//! UI stays idle (the ~0%-CPU rule applies to everything *outside* the video
//! box).
//!
//! The intended wiring uses the runner's `Proxy` and the
//! [`Ui::stream`](crate::Ui::stream) fast lane, so a 30–60 fps feed never
//! forces relayout or full-surface damage:
//!
//! ```ignore
//! // producer thread: capture/decode, convert, ship to the UI thread
//! let rgba = fbui_render::yuv::yuyv_to_rgba(&buf, w, h)?;
//! proxy.send(Msg::Frame(Image::from_rgba_bytes(w, h, &rgba)?));
//!
//! // App::update — precise damage, no relayout:
//! Msg::Frame(img) => { ui.stream(video, |v: &mut VideoView| v.push_frame(img)); }
//! ```
//!
//! Frames are [`Image`]s (premultiplied, blit-ready); the convenience
//! converters for the common camera/decoder formats live in
//! [`fbui_render::yuv`].

use std::any::Any;
use std::rc::Rc;

use fbui_render::geom::{Rect, Size};
use fbui_render::{Color, Image};

use crate::ctx::PaintCtx;
use crate::style::{self, Style};
use crate::theme::Theme;
use crate::tree::StreamDamage;
use crate::widget::Widget;

/// How a frame maps into the widget's box (CSS object-fit semantics).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VideoFit {
    /// Scale to fit entirely inside, preserving aspect ratio; letterbox the
    /// rest. The default — nothing is cropped.
    #[default]
    Contain,
    /// Scale to cover the whole box, preserving aspect ratio; crop overflow.
    Cover,
    /// Stretch to the box exactly, ignoring aspect ratio.
    Fill,
}

/// Where a `frame`-sized image lands inside `bounds` under `fit`. Aspect-fit
/// math only — pure and testable; the widget clips Cover's overflow.
pub fn fit_rect(frame: Size, bounds: Rect, fit: VideoFit) -> Rect {
    if frame.w <= 0.0 || frame.h <= 0.0 || bounds.is_empty() {
        return Rect::new(bounds.x, bounds.y, 0.0, 0.0);
    }
    let (sx, sy) = (bounds.w / frame.w, bounds.h / frame.h);
    let s = match fit {
        VideoFit::Contain => sx.min(sy),
        VideoFit::Cover => sx.max(sy),
        VideoFit::Fill => {
            return bounds;
        }
    };
    let (w, h) = (frame.w * s, frame.h * s);
    Rect::new(
        bounds.x + (bounds.w - w) / 2.0,
        bounds.y + (bounds.h - h) / 2.0,
        w,
        h,
    )
}

/// A widget that displays the most recent frame pushed into it (see module
/// docs). Fills the box its parent assigns; shows the letterbox color until
/// the first frame arrives.
pub struct VideoView {
    frame: Option<Rc<Image>>,
    fit: VideoFit,
    letterbox: Color,
}

impl VideoView {
    pub fn new() -> Self {
        VideoView {
            frame: None,
            fit: VideoFit::Contain,
            letterbox: Color::BLACK,
        }
    }

    /// Set the object-fit mode (default [`VideoFit::Contain`]).
    pub fn fit(mut self, fit: VideoFit) -> Self {
        self.fit = fit;
        self
    }

    /// The color behind the frame — the letterbox bars under
    /// [`Contain`](VideoFit::Contain), and the empty box before the first
    /// frame. Default black, like every video player.
    pub fn letterbox(mut self, color: Color) -> Self {
        self.letterbox = color;
        self
    }

    /// Replace the displayed frame. Prefer [`push_frame`](Self::push_frame)
    /// through [`Ui::stream`](crate::Ui::stream) for a live feed; this is the
    /// plain setter for [`Ui::with`](crate::Ui::with) (poster images, etc.).
    pub fn set_frame(&mut self, frame: Image) {
        self.frame = Some(Rc::new(frame));
    }

    /// Replace the displayed frame with a shared image (several views showing
    /// one feed).
    pub fn set_frame_shared(&mut self, frame: Rc<Image>) {
        self.frame = Some(frame);
    }

    /// Accept the next frame of a live feed, reporting the precise damage for
    /// [`Ui::stream`](crate::Ui::stream): repaint this widget's box, no
    /// relayout — the fast lane that lets a 60 fps feed coexist with an
    /// otherwise idle UI.
    pub fn push_frame(&mut self, frame: Image) -> StreamDamage {
        self.frame = Some(Rc::new(frame));
        StreamDamage::Repaint
    }

    /// The current frame's pixel size, if one has arrived.
    pub fn frame_size(&self) -> Option<Size> {
        self.frame.as_ref().map(|f| f.size())
    }
}

impl Default for VideoView {
    fn default() -> Self {
        VideoView::new()
    }
}

impl<Msg: 'static> Widget<Msg> for VideoView {
    fn layout_style(&self, _theme: &Theme) -> Style {
        // Fill whatever box the parent assigns: video regions are sized by
        // the surrounding layout, never by the (arbitrary) frame resolution —
        // a first frame arriving must not reflow the page.
        Style {
            size: taffy::Size {
                width: style::percent(1.0),
                height: style::percent(1.0),
            },
            flex_grow: 1.0,
            ..Style::default()
        }
    }

    fn clips(&self) -> bool {
        // Cover crops at the box edge; Contain/Fill never overflow, so the
        // clip costs nothing there beyond the mask push.
        true
    }

    fn paint(&self, ctx: &mut PaintCtx) {
        let b = ctx.bounds();
        let frame = self.frame.clone();
        let fit = self.fit;
        let letterbox = self.letterbox;
        let p = ctx.painter();
        p.push_clip(b);
        match frame {
            None => p.fill_rect(b, letterbox),
            Some(img) => {
                let dest = fit_rect(img.size(), b, fit);
                // Letterbox bars only where the frame doesn't cover.
                if fit == VideoFit::Contain
                    && (dest.w < b.w - f32::EPSILON || dest.h < b.h - f32::EPSILON)
                {
                    p.fill_rect(b, letterbox);
                }
                p.draw_image_scaled(&img, dest);
            }
        }
        p.pop_clip();
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contain_letterboxes_a_wide_frame() {
        // 200x100 frame in a 100x100 box: fits width, centered vertically.
        let r = fit_rect(
            Size::new(200.0, 100.0),
            Rect::new(0.0, 0.0, 100.0, 100.0),
            VideoFit::Contain,
        );
        assert_eq!((r.x, r.y, r.w, r.h), (0.0, 25.0, 100.0, 50.0));
    }

    #[test]
    fn cover_crops_a_wide_frame() {
        // Same frame under Cover: fits height, overflows horizontally,
        // centered.
        let r = fit_rect(
            Size::new(200.0, 100.0),
            Rect::new(0.0, 0.0, 100.0, 100.0),
            VideoFit::Cover,
        );
        assert_eq!((r.x, r.y, r.w, r.h), (-50.0, 0.0, 200.0, 100.0));
    }

    #[test]
    fn fill_stretches_to_the_box() {
        let b = Rect::new(10.0, 20.0, 64.0, 48.0);
        assert_eq!(fit_rect(Size::new(7.0, 3.0), b, VideoFit::Fill), b);
    }

    #[test]
    fn fit_respects_the_box_origin() {
        let r = fit_rect(
            Size::new(100.0, 100.0),
            Rect::new(50.0, 60.0, 40.0, 20.0),
            VideoFit::Contain,
        );
        // Square frame in a wide box: fits height, centered horizontally.
        assert_eq!((r.x, r.y, r.w, r.h), (60.0, 60.0, 20.0, 20.0));
    }

    #[test]
    fn degenerate_inputs_yield_empty() {
        let r = fit_rect(
            Size::new(0.0, 0.0),
            Rect::new(0.0, 0.0, 10.0, 10.0),
            VideoFit::Contain,
        );
        assert!(r.is_empty());
    }
}
