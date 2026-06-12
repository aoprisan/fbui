//! The painter: an immediate-mode drawing API over a tiny-skia shadow buffer.
//!
//! Everything here speaks **logical** coordinates; the painter holds a
//! [`Scale`] and applies it to tiny-skia as a transform, so the same widget code
//! draws correctly at 1× and 2×. Every primitive reports its device-pixel
//! bounding box to the [`DamageTracker`], clamped to the active clip, so only
//! what actually changed gets copied out.
//!
//! Three pieces of state stack:
//!
//! * **Clip** — rectangular, intersected on push. Drawing is masked to the
//!   current clip (a device-space tiny-skia [`Mask`] rebuilt on change), and
//!   damage is clamped to it.
//! * **Opacity groups** — `push_opacity` redirects subsequent drawing into a
//!   fresh transparent layer; `pop_opacity` composites that layer back with a
//!   global alpha. This is how a whole subtree fades as one, instead of each
//!   primitive blending separately (which double-darkens overlaps).
//!
//! The painter never owns the shadow buffer; it borrows it (and the damage
//! tracker) from [`crate::Surface`] for the duration of one paint pass.

use tiny_skia::{
    BlendMode, FillRule, GradientStop, LinearGradient, Mask, Paint, PathBuilder, PixmapPaint,
    RadialGradient, Shader, SpreadMode, Stroke, Transform,
};

use crate::color::Color;
use crate::damage::DamageTracker;
use crate::geom::{IRect, Point, Rect};
use crate::image::Image;
use crate::path::Path;
use crate::scale::Scale;

/// One opacity group: an off-screen layer and the alpha to composite it with.
struct Layer {
    pixmap: tiny_skia::Pixmap,
    opacity: f32,
}

/// Borrows a shadow buffer and paints into it, accumulating damage.
pub struct Painter<'a> {
    base: &'a mut tiny_skia::Pixmap,
    damage: &'a mut DamageTracker,
    scale: Scale,
    surface: IRect,
    /// Active opacity groups; the innermost is drawn into, `base` if empty.
    layers: Vec<Layer>,
    /// Intersected clip rectangles in device space; top is the active clip.
    clip_stack: Vec<IRect>,
    /// Cached device-space mask for the active clip, or `None` for no clip.
    clip_mask: Option<Mask>,
}

impl<'a> Painter<'a> {
    pub(crate) fn new(
        base: &'a mut tiny_skia::Pixmap,
        damage: &'a mut DamageTracker,
        scale: Scale,
    ) -> Self {
        let surface = IRect::from_wh(base.width(), base.height());
        Painter {
            base,
            damage,
            scale,
            surface,
            layers: Vec::new(),
            clip_stack: Vec::new(),
            clip_mask: None,
        }
    }

    /// The scale factor in effect.
    pub fn scale(&self) -> Scale {
        self.scale
    }

    /// The active clip in device pixels (the whole surface if none was pushed).
    pub fn clip(&self) -> IRect {
        self.clip_stack.last().copied().unwrap_or(self.surface)
    }

    /// Mutable view of the buffer currently being drawn into (innermost layer or
    /// the base). Used by the text module to composite glyph coverage directly.
    pub(crate) fn target(&mut self) -> &mut tiny_skia::Pixmap {
        match self.layers.last_mut() {
            Some(layer) => &mut layer.pixmap,
            None => self.base,
        }
    }

    /// Report a device-pixel damage rectangle (clamped to the active clip and
    /// surface). Exposed for the text module, which knows its own glyph bounds.
    pub fn add_damage(&mut self, dev: IRect) {
        let clamped = dev
            .intersect(self.clip())
            .clamp_to(self.surface.w, self.surface.h);
        self.damage.add(clamped);
    }

    /// Convert a logical rect to its damage rect under the current scale.
    fn damage_logical(&mut self, r: Rect) {
        let dev = self.scale.to_device_rect(r);
        self.add_damage(dev);
    }

    // ---- solid fills -----------------------------------------------------

    /// Fill the entire current target with `color` (whole-surface damage). Used
    /// to lay down an opaque base before incremental painting.
    pub fn clear(&mut self, color: Color) {
        let clip = self.clip();
        self.target().fill(color.to_tiny());
        self.add_damage(clip);
    }

    /// Fill a rectangle with a solid color.
    pub fn fill_rect(&mut self, rect: Rect, color: Color) {
        let Some(ts_rect) = rect.to_tiny() else {
            return;
        };
        let mut paint = Paint::default();
        paint.set_color(color.to_tiny());
        paint.anti_alias = true;
        let (t, mask) = (self.scale.transform(), self.clip_mask.clone());
        self.target().fill_rect(ts_rect, &paint, t, mask.as_ref());
        self.damage_logical(rect);
    }

    /// Stroke a rectangle outline of the given logical width, centered on the edge.
    pub fn stroke_rect(&mut self, rect: Rect, color: Color, width: f32) {
        let Some(path) = Path::rect(rect) else { return };
        self.stroke_path(&path, color, width);
    }

    /// Fill a rounded rectangle.
    pub fn fill_rounded_rect(&mut self, rect: Rect, radius: f32, color: Color) {
        if let Some(path) = Path::rounded_rect(rect, radius) {
            self.fill_path(&path, color);
        }
    }

    /// Stroke a rounded rectangle outline.
    pub fn stroke_rounded_rect(&mut self, rect: Rect, radius: f32, color: Color, width: f32) {
        if let Some(path) = Path::rounded_rect(rect, radius) {
            self.stroke_path(&path, color, width);
        }
    }

    // ---- paths -----------------------------------------------------------

    /// Fill an arbitrary path (non-zero winding) with a solid color.
    pub fn fill_path(&mut self, path: &Path, color: Color) {
        let mut paint = Paint::default();
        paint.set_color(color.to_tiny());
        paint.anti_alias = true;
        let (t, mask) = (self.scale.transform(), self.clip_mask.clone());
        self.target()
            .fill_path(&path.0, &paint, FillRule::Winding, t, mask.as_ref());
        self.damage_logical(path.bounds());
    }

    /// Stroke an arbitrary path with a solid color and logical line width.
    pub fn stroke_path(&mut self, path: &Path, color: Color, width: f32) {
        let mut paint = Paint::default();
        paint.set_color(color.to_tiny());
        paint.anti_alias = true;
        let stroke = Stroke {
            width,
            ..Stroke::default()
        };
        let (t, mask) = (self.scale.transform(), self.clip_mask.clone());
        self.target()
            .stroke_path(&path.0, &paint, &stroke, t, mask.as_ref());
        // Grow damage by half the stroke width on each side.
        self.damage_logical(path.bounds().inset(-(width / 2.0 + 1.0)));
    }

    // ---- gradients -------------------------------------------------------

    /// Fill a rectangle with a linear gradient running from `start` to `end`
    /// (logical coordinates). `stops` are `(offset 0–1, color)` pairs.
    pub fn fill_linear_gradient(
        &mut self,
        rect: Rect,
        start: Point,
        end: Point,
        stops: &[(f32, Color)],
    ) {
        let Some(ts_rect) = rect.to_tiny() else {
            return;
        };
        let Some(shader) = LinearGradient::new(
            to_ts_point(start),
            to_ts_point(end),
            to_stops(stops),
            SpreadMode::Pad,
            Transform::identity(),
        ) else {
            return;
        };
        self.fill_rect_with_shader(ts_rect, shader);
        self.damage_logical(rect);
    }

    /// Fill a rectangle with a radial gradient centered at `center` with the
    /// given logical `radius`.
    pub fn fill_radial_gradient(
        &mut self,
        rect: Rect,
        center: Point,
        radius: f32,
        stops: &[(f32, Color)],
    ) {
        let Some(ts_rect) = rect.to_tiny() else {
            return;
        };
        let Some(shader) = RadialGradient::new(
            to_ts_point(center),
            0.0,
            to_ts_point(center),
            radius,
            to_stops(stops),
            SpreadMode::Pad,
            Transform::identity(),
        ) else {
            return;
        };
        self.fill_rect_with_shader(ts_rect, shader);
        self.damage_logical(rect);
    }

    fn fill_rect_with_shader(&mut self, ts_rect: tiny_skia::Rect, shader: Shader<'_>) {
        let mut paint = Paint {
            shader,
            ..Paint::default()
        };
        paint.anti_alias = true;
        let (t, mask) = (self.scale.transform(), self.clip_mask.clone());
        self.target().fill_rect(ts_rect, &paint, t, mask.as_ref());
    }

    // ---- images ----------------------------------------------------------

    /// Blit a decoded image with its top-left at logical point `at`. The image is
    /// drawn 1:1 in device pixels (see [`Image`]).
    pub fn draw_image(&mut self, image: &Image, at: Point) {
        let dx = (at.x * self.scale.factor()).round() as i32;
        let dy = (at.y * self.scale.factor()).round() as i32;
        let paint = PixmapPaint {
            opacity: 1.0,
            blend_mode: BlendMode::SourceOver,
            ..PixmapPaint::default()
        };
        let mask = self.clip_mask.clone();
        self.target().draw_pixmap(
            dx,
            dy,
            image.pixmap.as_ref(),
            &paint,
            Transform::identity(),
            mask.as_ref(),
        );
        self.add_damage(IRect::new(dx, dy, image.width(), image.height()));
    }

    // ---- clip ------------------------------------------------------------

    /// Intersect the clip with `rect` (logical) until the matching `pop_clip`.
    pub fn push_clip(&mut self, rect: Rect) {
        let dev = self.scale.to_device_rect(rect);
        let new = self.clip().intersect(dev);
        self.clip_stack.push(new);
        self.rebuild_clip_mask();
    }

    /// Undo the most recent [`push_clip`](Self::push_clip).
    pub fn pop_clip(&mut self) {
        self.clip_stack.pop();
        self.rebuild_clip_mask();
    }

    fn rebuild_clip_mask(&mut self) {
        let (w, h) = (self.surface.w.max(1), self.surface.h.max(1));
        self.clip_mask = match self.clip_stack.last().copied() {
            // No explicit clip -> no mask (draw to the whole surface).
            None => None,
            // Fully clipped out: an all-zero mask discards every pixel.
            Some(c) if c.is_empty() => Mask::new(w, h),
            Some(c) => Self::rect_mask(w, h, c),
        };
    }

    /// Build a device-space coverage mask that admits exactly the rectangle `c`.
    fn rect_mask(w: u32, h: u32, c: IRect) -> Option<Mask> {
        let mut mask = Mask::new(w, h)?;
        let mut pb = PathBuilder::new();
        pb.push_rect(tiny_skia::Rect::from_xywh(
            c.x as f32, c.y as f32, c.w as f32, c.h as f32,
        )?);
        let path = pb.finish()?;
        // Crisp rectangular clip: no AA, identity transform (already device space).
        mask.fill_path(&path, FillRule::Winding, false, Transform::identity());
        Some(mask)
    }

    // ---- opacity groups --------------------------------------------------

    /// Begin an opacity group: subsequent drawing accumulates in an off-screen
    /// layer, composited back at `alpha` (0–1) on `pop_opacity`.
    pub fn push_opacity(&mut self, alpha: f32) {
        let pixmap = tiny_skia::Pixmap::new(self.surface.w.max(1), self.surface.h.max(1))
            .expect("opacity layer alloc");
        self.layers.push(Layer {
            pixmap,
            opacity: alpha.clamp(0.0, 1.0),
        });
    }

    /// Composite the innermost opacity group back into its parent.
    pub fn pop_opacity(&mut self) {
        let Some(layer) = self.layers.pop() else {
            return;
        };
        let paint = PixmapPaint {
            opacity: layer.opacity,
            blend_mode: BlendMode::SourceOver,
            ..PixmapPaint::default()
        };
        let mask = self.clip_mask.clone();
        self.target().draw_pixmap(
            0,
            0,
            layer.pixmap.as_ref(),
            &paint,
            Transform::identity(),
            mask.as_ref(),
        );
    }
}

fn to_ts_point(p: Point) -> tiny_skia::Point {
    tiny_skia::Point::from_xy(p.x, p.y)
}

fn to_stops(stops: &[(f32, Color)]) -> Vec<GradientStop> {
    stops
        .iter()
        .map(|&(pos, c)| GradientStop::new(pos, c.to_tiny()))
        .collect()
}
