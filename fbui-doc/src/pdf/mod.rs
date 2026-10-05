//! A PDF subset renderer for `no_std` targets.
//!
//! Scope (what a document viewer on a small device needs, and no more):
//!
//! * **File structure** — classic xref tables and PDF 1.5 xref streams,
//!   incremental updates (`/Prev` chains, hybrid `/XRefStm`), object streams,
//!   and a reconstructing fallback that scans for `n g obj` when the xref is
//!   damaged.
//! * **Filters** — Flate and LZW with predictors, ASCIIHex, ASCII85,
//!   RunLength; DCT (JPEG) images via zune-jpeg.
//! * **Graphics** — the full path and painting operator set, the graphics
//!   state (line width/cap/join/dash, `ExtGState` alpha), clipping, form
//!   XObjects, image XObjects and inline images (1–16 bpc, Gray/RGB/CMYK/
//!   Indexed/ICC/Separation, `Decode`, stencil masks, `SMask`), and axial /
//!   radial shadings (`sh` and shading patterns).
//! * **Text** — all text operators and render modes; embedded TrueType,
//!   OpenType and CFF (Type1C, CID-keyed) via ttf-parser, embedded Type 1
//!   via a built-in charstring interpreter, Type 3 glyph procedures, Type 0
//!   composite fonts with `Identity-H` and embedded CMaps. Non-embedded fonts
//!   (the standard 14) draw with a caller-supplied fallback face, stretched
//!   to the PDF's glyph widths so lines keep their length.
//!
//! Out of scope, documented in NOSTD.md: encryption, JBIG2/JPX/CCITT images
//! (drawn as a neutral placeholder), blend modes and soft-mask groups,
//! tiling patterns, shading types 4–7, and colour management beyond the
//! device spaces.
//!
//! Every input is treated as hostile: malformed files produce an [`Error`] or
//! a partially rendered page, never a panic, and recursion and decoded sizes
//! are bounded.

mod color;
mod content;
mod encoding;
mod filter;
mod font;
mod function;
mod image;
mod lexer;
mod object;
mod render;
mod type1;

use alloc::borrow::Cow;
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::fmt;

pub use object::{Dict, Object, Ref, Stream, StreamData};
pub use render::RenderOptions;

use lexer::{find, rfind, Lexer, Token};

/// Why a document or page could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Not well-formed where it mattered.
    Syntax(&'static str),
    /// Structurally broken (bad xref, undecodable stream).
    Corrupt(&'static str),
    /// Valid PDF using a feature outside this subset.
    Unsupported(&'static str),
    /// The document is encrypted.
    Encrypted,
    /// No page tree, or an empty one.
    NoPages,
    /// Page index past the end.
    PageOutOfRange,
    /// The requested raster would exceed [`RenderOptions::max_pixels`].
    TooLarge,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Syntax(s) => write!(f, "pdf syntax error: {s}"),
            Error::Corrupt(s) => write!(f, "corrupt pdf: {s}"),
            Error::Unsupported(s) => write!(f, "unsupported pdf feature: {s}"),
            Error::Encrypted => f.write_str("encrypted pdfs are not supported"),
            Error::NoPages => f.write_str("pdf has no pages"),
            Error::PageOutOfRange => f.write_str("page index out of range"),
            Error::TooLarge => f.write_str("page raster too large"),
        }
    }
}

impl core::error::Error for Error {}

pub type Result<T> = core::result::Result<T, Error>;

/// How deep reference chains / page trees / forms may nest.
const MAX_RESOLVE_DEPTH: u32 = 32;

/// The parsed members of one object stream.
type ObjStm = Vec<(u32, Object)>;

#[derive(Debug, Clone, Copy)]
enum XrefEntry {
    /// Byte offset of `n g obj` in the file.
    Offset(usize),
    /// Index inside object stream `stream`.
    InStream { stream: u32 },
}

/// One page, with inherited attributes already resolved.
#[derive(Debug, Clone)]
pub struct Page {
    pub(crate) dict: Dict,
    pub(crate) resources: Dict,
    /// Visible area in default user space (points), `[x0, y0, x1, y1]`.
    pub crop: [f32; 4],
    /// Clockwise rotation in degrees: 0, 90, 180 or 270.
    pub rotate: u32,
}

impl Page {
    /// Display size in points (1/72 in), after rotation.
    pub fn size(&self) -> (f32, f32) {
        let w = self.crop[2] - self.crop[0];
        let h = self.crop[3] - self.crop[1];
        if self.rotate % 180 == 90 {
            (h, w)
        } else {
            (w, h)
        }
    }
}

/// A parsed PDF document.
pub struct Document {
    data: Cow<'static, [u8]>,
    xref: BTreeMap<u32, XrefEntry>,
    trailer: Dict,
    pages: Vec<Page>,
    cache: RefCell<BTreeMap<u32, Object>>,
    objstm: RefCell<BTreeMap<u32, Rc<ObjStm>>>,
    pub(crate) fonts: RefCell<BTreeMap<Ref, Rc<font::Font>>>,
    pub(crate) fallback_font: Option<Rc<font::FallbackFace>>,
    reconstructed: bool,
}

impl Document {
    /// Parse a document from bytes. `&'static` input (an `include_bytes!`, a
    /// memory-mapped flash region) is borrowed, not copied.
    pub fn parse(data: impl Into<Cow<'static, [u8]>>) -> Result<Document> {
        let data = data.into();
        if find(&data[..data.len().min(1024)], b"%PDF", 0).is_none() {
            return Err(Error::Syntax("missing %PDF header"));
        }
        let mut doc = Document {
            data,
            xref: BTreeMap::new(),
            trailer: Dict::default(),
            pages: Vec::new(),
            cache: RefCell::new(BTreeMap::new()),
            objstm: RefCell::new(BTreeMap::new()),
            fonts: RefCell::new(BTreeMap::new()),
            fallback_font: None,
            reconstructed: false,
        };
        if doc.load_xref().is_err() || doc.trailer.get(b"Root").is_none() {
            doc.reconstruct();
        }
        if doc.trailer.contains(b"Encrypt") {
            return Err(Error::Encrypted);
        }
        if doc.load_pages().is_err() || doc.pages.is_empty() {
            if !doc.reconstructed {
                doc.reconstruct();
                doc.pages.clear();
                let _ = doc.load_pages();
            }
            if doc.pages.is_empty() {
                return Err(Error::NoPages);
            }
        }
        Ok(doc)
    }

    /// Set the face used for fonts the PDF doesn't embed (the standard 14 —
    /// Helvetica, Times, Courier — and any it references by name only).
    /// Without one, such text is not drawn. Any TrueType/OpenType face works;
    /// glyphs are stretched to the PDF's widths so text keeps its layout.
    pub fn set_fallback_font(&mut self, ttf: impl Into<Cow<'static, [u8]>>) {
        self.fallback_font = font::FallbackFace::new(ttf.into()).map(Rc::new);
        self.fonts.borrow_mut().clear();
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    pub fn page(&self, index: usize) -> Result<&Page> {
        self.pages.get(index).ok_or(Error::PageOutOfRange)
    }

    /// The document title from the info dictionary, if it's plain text.
    pub fn title(&self) -> Option<alloc::string::String> {
        let info = self.resolve(self.trailer.get(b"Info")?).ok()?;
        let t = self.resolve(info.as_dict()?.get(b"Title")?).ok()?;
        let bytes = t.as_string()?;
        let s = font::decode_text_string(bytes);
        (!s.is_empty()).then_some(s)
    }

    /// Render page `index` at `zoom` pixels per point (1.0 = 72 dpi).
    pub fn render_page(&self, index: usize, opts: &RenderOptions) -> Result<tiny_skia::Pixmap> {
        let page = self.page(index)?;
        render::render_page(self, page, opts)
    }

    /// Follow `obj` if it's a reference; otherwise return it as is.
    pub fn resolve(&self, obj: &Object) -> Result<Object> {
        let mut cur = obj.clone();
        for _ in 0..MAX_RESOLVE_DEPTH {
            match cur {
                Object::Ref(r) => cur = self.get(r)?,
                other => return Ok(other),
            }
        }
        Err(Error::Corrupt("reference cycle"))
    }

    /// Resolve `dict[key]`, `Null` if absent.
    pub fn get_in(&self, dict: &Dict, key: &[u8]) -> Object {
        match dict.get(key) {
            Some(o) => self.resolve(o).unwrap_or(Object::Null),
            None => Object::Null,
        }
    }

    /// Fetch indirect object `r` (`Null` if it doesn't exist, per the spec).
    pub fn get(&self, r: Ref) -> Result<Object> {
        if let Some(o) = self.cache.borrow().get(&r.num) {
            return Ok(o.clone());
        }
        let obj = match self.xref.get(&r.num) {
            None | Some(XrefEntry::Offset(usize::MAX)) => Object::Null,
            Some(XrefEntry::Offset(off)) => self.parse_indirect_at(*off, 0)?,
            Some(XrefEntry::InStream { stream }) => self.in_objstm(*stream, r.num)?,
        };
        self.cache.borrow_mut().insert(r.num, obj.clone());
        Ok(obj)
    }

    /// A stream's decoded bytes (byte filters only; an image codec is left
    /// for the image path, see [`filter::Decoded`]).
    pub(crate) fn decode_stream(&self, s: &Stream) -> Result<filter::Decoded> {
        let raw: &[u8] = match &s.data {
            StreamData::File(r) => self
                .data
                .get(r.clone())
                .ok_or(Error::Corrupt("stream range"))?,
            StreamData::Owned(v) => v,
        };
        let (filters, parms) = self.filters_of(&s.dict);
        filter::decode(raw, &filters, &parms)
    }

    /// A stream's fully decoded bytes; errors if an image codec remains.
    pub(crate) fn stream_bytes(&self, s: &Stream) -> Result<Vec<u8>> {
        let d = self.decode_stream(s)?;
        if d.image_filter.is_some() {
            return Err(Error::Unsupported("image codec in a data stream"));
        }
        Ok(d.data)
    }

    fn filters_of(&self, dict: &Dict) -> (Vec<Vec<u8>>, Vec<Option<Dict>>) {
        let key_f = if dict.contains(b"Filter") {
            &b"Filter"[..]
        } else {
            b"F"
        };
        let key_p = if dict.contains(b"DecodeParms") {
            &b"DecodeParms"[..]
        } else {
            b"DP"
        };
        let filters = match self.get_in(dict, key_f) {
            Object::Name(n) => alloc::vec![n],
            Object::Array(a) => a
                .iter()
                .filter_map(|o| self.resolve(o).ok()?.as_name().map(<[u8]>::to_vec))
                .collect(),
            _ => Vec::new(),
        };
        let parms = match self.get_in(dict, key_p) {
            Object::Dict(d) => alloc::vec![Some(d)],
            Object::Array(a) => a
                .iter()
                .map(|o| match self.resolve(o) {
                    Ok(Object::Dict(d)) => Some(d),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        (filters, parms)
    }

    // ---- file structure -------------------------------------------------

    fn parse_indirect_at(&self, off: usize, depth: u32) -> Result<Object> {
        let data: &[u8] = &self.data;
        let mut lx = Lexer::at(data, off);
        match (lx.next_token(), lx.next_token(), lx.next_token()) {
            (Some(Token::Int(_)), Some(Token::Int(_)), Some(Token::Keyword(b"obj"))) => {}
            _ => return Err(Error::Corrupt("xref offset does not point at an object")),
        }
        let obj = lx.parse_object(true)?;
        let Object::Dict(dict) = obj else {
            return Ok(obj);
        };
        if lx.next_token() != Some(Token::Keyword(b"stream")) {
            return Ok(Object::Dict(dict));
        }
        lx.skip_eol();
        let start = lx.pos;
        let declared = match dict.get(b"Length") {
            Some(Object::Int(n)) => Some(*n),
            Some(Object::Ref(r)) if depth < 4 => self.length_from_ref(*r, depth),
            _ => None,
        };
        let end = match declared {
            Some(n) if n >= 0 && endstream_follows(data, start + n as usize) => start + n as usize,
            _ => {
                // Length missing or wrong: find `endstream` and trim the EOL.
                let e =
                    find(data, b"endstream", start).ok_or(Error::Corrupt("unterminated stream"))?;
                let mut e2 = e;
                if e2 > start && data[e2 - 1] == b'\n' {
                    e2 -= 1;
                }
                if e2 > start && data[e2 - 1] == b'\r' {
                    e2 -= 1;
                }
                e2
            }
        };
        Ok(Object::Stream(Stream {
            dict,
            data: StreamData::File(start..end),
        }))
    }

    fn length_from_ref(&self, r: Ref, depth: u32) -> Option<i64> {
        match self.xref.get(&r.num)? {
            XrefEntry::Offset(off) => self.parse_indirect_at(*off, depth + 1).ok()?.as_i64(),
            XrefEntry::InStream { stream } => self.in_objstm(*stream, r.num).ok()?.as_i64(),
        }
    }

    fn in_objstm(&self, stream: u32, num: u32) -> Result<Object> {
        if let Some(objs) = self.objstm.borrow().get(&stream).cloned() {
            return Ok(lookup(&objs, num));
        }
        let objs = Rc::new(self.load_objstm(stream)?);
        self.objstm.borrow_mut().insert(stream, objs.clone());
        Ok(lookup(&objs, num))
    }

    fn load_objstm(&self, stream: u32) -> Result<Vec<(u32, Object)>> {
        let Some(XrefEntry::Offset(off)) = self.xref.get(&stream).copied() else {
            return Err(Error::Corrupt("object stream not in file"));
        };
        let obj = self.parse_indirect_at(off, 0)?;
        let s = obj.as_stream().ok_or(Error::Corrupt("object stream"))?;
        parse_objstm(&self.stream_bytes(s)?, &s.dict)
    }

    fn load_xref(&mut self) -> Result<()> {
        let data: &[u8] = &self.data;
        let tail = data.len().saturating_sub(4096);
        let sx = rfind(&data[tail..], b"startxref").ok_or(Error::Corrupt("no startxref"))? + tail;
        let mut lx = Lexer::at(data, sx + 9);
        let Some(Token::Int(mut off)) = lx.next_token() else {
            return Err(Error::Corrupt("bad startxref"));
        };
        let mut entries = BTreeMap::new();
        let mut trailer: Option<Dict> = None;
        let mut seen = Vec::new();
        loop {
            if off < 0 || off as usize >= data.len() || seen.contains(&off) || seen.len() > 64 {
                break;
            }
            seen.push(off);
            let t = self.read_xref_section(off as usize, &mut entries)?;
            if let Some(Object::Int(x)) = t.get(b"XRefStm") {
                let _ = self.read_xref_section(*x as usize, &mut entries);
            }
            let prev = t.get(b"Prev").and_then(Object::as_i64);
            match &mut trailer {
                None => trailer = Some(t),
                Some(tr) => {
                    for (k, v) in t.0 {
                        if !tr.contains(&k) {
                            tr.0.push((k, v));
                        }
                    }
                }
            }
            match prev {
                Some(p) => off = p,
                None => break,
            }
        }
        self.xref = entries;
        self.trailer = trailer.ok_or(Error::Corrupt("no trailer"))?;
        Ok(())
    }

    /// Read one xref section (table or stream) at `off`, adding entries not
    /// already present (newer sections were read first). Returns its trailer.
    fn read_xref_section(
        &self,
        off: usize,
        entries: &mut BTreeMap<u32, XrefEntry>,
    ) -> Result<Dict> {
        let data: &[u8] = &self.data;
        let mut lx = Lexer::at(data, off);
        lx.skip_ws();
        if data[lx.pos..].starts_with(b"xref") {
            lx.pos += 4;
            loop {
                let save = lx.pos;
                match (lx.next_token(), lx.next_token()) {
                    (Some(Token::Int(start)), Some(Token::Int(count))) => {
                        if start < 0 || !(0..=10_000_000).contains(&count) {
                            return Err(Error::Corrupt("xref subsection"));
                        }
                        for i in 0..count {
                            let (o, _g, kind) =
                                match (lx.next_token(), lx.next_token(), lx.next_token()) {
                                    (
                                        Some(Token::Int(o)),
                                        Some(Token::Int(g)),
                                        Some(Token::Keyword(k)),
                                    ) => (o, g, k),
                                    _ => return Err(Error::Corrupt("xref entry")),
                                };
                            let num = (start + i) as u32;
                            if kind == b"n" && o > 0 {
                                entries.entry(num).or_insert(XrefEntry::Offset(o as usize));
                            } else {
                                // Free entries still shadow older sections.
                                entries.entry(num).or_insert(XrefEntry::Offset(usize::MAX));
                            }
                        }
                    }
                    _ => {
                        lx.pos = save;
                        break;
                    }
                }
            }
            if lx.next_token() != Some(Token::Keyword(b"trailer")) {
                return Err(Error::Corrupt("missing trailer"));
            }
            return match lx.parse_object(true)? {
                Object::Dict(d) => Ok(d),
                _ => Err(Error::Corrupt("trailer is not a dict")),
            };
        }
        // An xref stream.
        let obj = self.parse_indirect_at(off, 0)?;
        let s = obj
            .as_stream()
            .ok_or(Error::Corrupt("xref is neither table nor stream"))?;
        let bytes = self.stream_bytes(s)?;
        let w: Vec<usize> = s
            .dict
            .get(b"W")
            .and_then(Object::as_array)
            .map(|a| {
                a.iter()
                    .map(|o| o.as_i64().unwrap_or(0).clamp(0, 8) as usize)
                    .collect()
            })
            .unwrap_or_default();
        if w.len() < 3 {
            return Err(Error::Corrupt("xref stream /W"));
        }
        let size = s.dict.get(b"Size").and_then(Object::as_i64).unwrap_or(0);
        let index: Vec<i64> = match s.dict.get(b"Index").and_then(Object::as_array) {
            Some(a) => a.iter().filter_map(Object::as_i64).collect(),
            None => alloc::vec![0, size],
        };
        let row = w[0] + w[1] + w[2];
        if row == 0 {
            return Err(Error::Corrupt("xref stream /W"));
        }
        let mut rows = bytes.chunks_exact(row);
        for pair in index.chunks_exact(2) {
            let (start, count) = (pair[0], pair[1]);
            for i in 0..count.max(0) {
                let Some(r) = rows.next() else { break };
                let f = |a: usize, n: usize| {
                    r[a..a + n].iter().fold(0u64, |acc, &b| acc << 8 | b as u64)
                };
                let ty = if w[0] == 0 { 1 } else { f(0, w[0]) };
                let f2 = f(w[0], w[1]);
                let num = (start + i) as u32;
                let e = match ty {
                    1 => XrefEntry::Offset(f2 as usize),
                    2 => XrefEntry::InStream { stream: f2 as u32 },
                    _ => XrefEntry::Offset(usize::MAX),
                };
                entries.entry(num).or_insert(e);
            }
        }
        Ok(s.dict.clone())
    }

    /// Rebuild the xref by scanning the file for `n g obj` headers.
    fn reconstruct(&mut self) {
        self.reconstructed = true;
        self.cache.borrow_mut().clear();
        self.objstm.borrow_mut().clear();
        let data: &[u8] = &self.data;
        let mut entries = BTreeMap::new();
        let mut i = 0;
        while let Some(p) = find(data, b"obj", i) {
            i = p + 3;
            // Walk back over `n g ` before the keyword.
            if let Some((num, start)) = obj_header_before(data, p) {
                let after = data.get(p + 3).copied().unwrap_or(b' ');
                if lexer::is_white(after) || lexer::is_delim(after) {
                    entries.insert(num, XrefEntry::Offset(start));
                }
            }
        }
        self.xref = entries;
        // Object streams: index their members too.
        let nums: Vec<u32> = self.xref.keys().copied().collect();
        let mut trailer = Dict::default();
        for num in nums {
            let Ok(obj) = self.get(Ref { num, gen: 0 }) else {
                continue;
            };
            let Some(d) = obj.as_dict() else { continue };
            match d.name(b"Type") {
                Some(b"ObjStm") => {
                    if let Ok(objs) = self.load_objstm(num) {
                        for (n, _) in objs.iter() {
                            self.xref
                                .entry(*n)
                                .or_insert(XrefEntry::InStream { stream: num });
                        }
                    }
                }
                Some(b"XRef") => {
                    for k in [&b"Root"[..], b"Info", b"Encrypt"] {
                        if let Some(v) = d.get(k) {
                            trailer.insert(k, v.clone());
                        }
                    }
                }
                Some(b"Catalog") if !trailer.contains(b"Root") => {
                    trailer.insert(b"Root", Object::Ref(Ref { num, gen: 0 }));
                }
                _ => {}
            }
        }
        // A classic trailer dict wins where present.
        let data: &[u8] = &self.data;
        if let Some(t) = rfind(data, b"trailer") {
            if let Ok(Object::Dict(d)) = Lexer::at(data, t + 7).parse_object(true) {
                for (k, v) in d.0 {
                    trailer.insert(&k, v);
                }
            }
        }
        self.cache.borrow_mut().clear();
        self.trailer = trailer;
    }

    fn load_pages(&mut self) -> Result<()> {
        let root = self.get_in(&self.trailer.clone(), b"Root");
        let root = root.as_dict().ok_or(Error::NoPages)?;
        let pages_ref = root.get(b"Pages").cloned().ok_or(Error::NoPages)?;
        let mut out = Vec::new();
        self.walk_pages(&pages_ref, None, None, None, 0, &mut Vec::new(), &mut out)?;
        self.pages = out;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn walk_pages(
        &self,
        node: &Object,
        resources: Option<&Dict>,
        media: Option<[f32; 4]>,
        rotate: Option<i64>,
        depth: u32,
        visited: &mut Vec<u32>,
        out: &mut Vec<Page>,
    ) -> Result<()> {
        if depth > MAX_RESOLVE_DEPTH || out.len() > 100_000 {
            return Err(Error::Corrupt("page tree too deep"));
        }
        if let Object::Ref(r) = node {
            if visited.contains(&r.num) {
                return Ok(());
            }
            visited.push(r.num);
        }
        let obj = self.resolve(node)?;
        let Some(dict) = obj.as_dict() else {
            return Ok(());
        };
        let res = match self.get_in(dict, b"Resources") {
            Object::Dict(d) => Some(d),
            _ => resources.cloned(),
        };
        let media = self.rect(dict, b"MediaBox").or(media);
        let rotate = self.get_in(dict, b"Rotate").as_i64().or(rotate);
        let kids = self.get_in(dict, b"Kids");
        let is_page =
            dict.name(b"Type") == Some(b"Page") || (kids.is_null() && dict.contains(b"Contents"));
        if is_page {
            let media = media.unwrap_or([0.0, 0.0, 612.0, 792.0]);
            let crop = self
                .rect(dict, b"CropBox")
                .map(|c| intersect(c, media))
                .filter(|c| c[2] > c[0] && c[3] > c[1])
                .unwrap_or(media);
            out.push(Page {
                dict: dict.clone(),
                resources: res.unwrap_or_default(),
                crop,
                rotate: (rotate.unwrap_or(0).rem_euclid(360) / 90 * 90) as u32,
            });
            return Ok(());
        }
        if let Some(kids) = kids.as_array() {
            for k in kids {
                self.walk_pages(k, res.as_ref(), media, rotate, depth + 1, visited, out)?;
            }
        }
        Ok(())
    }

    /// A normalized rectangle `[x0 y0 x1 y1]` from `dict[key]`.
    pub(crate) fn rect(&self, dict: &Dict, key: &[u8]) -> Option<[f32; 4]> {
        let arr = self.get_in(dict, key);
        let a = arr.as_array()?;
        if a.len() < 4 {
            return None;
        }
        let mut v = [0.0f32; 4];
        for (slot, o) in v.iter_mut().zip(a) {
            *slot = self.resolve(o).ok()?.as_f32()?;
        }
        let r = [
            v[0].min(v[2]),
            v[1].min(v[3]),
            v[0].max(v[2]),
            v[1].max(v[3]),
        ];
        (r[2] > r[0] && r[3] > r[1]).then_some(r)
    }
}

fn intersect(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [
        a[0].max(b[0]),
        a[1].max(b[1]),
        a[2].min(b[2]),
        a[3].min(b[3]),
    ]
}

fn lookup(objs: &[(u32, Object)], num: u32) -> Object {
    objs.iter()
        .find(|(n, _)| *n == num)
        .map(|(_, o)| o.clone())
        .unwrap_or(Object::Null)
}

fn parse_objstm(bytes: &[u8], dict: &Dict) -> Result<Vec<(u32, Object)>> {
    let n = dict
        .get(b"N")
        .and_then(Object::as_i64)
        .unwrap_or(0)
        .clamp(0, 1_000_000) as usize;
    let first = dict
        .get(b"First")
        .and_then(Object::as_i64)
        .unwrap_or(0)
        .max(0) as usize;
    let mut lx = Lexer::new(bytes);
    let mut header = Vec::with_capacity(n);
    for _ in 0..n {
        match (lx.next_token(), lx.next_token()) {
            (Some(Token::Int(num)), Some(Token::Int(off))) if num >= 0 && off >= 0 => {
                header.push((num as u32, off as usize))
            }
            _ => break,
        }
    }
    let mut out = Vec::with_capacity(header.len());
    for (num, off) in header {
        let mut lx = Lexer::at(bytes, first + off);
        if let Ok(o) = lx.parse_object(true) {
            out.push((num, o));
        }
    }
    Ok(out)
}

fn endstream_follows(data: &[u8], at: usize) -> bool {
    let mut i = at;
    while i < data.len() && lexer::is_white(data[i]) {
        i += 1;
    }
    data[i.min(data.len())..].starts_with(b"endstream")
}

/// Given the offset of `obj`, parse a preceding `num gen ` and return the
/// object number and where the header starts.
fn obj_header_before(data: &[u8], obj_kw: usize) -> Option<(u32, usize)> {
    let mut i = obj_kw;
    let skip_ws = |i: &mut usize| {
        while *i > 0 && lexer::is_white(data[*i - 1]) {
            *i -= 1;
        }
    };
    let digits = |i: &mut usize| -> Option<usize> {
        let end = *i;
        while *i > 0 && data[*i - 1].is_ascii_digit() {
            *i -= 1;
        }
        (end > *i).then_some(end)
    };
    skip_ws(&mut i);
    digits(&mut i)?;
    let gen_start = i;
    if gen_start == obj_kw {
        return None;
    }
    skip_ws(&mut i);
    if i == gen_start {
        return None;
    }
    let num_end = digits(&mut i)?;
    let num: u32 = core::str::from_utf8(&data[i..num_end]).ok()?.parse().ok()?;
    // Must start a token.
    if i > 0 && !lexer::is_white(data[i - 1]) && !lexer::is_delim(data[i - 1]) {
        return None;
    }
    Some((num, i))
}
