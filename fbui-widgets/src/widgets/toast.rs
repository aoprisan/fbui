//! [`Toasts`] — transient notifications floating above the page.
//!
//! `Toasts` is a *host*: a zero-size widget (add it once, anywhere — typically
//! the last child of the root) that owns a queue of messages and paints them
//! as a **floating overlay** ([`Widget::overlay_rect`]) stacked bottom-center
//! of the surface. Push from `App::update` via
//! [`Ui::with`](crate::Ui::with):
//!
//! ```ignore
//! ui.with::<Toasts, _>(toasts, |t| t.push(ToastKind::Success, "Saved"));
//! ```
//!
//! Each toast lives for a few seconds ([`Toasts::push_for`] overrides), fades
//! out on the frame clock, and disappears — driven entirely by
//! [`animate`](Widget::animate), so an idle screen with no toasts costs
//! nothing. Toasts are paint-only (no hit-testing): they never steal input.

use std::any::Any;

use fbui_render::geom::{Point, Rect, Size};
use fbui_render::Color;

use crate::ctx::PaintCtx;
use crate::style::{self, Style};
use crate::theme::Theme;
use crate::util::text_style;
use crate::widget::{Anim, Widget};

/// Card width (clamped to the surface), height, spacing, margins — logical px.
const CARD_W: f32 = 360.0;
const CARD_H: f32 = 44.0;
const CARD_GAP: f32 = 10.0;
const MARGIN: f32 = 16.0;
/// Fade-out tail at the end of a toast's life, seconds.
const FADE: f32 = 0.35;
/// Default time to live, seconds.
const DEFAULT_TTL: f32 = 3.0;
/// Success green (the palette has accent/danger; success is toast-local).
const SUCCESS: Color = Color::rgb(0x3d, 0xb2, 0x63);

/// The flavor of a toast, picking its edge color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    /// Neutral information (accent edge).
    Info,
    /// Something completed (green edge).
    Success,
    /// Something failed (danger edge).
    Error,
}

struct Entry {
    text: String,
    kind: ToastKind,
    /// Seconds this toast has been alive.
    age: f32,
    ttl: f32,
}

/// The toast host widget (see module docs).
#[derive(Default)]
pub struct Toasts {
    entries: Vec<Entry>,
}

impl Toasts {
    pub fn new() -> Self {
        Toasts::default()
    }

    /// Queue a toast with the default lifetime.
    pub fn push(&mut self, kind: ToastKind, text: impl Into<String>) {
        self.push_for(kind, text, DEFAULT_TTL);
    }

    /// Queue a toast that lives `ttl` seconds.
    pub fn push_for(&mut self, kind: ToastKind, text: impl Into<String>, ttl: f32) {
        self.entries.push(Entry {
            text: text.into(),
            kind,
            age: 0.0,
            ttl: ttl.max(FADE),
        });
    }

    /// How many toasts are currently showing.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn card_w(surface: Size) -> f32 {
        CARD_W.min(surface.w - 2.0 * MARGIN).max(0.0)
    }

    /// The rect of card `i` (0 = oldest, at the top of the pile; the newest
    /// sits nearest the bottom edge).
    fn card_rect(&self, i: usize, surface: Size) -> Rect {
        let w = Self::card_w(surface);
        let n = self.entries.len() as f32;
        let x = (surface.w - w) / 2.0;
        let y = surface.h - MARGIN - (n - i as f32) * CARD_H - (n - 1.0 - i as f32) * CARD_GAP;
        Rect::new(x, y, w, CARD_H)
    }
}

impl<Msg: 'static> Widget<Msg> for Toasts {
    fn layout_style(&self, _theme: &Theme) -> Style {
        // Out of the flow entirely: absolute, zero-size, no inset. The visible
        // cards live in the overlay, placed against the surface.
        Style {
            position: taffy::Position::Absolute,
            size: taffy::Size {
                width: style::length(0.0),
                height: style::length(0.0),
            },
            ..Style::default()
        }
    }

    fn overlay_rect(&self, _bounds: Rect, surface: Size) -> Option<Rect> {
        if self.entries.is_empty() {
            return None;
        }
        let first = self.card_rect(0, surface);
        let last = self.card_rect(self.entries.len() - 1, surface);
        Some(Rect::new(
            first.x,
            first.y,
            first.w,
            last.bottom() - first.y,
        ))
    }

    fn paint(&self, _ctx: &mut PaintCtx) {}

    fn paint_overlay(&self, ctx: &mut PaintCtx) {
        // ctx.bounds() is the overlay rect we reported; cards are laid out from
        // its top.
        let overlay = ctx.bounds();
        let theme = ctx.theme();
        let radius = theme.metrics.radius;
        let (bg, text_color, accent, danger, line) = (
            theme.palette.surface_alt,
            theme.palette.text,
            theme.palette.accent,
            theme.palette.danger,
            theme.palette.line,
        );
        let st = text_style(theme, theme.metrics.font_size, text_color);
        let font_size = theme.metrics.font_size;

        let cards: Vec<(Rect, f32, Color, &str)> = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let y = overlay.y + i as f32 * (CARD_H + CARD_GAP);
                let rect = Rect::new(overlay.x, y, overlay.w, CARD_H);
                let remaining = (e.ttl - e.age).max(0.0);
                let alpha = (remaining / FADE).clamp(0.0, 1.0);
                let edge = match e.kind {
                    ToastKind::Info => accent,
                    ToastKind::Success => SUCCESS,
                    ToastKind::Error => danger,
                };
                (rect, alpha, edge, e.text.as_str())
            })
            .collect();

        let (p, fonts) = ctx.painter_and_fonts();
        for (rect, alpha, edge, text) in cards {
            p.push_opacity(alpha);
            p.fill_rounded_rect(rect, radius, bg);
            p.stroke_rounded_rect(rect, radius, line, 1.0);
            // Kind-colored edge bar.
            p.fill_rounded_rect(Rect::new(rect.x, rect.y, 4.0, rect.h), 2.0, edge);
            let ty = rect.y + (rect.h - font_size) / 2.0 - 1.0;
            fonts.draw_text(p, text, &st, Point::new(rect.x + 16.0, ty), None);
            p.pop_opacity();
        }
    }

    fn animate(&mut self, dt: f32) -> Anim {
        if self.entries.is_empty() {
            return Anim::IDLE;
        }
        for e in &mut self.entries {
            e.age += dt;
        }
        self.entries.retain(|e| e.age < e.ttl);
        // The Ui damages this widget's overlay rect (current and last) for any
        // animating change, which is exactly the cards' footprint.
        Anim {
            repaint: true,
            relayout: false,
            running: !self.entries.is_empty(),
            damage: None,
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
