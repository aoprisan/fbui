//! Embedded Type 1 fonts (`FontFile`): the eexec-encrypted PostScript font
//! format pdfTeX and older tools embed. We parse just enough PostScript to
//! find the encoding, `Subrs` and `CharStrings`, and interpret Type 1
//! charstrings (Adobe Type 1 Font Format, ch. 6) into outlines — including
//! flex, hint replacement and `seac` accents.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use super::encoding::{self, Encoding};
use super::lexer::{Lexer, Token};

/// Bounds against hostile charstrings.
const MAX_SUBR_DEPTH: u32 = 10;
const MAX_STACK: usize = 48;
const MAX_OPS: u32 = 20_000;

pub struct Type1Font {
    /// Built-in encoding: code → glyph name.
    pub encoding: Vec<Option<Vec<u8>>>,
    pub font_matrix: [f32; 6],
    subrs: Vec<Vec<u8>>,
    charstrings: BTreeMap<Vec<u8>, Vec<u8>>,
}

/// Receives an outline in glyph space.
pub trait Sink {
    fn move_to(&mut self, x: f32, y: f32);
    fn line_to(&mut self, x: f32, y: f32);
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32);
    fn close(&mut self);
}

fn decrypt(data: &[u8], mut r: u16, skip: usize) -> Vec<u8> {
    const C1: u16 = 52845;
    const C2: u16 = 22719;
    let mut out = Vec::with_capacity(data.len());
    for &c in data {
        out.push(c ^ (r >> 8) as u8);
        r = (c as u16).wrapping_add(r).wrapping_mul(C1).wrapping_add(C2);
    }
    if out.len() >= skip {
        out.drain(..skip);
    } else {
        out.clear();
    }
    out
}

fn is_hex(b: u8) -> bool {
    b.is_ascii_hexdigit()
}

impl Type1Font {
    /// Parse a `FontFile` stream's decoded bytes. `len1` is `/Length1`, the
    /// cleartext part's size (found by searching for `eexec` if absent).
    pub fn parse(data: &[u8], len1: Option<usize>) -> Option<Type1Font> {
        let data = strip_pfb(data);
        let data = &data[..];
        let eexec = super::lexer::find(data, b"eexec", 0)?;
        let clear_end = len1
            .filter(|&l| l <= data.len() && l > eexec)
            .unwrap_or(eexec + 5);
        let clear = &data[..clear_end.min(data.len())];
        let mut enc_start = eexec + 5;
        while enc_start < data.len() && matches!(data[enc_start], b'\r' | b'\n' | b' ' | b'\t') {
            enc_start += 1;
        }
        let body = &data[enc_start.min(data.len())..];
        let binary = if body.len() >= 4 && body[..4].iter().all(|&b| is_hex(b)) {
            // Hex-encoded eexec section.
            let mut v = Vec::with_capacity(body.len() / 2);
            let mut hi = None;
            for &b in body {
                let Some(d) = (b as char).to_digit(16) else {
                    continue;
                };
                match hi.take() {
                    Some(h) => v.push((h << 4 | d) as u8),
                    None => hi = Some(d),
                }
            }
            v
        } else {
            body.to_vec()
        };
        let private = decrypt(&binary, 55665, 4);

        let mut font = Type1Font {
            encoding: alloc::vec![None; 256],
            font_matrix: [0.001, 0.0, 0.0, 0.001, 0.0, 0.0],
            subrs: Vec::new(),
            charstrings: BTreeMap::new(),
        };
        font.parse_cleartext(clear);
        font.parse_private(&private);
        (!font.charstrings.is_empty()).then_some(font)
    }

    fn parse_cleartext(&mut self, clear: &[u8]) {
        if let Some(p) = super::lexer::find(clear, b"/FontMatrix", 0) {
            let mut lx = Lexer::at(clear, p + 11);
            if let Ok(super::object::Object::Array(a)) = lx.parse_object(false) {
                if a.len() == 6 {
                    for (i, o) in a.iter().enumerate() {
                        self.font_matrix[i] = o.as_f32().unwrap_or(0.0);
                    }
                }
            }
        }
        let Some(p) = super::lexer::find(clear, b"/Encoding", 0) else {
            return;
        };
        let mut lx = Lexer::at(clear, p + 9);
        if let Some(Token::Keyword(b"StandardEncoding")) = lx.next_token() {
            set_from(&mut self.encoding, &encoding::STANDARD);
            return;
        }
        // `dup <code> /<name> put` entries until `readonly def` / `def`.
        let mut prev: [Option<Token>; 2] = [None, None];
        while let Some(t) = lx.next_token() {
            match &t {
                Token::Keyword(b"def") => break,
                Token::Keyword(b"put") => {
                    if let (Some(Token::Int(code)), Some(Token::Name(name))) = (&prev[0], &prev[1])
                    {
                        if (0..256).contains(code) {
                            self.encoding[*code as usize] = Some(name.clone());
                        }
                    }
                }
                _ => {}
            }
            prev = [prev[1].take(), Some(t)];
        }
    }

    fn parse_private(&mut self, p: &[u8]) {
        let len_iv = super::lexer::find(p, b"/lenIV", 0)
            .and_then(|i| match Lexer::at(p, i + 6).next_token() {
                Some(Token::Int(n)) => Some(n),
                _ => None,
            })
            .unwrap_or(4);
        let skip = if len_iv < 0 {
            None
        } else {
            Some(len_iv as usize)
        };
        let decode_cs = |raw: &[u8]| -> Vec<u8> {
            match skip {
                Some(n) => decrypt(raw, 4330, n),
                None => raw.to_vec(),
            }
        };

        // Subrs: `dup <i> <len> RD <bytes> NP`.
        if let Some(s) = super::lexer::find(p, b"/Subrs", 0) {
            let mut lx = Lexer::at(p, s + 6);
            if let Some(Token::Int(count)) = lx.next_token() {
                let count = count.clamp(0, 65_536) as usize;
                self.subrs = alloc::vec![Vec::new(); count];
                loop {
                    match lx.next_token() {
                        Some(Token::Keyword(b"dup")) => {}
                        Some(Token::Keyword(b"array")) => continue,
                        Some(_) if self.subrs.iter().all(Vec::is_empty) => continue,
                        _ => break,
                    }
                    let (Some(Token::Int(i)), Some(Token::Int(len))) =
                        (lx.next_token(), lx.next_token())
                    else {
                        break;
                    };
                    let _rd = lx.next_token();
                    let start = lx.pos + 1;
                    let end = start + len.max(0) as usize;
                    if end > p.len() {
                        break;
                    }
                    if (0..count as i64).contains(&i) {
                        self.subrs[i as usize] = decode_cs(&p[start..end]);
                    }
                    lx.pos = end;
                    let _np = lx.next_token();
                    if lx.at_end() {
                        break;
                    }
                }
            }
        }

        // CharStrings: `/<name> <len> RD <bytes> ND`.
        let Some(c) = super::lexer::find(p, b"/CharStrings", 0) else {
            return;
        };
        let mut lx = Lexer::at(p, c + 12);
        // Skip `<n> dict dup begin`.
        loop {
            match lx.next_token() {
                Some(Token::Keyword(b"begin")) => break,
                Some(_) => continue,
                None => return,
            }
        }
        loop {
            let name = match lx.next_token() {
                Some(Token::Name(n)) => n,
                Some(Token::Keyword(b"end")) | None => break,
                Some(_) => continue,
            };
            let Some(Token::Int(len)) = lx.next_token() else {
                break;
            };
            let _rd = lx.next_token();
            let start = lx.pos + 1;
            let end = start + len.max(0) as usize;
            if end > p.len() {
                break;
            }
            self.charstrings.insert(name, decode_cs(&p[start..end]));
            lx.pos = end;
            let _nd = lx.next_token();
        }
    }

    pub fn has_glyph(&self, name: &[u8]) -> bool {
        self.charstrings.contains_key(name)
    }

    /// Draw glyph `name` into `sink`; returns its advance width (glyph units).
    pub fn outline(&self, name: &[u8], sink: &mut dyn Sink) -> Option<f32> {
        let cs = self.charstrings.get(name)?;
        let mut st = Interp {
            font: self,
            sink,
            stack: Vec::new(),
            ps: Vec::new(),
            x: 0.0,
            y: 0.0,
            width: 0.0,
            open: false,
            flex: None,
            ops: 0,
            offset: (0.0, 0.0),
        };
        // `endchar` unwinds as `Err(Done)`: that is the normal way out.
        let _ = st.run(cs, 0);
        if st.open {
            st.sink.close();
        }
        Some(st.width)
    }
}

fn set_from(dst: &mut [Option<Vec<u8>>], src: &Encoding) {
    for (d, s) in dst.iter_mut().zip(src.iter()) {
        *d = s.map(<[u8]>::to_vec);
    }
}

/// `.pfb` segment headers (0x80 0x01/0x02 + length) wrapped around the
/// font — strip them so the rest of the parser sees a plain `.pfa` layout.
fn strip_pfb(data: &[u8]) -> Vec<u8> {
    if data.first() != Some(&0x80) {
        return data.to_vec();
    }
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i + 6 <= data.len() && data[i] == 0x80 && matches!(data[i + 1], 1 | 2) {
        let len = u32::from_le_bytes([data[i + 2], data[i + 3], data[i + 4], data[i + 5]]) as usize;
        let start = i + 6;
        let end = (start + len).min(data.len());
        out.extend_from_slice(&data[start..end]);
        i = end;
    }
    out
}

struct Interp<'a> {
    font: &'a Type1Font,
    sink: &'a mut dyn Sink,
    stack: Vec<f32>,
    /// The PostScript operand stack `callothersubr` / `pop` talk through.
    ps: Vec<f32>,
    x: f32,
    y: f32,
    width: f32,
    open: bool,
    /// Points collected during a flex sequence.
    flex: Option<Vec<(f32, f32)>>,
    ops: u32,
    /// Origin shift while drawing a `seac` accent.
    offset: (f32, f32),
}

struct Done;

impl Interp<'_> {
    fn pop(&mut self) -> f32 {
        self.stack.pop().unwrap_or(0.0)
    }

    fn arg(&self, i: usize) -> f32 {
        self.stack.get(i).copied().unwrap_or(0.0)
    }

    fn move_to(&mut self, dx: f32, dy: f32) {
        self.x += dx;
        self.y += dy;
        if let Some(f) = &mut self.flex {
            f.push((self.x, self.y));
            return;
        }
        if self.open {
            self.sink.close();
        }
        self.sink
            .move_to(self.x + self.offset.0, self.y + self.offset.1);
        self.open = true;
    }

    fn line_to(&mut self, dx: f32, dy: f32) {
        self.x += dx;
        self.y += dy;
        self.ensure_open();
        self.sink
            .line_to(self.x + self.offset.0, self.y + self.offset.1);
    }

    fn curve(&mut self, d: [f32; 6]) {
        let (x1, y1) = (self.x + d[0], self.y + d[1]);
        let (x2, y2) = (x1 + d[2], y1 + d[3]);
        let (x3, y3) = (x2 + d[4], y2 + d[5]);
        self.ensure_open();
        let (ox, oy) = self.offset;
        self.sink
            .curve_to(x1 + ox, y1 + oy, x2 + ox, y2 + oy, x3 + ox, y3 + oy);
        self.x = x3;
        self.y = y3;
    }

    fn ensure_open(&mut self) {
        if !self.open {
            self.sink
                .move_to(self.x + self.offset.0, self.y + self.offset.1);
            self.open = true;
        }
    }

    /// Run one charstring. `Err(Done)` = `endchar` reached (stop everything).
    fn run(&mut self, cs: &[u8], depth: u32) -> Result<(), Done> {
        if depth > MAX_SUBR_DEPTH {
            return Err(Done);
        }
        let mut i = 0;
        while i < cs.len() {
            self.ops += 1;
            if self.ops > MAX_OPS {
                return Err(Done);
            }
            let v = cs[i];
            i += 1;
            if v >= 32 {
                let n = match v {
                    32..=246 => v as i32 - 139,
                    247..=250 => (v as i32 - 247) * 256 + *cs.get(i).unwrap_or(&0) as i32 + 108,
                    251..=254 => -(v as i32 - 251) * 256 - *cs.get(i).unwrap_or(&0) as i32 - 108,
                    _ => {
                        let b = cs.get(i..i + 4).unwrap_or(&[0, 0, 0, 0]);
                        i32::from_be_bytes([b[0], b[1], b[2], b[3]])
                    }
                };
                i += match v {
                    247..=254 => 1,
                    255 => 4,
                    _ => 0,
                };
                if self.stack.len() < MAX_STACK {
                    self.stack.push(n as f32);
                }
                continue;
            }
            match v {
                // hstem, vstem: hints, ignored.
                1 | 3 => self.stack.clear(),
                4 => {
                    let dy = self.arg(0);
                    self.move_to(0.0, dy);
                    self.stack.clear();
                }
                5 => {
                    let (dx, dy) = (self.arg(0), self.arg(1));
                    self.line_to(dx, dy);
                    self.stack.clear();
                }
                6 => {
                    let dx = self.arg(0);
                    self.line_to(dx, 0.0);
                    self.stack.clear();
                }
                7 => {
                    let dy = self.arg(0);
                    self.line_to(0.0, dy);
                    self.stack.clear();
                }
                8 => {
                    let d = [
                        self.arg(0),
                        self.arg(1),
                        self.arg(2),
                        self.arg(3),
                        self.arg(4),
                        self.arg(5),
                    ];
                    self.curve(d);
                    self.stack.clear();
                }
                9 => {
                    if self.open {
                        self.sink.close();
                        self.open = false;
                    }
                    self.stack.clear();
                }
                10 => {
                    let n = self.pop();
                    let font = self.font;
                    if let Some(sub) = font.subrs.get(n as usize) {
                        self.run(sub, depth + 1)?;
                    }
                }
                11 => return Ok(()),
                13 => {
                    // hsbw: sbx wx
                    self.x = self.arg(0);
                    self.y = 0.0;
                    self.width = self.arg(1);
                    self.stack.clear();
                }
                14 => return Err(Done),
                21 => {
                    let (dx, dy) = (self.arg(0), self.arg(1));
                    self.move_to(dx, dy);
                    self.stack.clear();
                }
                22 => {
                    let dx = self.arg(0);
                    self.move_to(dx, 0.0);
                    self.stack.clear();
                }
                30 => {
                    let d = [0.0, self.arg(0), self.arg(1), self.arg(2), self.arg(3), 0.0];
                    self.curve(d);
                    self.stack.clear();
                }
                31 => {
                    let d = [self.arg(0), 0.0, self.arg(1), self.arg(2), 0.0, self.arg(3)];
                    self.curve(d);
                    self.stack.clear();
                }
                12 => {
                    let e = *cs.get(i).unwrap_or(&0);
                    i += 1;
                    self.escape(e, depth)?;
                }
                _ => self.stack.clear(),
            }
        }
        Ok(())
    }

    fn escape(&mut self, e: u8, depth: u32) -> Result<(), Done> {
        match e {
            // dotsection, vstem3, hstem3: hints.
            0..=2 => self.stack.clear(),
            6 => {
                // seac: asb adx ady bchar achar
                let (asb, adx, ady) = (self.arg(0), self.arg(1), self.arg(2));
                let (bchar, achar) = (self.arg(3) as usize, self.arg(4) as usize);
                self.stack.clear();
                let font = self.font;
                let name = |c: usize| encoding::STANDARD.get(c).copied().flatten();
                if let Some(cs) = name(bchar).and_then(|n| font.charstrings.get(n)) {
                    let w = self.width;
                    let _ = self.run(cs, depth + 1);
                    self.width = w;
                }
                if let Some(cs) = name(achar).and_then(|n| font.charstrings.get(n)) {
                    if self.open {
                        self.sink.close();
                        self.open = false;
                    }
                    let w = self.width;
                    self.offset = (adx - asb, ady);
                    let _ = self.run(cs, depth + 1);
                    self.offset = (0.0, 0.0);
                    self.width = w;
                }
                return Err(Done);
            }
            7 => {
                // sbw: sbx sby wx wy
                self.x = self.arg(0);
                self.y = self.arg(1);
                self.width = self.arg(2);
                self.stack.clear();
            }
            12 => {
                let b = self.pop();
                let a = self.pop();
                self.stack.push(if b != 0.0 { a / b } else { 0.0 });
            }
            16 => {
                // callothersubr: args... n othersubr#
                let other = self.pop() as i32;
                let n = (self.pop().max(0.0) as usize).min(self.stack.len());
                let args: Vec<f32> = self.stack.split_off(self.stack.len() - n);
                match other {
                    1 => self.flex = Some(Vec::new()),
                    2 => {}
                    0 => {
                        // End flex: 7 points collected (ref + 2 curves).
                        if let Some(pts) = self.flex.take() {
                            if pts.len() >= 7 {
                                self.ensure_open();
                                let (ox, oy) = self.offset;
                                for c in [&pts[1..4], &pts[4..7]] {
                                    self.sink.curve_to(
                                        c[0].0 + ox,
                                        c[0].1 + oy,
                                        c[1].0 + ox,
                                        c[1].1 + oy,
                                        c[2].0 + ox,
                                        c[2].1 + oy,
                                    );
                                }
                                self.x = pts[6].0;
                                self.y = pts[6].1;
                            }
                        }
                        // The final point comes back via two `pop`s.
                        self.ps.clear();
                        self.ps.push(self.y);
                        self.ps.push(self.x);
                    }
                    3 => {
                        // Hint replacement: `pop` must yield 3 → callsubr 3.
                        self.ps.clear();
                        self.ps.push(3.0);
                    }
                    _ => {
                        self.ps.clear();
                        self.ps.extend(args.iter().rev());
                    }
                }
            }
            17 => {
                let v = self.ps.pop().unwrap_or(0.0);
                if self.stack.len() < MAX_STACK {
                    self.stack.push(v);
                }
            }
            33 => {
                // setcurrentpoint (follows flex)
                self.x = self.arg(0);
                self.y = self.arg(1);
                self.stack.clear();
            }
            _ => self.stack.clear(),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eexec_roundtrip() {
        // Encrypt then decrypt recovers the plaintext after the 4 lead bytes.
        let plain = b"abcdHello";
        let mut r: u16 = 55665;
        let enc: Vec<u8> = plain
            .iter()
            .map(|&p| {
                let c = p ^ (r >> 8) as u8;
                r = (c as u16)
                    .wrapping_add(r)
                    .wrapping_mul(52845)
                    .wrapping_add(22719);
                c
            })
            .collect();
        assert_eq!(decrypt(&enc, 55665, 4), b"Hello");
    }
}
