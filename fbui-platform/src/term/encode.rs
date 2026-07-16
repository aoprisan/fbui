//! Pure escape-sequence encoders for the terminal backend.
//!
//! Everything here turns pixels into bytes and touches no fd, so it is unit
//! tested exhaustively and the [`TermDisplay`](super::display::TermDisplay)
//! stays a thin I/O shell around it. Two encodings:
//!
//! * **Kitty graphics protocol** — full-resolution RGB, base64-chunked APC
//!   escapes. A frame is one *base* image; damage is expressed as small *patch*
//!   images placed above it at a pixel offset, so a button repaint costs bytes
//!   proportional to the button, not the screen. Consolidation (retransmitting
//!   the base and deleting the patches) keeps the terminal's image store
//!   bounded.
//! * **Half-block cells** — one `▀` per character cell, foreground = upper
//!   pixel, background = lower pixel, 24-bit SGR colors, damage mapped to cell
//!   runs. Works in any truecolor terminal, no graphics support needed.
//!
//! The shadow buffer is little-endian `Xrgb8888` (memory order `[B,G,R,X]`),
//! exactly what [`PixelFormat::Xrgb8888`](crate::PixelFormat) promises.

use std::io::Write as _;

use crate::geom::Rect;

/// Kitty caps an APC payload at 4096 bytes of base64; we chunk at that limit.
const KITTY_CHUNK: usize = 4096;

/// Standard base64 (RFC 4648, with padding). ~20 lines beats a dependency.
pub fn base64(data: &[u8]) -> String {
    const AL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(AL[(n >> 18) as usize & 63] as char);
        out.push(AL[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            AL[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            AL[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Extract tightly-packed RGB bytes for `rect` from an Xrgb8888 shadow buffer.
///
/// `rect` must already be clamped to the buffer. Memory order per pixel is
/// `[B,G,R,X]`, so RGB is bytes `[2,1,0]`.
pub fn rgb_of_rect(shadow: &[u8], stride: usize, rect: Rect) -> Vec<u8> {
    let (x, y, w, h) = (
        rect.x as usize,
        rect.y as usize,
        rect.w as usize,
        rect.h as usize,
    );
    let mut rgb = Vec::with_capacity(w * h * 3);
    for row in y..y + h {
        let base = row * stride + x * 4;
        let line = &shadow[base..base + w * 4];
        for px in line.chunks_exact(4) {
            rgb.extend_from_slice(&[px[2], px[1], px[0]]);
        }
    }
    rgb
}

/// Where a kitty image goes: the base frame or a patch above it.
#[derive(Debug, Clone, Copy)]
pub struct KittyPlacement {
    /// Image id (`i=`). The base ping-pongs between two ids; patches count up.
    pub id: u32,
    /// Stacking order (`z=`); the base sits at 0, patches climb above it.
    pub z: i32,
    /// Pixel offset inside the anchor cell (`X=`/`Y=`), for patches that don't
    /// start on a cell boundary.
    pub x_off: u32,
    pub y_off: u32,
}

/// Append a kitty *transmit-and-display* (`a=T,f=24`) for an RGB payload of
/// `w`×`h` pixels, chunked per the protocol. The placement anchors at the
/// current cursor position; callers emit a cursor move first. `q=2` suppresses
/// terminal responses so the reply never pollutes the input stream, and `C=1`
/// keeps the cursor where it was.
pub fn kitty_transmit(out: &mut Vec<u8>, rgb: &[u8], w: u32, h: u32, place: KittyPlacement) {
    let payload = base64(rgb);
    let mut chunks = payload.as_bytes().chunks(KITTY_CHUNK).peekable();
    let mut first = true;
    while let Some(chunk) = chunks.next() {
        let last = chunks.peek().is_none();
        out.extend_from_slice(b"\x1b_G");
        if first {
            let _ = write!(
                out,
                "a=T,f=24,s={},v={},i={},p=1,z={},X={},Y={},q=2,C=1,m={}",
                w,
                h,
                place.id,
                place.z,
                place.x_off,
                place.y_off,
                if last { 0 } else { 1 }
            );
            first = false;
        } else {
            let _ = write!(out, "m={}", if last { 0 } else { 1 });
        }
        out.push(b';');
        out.extend_from_slice(chunk);
        out.extend_from_slice(b"\x1b\\");
    }
}

/// Append a kitty delete for one image id (`a=d,d=I` frees the placement *and*
/// the transmitted data, so the terminal's store doesn't grow).
pub fn kitty_delete(out: &mut Vec<u8>, id: u32) {
    let _ = write!(out, "\x1b_Ga=d,d=I,i={id},q=2\x1b\\");
}

/// Append a kitty delete-everything (`d=A`), used on teardown.
pub fn kitty_delete_all(out: &mut Vec<u8>) {
    out.extend_from_slice(b"\x1b_Ga=d,d=A,q=2\x1b\\");
}

/// Append `CSI row;col H` (1-based).
pub fn csi_goto(out: &mut Vec<u8>, row: u32, col: u32) {
    let _ = write!(out, "\x1b[{row};{col}H");
}

/// Re-emit the character cells covering `damage` (pixel coordinates, already
/// clamped) as half-blocks: each cell shows pixel `(cx, 2·cy)` as its upper
/// half (foreground) and `(cx, 2·cy+1)` as its lower half (background).
///
/// Consecutive cells sharing both colors reuse the active SGR, so a solid
/// region costs one escape plus one `▀` per cell. Ends with `SGR 0` so nothing
/// leaks into later terminal output.
pub fn cells_emit(out: &mut Vec<u8>, shadow: &[u8], stride: usize, surface_h: u32, damage: Rect) {
    if damage.is_empty() {
        return;
    }
    let cx0 = damage.x as u32;
    let cx1 = damage.right() as u32; // 1 px per cell horizontally
    let cy0 = damage.y as u32 / 2;
    let cy1 = (damage.bottom() as u32).div_ceil(2);

    /// An (r, g, b) pixel.
    type Rgb = (u8, u8, u8);
    let px = |x: u32, y: u32| -> Rgb {
        if y >= surface_h {
            return (0, 0, 0);
        }
        let i = y as usize * stride + x as usize * 4;
        (shadow[i + 2], shadow[i + 1], shadow[i]) // [B,G,R,X] -> (r,g,b)
    };

    for cy in cy0..cy1 {
        csi_goto(out, cy + 1, cx0 + 1);
        let mut cur: Option<(Rgb, Rgb)> = None;
        for cx in cx0..cx1 {
            let top = px(cx, cy * 2);
            let bot = px(cx, cy * 2 + 1);
            if cur != Some((top, bot)) {
                let _ = write!(
                    out,
                    "\x1b[38;2;{};{};{};48;2;{};{};{}m",
                    top.0, top.1, top.2, bot.0, bot.1, bot.2
                );
                cur = Some((top, bot));
            }
            out.extend_from_slice("▀".as_bytes());
        }
    }
    out.extend_from_slice(b"\x1b[0m");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Size;

    fn b64_decode(s: &str) -> Vec<u8> {
        const AL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let val = |c: u8| AL.iter().position(|&a| a == c).unwrap() as u32;
        let s: Vec<u8> = s.bytes().filter(|&c| c != b'=').collect();
        let mut out = Vec::new();
        for chunk in s.chunks(4) {
            let mut n = 0u32;
            for (i, &c) in chunk.iter().enumerate() {
                n |= val(c) << (18 - 6 * i);
            }
            out.push((n >> 16) as u8);
            if chunk.len() > 2 {
                out.push((n >> 8) as u8);
            }
            if chunk.len() > 3 {
                out.push(n as u8);
            }
        }
        out
    }

    #[test]
    fn base64_round_trips() {
        for data in [
            b"".as_slice(),
            b"f",
            b"fo",
            b"foo",
            b"foob",
            b"fooba",
            b"foobar",
            &[0u8, 255, 128, 7, 9],
        ] {
            assert_eq!(b64_decode(&base64(data)), data, "payload {data:?}");
        }
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(b"fo"), "Zm8=");
    }

    /// Build a w×h Xrgb8888 buffer (with optional stride padding) where each
    /// pixel encodes its coordinates: r=x, g=y, b=42.
    fn coord_buffer(w: u32, h: u32, pad: usize) -> (Vec<u8>, usize) {
        let stride = w as usize * 4 + pad;
        let mut buf = vec![0u8; stride * h as usize];
        for y in 0..h {
            for x in 0..w {
                let i = y as usize * stride + x as usize * 4;
                buf[i] = 42; // B
                buf[i + 1] = y as u8; // G
                buf[i + 2] = x as u8; // R
                buf[i + 3] = 0xff; // X
            }
        }
        (buf, stride)
    }

    #[test]
    fn rgb_extraction_honors_stride_and_rect() {
        let (buf, stride) = coord_buffer(8, 4, 12);
        let rgb = rgb_of_rect(&buf, stride, Rect::new(2, 1, 3, 2));
        assert_eq!(rgb.len(), 3 * 3 * 2);
        // First pixel of the rect is (x=2, y=1) -> r=2, g=1, b=42.
        assert_eq!(&rgb[0..3], &[2, 1, 42]);
        // Last pixel is (x=4, y=2).
        assert_eq!(&rgb[rgb.len() - 3..], &[4, 2, 42]);
    }

    #[test]
    fn kitty_transmit_single_chunk_shape() {
        let mut out = Vec::new();
        kitty_transmit(
            &mut out,
            &[1, 2, 3],
            1,
            1,
            KittyPlacement {
                id: 7,
                z: 3,
                x_off: 5,
                y_off: 9,
            },
        );
        let s = String::from_utf8(out).unwrap();
        assert_eq!(
            s,
            format!(
                "\x1b_Ga=T,f=24,s=1,v=1,i=7,p=1,z=3,X=5,Y=9,q=2,C=1,m=0;{}\x1b\\",
                base64(&[1, 2, 3])
            )
        );
    }

    #[test]
    fn kitty_transmit_chunks_large_payloads_and_data_survives() {
        // 3 KiB of RGB -> 4 KiB of base64 exactly at the chunk edge, plus more.
        let rgb: Vec<u8> = (0..9000u32).map(|i| (i % 251) as u8).collect();
        let mut out = Vec::new();
        kitty_transmit(
            &mut out,
            &rgb,
            60,
            50,
            KittyPlacement {
                id: 1,
                z: 0,
                x_off: 0,
                y_off: 0,
            },
        );
        let s = String::from_utf8(out).unwrap();

        // Every escape is APC ... ST; first has the keys, rest only m=.
        let parts: Vec<&str> = s
            .split("\x1b\\")
            .filter(|p| !p.is_empty())
            .map(|p| p.strip_prefix("\x1b_G").expect("APC intro"))
            .collect();
        assert!(
            parts.len() > 1,
            "expected chunking, got {} part(s)",
            parts.len()
        );
        assert!(parts[0].starts_with("a=T,f=24,s=60,v=50,i=1,"));
        assert!(parts[0].contains(",m=1;"));
        for mid in &parts[1..parts.len() - 1] {
            assert!(mid.starts_with("m=1;"), "middle chunk: {mid}");
        }
        assert!(parts.last().unwrap().starts_with("m=0;"));

        // Reassemble the base64 and check the pixels round-trip byte-for-byte.
        let payload: String = parts.iter().map(|p| p.split_once(';').unwrap().1).collect();
        assert_eq!(b64_decode(&payload), rgb);
        // No chunk exceeds the protocol limit.
        for p in &parts {
            assert!(p.split_once(';').unwrap().1.len() <= KITTY_CHUNK);
        }
    }

    #[test]
    fn kitty_deletes() {
        let mut out = Vec::new();
        kitty_delete(&mut out, 12);
        kitty_delete_all(&mut out);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\x1b_Ga=d,d=I,i=12,q=2\x1b\\\x1b_Ga=d,d=A,q=2\x1b\\"
        );
    }

    #[test]
    fn cells_runs_coalesce_same_colors() {
        // 4x4 px = 4x2 cells, all one color -> one SGR per row, four ▀ each.
        let size = Size::new(4, 4);
        let stride = 16;
        let mut buf = vec![0u8; stride * 4];
        for px in buf.chunks_exact_mut(4) {
            px.copy_from_slice(&[10, 20, 30, 0]); // b=10 g=20 r=30
        }
        let mut out = Vec::new();
        cells_emit(&mut out, &buf, stride, size.h, Rect::from_size(size));
        let s = String::from_utf8(out).unwrap();
        assert_eq!(
            s.matches("38;2;30;20;10;48;2;30;20;10m").count(),
            2,
            "{s:?}"
        );
        assert_eq!(s.matches('▀').count(), 8);
        assert!(s.starts_with("\x1b[1;1H"));
        assert!(s.contains("\x1b[2;1H"));
        assert!(s.ends_with("\x1b[0m"));
    }

    #[test]
    fn cells_damage_maps_pixels_to_cell_rows() {
        // Damage only pixel row 3 (0-based) of a 2x6 surface -> cell row 1
        // (pixels 2..4), i.e. cursor row 2; columns limited to the rect.
        let (buf, stride) = coord_buffer(2, 6, 0);
        let mut out = Vec::new();
        cells_emit(&mut out, &buf, stride, 6, Rect::new(1, 3, 1, 1));
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("\x1b[2;2H"), "{s:?}");
        assert_eq!(s.matches('▀').count(), 1);
        // fg = pixel (1,2): r=1,g=2 ; bg = pixel (1,3): r=1,g=3.
        assert!(s.contains("38;2;1;2;42;48;2;1;3;42m"), "{s:?}");
    }

    #[test]
    fn cells_odd_bottom_row_reads_black_not_oob() {
        let (buf, stride) = coord_buffer(2, 3, 0); // odd height
        let mut out = Vec::new();
        cells_emit(&mut out, &buf, stride, 3, Rect::new(0, 0, 2, 3));
        let s = String::from_utf8(out).unwrap();
        // Cell row 1's bottom half (pixel row 3) is off-surface -> black bg.
        assert!(s.contains("48;2;0;0;0m"), "{s:?}");
    }
}
