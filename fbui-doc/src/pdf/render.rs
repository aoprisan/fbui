//! The content-stream interpreter: graphics state, paths, text, images and
//! shadings, painted with tiny-skia.

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::vec::Vec;

use tiny_skia::{
    FillRule, FilterQuality, GradientStop, LineCap, LineJoin, Mask, Paint, Path, PathBuilder,
    Pixmap, PixmapPaint, Shader, SpreadMode, Stroke, StrokeDash, Transform,
};

use super::color::ColorSpace;
use super::content::{ContentParser, Op};
use super::filter::Decoded;
use super::font::Font;
use super::function::Function;
use super::image::{self, DecodedImage};
use super::object::{Dict, Object, Ref, StreamData};
use super::{Document, Error, Page, Result};
// Unused when another crate in the graph links std (its inherent f32
// methods then win); needed on a pure no_std build.
#[allow(unused_imports)]
use crate::math::F32Ext;

/// How to rasterize a page.
#[derive(Debug, Clone)]
pub struct RenderOptions {
    /// Pixels per PDF point (1.0 = 72 dpi; 1.5 ≈ 108 dpi).
    pub zoom: f32,
    /// Refuse page rasters (and skip images) bigger than this many pixels —
    /// the knob that keeps a hostile or huge page inside a small heap.
    pub max_pixels: u64,
    /// Paper colour (straight RGBA).
    pub background: [u8; 4],
    /// Draw annotation appearance streams (form fields, stamps, …).
    pub annotations: bool,
}

impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions {
            zoom: 1.0,
            max_pixels: 16 * 1024 * 1024,
            background: [255, 255, 255, 255],
            annotations: true,
        }
    }
}

/// Nesting bound for forms / Type 3 glyphs / patterns.
const MAX_FORM_DEPTH: u32 = 12;
/// Graphics-state stack bound (`q` without `Q`).
const MAX_GSTATE: usize = 128;

pub fn render_page(doc: &Document, page: &Page, opts: &RenderOptions) -> Result<Pixmap> {
    let zoom = if opts.zoom.is_finite() && opts.zoom > 0.0 {
        opts.zoom
    } else {
        1.0
    };
    let (w_pt, h_pt) = page.size();
    let w = (w_pt * zoom).ceil().max(1.0);
    let h = (h_pt * zoom).ceil().max(1.0);
    if (w as u64) * (h as u64) > opts.max_pixels || w > 30_000.0 || h > 30_000.0 {
        return Err(Error::TooLarge);
    }
    let mut pixmap = Pixmap::new(w as u32, h as u32).ok_or(Error::TooLarge)?;
    let [r, g, b, a] = opts.background;
    pixmap.fill(tiny_skia::Color::from_rgba8(r, g, b, a));

    let base = page_transform(page, zoom);
    let mut it = Interp::new(doc, &mut pixmap, opts, base);
    let content = page_content(doc, &page.dict);
    it.run(&content, &page.resources, 0);

    if opts.annotations {
        it.annotations(&page.dict, base);
    }
    Ok(pixmap)
}

/// Default user space → device pixels: crop offset, y flip, page rotation
/// and zoom.
fn page_transform(page: &Page, zoom: f32) -> Transform {
    let [x0, y0, x1, y1] = page.crop;
    let (cw, ch) = (x1 - x0, y1 - y0);
    // Unrotated: x' = x - x0, y' = y1 - y.
    let flip = Transform::from_row(1.0, 0.0, 0.0, -1.0, -x0, y1);
    let rot = match page.rotate {
        90 => Transform::from_row(0.0, 1.0, -1.0, 0.0, ch, 0.0),
        180 => Transform::from_row(-1.0, 0.0, 0.0, -1.0, cw, ch),
        270 => Transform::from_row(0.0, -1.0, 1.0, 0.0, 0.0, cw),
        _ => Transform::identity(),
    };
    flip.post_concat(rot).post_scale(zoom, zoom)
}

fn page_content(doc: &Document, page: &Dict) -> Vec<u8> {
    let mut out = Vec::new();
    let mut push = |o: &Object| {
        if let Ok(Object::Stream(s)) = doc.resolve(o) {
            if let Ok(b) = doc.stream_bytes(&s) {
                out.extend_from_slice(&b);
                out.push(b'\n');
            }
        }
    };
    if let Some(o) = page.get(b"Contents") {
        match doc.resolve(o) {
            Ok(Object::Array(a)) => a.iter().for_each(&mut push),
            _ => push(o),
        }
    }
    out
}

/// What a fill or stroke paints with. A tiling pattern owns its cell
/// pixmap, which tiny-skia's pattern shader borrows — so the `Paint` is made
/// on demand from a value the caller holds.
enum PaintSrc {
    Shader(Shader<'static>),
    Tile {
        cell: Pixmap,
        ts: Transform,
        alpha: f32,
    },
}

impl PaintSrc {
    fn paint(&self) -> Paint<'_> {
        let shader = match self {
            PaintSrc::Shader(s) => s.clone(),
            PaintSrc::Tile { cell, ts, alpha } => tiny_skia::Pattern::new(
                cell.as_ref(),
                SpreadMode::Repeat,
                FilterQuality::Bilinear,
                *alpha,
                *ts,
            ),
        };
        Paint {
            shader,
            anti_alias: true,
            ..Paint::default()
        }
    }
}

#[derive(Clone)]
enum Fill {
    Solid([f32; 3]),
    /// A pattern resource, resolved at paint time.
    Pattern(Object),
}

#[derive(Clone)]
struct GState {
    ctm: Transform,
    fill_cs: ColorSpace,
    stroke_cs: ColorSpace,
    fill: Fill,
    stroke: Fill,
    fill_alpha: f32,
    stroke_alpha: f32,
    line_width: f32,
    cap: LineCap,
    join: LineJoin,
    miter: f32,
    dash: Option<(Vec<f32>, f32)>,
    clip: Option<Rc<Mask>>,
    font: Option<Rc<Font>>,
    font_size: f32,
    char_spacing: f32,
    word_spacing: f32,
    hscale: f32,
    leading: f32,
    rise: f32,
    render_mode: u8,
}

impl GState {
    fn new(ctm: Transform) -> Self {
        GState {
            ctm,
            fill_cs: ColorSpace::Gray,
            stroke_cs: ColorSpace::Gray,
            fill: Fill::Solid([0.0; 3]),
            stroke: Fill::Solid([0.0; 3]),
            fill_alpha: 1.0,
            stroke_alpha: 1.0,
            line_width: 1.0,
            cap: LineCap::Butt,
            join: LineJoin::Miter,
            miter: 10.0,
            dash: None,
            clip: None,
            font: None,
            font_size: 0.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            hscale: 1.0,
            leading: 0.0,
            rise: 0.0,
            render_mode: 0,
        }
    }
}

struct Interp<'a> {
    doc: &'a Document,
    pixmap: &'a mut Pixmap,
    opts: &'a RenderOptions,
    gs: GState,
    stack: Vec<GState>,
    path: PathBuilder,
    pending_clip: Option<FillRule>,
    tm: Transform,
    tlm: Transform,
    /// Glyph outlines (device space) for text render modes 4–7.
    text_clip: Option<PathBuilder>,
    /// Pattern space: the CTM at the start of the page / current form.
    pattern_base: Transform,
    images: BTreeMap<Ref, Rc<DecodedImage>>,
    /// Inside a Type 3 `d1` glyph: colour operators are ignored.
    in_uncoloured_glyph: bool,
}

fn num(ops: &[Object], i: usize) -> f32 {
    ops.get(i).and_then(Object::as_f32).unwrap_or(0.0)
}

fn nums(ops: &[Object]) -> Vec<f32> {
    ops.iter().filter_map(Object::as_f32).collect()
}

impl<'a> Interp<'a> {
    fn new(
        doc: &'a Document,
        pixmap: &'a mut Pixmap,
        opts: &'a RenderOptions,
        base: Transform,
    ) -> Self {
        Interp {
            doc,
            pixmap,
            opts,
            gs: GState::new(base),
            stack: Vec::new(),
            path: PathBuilder::new(),
            pending_clip: None,
            tm: Transform::identity(),
            tlm: Transform::identity(),
            text_clip: None,
            pattern_base: base,
            images: BTreeMap::new(),
            in_uncoloured_glyph: false,
        }
    }

    fn run(&mut self, content: &[u8], resources: &Dict, depth: u32) {
        if depth > MAX_FORM_DEPTH {
            return;
        }
        let base_stack = self.stack.len();
        for op in ContentParser::new(content) {
            match op {
                Op::Op(name, args) => self.op(name, &args, resources, depth),
                Op::InlineImage(dict, data) => {
                    let decoded = {
                        let s = super::object::Stream {
                            dict: dict.clone(),
                            data: StreamData::Owned(data),
                        };
                        self.doc.decode_stream(&s)
                    };
                    if let Ok(d) = decoded {
                        self.draw_image(&dict, d, Some(resources), None);
                    }
                }
            }
        }
        // Unbalanced `q`s don't leak out of a content stream.
        while self.stack.len() > base_stack {
            if let Some(g) = self.stack.pop() {
                self.gs = g;
            }
        }
    }

    fn op(&mut self, name: &[u8], a: &[Object], res: &Dict, depth: u32) {
        match name {
            // ---- graphics state
            b"q" => {
                if self.stack.len() < MAX_GSTATE {
                    self.stack.push(self.gs.clone());
                }
            }
            b"Q" => {
                if let Some(g) = self.stack.pop() {
                    self.gs = g;
                }
            }
            b"cm" if a.len() >= 6 => {
                let m = Transform::from_row(
                    num(a, 0),
                    num(a, 1),
                    num(a, 2),
                    num(a, 3),
                    num(a, 4),
                    num(a, 5),
                );
                self.gs.ctm = self.gs.ctm.pre_concat(m);
            }
            b"w" => self.gs.line_width = num(a, 0).abs(),
            b"J" => {
                self.gs.cap = match num(a, 0) as i32 {
                    1 => LineCap::Round,
                    2 => LineCap::Square,
                    _ => LineCap::Butt,
                }
            }
            b"j" => {
                self.gs.join = match num(a, 0) as i32 {
                    1 => LineJoin::Round,
                    2 => LineJoin::Bevel,
                    _ => LineJoin::Miter,
                }
            }
            b"M" => self.gs.miter = num(a, 0).max(1.0),
            b"d" => {
                let arr = a
                    .first()
                    .and_then(Object::as_array)
                    .map(nums)
                    .unwrap_or_default();
                self.gs.dash = (!arr.is_empty()).then(|| (arr, num(a, 1)));
            }
            b"gs" => {
                if let Some(n) = a.first().and_then(Object::as_name) {
                    self.ext_gstate(res, n);
                }
            }
            // ---- path construction
            b"m" => self.path.move_to(num(a, 0), num(a, 1)),
            b"l" => self.path.line_to(num(a, 0), num(a, 1)),
            b"c" => self.path.cubic_to(
                num(a, 0),
                num(a, 1),
                num(a, 2),
                num(a, 3),
                num(a, 4),
                num(a, 5),
            ),
            b"v" => {
                let p = self.path.last_point().unwrap_or_default();
                self.path
                    .cubic_to(p.x, p.y, num(a, 0), num(a, 1), num(a, 2), num(a, 3));
            }
            b"y" => self.path.cubic_to(
                num(a, 0),
                num(a, 1),
                num(a, 2),
                num(a, 3),
                num(a, 2),
                num(a, 3),
            ),
            b"h" => self.path.close(),
            b"re" => {
                let (x, y, w, h) = (num(a, 0), num(a, 1), num(a, 2), num(a, 3));
                self.path.move_to(x, y);
                self.path.line_to(x + w, y);
                self.path.line_to(x + w, y + h);
                self.path.line_to(x, y + h);
                self.path.close();
            }
            // ---- path painting
            b"S" => self.paint(false, None, true, res, depth),
            b"s" => {
                self.path.close();
                self.paint(false, None, true, res, depth)
            }
            b"f" | b"F" => self.paint(true, Some(FillRule::Winding), false, res, depth),
            b"f*" => self.paint(true, Some(FillRule::EvenOdd), false, res, depth),
            b"B" => self.paint(true, Some(FillRule::Winding), true, res, depth),
            b"B*" => self.paint(true, Some(FillRule::EvenOdd), true, res, depth),
            b"b" => {
                self.path.close();
                self.paint(true, Some(FillRule::Winding), true, res, depth)
            }
            b"b*" => {
                self.path.close();
                self.paint(true, Some(FillRule::EvenOdd), true, res, depth)
            }
            b"n" => self.paint(false, None, false, res, depth),
            b"W" => self.pending_clip = Some(FillRule::Winding),
            b"W*" => self.pending_clip = Some(FillRule::EvenOdd),
            // ---- colour
            b"CS" | b"cs" if !self.in_uncoloured_glyph => {
                let cs = ColorSpace::parse(self.doc, a.first().unwrap_or(&Object::Null), Some(res));
                let init = cs.initial();
                let fill = if matches!(cs, ColorSpace::Pattern) {
                    Fill::Solid([0.0; 3])
                } else {
                    Fill::Solid(cs.to_rgb(&init))
                };
                if name == b"CS" {
                    self.gs.stroke_cs = cs;
                    self.gs.stroke = fill;
                } else {
                    self.gs.fill_cs = cs;
                    self.gs.fill = fill;
                }
            }
            b"SC" | b"SCN" | b"sc" | b"scn" if !self.in_uncoloured_glyph => {
                let stroke = name[0] == b'S';
                let cs = if stroke {
                    &self.gs.stroke_cs
                } else {
                    &self.gs.fill_cs
                };
                let fill = match (cs, a.last()) {
                    (ColorSpace::Pattern, Some(Object::Name(n))) => {
                        Fill::Pattern(Object::Name(n.clone()))
                    }
                    _ => Fill::Solid(cs.to_rgb(&nums(a))),
                };
                if stroke {
                    self.gs.stroke = fill;
                } else {
                    self.gs.fill = fill;
                }
            }
            b"G" | b"g" | b"RG" | b"rg" | b"K" | b"k" if !self.in_uncoloured_glyph => {
                let cs = match name {
                    b"G" | b"g" => ColorSpace::Gray,
                    b"RG" | b"rg" => ColorSpace::Rgb,
                    _ => ColorSpace::Cmyk,
                };
                let rgb = cs.to_rgb(&nums(a));
                if name[0].is_ascii_uppercase() {
                    self.gs.stroke_cs = cs;
                    self.gs.stroke = Fill::Solid(rgb);
                } else {
                    self.gs.fill_cs = cs;
                    self.gs.fill = Fill::Solid(rgb);
                }
            }
            b"sh" => {
                if let Some(n) = a.first().and_then(Object::as_name) {
                    let sh = self.lookup(res, b"Shading", n);
                    let ctm = self.gs.ctm;
                    if let Some(shader) = self.shading(&sh, ctm, self.gs.fill_alpha) {
                        let rect = tiny_skia::Rect::from_xywh(
                            0.0,
                            0.0,
                            self.pixmap.width() as f32,
                            self.pixmap.height() as f32,
                        );
                        if let Some(rect) = rect {
                            let paint = Paint {
                                shader,
                                anti_alias: true,
                                ..Paint::default()
                            };
                            let clip = self.gs.clip.clone();
                            self.pixmap.fill_rect(
                                rect,
                                &paint,
                                Transform::identity(),
                                clip.as_deref(),
                            );
                        }
                    }
                }
            }
            // ---- XObjects
            b"Do" => {
                if let Some(n) = a.first().and_then(Object::as_name) {
                    self.xobject(res, n, depth);
                }
            }
            // ---- text
            b"BT" => {
                self.tm = Transform::identity();
                self.tlm = Transform::identity();
            }
            b"ET" => self.end_text_clip(),
            b"Tc" => self.gs.char_spacing = num(a, 0),
            b"Tw" => self.gs.word_spacing = num(a, 0),
            b"Tz" => self.gs.hscale = num(a, 0) / 100.0,
            b"TL" => self.gs.leading = num(a, 0),
            b"Ts" => self.gs.rise = num(a, 0),
            b"Tr" => self.gs.render_mode = num(a, 0).clamp(0.0, 7.0) as u8,
            b"Tf" => {
                if let Some(n) = a.first().and_then(Object::as_name) {
                    let f = self.lookup(res, b"Font", n);
                    self.gs.font = self.load_font(&f, a.first());
                }
                self.gs.font_size = num(a, 1);
            }
            b"Td" => self.next_line(num(a, 0), num(a, 1)),
            b"TD" => {
                self.gs.leading = -num(a, 1);
                self.next_line(num(a, 0), num(a, 1));
            }
            b"Tm" if a.len() >= 6 => {
                let m = Transform::from_row(
                    num(a, 0),
                    num(a, 1),
                    num(a, 2),
                    num(a, 3),
                    num(a, 4),
                    num(a, 5),
                );
                self.tm = m;
                self.tlm = m;
            }
            b"T*" => self.next_line(0.0, -self.gs.leading),
            b"Tj" => {
                if let Some(s) = a.first().and_then(Object::as_string) {
                    self.show(s, res, depth);
                }
            }
            b"'" => {
                self.next_line(0.0, -self.gs.leading);
                if let Some(s) = a.first().and_then(Object::as_string) {
                    self.show(s, res, depth);
                }
            }
            b"\"" => {
                self.gs.word_spacing = num(a, 0);
                self.gs.char_spacing = num(a, 1);
                self.next_line(0.0, -self.gs.leading);
                if let Some(s) = a.get(2).and_then(Object::as_string) {
                    self.show(s, res, depth);
                }
            }
            b"TJ" => {
                if let Some(arr) = a.first().and_then(Object::as_array) {
                    for item in arr {
                        match item {
                            Object::String(s) => self.show(s, res, depth),
                            o => {
                                if let Some(n) = o.as_f32() {
                                    let tx = -n / 1000.0 * self.gs.font_size * self.gs.hscale;
                                    self.tm = self.tm.pre_translate(tx, 0.0);
                                }
                            }
                        }
                    }
                }
            }
            b"d1" => self.in_uncoloured_glyph = true,
            _ => {}
        }
    }

    fn lookup(&self, res: &Dict, category: &[u8], name: &[u8]) -> Object {
        match self.doc.get_in(res, category) {
            Object::Dict(d) => d.get(name).cloned().unwrap_or(Object::Null),
            _ => Object::Null,
        }
    }

    fn ext_gstate(&mut self, res: &Dict, name: &[u8]) {
        let gs = self
            .doc
            .resolve(&self.lookup(res, b"ExtGState", name))
            .unwrap_or_default();
        let Some(d) = gs.as_dict() else { return };
        for (k, v) in d.iter() {
            let v = self.doc.resolve(v).unwrap_or_default();
            match k {
                b"LW" => self.gs.line_width = v.as_f32().unwrap_or(1.0).abs(),
                b"LC" => self.op(b"J", &[v], res, 0),
                b"LJ" => self.op(b"j", &[v], res, 0),
                b"ML" => self.gs.miter = v.as_f32().unwrap_or(10.0).max(1.0),
                b"CA" => self.gs.stroke_alpha = v.as_f32().unwrap_or(1.0).clamp(0.0, 1.0),
                b"ca" => self.gs.fill_alpha = v.as_f32().unwrap_or(1.0).clamp(0.0, 1.0),
                b"D" => {
                    if let Some(arr) = v.as_array() {
                        let a: Vec<Object> = arr.to_vec();
                        self.op(b"d", &a, res, 0);
                    }
                }
                b"Font" => {
                    if let Some(arr) = v.as_array() {
                        if let Some(f) = arr.first() {
                            let fo = self.doc.resolve(f).unwrap_or_default();
                            self.gs.font = self.load_font(&fo, Some(f));
                            self.gs.font_size = arr
                                .get(1)
                                .and_then(Object::as_f32)
                                .unwrap_or(self.gs.font_size);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // ---- painting -------------------------------------------------------

    fn take_path(&mut self) -> Option<Path> {
        core::mem::replace(&mut self.path, PathBuilder::new()).finish()
    }

    fn paint(&mut self, fill: bool, rule: Option<FillRule>, stroke: bool, res: &Dict, depth: u32) {
        let clip_rule = self.pending_clip.take();
        let path = self.take_path();
        if let Some(path) = &path {
            if fill {
                let f = self.gs.fill.clone();
                let alpha = self.gs.fill_alpha;
                if let Some(src) = self.paint_for(&f, alpha, res, depth) {
                    let clip = self.gs.clip.clone();
                    self.pixmap.fill_path(
                        path,
                        &src.paint(),
                        rule.unwrap_or(FillRule::Winding),
                        self.gs.ctm,
                        clip.as_deref(),
                    );
                }
            }
            if stroke {
                self.stroke_path(path, res, depth, self.gs.ctm);
            }
        }
        if let Some(rule) = clip_rule {
            self.clip(path.as_ref(), rule, self.gs.ctm);
        }
    }

    fn stroke_path(&mut self, path: &Path, res: &Dict, depth: u32, ts: Transform) {
        let s = self.gs.stroke.clone();
        let alpha = self.gs.stroke_alpha;
        let Some(src) = self.paint_for(&s, alpha, res, depth) else {
            return;
        };
        let paint = src.paint();
        // A zero-width line is "the thinnest line the device can render";
        // keep any line at least ~one device pixel wide so it stays visible.
        let scale = (ts.sx * ts.sx + ts.ky * ts.ky)
            .sqrt()
            .max((ts.kx * ts.kx + ts.sy * ts.sy).sqrt());
        let min = if scale > 0.0 { 0.75 / scale } else { 0.0 };
        let mut stroke = Stroke {
            width: self.gs.line_width.max(min),
            miter_limit: self.gs.miter,
            line_cap: self.gs.cap,
            line_join: self.gs.join,
            dash: None,
        };
        if let Some((arr, phase)) = &self.gs.dash {
            let mut arr = arr.clone();
            if arr.len() % 2 == 1 {
                arr.extend_from_within(..);
            }
            if arr.iter().any(|&v| v > 0.0) && arr.iter().all(|&v| v >= 0.0) {
                stroke.dash = StrokeDash::new(arr, *phase);
            }
        }
        let clip = self.gs.clip.clone();
        self.pixmap
            .stroke_path(path, &paint, &stroke, ts, clip.as_deref());
    }

    fn clip(&mut self, path: Option<&Path>, rule: FillRule, ts: Transform) {
        let (w, h) = (self.pixmap.width(), self.pixmap.height());
        let mut mask = match &self.gs.clip {
            Some(m) => (**m).clone(),
            None => {
                let Some(mut m) = Mask::new(w, h) else { return };
                if let Some(p) = path {
                    m.fill_path(p, rule, true, ts);
                }
                self.gs.clip = Some(Rc::new(m));
                return;
            }
        };
        match path {
            Some(p) => mask.intersect_path(p, rule, true, ts),
            None => mask.clear(),
        }
        self.gs.clip = Some(Rc::new(mask));
    }

    fn paint_for(&mut self, fill: &Fill, alpha: f32, res: &Dict, depth: u32) -> Option<PaintSrc> {
        Some(match fill {
            Fill::Solid([r, g, b]) => {
                PaintSrc::Shader(Shader::SolidColor(tiny_skia::Color::from_rgba(
                    r.clamp(0.0, 1.0),
                    g.clamp(0.0, 1.0),
                    b.clamp(0.0, 1.0),
                    alpha.clamp(0.0, 1.0),
                )?))
            }
            Fill::Pattern(Object::Name(n)) => {
                let pat = self.doc.resolve(&self.lookup(res, b"Pattern", n)).ok()?;
                self.pattern_shader(&pat, alpha, depth)?
            }
            Fill::Pattern(_) => return None,
        })
    }

    fn pattern_shader(&mut self, pat: &Object, alpha: f32, depth: u32) -> Option<PaintSrc> {
        let d = pat.as_dict()?;
        let m: Vec<f32> = self
            .doc
            .get_in(d, b"Matrix")
            .as_array()
            .map(nums)
            .unwrap_or_default();
        let pm = if m.len() == 6 {
            Transform::from_row(m[0], m[1], m[2], m[3], m[4], m[5])
        } else {
            Transform::identity()
        };
        let space = self.pattern_base.pre_concat(pm);
        match self.doc.get_in(d, b"PatternType").as_i64()? {
            2 => {
                let sh = self.doc.get_in(d, b"Shading");
                self.shading(&sh, space, alpha).map(PaintSrc::Shader)
            }
            1 => self.tiling(pat, space, alpha, depth),
            _ => None,
        }
    }

    /// Render one tiling-pattern cell to a pixmap and repeat it.
    fn tiling(
        &mut self,
        pat: &Object,
        space: Transform,
        alpha: f32,
        depth: u32,
    ) -> Option<PaintSrc> {
        if depth >= MAX_FORM_DEPTH {
            return None;
        }
        let s = pat.as_stream()?;
        let d = &s.dict;
        let bbox = self.doc.rect(d, b"BBox")?;
        let xs = self
            .doc
            .get_in(d, b"XStep")
            .as_f32()
            .unwrap_or(bbox[2] - bbox[0])
            .abs();
        let ys = self
            .doc
            .get_in(d, b"YStep")
            .as_f32()
            .unwrap_or(bbox[3] - bbox[1])
            .abs();
        if xs <= 0.0 || ys <= 0.0 {
            return None;
        }
        // Cell resolution: the pattern space's device scale.
        let sc = (space.sx * space.sx + space.ky * space.ky).sqrt().max(0.01);
        let (cw, ch) = (
            (xs * sc).ceil().clamp(1.0, 1024.0),
            (ys * sc).ceil().clamp(1.0, 1024.0),
        );
        let mut cell = Pixmap::new(cw as u32, ch as u32)?;
        let res = self
            .doc
            .get_in(d, b"Resources")
            .as_dict()
            .cloned()
            .unwrap_or_default();
        let content = self.doc.stream_bytes(s).ok()?;
        // Cell space: pattern units scaled to the cell, y down.
        let cell_ts = Transform::from_row(
            cw / xs,
            0.0,
            0.0,
            -ch / ys,
            -bbox[0] * cw / xs,
            bbox[1] * ch / ys + ch,
        );
        {
            let opts = self.opts;
            let mut sub = Interp::new(self.doc, &mut cell, opts, cell_ts);
            if self.doc.get_in(d, b"PaintType").as_i64() == Some(2) {
                // Uncoloured: paints in the current fill colour.
                sub.gs.fill = self.gs.fill.clone();
                sub.gs.stroke = self.gs.fill.clone();
                sub.in_uncoloured_glyph = true;
            }
            sub.run(&content, &res, depth + 1);
        }
        // Map cell pixels back to device: cell → pattern space → device.
        let inv = cell_ts.invert()?;
        let ts = space.pre_concat(inv);
        Some(PaintSrc::Tile { cell, ts, alpha })
    }

    /// An axial (2) or radial (3) shading as a gradient shader whose
    /// gradient space maps through `ts`.
    fn shading(&self, sh: &Object, ts: Transform, alpha: f32) -> Option<Shader<'static>> {
        let sh = self.doc.resolve(sh).ok()?;
        let d = sh.as_dict()?;
        let ty = self.doc.get_in(d, b"ShadingType").as_i64()?;
        let cs = ColorSpace::parse(self.doc, &self.doc.get_in(d, b"ColorSpace"), None);
        let coords = self
            .doc
            .get_in(d, b"Coords")
            .as_array()
            .map(nums)
            .unwrap_or_default();
        let domain = self
            .doc
            .get_in(d, b"Domain")
            .as_array()
            .map(nums)
            .unwrap_or_default();
        let (t0, t1) = (
            domain.first().copied().unwrap_or(0.0),
            domain.get(1).copied().unwrap_or(1.0),
        );
        let func = Function::parse(self.doc, d.get(b"Function")?)?;
        const STOPS: usize = 32;
        let stops: Vec<GradientStop> = (0..=STOPS)
            .map(|i| {
                let f = i as f32 / STOPS as f32;
                let [r, g, b] = cs.to_rgb(&func.eval(&[t0 + f * (t1 - t0)]));
                let c = tiny_skia::Color::from_rgba(
                    r.clamp(0.0, 1.0),
                    g.clamp(0.0, 1.0),
                    b.clamp(0.0, 1.0),
                    alpha.clamp(0.0, 1.0),
                )
                .unwrap_or(tiny_skia::Color::BLACK);
                GradientStop::new(f, c)
            })
            .collect();
        match ty {
            2 if coords.len() >= 4 => tiny_skia::LinearGradient::new(
                tiny_skia::Point::from_xy(coords[0], coords[1]),
                tiny_skia::Point::from_xy(coords[2], coords[3]),
                stops,
                SpreadMode::Pad,
                ts,
            ),
            3 if coords.len() >= 6 => tiny_skia::RadialGradient::new(
                tiny_skia::Point::from_xy(coords[0], coords[1]),
                coords[2].max(0.0),
                tiny_skia::Point::from_xy(coords[3], coords[4]),
                coords[5].max(0.0),
                stops,
                SpreadMode::Pad,
                ts,
            ),
            _ => None,
        }
    }

    // ---- XObjects -------------------------------------------------------

    fn xobject(&mut self, res: &Dict, name: &[u8], depth: u32) {
        let raw = self.lookup(res, b"XObject", name);
        let r = raw.as_ref();
        let Ok(obj) = self.doc.resolve(&raw) else {
            return;
        };
        let Some(s) = obj.as_stream() else { return };
        match s.dict.name(b"Subtype") {
            Some(b"Image") => {
                if let Some(r) = r {
                    let is_mask = self
                        .doc
                        .get_in(&s.dict, b"ImageMask")
                        .as_bool()
                        .unwrap_or(false);
                    if !is_mask {
                        if let Some(img) = self.images.get(&r).cloned() {
                            self.blit(&img);
                            return;
                        }
                    }
                }
                if let Ok(d) = self.doc.decode_stream(s) {
                    self.draw_image(&s.dict.clone(), d, Some(res), r);
                }
            }
            Some(b"Form") => self.form(s, res, depth),
            _ => {}
        }
    }

    fn form(&mut self, s: &super::object::Stream, parent_res: &Dict, depth: u32) {
        if depth >= MAX_FORM_DEPTH {
            return;
        }
        let d = &s.dict;
        let Ok(content) = self.doc.stream_bytes(s) else {
            return;
        };
        let res = match self.doc.get_in(d, b"Resources") {
            Object::Dict(r) => r,
            _ => parent_res.clone(),
        };
        let saved = self.gs.clone();
        let saved_base = self.pattern_base;
        let m: Vec<f32> = self
            .doc
            .get_in(d, b"Matrix")
            .as_array()
            .map(nums)
            .unwrap_or_default();
        if m.len() == 6 {
            self.gs.ctm = self
                .gs
                .ctm
                .pre_concat(Transform::from_row(m[0], m[1], m[2], m[3], m[4], m[5]));
        }
        self.pattern_base = self.gs.ctm;
        if let Some(b) = self.doc.rect(d, b"BBox") {
            let p = tiny_skia::Rect::from_ltrb(b[0], b[1], b[2], b[3]).map(PathBuilder::from_rect);
            self.clip(p.as_ref(), FillRule::Winding, self.gs.ctm);
        }
        let saved_path = core::mem::replace(&mut self.path, PathBuilder::new());
        self.run(&content, &res, depth + 1);
        self.path = saved_path;
        self.gs = saved;
        self.pattern_base = saved_base;
    }

    fn draw_image(
        &mut self,
        dict: &Dict,
        decoded: Decoded,
        res: Option<&Dict>,
        cache_as: Option<Ref>,
    ) {
        let fill = match self.gs.fill {
            Fill::Solid([r, g, b]) => [
                (r.clamp(0.0, 1.0) * 255.0) as u8,
                (g.clamp(0.0, 1.0) * 255.0) as u8,
                (b.clamp(0.0, 1.0) * 255.0) as u8,
                255,
            ],
            Fill::Pattern(_) => [128, 128, 128, 255],
        };
        let Some(img) = image::decode(self.doc, dict, decoded, res, fill, self.opts.max_pixels)
        else {
            return;
        };
        let img = Rc::new(img);
        let is_mask = self
            .doc
            .get_in(dict, b"ImageMask")
            .as_bool()
            .unwrap_or(false);
        if let (Some(r), false) = (cache_as, is_mask) {
            // Keep only modest images around for reuse on this page.
            if (img.pixmap.width() as u64 * img.pixmap.height() as u64) < 4 * 1024 * 1024 {
                self.images.insert(r, img.clone());
            }
        }
        self.blit(&img);
    }

    fn blit(&mut self, img: &DecodedImage) {
        let (w, h) = (img.pixmap.width() as f32, img.pixmap.height() as f32);
        // Image space: unit square, (0,0) of the pixmap at its top-left.
        let ts = self
            .gs
            .ctm
            .pre_concat(Transform::from_row(1.0 / w, 0.0, 0.0, -1.0 / h, 0.0, 1.0));
        // Upscaling a tiny bitmap smoothly smears it; keep its pixels.
        let up = (ts.sx * ts.sx + ts.ky * ts.ky).sqrt();
        let quality = if img.smooth || up < 1.5 {
            FilterQuality::Bilinear
        } else {
            FilterQuality::Nearest
        };
        let paint = PixmapPaint {
            opacity: self.gs.fill_alpha,
            quality,
            ..PixmapPaint::default()
        };
        let clip = self.gs.clip.clone();
        self.pixmap
            .draw_pixmap(0, 0, img.pixmap.as_ref(), &paint, ts, clip.as_deref());
    }

    // ---- text -----------------------------------------------------------

    fn load_font(&self, f: &Object, key: Option<&Object>) -> Option<Rc<Font>> {
        let r = match key {
            Some(Object::Ref(r)) => Some(*r),
            _ => None,
        };
        // Fonts referenced indirectly are cached document-wide.
        let fref = match f {
            Object::Ref(r) => Some(*r),
            _ => r,
        };
        if let Some(fr) = fref {
            if let Some(font) = self.doc.fonts.borrow().get(&fr) {
                return Some(font.clone());
            }
        }
        let obj = self.doc.resolve(f).ok()?;
        let font = Rc::new(Font::load(self.doc, obj.as_dict()?));
        if let Some(fr) = fref {
            self.doc.fonts.borrow_mut().insert(fr, font.clone());
        }
        Some(font)
    }

    fn next_line(&mut self, tx: f32, ty: f32) {
        self.tlm = self.tlm.pre_translate(tx, ty);
        self.tm = self.tlm;
    }

    fn show(&mut self, s: &[u8], res: &Dict, depth: u32) {
        let Some(font) = self.gs.font.clone() else {
            return;
        };
        let fs = self.gs.font_size;
        let th = self.gs.hscale;
        let mode = self.gs.render_mode;
        let fill_src = if matches!(mode, 0 | 2 | 4 | 6) {
            let f = self.gs.fill.clone();
            self.paint_for(&f, self.gs.fill_alpha, res, depth)
        } else {
            None
        };
        let fill_paint = fill_src.as_ref().map(PaintSrc::paint);
        for (code, len) in font.codes(s) {
            let trm = self
                .gs
                .ctm
                .pre_concat(self.tm)
                .pre_concat(Transform::from_row(
                    fs * th,
                    0.0,
                    0.0,
                    fs,
                    0.0,
                    self.gs.rise,
                ));
            if let Some(t3) = &font.type3 {
                if mode != 3 && mode != 7 {
                    self.type3_glyph(&font, t3, code, trm, res, depth);
                }
            } else if let Some(path) = font.outline(code) {
                if let Some(paint) = &fill_paint {
                    let clip = self.gs.clip.clone();
                    self.pixmap
                        .fill_path(&path, paint, FillRule::Winding, trm, clip.as_deref());
                    if font.fake_bold {
                        let stroke = Stroke {
                            width: 0.03,
                            ..Stroke::default()
                        };
                        self.pixmap
                            .stroke_path(&path, paint, &stroke, trm, clip.as_deref());
                    }
                }
                if matches!(mode, 1 | 2 | 5 | 6) {
                    // Stroke widths are in user space: undo the text matrix.
                    self.stroke_glyph(&path, trm, res, depth);
                }
                if mode >= 4 {
                    if let Some(dev) = (*path).clone().transform(trm) {
                        self.text_clip
                            .get_or_insert_with(PathBuilder::new)
                            .push_path(&dev);
                    }
                }
            }
            let w0 = font.width(code);
            let ws = if font.is_space(code, len) {
                self.gs.word_spacing
            } else {
                0.0
            };
            let tx = (w0 * fs + self.gs.char_spacing + ws) * th;
            self.tm = self.tm.pre_translate(tx, 0.0);
        }
    }

    fn stroke_glyph(&mut self, path: &Path, trm: Transform, res: &Dict, depth: u32) {
        // Bring the glyph to user space so the user-space line width applies.
        let to_user = self.gs.ctm.invert().map(|inv| inv.pre_concat(trm));
        if let Some(p) = to_user.and_then(|t| path.clone().transform(t)) {
            let ctm = self.gs.ctm;
            self.stroke_path(&p, res, depth, ctm);
        }
    }

    fn type3_glyph(
        &mut self,
        font: &Rc<Font>,
        t3: &super::font::Type3,
        code: u32,
        trm: Transform,
        res: &Dict,
        depth: u32,
    ) {
        if depth >= MAX_FORM_DEPTH {
            return;
        }
        let Some(name) = font.glyph_name(code) else {
            return;
        };
        let Ok(Object::Stream(proc_)) = self
            .doc
            .resolve(t3.procs.get(name).unwrap_or(&Object::Null))
        else {
            return;
        };
        let Ok(content) = self.doc.stream_bytes(&proc_) else {
            return;
        };
        let glyph_res = t3.resources.clone().unwrap_or_else(|| res.clone());
        let saved = self.gs.clone();
        let saved_tm = (self.tm, self.tlm);
        let saved_path = core::mem::replace(&mut self.path, PathBuilder::new());
        let saved_uncol = self.in_uncoloured_glyph;
        self.gs.ctm = trm.pre_concat(t3.matrix);
        self.run(&content, &glyph_res, depth + 1);
        self.in_uncoloured_glyph = saved_uncol;
        self.path = saved_path;
        (self.tm, self.tlm) = saved_tm;
        self.gs = saved;
    }

    fn end_text_clip(&mut self) {
        if let Some(pb) = self.text_clip.take() {
            let p = pb.finish();
            self.clip(p.as_ref(), FillRule::Winding, Transform::identity());
        }
    }

    // ---- annotations ----------------------------------------------------

    fn annotations(&mut self, page: &Dict, base: Transform) {
        let annots = self.doc.get_in(page, b"Annots");
        let Some(list) = annots.as_array() else {
            return;
        };
        for a in list.iter().take(1000) {
            let Ok(obj) = self.doc.resolve(a) else {
                continue;
            };
            let Some(d) = obj.as_dict() else { continue };
            let flags = self.doc.get_in(d, b"F").as_i64().unwrap_or(0);
            if flags & (2 | 32) != 0 || d.name(b"Subtype") == Some(b"Popup") {
                continue;
            }
            let Some(rect) = self.doc.rect(d, b"Rect") else {
                continue;
            };
            let ap = self.doc.get_in(d, b"AP");
            let Some(ap) = ap.as_dict() else { continue };
            let n = self.doc.get_in(ap, b"N");
            let stream = match n {
                Object::Stream(s) => s,
                Object::Dict(states) => {
                    let Some(state) = d.name(b"AS") else { continue };
                    match self.doc.get_in(&states, state) {
                        Object::Stream(s) => s,
                        _ => continue,
                    }
                }
                _ => continue,
            };
            let Some(bbox) = self.doc.rect(&stream.dict, b"BBox") else {
                continue;
            };
            let m: Vec<f32> = self
                .doc
                .get_in(&stream.dict, b"Matrix")
                .as_array()
                .map(nums)
                .unwrap_or_default();
            let matrix = if m.len() == 6 {
                Transform::from_row(m[0], m[1], m[2], m[3], m[4], m[5])
            } else {
                Transform::identity()
            };
            // Transformed bbox → fit to Rect (§12.5.5, algorithm 8.1).
            let mut pts = [
                tiny_skia::Point::from_xy(bbox[0], bbox[1]),
                tiny_skia::Point::from_xy(bbox[2], bbox[1]),
                tiny_skia::Point::from_xy(bbox[2], bbox[3]),
                tiny_skia::Point::from_xy(bbox[0], bbox[3]),
            ];
            matrix.map_points(&mut pts);
            let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
            for p in pts {
                x0 = x0.min(p.x);
                y0 = y0.min(p.y);
                x1 = x1.max(p.x);
                y1 = y1.max(p.y);
            }
            if x1 - x0 <= 0.0 || y1 - y0 <= 0.0 {
                continue;
            }
            let sx = (rect[2] - rect[0]) / (x1 - x0);
            let sy = (rect[3] - rect[1]) / (y1 - y0);
            let fit = Transform::from_row(sx, 0.0, 0.0, sy, rect[0] - x0 * sx, rect[1] - y0 * sy);
            self.gs = GState::new(base.pre_concat(fit));
            self.pattern_base = base;
            let res = Dict::default();
            self.form(&stream, &res, 0);
        }
    }
}
