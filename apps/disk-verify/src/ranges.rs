//! Sector ranges: the skip list the operator types, and the scan plan left
//! after removing it from the disk.
//!
//! Syntax (sectors are logical sectors, numbers are decimal or `0x` hex),
//! items separated by commas and/or whitespace:
//!
//! | item      | meaning                                  |
//! |-----------|------------------------------------------|
//! | `A`       | the one sector `A`                       |
//! | `A-B`     | sectors `A` through `B`, inclusive       |
//! | `A+N`     | `N` sectors starting at `A`              |
//! | `A-`      | from `A` to the end of the disk          |

use std::fmt;

/// A half-open run of sectors, `start..end`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Range {
    pub start: u64,
    pub end: u64,
}

impl Range {
    pub fn new(start: u64, end: u64) -> Self {
        Range { start, end }
    }

    pub fn len(&self) -> u64 {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }
}

impl fmt::Display for Range {
    /// Inclusive, the way the operator typed it.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.len() == 1 {
            write!(f, "{}", self.start)
        } else {
            write!(f, "{}-{}", self.start, self.end - 1)
        }
    }
}

fn number(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let parsed = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => s.parse::<u64>(),
    };
    parsed.map_err(|_| format!("'{s}' is not a sector number"))
}

/// Parse a skip list against a disk of `total` sectors. The result is sorted,
/// merged (overlapping or adjacent ranges become one) and clamped to the disk.
/// A range that starts past the end of the disk is an error: it is almost
/// certainly a typo, and silently ignoring it would scan what the operator
/// meant to skip.
pub fn parse_skips(text: &str, total: u64) -> Result<Vec<Range>, String> {
    let mut out = Vec::new();
    for item in text.split(|c: char| c == ',' || c.is_whitespace()) {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let range = if let Some((a, n)) = item.split_once('+') {
            let start = number(a)?;
            let count = number(n)?;
            if count == 0 {
                return Err(format!("'{item}' skips zero sectors"));
            }
            Range::new(start, start.saturating_add(count))
        } else if let Some((a, b)) = item.split_once('-') {
            let start = number(a)?;
            let end = if b.trim().is_empty() {
                total
            } else {
                number(b)?.saturating_add(1)
            };
            if end <= start {
                return Err(format!("'{item}' ends before it starts"));
            }
            Range::new(start, end)
        } else {
            let s = number(item)?;
            Range::new(s, s.saturating_add(1))
        };
        if range.start >= total {
            return Err(format!(
                "'{item}' starts past the end of the disk ({total} sectors)"
            ));
        }
        out.push(Range::new(range.start, range.end.min(total)));
    }
    Ok(merge(out))
}

/// Sort and coalesce overlapping or touching ranges.
pub fn merge(mut ranges: Vec<Range>) -> Vec<Range> {
    ranges.retain(|r| !r.is_empty());
    ranges.sort_by_key(|r| r.start);
    let mut out: Vec<Range> = Vec::with_capacity(ranges.len());
    for r in ranges {
        match out.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => out.push(r),
        }
    }
    out
}

/// What is left of `0..total` after removing `skips` (sorted and merged, as
/// [`parse_skips`] returns them): the ranges the scan reads.
pub fn plan(total: u64, skips: &[Range]) -> Vec<Range> {
    let mut out = Vec::new();
    let mut at = 0;
    for s in skips {
        if s.start > at {
            out.push(Range::new(at, s.start));
        }
        at = at.max(s.end);
    }
    if at < total {
        out.push(Range::new(at, total));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_form() {
        let r = parse_skips("5, 10-19 30+5\n0x40-", 100).unwrap();
        assert_eq!(
            r,
            vec![
                Range::new(5, 6),
                Range::new(10, 20),
                Range::new(30, 35),
                Range::new(64, 100)
            ]
        );
    }

    #[test]
    fn merges_and_clamps() {
        let r = parse_skips("10-20, 15-30, 31, 90-500", 100).unwrap();
        assert_eq!(r, vec![Range::new(10, 32), Range::new(90, 100)]);
    }

    #[test]
    fn empty_text_skips_nothing() {
        assert_eq!(parse_skips("  ,, ", 100).unwrap(), vec![]);
        assert_eq!(plan(100, &[]), vec![Range::new(0, 100)]);
    }

    #[test]
    fn rejects_nonsense() {
        assert!(parse_skips("abc", 100).is_err());
        assert!(parse_skips("20-10", 100).is_err());
        assert!(parse_skips("5+0", 100).is_err());
        assert!(parse_skips("100", 100).is_err());
        assert!(parse_skips("1--2", 100).is_err());
    }

    #[test]
    fn plan_is_the_complement() {
        let skips = parse_skips("0-9, 50-59, 95-", 100).unwrap();
        assert_eq!(
            plan(100, &skips),
            vec![Range::new(10, 50), Range::new(60, 95)]
        );
        let all = parse_skips("0-", 100).unwrap();
        assert_eq!(plan(100, &all), vec![]);
    }

    #[test]
    fn displays_inclusively() {
        assert_eq!(Range::new(10, 20).to_string(), "10-19");
        assert_eq!(Range::new(7, 8).to_string(), "7");
    }
}
