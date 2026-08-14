//! Pixel-format conversion for camera/decoder frames: packed YUV to the RGBA
//! the render layer draws.
//!
//! V4L2 webcams overwhelmingly hand out **YUYV** (YUV 4:2:2 packed), and
//! hardware/software video decoders hand out **NV12** (planar Y + interleaved
//! UV, 4:2:0). These functions turn either into straight-alpha RGBA8 rows,
//! ready for [`crate::Image::from_rgba_bytes`] and a
//! `VideoView`-style blit. Conversion uses BT.601 limited ("studio") range —
//! what actual capture hardware produces — in fixed-point integer arithmetic.
//!
//! These are deliberately plain, allocation-per-call functions: a video
//! pipeline converts on its producer thread (not the UI thread), and one
//! `Vec` per frame is noise next to the decode itself.

/// Clamp a fixed-point BT.601 result to a byte.
#[inline]
fn clamp8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// One YUV pixel (limited range) to RGB, BT.601 fixed point.
#[inline]
fn yuv_to_rgb(y: u8, u: u8, v: u8) -> (u8, u8, u8) {
    let c = 298 * (y as i32 - 16);
    let d = u as i32 - 128;
    let e = v as i32 - 128;
    (
        clamp8((c + 409 * e + 128) >> 8),
        clamp8((c - 100 * d - 208 * e + 128) >> 8),
        clamp8((c + 516 * d + 128) >> 8),
    )
}

/// Convert a packed **YUYV** (a.k.a. YUY2, 4:2:2) frame to straight-alpha
/// RGBA8. `src` must be exactly `width * height * 2` bytes and `width` must be
/// even (YUYV pairs pixels). Returns `width * height * 4` bytes.
pub fn yuyv_to_rgba(src: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let (w, h) = (width as usize, height as usize);
    if !width.is_multiple_of(2) {
        return Err(format!("yuyv width {width} must be even"));
    }
    if src.len() != w * h * 2 {
        return Err(format!(
            "yuyv buffer is {} bytes, expected {} for {width}x{height}",
            src.len(),
            w * h * 2
        ));
    }
    let mut out = Vec::with_capacity(w * h * 4);
    for quad in src.chunks_exact(4) {
        let [y0, u, y1, v] = [quad[0], quad[1], quad[2], quad[3]];
        for y in [y0, y1] {
            let (r, g, b) = yuv_to_rgb(y, u, v);
            out.extend_from_slice(&[r, g, b, 255]);
        }
    }
    Ok(out)
}

/// Convert an **NV12** (planar Y + interleaved UV, 4:2:0) frame to
/// straight-alpha RGBA8. `y_plane` is `width * height` bytes; `uv_plane` is
/// `width * height / 2` bytes (one interleaved U,V pair per 2×2 block). Both
/// dimensions must be even. Returns `width * height * 4` bytes.
pub fn nv12_to_rgba(
    y_plane: &[u8],
    uv_plane: &[u8],
    width: u32,
    height: u32,
) -> Result<Vec<u8>, String> {
    let (w, h) = (width as usize, height as usize);
    if !width.is_multiple_of(2) || !height.is_multiple_of(2) {
        return Err(format!("nv12 dimensions {width}x{height} must be even"));
    }
    if y_plane.len() != w * h {
        return Err(format!(
            "nv12 Y plane is {} bytes, expected {} for {width}x{height}",
            y_plane.len(),
            w * h
        ));
    }
    if uv_plane.len() != w * h / 2 {
        return Err(format!(
            "nv12 UV plane is {} bytes, expected {} for {width}x{height}",
            uv_plane.len(),
            w * h / 2
        ));
    }
    let mut out = Vec::with_capacity(w * h * 4);
    for row in 0..h {
        let uv_row = &uv_plane[(row / 2) * w..];
        for col in 0..w {
            let y = y_plane[row * w + col];
            let u = uv_row[col & !1];
            let v = uv_row[(col & !1) + 1];
            let (r, g, b) = yuv_to_rgb(y, u, v);
            out.extend_from_slice(&[r, g, b, 255]);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(got: &[u8], want: (u8, u8, u8), tol: i32, what: &str) {
        for (g, w) in got[..3].iter().zip([want.0, want.1, want.2]) {
            assert!(
                (*g as i32 - w as i32).abs() <= tol,
                "{what}: got {:?}, want {:?}",
                &got[..3],
                want
            );
        }
        assert_eq!(got[3], 255, "{what}: opaque");
    }

    #[test]
    fn yuyv_black_white_and_grey() {
        // Limited range: Y=16 is black, Y=235 is white, neutral chroma 128.
        let src = [16u8, 128, 235, 128]; // two pixels: black, white
        let rgba = yuyv_to_rgba(&src, 2, 1).unwrap();
        assert_close(&rgba[0..4], (0, 0, 0), 1, "black");
        assert_close(&rgba[4..8], (255, 255, 255), 1, "white");

        // Mid grey: Y=126 -> exactly 128 in full range.
        let rgba = yuyv_to_rgba(&[126, 128, 126, 128], 2, 1).unwrap();
        assert_close(&rgba[0..4], (128, 128, 128), 1, "grey");
    }

    #[test]
    fn yuyv_primaries() {
        // BT.601 limited-range encodings of pure red / green / blue.
        let cases = [
            ((81u8, 90u8, 240u8), (255u8, 0u8, 0u8), "red"),
            ((145, 54, 34), (0, 255, 0), "green"),
            ((41, 240, 110), (0, 0, 255), "blue"),
        ];
        for ((y, u, v), rgb, name) in cases {
            let rgba = yuyv_to_rgba(&[y, u, y, v], 2, 1).unwrap();
            assert_close(&rgba[0..4], rgb, 3, name);
        }
    }

    #[test]
    fn yuyv_rejects_bad_sizes() {
        assert!(yuyv_to_rgba(&[0; 6], 3, 1).is_err(), "odd width");
        assert!(yuyv_to_rgba(&[0; 7], 2, 1).is_err(), "byte count");
    }

    #[test]
    fn nv12_solid_color_and_subsampling() {
        // A 2x2 white frame: Y plane all 235, one UV pair at neutral.
        let rgba = nv12_to_rgba(&[235; 4], &[128, 128], 2, 2).unwrap();
        for px in rgba.chunks_exact(4) {
            assert_close(px, (255, 255, 255), 1, "white");
        }

        // 4x2 with two UV pairs: left 2x2 block red chroma, right blue chroma.
        // Y chosen per block so the colors are the BT.601 primaries.
        let y = [81, 81, 41, 41, 81, 81, 41, 41];
        let uv = [90, 240, 240, 110]; // (U,V) red | (U,V) blue
        let rgba = nv12_to_rgba(&y, &uv, 4, 2).unwrap();
        assert_close(&rgba[0..4], (255, 0, 0), 3, "left block red");
        assert_close(&rgba[8..12], (0, 0, 255), 3, "right block blue");
        // Second row samples the same UV row (4:2:0 vertical subsampling).
        assert_close(&rgba[16..20], (255, 0, 0), 3, "row 1 left red");
    }

    #[test]
    fn nv12_rejects_bad_sizes() {
        assert!(nv12_to_rgba(&[0; 4], &[0; 2], 2, 1).is_err(), "odd height");
        assert!(nv12_to_rgba(&[0; 3], &[0; 2], 2, 2).is_err(), "y plane");
        assert!(nv12_to_rgba(&[0; 4], &[0; 3], 2, 2).is_err(), "uv plane");
    }
}
