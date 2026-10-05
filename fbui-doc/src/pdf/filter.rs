//! Stream filters (ISO 32000-1 §7.4): Flate (+ PNG/TIFF predictors), LZW,
//! ASCIIHex, ASCII85, RunLength. Image codecs (DCT, JPX, JBIG2, CCITT) are
//! not byte filters — decoding stops in front of them and hands the rest to
//! the image path.

use alloc::vec;
use alloc::vec::Vec;

use super::object::{Dict, Object};
use super::{Error, Result};

/// Cap on any one decoded stream, so a zip bomb fails instead of exhausting
/// a small target's heap.
pub const MAX_DECODED: usize = 256 * 1024 * 1024;

/// The result of running a stream's byte filters.
pub struct Decoded {
    pub data: Vec<u8>,
    /// An image codec left to apply (`DCTDecode`, …) and its parms.
    pub image_filter: Option<(Vec<u8>, Option<Dict>)>,
}

/// Apply `filters` (with matching `parms`) to `data`, in order.
pub fn decode(data: &[u8], filters: &[Vec<u8>], parms: &[Option<Dict>]) -> Result<Decoded> {
    let mut cur: Option<Vec<u8>> = None;
    for (i, f) in filters.iter().enumerate() {
        let parm = parms.get(i).cloned().flatten();
        let input: &[u8] = cur.as_deref().unwrap_or(data);
        let out = match f.as_slice() {
            b"FlateDecode" | b"Fl" => predict(inflate(input)?, parm.as_ref())?,
            b"LZWDecode" | b"LZW" => {
                let early = parm
                    .as_ref()
                    .and_then(|p| p.get(b"EarlyChange"))
                    .and_then(Object::as_i64)
                    .unwrap_or(1);
                predict(lzw(input, early != 0)?, parm.as_ref())?
            }
            b"ASCIIHexDecode" | b"AHx" => ascii_hex(input),
            b"ASCII85Decode" | b"A85" => ascii85(input),
            b"RunLengthDecode" | b"RL" => run_length(input),
            b"Crypt" => input.to_vec(),
            b"DCTDecode" | b"DCT" | b"JPXDecode" | b"JBIG2Decode" | b"CCITTFaxDecode" | b"CCF" => {
                return Ok(Decoded {
                    data: cur.unwrap_or_else(|| data.to_vec()),
                    image_filter: Some((f.clone(), parm)),
                });
            }
            _ => return Err(Error::Unsupported("stream filter")),
        };
        cur = Some(out);
    }
    Ok(Decoded {
        data: cur.unwrap_or_else(|| data.to_vec()),
        image_filter: None,
    })
}

/// zlib inflate, tolerant of a truncated or corrupt tail (keeps what
/// decoded) and of a missing zlib header (raw deflate).
pub fn inflate(input: &[u8]) -> Result<Vec<u8>> {
    use miniz_oxide::inflate::{decompress_to_vec_with_limit, decompress_to_vec_zlib_with_limit};
    match decompress_to_vec_zlib_with_limit(input, MAX_DECODED) {
        Ok(v) => Ok(v),
        Err(e) if !e.output.is_empty() => Ok(e.output),
        Err(_) => match decompress_to_vec_with_limit(input, MAX_DECODED) {
            Ok(v) => Ok(v),
            Err(e) if !e.output.is_empty() => Ok(e.output),
            Err(_) => Err(Error::Corrupt("flate stream")),
        },
    }
}

fn parm_i(parm: Option<&Dict>, key: &[u8], default: i64) -> i64 {
    parm.and_then(|p| p.get(key))
        .and_then(Object::as_i64)
        .unwrap_or(default)
}

/// Undo a PNG (10–15) or TIFF (2) predictor.
fn predict(data: Vec<u8>, parm: Option<&Dict>) -> Result<Vec<u8>> {
    let predictor = parm_i(parm, b"Predictor", 1);
    if predictor <= 1 {
        return Ok(data);
    }
    let colors = parm_i(parm, b"Colors", 1).clamp(1, 32) as usize;
    let bpc = parm_i(parm, b"BitsPerComponent", 8).clamp(1, 16) as usize;
    let columns = parm_i(parm, b"Columns", 1).clamp(1, 1 << 24) as usize;
    let bpp = (colors * bpc).div_ceil(8).max(1);
    let row = (colors * bpc * columns).div_ceil(8);

    if predictor == 2 {
        // TIFF horizontal differencing; only the common 8-bit case.
        let mut out = data;
        if bpc == 8 {
            for line in out.chunks_mut(row) {
                for i in bpp..line.len() {
                    line[i] = line[i].wrapping_add(line[i - bpp]);
                }
            }
        }
        return Ok(out);
    }

    // PNG: each row is prefixed with a filter-type byte.
    let mut out = Vec::with_capacity(data.len());
    let mut prev = vec![0u8; row];
    for chunk in data.chunks(row + 1) {
        if chunk.len() < 2 {
            break;
        }
        let ty = chunk[0];
        let mut cur = vec![0u8; row];
        cur[..chunk.len() - 1].copy_from_slice(&chunk[1..]);
        for i in 0..row {
            let a = if i >= bpp { cur[i - bpp] } else { 0 };
            let b = prev[i];
            let c = if i >= bpp { prev[i - bpp] } else { 0 };
            cur[i] = match ty {
                0 => cur[i],
                1 => cur[i].wrapping_add(a),
                2 => cur[i].wrapping_add(b),
                3 => cur[i].wrapping_add(((a as u16 + b as u16) / 2) as u8),
                4 => cur[i].wrapping_add(paeth(a, b, c)),
                _ => cur[i],
            };
        }
        out.extend_from_slice(&cur);
        prev = cur;
    }
    Ok(out)
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i16 + b as i16 - c as i16;
    let (pa, pb, pc) = (
        (p - a as i16).abs(),
        (p - b as i16).abs(),
        (p - c as i16).abs(),
    );
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

fn ascii_hex(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len() / 2);
    let mut hi: Option<u8> = None;
    for &b in input {
        if b == b'>' {
            break;
        }
        let v = match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            _ => continue,
        };
        match hi.take() {
            Some(h) => out.push(h << 4 | v),
            None => hi = Some(v),
        }
    }
    if let Some(h) = hi {
        out.push(h << 4);
    }
    out
}

fn ascii85(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len() * 4 / 5);
    let mut group = [0u8; 5];
    let mut n = 0;
    let mut i = 0;
    // Optional `<~` prefix.
    if input.starts_with(b"<~") {
        i = 2;
    }
    while i < input.len() {
        let b = input[i];
        i += 1;
        match b {
            b'~' => break,
            b'z' if n == 0 => out.extend_from_slice(&[0, 0, 0, 0]),
            b'!'..=b'u' => {
                group[n] = b - b'!';
                n += 1;
                if n == 5 {
                    let v = group.iter().fold(0u64, |acc, &d| acc * 85 + d as u64) as u32;
                    out.extend_from_slice(&v.to_be_bytes());
                    n = 0;
                }
            }
            _ => {}
        }
    }
    if n > 1 {
        for g in group.iter_mut().skip(n) {
            *g = 84;
        }
        let v = group.iter().fold(0u64, |acc, &d| acc * 85 + d as u64) as u32;
        out.extend_from_slice(&v.to_be_bytes()[..n - 1]);
    }
    out
}

fn run_length(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < input.len() {
        let len = input[i];
        i += 1;
        match len {
            128 => break,
            0..=127 => {
                let n = len as usize + 1;
                let end = (i + n).min(input.len());
                out.extend_from_slice(&input[i..end]);
                i = end;
            }
            _ => {
                if let Some(&b) = input.get(i) {
                    out.extend(core::iter::repeat_n(b, 257 - len as usize));
                }
                i += 1;
            }
        }
    }
    out
}

/// LZW with 9–12 bit codes, MSB first (the TIFF/PDF variant).
fn lzw(input: &[u8], early_change: bool) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut table: Vec<Vec<u8>> = Vec::new();
    let reset = |t: &mut Vec<Vec<u8>>| {
        t.clear();
        for i in 0..256u16 {
            t.push(vec![i as u8]);
        }
        t.push(Vec::new()); // 256 clear
        t.push(Vec::new()); // 257 EOD
    };
    reset(&mut table);
    let mut bits = 9u32;
    let mut acc: u32 = 0;
    let mut nacc = 0u32;
    let mut prev: Option<usize> = None;
    let early = u32::from(early_change);
    for &byte in input {
        acc = (acc << 8) | byte as u32;
        nacc += 8;
        while nacc >= bits {
            let code = ((acc >> (nacc - bits)) & ((1 << bits) - 1)) as usize;
            nacc -= bits;
            if code == 256 {
                reset(&mut table);
                bits = 9;
                prev = None;
                continue;
            }
            if code == 257 {
                return Ok(out);
            }
            let entry = if code < table.len() {
                let e = table[code].clone();
                if let Some(p) = prev {
                    let mut n = table[p].clone();
                    n.push(e[0]);
                    table.push(n);
                }
                e
            } else if let Some(p) = prev {
                let mut e = table[p].clone();
                e.push(table[p][0]);
                table.push(e.clone());
                e
            } else {
                return Err(Error::Corrupt("lzw code"));
            };
            if out.len() + entry.len() > MAX_DECODED {
                return Err(Error::Corrupt("lzw output too large"));
            }
            out.extend_from_slice(&entry);
            prev = Some(code);
            let next = table.len() as u32 + early;
            bits = if next >= 2048 {
                12
            } else if next >= 1024 {
                11
            } else if next >= 512 {
                10
            } else {
                9
            };
            if table.len() >= 4096 {
                // Table full without a clear code: stop growing.
                bits = 12;
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_filters() {
        assert_eq!(ascii_hex(b"48 65 6c6C6f>"), b"Hello");
        assert_eq!(ascii85(b"87cURD]i,\"Ebo80~>"), b"Hello World!");
        assert_eq!(ascii85(b"z~>"), [0, 0, 0, 0]);
    }

    #[test]
    fn run_length_roundtrip() {
        // literal "ab", then 3x 'c', then EOD
        assert_eq!(run_length(&[1, b'a', b'b', 254, b'c', 128]), b"abccc");
    }

    #[test]
    fn lzw_spec_example() {
        // ISO 32000-1 §7.4.4.2 example: "-----A---B" encodes to these bytes.
        let enc = [0x80, 0x0B, 0x60, 0x50, 0x22, 0x0C, 0x0C, 0x85, 0x01];
        assert_eq!(lzw(&enc, true).unwrap(), b"-----A---B");
    }

    #[test]
    fn flate_with_png_up_predictor() {
        // Two 3-byte rows; row 2 uses the Up filter (+ row above).
        let raw = [0u8, 1, 2, 3, 2, 1, 1, 1];
        let z = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6);
        let mut p = Dict::default();
        p.insert(b"Predictor", Object::Int(12));
        p.insert(b"Columns", Object::Int(3));
        let out = predict(inflate(&z).unwrap(), Some(&p)).unwrap();
        assert_eq!(out, [1, 2, 3, 2, 3, 4]);
    }
}
