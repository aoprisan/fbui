//! A custom widget built from scratch: `Dot`, a tappable disc that pulses.
//!
//! The point of this example is that `Dot` lives *here*, in application code —
//! not in the toolkit — yet drops into the tree like any built-in widget. It
//! exercises every hook a typical interactive widget needs: `measure` (an
//! intrinsic size), `paint`, `event` (pointer + keyboard, emitting a `Msg`),
//! `animate` (a frame-clock pulse via a `Tween`), and `focusable`.
//!
//! ```text
//! cargo run -p fbui --example custom_widget --features platform
//! ```
//! Click the dot (or Tab to it and press Space/Enter). Esc quits.

use std::any::Any;

use fbui::anim::{Easing, Tween};
use fbui::ctx::EventCtx;
use fbui::event::{Event, Key, PointerButton};
use fbui::render::geom::{Rect, Size};
use fbui::render::{Color, FontContext};
use fbui::widget::{AvailableSize, KnownDims};
use fbui::widgets::{Align, Container, Label};
use fbui::{Anim, App, PaintCtx, Style, Theme, Ui, Widget, WidgetId};

/// A circular, focusable button that emits `on_tap` and gives a little pulse of
/// feedback when activated.
struct Dot<Msg> {
    radius: f32,
    pressed: bool,
    /// A scale factor on the disc radius, animated back to `1.0` after a tap.
    pulse: Tween<f32>,
    on_tap: Option<Box<dyn Fn() -> Msg>>,
}

impl<Msg> Dot<Msg> {
    /// How far the disc swells at the start of a tap, and how long the pulse runs.
    const MAX_PULSE: f32 = 1.35;
    const PULSE_SECS: f32 = 0.22;

    fn new(radius: f32) -> Self {
        Dot {
            radius,
            pressed: false,
            pulse: Tween::settled(1.0, Self::PULSE_SECS, Easing::EaseOut),
            on_tap: None,
        }
    }

    /// Set the message factory invoked on tap.
    fn on_tap(mut self, f: impl Fn() -> Msg + 'static) -> Self {
        self.on_tap = Some(Box::new(f));
        self
    }

    /// Emit the tap message and kick off the pulse animation.
    fn fire(&mut self, ctx: &mut EventCtx<Msg>) {
        if let Some(f) = &self.on_tap {
            ctx.emit(f());
        }
        // Start swollen and ease back to rest; ask the Ui to keep the clock alive.
        self.pulse = Tween::new(Self::MAX_PULSE, 1.0, Self::PULSE_SECS, Easing::EaseOut);
        ctx.request_anim();
    }
}

impl<Msg: 'static> Widget<Msg> for Dot<Msg> {
    fn layout_style(&self, _theme: &Theme) -> Style {
        Style::default()
    }

    fn measure(
        &mut self,
        _fonts: &mut FontContext,
        _theme: &Theme,
        _known: KnownDims,
        _available: AvailableSize,
    ) -> Option<Size> {
        // Reserve room for the fully-swollen pulse so it never clips.
        let side = 2.0 * self.radius * Self::MAX_PULSE;
        Some(Size::new(side, side))
    }

    fn focusable(&self) -> bool {
        true
    }

    fn paint(&self, ctx: &mut PaintCtx) {
        let b = ctx.bounds();
        let focused = ctx.is_focused();
        // Pull what we need from the theme before borrowing the painter.
        let theme = ctx.theme();
        let accent = theme.palette.accent;
        let ring_color = theme.palette.text;
        let focus_width = theme.metrics.focus_width;

        let fill = if self.pressed {
            darken(accent, 0.8)
        } else {
            accent
        };
        let (cx, cy) = (b.x + b.w / 2.0, b.y + b.h / 2.0);
        let r = self.radius * self.pulse.value();

        let p = ctx.painter();
        // A circle is a rounded rect with a corner radius of half its side.
        let disc = Rect::new(cx - r, cy - r, 2.0 * r, 2.0 * r);
        p.fill_rounded_rect(disc, r, fill);
        if focused {
            let rr = self.radius;
            let ring = Rect::new(cx - rr, cy - rr, 2.0 * rr, 2.0 * rr);
            p.stroke_rounded_rect(ring, rr, ring_color, focus_width);
        }
    }

    fn animate(&mut self, dt: f32) -> Anim {
        if self.pulse.is_done() {
            return Anim::IDLE;
        }
        let running = self.pulse.advance(dt);
        // Repaint each frame; let the clock stop once the pulse settles.
        Anim {
            repaint: true,
            running,
            ..Anim::IDLE
        }
    }

    fn event(&mut self, ctx: &mut EventCtx<Msg>) {
        match ctx.event().clone() {
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

/// Scale each RGB channel toward black by `factor` (0 = black, 1 = unchanged).
fn darken(c: Color, factor: f32) -> Color {
    let ch = |v: u8| (v as f32 * factor).round().clamp(0.0, 255.0) as u8;
    Color::rgba(ch(c.r), ch(c.g), ch(c.b), c.a)
}

#[derive(Clone)]
enum Msg {
    Tapped,
}

#[derive(Default)]
struct Demo {
    taps: u32,
    count: Option<WidgetId>,
}

impl App for Demo {
    type Message = Msg;

    fn build(&mut self, ui: &mut Ui<Msg>) {
        let root = ui.set_root(
            Container::column()
                .fill()
                .padding(24.0)
                .gap(20.0)
                .align(Align::Center),
        );

        ui.add_child(root, Label::new("Custom widget: Dot").size(24.0).bold());
        ui.add_child(root, Dot::new(40.0).on_tap(|| Msg::Tapped));
        self.count = Some(ui.add_child(root, Label::new("Taps: 0").size(18.0)));
    }

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Tapped => self.taps += 1,
        }
        let text = format!("Taps: {}", self.taps);
        if let Some(id) = self.count {
            ui.with::<Label, _>(id, |l| l.set_text(text));
        }
    }
}

fn main() {
    if let Err(e) = fbui::run(Demo::default()) {
        eprintln!("custom_widget: {e}");
        std::process::exit(1);
    }
}
