//! Bitmap fonts made from a TTF render like the TTF: the same pen positions
//! and baselines, coverage within 4-bit quantization — and, like everything
//! else, the same bytes whether painted whole-screen or in bands.
#![cfg(feature = "outline-text")]

use fbui_render::text::BitmapFont;
use fbui_render::{
    Color, FontContext, IRect, Point, Rect, Scale, Surface, TargetFormat, TextStyle,
};

const TTF: &[u8] = include_bytes!("../fonts/Inter-Regular.ttf");
const FBF: [&[u8]; 4] = [
    include_bytes!("../fonts/Inter-12.fbf"),
    include_bytes!("../fonts/Inter-16.fbf"),
    include_bytes!("../fonts/Inter-20.fbf"),
    include_bytes!("../fonts/Inter-24.fbf"),
];

fn bitmap() -> FontContext {
    FontContext::with_bitmap_fonts(FBF.iter().map(|f| BitmapFont::from_bytes(f).unwrap()))
}

fn render(fc: &mut FontContext, text: &str, size: f32, w: u32, h: u32) -> Vec<u8> {
    let mut s = Surface::new(w, h, Scale::ONE);
    let st = TextStyle::new(size, Color::WHITE);
    s.paint(|p| {
        p.fill_rect(Rect::new(0.0, 0.0, w as f32, h as f32), Color::BLACK);
        fc.draw_text(p, text, &st, Point::new(3.0, 2.0), Some(w as f32 - 6.0));
    });
    s.to_rgba()
}

/// Ink bounding box and vertical ink centroid of an RGBA image (any
/// non-black pixel is ink).
fn ink(rgba: &[u8], w: usize) -> (u64, [usize; 4], f64) {
    let (mut sum, mut wy) = (0u64, 0f64);
    let mut bb = [usize::MAX, usize::MAX, 0, 0];
    for (i, px) in rgba.chunks_exact(4).enumerate() {
        let v = px[0] as u64;
        if v == 0 {
            continue;
        }
        let (x, y) = (i % w, i / w);
        sum += v;
        wy += v as f64 * y as f64;
        bb = [bb[0].min(x), bb[1].min(y), bb[2].max(x), bb[3].max(y)];
    }
    (sum, bb, wy / sum.max(1) as f64)
}

#[test]
fn bitmap_inter_draws_like_outline_inter() {
    // Same glyph shapes and the same lines; each glyph's pen is snapped to a
    // whole pixel (the outline path renders quarter-pixel variants), so
    // pixels shift by up to half a pixel but the ink doesn't change.
    let mut outline = FontContext::with_static_fonts([TTF]);
    let mut bm = bitmap();
    for (text, size) in [
        ("Hill 0123 mmm", 16.0),
        ("Fill 9876 ijk", 16.0),
        ("Count: 42", 20.0),
        ("small print", 12.0),
        ("Résumé — 24 px", 24.0),
    ] {
        let (a, b) = (
            render(&mut outline, text, size, 220, 48),
            render(&mut bm, text, size, 220, 48),
        );
        let ((ia, ba, ya), (ib, bb, yb)) = (ink(&a, 220), ink(&b, 220));
        let ratio = ib as f64 / ia as f64;
        assert!((0.97..=1.03).contains(&ratio), "{text:?}: ink {ia} vs {ib}");
        for k in 0..4 {
            assert!(
                ba[k].abs_diff(bb[k]) <= 1,
                "{text:?}: bounds {ba:?} vs {bb:?}"
            );
        }
        assert!(
            (ya - yb).abs() < 0.25,
            "{text:?}: baseline {ya:.2} vs {yb:.2}"
        );
    }
}

#[test]
fn kerning_is_not_applied() {
    // The one layout difference by design: no shaping, so no kerning. Kerned
    // pairs come out a little wider than the outline path sets them.
    let mut outline = FontContext::with_static_fonts([TTF]);
    let mut bm = bitmap();
    let st = TextStyle::new(16.0, Color::WHITE);
    let (o, b) = (
        outline.layout("AV To Wa", &st, None).size().w,
        bm.layout("AV To Wa", &st, None).size().w,
    );
    assert!(b > o && b - o < 8.0, "outline {o}, bitmap {b}");
    // Unkerned text measures the same, to the pixel.
    let (o, b) = (
        outline.layout("Hill 0123", &st, None).size().w,
        bm.layout("Hill 0123", &st, None).size().w,
    );
    assert!((o - b).abs() < 1.0, "outline {o}, bitmap {b}");
}

#[test]
fn bitmap_text_wraps_and_measures() {
    let mut fc = bitmap();
    let st = TextStyle::new(16.0, Color::WHITE);
    let one = fc.layout("The quick brown fox", &st, None);
    assert_eq!(one.line_count(), 1);
    let w = one.size().w;
    assert!((100.0..200.0).contains(&w), "{w}");
    let wrapped = fc.layout("The quick brown fox", &st, Some(w * 0.6));
    assert_eq!(wrapped.line_count(), 2);
    assert!(wrapped.size().w <= w * 0.6);
    assert_eq!(wrapped.size().h, 2.0 * st.line_height);
    // Unknown characters set as `?`, not nothing.
    let q = fc.layout("?", &st, None).size().w;
    assert_eq!(fc.layout("\u{4e2d}", &st, None).size().w, q);
}

#[test]
fn bitmap_text_is_band_exact() {
    let mut fc = bitmap();
    let (w, h) = (160u32, 90u32);
    let paint = |p: &mut fbui_render::Painter<'_>, fc: &mut FontContext| {
        p.fill_rect(
            Rect::new(0.0, 0.0, w as f32, h as f32),
            Color::rgb(20, 22, 27),
        );
        p.push_clip(Rect::new(4.0, 3.5, 150.0, 80.0));
        fc.draw_text(
            p,
            "Bitmap fonts across bands: wrapped, clipped, Ünïcödé…",
            &TextStyle::new(20.0, Color::rgba(240, 240, 200, 220)),
            Point::new(5.0, 4.0),
            Some(140.0),
        );
        p.pop_clip();
    };
    let stride = w as usize * 4;
    let mut full = Surface::new(w, h, Scale::ONE);
    full.paint(|p| paint(p, &mut fc));
    let mut want = vec![0u8; stride * h as usize];
    full.present_to_buffer(&mut want, stride, TargetFormat::Xrgb8888, 0);
    for rows in [1, 5, 16] {
        let mut band = Surface::banded(w, h, rows, Scale::ONE);
        let mut got = vec![0u8; stride * h as usize];
        band.paint_banded(
            IRect::from_wh(w, h),
            &mut got,
            stride,
            TargetFormat::Xrgb8888,
            |p| paint(p, &mut fc),
        );
        assert!(got == want, "{rows}-row bands");
    }
}
