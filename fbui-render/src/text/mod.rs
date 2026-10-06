//! Text: shaping, layout, and rasterization via cosmic-text + swash.
//!
//! cosmic-text does the hard parts — Unicode segmentation, bidi reordering,
//! font fallback, and HarfBuzz-grade shaping — so CJK and right-to-left scripts
//! "just work" given fonts that cover them. We own only the glue: shape a string
//! into a [`TextLayout`], then composite each glyph's coverage (cached in the
//! bounded `GlyphAtlas`) into the painter's shadow buffer with source-over
//! alpha blending.
//!
//! HiDPI is handled at rasterization time: glyphs are rendered at
//! `size × scale` device pixels via cosmic-text's `physical(_, scale)`, so text
//! stays crisp at 2× instead of being a scaled-up 1× bitmap.
//!
//! That outline path is the `outline-text` feature (on by default). The other
//! backend is [bitmap fonts](bitmap): glyphs pre-rasterized at fixed sizes,
//! read in place from flash, laid out with no shaping engine — for a small
//! device that wants text without cosmic-text's heap and code. A
//! [`FontContext`] holding bitmap fonts uses them; the widget code above is
//! the same either way.

#[allow(unused_imports)]
use crate::prelude::*;

#[cfg(feature = "outline-text")]
mod atlas;
pub mod bitmap;

#[cfg(feature = "outline-text")]
use cosmic_text::{Attrs, Buffer, Cursor, Family, FontSystem, Metrics, Shaping, Style, Weight};

use crate::color::Color;
use crate::geom::{IRect, Point, Rect, Size};
use crate::painter::Painter;
#[cfg(feature = "outline-text")]
use atlas::GlyphAtlas;
use bitmap::BitmapLayout;
pub use bitmap::{BitmapFont, BitmapFontError, BitmapFontWriter};

/// Which font to shape with. `Name` picks a specific family; the others are the
/// generic CSS-style buckets resolved against the font database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FontFamily {
    SansSerif,
    Serif,
    Monospace,
    Name(String),
}

/// How a run of text should look.
#[derive(Debug, Clone)]
pub struct TextStyle {
    /// Font size in logical pixels.
    pub size: f32,
    /// Baseline-to-baseline distance in logical pixels.
    pub line_height: f32,
    pub color: Color,
    pub family: FontFamily,
    pub bold: bool,
    pub italic: bool,
}

impl TextStyle {
    /// A sane sans-serif body style at the given size (line height 1.25×).
    pub fn new(size: f32, color: Color) -> Self {
        TextStyle {
            size,
            line_height: size * 1.25,
            color,
            family: FontFamily::SansSerif,
            bold: false,
            italic: false,
        }
    }

    pub fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    pub fn italic(mut self) -> Self {
        self.italic = true;
        self
    }

    pub fn family(mut self, family: FontFamily) -> Self {
        self.family = family;
        self
    }

    #[cfg(feature = "outline-text")]
    fn attrs(&self) -> Attrs<'_> {
        let family = match &self.family {
            FontFamily::SansSerif => Family::SansSerif,
            FontFamily::Serif => Family::Serif,
            FontFamily::Monospace => Family::Monospace,
            FontFamily::Name(n) => Family::Name(n),
        };
        Attrs::new()
            .family(family)
            .weight(if self.bold {
                Weight::BOLD
            } else {
                Weight::NORMAL
            })
            .style(if self.italic {
                Style::Italic
            } else {
                Style::Normal
            })
    }
}

/// A shaped, laid-out paragraph ready to draw. The (expensive) layout is done
/// once and reused across repaints.
pub struct TextLayout {
    repr: Repr,
}

enum Repr {
    #[cfg(feature = "outline-text")]
    Outline(OutlineLayout),
    Bitmap(BitmapLayout),
    /// No font to lay out with: nothing, one empty line.
    Empty {
        line_height: f32,
    },
}

impl TextLayout {
    /// The measured logical size of the laid-out text (width of the widest line,
    /// total height of all lines).
    pub fn size(&self) -> Size {
        match &self.repr {
            #[cfg(feature = "outline-text")]
            Repr::Outline(l) => l.size(),
            Repr::Bitmap(l) => l.size(),
            Repr::Empty { .. } => Size::new(0.0, 0.0),
        }
    }

    /// The logical height of one line (font size × line-height factor), the
    /// vertical step between wrapped or explicit lines.
    pub fn line_height(&self) -> f32 {
        match &self.repr {
            #[cfg(feature = "outline-text")]
            Repr::Outline(l) => l.line_height(),
            Repr::Bitmap(l) => l.line_height(),
            Repr::Empty { line_height } => *line_height,
        }
    }

    /// Number of *visual* lines after wrapping (at least 1, even for empty text).
    pub fn line_count(&self) -> usize {
        match &self.repr {
            #[cfg(feature = "outline-text")]
            Repr::Outline(l) => l.line_count(),
            Repr::Bitmap(l) => l.line_count(),
            Repr::Empty { .. } => 1,
        }
    }

    /// The byte offset (a char boundary in the source text) nearest to logical
    /// point (`x`, `y`) measured from the layout's top-left. Points above the
    /// first line map to its start, below the last line to its end, and
    /// beyond a line's ends to that line's ends — what a click or drag into
    /// text should resolve to.
    pub fn hit(&self, x: f32, y: f32) -> usize {
        match &self.repr {
            #[cfg(feature = "outline-text")]
            Repr::Outline(l) => l.hit(x, y),
            Repr::Bitmap(l) => l.hit(x, y),
            Repr::Empty { .. } => 0,
        }
    }

    /// The caret box for the boundary before byte `idx`: zero-width, at the
    /// glyph edge, spanning the line's height. Falls back to the start of the
    /// first line (or the line's end for an offset past the text) so a caret
    /// always has somewhere to draw — including in empty text.
    pub fn caret(&self, idx: usize) -> Rect {
        match &self.repr {
            #[cfg(feature = "outline-text")]
            Repr::Outline(l) => l.caret(idx),
            Repr::Bitmap(l) => l.caret(idx),
            Repr::Empty { line_height } => Rect::new(0.0, 0.0, 0.0, *line_height),
        }
    }

    /// Highlight boxes covering the source byte range `a..b` (either order),
    /// one per visual line touched — plus a thin marker at a line's end when
    /// the selection continues onto the next line, so a selected line break
    /// is visible. Empty when `a == b`.
    pub fn selection_rects(&self, a: usize, b: usize) -> Vec<Rect> {
        match &self.repr {
            #[cfg(feature = "outline-text")]
            Repr::Outline(l) => l.selection_rects(a, b),
            Repr::Bitmap(l) => l.selection_rects(a, b),
            Repr::Empty { .. } => Vec::new(),
        }
    }
}

/// A cosmic-text layout: the shaped buffer and its measured size.
#[cfg(feature = "outline-text")]
struct OutlineLayout {
    buffer: Buffer,
    measured: Size,
}

#[cfg(feature = "outline-text")]
impl OutlineLayout {
    /// The measured logical size of the laid-out text (width of the widest line,
    /// total height of all lines).
    pub fn size(&self) -> Size {
        self.measured
    }

    /// The logical height of one line (font size × line-height factor), the
    /// vertical step between wrapped or explicit lines.
    pub fn line_height(&self) -> f32 {
        self.buffer.metrics().line_height
    }

    /// Number of *visual* lines after wrapping (at least 1, even for empty text).
    pub fn line_count(&self) -> usize {
        self.buffer.layout_runs().count().max(1)
    }

    /// Byte offset in the source text of the start of paragraph `line`
    /// (cosmic-text splits the text on line endings; each piece is a
    /// paragraph that may wrap into several visual lines).
    fn paragraph_start(&self, line: usize) -> usize {
        self.buffer
            .lines
            .iter()
            .take(line)
            .map(|l| l.text().len() + l.ending().as_str().len())
            .sum()
    }

    /// The source text's total byte length as the buffer sees it.
    fn text_len(&self) -> usize {
        self.paragraph_start(self.buffer.lines.len())
    }

    /// Split a source byte offset into (paragraph, byte offset within it).
    fn cursor_at(&self, idx: usize) -> Cursor {
        let mut start = 0usize;
        for (i, l) in self.buffer.lines.iter().enumerate() {
            let len = l.text().len();
            if idx <= start + len {
                return Cursor::new(i, idx - start);
            }
            start += len + l.ending().as_str().len();
        }
        let last = self.buffer.lines.len().saturating_sub(1);
        let last_len = self.buffer.lines.last().map_or(0, |l| l.text().len());
        Cursor::new(last, last_len)
    }

    /// The byte offset (a char boundary in the source text) nearest to logical
    /// point (`x`, `y`) measured from the layout's top-left. Points above the
    /// first line map to its start, below the last line to its end, and
    /// beyond a line's ends to that line's ends — what a click or drag into
    /// text should resolve to.
    pub fn hit(&self, x: f32, y: f32) -> usize {
        match self.buffer.hit(x, y) {
            Some(c) => (self.paragraph_start(c.line) + c.index).min(self.text_len()),
            None => 0,
        }
    }

    /// The caret box for the boundary before byte `idx`: zero-width, at the
    /// glyph edge, spanning the line's height. Falls back to the start of the
    /// first line (or the line's end for an offset past the text) so a caret
    /// always has somewhere to draw — including in empty text.
    pub fn caret(&self, idx: usize) -> Rect {
        let idx = idx.min(self.text_len());
        let cursor = self.cursor_at(idx);
        let lh = self.line_height();
        // A wrapped paragraph has several runs with the same `line_i`; the
        // boundary at a wrap point belongs to the *later* run (that's where
        // typing continues), so prefer the last run that claims the cursor —
        // except at index 0 of the paragraph, which is the first run's start.
        let mut best: Option<Rect> = None;
        for run in self.buffer.layout_runs() {
            if run.line_i != cursor.line {
                continue;
            }
            let run_start = run.glyphs.iter().map(|g| g.start).min().unwrap_or(0);
            let run_end = run.glyphs.iter().map(|g| g.end).max().unwrap_or(0);
            let claims =
                run.glyphs.is_empty() || (cursor.index >= run_start && cursor.index <= run_end);
            if !claims {
                continue;
            }
            if let Some(x) = run.cursor_position(&cursor) {
                let r = Rect::new(x, run.line_top, 0.0, run.line_height.max(lh));
                let at_wrap_start = cursor.index == run_start && cursor.index != 0;
                if best.is_none() || at_wrap_start || cursor.index == run_end {
                    best = Some(r);
                }
            }
        }
        best.unwrap_or_else(|| {
            // No run for this paragraph (empty text, or a font-less layout):
            // stack empty paragraphs by line height.
            Rect::new(0.0, cursor.line as f32 * lh, 0.0, lh)
        })
    }

    /// Highlight boxes covering the source byte range `a..b` (either order),
    /// one per visual line touched — plus a thin marker at a line's end when
    /// the selection continues onto the next line, so a selected line break
    /// is visible. Empty when `a == b`.
    pub fn selection_rects(&self, a: usize, b: usize) -> Vec<Rect> {
        let (a, b) = (a.min(b), a.max(b));
        let len = self.text_len();
        let (a, b) = (a.min(len), b.min(len));
        if a == b {
            return Vec::new();
        }
        let (ca, cb) = (self.cursor_at(a), self.cursor_at(b));
        let mut out = Vec::new();
        for run in self.buffer.layout_runs() {
            if run.line_i < ca.line || run.line_i > cb.line {
                continue;
            }
            let mut any = false;
            for (x, w) in run.highlight(ca, cb) {
                any = true;
                out.push(Rect::new(x, run.line_top, w, run.line_height));
            }
            // A run wholly inside the selection but with nothing highlighted
            // (an empty line, or a wrap boundary) still shows as selected.
            let run_end = run.glyphs.iter().map(|g| g.end).max().unwrap_or(0);
            let continues = run.line_i < cb.line
                || (run.line_i == cb.line && cb.index > run_end && !run.glyphs.is_empty());
            if !any && run.line_i > ca.line && continues {
                out.push(Rect::new(0.0, run.line_top, 0.0, run.line_height));
            }
            if continues && run.line_i < cb.line {
                // Mark the selected line ending with a small tail.
                let x = out
                    .last()
                    .filter(|r| (r.y - run.line_top).abs() < 0.01)
                    .map(|r| r.right())
                    .unwrap_or(0.0);
                out.push(Rect::new(
                    x,
                    run.line_top,
                    run.line_height * 0.3,
                    run.line_height,
                ));
            }
        }
        out
    }
}

/// Embedded default font (Inter Regular, SIL Open Font License), compiled in
/// only under the `bundled-font` feature. Lets a target render text with no
/// host fonts and no asset files — see [`FontContext::with_default_font`]. The
/// license travels with it in `fbui-render/fonts/Inter-LICENSE.txt`.
#[cfg(feature = "bundled-font")]
pub const DEFAULT_FONT: &[u8] = include_bytes!("../../fonts/Inter-Regular.ttf");

/// Owns the font database and glyph cache. One per application (or per thread).
///
/// Construction does **not** scan the host's installed fonts: [`new`] starts
/// from an empty database, so on a minimal target (a boot image, a kiosk) text
/// renders only from fonts you load — deterministic and host-independent, which
/// is what an embedded/ISO target wants. Use `with_fonts` /
/// `with_static_fonts` to start from a bundled set (outline text), or
/// `with_default_font` (behind the `bundled-font` feature) for the
/// compiled-in default; or [`with_bitmap_fonts`] for pre-rasterized fonts
/// with no shaping engine.
///
/// [`new`]: FontContext::new
/// [`with_bitmap_fonts`]: FontContext::with_bitmap_fonts
pub struct FontContext {
    #[cfg(feature = "outline-text")]
    font_system: FontSystem,
    #[cfg(feature = "outline-text")]
    atlas: GlyphAtlas,
    /// Bitmap fonts; when there are any, they set all text.
    bitmaps: Vec<BitmapFont>,
    /// Device pixels per logical pixel, for choosing a bitmap size.
    scale: f32,
}

impl Default for FontContext {
    fn default() -> Self {
        Self::new()
    }
}

impl FontContext {
    /// Build a context with an empty font database. Load fonts with
    /// `load_font_data` / `add_bitmap_font` before drawing, or prefer
    /// `with_fonts` / [`with_bitmap_fonts`](Self::with_bitmap_fonts) to
    /// supply them up front.
    pub fn new() -> Self {
        FontContext {
            #[cfg(feature = "outline-text")]
            font_system: FontSystem::new(),
            #[cfg(feature = "outline-text")]
            atlas: GlyphAtlas::new(),
            bitmaps: Vec::new(),
            scale: 1.0,
        }
    }

    /// A context that sets all text in `fonts` — pre-rasterized
    /// [bitmap fonts](bitmap), read in place. No shaping engine or
    /// rasterizer runs, and no font data is copied: the cheapest way to have
    /// text on a small device, at the cost of fixed sizes and no shaping.
    /// The outline database starts empty and is never touched.
    pub fn with_bitmap_fonts(fonts: impl IntoIterator<Item = BitmapFont>) -> Self {
        let mut fc = FontContext {
            #[cfg(feature = "outline-text")]
            font_system: FontSystem::new_with_locale_and_db(
                "en-US".into(),
                cosmic_text::fontdb::Database::new(),
            ),
            #[cfg(feature = "outline-text")]
            atlas: GlyphAtlas::new(),
            bitmaps: Vec::new(),
            scale: 1.0,
        };
        fc.bitmaps.extend(fonts);
        fc
    }

    /// The bundled bitmap fonts: Inter Regular at 12, 16, 20 and 24 px,
    /// Latin-1 (behind the `bundled-bitmap-font` feature). See
    /// [`with_bitmap_fonts`](Self::with_bitmap_fonts).
    #[cfg(feature = "bundled-bitmap-font")]
    pub fn with_default_bitmap_fonts() -> Self {
        Self::with_bitmap_fonts(bitmap::bundled())
    }

    /// Add a bitmap font. Once a context holds any, bitmap fonts set all of
    /// its text.
    pub fn add_bitmap_font(&mut self, font: BitmapFont) {
        self.bitmaps.push(font);
    }

    /// Whether text is set in bitmap fonts.
    pub fn uses_bitmap_fonts(&self) -> bool {
        !self.bitmaps.is_empty()
    }

    /// The device scale text is drawn at. Bitmap fonts pick the size nearest
    /// `style.size × scale` device pixels when laying out, so the owner of
    /// the context (the `Ui`) keeps this in step with its surface. The
    /// outline path rasterizes at draw time and ignores it.
    pub fn set_scale(&mut self, scale: crate::Scale) {
        self.scale = scale.factor();
    }

    #[cfg(feature = "outline-text")]
    /// Build a context from a fixed set of in-memory fonts (TTF/OTF), with **no**
    /// host-font dependence — rendering is reproducible across machines, the
    /// property a boot image or kiosk needs.
    ///
    /// The first loaded face is installed as the default for every generic family
    /// (sans-serif/serif/monospace), so a default [`TextStyle`] resolves to it
    /// without the caller naming a family. Supply whatever script coverage you
    /// need: on a minimal target there is no fallback to a system font.
    pub fn with_fonts(fonts: impl IntoIterator<Item = Vec<u8>>) -> Self {
        let mut db = cosmic_text::fontdb::Database::new();
        for data in fonts {
            db.load_font_data(data);
        }
        Self::from_db(db)
    }

    #[cfg(feature = "outline-text")]
    /// As [`with_fonts`](Self::with_fonts), for font data that lives for the
    /// whole program — `include_bytes!` in flash, say. The bytes are used in
    /// place: nothing is copied into the heap, which on a small target saves
    /// the size of every font (Inter alone is ~300 KiB).
    pub fn with_static_fonts(fonts: impl IntoIterator<Item = &'static [u8]>) -> Self {
        let mut db = cosmic_text::fontdb::Database::new();
        for data in fonts {
            db.load_font_source(cosmic_text::fontdb::Source::Binary(alloc::sync::Arc::new(
                data,
            )));
        }
        Self::from_db(db)
    }

    #[cfg(feature = "outline-text")]
    /// Install the first face as every generic family's default and build
    /// the context around `db`.
    fn from_db(mut db: cosmic_text::fontdb::Database) -> Self {
        // Point the generic families at the first loaded face so `Family::SansSerif`
        // (the `TextStyle` default) matches it; otherwise cosmic-text looks for its
        // built-in default names ("Open Sans", …) which an empty db never has.
        // Bind in its own scope so the `faces()` borrow ends before the mutations.
        let default_family = db
            .faces()
            .next()
            .and_then(|f| f.families.first())
            .map(|(name, _)| name.clone());
        if let Some(name) = default_family {
            db.set_sans_serif_family(name.clone());
            db.set_serif_family(name.clone());
            db.set_monospace_family(name);
        }
        FontContext {
            // A fixed locale keeps shaping deterministic; the loaded fonts, not the
            // host, decide coverage.
            font_system: FontSystem::new_with_locale_and_db("en-US".to_string(), db),
            atlas: GlyphAtlas::new(),
            bitmaps: Vec::new(),
            scale: 1.0,
        }
    }

    /// Build a context from the compiled-in [`DEFAULT_FONT`] (Inter Regular).
    /// Available under the `bundled-font` feature — a turnkey path to legible
    /// text on a target with no fonts of its own. Override by supplying your own
    /// via [`with_fonts`](Self::with_fonts).
    #[cfg(feature = "bundled-font")]
    pub fn with_default_font() -> Self {
        Self::with_static_fonts([DEFAULT_FONT])
    }

    #[cfg(feature = "outline-text")]
    /// Add a font from in-memory bytes (TTF/OTF). Useful for bundling a fixed
    /// font so rendering is reproducible regardless of the host's installed set.
    pub fn load_font_data(&mut self, data: Vec<u8>) {
        self.font_system.db_mut().load_font_data(data);
    }

    #[cfg(feature = "outline-text")]
    /// Add a font whose bytes live for the whole program, used in place (see
    /// [`with_static_fonts`](Self::with_static_fonts)).
    pub fn load_static_font(&mut self, data: &'static [u8]) {
        self.font_system
            .db_mut()
            .load_font_source(cosmic_text::fontdb::Source::Binary(alloc::sync::Arc::new(
                data,
            )));
    }

    /// Shape and lay out `text` in `style`, wrapping at `max_width` logical
    /// pixels (or unbounded if `None`).
    pub fn layout(&mut self, text: &str, style: &TextStyle, max_width: Option<f32>) -> TextLayout {
        if let Some(font) = bitmap::select(&self.bitmaps, style, self.scale) {
            let l = BitmapLayout::new(font, text, style, max_width, self.scale);
            return TextLayout {
                repr: Repr::Bitmap(l),
            };
        }
        #[cfg(feature = "outline-text")]
        {
            TextLayout {
                repr: self.layout_outline(text, style, max_width),
            }
        }
        #[cfg(not(feature = "outline-text"))]
        {
            let _ = (text, max_width);
            TextLayout {
                repr: Repr::Empty {
                    line_height: style.line_height,
                },
            }
        }
    }

    #[cfg(feature = "outline-text")]
    fn layout_outline(&mut self, text: &str, style: &TextStyle, max_width: Option<f32>) -> Repr {
        let metrics = Metrics::new(style.size, style.line_height);
        // cosmic-text panics shaping with an empty font database. A hosted
        // build almost never sees one, but a bare-metal target that forgot to
        // load a font must not crash: lay out nothing (zero-size) instead.
        if self.font_system.db().is_empty() {
            return Repr::Empty {
                line_height: style.line_height,
            };
        }
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        buffer.set_size(max_width, None);
        buffer.set_text(text, &style.attrs(), Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.font_system, false);

        // Measure: widest run, and the bottom of the last line.
        let mut width = 0.0f32;
        let mut height = 0.0f32;
        for run in buffer.layout_runs() {
            width = width.max(run.line_w);
            height = height.max(run.line_top + run.line_height);
        }
        Repr::Outline(OutlineLayout {
            buffer,
            measured: Size::new(width, height),
        })
    }

    /// Convenience: shape and draw in one call.
    pub fn draw_text(
        &mut self,
        painter: &mut Painter,
        text: &str,
        style: &TextStyle,
        at: Point,
        max_width: Option<f32>,
    ) {
        let layout = self.layout(text, style, max_width);
        self.draw(painter, &layout, style.color, at);
    }

    /// Composite an already-shaped [`TextLayout`] into the painter with its
    /// top-left at logical point `at`, in `color`.
    pub fn draw(&mut self, painter: &mut Painter, layout: &TextLayout, color: Color, at: Point) {
        match &layout.repr {
            #[cfg(feature = "outline-text")]
            Repr::Outline(l) => self.draw_outline(painter, l, color, at),
            Repr::Bitmap(l) => draw_bitmap(painter, l, color, at),
            Repr::Empty { .. } => {}
        }
    }

    #[cfg(feature = "outline-text")]
    fn draw_outline(
        &mut self,
        painter: &mut Painter,
        layout: &OutlineLayout,
        color: Color,
        at: Point,
    ) {
        let scale = painter.scale().factor();
        let clip = painter.clip();
        let base_x = (at.x * scale).round() as i32;
        let base_y = (at.y * scale).round() as i32;
        // Glyphs are composited by hand, so apply a band's offset here: the
        // clip and damage stay in surface space, pixel writes go to the band.
        let (ox, oy) = painter.origin();
        let target_clip = IRect::new(clip.x - ox, clip.y - oy, clip.w, clip.h);
        let target = painter.target();
        let (tw, th) = (target.width() as i32, target.height() as i32);

        let mut dirty = IRect::EMPTY;

        for run in layout.buffer.layout_runs() {
            let line_y = (run.line_y * scale).round() as i32;
            for glyph in run.glyphs.iter() {
                let physical = glyph.physical((0.0, 0.0), scale);
                // Per-glyph color (rich text) overrides the run color if present.
                let gc = glyph
                    .color_opt
                    .map(|c| Color::rgba(c.r(), c.g(), c.b(), c.a()))
                    .unwrap_or(color);

                let Some(raster) = self.atlas.get(&mut self.font_system, physical.cache_key) else {
                    continue;
                };
                let gx = base_x + physical.x + raster.left;
                let gy = base_y + line_y + physical.y - raster.top;

                let (rw, rcolor, data) = (raster.width as i32, raster.color, &raster.data);
                composite(
                    target.pixels_mut(),
                    (tw, th),
                    (raster.width, raster.height),
                    (gx - ox, gy - oy),
                    gc,
                    target_clip,
                    |col, row| {
                        if rcolor {
                            // Emoji: straight RGBA source.
                            let o = ((row * rw + col) * 4) as usize;
                            Sample::Rgba(data[o], data[o + 1], data[o + 2], data[o + 3])
                        } else {
                            Sample::Coverage(data[(row * rw + col) as usize])
                        }
                    },
                );
                dirty = dirty.union(IRect::new(gx, gy, raster.width, raster.height));
            }
        }

        painter.add_damage(dirty);
    }
}

/// Draw a bitmap-font layout with its top-left at logical `at`.
fn draw_bitmap(painter: &mut Painter, layout: &BitmapLayout, color: Color, at: Point) {
    let scale = painter.scale().factor();
    let clip = painter.clip();
    let base_x = (at.x * scale).round() as i32;
    let base_y = (at.y * scale).round() as i32;
    let (ox, oy) = painter.origin();
    let target_clip = IRect::new(clip.x - ox, clip.y - oy, clip.w, clip.h);
    let target = painter.target();
    let (tw, th) = (target.width() as i32, target.height() as i32);
    let mut dirty = IRect::EMPTY;
    for (glyph, pen_x, baseline) in layout.glyphs() {
        // Placed like the outline path: pen and baseline rounded to device
        // pixels, then the bitmap's own offsets.
        let gx = base_x + pen_x.round() as i32 + glyph.left;
        let gy = base_y + baseline.round() as i32 - glyph.top;
        composite(
            target.pixels_mut(),
            (tw, th),
            (glyph.width, glyph.height),
            (gx - ox, gy - oy),
            color,
            target_clip,
            |col, row| Sample::Coverage(glyph.coverage(col as u32, row as u32)),
        );
        dirty = dirty.union(IRect::new(gx, gy, glyph.width, glyph.height));
    }
    painter.add_damage(dirty);
}

/// One glyph pixel: coverage of the run colour, or a colour (emoji) pixel.
enum Sample {
    Coverage(u8),
    #[cfg_attr(not(feature = "outline-text"), allow(dead_code))]
    Rgba(u8, u8, u8, u8),
}

/// Blend one glyph of `size` at `at` (target pixels) into the target's
/// premultiplied pixels with source-over alpha, clipped to `clip` and the
/// buffer bounds. `sample(col, row)` reads the glyph.
fn composite(
    pixels: &mut [tiny_skia::PremultipliedColorU8],
    (tw, th): (i32, i32),
    (gw, gh): (u32, u32),
    (gx, gy): (i32, i32),
    color: Color,
    clip: IRect,
    sample: impl Fn(i32, i32) -> Sample,
) {
    for row in 0..gh as i32 {
        let py = gy + row;
        if py < 0 || py >= th || py < clip.y || py >= clip.bottom() {
            continue;
        }
        for col in 0..gw as i32 {
            let px = gx + col;
            if px < 0 || px >= tw || px < clip.x || px >= clip.right() {
                continue;
            }
            let idx = (py * tw + px) as usize;
            match sample(col, row) {
                Sample::Rgba(sr, sg, sb, sa) => {
                    let ea = mul255(sa, color.a);
                    blend(&mut pixels[idx], sr, sg, sb, ea);
                }
                Sample::Coverage(cov) => {
                    // Coverage mask modulated by the run color's alpha.
                    let ea = mul255(cov, color.a);
                    blend(&mut pixels[idx], color.r, color.g, color.b, ea);
                }
            }
        }
    }
}

/// `a * b / 255`, rounded.
#[inline]
fn mul255(a: u8, b: u8) -> u8 {
    let t = a as u32 * b as u32 + 128;
    (((t >> 8) + t) >> 8) as u8
}

/// Source-over a straight-alpha source `(sr,sg,sb)` with effective alpha `ea`
/// onto a premultiplied destination pixel.
#[inline]
fn blend(dst: &mut tiny_skia::PremultipliedColorU8, sr: u8, sg: u8, sb: u8, ea: u8) {
    if ea == 0 {
        return;
    }
    // Premultiplied source.
    let (spr, spg, spb) = (mul255(sr, ea), mul255(sg, ea), mul255(sb, ea));
    let inv = 255 - ea;
    let dr = mul255(dst.red(), inv) + spr;
    let dg = mul255(dst.green(), inv) + spg;
    let db = mul255(dst.blue(), inv) + spb;
    let da = mul255(dst.alpha(), inv) + ea;
    // out_channel <= out_alpha holds, so this premultiplied value is always valid.
    *dst = tiny_skia::PremultipliedColorU8::from_rgba(dr, dg, db, da)
        .unwrap_or_else(|| tiny_skia::PremultipliedColorU8::from_rgba(0, 0, 0, da).unwrap());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mul255_endpoints() {
        assert_eq!(mul255(255, 255), 255);
        assert_eq!(mul255(0, 255), 0);
        assert_eq!(mul255(255, 0), 0);
        // ~half
        assert!((mul255(128, 255) as i32 - 128).abs() <= 1);
    }

    #[test]
    fn blend_full_coverage_replaces() {
        let mut px = tiny_skia::PremultipliedColorU8::from_rgba(0, 0, 0, 255).unwrap();
        blend(&mut px, 255, 255, 255, 255);
        assert_eq!((px.red(), px.green(), px.blue()), (255, 255, 255));
    }

    #[test]
    fn blend_zero_coverage_is_noop() {
        let mut px = tiny_skia::PremultipliedColorU8::from_rgba(10, 20, 30, 255).unwrap();
        blend(&mut px, 255, 255, 255, 0);
        assert_eq!((px.red(), px.green(), px.blue()), (10, 20, 30));
    }
}
