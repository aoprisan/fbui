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
//!
//! **Band origin.** A banded surface (see [`crate::Surface::banded`]) paints
//! the screen a strip at a time through a buffer only a few rows tall. The
//! painter then holds an `origin`: the device-space position of the buffer's
//! top-left pixel. Everything the painter tracks — clip, damage, the surface
//! rect — stays in full-surface device space; only the final pixel writes are
//! shifted by `-origin`, so a widget can't tell a band from the whole screen.
//! With a zero origin (every non-banded surface) the painter takes exactly
//! the code path it always did.

#[allow(unused_imports)]
use crate::prelude::*;

use tiny_skia::{
    BlendMode, FillRule, FilterQuality, GradientStop, LinearGradient, Mask, Paint, PathBuilder,
    PathStroker, PixmapPaint, RadialGradient, Shader, SpreadMode, Stroke, Transform,
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
    /// The whole surface in device pixels — larger than `base` when banded.
    surface: IRect,
    /// Device-space position of `base`'s top-left pixel (`(0, 0)` unless
    /// banded).
    origin: (i32, i32),
    /// `base` is one band of a taller surface (see [`Painter::banded`]).
    banded: bool,
    /// Entries at the bottom of `clip_stack` that bound drawing without a
    /// mask: a band's own rows. A mask changes how tiny-skia rounds its
    /// blending, and stray ink outside the band lands in rows that are never
    /// copied out, so the band clips by rect (text, damage, culling) only.
    unmasked_clips: usize,
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
            origin: (0, 0),
            banded: false,
            unmasked_clips: 0,
            layers: Vec::new(),
            clip_stack: Vec::new(),
            clip_mask: None,
        }
    }

    /// A painter over one band of a larger surface: `base` holds the device
    /// pixels starting at `origin` of a `surface`-sized screen. Drawing is
    /// clipped to `band` (device space, inside `base`'s extent) for text,
    /// damage and culling; shapes may spill into the rest of `base`, which
    /// the caller doesn't copy out.
    pub(crate) fn banded(
        base: &'a mut tiny_skia::Pixmap,
        damage: &'a mut DamageTracker,
        scale: Scale,
        surface: IRect,
        origin: (i32, i32),
        band: IRect,
    ) -> Self {
        let banded = base.height() < surface.h || base.width() < surface.w;
        let mut p = Painter {
            base,
            damage,
            scale,
            surface,
            origin,
            banded,
            unmasked_clips: 1,
            layers: Vec::new(),
            clip_stack: Vec::new(),
            clip_mask: None,
        };
        p.clip_stack.push(band.intersect(surface));
        p
    }

    /// Device-space position of the target buffer's top-left pixel.
    pub(crate) fn origin(&self) -> (i32, i32) {
        self.origin
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

    /// The target together with the active clip mask. Borrowing both at
    /// once (rather than cloning the mask, a full target-sized buffer, for
    /// every primitive) keeps a clipped paint at one mask's worth of memory.
    fn target_and_mask(&mut self) -> (&mut tiny_skia::Pixmap, Option<&Mask>) {
        let target = match self.layers.last_mut() {
            Some(layer) => &mut layer.pixmap,
            None => &mut *self.base,
        };
        (target, self.clip_mask.as_ref())
    }

    /// A rect fill's geometry and transform. tiny-skia only takes its exact
    /// rectangle rasterizer under an identity transform, so at 1× a band's
    /// offset is folded into the rect itself rather than the transform —
    /// otherwise a banded fill would go through the path rasterizer and
    /// anti-alias its edges differently from the whole-screen one.
    fn rect_geometry(&self, r: tiny_skia::Rect) -> Option<(tiny_skia::Rect, Transform)> {
        if self.origin == (0, 0) || self.scale.factor() != 1.0 {
            return Some((r, self.transform()));
        }
        let (ox, oy) = (self.origin.0 as f32, self.origin.1 as f32);
        let moved = tiny_skia::Rect::from_ltrb(
            r.left() - ox,
            r.top() - oy,
            r.right() - ox,
            r.bottom() - oy,
        )?;
        Some((moved, Transform::identity()))
    }

    /// The logical → target-pixel transform: the scale, then the band offset.
    fn transform(&self) -> Transform {
        let t = self.scale.transform();
        if self.origin == (0, 0) {
            t
        } else {
            t.post_translate(-self.origin.0 as f32, -self.origin.1 as f32)
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
        let Some((ts_rect, t)) = self.rect_geometry(ts_rect) else {
            return;
        };
        let (target, mask) = self.target_and_mask();
        target.fill_rect(ts_rect, &paint, t, mask);
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
        if self.banded {
            self.fill_path_banded(&path.0, &paint);
        } else {
            let t = self.transform();
            let (target, mask) = self.target_and_mask();
            target.fill_path(&path.0, &paint, FillRule::Winding, t, mask);
        }
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
        let hairline = width * self.scale.factor() <= 1.0;
        if self.banded && !hairline {
            // What tiny-skia's `stroke_path` does for a thick stroke — outline
            // at the transform's resolution, then fill — but through the band
            // route below.
            let res = PathStroker::compute_resolution_scale(&self.scale.transform());
            if let Some(outline) = path.0.stroke(&stroke, res) {
                self.fill_path_banded(&outline, &paint);
            }
        } else if self.banded {
            self.stroke_hairline_banded(&path.0, color, width);
        } else {
            let t = self.transform();
            let (target, mask) = self.target_and_mask();
            target.stroke_path(&path.0, &paint, &stroke, t, mask);
        }
        // Grow damage by half the stroke width on each side.
        self.damage_logical(path.bounds().inset(-(width / 2.0 + 1.0)));
    }

    /// A hairline (a stroke at most one device pixel wide) on a banded
    /// target. tiny-skia draws these with a dedicated scan converter that
    /// clips each line to its target, so the band would change them; instead
    /// the converter (vendored in [`crate::hairline`]) runs against the whole
    /// screen and a blitter keeps the band's rows. The setup mirrors
    /// tiny-skia's `stroke_path` hairline branch step for step.
    fn stroke_hairline_banded(&mut self, path: &tiny_skia::Path, color: Color, width: f32) {
        let mut c = color.to_tiny();
        // `treat_as_hairline`: a sub-pixel width is a full hairline at
        // reduced opacity, quantized the way tiny-skia does it.
        let coverage = if width == 0.0 {
            1.0
        } else {
            let len = width * self.scale.factor();
            (len + len) * 0.5
        };
        if coverage != 1.0 {
            let scale = (coverage * 256.0) as i32;
            let new_alpha = (255 * scale) >> 8;
            c.apply_opacity(new_alpha as f32 / 255.0);
        }
        let t = self.scale.transform();
        let dev = if t.is_identity() {
            path.clone()
        } else {
            match path.clone().transform(t) {
                Some(p) => p,
                None => return,
            }
        };
        let masked = self.clip_stack.len() > self.unmasked_clips;
        let write = self.clip();
        let (sw, sh) = (self.surface.w, self.surface.h);
        let origin = self.origin;
        let mut blitter =
            crate::hairline::BandBlitter::new(self.target(), origin, write, c, masked);
        crate::hairline::stroke(&dev, tiny_skia::LineCap::Butt, sw, sh, &mut blitter);
    }

    /// Fill a logical-space path on a banded target so the pixels come out as
    /// they would on the whole screen.
    ///
    /// tiny-skia rasterizes a shape that sticks out of the target by clipping
    /// its edges to the target first, which nudges anti-aliasing along every
    /// clipped edge. A band clips almost every shape the whole screen
    /// wouldn't, so a path that leaves the band takes a detour: its coverage
    /// is rasterized into a mask over its own bounds — where it is clipped by
    /// nothing the whole screen doesn't clip it by — and the band's rows of
    /// that coverage are blended in. A path inside the band (and the screen)
    /// is drawn directly, merely shifted.
    fn fill_path_banded(&mut self, path: &tiny_skia::Path, paint: &Paint) {
        let Some(dev) = path.clone().transform(self.scale.transform()) else {
            return;
        };
        let b = dev.bounds();
        let (l, t, r, bt) = (
            b.left().floor() as i32,
            b.top().floor() as i32,
            b.right().ceil() as i32,
            b.bottom().ceil() as i32,
        );
        let (ox, oy) = self.origin;
        let (tw, th) = (self.base.width() as i32, self.base.height() as i32);
        let (sw, sh) = (self.surface.w as i32, self.surface.h as i32);
        let inside_target = l >= ox && t >= oy && r <= ox + tw && bt <= oy + th;
        if inside_target {
            let shift = Transform::from_translate(-ox as f32, -oy as f32);
            let (target, mask) = self.target_and_mask();
            target.fill_path(&dev, paint, FillRule::Winding, shift, mask);
            return;
        }
        // What of the shape this band shows: its bounds ∩ band ∩ clip.
        let band = IRect::new(ox, oy, tw as u32, th as u32);
        let show = IRect::new(l, t, (r - l).max(0) as u32, (bt - t).max(0) as u32)
            .intersect(band)
            .intersect(self.clip());
        if show.is_empty() {
            return;
        }
        // The coverage mask. Clipped by the screen (it pokes out), it must be
        // rasterized against the screen's own edges with no offset at all —
        // tiny-skia's edge clipping isn't translation-invariant — so it spans
        // from the origin; otherwise it covers just the shape's bounds.
        let clipped = l < 0 || t < 0 || r > sw || bt > sh;
        let (mx, my) = if clipped { (0, 0) } else { (l, t) };
        let (mw, mh) = (r.min(sw) - mx, bt.min(sh) - my);
        if mw <= 0 || mh <= 0 {
            return;
        }
        let Some(mut coverage) = Mask::new(mw as u32, mh as u32) else {
            return;
        };
        let to_mask = if clipped {
            Transform::identity()
        } else {
            Transform::from_translate(-mx as f32, -my as f32)
        };
        coverage.fill_path(&dev, FillRule::Winding, true, to_mask);
        // An opaque colour with no clip mask is one tiny-skia blends
        // differently: it strength-reduces source-over to a plain lerp by
        // coverage (and a memset where coverage is full), which rounds unlike
        // its masked path. Apply that lerp here, exactly as it does.
        let opaque = match &paint.shader {
            Shader::SolidColor(c) => c.is_opaque(),
            _ => false,
        };
        if opaque && self.clip_stack.len() <= self.unmasked_clips {
            let Shader::SolidColor(c) = &paint.shader else {
                return;
            };
            let src = c.premultiply().to_color_u8();
            let src = [src.red(), src.green(), src.blue(), src.alpha()];
            let cov = coverage.data();
            let target = self.target();
            let px = target.pixels_mut();
            for y in show.y..show.bottom() {
                for x in show.x..show.right() {
                    let a = cov[(y - my) as usize * mw as usize + (x - mx) as usize];
                    let d = &mut px[(y - oy) as usize * tw as usize + (x - ox) as usize];
                    *d = match a {
                        0 => continue,
                        255 => tiny_skia::PremultipliedColorU8::from_rgba(
                            src[0], src[1], src[2], src[3],
                        )
                        .unwrap_or(*d),
                        a => {
                            // lowp `Lerp1Float`: coverage through f32 the way
                            // tiny-skia carries it, then div255.
                            let t = ((a as f32 * (1.0 / 255.0)) * 255.0 + 0.5) as u16;
                            let lerp = |from: u8, to: u8| {
                                ((from as u16 * (255 - t) + to as u16 * t + 255) >> 8) as u8
                            };
                            tiny_skia::PremultipliedColorU8::from_rgba(
                                lerp(d.red(), src[0]),
                                lerp(d.green(), src[1]),
                                lerp(d.blue(), src[2]),
                                lerp(d.alpha(), src[3]),
                            )
                            .unwrap_or(*d)
                        }
                    };
                }
            }
            return;
        }
        // Re-home the visible rows into a band-sized mask (zero outside
        // `show`, which also applies the clip) and blend through it.
        let Some(mut band_mask) = Mask::new(tw as u32, th as u32) else {
            return;
        };
        {
            let src = coverage.data();
            let dst = band_mask.data_mut();
            let cols = show.w as usize;
            for y in show.y..show.bottom() {
                let s = (y - my) as usize * mw as usize + (show.x - mx) as usize;
                let d = (y - oy) as usize * tw as usize + (show.x - ox) as usize;
                dst[d..d + cols].copy_from_slice(&src[s..s + cols]);
            }
        }
        drop(coverage);
        let Some(rect) = tiny_skia::Rect::from_xywh(
            (show.x - ox) as f32,
            (show.y - oy) as f32,
            show.w as f32,
            show.h as f32,
        ) else {
            return;
        };
        self.target()
            .fill_rect(rect, paint, Transform::identity(), Some(&band_mask));
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
        let Some((moved, t)) = self.rect_geometry(ts_rect) else {
            return;
        };
        if t.is_identity() && moved != ts_rect {
            // The gradient was defined against the unmoved rect.
            paint.shader.transform(Transform::from_translate(
                moved.left() - ts_rect.left(),
                moved.top() - ts_rect.top(),
            ));
        }
        let (target, mask) = self.target_and_mask();
        target.fill_rect(moved, &paint, t, mask);
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
        let (ox, oy) = self.origin;
        let (target, mask) = self.target_and_mask();
        target.draw_pixmap(
            dx - ox,
            dy - oy,
            image.pixmap.as_ref(),
            &paint,
            Transform::identity(),
            mask,
        );
        self.add_damage(IRect::new(dx, dy, image.width(), image.height()));
    }

    /// Blit a decoded image **scaled** to fill the logical rect `dest`
    /// (bilinear; 1:1 falls back to nearest). This is the object-fit
    /// primitive a video frame or a fitted photo rides — the caller picks the
    /// dest rect (contain/cover/stretch math) and clips as needed.
    pub fn draw_image_scaled(&mut self, image: &Image, dest: Rect) {
        let dev = self.scale.to_device_rect(dest);
        if dev.is_empty() || image.width() == 0 || image.height() == 0 {
            return;
        }
        let sx = dev.w as f32 / image.width() as f32;
        let sy = dev.h as f32 / image.height() as f32;
        let one_to_one = (sx - 1.0).abs() < f32::EPSILON && (sy - 1.0).abs() < f32::EPSILON;
        let paint = PixmapPaint {
            opacity: 1.0,
            blend_mode: BlendMode::SourceOver,
            quality: if one_to_one {
                FilterQuality::Nearest
            } else {
                FilterQuality::Bilinear
            },
        };
        let (ox, oy) = self.origin;
        let t = Transform::from_row(sx, 0.0, 0.0, sy, (dev.x - ox) as f32, (dev.y - oy) as f32);
        let (target, mask) = self.target_and_mask();
        target.draw_pixmap(0, 0, image.pixmap.as_ref(), &paint, t, mask);
        self.add_damage(dev);
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
        // The mask covers the buffer actually drawn into (a band, when
        // banded), in that buffer's pixel space.
        let (w, h) = (self.base.width().max(1), self.base.height().max(1));
        let (ox, oy) = self.origin;
        // Free the old mask before allocating its replacement.
        self.clip_mask = None;
        if self.clip_stack.len() <= self.unmasked_clips {
            return;
        }
        self.clip_mask = match self.clip_stack.last().copied() {
            // No explicit clip -> no mask (draw to the whole surface).
            None => None,
            // Fully clipped out: an all-zero mask discards every pixel.
            Some(c) if c.is_empty() => Mask::new(w, h),
            Some(c) => Self::rect_mask(w, h, IRect::new(c.x - ox, c.y - oy, c.w, c.h)),
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
        let pixmap = tiny_skia::Pixmap::new(self.base.width().max(1), self.base.height().max(1))
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
        let (target, mask) = self.target_and_mask();
        target.draw_pixmap(
            0,
            0,
            layer.pixmap.as_ref(),
            &paint,
            Transform::identity(),
            mask,
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
