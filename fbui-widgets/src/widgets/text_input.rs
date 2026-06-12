//! [`TextInput`] — a single-line editable field with a caret and selection.
//!
//! v1 scope per PLAN: cursor + selection + basic editing, **no IME**, no
//! clipboard, single line. Caret hit-testing measures substring widths, which is
//! O(n) per click but fine for the short strings a field holds.

use std::any::Any;

use fbui_render::geom::{Point, Rect};
use fbui_render::{FontContext, TextStyle};

use crate::ctx::{EventCtx, PaintCtx};
use crate::event::{Event, Key, PointerButton};
use crate::style::{self, Style};
use crate::theme::Theme;
use crate::util::text_style;
use crate::widget::Widget;

const PAD: f32 = 8.0;
const HEIGHT: f32 = 36.0;

/// A single-line text field.
pub struct TextInput<Msg> {
    text: String,
    placeholder: String,
    cursor: usize,
    anchor: usize,
    on_change: Option<Box<dyn Fn(String) -> Msg>>,
}

impl<Msg> TextInput<Msg> {
    pub fn new() -> Self {
        TextInput {
            text: String::new(),
            placeholder: String::new(),
            cursor: 0,
            anchor: 0,
            on_change: None,
        }
    }

    pub fn placeholder(mut self, text: impl Into<String>) -> Self {
        self.placeholder = text.into();
        self
    }

    pub fn value(mut self, text: impl Into<String>) -> Self {
        self.text = text.into();
        self.cursor = self.text.len();
        self.anchor = self.cursor;
        self
    }

    pub fn on_change(mut self, f: impl Fn(String) -> Msg + 'static) -> Self {
        self.on_change = Some(Box::new(f));
        self
    }

    /// The current text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Replace the text (call via [`Ui::with`](crate::Ui::with)).
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.cursor.min(self.text.len());
        self.anchor = self.anchor.min(self.text.len());
    }

    fn style_for(&self, theme: &Theme) -> TextStyle {
        text_style(theme, theme.metrics.font_size, theme.palette.text)
    }

    fn selection(&self) -> (usize, usize) {
        (self.cursor.min(self.anchor), self.cursor.max(self.anchor))
    }

    fn has_selection(&self) -> bool {
        self.cursor != self.anchor
    }

    fn delete_selection(&mut self) -> bool {
        if !self.has_selection() {
            return false;
        }
        let (a, b) = self.selection();
        self.text.replace_range(a..b, "");
        self.cursor = a;
        self.anchor = a;
        true
    }

    fn insert(&mut self, s: &str) {
        self.delete_selection();
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
        self.anchor = self.cursor;
    }

    fn prev_boundary(&self, i: usize) -> usize {
        self.text[..i]
            .char_indices()
            .next_back()
            .map(|(idx, _)| idx)
            .unwrap_or(0)
    }

    fn next_boundary(&self, i: usize) -> usize {
        self.text[i..]
            .char_indices()
            .nth(1)
            .map(|(idx, _)| i + idx)
            .unwrap_or(self.text.len())
    }

    fn move_cursor(&mut self, to: usize, extend: bool) {
        self.cursor = to;
        if !extend {
            self.anchor = to;
        }
    }

    fn fire(&self, ctx: &mut EventCtx<Msg>) {
        if let Some(f) = &self.on_change {
            ctx.emit(f(self.text.clone()));
        }
    }

    /// Logical x of the caret/byte boundary `idx`, measured from the text origin.
    fn x_of(&self, fonts: &mut FontContext, style: &TextStyle, idx: usize) -> f32 {
        if idx == 0 {
            return 0.0;
        }
        fonts.layout(&self.text[..idx], style, None).size().w
    }

    /// Nearest byte boundary to local x (measured from the text origin).
    fn idx_at_x(&self, fonts: &mut FontContext, style: &TextStyle, x: f32) -> usize {
        let mut best = 0usize;
        let mut best_d = f32::MAX;
        let mut idx = 0usize;
        loop {
            let cx = self.x_of(fonts, style, idx);
            let d = (cx - x).abs();
            if d < best_d {
                best_d = d;
                best = idx;
            }
            if idx >= self.text.len() {
                break;
            }
            idx = self.next_boundary(idx);
        }
        best
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
        let (text, cursor, sel) = (self.text.clone(), self.cursor, self.selection());
        let placeholder = self.placeholder.clone();

        let text_origin = Point::new(b.x + PAD, b.y + (b.h - st.size) / 2.0 - 1.0);
        let (p, fonts) = ctx.painter_and_fonts();

        p.fill_rounded_rect(b, radius, surface);
        p.stroke_rounded_rect(
            b,
            radius,
            if focused { accent } else { line },
            if focused { 2.0 } else { 1.0 },
        );

        p.push_clip(Rect::new(b.x + PAD, b.y, b.w - 2.0 * PAD, b.h));

        if text.is_empty() && !placeholder.is_empty() {
            fonts.draw_text(p, &placeholder, &placeholder_style, text_origin, None);
        } else {
            // Selection highlight.
            if sel.0 != sel.1 {
                let x0 = self.x_of(fonts, &st, sel.0);
                let x1 = self.x_of(fonts, &st, sel.1);
                p.fill_rect(
                    Rect::new(text_origin.x + x0, b.y + 4.0, x1 - x0, b.h - 8.0),
                    accent_sel,
                );
            }
            fonts.draw_text(p, &text, &st, text_origin, None);
        }

        // Caret.
        if focused {
            let cx = text_origin.x + self.x_of(fonts, &st, cursor);
            p.fill_rect(Rect::new(cx, b.y + 6.0, 1.5, b.h - 12.0), accent);
        }
        p.pop_clip();
    }

    fn event(&mut self, ctx: &mut EventCtx<Msg>) {
        let ev = ctx.event().clone();
        match ev {
            Event::PointerDown {
                button: PointerButton::Left,
                pos,
            } => {
                ctx.request_focus();
                let b = ctx.bounds();
                let st = self.style_for(ctx.theme());
                let local_x = pos.x - (b.x + PAD);
                let idx = self.idx_at_x(ctx.fonts(), &st, local_x);
                self.move_cursor(idx, false);
                ctx.request_paint();
                ctx.set_handled();
            }
            Event::Key {
                key,
                pressed: true,
                mods,
            } if ctx.is_focused() => {
                let mut changed = true;
                match key {
                    Key::Char(c) => self.insert(&c.to_string()),
                    Key::Space => self.insert(" "),
                    Key::Backspace => {
                        if !self.delete_selection() && self.cursor > 0 {
                            let prev = self.prev_boundary(self.cursor);
                            self.text.replace_range(prev..self.cursor, "");
                            self.cursor = prev;
                            self.anchor = prev;
                        }
                    }
                    Key::Delete => {
                        if !self.delete_selection() && self.cursor < self.text.len() {
                            let next = self.next_boundary(self.cursor);
                            self.text.replace_range(self.cursor..next, "");
                        }
                    }
                    Key::Left => {
                        let to = self.prev_boundary(self.cursor);
                        self.move_cursor(to, mods.shift);
                        changed = false;
                    }
                    Key::Right => {
                        let to = self.next_boundary(self.cursor);
                        self.move_cursor(to, mods.shift);
                        changed = false;
                    }
                    Key::Home => {
                        self.move_cursor(0, mods.shift);
                        changed = false;
                    }
                    Key::End => {
                        self.move_cursor(self.text.len(), mods.shift);
                        changed = false;
                    }
                    _ => changed = false,
                }
                if changed {
                    self.fire(ctx);
                }
                ctx.request_paint();
                ctx.set_handled();
            }
            _ => {}
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
