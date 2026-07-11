//! [`TabBar`] — a segmented tab strip for switching between views.

use std::any::Any;

use fbui_render::geom::{Point, Rect};

use crate::ctx::{EventCtx, PaintCtx};
use crate::event::{Event, Key, PointerButton};
use crate::style::{self, Style};
use crate::theme::Theme;
use crate::util::{focus_ring, text_style};
use crate::widget::Widget;

/// Bar height, logical px.
const HEIGHT: f32 = 36.0;
/// Inset from the track to the segments.
const INSET: f32 = 3.0;

/// A horizontal row of equal-width tabs; exactly one is selected. Emits
/// `on_select(index)` when the selection changes — by click/tap, or by
/// Left/Right/Home/End while focused. The bar owns which tab is active; the
/// app swaps the content below it in `update` (show/hide subtrees, or rebuild).
///
/// Like the [`Keyboard`](crate::widgets::Keyboard), it is one tree node that
/// paints its segments itself and hit-tests taps internally, so a tab switch
/// damages one widget. Equal-width segments keep the geometry independent of
/// label text (and of which fonts a host has).
pub struct TabBar<Msg> {
    labels: Vec<String>,
    selected: usize,
    /// The segment under a pressed pointer, armed like a Button press.
    pressed: Option<usize>,
    on_select: Option<Box<dyn Fn(usize) -> Msg>>,
}

impl<Msg> TabBar<Msg> {
    /// A bar with one tab per label; the first is selected.
    pub fn new(labels: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let labels: Vec<String> = labels.into_iter().map(Into::into).collect();
        TabBar {
            labels,
            selected: 0,
            pressed: None,
            on_select: None,
        }
    }

    /// Set the message factory invoked with the newly selected tab's index.
    pub fn on_select(mut self, f: impl Fn(usize) -> Msg + 'static) -> Self {
        self.on_select = Some(Box::new(f));
        self
    }

    /// Builder form of [`set_selected`](Self::set_selected).
    pub fn selected(mut self, index: usize) -> Self {
        self.set_selected(index);
        self
    }

    /// The active tab's index.
    pub fn selected_index(&self) -> usize {
        self.selected
    }

    /// Select a tab programmatically (clamped; emits nothing). Call via
    /// [`Ui::with`](crate::Ui::with), which repaints the bar.
    pub fn set_selected(&mut self, index: usize) {
        self.selected = index.min(self.labels.len().saturating_sub(1));
    }

    /// How many tabs the bar has.
    pub fn len(&self) -> usize {
        self.labels.len()
    }

    /// Whether the bar has no tabs.
    pub fn is_empty(&self) -> bool {
        self.labels.is_empty()
    }

    /// The rect of segment `index`, laid out to fill `bounds` (this widget's
    /// bounds) — the exact geometry `paint` and hit-testing use. Lets tests
    /// locate a tab without duplicating the layout constants.
    pub fn tab_rect(&self, bounds: Rect, index: usize) -> Option<Rect> {
        if index >= self.labels.len() {
            return None;
        }
        let n = self.labels.len() as f32;
        let w = (bounds.w - 2.0 * INSET) / n;
        let h = bounds.h - 2.0 * INSET;
        Some(Rect::new(
            bounds.x + INSET + index as f32 * w,
            bounds.y + INSET,
            w,
            h,
        ))
    }

    /// The segment containing `pos`, if any.
    fn tab_at(&self, bounds: Rect, pos: Point) -> Option<usize> {
        (0..self.labels.len()).find(|&i| {
            self.tab_rect(bounds, i)
                .is_some_and(|r| r.contains_point(pos))
        })
    }

    /// Change selection and emit `on_select`; a no-op when already there.
    fn select(&mut self, index: usize, ctx: &mut EventCtx<Msg>) {
        if index == self.selected || index >= self.labels.len() {
            return;
        }
        self.selected = index;
        if let Some(f) = &self.on_select {
            ctx.emit(f(index));
        }
        ctx.request_paint();
    }
}

impl<Msg: 'static> Widget<Msg> for TabBar<Msg> {
    fn layout_style(&self, _theme: &Theme) -> Style {
        Style {
            size: taffy::Size {
                width: style::percent(1.0),
                height: style::length(HEIGHT),
            },
            flex_grow: 0.0,
            flex_shrink: 0.0,
            ..Style::default()
        }
    }

    fn focusable(&self) -> bool {
        true
    }

    fn paint(&self, ctx: &mut PaintCtx) {
        let b = ctx.bounds();
        let focused = ctx.is_focused();
        let theme = ctx.theme();
        let radius = theme.metrics.radius;
        let fw = theme.metrics.focus_width;
        let font_size = theme.metrics.font_size;
        let track = theme.palette.surface_alt;
        let active = theme.palette.accent;
        let ring = theme.palette.accent;
        let active_st = text_style(theme, font_size, theme.palette.on_accent);
        let idle_st = text_style(theme, font_size, theme.palette.text);
        let selected = self.selected;

        let (p, fonts) = ctx.painter_and_fonts();
        p.fill_rounded_rect(b, radius, track);
        for (i, label) in self.labels.iter().enumerate() {
            let Some(r) = self.tab_rect(b, i) else { break };
            if i == selected {
                p.fill_rounded_rect(r, (radius - INSET).max(0.0), active);
            }
            let st = if i == selected { &active_st } else { &idle_st };
            let ts = fonts.layout(label, st, None).size();
            let tx = r.x + (r.w - ts.w) / 2.0;
            let ty = r.y + (r.h - ts.h) / 2.0;
            fonts.draw_text(p, label, st, Point::new(tx, ty), None);
        }
        if focused {
            focus_ring(p, b, radius, ring, fw);
        }
    }

    fn event(&mut self, ctx: &mut EventCtx<Msg>) {
        let b = ctx.bounds();
        let ev = ctx.event().clone();
        match ev {
            Event::PointerDown {
                button: PointerButton::Left,
                pos,
            } => {
                self.pressed = self.tab_at(b, pos);
                ctx.capture_pointer();
                ctx.request_focus();
                ctx.request_paint();
                ctx.set_handled();
            }
            Event::PointerUp {
                button: PointerButton::Left,
                pos,
            } => {
                // Fire only if the release lands on the pressed segment.
                let down = self.pressed.take();
                ctx.release_pointer();
                if let Some(i) = down {
                    if self.tab_at(b, pos) == Some(i) {
                        self.select(i, ctx);
                    }
                }
                ctx.set_handled();
            }
            Event::Key {
                key, pressed: true, ..
            } if ctx.is_focused() => {
                let last = self.labels.len().saturating_sub(1);
                match key {
                    Key::Left => self.select(self.selected.saturating_sub(1), ctx),
                    Key::Right => self.select((self.selected + 1).min(last), ctx),
                    Key::Home => self.select(0, ctx),
                    Key::End => self.select(last, ctx),
                    _ => return,
                }
                ctx.set_handled();
            }
            _ => {}
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
