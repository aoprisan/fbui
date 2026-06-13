//! [`Button`] — a clickable, focusable label.

use std::any::Any;

use fbui_render::geom::{Point, Size};
use fbui_render::{Color, FontContext};

use crate::ctx::{EventCtx, PaintCtx};
use crate::event::{Event, Key, PointerButton};
use crate::style::{self, Style};
use crate::theme::Theme;
use crate::util::{darken, focus_ring, text_style};
use crate::widget::{AvailableSize, KnownDims, Widget};

/// A button's visual role, which picks its fill from the theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ButtonVariant {
    /// Accent-filled — the primary action. The default.
    #[default]
    Primary,
    /// Neutral surface fill, for secondary actions (Cancel, Back).
    Secondary,
    /// Danger fill, for destructive actions (Delete, Erase).
    Danger,
}

/// A push button. Emits its `on_press` message on click (or Space/Enter when
/// focused).
pub struct Button<Msg> {
    label: String,
    on_press: Option<Box<dyn Fn() -> Msg>>,
    variant: ButtonVariant,
    pressed: bool,
}

impl<Msg> Button<Msg> {
    pub fn new(label: impl Into<String>) -> Self {
        Button {
            label: label.into(),
            on_press: None,
            variant: ButtonVariant::Primary,
            pressed: false,
        }
    }

    /// Set the message factory invoked on press.
    pub fn on_press(mut self, f: impl Fn() -> Msg + 'static) -> Self {
        self.on_press = Some(Box::new(f));
        self
    }

    /// Set the visual variant.
    pub fn variant(mut self, variant: ButtonVariant) -> Self {
        self.variant = variant;
        self
    }

    /// Shorthand for the [`Secondary`](ButtonVariant::Secondary) variant.
    pub fn secondary(self) -> Self {
        self.variant(ButtonVariant::Secondary)
    }

    /// Shorthand for the [`Danger`](ButtonVariant::Danger) variant — a
    /// destructive action.
    pub fn danger(self) -> Self {
        self.variant(ButtonVariant::Danger)
    }

    /// `(fill, text, focus-ring)` colors for this variant.
    fn colors(&self, theme: &Theme) -> (Color, Color, Color) {
        let p = &theme.palette;
        match self.variant {
            ButtonVariant::Primary => (p.accent, p.on_accent, p.accent),
            ButtonVariant::Secondary => (p.surface_alt, p.text, p.accent),
            ButtonVariant::Danger => (p.danger, p.on_accent, p.danger),
        }
    }

    pub fn set_label(&mut self, label: impl Into<String>) {
        self.label = label.into();
    }

    fn fire(&self, ctx: &mut EventCtx<Msg>) {
        if let Some(f) = &self.on_press {
            ctx.emit(f());
        }
    }

    const PAD_X: f32 = 14.0;
    const PAD_Y: f32 = 8.0;
}

impl<Msg: 'static> Widget<Msg> for Button<Msg> {
    fn layout_style(&self, _theme: &Theme) -> Style {
        Style {
            padding: style::uniform(0.0),
            ..Style::default()
        }
    }

    fn measure(
        &mut self,
        fonts: &mut FontContext,
        theme: &Theme,
        _known: KnownDims,
        _available: AvailableSize,
    ) -> Option<Size> {
        let st = text_style(theme, theme.metrics.font_size, theme.palette.on_accent);
        let t = fonts.layout(&self.label, &st, None).size();
        Some(Size::new(t.w + 2.0 * Self::PAD_X, t.h + 2.0 * Self::PAD_Y))
    }

    fn focusable(&self) -> bool {
        true
    }

    fn paint(&self, ctx: &mut PaintCtx) {
        let b = ctx.bounds();
        let focused = ctx.is_focused();
        // Extract everything from the theme before borrowing the painter.
        let theme = ctx.theme();
        let radius = theme.metrics.radius;
        let fw = theme.metrics.focus_width;
        let (fill, fg, ring) = self.colors(theme);
        let st = text_style(theme, theme.metrics.font_size, fg);
        let bg = if self.pressed {
            darken(fill, 0.8)
        } else {
            fill
        };
        let label = &self.label;

        let (p, fonts) = ctx.painter_and_fonts();
        p.fill_rounded_rect(b, radius, bg);
        // Center the label.
        let ts = fonts.layout(label, &st, None).size();
        let tx = b.x + (b.w - ts.w) / 2.0;
        let ty = b.y + (b.h - ts.h) / 2.0;
        fonts.draw_text(p, label, &st, Point::new(tx, ty), None);
        if focused {
            focus_ring(p, b, radius, ring, fw);
        }
    }

    fn event(&mut self, ctx: &mut EventCtx<Msg>) {
        let ev = ctx.event().clone();
        match ev {
            Event::PointerDown {
                button: PointerButton::Left,
                ..
            } => {
                self.pressed = true;
                ctx.capture_pointer();
                ctx.request_focus();
                ctx.request_paint();
                ctx.set_handled();
            }
            Event::PointerUp {
                button: PointerButton::Left,
                pos,
            } => {
                let inside = ctx.bounds().contains_point(pos);
                if self.pressed && inside {
                    self.fire(ctx);
                }
                self.pressed = false;
                ctx.release_pointer();
                ctx.request_paint();
                ctx.set_handled();
            }
            Event::Key {
                key: Key::Space | Key::Enter,
                pressed: true,
                ..
            } => {
                if ctx.is_focused() {
                    self.fire(ctx);
                    ctx.request_paint();
                    ctx.set_handled();
                }
            }
            _ => {}
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
