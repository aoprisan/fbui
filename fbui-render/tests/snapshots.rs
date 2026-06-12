//! Golden-image snapshot tests for the painter primitives.
//!
//! Each test paints a small scene exercising one family of primitives into a
//! [`Surface`] and compares the shadow buffer against a committed PNG via
//! `fbui-testkit`. These are font-free on purpose: tiny-skia is deterministic
//! across hosts for a fixed version, so the goldens are reproducible in CI. Text
//! rendering (font-dependent) is checked structurally in `text.rs` instead.
//!
//! Regenerate goldens after an intentional change with:
//!
//! ```text
//! FBUI_UPDATE_SNAPSHOTS=1 cargo test -p fbui-render --test snapshots
//! ```

use fbui_render::geom::{Point, Rect};
use fbui_render::path::PathBuilder;
use fbui_render::{Color, Image, Scale, Surface};
use fbui_testkit::{assert_snapshot_in, Tolerance};

const DIR: &str = "tests/snapshots";

fn check(name: &str, surface: &Surface) {
    assert_snapshot_in(DIR, name, surface.pixmap(), Tolerance::FUZZY);
}

#[test]
fn rects_and_paths() {
    let mut s = Surface::with_base(200, 160, Scale::ONE, Color::rgb(0x20, 0x20, 0x28));
    s.paint(|p| {
        p.fill_rect(
            Rect::new(10.0, 10.0, 60.0, 40.0),
            Color::rgb(0xff, 0x55, 0x55),
        );
        p.stroke_rect(
            Rect::new(80.0, 10.0, 60.0, 40.0),
            Color::rgb(0x55, 0xff, 0x55),
            4.0,
        );
        p.fill_rounded_rect(
            Rect::new(10.0, 60.0, 60.0, 40.0),
            12.0,
            Color::rgb(0x55, 0x88, 0xff),
        );
        p.stroke_rounded_rect(Rect::new(80.0, 60.0, 60.0, 40.0), 12.0, Color::WHITE, 3.0);

        // A filled triangle and a stroked open polyline.
        let mut tri = PathBuilder::new();
        tri.move_to(20.0, 150.0);
        tri.line_to(60.0, 115.0);
        tri.line_to(100.0, 150.0);
        tri.close();
        if let Some(path) = tri.finish() {
            p.fill_path(&path, Color::rgb(0xff, 0xcc, 0x33));
        }

        let mut line = PathBuilder::new();
        line.move_to(115.0, 150.0);
        line.quad_to(150.0, 110.0, 185.0, 150.0);
        if let Some(path) = line.finish() {
            p.stroke_path(&path, Color::rgb(0xcc, 0x66, 0xff), 3.0);
        }
    });
    check("rects_and_paths", &s);
}

#[test]
fn gradients() {
    let mut s = Surface::new(200, 120, Scale::ONE);
    s.paint(|p| {
        p.fill_linear_gradient(
            Rect::new(10.0, 10.0, 80.0, 100.0),
            Point::new(10.0, 10.0),
            Point::new(90.0, 110.0),
            &[
                (0.0, Color::rgb(0xff, 0x00, 0x88)),
                (1.0, Color::rgb(0x00, 0x88, 0xff)),
            ],
        );
        p.fill_radial_gradient(
            Rect::new(110.0, 10.0, 80.0, 100.0),
            Point::new(150.0, 60.0),
            45.0,
            &[
                (0.0, Color::WHITE),
                (0.7, Color::rgb(0x33, 0x99, 0x33)),
                (1.0, Color::BLACK),
            ],
        );
    });
    check("gradients", &s);
}

#[test]
fn clipping() {
    let mut s = Surface::new(160, 160, Scale::ONE);
    s.paint(|p| {
        // A circle-ish stack of rects, clipped to a centered rectangle so half of
        // each is masked away — proves the clip stack intersects.
        p.push_clip(Rect::new(40.0, 40.0, 80.0, 80.0));
        for i in 0..8 {
            let v = (i * 28) as u8;
            p.fill_rect(
                Rect::new(20.0 + i as f32 * 12.0, 0.0, 40.0, 160.0),
                Color::rgb(v, 0x66, 0xcc),
            );
        }
        p.pop_clip();
        // Outside the clip again: a frame that should be fully visible.
        p.stroke_rect(Rect::new(40.0, 40.0, 80.0, 80.0), Color::WHITE, 2.0);
    });
    check("clipping", &s);
}

#[test]
fn opacity_group() {
    let mut s = Surface::with_base(160, 120, Scale::ONE, Color::rgb(0x10, 0x10, 0x10));
    s.paint(|p| {
        // Two overlapping opaque rects inside a 50% group composite as one layer,
        // so the overlap is NOT double-darkened against the background.
        p.push_opacity(0.5);
        p.fill_rect(
            Rect::new(20.0, 20.0, 80.0, 60.0),
            Color::rgb(0xff, 0x40, 0x40),
        );
        p.fill_rect(
            Rect::new(60.0, 40.0, 80.0, 60.0),
            Color::rgb(0x40, 0x40, 0xff),
        );
        p.pop_opacity();
    });
    check("opacity_group", &s);
}

#[test]
fn image_blit() {
    // Build an 8x8 checkerboard image in memory and blit it scaled-by-position.
    let mut img = image::RgbaImage::new(8, 8);
    for y in 0..8 {
        for x in 0..8 {
            let on = (x + y) % 2 == 0;
            let c = if on {
                [0xff, 0xcc, 0x00, 0xff]
            } else {
                [0x22, 0x22, 0x44, 0xff]
            };
            img.put_pixel(x, y, image::Rgba(c));
        }
    }
    let image = Image::from_rgba(img);

    let mut s = Surface::new(64, 64, Scale::ONE);
    s.paint(|p| {
        p.draw_image(&image, Point::new(8.0, 8.0));
        p.draw_image(&image, Point::new(40.0, 40.0));
    });
    check("image_blit", &s);
}
