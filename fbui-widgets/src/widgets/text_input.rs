//! [`TextInput`] — a single-line editable field with a caret and selection.
//!
//! Editing semantics come from the shared [`EditState`] core (character and
//! word deletion, word jumps, select-all, cut/copy/paste against the
//! [`Ui`](crate::Ui)'s clipboard); this widget adds the single-line specifics:
//! pointer placement and drag-selection via the shaped layout's hit-testing, a
//! long-press selecting the word under the finger, and a horizontal scroll that
//! keeps the caret inside the box when the value outgrows it. See
//! `docs/text-editing.md` for the key table. Still **no IME**.

use std::any::Any;

use fbui_render::geom::{Point, Rect};
use fbui_render::{FontContext, TextLayout, TextStyle};

use super::edit::EditState;
use crate::ctx::{EventCtx, PaintCtx};
use crate::describe::Describe;
use crate::event::{Event, Key, Modifiers, PointerButton};
use crate::style::{self, Style};
use crate::theme::Theme;
use crate::util::text_style;
use crate::widget::Widget;

const PAD: f32 = 8.0;
const HEIGHT: f32 = 36.0;

/// A single-line text field.
pub struct TextInput<Msg> {
    edit: EditState,
    placeholder: String,
    on_change: Option<Box<dyn Fn(String) -> Msg>>,
    /// Horizontal scroll (logical px) so the caret stays visible in a long value.
    scroll_x: f32,
    /// A press is down and motion extends the selection.
    dragging: bool,
}

impl<Msg> TextInput<Msg> {
    pub fn new() -> Self {
        TextInput {
            edit: EditState::default(),
            placeholder: String::new(),
            on_change: None,
            scroll_x: 0.0,
            dragging: false,
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

    /// How far the text is scrolled left (logical px) to keep the caret
    /// visible in a value wider than the box; `0` while it fits.
    pub fn scroll_offset(&self) -> f32 {
        self.scroll_x
    }

    fn style_for(&self, theme: &Theme) -> TextStyle {
        text_style(theme, theme.metrics.font_size, theme.palette.text)
    }

    fn layout(&self, fonts: &mut FontContext, theme: &Theme) -> TextLayout {
        fonts.layout(&self.edit.text, &self.style_for(theme), None)
    }

    fn fire(&self, ctx: &mut EventCtx<Msg>) {
        if let Some(f) = &self.on_change {
            ctx.emit(f(self.edit.text.clone()));
        }
    }

    /// Apply a key directly to this field, bypassing the event system —
    /// insert/backspace/delete/cursor semantics match hardware typing (no
    /// modifiers, so no Shift-extend and no Ctrl chords). Returns whether the
    /// text changed.
    ///
    /// **This does *not* fire `on_change`** — it is a plain state mutation for
    /// programmatic edits (call it via [`Ui::with`](crate::Ui::with); read
    /// [`text`](Self::text) afterwards). To route an on-screen
    /// [`Keyboard`](crate::widgets::Keyboard)'s taps to the focused field, use
    /// [`Ui::send_key`](crate::Ui::send_key) instead: it replays the key
    /// through the real event path, so `on_change` fires and repaint is
    /// requested exactly as if the key had been typed on hardware.
    pub fn apply_key(&mut self, key: Key) -> bool {
        let mut scratch = String::new();
        self.edit
            .apply(key, Modifiers::default(), false, &mut scratch)
            .changed
    }

    /// Byte offset under surface point `pos`, honoring the scroll offset.
    fn hit(&self, fonts: &mut FontContext, theme: &Theme, bounds: Rect, pos: Point) -> usize {
        let layout = self.layout(fonts, theme);
        let local_x = pos.x - (bounds.x + PAD) + self.scroll_x;
        layout.hit(local_x, layout.line_height() / 2.0)
    }

    /// Scroll horizontally so the caret is inside the visible text box.
    fn keep_caret_visible(&mut self, fonts: &mut FontContext, theme: &Theme, bounds: Rect) {
        let layout = self.layout(fonts, theme);
        let visible = (bounds.w - 2.0 * PAD).max(1.0);
        let caret_x = layout.caret(self.edit.cursor).x;
        let max_scroll = (layout.size().w - visible + 2.0).max(0.0);
        if caret_x - self.scroll_x > visible {
            self.scroll_x = caret_x - visible + 1.0;
        } else if caret_x - self.scroll_x < 0.0 {
            self.scroll_x = caret_x;
        }
        self.scroll_x = self.scroll_x.clamp(0.0, max_scroll);
    }
}

impl<Msg> Default for TextInput<Msg> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Msg: 'static> Widget<Msg> for TextInput<Msg> {
    fn layout_style(&self, _theme: &Theme) -> Style {
        Style {
            size: taffy::Size {
                width: style::auto(),
                height: style::length(HEIGHT),
            },
            min_size: taffy::Size {
                width: style::length(120.0),
                height: style::length(HEIGHT),
            },
            flex_grow: 1.0,
            ..Style::default()
        }
    }

    fn focusable(&self) -> bool {
        true
    }

    fn paint(&self, ctx: &mut PaintCtx) {
        let theme = ctx.theme();
        let b = ctx.bounds();
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

        let text_origin = Point::new(b.x + PAD - self.scroll_x, b.y + (b.h - st.size) / 2.0 - 1.0);
        let (p, fonts) = ctx.painter_and_fonts();

        p.fill_rounded_rect(b, radius, surface);
        p.stroke_rounded_rect(
            b,
            radius,
            if focused { accent } else { line },
            if focused { 2.0 } else { 1.0 },
        );

        p.push_clip(Rect::new(b.x + PAD, b.y, b.w - 2.0 * PAD, b.h));

        if self.edit.text.is_empty() && !placeholder.is_empty() {
            fonts.draw_text(
                p,
                &placeholder,
                &placeholder_style,
                Point::new(b.x + PAD, text_origin.y),
                None,
            );
        } else {
            let layout = fonts.layout(&self.edit.text, &st, None);
            if sel.0 != sel.1 {
                for r in layout.selection_rects(sel.0, sel.1) {
                    p.fill_rect(
                        Rect::new(text_origin.x + r.x, b.y + 4.0, r.w, b.h - 8.0),
                        accent_sel,
                    );
                }
            }
            fonts.draw(p, &layout, st.color, text_origin);
            if focused {
                let cx = text_origin.x + layout.caret(cursor).x;
                p.fill_rect(Rect::new(cx, b.y + 6.0, 1.5, b.h - 12.0), accent);
            }
        }
        p.pop_clip();
    }

    fn event(&mut self, ctx: &mut EventCtx<Msg>) {
        let ev = ctx.event().clone();
        let b = ctx.bounds();
        match ev {
            Event::PointerDown {
                button: PointerButton::Left,
                pos,
            } => {
                ctx.request_focus();
                let theme = ctx.theme().clone();
                let idx = self.hit(ctx.fonts(), &theme, b, pos);
                self.edit.move_cursor(idx, false);
                self.dragging = true;
                ctx.capture_pointer();
                ctx.request_paint();
                ctx.set_handled();
            }
            Event::PointerMove { pos } if self.dragging => {
                let theme = ctx.theme().clone();
                let idx = self.hit(ctx.fonts(), &theme, b, pos);
                if idx != self.edit.cursor {
                    self.edit.move_cursor(idx, true);
                    self.keep_caret_visible(ctx.fonts(), &theme, b);
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
                // Touch has no double-click: a long-press selects the word.
                let theme = ctx.theme().clone();
                let idx = self.hit(ctx.fonts(), &theme, b, pos);
                let (a, w) = self.edit.word_at(idx);
                self.edit.select(a, w);
                ctx.request_paint();
                ctx.set_handled();
            }
            Event::Key {
                key,
                pressed: true,
                mods,
            } if ctx.is_focused() => {
                let mut clipboard = ctx.clipboard().to_string();
                let applied = self.edit.apply(key, mods, false, &mut clipboard);
                if applied.handled {
                    if clipboard != ctx.clipboard() {
                        ctx.set_clipboard(clipboard);
                    }
                    let theme = ctx.theme().clone();
                    self.keep_caret_visible(ctx.fonts(), &theme, b);
                    if applied.changed {
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
        "TextInput"
    }

    fn describe(&self, out: &mut Describe) {
        out.text(&self.edit.text);
        if !self.placeholder.is_empty() {
            out.prop("placeholder", &self.placeholder);
        }
        out.prop("cursor", self.edit.cursor);
        if self.edit.has_selection() {
            let (a, b) = self.edit.selection();
            out.prop("selection", format!("{a}..{b}"));
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
