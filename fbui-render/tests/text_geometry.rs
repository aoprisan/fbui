//! Caret / hit-test / selection geometry on a [`TextLayout`], pinned against
//! the bundled Inter face (loaded straight from the repo, so these run on any
//! host with no font dependence and no feature flag).

use fbui_render::{Color, FontContext, TextStyle};

fn fonts() -> FontContext {
    FontContext::with_fonts([include_bytes!("../fonts/Inter-Regular.ttf").to_vec()])
}

fn style() -> TextStyle {
    TextStyle::new(16.0, Color::WHITE)
}

#[test]
fn caret_advances_monotonically_along_a_line() {
    let mut fonts = fonts();
    let text = "hello world";
    let layout = fonts.layout(text, &style(), None);
    assert_eq!(layout.line_count(), 1);
    let mut last = -1.0;
    for i in 0..=text.len() {
        let c = layout.caret(i);
        assert!(c.x > last, "caret {i} at {} not after {last}", c.x);
        assert!((c.y - 0.0).abs() < 0.01, "single line sits at the top");
        assert!(c.h >= 16.0);
        last = c.x;
    }
    assert!((layout.caret(text.len()).x - layout.size().w).abs() < 1.0);
}

#[test]
fn hit_round_trips_the_caret() {
    let mut fonts = fonts();
    let text = "The quick brown fox";
    let layout = fonts.layout(text, &style(), None);
    for i in 0..=text.len() {
        let c = layout.caret(i);
        // A point a hair right of the boundary resolves back to it.
        let got = layout.hit(c.x + 0.5, c.h / 2.0);
        assert_eq!(got, i, "hit at x={} should be {i}, got {got}", c.x);
    }
    // Off the ends clamp.
    assert_eq!(layout.hit(-50.0, 5.0), 0);
    assert_eq!(layout.hit(10_000.0, 5.0), text.len());
    assert_eq!(layout.hit(5.0, -100.0), 0);
    assert_eq!(layout.hit(5.0, 1000.0), text.len());
}

#[test]
fn explicit_newlines_stack_lines_and_offsets_span_the_break() {
    let mut fonts = fonts();
    let text = "ab\ncd\n\nef";
    let layout = fonts.layout(text, &style(), None);
    assert_eq!(layout.line_count(), 4);
    let lh = layout.line_height();
    assert_eq!(layout.caret(0).y, 0.0);
    // "cd" starts at byte 3, on line 2.
    let c = layout.caret(3);
    assert!(
        (c.y - lh).abs() < 0.5,
        "second line at one line height: {c:?}"
    );
    assert!(c.x < 0.5, "start of line: {c:?}");
    // The empty line (byte 6) has a caret too.
    let e = layout.caret(6);
    assert!((e.y - 2.0 * lh).abs() < 0.5, "empty third line: {e:?}");
    // Hit into each line returns an offset inside that line.
    assert!(layout.hit(1.0, lh * 1.5) >= 3 && layout.hit(1.0, lh * 1.5) <= 5);
    assert_eq!(layout.hit(100.0, lh * 2.5), 6);
    assert_eq!(layout.hit(100.0, lh * 3.5), text.len());
    // End of first line, not start of second: clicking far right on line 1.
    assert_eq!(layout.hit(1000.0, lh * 0.5), 2);
}

#[test]
fn wrapping_produces_multiple_lines_and_caret_follows() {
    let mut fonts = fonts();
    let text = "one two three four five six seven eight nine ten";
    let unbounded = fonts.layout(text, &style(), None);
    let narrow = fonts.layout(text, &style(), Some(unbounded.size().w / 3.0));
    assert!(
        narrow.line_count() >= 3,
        "wrapped into {} lines",
        narrow.line_count()
    );
    let end = narrow.caret(text.len());
    assert!(
        end.y > narrow.line_height(),
        "end caret on a later line: {end:?}"
    );
    // Every boundary hit-tests back to itself even across wraps.
    for i in 0..=text.len() {
        let c = narrow.caret(i);
        let got = narrow.hit(c.x + 0.5, c.y + c.h / 2.0);
        assert_eq!(got, i, "wrapped hit at {c:?} should be {i}");
    }
}

#[test]
fn selection_rects_cover_the_range() {
    let mut fonts = fonts();
    let text = "abc\ndef";
    let layout = fonts.layout(text, &style(), None);
    assert!(layout.selection_rects(2, 2).is_empty());
    let one = layout.selection_rects(1, 3);
    assert_eq!(one.len(), 1);
    assert!((one[0].x - layout.caret(1).x).abs() < 0.5);
    assert!((one[0].right() - layout.caret(3).x).abs() < 0.5);
    // Across the line break: a box on each line plus the line-end tail.
    let two = layout.selection_rects(1, 5);
    assert!(two.len() >= 2, "{two:?}");
    assert!(
        two.iter().any(|r| r.y > 0.5),
        "second line covered: {two:?}"
    );
    // Reversed order is the same selection.
    assert_eq!(layout.selection_rects(5, 1).len(), two.len());
}

#[test]
fn empty_text_has_a_caret_and_one_line() {
    let mut fonts = fonts();
    let layout = fonts.layout("", &style(), Some(100.0));
    assert_eq!(layout.line_count(), 1);
    let c = layout.caret(0);
    assert_eq!((c.x, c.y), (0.0, 0.0));
    assert!(c.h >= 16.0);
    assert_eq!(layout.hit(30.0, 5.0), 0);
}
