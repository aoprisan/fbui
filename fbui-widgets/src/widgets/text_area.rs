//! [`TextArea`] — a multi-line editable text box: word-wrapped paragraphs,
//! a caret that moves by line, selection, the shared clipboard chords, and a
//! vertical scroll that follows the caret.
//!
//! The editing core is the same [`EditState`] the single-line
//! [`TextInput`](super::TextInput) uses; what this widget adds is everything
//! that needs *line geometry* — `Up`/`Down`, line `Home`/`End`, paging, click
//! and drag placement across wrapped lines — which it gets from the shaped
//! [`TextLayout`]'s caret / hit-test / selection API. The layout is re-shaped
//! on each event and paint (cosmic-text shaping a few hundred characters is
//! well inside a frame budget; the area repaints only its own box).
//! See `docs/text-editing.md`.

use std::any::Any;

use fbui_render::geom::{Point, Rect};
use fbui_render::{FontContext, TextLayout, TextStyle};

use super::edit::EditState;
use crate::ctx::{EventCtx, PaintCtx};
use crate::event::{Event, Key, Modifiers, PointerButton};
use crate::style::{self, Style};
use crate::theme::Theme;
use crate::util::text_style;
use crate::widget::Widget;

const PAD: f32 = 8.0;
/// Room kept clear on the right for the scrollbar thumb.
const BAR: f32 = 8.0;

/// A multi-line text box.
pub struct TextArea<Msg> {
    edit: EditState,
    placeholder: String,
    rows: usize,
    grow: f32,
    on_change: Option<Box<dyn Fn(String) -> Msg>>,
    /// Vertical scroll offset (logical px) of the content within the box.
    scroll_y: f32,
    /// The x the caret is aiming for across consecutive vertical moves, so
    /// Up/Down through a short line don't lose the column.
    goal_x: Option<f32>,
    dragging: bool,
    /// The content height at the last event, for clamping the scroll (paint
    /// derives the scrollbar from the layout it shapes, so it's never stale).
    content_h: f32,
}

impl<Msg> TextArea<Msg> {
    pub fn new() -> Self {
        TextArea {
            edit: EditState::default(),
            placeholder: String::new(),
            rows: 4,
            grow: 0.0,
            on_change: None,
            scroll_y: 0.0,
            goal_x: None,
            dragging: false,
            content_h: 0.0,
        }
    }

    pub fn placeholder(mut self, text: impl Into<String>) -> Self {
        self.placeholder = text.into();
        self
    }

    pub fn value(mut self, text: impl Into<String>) -> Self {
        self.edit = EditState::new(text);
        self
    }

    /// Height in text lines (default 4). The box never grows with its
    /// content; it scrolls.
    pub fn rows(mut self, rows: usize) -> Self {
        self.rows = rows.max(1);
        self
    }

    /// Flex-grow factor, to let the box take the remaining space in a column
    /// (`rows` then acts as the minimum height).
    pub fn grow(mut self, grow: f32) -> Self {
        self.grow = grow;
        self
    }

    pub fn on_change(mut self, f: impl Fn(String) -> Msg + 'static) -> Self {
        self.on_change = Some(Box::new(f));
        self
    }

    /// The current text.
    pub fn text(&self) -> &str {
        &self.edit.text
    }

    /// Replace the text (call via [`Ui::with`](crate::Ui::with)).
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.edit.set_text(text);
        self.goal_x = None;
    }

    /// The selected byte range (`start..end`, empty when nothing is selected).
    pub fn selection(&self) -> std::ops::Range<usize> {
        let (a, b) = self.edit.selection();
        a..b
    }

    /// Select `range` (clamped to char boundaries), caret at its end.
    pub fn select(&mut self, range: std::ops::Range<usize>) {
        self.edit.select(range.start, range.end);
    }

    /// Select everything.
    pub fn select_all(&mut self) {
        self.edit.select_all();
    }

    /// The caret's byte offset.
    pub fn cursor(&self) -> usize {
        self.edit.cursor
    }

    /// The current vertical scroll offset in logical pixels.
    pub fn scroll_offset(&self) -> f32 {
        self.scroll_y
    }

    /// Apply a key as if typed (no modifiers): the on-screen keyboard path.
    /// Does **not** fire `on_change`; use [`Ui::send_key`](crate::Ui::send_key)
    /// for that. Returns whether the text changed.
    pub fn apply_key(&mut self, key: Key) -> bool {
        let mut scratch = String::new();
        self.edit
            .apply(key, Modifiers::default(), true, &mut scratch)
            .changed
    }

    fn style_for(&self, theme: &Theme) -> TextStyle {
        text_style(theme, theme.metrics.font_size, theme.palette.text)
    }

    fn line_height(&self, theme: &Theme) -> f32 {
        self.style_for(theme).line_height
    }

    /// The text box inside the frame (padding and scrollbar gutter removed).
    fn inner(bounds: Rect) -> Rect {
        Rect::new(
            bounds.x + PAD,
            bounds.y + PAD,
            (bounds.w - 2.0 * PAD - BAR).max(1.0),
            (bounds.h - 2.0 * PAD).max(1.0),
        )
    }

    fn layout(&self, fonts: &mut FontContext, theme: &Theme, inner: Rect) -> TextLayout {
        fonts.layout(&self.edit.text, &self.style_for(theme), Some(inner.w))
    }

    fn max_scroll(&self, inner: Rect) -> f32 {
        (self.content_h - inner.h).max(0.0)
    }

    fn fire(&self, ctx: &mut EventCtx<Msg>) {
        if let Some(f) = &self.on_change {
            ctx.emit(f(self.edit.text.clone()));
        }
    }

    /// Byte offset under surface point `pos`.
    fn hit(&self, layout: &TextLayout, inner: Rect, pos: Point) -> usize {
        layout.hit(pos.x - inner.x, pos.y - inner.y + self.scroll_y)
    }

    /// Scroll so the caret line is fully inside the viewport, and refresh the
    /// cached content height.
    fn keep_caret_visible(&mut self, layout: &TextLayout, inner: Rect) {
        self.content_h = layout.size().h.max(layout.line_height());
        let caret = layout.caret(self.edit.cursor);
        if caret.y < self.scroll_y {
            self.scroll_y = caret.y;
        } else if caret.bottom() > self.scroll_y + inner.h {
            self.scroll_y = caret.bottom() - inner.h;
        }
        self.scroll_y = self.scroll_y.clamp(0.0, self.max_scroll(inner));
    }

    /// Move the caret one line up or down, keeping the goal column.
    fn move_vertical(&mut self, layout: &TextLayout, down: bool, extend: bool) {
        let caret = layout.caret(self.edit.cursor);
        let x = *self.goal_x.get_or_insert(caret.x);
        let lh = layout.line_height();
        let y = if down {
            caret.bottom() + lh / 2.0
        } else {
            caret.y - lh / 2.0
        };
        let to = if y < 0.0 {
            0
        } else if y > layout.size().h {
            self.edit.text.len()
        } else {
            layout.hit(x, y)
        };
        self.edit.move_cursor(to, extend);
    }

    /// Move to the start or end of the caret's visual line.
    fn move_line_edge(&mut self, layout: &TextLayout, end: bool, extend: bool) {
        let caret = layout.caret(self.edit.cursor);
        let y = caret.y + caret.h / 2.0;
        let to = if end {
            layout.hit(f32::MAX, y)
        } else {
            layout.hit(-1.0, y)
        };
        self.edit.move_cursor(to, extend);
    }

    /// Page up/down by the viewport height, keeping the goal column.
    fn move_page(&mut self, layout: &TextLayout, inner: Rect, down: bool, extend: bool) {
        let caret = layout.caret(self.edit.cursor);
        let x = *self.goal_x.get_or_insert(caret.x);
        let step = inner.h.max(layout.line_height());
        let y = caret.y + caret.h / 2.0 + if down { step } else { -step };
        let to = if y < 0.0 {
            0
        } else if y > layout.size().h {
            self.edit.text.len()
        } else {
            layout.hit(x, y)
        };
        self.edit.move_cursor(to, extend);
    }

    fn thumb_rect(&self, content_h: f32, inner: Rect, b: Rect) -> Option<Rect> {
        let max_off = (content_h - inner.h).max(0.0);
        if max_off <= 0.0 {
            return None;
        }
        let track = Rect::new(b.right() - 6.0, b.y + 4.0, 4.0, b.h - 8.0);
        let frac_visible = (inner.h / content_h).clamp(0.0, 1.0);
        let thumb_h = (track.h * frac_visible).max(16.0).min(track.h);
        let t = (self.scroll_y / max_off).clamp(0.0, 1.0);
        Some(Rect::new(
            track.x,
            track.y + t * (track.h - thumb_h),
            track.w,
            thumb_h,
        ))
    }
}

impl<Msg> Default for TextArea<Msg> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Msg: 'static> Widget<Msg> for TextArea<Msg> {
    fn layout_style(&self, theme: &Theme) -> Style {
        let h = self.rows as f32 * self.line_height(theme) + 2.0 * PAD;
        Style {
            size: taffy::Size {
                width: style::auto(),
                height: if self.grow > 0.0 {
                    style::auto()
                } else {
                    style::length(h)
                },
            },
            min_size: taffy::Size {
                width: style::length(120.0),
                height: style::length(h),
            },
            flex_grow: self.grow,
            ..Style::default()
        }
    }

    fn focusable(&self) -> bool {
        true
    }

    fn placed(&mut self, bounds: Rect, _scale: fbui_render::Scale) {
        // Keep the scroll in range if the box was resized.
        let inner = Self::inner(bounds);
        self.scroll_y = self.scroll_y.clamp(0.0, self.max_scroll(inner));
    }

    fn paint(&self, ctx: &mut PaintCtx) {
        let theme = ctx.theme();
        let b = ctx.bounds();
        let inner = Self::inner(b);
        let focused = ctx.is_focused();
        let radius = 6.0;
        let st = self.style_for(theme);
        let placeholder_style = text_style(theme, theme.metrics.font_size, theme.palette.muted);
        let (surface, accent, line, accent_sel) = (
            theme.palette.surface,
            theme.palette.accent,
            theme.palette.line,
            theme.palette.accent.with_alpha(0x55),
        );
        let (cursor, sel) = (self.edit.cursor, self.edit.selection());
        let placeholder = self.placeholder.clone();

        let origin = Point::new(inner.x, inner.y - self.scroll_y);
        let (p, fonts) = ctx.painter_and_fonts();

        p.fill_rounded_rect(b, radius, surface);
        p.stroke_rounded_rect(
            b,
            radius,
            if focused { accent } else { line },
            if focused { 2.0 } else { 1.0 },
        );

        // Shape once; text, selection, caret, and scrollbar all read it.
        let layout = fonts.layout(&self.edit.text, &st, Some(inner.w));
        let content_h = layout.size().h.max(layout.line_height());
        let thumb = self.thumb_rect(content_h, inner, b);

        p.push_clip(Rect::new(inner.x, inner.y, inner.w + BAR, inner.h));
        if self.edit.text.is_empty() && !placeholder.is_empty() {
            fonts.draw_text(
                p,
                &placeholder,
                &placeholder_style,
                Point::new(inner.x, inner.y),
                Some(inner.w),
            );
        } else {
            if sel.0 != sel.1 {
                for r in layout.selection_rects(sel.0, sel.1) {
                    p.fill_rect(
                        Rect::new(origin.x + r.x, origin.y + r.y, r.w, r.h),
                        accent_sel,
                    );
                }
            }
            fonts.draw(p, &layout, st.color, origin);
        }
        if focused {
            let c = layout.caret(cursor);
            p.fill_rect(
                Rect::new(origin.x + c.x, origin.y + c.y + 1.0, 1.5, c.h - 2.0),
                accent,
            );
        }
        p.pop_clip();

        if let Some(t) = thumb {
            p.fill_rounded_rect(t, 2.0, line);
        }
    }

    fn event(&mut self, ctx: &mut EventCtx<Msg>) {
        let ev = ctx.event().clone();
        let b = ctx.bounds();
        let inner = Self::inner(b);
        let theme = ctx.theme().clone();
        match ev {
            Event::PointerDown {
                button: PointerButton::Left,
                pos,
            } => {
                ctx.request_focus();
                let layout = self.layout(ctx.fonts(), &theme, inner);
                let idx = self.hit(&layout, inner, pos);
                self.edit.move_cursor(idx, false);
                self.goal_x = None;
                self.dragging = true;
                self.keep_caret_visible(&layout, inner);
                ctx.capture_pointer();
                ctx.request_paint();
                ctx.set_handled();
            }
            Event::PointerMove { pos } if self.dragging => {
                let layout = self.layout(ctx.fonts(), &theme, inner);
                let idx = self.hit(&layout, inner, pos);
                if idx != self.edit.cursor {
                    self.edit.move_cursor(idx, true);
                    self.keep_caret_visible(&layout, inner);
                    ctx.request_paint();
                }
                ctx.set_handled();
            }
            Event::PointerUp {
                button: PointerButton::Left,
                ..
            } if self.dragging => {
                self.dragging = false;
                ctx.release_pointer();
                ctx.set_handled();
            }
            Event::LongPress { pos } => {
                let layout = self.layout(ctx.fonts(), &theme, inner);
                let idx = self.hit(&layout, inner, pos);
                let (a, w) = self.edit.word_at(idx);
                self.edit.select(a, w);
                self.goal_x = None;
                ctx.request_paint();
                ctx.set_handled();
            }
            Event::Scroll { delta_y, .. } => {
                let layout = self.layout(ctx.fonts(), &theme, inner);
                self.content_h = layout.size().h.max(layout.line_height());
                let max = self.max_scroll(inner);
                // Same sign convention as `ScrollView`: positive delta scrolls down.
                let next = (self.scroll_y + delta_y).clamp(0.0, max);
                if (next - self.scroll_y).abs() > f32::EPSILON {
                    self.scroll_y = next;
                    ctx.request_paint();
                    ctx.set_handled();
                }
                // At a bound the wheel bubbles so an enclosing ScrollView moves.
            }
            Event::Key {
                key,
                pressed: true,
                mods,
            } if ctx.is_focused() => {
                let layout = self.layout(ctx.fonts(), &theme, inner);
                let extend = mods.shift;
                let mut handled = true;
                let mut changed = false;
                let vertical = matches!(key, Key::Up | Key::Down | Key::PageUp | Key::PageDown);
                match key {
                    Key::Up | Key::Down if !mods.ctrl => {
                        self.move_vertical(&layout, key == Key::Down, extend);
                    }
                    Key::PageUp | Key::PageDown => {
                        self.move_page(&layout, inner, key == Key::PageDown, extend);
                    }
                    Key::Home | Key::End if !mods.ctrl => {
                        self.move_line_edge(&layout, key == Key::End, extend);
                    }
                    _ => {
                        let mut clipboard = ctx.clipboard().to_string();
                        let applied = self.edit.apply(key, mods, true, &mut clipboard);
                        if clipboard != ctx.clipboard() {
                            ctx.set_clipboard(clipboard);
                        }
                        handled = applied.handled;
                        changed = applied.changed;
                    }
                }
                if !vertical {
                    self.goal_x = None;
                }
                if handled {
                    // Re-shape after an edit so the caret lands on the new text.
                    let layout = if changed {
                        self.layout(ctx.fonts(), &theme, inner)
                    } else {
                        layout
                    };
                    self.keep_caret_visible(&layout, inner);
                    if changed {
                        self.fire(ctx);
                    }
                    ctx.request_paint();
                    ctx.set_handled();
                }
            }
            Event::FocusLost => {
                self.dragging = false;
                ctx.request_paint();
            }
            _ => {}
        }
    }

    fn debug_name(&self) -> &'static str {
        "TextArea"
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
