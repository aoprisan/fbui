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

use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Style, Weight};

use crate::color::Color;
use crate::geom::{IRect, Point, Size};
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
/// bundled set, or [`with_default_font`] for the compiled-in default.
///
/// [`new`]: FontContext::new
/// [`with_fonts`]: FontContext::with_fonts
/// [`with_default_font`]: FontContext::with_default_font
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
