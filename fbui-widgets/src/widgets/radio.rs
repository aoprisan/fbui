//! [`RadioGroup`] — a single-choice list of options.
//!
//! A radio group is one widget, not one-per-option: it owns the list of labels
//! and the selected index, takes a single tab stop, and moves the selection with
//! the arrow keys (the conventional, accessible behavior for a radio group — Tab
//! moves *between* groups, arrows move *within* one). It emits `on_change(index)`
//! when the selection changes, mirroring [`Checkbox`](super::Checkbox)'s
//! `on_toggle`.

use std::any::Any;

use fbui_render::geom::{Point, Rect, Size};
use fbui_render::FontContext;

use crate::ctx::{EventCtx, PaintCtx};
use crate::event::{Event, Key, PointerButton};
use crate::style::Style;
use crate::theme::Theme;
use crate::util::{focus_ring, text_style};
use crate::widget::{AvailableSize, KnownDims, Widget};

/// Diameter of the radio disc, logical px.
const DISC: f32 = 20.0;
/// Gap between the disc and its label.
const GAP: f32 = 8.0;
/// Vertical gap between option rows.
const ROW_GAP: f32 = 6.0;

/// A vertical list of options where exactly one is selected. Toggles on click or
/// arrow keys; emits `on_change(new_index)`.
pub struct RadioGroup<Msg> {
    options: Vec<String>,
    selected: usize,
    on_change: Option<Box<dyn Fn(usize) -> Msg>>,
}

impl<Msg> RadioGroup<Msg> {
    /// A group over `options`, with the first option selected.
    pub fn new(options: impl IntoIterator<Item = impl Into<String>>) -> Self {
        RadioGroup {
            options: options.into_iter().map(Into::into).collect(),
            selected: 0,
            on_change: None,
        }
    }

    /// Pre-select an option by index (clamped to the option count).
    pub fn selected(mut self, index: usize) -> Self {
        self.selected = self.clamp(index);
        self
    }

    pub fn on_change(mut self, f: impl Fn(usize) -> Msg + 'static) -> Self {
        self.on_change = Some(Box::new(f));
        self
    }

    /// The selected option index.
    pub fn selection(&self) -> usize {
        self.selected
    }

    /// Set the selection (call via [`Ui::with`](crate::Ui::with)); clamped to the
    /// option count. Does not emit `on_change`.
    pub fn set_selection(&mut self, index: usize) {
        self.selected = self.clamp(index);
    }

    fn clamp(&self, index: usize) -> usize {
        index.min(self.options.len().saturating_sub(1))
    }

    /// Row pitch (row height + gap). The disc sets the row height, so geometry is
    /// font-independent and identical in `measure` and `paint`.
    fn pitch(&self) -> f32 {
        DISC + ROW_GAP
    }

    /// Move the selection to `index`, emitting + repainting if it changed.
    fn select(&mut self, index: usize, ctx: &mut EventCtx<Msg>) {
        let index = self.clamp(index);
        if index != self.selected {
            self.selected = index;
            if let Some(f) = &self.on_change {
                ctx.emit(f(index));
            }
            ctx.request_paint();
        }
        // A click or arrow on the group is handled even when it lands on the
        // current selection (it shouldn't fall through to widgets behind).
        ctx.set_handled();
    }
}

impl<Msg: 'static> Widget<Msg> for RadioGroup<Msg> {
    fn layout_style(&self, _theme: &Theme) -> Style {
        Style::default()
    }

    fn measure(
        &mut self,
        fonts: &mut FontContext,
        theme: &Theme,
        _known: KnownDims,
        _available: AvailableSize,
    ) -> Option<Size> {
        if self.options.is_empty() {
            return Some(Size::new(0.0, 0.0));
        }
        let st = text_style(theme, theme.metrics.font_size, theme.palette.text);
        let mut max_w = 0.0f32;
        for opt in &self.options {
            max_w = max_w.max(fonts.layout(opt, &st, None).size().w);
        }
        let n = self.options.len() as f32;
        let w = DISC + GAP + max_w;
        let h = n * DISC + (n - 1.0) * ROW_GAP;
        Some(Size::new(w, h))
    }

    fn focusable(&self) -> bool {
        true
    }

    fn paint(&self, ctx: &mut PaintCtx) {
        let theme = ctx.theme();
        let b = ctx.bounds();
        let focused = ctx.is_focused();
        let st = text_style(theme, theme.metrics.font_size, theme.palette.text);
        let (accent, surface, line, on_accent, fw) = (
            theme.palette.accent,
            theme.palette.surface,
            theme.palette.line,
            theme.palette.on_accent,
            theme.metrics.focus_width,
        );
        let pitch = self.pitch();
        let selected = self.selected;
        let options = self.options.clone();

        let (p, fonts) = ctx.painter_and_fonts();
        for (i, opt) in options.iter().enumerate() {
            let ry = b.y + i as f32 * pitch;
            let disc = Rect::new(b.x, ry, DISC, DISC);
            if i == selected {
                // Filled disc with a contrasting inner dot.
                p.fill_rounded_rect(disc, DISC / 2.0, accent);
                let dot = disc.inset(DISC * 0.3);
                p.fill_rounded_rect(dot, dot.w / 2.0, on_accent);
            } else {
                p.fill_rounded_rect(disc, DISC / 2.0, surface);
                p.stroke_rounded_rect(disc, DISC / 2.0, line, 1.5);
            }
            let ty = ry + (DISC - st.size) / 2.0 - 1.0;
            fonts.draw_text(p, opt, &st, Point::new(b.x + DISC + GAP, ty), None);
        }
        if focused {
            focus_ring(p, b, 6.0, accent, fw);
        }
    }

    fn event(&mut self, ctx: &mut EventCtx<Msg>) {
        match ctx.event().clone() {
            Event::PointerDown {
                button: PointerButton::Left,
                ..
            } => {
                ctx.request_focus();
                if let Some(local) = ctx.local_pointer() {
                    let row = (local.y / self.pitch()).floor();
                    if row >= 0.0 {
                        self.select(row as usize, ctx);
                    }
                }
            }
            Event::Key {
                key: Key::Down | Key::Right,
                pressed: true,
                ..
            } if ctx.is_focused() => {
                self.select(self.selected + 1, ctx);
            }
            Event::Key {
                key: Key::Up | Key::Left,
                pressed: true,
                ..
            } if ctx.is_focused() => {
                self.select(self.selected.saturating_sub(1), ctx);
            }
            _ => {}
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
