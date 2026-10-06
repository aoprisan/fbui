//! Bitmap fonts: glyphs rasterized ahead of time, laid out and drawn with no
//! shaping engine and no rasterizer at run time.
//!
//! The outline path (cosmic-text + swash) is the right default: any size, any
//! script, kerning and ligatures. It also costs ~70 KiB of heap once text is
//! shaped, plus the font file and the shaping and rasterizing code in flash.
//! A small device showing Latin text at a handful of sizes can instead carry
//! glyphs pre-rendered at exactly those sizes: a few tens of KiB of flash,
//! read in place, and no heap beyond each layout's line table.
//!
//! # The `.fbf` format
//!
//! One font = one face at one pixel size, little-endian, read zero-copy from
//! a `&'static [u8]` (typically `include_bytes!`):
//!
//! ```text
//! header   "FBF1"  px: f32  ascent: f32  descent: f32
//!          flags: u8 (1 = bold, 2 = italic)  bpp: u8 (= 4)  glyphs: u16
//!          family_len: u8  family: [u8; family_len]  (padded to 4 bytes)
//! glyphs   × glyphs, sorted by code point, 20 bytes each:
//!          code: u32  advance: f32  left: i16  top: i16  w: u16  h: u16
//!          offset: u32 (into the bitmap section)
//! bitmaps  4-bit coverage, rows of ceil(w/2) bytes, high nibble first
//! ```
//!
//! Metrics are in device pixels at `px`. `top` is the distance from the
//! baseline up to the bitmap's first row, `left` from the pen to its first
//! column — the same placement swash reports, so a bitmap font made from a
//! TTF lands its glyphs where the outline path would.
//!
//! Make one with the `make_bitmap_font` example (`cargo run -p fbui-render
//! --example make_bitmap_font -- font.ttf out.fbf 16 --chars latin1`), or
//! build one in code with [`BitmapFontWriter`].
//!
//! # What you give up
//!
//! Sizes are the ones you generated: text at another size uses the nearest
//! one (it isn't scaled). No shaping — one glyph per character, no kerning,
//! ligatures, complex scripts or right-to-left text; characters the font
//! lacks draw as its `?` (or not at all). Wrapping breaks at spaces, or
//! inside a word too long for the line.

#[allow(unused_imports)]
use crate::prelude::*;

use core::fmt;

use super::{FontFamily, TextStyle};
use crate::geom::{Rect, Size};

const MAGIC: &[u8; 4] = b"FBF1";
const GLYPH_RECORD: usize = 20;

/// Why a byte slice isn't a usable `.fbf` font.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BitmapFontError(&'static str);

impl fmt::Display for BitmapFontError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "bad bitmap font: {}", self.0)
    }
}

/// A validated `.fbf` font, read in place (see the [module docs](self)).
/// `Copy`: it is a view of `'static` bytes.
#[derive(Clone, Copy)]
pub struct BitmapFont {
    data: &'static [u8],
    px: f32,
    ascent: f32,
    descent: f32,
    bold: bool,
    italic: bool,
    family: &'static str,
    count: usize,
    table: usize,
    bitmaps: usize,
}

impl fmt::Debug for BitmapFont {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BitmapFont")
            .field("family", &self.family)
            .field("px", &self.px)
            .field("bold", &self.bold)
            .field("italic", &self.italic)
            .field("glyphs", &self.count)
            .finish()
    }
}

/// One glyph's placement and coverage.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BitmapGlyph {
    /// Pen advance, device pixels.
    pub advance: f32,
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    /// 4-bit rows, `(width + 1) / 2` bytes each.
    pub data: &'static [u8],
}

impl BitmapGlyph {
    /// Coverage (0–255) at column `x`, row `y`.
    pub fn coverage(&self, x: u32, y: u32) -> u8 {
        let row = (self.width as usize).div_ceil(2);
        let byte = self.data[y as usize * row + x as usize / 2];
        let nib = if x.is_multiple_of(2) {
            byte >> 4
        } else {
            byte & 0x0f
        };
        nib * 17
    }
}

fn rd_u16(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(o..o + 2)?.try_into().ok()?))
}
fn rd_u32(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?))
}
fn rd_i16(d: &[u8], o: usize) -> Option<i16> {
    Some(i16::from_le_bytes(d.get(o..o + 2)?.try_into().ok()?))
}
fn rd_f32(d: &[u8], o: usize) -> Option<f32> {
    Some(f32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?))
}

impl BitmapFont {
    /// Check `data` is a well-formed `.fbf` font and wrap it. Every offset,
    /// size and ordering is checked here, so drawing never reads out of
    /// bounds; a bad file is an error, never a panic.
    pub fn from_bytes(data: &'static [u8]) -> Result<BitmapFont, BitmapFontError> {
        let e = BitmapFontError;
        if data.get(0..4) != Some(&MAGIC[..]) {
            return Err(e("not an FBF1 file"));
        }
        let short = e("truncated header");
        let px = rd_f32(data, 4).ok_or(short)?;
        let ascent = rd_f32(data, 8).ok_or(short)?;
        let descent = rd_f32(data, 12).ok_or(short)?;
        let flags = *data.get(16).ok_or(short)?;
        let bpp = *data.get(17).ok_or(short)?;
        let count = rd_u16(data, 18).ok_or(short)? as usize;
        let flen = *data.get(20).ok_or(short)? as usize;
        let family = data.get(21..21 + flen).ok_or(short)?;
        let family = core::str::from_utf8(family).map_err(|_| e("family name is not UTF-8"))?;
        if bpp != 4 {
            return Err(e("only 4-bit coverage is supported"));
        }
        if !(px.is_finite() && px > 0.0 && ascent.is_finite() && descent.is_finite()) {
            return Err(e("bad metrics"));
        }
        let table = (21 + flen).div_ceil(4) * 4;
        let bitmaps = table + count * GLYPH_RECORD;
        if data.len() < bitmaps {
            return Err(e("truncated glyph table"));
        }
        let font = BitmapFont {
            data,
            px,
            ascent,
            descent,
            bold: flags & 1 != 0,
            italic: flags & 2 != 0,
            family,
            count,
            table,
            bitmaps,
        };
        let mut prev: Option<u32> = None;
        for i in 0..count {
            let r = table + i * GLYPH_RECORD;
            let code = rd_u32(data, r).ok_or(short)?;
            if prev.is_some_and(|p| p >= code) {
                return Err(e("glyphs not sorted by code point"));
            }
            prev = Some(code);
            let advance = rd_f32(data, r + 4).ok_or(short)?;
            if !advance.is_finite() {
                return Err(e("bad advance"));
            }
            let (w, h) = (
                rd_u16(data, r + 12).ok_or(short)?,
                rd_u16(data, r + 14).ok_or(short)?,
            );
            let off = rd_u32(data, r + 16).ok_or(short)? as usize;
            let len = (w as usize).div_ceil(2) * h as usize;
            let start = bitmaps
                .checked_add(off)
                .ok_or(e("bitmap offset overflows"))?;
            if start.checked_add(len).is_none_or(|end| end > data.len()) {
                return Err(e("glyph bitmap out of bounds"));
            }
        }
        Ok(font)
    }

    /// The size the glyphs were rendered at, in device pixels.
    pub fn pixel_size(&self) -> f32 {
        self.px
    }

    pub fn family(&self) -> &'static str {
        self.family
    }

    pub fn is_bold(&self) -> bool {
        self.bold
    }

    pub fn is_italic(&self) -> bool {
        self.italic
    }

    /// How many glyphs the font carries.
    pub fn glyph_count(&self) -> usize {
        self.count
    }

    /// Whether the font has a glyph for `ch`.
    pub fn has_glyph(&self, ch: char) -> bool {
        self.find(ch).is_some()
    }

    fn find(&self, ch: char) -> Option<usize> {
        let code = ch as u32;
        let (mut lo, mut hi) = (0usize, self.count);
        while lo < hi {
            let mid = (lo + hi) / 2;
            let c = rd_u32(self.data, self.table + mid * GLYPH_RECORD)?;
            match c.cmp(&code) {
                core::cmp::Ordering::Less => lo = mid + 1,
                core::cmp::Ordering::Greater => hi = mid,
                core::cmp::Ordering::Equal => return Some(mid),
            }
        }
        None
    }

    fn record(&self, i: usize) -> Option<BitmapGlyph> {
        let d = self.data;
        let r = self.table + i * GLYPH_RECORD;
        let (w, h) = (rd_u16(d, r + 12)? as u32, rd_u16(d, r + 14)? as u32);
        let start = self.bitmaps + rd_u32(d, r + 16)? as usize;
        let len = (w as usize).div_ceil(2) * h as usize;
        Some(BitmapGlyph {
            advance: rd_f32(d, r + 4)?,
            left: rd_i16(d, r + 8)? as i32,
            top: rd_i16(d, r + 10)? as i32,
            width: w,
            height: h,
            data: d.get(start..start + len)?,
        })
    }

    /// The glyph for `ch`, falling back to `?` for a character the font
    /// lacks (`None` only if it lacks that too).
    pub(crate) fn glyph(&self, ch: char) -> Option<BitmapGlyph> {
        self.find(ch)
            .or_else(|| self.find('?'))
            .and_then(|i| self.record(i))
    }

    fn advance(&self, ch: char) -> f32 {
        self.glyph(ch).map_or(0.0, |g| g.advance)
    }
}

/// Builds a `.fbf` font in memory — what the `make_bitmap_font` generator
/// uses, and a way to make small fonts for tests.
pub struct BitmapFontWriter {
    px: f32,
    ascent: f32,
    descent: f32,
    flags: u8,
    family: String,
    glyphs: Vec<PendingGlyph>,
}

/// A glyph waiting to be written: its table record and packed coverage.
struct PendingGlyph {
    code: u32,
    advance: f32,
    left: i16,
    top: i16,
    width: u16,
    height: u16,
    packed: Vec<u8>,
}

impl BitmapFontWriter {
    /// A font rendered at `px` device pixels, with the face's ascent and
    /// descent (both positive, device pixels) at that size.
    pub fn new(family: &str, px: f32, ascent: f32, descent: f32) -> Self {
        BitmapFontWriter {
            px,
            ascent,
            descent,
            flags: 0,
            family: family.into(),
            glyphs: Vec::new(),
        }
    }

    pub fn bold(mut self, on: bool) -> Self {
        self.flags = (self.flags & !1) | u8::from(on);
        self
    }

    pub fn italic(mut self, on: bool) -> Self {
        self.flags = (self.flags & !2) | (u8::from(on) << 1);
        self
    }

    /// Add `ch`: its advance, its bitmap's offset from the pen (`left`) and
    /// from the baseline up to its top row (`top`), and `width × height`
    /// 8-bit coverage values (quantized to 4 bits). A repeated character
    /// replaces the earlier one.
    #[allow(clippy::too_many_arguments)]
    pub fn glyph(
        &mut self,
        ch: char,
        advance: f32,
        left: i16,
        top: i16,
        width: u16,
        height: u16,
        coverage: &[u8],
    ) -> &mut Self {
        let row = (width as usize).div_ceil(2);
        let mut packed = vec![0u8; row * height as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let c = coverage.get(y * width as usize + x).copied().unwrap_or(0);
                // Round to the nearest of 16 levels (0, 17, …, 255).
                let n = ((c as u16 + 8) / 17).min(15) as u8;
                packed[y * row + x / 2] |= if x.is_multiple_of(2) { n << 4 } else { n };
            }
        }
        self.glyphs.retain(|g| g.code != ch as u32);
        self.glyphs.push(PendingGlyph {
            code: ch as u32,
            advance,
            left,
            top,
            width,
            height,
            packed,
        });
        self
    }

    /// The `.fbf` bytes.
    pub fn finish(mut self) -> Vec<u8> {
        self.glyphs.sort_by_key(|g| g.code);
        let fam = self.family.as_bytes();
        let fam = &fam[..fam.len().min(255)];
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&self.px.to_le_bytes());
        out.extend_from_slice(&self.ascent.to_le_bytes());
        out.extend_from_slice(&self.descent.to_le_bytes());
        out.push(self.flags);
        out.push(4);
        out.extend_from_slice(&(self.glyphs.len() as u16).to_le_bytes());
        out.push(fam.len() as u8);
        out.extend_from_slice(fam);
        while out.len() % 4 != 0 {
            out.push(0);
        }
        let mut offset = 0u32;
        for g in &self.glyphs {
            out.extend_from_slice(&g.code.to_le_bytes());
            out.extend_from_slice(&g.advance.to_le_bytes());
            out.extend_from_slice(&g.left.to_le_bytes());
            out.extend_from_slice(&g.top.to_le_bytes());
            out.extend_from_slice(&g.width.to_le_bytes());
            out.extend_from_slice(&g.height.to_le_bytes());
            out.extend_from_slice(&offset.to_le_bytes());
            offset += g.packed.len() as u32;
        }
        for g in &self.glyphs {
            out.extend_from_slice(&g.packed);
        }
        out
    }
}

// ---- layout -------------------------------------------------------------

/// Choose the face for `style` from `fonts` at `scale`: family first (a
/// named family must match; the generic ones take any), then bold/italic,
/// then the pixel size nearest the requested one.
pub(crate) fn select(fonts: &[BitmapFont], style: &TextStyle, scale: f32) -> Option<BitmapFont> {
    let want = style.size * scale;
    let named = match &style.family {
        FontFamily::Name(n) => Some(n.as_str()),
        _ => None,
    };
    let family_ok = |f: &BitmapFont| named.is_none_or(|n| f.family.eq_ignore_ascii_case(n));
    let pool: Vec<&BitmapFont> = if fonts.iter().any(family_ok) {
        fonts.iter().filter(|f| family_ok(f)).collect()
    } else {
        fonts.iter().collect()
    };
    let rank = |f: &&BitmapFont| {
        let style_miss = u8::from(f.bold != style.bold) + u8::from(f.italic != style.italic);
        // Size distance in 1/64 px keeps the comparison total.
        let dist = ((f.px - want).abs() * 64.0) as u32;
        (style_miss, dist)
    };
    pool.into_iter().min_by_key(rank).copied()
}

/// One character placed on a line.
#[derive(Clone, Copy, Debug)]
struct Placed {
    /// Byte offset of the character in the source text.
    byte: usize,
    ch: char,
    /// Pen position and advance, device pixels from the line start.
    x: f32,
    advance: f32,
}

/// One visual line.
#[derive(Clone, Debug)]
struct Line {
    /// Source byte range the line covers (trailing spaces included, the
    /// line ending excluded).
    start: usize,
    end: usize,
    glyphs: Vec<Placed>,
    /// Width without trailing spaces, device pixels.
    width: f32,
}

/// A laid-out paragraph set in a bitmap font.
#[derive(Clone, Debug)]
pub(crate) struct BitmapLayout {
    font: BitmapFont,
    /// Device pixels per logical pixel.
    scale: f32,
    /// Logical line height.
    line_height: f32,
    lines: Vec<Line>,
    text_len: usize,
}

impl BitmapLayout {
    pub fn new(
        font: BitmapFont,
        text: &str,
        style: &TextStyle,
        max_width: Option<f32>,
        scale: f32,
    ) -> Self {
        let scale = if scale > 0.0 { scale } else { 1.0 };
        let max = max_width.map(|w| w * scale);
        let mut lines = Vec::new();
        let mut para_start = 0usize;
        for para in text.split('\n') {
            let body = para.strip_suffix('\r').unwrap_or(para);
            wrap(&font, body, para_start, max, &mut lines);
            para_start += para.len() + 1;
        }
        BitmapLayout {
            font,
            scale,
            line_height: style.line_height,
            lines,
            text_len: text.len(),
        }
    }

    pub fn size(&self) -> Size {
        let w = self.lines.iter().fold(0.0f32, |m, l| m.max(l.width)) / self.scale;
        Size::new(w, self.lines.len() as f32 * self.line_height)
    }

    pub fn line_height(&self) -> f32 {
        self.line_height
    }

    pub fn line_count(&self) -> usize {
        self.lines.len().max(1)
    }

    /// The logical distance from a line's top to its baseline: the face
    /// centred in the line, as cosmic-text does it.
    fn baseline(&self) -> f32 {
        let (a, d) = (
            self.font.ascent / self.scale,
            self.font.descent / self.scale,
        );
        (self.line_height - (a + d)) / 2.0 + a
    }

    /// Logical x of the boundary before byte `idx` on line `l`.
    fn x_at(&self, l: &Line, idx: usize) -> f32 {
        for g in &l.glyphs {
            if g.byte >= idx {
                return g.x / self.scale;
            }
        }
        l.glyphs
            .last()
            .map_or(0.0, |g| (g.x + g.advance) / self.scale)
    }

    pub fn hit(&self, x: f32, y: f32) -> usize {
        if self.lines.is_empty() {
            return 0;
        }
        if y < 0.0 {
            return self.lines[0].start;
        }
        let i = (y / self.line_height) as usize;
        let Some(l) = self.lines.get(i) else {
            return self.text_len;
        };
        let xd = x * self.scale;
        for g in &l.glyphs {
            if xd < g.x + g.advance / 2.0 {
                return g.byte;
            }
        }
        l.end
    }

    /// The visual line holding the boundary before `idx`: at a wrap point
    /// (the end of one line is the start of the next, same paragraph) the
    /// later line, where typing continues.
    fn line_of(&self, idx: usize) -> Option<usize> {
        let mut found = None;
        for (i, l) in self.lines.iter().enumerate() {
            if idx >= l.start && idx <= l.end {
                found = Some(i);
            }
            if idx < l.start {
                break;
            }
        }
        found
    }

    pub fn caret(&self, idx: usize) -> Rect {
        let idx = idx.min(self.text_len);
        let lh = self.line_height;
        match self.line_of(idx) {
            Some(i) => Rect::new(self.x_at(&self.lines[i], idx), i as f32 * lh, 0.0, lh),
            None => Rect::new(0.0, 0.0, 0.0, lh),
        }
    }

    pub fn selection_rects(&self, a: usize, b: usize) -> Vec<Rect> {
        let (a, b) = (a.min(b).min(self.text_len), a.max(b).min(self.text_len));
        let mut out = Vec::new();
        if a == b {
            return out;
        }
        let lh = self.line_height;
        for (i, l) in self.lines.iter().enumerate() {
            if l.end < a || l.start > b {
                continue;
            }
            let top = i as f32 * lh;
            let (s, e) = (a.max(l.start), b.min(l.end));
            let (x0, x1) = (self.x_at(l, s), self.x_at(l, e));
            if x1 > x0 {
                out.push(Rect::new(x0, top, x1 - x0, lh));
            }
            // The selection runs on past this line's end: mark it, as the
            // outline path does, so a selected line break is visible.
            if b > l.end && i + 1 < self.lines.len() {
                out.push(Rect::new(x1.max(x0), top, lh * 0.3, lh));
            }
        }
        out
    }

    /// Every glyph to draw: (glyph, device x from the layout's left, device
    /// baseline y from its top).
    pub fn glyphs(&self) -> impl Iterator<Item = (BitmapGlyph, f32, f32)> + '_ {
        let base = self.baseline();
        self.lines.iter().enumerate().flat_map(move |(i, l)| {
            let y = (i as f32 * self.line_height + base) * self.scale;
            l.glyphs
                .iter()
                .filter(|g| !g.ch.is_whitespace())
                .filter_map(move |g| self.font.glyph(g.ch).map(|gl| (gl, g.x, y)))
        })
    }
}

/// Greedy word wrap of one paragraph into `out`. Breaks after spaces (which
/// hang past the margin, uncounted); a word wider than the line is broken
/// between characters.
fn wrap(font: &BitmapFont, para: &str, base: usize, max: Option<f32>, out: &mut Vec<Line>) {
    let mut line = Line {
        start: base,
        end: base,
        glyphs: Vec::new(),
        width: 0.0,
    };
    let mut pen = 0.0f32;
    // Index into `line.glyphs` just after the last space: a break point.
    let mut last_break: Option<usize> = None;
    for (off, ch) in para.char_indices() {
        let byte = base + off;
        let adv = font.advance(ch);
        let space = ch == ' ' || ch == '\t';
        let overflow = max.is_some_and(|m| !space && pen + adv > m && !line.glyphs.is_empty());
        if overflow {
            // Break at the last space if there is one, else here.
            let cut = last_break.unwrap_or(line.glyphs.len());
            let rest: Vec<Placed> = line.glyphs.split_off(cut);
            let next_start = rest.first().map_or(byte, |g| g.byte);
            line.end = next_start;
            line.width = visible_width(&line.glyphs);
            out.push(core::mem::replace(
                &mut line,
                Line {
                    start: next_start,
                    end: next_start,
                    glyphs: Vec::new(),
                    width: 0.0,
                },
            ));
            let shift = rest.first().map_or(0.0, |g| g.x);
            pen = 0.0;
            for mut g in rest {
                g.x -= shift;
                pen = g.x + g.advance;
                line.glyphs.push(g);
            }
            last_break = None;
        }
        line.glyphs.push(Placed {
            byte,
            ch,
            x: pen,
            advance: adv,
        });
        pen += adv;
        if space {
            last_break = Some(line.glyphs.len());
        }
    }
    line.end = base + para.len();
    line.width = visible_width(&line.glyphs);
    out.push(line);
}

/// A line's width without its trailing spaces.
fn visible_width(glyphs: &[Placed]) -> f32 {
    glyphs
        .iter()
        .rev()
        .find(|g| !g.ch.is_whitespace())
        .map_or(0.0, |g| g.x + g.advance)
}

/// The bundled bitmap fonts (`bundled-bitmap-font`): Inter Regular at 12, 16,
/// 20 and 24 px, Latin-1, generated by the `make_bitmap_font` example.
#[cfg(feature = "bundled-bitmap-font")]
pub fn bundled() -> Vec<BitmapFont> {
    const FILES: [&[u8]; 4] = [
        include_bytes!("../../fonts/Inter-12.fbf"),
        include_bytes!("../../fonts/Inter-16.fbf"),
        include_bytes!("../../fonts/Inter-20.fbf"),
        include_bytes!("../../fonts/Inter-24.fbf"),
    ];
    // Generated and checked in CI (`bundled_bitmap_fonts_parse`), so a
    // parse failure here is a build defect, not input to handle.
    FILES
        .iter()
        .filter_map(|f| BitmapFont::from_bytes(f).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Color;

    /// A monospace-ish test font: every glyph `adv` wide with a solid
    /// `adv-2 × 10` box, plus a blank space and a `?`.
    fn font(px: f32, adv: f32, chars: &str) -> BitmapFont {
        let mut w = BitmapFontWriter::new("Test", px, px * 0.8, px * 0.2);
        let bw = (adv as u16).saturating_sub(2).max(1);
        let ink = vec![255u8; bw as usize * 10];
        for ch in chars.chars().chain(['?']) {
            if ch == ' ' {
                w.glyph(ch, adv, 0, 0, 0, 0, &[]);
            } else {
                w.glyph(ch, adv, 1, 10, bw, 10, &ink);
            }
        }
        let bytes: &'static [u8] = Box::leak(w.finish().into_boxed_slice());
        BitmapFont::from_bytes(bytes).unwrap()
    }

    fn style(size: f32) -> TextStyle {
        TextStyle::new(size, Color::WHITE)
    }

    #[test]
    fn writer_round_trips_and_quantizes_to_4_bits() {
        let mut w = BitmapFontWriter::new("Round", 13.0, 10.0, 3.0).bold(true);
        w.glyph('b', 7.5, -1, 9, 3, 2, &[0, 128, 255, 17, 34, 250]);
        w.glyph('a', 6.0, 0, 8, 1, 1, &[200]);
        let bytes: &'static [u8] = Box::leak(w.finish().into_boxed_slice());
        let f = BitmapFont::from_bytes(bytes).unwrap();
        assert_eq!(
            (f.family(), f.pixel_size(), f.is_bold(), f.is_italic()),
            ("Round", 13.0, true, false)
        );
        assert_eq!(f.glyph_count(), 2);
        let g = f.glyph('b').unwrap();
        assert_eq!(
            (g.advance, g.left, g.top, g.width, g.height),
            (7.5, -1, 9, 3, 2)
        );
        let row0: Vec<u8> = (0..3).map(|x| g.coverage(x, 0)).collect();
        let row1: Vec<u8> = (0..3).map(|x| g.coverage(x, 1)).collect();
        assert_eq!(row0, [0, 136, 255]);
        assert_eq!(row1, [17, 34, 255]);
        // Missing characters fall back to `?` — absent here, so none.
        assert!(f.glyph('z').is_none());
        assert!(f.has_glyph('a') && !f.has_glyph('z'));
    }

    #[test]
    fn corrupt_files_are_errors_never_panics() {
        let good: &'static [u8] = Box::leak(
            {
                let mut w = BitmapFontWriter::new("C", 10.0, 8.0, 2.0);
                w.glyph('x', 5.0, 0, 5, 4, 5, &[255; 20]);
                w.glyph('y', 5.0, 0, 5, 4, 5, &[128; 20]);
                w.finish()
            }
            .into_boxed_slice(),
        );
        assert!(BitmapFont::from_bytes(good).is_ok());
        // Every truncation, and a burst of byte flips at every offset.
        for n in 0..good.len() {
            let t: &'static [u8] = Box::leak(good[..n].to_vec().into_boxed_slice());
            assert!(BitmapFont::from_bytes(t).is_err(), "truncated to {n}");
        }
        for i in 0..good.len() {
            for v in [0x00u8, 0xff, 0x80, 0x7f] {
                let mut m = good.to_vec();
                m[i] = v;
                let m: &'static [u8] = Box::leak(m.into_boxed_slice());
                if let Ok(f) = BitmapFont::from_bytes(m) {
                    // Whatever parsed must draw without going out of bounds.
                    for ch in ['x', 'y', '?'] {
                        if let Some(g) = f.glyph(ch) {
                            for y in 0..g.height {
                                for x in 0..g.width {
                                    let _ = g.coverage(x, y);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn nearest_size_and_style_win() {
        let fonts = [
            font(12.0, 6.0, "a"),
            font(16.0, 8.0, "a"),
            font(24.0, 12.0, "a"),
        ];
        let pick =
            |size: f32, scale: f32| select(&fonts, &style(size), scale).unwrap().pixel_size();
        assert_eq!(pick(16.0, 1.0), 16.0);
        assert_eq!(pick(13.0, 1.0), 12.0);
        assert_eq!(pick(15.0, 1.0), 16.0);
        assert_eq!(pick(12.0, 2.0), 24.0, "sizes are device pixels");
        assert!(select(&[], &style(16.0), 1.0).is_none());
    }

    #[test]
    fn measures_lines_and_wraps_at_spaces() {
        let f = font(16.0, 10.0, "abcdefghij");
        let st = style(16.0);
        let l = BitmapLayout::new(f, "abc de", &st, None, 1.0);
        assert_eq!(l.line_count(), 1);
        assert_eq!(l.size().w, 60.0);
        assert_eq!(l.size().h, st.line_height);
        // 45 px wide: "abc " fits (the space hangs), "de" goes down.
        let l = BitmapLayout::new(f, "abc de", &st, Some(45.0), 1.0);
        assert_eq!(l.line_count(), 2);
        assert_eq!(l.size().w, 30.0, "trailing space not counted");
        assert_eq!(
            l.caret(4).y,
            st.line_height,
            "the wrap point belongs to the next line"
        );
        // A word longer than the line breaks inside it.
        let l = BitmapLayout::new(f, "abcdefghij", &st, Some(35.0), 1.0);
        assert_eq!(l.line_count(), 4);
        // Explicit newlines, including an empty line and CRLF.
        let l = BitmapLayout::new(f, "ab\n\r\ncd", &st, None, 1.0);
        assert_eq!(l.line_count(), 3);
        assert_eq!(l.caret(7).y, 2.0 * st.line_height);
        assert_eq!(l.caret(7).x, 20.0);
        // Empty text: one empty line, a caret at the origin.
        let l = BitmapLayout::new(f, "", &st, None, 1.0);
        assert_eq!((l.line_count(), l.size().w), (1, 0.0));
        assert_eq!(l.caret(0), Rect::new(0.0, 0.0, 0.0, st.line_height));
    }

    #[test]
    fn hit_and_caret_agree() {
        let f = font(16.0, 10.0, "abcdefghij");
        let st = style(16.0);
        let text = "abc def\nghij";
        let l = BitmapLayout::new(f, text, &st, Some(45.0), 1.0);
        for idx in text.char_indices().map(|(i, _)| i).chain([text.len()]) {
            let c = l.caret(idx);
            // Clicking just right of a caret lands on that boundary.
            assert_eq!(l.hit(c.x + 1.0, c.y + c.h / 2.0), idx, "idx {idx}");
        }
        assert_eq!(l.hit(-5.0, -5.0), 0);
        assert_eq!(l.hit(500.0, 500.0), text.len());
        assert_eq!(l.hit(500.0, 1.0), 4, "past a wrapped line's end: its end");
    }

    #[test]
    fn logical_metrics_follow_the_scale() {
        // At 2× a 16 px style uses the 32 px face, and measures in logical px.
        let fonts = [font(16.0, 10.0, "ab"), font(32.0, 20.0, "ab")];
        let st = style(16.0);
        let f = select(&fonts, &st, 2.0).unwrap();
        let l = BitmapLayout::new(f, "ab", &st, None, 2.0);
        assert_eq!(l.size().w, 20.0);
        assert_eq!(l.caret(1).x, 10.0);
        // Its glyphs are placed in device pixels.
        let xs: Vec<f32> = l.glyphs().map(|g| g.1).collect();
        assert_eq!(xs, [0.0, 20.0]);
    }

    #[test]
    fn selection_covers_each_line_and_marks_breaks() {
        let f = font(16.0, 10.0, "abcdef");
        let st = style(16.0);
        let l = BitmapLayout::new(f, "abc\ndef", &st, None, 1.0);
        let r = l.selection_rects(1, 6);
        // "bc" + the line-break marker on line 0, "de" on line 1.
        assert_eq!(r[0], Rect::new(10.0, 0.0, 20.0, st.line_height));
        assert_eq!(r[1].x, 30.0);
        assert_eq!(r[2], Rect::new(0.0, st.line_height, 20.0, st.line_height));
        assert!(l.selection_rects(3, 3).is_empty());
        assert_eq!(l.selection_rects(6, 1), r, "either order");
    }

    #[cfg(feature = "bundled-bitmap-font")]
    #[test]
    fn bundled_bitmap_fonts_parse() {
        let fonts = bundled();
        assert_eq!(fonts.len(), 4);
        for f in &fonts {
            assert_eq!(f.family(), "Inter");
            for ch in (' '..='~').chain(['é', 'ß', '€', '…', '→']) {
                assert!(f.has_glyph(ch), "{} px lacks {ch:?}", f.pixel_size());
            }
        }
    }
}
