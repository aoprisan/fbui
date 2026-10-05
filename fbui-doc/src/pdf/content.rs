//! Content-stream tokenizing: operands followed by an operator (§7.8.2),
//! plus inline images (`BI … ID <data> EI`).

use alloc::vec::Vec;

use super::lexer::{find, is_white, Lexer, Token};
use super::object::{Dict, Object};

/// Operand stack bound; real content never comes close.
const MAX_OPERANDS: usize = 64;

pub enum Op<'a> {
    /// An operator and its operands.
    Op(&'a [u8], Vec<Object>),
    /// An inline image: its (abbreviation-expanded) dict and raw data.
    InlineImage(Dict, Vec<u8>),
}

pub struct ContentParser<'a> {
    lx: Lexer<'a>,
}

impl<'a> ContentParser<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        ContentParser {
            lx: Lexer::new(data),
        }
    }
}

impl<'a> Iterator for ContentParser<'a> {
    type Item = Op<'a>;

    fn next(&mut self) -> Option<Op<'a>> {
        let mut operands = Vec::new();
        loop {
            let tok = self.lx.next_token()?;
            match tok {
                Token::Keyword(b"BI") => return self.inline_image(),
                Token::Keyword(b"true") => operands.push(Object::Bool(true)),
                Token::Keyword(b"false") => operands.push(Object::Bool(false)),
                Token::Keyword(b"null") => operands.push(Object::Null),
                Token::Keyword(k) => return Some(Op::Op(k, operands)),
                Token::ArrayClose | Token::DictClose | Token::BraceOpen | Token::BraceClose => {}
                t => {
                    let o = self.lx.object_from(t, false, 0).unwrap_or(Object::Null);
                    if operands.len() < MAX_OPERANDS {
                        operands.push(o);
                    }
                }
            }
        }
    }
}

impl<'a> ContentParser<'a> {
    fn inline_image(&mut self) -> Option<Op<'a>> {
        let mut dict = Dict::default();
        loop {
            match self.lx.next_token()? {
                Token::Keyword(b"ID") => break,
                Token::Name(k) => {
                    let v = self.lx.parse_object(false).ok()?;
                    dict.0.push((expand_key(&k).to_vec(), expand_value(v)));
                }
                _ => {}
            }
        }
        // Exactly one whitespace byte separates `ID` from the data.
        if self.lx.peek_byte().is_some_and(is_white) {
            self.lx.pos += 1;
        }
        let data = self.lx.data;
        let start = self.lx.pos;
        // With no filter the length is known; prefer it to searching for
        // `EI`, which can occur inside binary data.
        let known = raw_len(&dict).filter(|&n| start + n <= data.len());
        let end = match known {
            Some(n) => start + n,
            None => {
                let mut from = start;
                loop {
                    match find(data, b"EI", from) {
                        Some(p) => {
                            let before = p == start || is_white(data[p - 1]);
                            let after = data.get(p + 2).is_none_or(|&b| is_white(b));
                            if before && after {
                                break p.saturating_sub(1).max(start);
                            }
                            from = p + 2;
                        }
                        None => break data.len(),
                    }
                }
            }
        };
        let bytes = data[start..end].to_vec();
        // Skip past `EI`.
        self.lx.pos = end;
        match find(data, b"EI", end) {
            Some(p) => self.lx.pos = p + 2,
            None => self.lx.pos = data.len(),
        }
        Some(Op::InlineImage(dict, bytes))
    }
}

fn raw_len(d: &Dict) -> Option<usize> {
    if d.contains(b"Filter") {
        return None;
    }
    let w = d.get(b"Width")?.as_i64()? as usize;
    let h = d.get(b"Height")?.as_i64()? as usize;
    let mask = d
        .get(b"ImageMask")
        .and_then(Object::as_bool)
        .unwrap_or(false);
    let bpc = if mask {
        1
    } else {
        d.get(b"BitsPerComponent")?.as_i64()? as usize
    };
    let comps = if mask {
        1
    } else {
        match d.get(b"ColorSpace") {
            Some(Object::Name(n)) => match n.as_slice() {
                b"DeviceGray" | b"CalGray" | b"Indexed" => 1,
                b"DeviceRGB" | b"CalRGB" => 3,
                b"DeviceCMYK" => 4,
                _ => return None,
            },
            Some(Object::Array(a)) if a.first().and_then(Object::as_name) == Some(b"Indexed") => 1,
            _ => return None,
        }
    };
    Some((w * comps * bpc).div_ceil(8) * h)
}

fn expand_key(k: &[u8]) -> &[u8] {
    match k {
        b"BPC" => b"BitsPerComponent",
        b"CS" => b"ColorSpace",
        b"D" => b"Decode",
        b"DP" => b"DecodeParms",
        b"F" => b"Filter",
        b"H" => b"Height",
        b"IM" => b"ImageMask",
        b"I" => b"Interpolate",
        b"W" => b"Width",
        other => other,
    }
}

fn expand_name(n: &[u8]) -> &[u8] {
    match n {
        b"G" => b"DeviceGray",
        b"RGB" => b"DeviceRGB",
        b"CMYK" => b"DeviceCMYK",
        b"I" => b"Indexed",
        b"AHx" => b"ASCIIHexDecode",
        b"A85" => b"ASCII85Decode",
        b"LZW" => b"LZWDecode",
        b"Fl" => b"FlateDecode",
        b"RL" => b"RunLengthDecode",
        b"CCF" => b"CCITTFaxDecode",
        b"DCT" => b"DCTDecode",
        other => other,
    }
}

fn expand_value(v: Object) -> Object {
    match v {
        Object::Name(n) => Object::Name(expand_name(&n).to_vec()),
        Object::Array(a) => Object::Array(a.into_iter().map(expand_value).collect()),
        o => o,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ops_and_inline_image() {
        let src = b"1 0 0 RG 10 20 m 30 40 l S BI /W 2 /H 1 /CS /G /BPC 8 ID \x00\xff EI Q";
        let ops: Vec<_> = ContentParser::new(src).collect();
        let names: Vec<&[u8]> = ops
            .iter()
            .map(|o| match o {
                Op::Op(k, _) => *k,
                Op::InlineImage(..) => b"<img>",
            })
            .collect();
        assert_eq!(names, [&b"RG"[..], b"m", b"l", b"S", b"<img>", b"Q"]);
        if let Op::InlineImage(d, data) = &ops[4] {
            assert_eq!(d.name(b"ColorSpace"), Some(&b"DeviceGray"[..]));
            assert_eq!(data, &[0x00, 0xff]);
        }
    }
}
