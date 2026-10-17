//! The headless parts of `disk-verify`: the skip-list parser, the pattern, the
//! surface scan, device discovery, and the `SurfaceMap` widget. `main.rs` is
//! the app that wires them into a UI.

pub mod devices;
pub mod map;
pub mod pattern;
pub mod ranges;
pub mod scan;

/// `1234567` → `1,234,567`.
pub fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `75` → `1m15s`.
pub fn duration(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{h}h{m:02}m")
    } else if m > 0 {
        format!("{m}m{s:02}s")
    } else {
        format!("{s}s")
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn formats() {
        assert_eq!(super::thousands(0), "0");
        assert_eq!(super::thousands(1234), "1,234");
        assert_eq!(super::thousands(1234567), "1,234,567");
        assert_eq!(super::duration(5), "5s");
        assert_eq!(super::duration(75), "1m15s");
        assert_eq!(super::duration(3725), "1h02m");
    }
}
