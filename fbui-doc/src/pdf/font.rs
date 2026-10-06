//! Fonts (ISO 32000-1 §9.5–9.10): turning string bytes into glyph outlines
//! and advances.
//!
//! A [`Font`] knows how to split a string into codes, each code's width,
//! and its outline as a `tiny_skia::Path` in *text space* (1.0 = the font
//! size). Outlines are cached per code.

use alloc::borrow::Cow;
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use tiny_skia::{Path, PathBuilder, Transform};
use ttf_parser::{GlyphId, OutlineBuilder};

use super::encoding::{self, glyph_to_unicode};
use super::lexer::{Lexer, Token};
use super::object::{Dict, Object};
use super::type1::{Sink, Type1Font};
use super::Document;

/// A face for text the PDF doesn't embed.
pub struct FallbackFace {
    data: Cow<'static, [u8]>,
    upem: f32,
}

impl FallbackFace {
    pub fn new(data: Cow<'static, [u8]>) -> Option<FallbackFace> {
        let face = ttf_parser::Face::parse(&data, 0).ok()?;
        let upem = face.units_per_em() as f32;
        Some(FallbackFace { data, upem })
    }
}

enum Program {
    /// TrueType or OpenType (glyf or CFF inside an sfnt).
    Sfnt(Rc<Vec<u8>>),
    /// A bare CFF table (`FontFile3` `/Type1C` or `/CIDFontType0C`).
    Cff(Rc<Vec<u8>>),
    Type1(Type1Font),
    Fallback(Rc<FallbackFace>),
    /// Type 3: glyphs are content streams the renderer runs.
    Type3,
    None,
}

/// How codes map to glyph ids in a composite (Type 0) font.
enum CidToGid {
    Identity,
    Map(Vec<u16>),
}

/// A Type 3 font's glyph procedures and their context.
pub struct Type3 {
    pub matrix: Transform,
    pub procs: Dict,
    pub resources: Option<Dict>,
}

/// The parsed bits of a CMap: code space and code → CID / code → Unicode.
#[derive(Default)]
struct CMap {
    /// `(low, high, byte length)` codespace ranges.
    codespace: Vec<(u32, u32, u8)>,
    /// `(low, high, first value)` ranges (cidrange / bfrange).
    ranges: Vec<(u32, u32, u32)>,
    /// Single mappings to text (bfchar/bfrange with strings).
    text: BTreeMap<u32, String>,
}

pub struct Font {
    program: Program,
    composite: bool,
    /// Glyph space → text space.
    matrix: Transform,
    /// Simple fonts: code → glyph name after `/Differences`.
    names: Vec<Option<Vec<u8>>>,
    /// Whether the encoding was given explicitly (vs the font's built-in).
    explicit_encoding: bool,
    symbolic: bool,
    first_char: u32,
    widths: Vec<f32>,
    missing_width: f32,
    /// Composite: `(first cid, last cid, width)`.
    cid_widths: Vec<(u32, u32, f32)>,
    default_width: f32,
    /// Composite: code → CID. `None` = Identity (2-byte codes).
    encoding_cmap: Option<CMap>,
    cid_to_gid: CidToGid,
    to_unicode: Option<CMap>,
    /// Draw with a thin stroke too (fallback for a bold face we lack).
    pub fake_bold: bool,
    pub type3: Option<Type3>,
    cache: RefCell<BTreeMap<u32, Option<Rc<Path>>>>,
    /// CID → GID for CID-keyed CFF, built on first use.
    cff_cid_map: RefCell<Option<BTreeMap<u16, u16>>>,
}

impl Font {
    pub fn load(doc: &Document, dict: &Dict) -> Font {
        let subtype = dict.name(b"Subtype").unwrap_or(b"Type1").to_vec();
        let mut font = Font {
            program: Program::None,
            composite: false,
            matrix: Transform::from_scale(0.001, 0.001),
            names: alloc::vec![None; 256],
            explicit_encoding: false,
            symbolic: false,
            first_char: 0,
            widths: Vec::new(),
            missing_width: 0.0,
            cid_widths: Vec::new(),
            default_width: 1000.0,
            encoding_cmap: None,
            cid_to_gid: CidToGid::Identity,
            to_unicode: None,
            fake_bold: false,
            type3: None,
            cache: RefCell::new(BTreeMap::new()),
            cff_cid_map: RefCell::new(None),
        };
        if let Some(s) = doc.get_in(dict, b"ToUnicode").as_stream() {
            if let Ok(bytes) = doc.stream_bytes(s) {
                font.to_unicode = Some(parse_cmap(&bytes));
            }
        }
        let base = doc.get_in(dict, b"BaseFont");
        let base_name = base.as_name().unwrap_or(b"");
        let bold_name = contains(base_name, b"Bold")
            || contains(base_name, b"Black")
            || contains(base_name, b"Heavy");

        if subtype == b"Type0" {
            font.composite = true;
            font.load_composite(doc, dict);
        } else if subtype == b"Type3" {
            font.load_simple_metrics(doc, dict);
            font.load_encoding(doc, dict, None);
            let m = floats(doc, &doc.get_in(dict, b"FontMatrix"));
            if m.len() == 6 {
                font.matrix = Transform::from_row(m[0], m[1], m[2], m[3], m[4], m[5]);
            }
            let procs = doc
                .get_in(dict, b"CharProcs")
                .as_dict()
                .cloned()
                .unwrap_or_default();
            let resources = doc.get_in(dict, b"Resources").as_dict().cloned();
            font.type3 = Some(Type3 {
                matrix: font.matrix,
                procs,
                resources,
            });
            font.program = Program::Type3;
        } else {
            let desc = doc.get_in(dict, b"FontDescriptor");
            let desc = desc.as_dict().cloned().unwrap_or_default();
            font.load_simple_metrics(doc, dict);
            font.missing_width = doc.get_in(&desc, b"MissingWidth").as_f32().unwrap_or(0.0);
            let flags = doc.get_in(&desc, b"Flags").as_i64().unwrap_or(0);
            font.symbolic = flags & 4 != 0;
            font.load_program(doc, &desc);
            let builtin = match &font.program {
                Program::Type1(t1) => Some(t1.encoding.clone()),
                _ => None,
            };
            font.load_encoding(doc, dict, builtin);
            if matches!(font.program, Program::None) {
                if let Some(fb) = &doc.fallback_font {
                    font.program = Program::Fallback(fb.clone());
                    font.matrix = Transform::from_scale(1.0 / fb.upem, 1.0 / fb.upem);
                }
            }
        }
        // Synthesize bold only where we substituted the face: an embedded
        // bold font already is bold.
        font.fake_bold = bold_name && matches!(font.program, Program::Fallback(_));
        font
    }

    fn load_simple_metrics(&mut self, doc: &Document, dict: &Dict) {
        self.first_char = doc
            .get_in(dict, b"FirstChar")
            .as_i64()
            .unwrap_or(0)
            .clamp(0, 255) as u32;
        self.widths = floats(doc, &doc.get_in(dict, b"Widths"));
    }

    fn load_encoding(
        &mut self,
        doc: &Document,
        dict: &Dict,
        builtin: Option<Vec<Option<Vec<u8>>>>,
    ) {
        let from = |e: &encoding::Encoding| -> Vec<Option<Vec<u8>>> {
            e.iter().map(|n| n.map(<[u8]>::to_vec)).collect()
        };
        let pick = |name: &[u8]| match name {
            b"WinAnsiEncoding" => Some(from(&encoding::WIN_ANSI)),
            b"MacRomanEncoding" => Some(from(&encoding::MAC_ROMAN)),
            b"StandardEncoding" => Some(from(&encoding::STANDARD)),
            _ => None,
        };
        let default = || builtin.clone().unwrap_or_else(|| from(&encoding::STANDARD));
        let enc = doc.get_in(dict, b"Encoding");
        match &enc {
            Object::Name(n) => {
                self.explicit_encoding = true;
                self.names = pick(n).unwrap_or_else(default);
            }
            Object::Dict(d) => {
                self.explicit_encoding = true;
                self.names = d
                    .name(b"BaseEncoding")
                    .and_then(pick)
                    .unwrap_or_else(default);
                if let Some(diffs) = doc.get_in(d, b"Differences").as_array() {
                    let mut code = 0usize;
                    for o in diffs {
                        match o {
                            Object::Int(c) => code = (*c).clamp(0, 255) as usize,
                            Object::Name(n) => {
                                if code < 256 {
                                    self.names[code] = Some(n.clone());
                                }
                                code += 1;
                            }
                            _ => {}
                        }
                    }
                }
            }
            _ => self.names = default(),
        }
    }

    fn load_program(&mut self, doc: &Document, desc: &Dict) {
        if let Some(s) = doc.get_in(desc, b"FontFile2").as_stream() {
            if let Ok(b) = doc.stream_bytes(s) {
                if let Ok(face) = ttf_parser::Face::parse(&b, 0) {
                    let upem = face.units_per_em().max(1) as f32;
                    self.matrix = Transform::from_scale(1.0 / upem, 1.0 / upem);
                    self.program = Program::Sfnt(Rc::new(b));
                }
            }
        } else if let Some(s) = doc.get_in(desc, b"FontFile3").as_stream() {
            let sub = s.dict.name(b"Subtype").unwrap_or(b"").to_vec();
            if let Ok(b) = doc.stream_bytes(s) {
                if sub == b"OpenType" {
                    if let Ok(face) = ttf_parser::Face::parse(&b, 0) {
                        let upem = face.units_per_em().max(1) as f32;
                        self.matrix = Transform::from_scale(1.0 / upem, 1.0 / upem);
                        self.program = Program::Sfnt(Rc::new(b));
                    }
                } else if let Some(t) = ttf_parser::cff::Table::parse(&b) {
                    let m = t.matrix();
                    self.matrix = Transform::from_row(m.sx, m.ky, m.kx, m.sy, m.tx, m.ty);
                    self.program = Program::Cff(Rc::new(b));
                }
            }
        } else if let Some(s) = doc.get_in(desc, b"FontFile").as_stream() {
            let len1 = doc
                .get_in(&s.dict, b"Length1")
                .as_i64()
                .map(|v| v.max(0) as usize);
            if let Ok(b) = doc.stream_bytes(s) {
                if let Some(t1) = Type1Font::parse(&b, len1) {
                    let m = t1.font_matrix;
                    self.matrix = Transform::from_row(m[0], m[1], m[2], m[3], m[4], m[5]);
                    self.program = Program::Type1(t1);
                }
            }
        }
    }

    fn load_composite(&mut self, doc: &Document, dict: &Dict) {
        match doc.get_in(dict, b"Encoding") {
            Object::Name(n) if n == b"Identity-H" || n == b"Identity-V" => {}
            Object::Stream(s) => {
                if let Ok(b) = doc.stream_bytes(&s) {
                    self.encoding_cmap = Some(parse_cmap(&b));
                }
            }
            // Predefined CJK CMaps aren't bundled; Identity is the best guess.
            _ => {}
        }
        let desc_fonts = doc.get_in(dict, b"DescendantFonts");
        let Some(cid_font) = desc_fonts.as_array().and_then(|a| a.first()) else {
            return;
        };
        let cid_font = doc.resolve(cid_font).unwrap_or_default();
        let Some(cf) = cid_font.as_dict() else { return };
        self.default_width = doc.get_in(cf, b"DW").as_f32().unwrap_or(1000.0);
        if let Some(w) = doc.get_in(cf, b"W").as_array() {
            let mut i = 0;
            while i < w.len() {
                let first = doc
                    .resolve(&w[i])
                    .ok()
                    .and_then(|o| o.as_i64())
                    .unwrap_or(0)
                    .max(0) as u32;
                match w.get(i + 1).map(|o| doc.resolve(o).unwrap_or_default()) {
                    Some(Object::Array(ws)) => {
                        for (k, o) in ws.iter().enumerate() {
                            let c = first + k as u32;
                            let v = doc
                                .resolve(o)
                                .ok()
                                .and_then(|o| o.as_f32())
                                .unwrap_or(self.default_width);
                            self.cid_widths.push((c, c, v));
                        }
                        i += 2;
                    }
                    Some(last) => {
                        let last = last.as_i64().unwrap_or(0).max(0) as u32;
                        let v = w
                            .get(i + 2)
                            .and_then(|o| doc.resolve(o).ok()?.as_f32())
                            .unwrap_or(self.default_width);
                        self.cid_widths.push((first, last, v));
                        i += 3;
                    }
                    None => break,
                }
            }
        }
        match doc.get_in(cf, b"CIDToGIDMap") {
            Object::Stream(s) => {
                if let Ok(b) = doc.stream_bytes(&s) {
                    self.cid_to_gid = CidToGid::Map(
                        b.as_chunks::<2>()
                            .0
                            .iter()
                            .map(|c| u16::from_be_bytes([c[0], c[1]]))
                            .collect(),
                    );
                }
            }
            _ => self.cid_to_gid = CidToGid::Identity,
        }
        let desc = doc.get_in(cf, b"FontDescriptor");
        let desc = desc.as_dict().cloned().unwrap_or_default();
        self.load_program(doc, &desc);
        if matches!(self.program, Program::None) {
            if let Some(fb) = &doc.fallback_font {
                self.program = Program::Fallback(fb.clone());
                self.matrix = Transform::from_scale(1.0 / fb.upem, 1.0 / fb.upem);
            }
        }
    }

    /// Split `s` into `(code, byte length)` pairs.
    pub fn codes(&self, s: &[u8]) -> Vec<(u32, usize)> {
        if !self.composite {
            return s.iter().map(|&b| (b as u32, 1)).collect();
        }
        let mut out = Vec::with_capacity(s.len() / 2);
        let mut i = 0;
        while i < s.len() {
            let n = match &self.encoding_cmap {
                Some(cm) if !cm.codespace.is_empty() => code_len(cm, &s[i..]),
                _ => 2,
            };
            let n = n.min(s.len() - i).max(1);
            let code = s[i..i + n].iter().fold(0u32, |a, &b| a << 8 | b as u32);
            out.push((code, n));
            i += n;
        }
        out
    }

    fn cid(&self, code: u32) -> u32 {
        match &self.encoding_cmap {
            Some(cm) => cm
                .ranges
                .iter()
                .find(|(lo, hi, _)| (*lo..=*hi).contains(&code))
                .map(|(lo, _, first)| first + (code - lo))
                .unwrap_or(code),
            None => code,
        }
    }

    /// Advance of `code` in text space (before font size).
    pub fn width(&self, code: u32) -> f32 {
        if self.composite {
            let cid = self.cid(code);
            let w = self
                .cid_widths
                .iter()
                .find(|(a, b, _)| (*a..=*b).contains(&cid))
                .map(|(_, _, w)| *w)
                .unwrap_or(self.default_width);
            return w / 1000.0;
        }
        let w = code
            .checked_sub(self.first_char)
            .and_then(|i| self.widths.get(i as usize))
            .copied();
        match w {
            Some(w) if self.type3.is_some() => self.matrix.sx * w,
            Some(w) => w / 1000.0,
            None => match self.program_advance(code) {
                Some(a) if self.type3.is_none() => a,
                _ => self.missing_width / 1000.0,
            },
        }
    }

    /// Whether a single-byte code 32 gets word spacing (simple fonts only).
    pub fn is_space(&self, code: u32, len: usize) -> bool {
        len == 1 && code == 32
    }

    pub fn glyph_name(&self, code: u32) -> Option<&[u8]> {
        self.names.get(code as usize)?.as_deref()
    }

    /// The outline of `code` in text space, cached.
    pub fn outline(&self, code: u32) -> Option<Rc<Path>> {
        if let Some(p) = self.cache.borrow().get(&code) {
            return p.clone();
        }
        let path = self.build_outline(code).map(Rc::new);
        self.cache.borrow_mut().insert(code, path.clone());
        path
    }

    fn build_outline(&self, code: u32) -> Option<Path> {
        let mut pb = PathSink(PathBuilder::new());
        let mut matrix = self.matrix;
        match &self.program {
            Program::Sfnt(data) => {
                let face = ttf_parser::Face::parse(data, 0).ok()?;
                let gid = self.sfnt_gid(&face, code)?;
                face.outline_glyph(gid, &mut pb)?;
            }
            Program::Cff(data) => {
                let t = ttf_parser::cff::Table::parse(data)?;
                let gid = self.cff_gid(&t, code)?;
                t.outline(gid, &mut pb).ok()?;
            }
            Program::Type1(t1) => {
                let name = self
                    .glyph_name(code)
                    .filter(|n| t1.has_glyph(n))
                    .unwrap_or(b".notdef");
                t1.outline(name, &mut pb)?;
            }
            Program::Fallback(fb) => {
                let face = ttf_parser::Face::parse(&fb.data, 0).ok()?;
                let ch = self.unicode(code)?;
                let gid = face.glyph_index(ch)?;
                face.outline_glyph(gid, &mut pb)?;
                // Stretch to the PDF's advance so the line keeps its length.
                let adv = face.glyph_hor_advance(gid).unwrap_or(0) as f32 / fb.upem;
                let want = self.width(code);
                if adv > 0.0 && want > 0.0 {
                    let sx = (want / adv).clamp(0.5, 2.0);
                    matrix = matrix.post_scale(sx, 1.0);
                }
            }
            Program::Type3 | Program::None => return None,
        }
        pb.0.finish()?.transform(matrix)
    }

    fn sfnt_gid(&self, face: &ttf_parser::Face, code: u32) -> Option<GlyphId> {
        if self.composite {
            let cid = self.cid(code);
            return Some(GlyphId(match &self.cid_to_gid {
                CidToGid::Identity => cid as u16,
                CidToGid::Map(m) => *m.get(cid as usize)?,
            }));
        }
        let cmap = face.tables().cmap?;
        let sub = |plat: ttf_parser::PlatformId, enc: u16, c: u32| {
            cmap.subtables
                .into_iter()
                .find(|s| s.platform_id == plat && s.encoding_id == enc)
                .and_then(|s| s.glyph_index(c))
        };
        use ttf_parser::PlatformId::{Macintosh, Windows};
        if !self.symbolic || self.explicit_encoding {
            if let Some(ch) = self.glyph_name(code).and_then(glyph_to_unicode) {
                if let Some(g) = face.glyph_index(ch) {
                    return Some(g);
                }
            }
        }
        sub(Windows, 0, 0xF000 + code)
            .or_else(|| sub(Windows, 0, code))
            .or_else(|| sub(Macintosh, 0, code))
            .or_else(|| face.glyph_index(char::from_u32(code)?))
            .or_else(|| cmap.subtables.into_iter().find_map(|s| s.glyph_index(code)))
    }

    fn cff_gid(&self, t: &ttf_parser::cff::Table, code: u32) -> Option<GlyphId> {
        if self.composite {
            let cid = self.cid(code);
            // CID-keyed: charset maps GID → CID; invert once.
            let mut map = self.cff_cid_map.borrow_mut();
            let map = map.get_or_insert_with(|| {
                (0..t.number_of_glyphs())
                    .filter_map(|g| Some((t.glyph_cid(GlyphId(g))?, g)))
                    .collect()
            });
            if map.is_empty() {
                return Some(GlyphId(cid as u16));
            }
            return map.get(&(cid as u16)).map(|&g| GlyphId(g));
        }
        if self.explicit_encoding || self.symbolic {
            if let Some(name) = self.glyph_name(code) {
                if let Ok(name) = core::str::from_utf8(name) {
                    if let Some(g) = t.glyph_index_by_name(name) {
                        return Some(g);
                    }
                }
            }
        }
        t.glyph_index(code as u8).or_else(|| {
            let name = encoding::STANDARD.get(code as usize).copied().flatten()?;
            t.glyph_index_by_name(core::str::from_utf8(name).ok()?)
        })
    }

    /// The font program's own advance for `code`, in text space.
    fn program_advance(&self, code: u32) -> Option<f32> {
        match &self.program {
            Program::Sfnt(data) => {
                let face = ttf_parser::Face::parse(data, 0).ok()?;
                let gid = self.sfnt_gid(&face, code)?;
                Some(face.glyph_hor_advance(gid)? as f32 * self.matrix.sx)
            }
            Program::Type1(t1) => {
                let name = self.glyph_name(code)?;
                let w = t1.outline(name, &mut NullSink)?;
                Some(w * self.matrix.sx)
            }
            Program::Fallback(fb) => {
                let face = ttf_parser::Face::parse(&fb.data, 0).ok()?;
                let gid = face.glyph_index(self.unicode(code)?)?;
                Some(face.glyph_hor_advance(gid)? as f32 / fb.upem)
            }
            _ => None,
        }
    }

    /// Best-effort Unicode for `code`: ToUnicode, then the glyph name.
    pub fn unicode(&self, code: u32) -> Option<char> {
        if let Some(tu) = &self.to_unicode {
            if let Some(s) = tu.text.get(&code) {
                return s.chars().next();
            }
            if let Some((lo, _, first)) = tu
                .ranges
                .iter()
                .find(|(lo, hi, _)| (*lo..=*hi).contains(&code))
            {
                return char::from_u32(first + (code - lo));
            }
        }
        if self.composite {
            return None;
        }
        if let Some(n) = self.glyph_name(code) {
            return glyph_to_unicode(n);
        }
        // Symbolic fonts without names: assume Latin-1.
        char::from_u32(code)
    }
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn floats(doc: &Document, o: &Object) -> Vec<f32> {
    match o {
        Object::Array(a) => a
            .iter()
            .map(|x| doc.resolve(x).ok().and_then(|v| v.as_f32()).unwrap_or(0.0))
            .collect(),
        _ => Vec::new(),
    }
}

fn code_len(cm: &CMap, s: &[u8]) -> usize {
    for n in 1..=4u8 {
        if s.len() < n as usize {
            break;
        }
        let code = s[..n as usize].iter().fold(0u32, |a, &b| a << 8 | b as u32);
        if cm
            .codespace
            .iter()
            .any(|&(lo, hi, len)| len == n && (lo..=hi).contains(&code))
        {
            return n as usize;
        }
    }
    cm.codespace.first().map(|c| c.2 as usize).unwrap_or(1)
}

fn bytes_to_u32(b: &[u8]) -> u32 {
    b.iter().take(4).fold(0u32, |a, &x| a << 8 | x as u32)
}

fn utf16be(b: &[u8]) -> String {
    let units = b
        .chunks(2)
        .map(|c| u16::from_be_bytes([c[0], *c.get(1).unwrap_or(&0)]));
    char::decode_utf16(units)
        .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

/// Parse the operators of a CMap we care about: `begincodespacerange`,
/// `begincidrange`/`begincidchar`, `beginbfrange`/`beginbfchar`.
fn parse_cmap(data: &[u8]) -> CMap {
    let mut cm = CMap::default();
    let mut lx = Lexer::new(data);
    let mut stack: Vec<Object> = Vec::new();
    let mut guard = 0u32;
    while let Some(tok) = lx.next_token() {
        guard += 1;
        if guard > 1_000_000 {
            break;
        }
        match tok {
            Token::Keyword(k) => {
                match k {
                    b"endcodespacerange" => {
                        for p in stack.as_chunks::<2>().0 {
                            if let (Some(lo), Some(hi)) = (p[0].as_string(), p[1].as_string()) {
                                cm.codespace.push((
                                    bytes_to_u32(lo),
                                    bytes_to_u32(hi),
                                    lo.len().clamp(1, 4) as u8,
                                ));
                            }
                        }
                    }
                    b"endcidrange" => {
                        for p in stack.as_chunks::<3>().0 {
                            if let (Some(lo), Some(hi), Some(c)) =
                                (p[0].as_string(), p[1].as_string(), p[2].as_i64())
                            {
                                cm.ranges.push((
                                    bytes_to_u32(lo),
                                    bytes_to_u32(hi),
                                    c.max(0) as u32,
                                ));
                            }
                        }
                    }
                    b"endcidchar" => {
                        for p in stack.as_chunks::<2>().0 {
                            if let (Some(c), Some(v)) = (p[0].as_string(), p[1].as_i64()) {
                                let c = bytes_to_u32(c);
                                cm.ranges.push((c, c, v.max(0) as u32));
                            }
                        }
                    }
                    b"endbfchar" => {
                        for p in stack.as_chunks::<2>().0 {
                            if let (Some(c), Some(v)) = (p[0].as_string(), p[1].as_string()) {
                                cm.text.insert(bytes_to_u32(c), utf16be(v));
                            }
                        }
                    }
                    b"endbfrange" => {
                        for p in stack.as_chunks::<3>().0 {
                            let (Some(lo), Some(hi)) = (p[0].as_string(), p[1].as_string()) else {
                                continue;
                            };
                            let (lo, hi) = (bytes_to_u32(lo), bytes_to_u32(hi));
                            match &p[2] {
                                Object::String(v) => {
                                    let s = utf16be(v);
                                    let mut chars = s.chars();
                                    if let (Some(c), None) = (chars.next(), chars.next()) {
                                        cm.ranges.push((lo, hi, c as u32));
                                    }
                                }
                                Object::Array(a) => {
                                    for (i, o) in a.iter().enumerate() {
                                        if let Some(v) = o.as_string() {
                                            cm.text.insert(lo + i as u32, utf16be(v));
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
                stack.clear();
            }
            t => {
                if let Ok(o) = lx.object_from(t, false, 0) {
                    if stack.len() < 300 {
                        stack.push(o);
                    }
                }
            }
        }
    }
    cm
}

/// A PDF text string (UTF-16BE with BOM, or PDFDocEncoding ≈ Latin-1).
pub fn decode_text_string(b: &[u8]) -> String {
    if b.starts_with(&[0xFE, 0xFF]) {
        return utf16be(&b[2..]);
    }
    if let Some(rest) = b.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    b.iter().map(|&c| c as char).collect()
}

struct PathSink(PathBuilder);

impl OutlineBuilder for PathSink {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.move_to(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.line_to(x, y);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.0.quad_to(x1, y1, x, y);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.0.cubic_to(x1, y1, x2, y2, x, y);
    }
    fn close(&mut self) {
        self.0.close();
    }
}

impl Sink for PathSink {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.move_to(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.line_to(x, y);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.0.cubic_to(x1, y1, x2, y2, x, y);
    }
    fn close(&mut self) {
        self.0.close();
    }
}

struct NullSink;

impl Sink for NullSink {
    fn move_to(&mut self, _: f32, _: f32) {}
    fn line_to(&mut self, _: f32, _: f32) {}
    fn curve_to(&mut self, _: f32, _: f32, _: f32, _: f32, _: f32, _: f32) {}
    fn close(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmap_parses_ranges_and_chars() {
        let src = b"1 begincodespacerange <0000> <FFFF> endcodespacerange
            2 beginbfchar <0003> <0020> <0011> <00660069> endbfchar
            1 beginbfrange <0024> <0026> <0041> endbfrange";
        let cm = parse_cmap(src);
        assert_eq!(cm.codespace, [(0, 0xFFFF, 2)]);
        assert_eq!(cm.text.get(&3).map(String::as_str), Some(" "));
        assert_eq!(cm.text.get(&0x11).map(String::as_str), Some("fi"));
        assert_eq!(cm.ranges, [(0x24, 0x26, 0x41)]);
    }

    #[test]
    fn text_strings() {
        assert_eq!(decode_text_string(b"\xfe\xff\x00H\x00i"), "Hi");
        assert_eq!(decode_text_string(b"caf\xe9"), "café");
    }
}
