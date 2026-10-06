//! A banded surface must put exactly the bytes on screen that a whole-screen
//! shadow does — the fbui rule that a cheaper path never diverges from the
//! slow one. The scene below is built to stress every place a band offset
//! could slip: clips, anti-aliased edges (opaque and translucent), thick
//! strokes and hairlines, gradients, opacity groups, text and images, all
//! straddling band boundaries, at several scales, rotations and both pixel
//! formats. The one exception — bilinear-scaled images, off by at most one
//! level — has its own test.

use fbui_render::{
    Color, FontContext, IRect, Image, Painter, Path, PathBuilder, Point, Rect, Rotation, Scale,
    Surface, TargetFormat, TextStyle,
};

const FONT: &[u8] = include_bytes!("../fonts/Inter-Regular.ttf");
const W: u32 = 96;
const H: u32 = 72;

fn checker() -> Image {
    let (w, h) = (7u32, 5u32);
    let mut rgba = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let on = (x + y) % 2 == 0;
            rgba.extend_from_slice(&[
                if on { 250 } else { 20 },
                (x * 30) as u8,
                (y * 50) as u8,
                255,
            ]);
        }
    }
    Image::from_rgba_bytes(w, h, &rgba).unwrap()
}

/// Paint the scene in the logical space of a `lw × lh` surface. Repaints
/// everything (the contract `paint_banded` requires).
fn scene(p: &mut Painter<'_>, fonts: &mut FontContext, img: &Image, lw: f32, lh: f32) {
    p.fill_rect(Rect::new(0.0, 0.0, lw, lh), Color::rgb(0x14, 0x16, 0x1b));
    p.fill_linear_gradient(
        Rect::new(2.0, 3.0, lw - 4.0, lh * 0.6),
        Point::new(0.0, 3.0),
        Point::new(0.0, lh * 0.6),
        &[
            (0.0, Color::rgb(30, 60, 200)),
            (1.0, Color::rgb(220, 120, 40)),
        ],
    );
    p.fill_radial_gradient(
        Rect::new(lw * 0.5, 4.0, lw * 0.45, lh * 0.5),
        Point::new(lw * 0.7, lh * 0.3),
        lh * 0.25,
        &[
            (0.0, Color::rgba(255, 255, 255, 200)),
            (1.0, Color::rgba(0, 80, 0, 40)),
        ],
    );
    // A clip whose edges fall mid-band, with AA shapes crossing it: one
    // translucent, two opaque (tiny-skia blends those differently).
    p.push_clip(Rect::new(5.3, 7.7, lw * 0.6, lh * 0.55));
    p.fill_path(
        &Path::circle(lw * 0.3, lh * 0.4, lh * 0.3).unwrap(),
        Color::rgba(200, 30, 90, 180),
    );
    p.stroke_rounded_rect(
        Rect::new(9.5, 11.25, lw * 0.4, lh * 0.33),
        6.0,
        Color::WHITE,
        2.5,
    );
    p.stroke_rounded_rect(
        Rect::new(7.5, 9.5, lw * 0.5, lh * 0.4),
        5.0,
        Color::rgb(250, 120, 20),
        1.0,
    );
    p.pop_clip();
    // Hairlines outside any clip: a diagonal polyline, a curve thinner than a
    // pixel (drawn at reduced opacity), and an opaque circle's outline.
    let mut b = PathBuilder::new();
    b.move_to(3.0, 2.0)
        .line_to(lw - 5.0, lh - 3.0)
        .line_to(lw * 0.2, lh * 0.9);
    p.stroke_path(&b.finish().unwrap(), Color::rgb(250, 200, 20), 1.0);
    let mut b = PathBuilder::new();
    b.move_to(lw * 0.9, 1.0)
        .quad_to(0.0, lh * 0.5, lw * 0.8, lh - 1.0);
    p.stroke_path(&b.finish().unwrap(), Color::rgba(120, 220, 255, 200), 0.6);
    p.stroke_path(
        &Path::circle(lw * 0.75, lh * 0.7, lh * 0.2).unwrap(),
        Color::rgb(90, 250, 140),
        1.0,
    );
    // An opacity group (an off-screen layer) holding text and a shape.
    p.push_opacity(0.55);
    p.fill_rounded_rect(
        Rect::new(lw * 0.1, lh * 0.55, lw * 0.8, lh * 0.3),
        5.0,
        Color::rgb(250, 250, 90),
    );
    fonts.draw_text(
        p,
        "Banded g\u{e9}nial 0123",
        &TextStyle::new(11.0, Color::rgb(10, 10, 10)),
        Point::new(lw * 0.12, lh * 0.57),
        Some(lw * 0.75),
    );
    p.pop_opacity();
    // Text outside any group, across the middle.
    fonts.draw_text(
        p,
        "fbui wraps across bands",
        &TextStyle::new(9.5, Color::rgb(240, 240, 240)),
        Point::new(3.0, lh * 0.38),
        Some(lw - 6.0),
    );
    p.draw_image(img, Point::new(lw - 12.0, lh * 0.45));
    p.stroke_rect(
        Rect::new(0.5, 0.5, lw - 1.0, lh - 1.0),
        Color::rgb(90, 200, 255),
        1.0,
    );
}

/// The one primitive banding doesn't reproduce exactly: a bilinear-scaled
/// image (see `scaled_images_differ_by_at_most_one_level`).
fn scaled_image(p: &mut Painter<'_>, img: &Image, lw: f32, lh: f32) {
    p.fill_rect(Rect::new(0.0, 0.0, lw, lh), Color::rgb(0x14, 0x16, 0x1b));
    p.draw_image_scaled(img, Rect::new(4.0, lh * 0.2, lw * 0.7, lh * 0.65));
}

struct Case {
    scale: f32,
    rotation: Rotation,
    format: TargetFormat,
    dither: bool,
}

fn panel(rotation: Rotation) -> (u32, u32) {
    // The surface is W × H in UI orientation; the panel is that turned.
    rotation.surface_size(W, H)
}

/// The reference: a whole-screen shadow painted once and presented at age 0.
fn reference(c: &Case, fonts: &mut FontContext, img: &Image) -> (Vec<u8>, usize) {
    let sc = Scale::new(c.scale);
    let mut s = Surface::new(W, H, sc);
    s.set_rotation(c.rotation);
    s.set_dither(c.dither);
    let (lw, lh) = (W as f32 / c.scale, H as f32 / c.scale);
    s.paint(|p| scene(p, fonts, img, lw, lh));
    let (pw, ph) = panel(c.rotation);
    let stride = pw as usize * c.format.bytes_per_pixel() + 8;
    let mut dst = vec![0x5a; stride * ph as usize];
    s.present_to_buffer(&mut dst, stride, c.format, 0);
    (dst, stride)
}

fn banded(
    c: &Case,
    rows: u32,
    region: IRect,
    fonts: &mut FontContext,
    img: &Image,
    dst: &mut [u8],
    stride: usize,
) -> Vec<IRect> {
    let sc = Scale::new(c.scale);
    let mut s = Surface::banded(W, H, rows, sc);
    assert_eq!((s.width(), s.height()), (W, H));
    assert_eq!(
        s.pixmap().height(),
        (rows + 2).min(H),
        "the shadow is one band plus guard rows"
    );
    s.set_rotation(c.rotation);
    s.set_dither(c.dither);
    let (lw, lh) = (W as f32 / c.scale, H as f32 / c.scale);
    s.paint_banded(region, dst, stride, c.format, |p| {
        scene(p, fonts, img, lw, lh)
    })
}

fn cases() -> Vec<Case> {
    let mut v = Vec::new();
    for &scale in &[1.0, 2.0, 1.5] {
        for &rotation in &[
            Rotation::Rot0,
            Rotation::Rot90,
            Rotation::Rot180,
            Rotation::Rot270,
        ] {
            v.push(Case {
                scale,
                rotation,
                format: TargetFormat::Xrgb8888,
                dither: false,
            });
        }
        v.push(Case {
            scale,
            rotation: Rotation::Rot0,
            format: TargetFormat::Rgb565,
            dither: true,
        });
        v.push(Case {
            scale,
            rotation: Rotation::Rot90,
            format: TargetFormat::Rgb565,
            dither: true,
        });
        v.push(Case {
            scale,
            rotation: Rotation::Rot0,
            format: TargetFormat::Rgb565,
            dither: false,
        });
    }
    v
}

#[test]
fn every_band_height_matches_the_whole_screen_byte_for_byte() {
    let mut fonts = FontContext::with_fonts([FONT.to_vec()]);
    let img = checker();
    for c in cases() {
        let (want, stride) = reference(&c, &mut fonts, &img);
        // 1 row (the extreme), odd heights that don't divide H, a band taller
        // than the screen (one pass).
        for rows in [1, 4, 7, 16, 33, H, 500] {
            let (_, ph) = panel(c.rotation);
            let mut got = vec![0x5a; stride * ph as usize];
            let written = banded(
                &c,
                rows,
                IRect::from_wh(W, H),
                &mut fonts,
                &img,
                &mut got,
                stride,
            );
            assert!(
                got == want,
                "scale {} {:?} {:?} dither {} rows {rows}: banded output differs",
                c.scale,
                c.rotation,
                c.format,
                c.dither
            );
            // Panel-space rects that tile the panel exactly.
            let area: u64 = written.iter().map(|r| r.w as u64 * r.h as u64).sum();
            assert_eq!(area, W as u64 * H as u64);
        }
    }
}

#[test]
fn a_partial_region_writes_only_inside_it_and_matches_there() {
    let mut fonts = FontContext::with_fonts([FONT.to_vec()]);
    let img = checker();
    for c in cases() {
        let (want, stride) = reference(&c, &mut fonts, &img);
        let region = IRect::new(13, 9, 41, 37);
        let (pw, ph) = panel(c.rotation);
        let bpp = c.format.bytes_per_pixel();
        // Start from a buffer that is the reference everywhere but scrambled
        // inside the region: a correct partial repaint restores it exactly.
        let mut got = want.clone();
        let inside = c.rotation.map_rect(region, W, H);
        for y in inside.y..inside.bottom() {
            let o = y as usize * stride + inside.x as usize * bpp;
            got[o..o + inside.w as usize * bpp].fill(0xEE);
        }
        let written = banded(&c, 6, region, &mut fonts, &img, &mut got, stride);
        assert!(
            got == want,
            "scale {} {:?} {:?}: partial repaint differs",
            c.scale,
            c.rotation,
            c.format
        );
        for r in &written {
            assert!(r.intersect(inside) == *r, "{r:?} outside {inside:?}");
        }
        // Stride padding beyond the panel's rows is never written.
        for y in 0..ph as usize {
            let pad = &got[y * stride + pw as usize * bpp..(y + 1) * stride];
            assert!(pad.iter().all(|&b| b == 0x5a));
        }
    }
}

#[test]
fn whole_screen_operations_are_inert_on_a_banded_surface() {
    let mut s = Surface::banded(W, H, 8, Scale::ONE);
    assert_eq!(s.band_rows(), Some(8));
    s.paint(|p| p.fill_rect(Rect::new(0.0, 0.0, 10.0, 10.0), Color::WHITE));
    let mut dst = vec![0u8; W as usize * 4 * H as usize];
    assert!(s
        .present_to_buffer(&mut dst, W as usize * 4, TargetFormat::Xrgb8888, 0)
        .is_empty());
    assert!(
        dst.iter().all(|&b| b == 0),
        "nothing reaches the screen outside paint_banded"
    );
    // Scroll-blit has nothing retained to reuse: the whole rect is repaint.
    let r = Rect::new(0.0, 10.0, 50.0, 30.0);
    assert_eq!(s.scroll_region(r, 5.0), r);
    assert_eq!(Surface::new(4, 4, Scale::ONE).band_rows(), None);
}

#[test]
fn scaled_images_differ_by_at_most_one_level() {
    // tiny-skia samples a scaled image through the inverse of its transform,
    // computed in f32; a band's offset changes that rounding, so bilinear
    // samples may land one level apart. Pin that it stays at one.
    let img = checker();
    for scale in [1.0f32, 1.5, 2.0] {
        let sc = Scale::new(scale);
        let (lw, lh) = (W as f32 / scale, H as f32 / scale);
        let stride = W as usize * 4;
        let mut s = Surface::new(W, H, sc);
        s.paint(|p| scaled_image(p, &img, lw, lh));
        let mut want = vec![0u8; stride * H as usize];
        s.present_to_buffer(&mut want, stride, TargetFormat::Xrgb8888, 0);
        for rows in [1, 7, 16] {
            let mut b = Surface::banded(W, H, rows, sc);
            let mut got = vec![0u8; stride * H as usize];
            b.paint_banded(
                IRect::from_wh(W, H),
                &mut got,
                stride,
                TargetFormat::Xrgb8888,
                |p| scaled_image(p, &img, lw, lh),
            );
            let worst = want
                .iter()
                .zip(&got)
                .map(|(a, b)| a.abs_diff(*b))
                .max()
                .unwrap();
            assert!(worst <= 1, "scale {scale} rows {rows}: off by {worst}");
        }
    }
}
