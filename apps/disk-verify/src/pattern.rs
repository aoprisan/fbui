//! The expected content: a byte sequence repeated across the whole disk,
//! anchored at byte 0 — so byte `o` of the disk must equal
//! `bytes[o % bytes.len()]`, whatever ranges are skipped.

/// A repeating byte pattern, 1 to [`Pattern::MAX_LEN`] bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pattern {
    bytes: Vec<u8>,
}

impl Pattern {
    pub const MAX_LEN: usize = 4096;

    /// Parse the operator's text: hex digits (spaces, `:` and `_` ignored, an
    /// optional `0x` prefix), or one of the words `zero`/`zeros` and
    /// `ones`/`ff`. `"00"` is a zero-filled disk, `"de ad be ef"` a 4-byte cycle.
    pub fn parse(text: &str) -> Result<Pattern, String> {
        let t = text.trim().to_ascii_lowercase();
        match t.as_str() {
            "" => return Err("enter a pattern, e.g. 00 or deadbeef".into()),
            "zero" | "zeros" | "zeroes" => return Ok(Pattern { bytes: vec![0] }),
            "one" | "ones" => return Ok(Pattern { bytes: vec![0xff] }),
            _ => {}
        }
        let t = t.strip_prefix("0x").unwrap_or(&t);
        let digits: Vec<u8> = t
            .bytes()
            .filter(|b| !matches!(b, b' ' | b'\t' | b':' | b'_'))
            .collect();
        if !digits.len().is_multiple_of(2) {
            return Err("pattern needs whole bytes (an even number of hex digits)".into());
        }
        let nibble = |c: u8| match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            _ => None,
        };
        let mut bytes = Vec::with_capacity(digits.len() / 2);
        for pair in digits.chunks(2) {
            match (nibble(pair[0]), nibble(pair[1])) {
                (Some(h), Some(l)) => bytes.push(h << 4 | l),
                _ => return Err(format!("'{text}' is not hex")),
            }
        }
        if bytes.len() > Self::MAX_LEN {
            return Err(format!("pattern is longer than {} bytes", Self::MAX_LEN));
        }
        Ok(Pattern { bytes })
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The byte the disk must hold at absolute offset `offset`.
    pub fn at(&self, offset: u64) -> u8 {
        self.bytes[(offset % self.bytes.len() as u64) as usize]
    }

    /// A short label for the UI: `00`, `de ad be ef`, `01 02 03 … (600 B)`.
    pub fn describe(&self) -> String {
        let shown: Vec<String> = self
            .bytes
            .iter()
            .take(8)
            .map(|b| format!("{b:02x}"))
            .collect();
        let mut s = shown.join(" ");
        if self.bytes.len() > 8 {
            s.push_str(&format!(" … ({} B)", self.bytes.len()));
        }
        s
    }
}

/// A precomputed run of the pattern, long enough that any `len`-byte window
/// starting at any phase is one contiguous slice — so checking a block is a
/// single `memcmp`, not a per-byte modulo.
pub struct Reference {
    period: usize,
    buf: Vec<u8>,
}

impl Reference {
    pub fn new(pattern: &Pattern, len: usize) -> Self {
        let period = pattern.bytes.len();
        let buf = (0..len + period)
            .map(|i| pattern.bytes[i % period])
            .collect();
        Reference { period, buf }
    }

    /// The expected bytes for `len` bytes starting at absolute `offset`.
    pub fn window(&self, offset: u64, len: usize) -> &[u8] {
        let phase = (offset % self.period as u64) as usize;
        &self.buf[phase..phase + len]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_and_words() {
        assert_eq!(Pattern::parse("00").unwrap().bytes(), &[0]);
        assert_eq!(Pattern::parse("zeros").unwrap().bytes(), &[0]);
        assert_eq!(Pattern::parse("ones").unwrap().bytes(), &[0xff]);
        assert_eq!(
            Pattern::parse("0xDE ad:be_EF").unwrap().bytes(),
            &[0xde, 0xad, 0xbe, 0xef]
        );
    }

    #[test]
    fn rejects_bad_hex() {
        assert!(Pattern::parse("").is_err());
        assert!(Pattern::parse("abc").is_err());
        assert!(Pattern::parse("zz").is_err());
        assert!(Pattern::parse(&"00".repeat(Pattern::MAX_LEN + 1)).is_err());
    }

    #[test]
    fn window_matches_at_every_phase() {
        let p = Pattern::parse("010203").unwrap();
        let r = Reference::new(&p, 16);
        for off in 0..10u64 {
            let w = r.window(off, 16);
            for (i, &b) in w.iter().enumerate() {
                assert_eq!(b, p.at(off + i as u64));
            }
        }
    }

    #[test]
    fn describes_compactly() {
        assert_eq!(
            Pattern::parse("deadbeef").unwrap().describe(),
            "de ad be ef"
        );
        let long = Pattern::parse(&"ab".repeat(20)).unwrap().describe();
        assert!(long.ends_with("… (20 B)"), "{long}");
    }
}
