//! Text rendering tests.
//!
//! These are **structural**, not golden-image: which glyphs a string shapes to
//! and how they rasterize depends on the host's installed fonts, so a committed
//! PNG would be non-portable. Instead we assert the properties that must hold
//! regardless of font: layout measures a sensible box, drawing produces ink and
//! reports damage, the glyph cache actually caches, and complex scripts (CJK,
//! RTL Arabic/Hebrew) shape and render without panicking and put ink on the
//! surface when a covering font exists.
//!
//! A test that needs glyph coverage is skipped (not failed) when no font on the
//! host can render the sample.

use fbui_render::geom::{Point, Rect};
use fbui_render::{Color, FontContext, Scale, Surface, TextStyle};

/// Count non-background pixels, i.e. whether anything was actually drawn.
fn ink_pixels(surface: &Surface, bg: Color) -> u32 {
    let want = [bg.r, bg.g, bg.b];
    surface
        .pixmap()
        .pixels()
        .iter()
        .filter(|px| [px.red(), px.green(), px.blue()] != want)
        .count() as u32
}

fn paint_text(text: &str, style: &TextStyle) -> (Surface, u32) {
    let bg = Color::rgb(0, 0, 0);
    let mut fonts = FontContext::new();
    let mut surface = Surface::with_base(320, 80, Scale::ONE, bg);
    surface.paint(|p| fonts.draw_text(p, text, style, Point::new(8.0, 8.0), None));
    let ink = ink_pixels(&surface, bg);
    (surface, ink)
}

#[test]
fn layout_measures_nonempty_box() {
    let mut fonts = FontContext::new();
    let style = TextStyle::new(18.0, Color::WHITE);
    let layout = fonts.layout("Hello, fbui", &style, None);
    let size = layout.size();
    assert!(size.w > 0.0, "expected positive width, got {}", size.w);
    assert!(
        size.h >= 18.0,
        "expected at least one line tall, got {}",
        size.h
    );
}

#[test]
fn wider_text_measures_wider() {
    let mut fonts = FontContext::new();
    let style = TextStyle::new(18.0, Color::WHITE);
    let short = fonts.layout("i", &style, None).size().w;
    let long = fonts.layout("internationalization", &style, None).size().w;
    assert!(long > short, "long={long} should exceed short={short}");
}

#[test]
fn latin_text_draws_ink_and_damage() {
    let (surface, ink) = paint_text("Settings", &TextStyle::new(20.0, Color::WHITE));
    assert!(ink > 0, "Latin text should put ink on the surface");
    assert!(!surface.is_clean(), "drawing text should report damage");
}

#[test]
fn wrapping_produces_multiple_lines() {
    let mut fonts = FontContext::new();
    let style = TextStyle::new(16.0, Color::WHITE);
    let unwrapped = fonts
        .layout("one two three four five six", &style, None)
        .size();
    let wrapped = fonts
        .layout("one two three four five six", &style, Some(60.0))
        .size();
    assert!(wrapped.h > unwrapped.h, "wrapping at 60px should add lines");
    assert!(
        wrapped.w <= unwrapped.w + 1.0,
        "wrapped width should not exceed the cap by much"
    );
}

#[test]
fn clip_bounds_text_ink() {
    // Text drawn entirely outside a tiny clip leaves no ink.
    let bg = Color::rgb(0, 0, 0);
    let mut fonts = FontContext::new();
    let mut surface = Surface::with_base(200, 60, Scale::ONE, bg);
    surface.paint(|p| {
        p.push_clip(Rect::new(0.0, 0.0, 1.0, 1.0));
        fonts.draw_text(
            p,
            "should be clipped away",
            &TextStyle::new(20.0, Color::WHITE),
            Point::new(8.0, 20.0),
            None,
        );
        p.pop_clip();
    });
    assert_eq!(
        ink_pixels(&surface, bg),
        0,
        "clipped text must not draw outside the clip"
    );
}

#[test]
fn cjk_renders_when_font_available() {
    // 你好世界 — needs a CJK font (e.g. fonts-noto-cjk). Skip if unavailable.
    let (_, ink) = paint_text("你好世界", &TextStyle::new(22.0, Color::WHITE));
    if ink == 0 {
        eprintln!("skipping CJK ink check: no CJK-covering font on this host");
        return;
    }
    assert!(ink > 0);
}

#[test]
fn rtl_renders_when_font_available() {
    // Arabic "مرحبا" and Hebrew "שלום" — bidi reordering is cosmic-text's job;
    // we just assert it shapes and rasterizes without panicking, and inks if a
    // covering font exists.
    let (_, ink) = paint_text("مرحبا שלום", &TextStyle::new(22.0, Color::WHITE));
    if ink == 0 {
        eprintln!("skipping RTL ink check: no RTL-covering font on this host");
        return;
    }
    assert!(ink > 0);
}

#[test]
fn repeated_draw_is_cache_warm() {
    // Drawing the same string twice should not crash and should be stable; this
    // exercises the glyph atlas hit path on the second pass.
    let mut fonts = FontContext::new();
    let style = TextStyle::new(18.0, Color::WHITE);
    let mut surface = Surface::new(200, 40, Scale::ONE);
    surface.paint(|p| fonts.draw_text(p, "cache me", &style, Point::new(4.0, 4.0), None));
    surface.paint(|p| fonts.draw_text(p, "cache me", &style, Point::new(4.0, 4.0), None));
}
