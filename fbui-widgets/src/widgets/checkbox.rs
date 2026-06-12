//! [`Checkbox`] — a labeled boolean toggle.

use std::any::Any;

use fbui_render::geom::{Point, Rect, Size};
use fbui_render::{FontContext, PathBuilder};

use crate::ctx::{EventCtx, PaintCtx};
use crate::event::{Event, Key, PointerButton};
use crate::style::Style;
use crate::theme::Theme;
use crate::util::{focus_ring, text_style};
use crate::widget::{AvailableSize, KnownDims, Widget};

const BOX: f32 = 20.0;
const GAP: f32 = 8.0;

/// A checkbox with a trailing label. Toggles on click or Space; emits
/// `on_toggle(new_value)`.
pub struct Checkbox<Msg> {
    label: String,
    checked: bool,
    on_toggle: Option<Box<dyn Fn(bool) -> Msg>>,
}

impl<Msg> Checkbox<Msg> {
    pub fn new(label: impl Into<String>, checked: bool) -> Self {
        Checkbox {
            label: label.into(),
            checked,
            on_toggle: None,
        }
    }

    pub fn on_toggle(mut self, f: impl Fn(bool) -> Msg + 'static) -> Self {
        self.on_toggle = Some(Box::new(f));
        self
    }

    /// Current state.
    pub fn checked(&self) -> bool {
        self.checked
    }

    /// Set state (call via [`Ui::with`](crate::Ui::with)).
    pub fn set_checked(&mut self, checked: bool) {
        self.checked = checked;
    }

    fn toggle(&mut self, ctx: &mut EventCtx<Msg>) {
        self.checked = !self.checked;
        if let Some(f) = &self.on_toggle {
            ctx.emit(f(self.checked));
        }
        ctx.request_paint();
        ctx.set_handled();
    }
}

impl<Msg: 'static> Widget<Msg> for Checkbox<Msg> {
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
        let st = text_style(theme, theme.metrics.font_size, theme.palette.text);
        let t = fonts.layout(&self.label, &st, None).size();
        Some(Size::new(BOX + GAP + t.w, BOX.max(t.h)))
    }

    fn focusable(&self) -> bool {
        true
    }

    fn paint(&self, ctx: &mut PaintCtx) {
        let theme = ctx.theme();
        let b = ctx.bounds();
        let focused = ctx.is_focused();
        let box_rect = Rect::new(b.x, b.y + (b.h - BOX) / 2.0, BOX, BOX);
        let st = text_style(theme, theme.metrics.font_size, theme.palette.text);
        let (accent, surface, line, on_accent) = (
            theme.palette.accent,
            theme.palette.surface,
            theme.palette.line,
            theme.palette.on_accent,
        );
        let fw = theme.metrics.focus_width;
        let checked = self.checked;

        let (p, fonts) = ctx.painter_and_fonts();
        if checked {
            p.fill_rounded_rect(box_rect, 4.0, accent);
            // Tick mark.
            let mut tick = PathBuilder::new();
            tick.move_to(box_rect.x + 4.0, box_rect.y + BOX * 0.55);
            tick.line_to(box_rect.x + BOX * 0.42, box_rect.y + BOX - 5.0);
            tick.line_to(box_rect.x + BOX - 4.0, box_rect.y + 5.0);
            if let Some(path) = tick.finish() {
                p.stroke_path(&path, on_accent, 2.5);
            }
        } else {
            p.fill_rounded_rect(box_rect, 4.0, surface);
            p.stroke_rounded_rect(box_rect, 4.0, line, 1.5);
        }
        let ty = b.y + (b.h - st.size) / 2.0 - 1.0;
        fonts.draw_text(p, &self.label, &st, Point::new(b.x + BOX + GAP, ty), None);
        if focused {
            focus_ring(p, b, 6.0, accent, fw);
        }
    }

    fn event(&mut self, ctx: &mut EventCtx<Msg>) {
        let ev = ctx.event().clone();
        match ev {
            Event::PointerDown {
                button: PointerButton::Left,
                ..
            } => {
                ctx.request_focus();
                self.toggle(ctx);
            }
            Event::Key {
                key: Key::Space,
                pressed: true,
                ..
            } if ctx.is_focused() => {
                self.toggle(ctx);
            }
            _ => {}
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
