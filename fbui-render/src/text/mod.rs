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

mod atlas;

use cosmic_text::{Attrs, Buffer, Cursor, Family, FontSystem, Metrics, Shaping, Style, Weight};

use crate::color::Color;
use crate::geom::{IRect, Point, Rect, Size};
use crate::painter::Painter;
use atlas::GlyphAtlas;

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

/// A shaped, laid-out paragraph ready to draw. Holds the cosmic-text buffer so
/// the (expensive) shaping is done once and reused across repaints.
pub struct TextLayout {
    buffer: Buffer,
    measured: Size,
}

impl TextLayout {
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
/// is what an embedded/ISO target wants. Use [`with_fonts`] to start from a
/// bundled set, or `with_default_font` (behind the `bundled-font` feature) for
/// the compiled-in default.
///
/// [`new`]: FontContext::new
/// [`with_fonts`]: FontContext::with_fonts
pub struct FontContext {
    font_system: FontSystem,
    atlas: GlyphAtlas,
}

impl Default for FontContext {
    fn default() -> Self {
        Self::new()
    }
}

impl FontContext {
    /// Build a context with an empty font database. Load fonts with
    /// [`load_font_data`](Self::load_font_data) before drawing, or prefer
    /// [`with_fonts`](Self::with_fonts) to supply them up front.
    pub fn new() -> Self {
        FontContext {
            font_system: FontSystem::new(),
            atlas: GlyphAtlas::new(),
        }
    }

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
        }
    }

    /// Build a context from the compiled-in [`DEFAULT_FONT`] (Inter Regular).
    /// Available under the `bundled-font` feature — a turnkey path to legible
    /// text on a target with no fonts of its own. Override by supplying your own
    /// via [`with_fonts`](Self::with_fonts).
    #[cfg(feature = "bundled-font")]
    pub fn with_default_font() -> Self {
        Self::with_fonts([DEFAULT_FONT.to_vec()])
    }

    /// Add a font from in-memory bytes (TTF/OTF). Useful for bundling a fixed
    /// font so rendering is reproducible regardless of the host's installed set.
    pub fn load_font_data(&mut self, data: Vec<u8>) {
        self.font_system.db_mut().load_font_data(data);
    }

    /// Shape and lay out `text` in `style`, wrapping at `max_width` logical
    /// pixels (or unbounded if `None`).
    pub fn layout(&mut self, text: &str, style: &TextStyle, max_width: Option<f32>) -> TextLayout {
        let metrics = Metrics::new(style.size, style.line_height);
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
        TextLayout {
            buffer,
            measured: Size::new(width, height),
        }
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
        let scale = painter.scale().factor();
        let clip = painter.clip();
        let base_x = (at.x * scale).round() as i32;
        let base_y = (at.y * scale).round() as i32;
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

                composite_glyph(target.pixels_mut(), tw, th, raster, gx, gy, gc, clip);
                dirty = dirty.union(IRect::new(gx, gy, raster.width, raster.height));
            }
        }

        painter.add_damage(dirty);
    }
}

/// Blend one rasterized glyph into the target's premultiplied pixels with
/// source-over alpha, clipped to `clip` and the buffer bounds.
#[allow(clippy::too_many_arguments)]
fn composite_glyph(
    pixels: &mut [tiny_skia::PremultipliedColorU8],
    tw: i32,
    th: i32,
    raster: &atlas::RasterGlyph,
    gx: i32,
    gy: i32,
    color: Color,
    clip: IRect,
) {
    let gw = raster.width as i32;
    for row in 0..raster.height as i32 {
        let py = gy + row;
        if py < 0 || py >= th || py < clip.y || py >= clip.bottom() {
            continue;
        }
        for col in 0..gw {
            let px = gx + col;
            if px < 0 || px >= tw || px < clip.x || px >= clip.right() {
                continue;
            }
            let idx = (py * tw + px) as usize;
            if raster.color {
                // Emoji: straight RGBA source.
                let o = ((row * gw + col) * 4) as usize;
                let (sr, sg, sb, sa) = (
                    raster.data[o],
                    raster.data[o + 1],
                    raster.data[o + 2],
                    raster.data[o + 3],
                );
                let ea = mul255(sa, color.a);
                blend(&mut pixels[idx], sr, sg, sb, ea);
            } else {
                // Coverage mask modulated by the run color's alpha.
                let cov = raster.data[(row * gw + col) as usize];
                let ea = mul255(cov, color.a);
                blend(&mut pixels[idx], color.r, color.g, color.b, ea);
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
