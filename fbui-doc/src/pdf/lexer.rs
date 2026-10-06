//! Tokenizer and object parser (ISO 32000-1 §7.2–7.3). Lenient by design: a
//! viewer should show what it can of a slightly broken file, not reject it.

use alloc::vec::Vec;

use super::object::{Dict, Object, Ref};
use super::{Error, Result};

/// Nesting limit for arrays/dicts, so a hostile file can't overflow the stack.
const MAX_DEPTH: u32 = 64;

#[derive(Debug, Clone, PartialEq)]
pub enum Token<'a> {
    Int(i64),
    Real(f32),
    String(Vec<u8>),
    Name(Vec<u8>),
    ArrayOpen,
    ArrayClose,
    DictOpen,
    DictClose,
    BraceOpen,
    BraceClose,
    /// A bare word: `obj`, `R`, `true`, a content-stream operator, …
    Keyword(&'a [u8]),
}

pub fn is_white(b: u8) -> bool {
    matches!(b, b'\0' | b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')
}

pub fn is_delim(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

fn is_regular(b: u8) -> bool {
    !is_white(b) && !is_delim(b)
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[derive(Clone)]
pub struct Lexer<'a> {
    pub data: &'a [u8],
    pub pos: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Lexer { data, pos: 0 }
    }

    pub fn at(data: &'a [u8], pos: usize) -> Self {
        Lexer {
            data,
            pos: pos.min(data.len()),
        }
    }

    pub fn peek_byte(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    pub fn skip_ws(&mut self) {
        while let Some(b) = self.peek_byte() {
            if is_white(b) {
                self.pos += 1;
            } else if b == b'%' {
                while let Some(c) = self.peek_byte() {
                    if c == b'\n' || c == b'\r' {
                        break;
                    }
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    pub fn at_end(&mut self) -> bool {
        self.skip_ws();
        self.pos >= self.data.len()
    }

    /// The next token, or `None` at end of input.
    pub fn next_token(&mut self) -> Option<Token<'a>> {
        self.skip_ws();
        let b = self.peek_byte()?;
        let tok = match b {
            b'[' => {
                self.pos += 1;
                Token::ArrayOpen
            }
            b']' => {
                self.pos += 1;
                Token::ArrayClose
            }
            b'{' => {
                self.pos += 1;
                Token::BraceOpen
            }
            b'}' => {
                self.pos += 1;
                Token::BraceClose
            }
            b'<' => {
                if self.data.get(self.pos + 1) == Some(&b'<') {
                    self.pos += 2;
                    Token::DictOpen
                } else {
                    self.pos += 1;
                    Token::String(self.hex_string())
                }
            }
            b'>' => {
                // `>>` closes a dict; a stray `>` is skipped as one.
                self.pos += 1;
                if self.peek_byte() == Some(b'>') {
                    self.pos += 1;
                }
                Token::DictClose
            }
            b'(' => {
                self.pos += 1;
                Token::String(self.literal_string())
            }
            b'/' => {
                self.pos += 1;
                Token::Name(self.name())
            }
            b')' => {
                // Unbalanced close paren: skip it.
                self.pos += 1;
                return self.next_token();
            }
            b'+' | b'-' | b'.' | b'0'..=b'9' => self.number(),
            _ => {
                let start = self.pos;
                while self.peek_byte().is_some_and(is_regular) {
                    self.pos += 1;
                }
                if self.pos == start {
                    self.pos += 1;
                }
                Token::Keyword(&self.data[start..self.pos])
            }
        };
        Some(tok)
    }

    fn number(&mut self) -> Token<'a> {
        let start = self.pos;
        let mut neg = false;
        // Accept (and fold) runs of signs, as some producers emit `--5`.
        while let Some(c @ (b'+' | b'-')) = self.peek_byte() {
            if c == b'-' {
                neg = !neg;
            }
            self.pos += 1;
        }
        let mut int: i64 = 0;
        let mut frac: f64 = 0.0;
        let mut scale = 1.0f64;
        let mut real = false;
        let mut digits = 0;
        while let Some(c) = self.peek_byte() {
            match c {
                b'0'..=b'9' => {
                    digits += 1;
                    if real {
                        scale /= 10.0;
                        frac += (c - b'0') as f64 * scale;
                    } else {
                        int = int.saturating_mul(10).saturating_add((c - b'0') as i64);
                    }
                }
                b'.' if !real => real = true,
                // Stray signs/dots inside a number end it.
                _ => break,
            }
            self.pos += 1;
        }
        if digits == 0 {
            // `+`, `-`, `.` alone: treat as a keyword so the caller can skip it.
            if self.pos == start {
                self.pos += 1;
            }
            return Token::Keyword(&self.data[start..self.pos]);
        }
        if real {
            let v = (int as f64 + frac) as f32;
            Token::Real(if neg { -v } else { v })
        } else {
            Token::Int(if neg { -int } else { int })
        }
    }

    fn name(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        while let Some(b) = self.peek_byte() {
            if !is_regular(b) {
                break;
            }
            self.pos += 1;
            if b == b'#' {
                let h = self.data.get(self.pos).copied().and_then(hex_val);
                let l = self.data.get(self.pos + 1).copied().and_then(hex_val);
                if let (Some(h), Some(l)) = (h, l) {
                    out.push(h << 4 | l);
                    self.pos += 2;
                    continue;
                }
            }
            out.push(b);
        }
        out
    }

    fn hex_string(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut hi: Option<u8> = None;
        while let Some(b) = self.peek_byte() {
            self.pos += 1;
            if b == b'>' {
                break;
            }
            if let Some(v) = hex_val(b) {
                match hi.take() {
                    Some(h) => out.push(h << 4 | v),
                    None => hi = Some(v),
                }
            }
        }
        if let Some(h) = hi {
            out.push(h << 4);
        }
        out
    }

    fn literal_string(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut depth = 1u32;
        while let Some(b) = self.peek_byte() {
            self.pos += 1;
            match b {
                b'(' => {
                    depth += 1;
                    out.push(b);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    out.push(b);
                }
                b'\\' => {
                    let Some(e) = self.peek_byte() else { break };
                    self.pos += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'0'..=b'7' => {
                            let mut v = (e - b'0') as u32;
                            for _ in 0..2 {
                                match self.peek_byte() {
                                    Some(d @ b'0'..=b'7') => {
                                        v = v * 8 + (d - b'0') as u32;
                                        self.pos += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push(v as u8);
                        }
                        b'\r' => {
                            // Line continuation; swallow a following LF too.
                            if self.peek_byte() == Some(b'\n') {
                                self.pos += 1;
                            }
                        }
                        b'\n' => {}
                        other => out.push(other),
                    }
                }
                b'\r' => {
                    // Raw end-of-line in a string reads as a single LF.
                    if self.peek_byte() == Some(b'\n') {
                        self.pos += 1;
                    }
                    out.push(b'\n');
                }
                _ => out.push(b),
            }
        }
        out
    }

    /// Parse one object. With `refs`, `n g R` becomes an [`Object::Ref`]
    /// (file syntax); content streams pass `false`.
    pub fn parse_object(&mut self, refs: bool) -> Result<Object> {
        let tok = self
            .next_token()
            .ok_or(Error::Syntax("unexpected end of data"))?;
        self.object_from(tok, refs, 0)
    }

    pub fn object_from(&mut self, tok: Token<'a>, refs: bool, depth: u32) -> Result<Object> {
        if depth > MAX_DEPTH {
            return Err(Error::Syntax("objects nested too deeply"));
        }
        Ok(match tok {
            Token::Int(i) => {
                if refs {
                    if let Some(r) = self.try_ref(i) {
                        return Ok(Object::Ref(r));
                    }
                }
                Object::Int(i)
            }
            Token::Real(r) => Object::Real(r),
            Token::String(s) => Object::String(s),
            Token::Name(n) => Object::Name(n),
            Token::ArrayOpen => {
                let mut items = Vec::new();
                loop {
                    match self.next_token() {
                        None | Some(Token::ArrayClose) => break,
                        // A stray dict close inside an array: skip it.
                        Some(Token::DictClose) => continue,
                        Some(t) => items.push(self.object_from(t, refs, depth + 1)?),
                    }
                }
                Object::Array(items)
            }
            Token::DictOpen => Object::Dict(self.dict_body(refs, depth)?),
            Token::Keyword(b"true") => Object::Bool(true),
            Token::Keyword(b"false") => Object::Bool(false),
            Token::Keyword(b"null") => Object::Null,
            Token::Keyword(_) | Token::ArrayClose | Token::DictClose => Object::Null,
            Token::BraceOpen | Token::BraceClose => Object::Null,
        })
    }

    fn dict_body(&mut self, refs: bool, depth: u32) -> Result<Dict> {
        let mut dict = Dict::default();
        loop {
            match self.next_token() {
                None | Some(Token::DictClose) => break,
                Some(Token::Name(key)) => {
                    let Some(vt) = self.next_token() else { break };
                    if vt == Token::DictClose {
                        // `/Key >>`: key with no value.
                        break;
                    }
                    let v = self.object_from(vt, refs, depth + 1)?;
                    dict.0.push((key, v));
                }
                // Garbage where a key should be: skip it.
                Some(_) => continue,
            }
        }
        Ok(dict)
    }

    /// After an integer, look ahead for `gen R`; rewind if absent.
    fn try_ref(&mut self, num: i64) -> Option<Ref> {
        let save = self.pos;
        if let Some(Token::Int(gen)) = self.next_token() {
            if let Some(Token::Keyword(b"R")) = self.next_token() {
                if (0..=u32::MAX as i64).contains(&num) && (0..=u16::MAX as i64).contains(&gen) {
                    return Some(Ref {
                        num: num as u32,
                        gen: gen as u16,
                    });
                }
            }
        }
        self.pos = save;
        None
    }

    /// Skip a single end-of-line (CRLF, LF or a lone CR) — what follows the
    /// `stream` keyword and `ID` of an inline image.
    pub fn skip_eol(&mut self) {
        match self.peek_byte() {
            Some(b'\r') => {
                self.pos += 1;
                if self.peek_byte() == Some(b'\n') {
                    self.pos += 1;
                }
            }
            Some(b'\n') => self.pos += 1,
            _ => {}
        }
    }
}

/// Find `needle` in `hay` starting at `from`.
pub fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from >= hay.len() {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

/// Find the last `needle` in `hay`.
pub fn rfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    hay.windows(needle.len()).rposition(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn obj(s: &str) -> Object {
        Lexer::new(s.as_bytes()).parse_object(true).unwrap()
    }

    #[test]
    fn scalars() {
        assert_eq!(obj("42"), Object::Int(42));
        assert_eq!(obj("-3.5"), Object::Real(-3.5));
        assert_eq!(obj(".5"), Object::Real(0.5));
        assert_eq!(obj("true"), Object::Bool(true));
        assert_eq!(obj("/A#20B"), Object::Name(b"A B".to_vec()));
        assert_eq!(obj("(a\\(b\\)\\101)"), Object::String(b"a(b)A".to_vec()));
        assert_eq!(obj("(x(y)z)"), Object::String(b"x(y)z".to_vec()));
        assert_eq!(obj("<48 69 7>"), Object::String(b"Hip".to_vec()));
    }

    #[test]
    fn containers_and_refs() {
        let o = obj("<< /Type /Page /Kids [1 0 R 2 0 R] /N 5 /M [1 2] >>");
        let d = o.as_dict().unwrap();
        assert_eq!(d.name(b"Type"), Some(&b"Page"[..]));
        assert_eq!(
            d.get(b"Kids"),
            Some(&Object::Array(vec![
                Object::Ref(Ref { num: 1, gen: 0 }),
                Object::Ref(Ref { num: 2, gen: 0 })
            ]))
        );
        assert_eq!(d.get(b"N"), Some(&Object::Int(5)));
        assert_eq!(
            d.get(b"M"),
            Some(&Object::Array(vec![Object::Int(1), Object::Int(2)]))
        );
    }

    #[test]
    fn hostile_nesting_is_an_error_not_a_crash() {
        let deep = "[".repeat(10_000);
        assert!(Lexer::new(deep.as_bytes()).parse_object(true).is_err());
    }
}
