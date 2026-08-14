//! [`Navigator`] — a stack of screens with slide transitions: the multi-page
//! backbone of a kiosk app (attract → menu → form → confirmation).
//!
//! Screens are ordinary widget subtrees, kept **retained**: pushing a new
//! screen covers the previous one without tearing it down, so its scroll
//! positions, text, and toggles survive until it's shown again. Only the
//! active screen is interactive — covered screens drop out of hit-testing and
//! the Tab order (via [`Widget::active_children`]) — and popped screens are
//! removed from the tree after their exit transition settles (via
//! [`Widget::take_child_removals`]).
//!
//! ## Layout & transition model
//!
//! Screen `i` is absolutely positioned at `i × 100%` of the navigator's width
//! ([`Widget::position_child`]), forming one long horizontal strip; the
//! navigator clips and slides the strip with its
//! [`content_offset`](Widget::content_offset), tweened between screen
//! indices. Because a slide moves everything in the viewport uniformly, the
//! transition rides the **scroll-blit fast path**: each frame memmoves the
//! previous frame's pixels sideways and repaints only the strip that scrolled
//! into view. Offsets are snapped to device pixels so the blit and a full
//! repaint place content identically; a behavior test pins the equivalence
//! (per the fast-path invariant) under the snapshot tolerance — tiny-skia's
//! anti-aliasing of shapes clipped by the canvas edge is not exactly
//! translation-invariant, so a few seam pixels may wobble by a couple of
//! code-values, and anything beyond that fails the test.
//!
//! ## Driving it
//!
//! Tree mutation is the app's side of the contract, so push/pop are static
//! helpers taking the [`Ui`]:
//!
//! ```ignore
//! let nav = ui.set_root(Navigator::new());
//! let home = Navigator::push(&mut ui, nav, Container::column());
//! // … build the home screen under `home` …
//! // later, from App::update:
//! let detail = Navigator::push(&mut ui, nav, Container::column());
//! Navigator::pop(&mut ui, nav);   // slides back; removes `detail` when settled
//! ```
//!
//! Pushing remembers what was focused; popping restores it — per-screen focus
//! memory. With [`back_key`](Navigator::back_key) enabled (default), an
//! Escape that bubbles up from inside the stack pops one screen (the runner
//! reserves Escape for app exit, so this is for embedders that deliver it —
//! and for hardware "back" buttons routed through
//! [`Ui::send_key`](crate::Ui::send_key)).

use std::any::Any;
use std::ops::Range;

use fbui_render::geom::{Point, Rect};
use fbui_render::Scale;

use crate::anim::{Easing, Tween};
use crate::ctx::EventCtx;
use crate::event::{Event, Key};
use crate::style::{self, Style};
use crate::theme::Theme;
use crate::tree::{Ui, WidgetId};
use crate::widget::{Anim, Widget};
use crate::PaintCtx;

/// Default transition duration, seconds.
const DEFAULT_DURATION: f32 = 0.25;

/// A screen-stack container with slide transitions (see module docs).
pub struct Navigator<Msg> {
    /// Index of the active (target) screen.
    top: usize,
    /// Slide position in screen units, tweened toward `top`.
    pos: Tween<f32>,
    duration: f32,
    easing: Easing,
    /// Laid-out bounds + scale, cached from `placed` for offset snapping.
    bounds: Rect,
    scale: f32,
    /// Pending horizontal blit (logical px) for the next paint.
    blit_dx: f32,
    /// A pop happened: remove screens above `top` once the slide settles.
    reap: bool,
    /// Focus remembered per covered screen (what was focused when the screen
    /// above it was pushed), restored on pop.
    focus_stack: Vec<Option<WidgetId>>,
    /// Emitted when the back key pops a screen.
    on_back: Option<Msg>,
    /// Whether a bubbling Escape pops (default true).
    back_key: bool,
}

impl<Msg> Navigator<Msg> {
    pub fn new() -> Self {
        Navigator {
            top: 0,
            pos: Tween::settled(0.0, DEFAULT_DURATION, Easing::EaseInOut),
            duration: DEFAULT_DURATION,
            easing: Easing::EaseInOut,
            bounds: Rect::new(0.0, 0.0, 0.0, 0.0),
            scale: 1.0,
            blit_dx: 0.0,
            reap: false,
            focus_stack: Vec::new(),
            on_back: None,
            back_key: true,
        }
    }

    /// Set the slide duration in seconds; `0` makes every transition instant.
    pub fn duration(mut self, seconds: f32) -> Self {
        self.duration = seconds.max(0.0);
        self.pos = Tween::settled(self.top as f32, self.duration, self.easing);
        self
    }

    /// Set the easing curve (default [`Easing::EaseInOut`]).
    pub fn easing(mut self, easing: Easing) -> Self {
        self.easing = easing;
        self.pos = Tween::settled(self.top as f32, self.duration, self.easing);
        self
    }

    /// Enable/disable popping on a bubbling Escape key (default enabled).
    pub fn back_key(mut self, on: bool) -> Self {
        self.back_key = on;
        self
    }

    /// Message to emit when the back key pops a screen.
    pub fn on_back(mut self, msg: Msg) -> Self {
        self.on_back = Some(msg);
        self
    }

    /// The active screen's index (0 = the root screen).
    pub fn depth(&self) -> usize {
        self.top
    }

    /// Whether a slide transition is currently running.
    pub fn is_transitioning(&self) -> bool {
        !self.pos.is_done()
    }

    /// The slide offset in logical px, snapped to device pixels so the blit
    /// fast path and the repaint place content identically.
    fn snapped_offset(&self) -> f32 {
        let px = self.pos.value() * self.bounds.w * self.scale;
        px.round() / self.scale.max(f32::EPSILON)
    }

    // -- state transitions (widget side; the static push/pop/settle helpers
    // below drive these together with the tree mutations) --

    /// Record a push: the new screen is already added as the child at
    /// `index`. `prev_focus` is what held focus on the screen it covers. The
    /// very first screen (index 0) appears in place — there's nothing to
    /// slide over, and no focus to remember.
    fn note_push_at(&mut self, index: usize, prev_focus: Option<WidgetId>) {
        if index == 0 {
            self.top = 0;
            self.pos = Tween::settled(0.0, self.duration, self.easing);
            return;
        }
        self.focus_stack.push(prev_focus);
        self.top = index;
        self.pos.retarget(index as f32);
    }

    /// Record a pop, arming the reap of screens above the new top once the
    /// slide settles. Returns the focus to restore, or `None` when already at
    /// the root screen.
    fn note_pop(&mut self) -> Option<Option<WidgetId>> {
        if self.top == 0 {
            return None;
        }
        self.top -= 1;
        self.pos.retarget(self.top as f32);
        self.reap = true;
        Some(self.focus_stack.pop().unwrap_or(None))
    }

    /// Snap any in-flight transition to its end state, returning the child
    /// indices (out of `len`) whose reap was pending. Used by
    /// [`Navigator::push`] so a push during a pop's exit slide starts from
    /// settled indices.
    fn snap(&mut self, len: usize) -> Vec<usize> {
        self.pos = Tween::settled(self.top as f32, self.duration, self.easing);
        self.blit_dx = 0.0;
        if self.reap {
            self.reap = false;
            (self.top + 1..len).collect()
        } else {
            Vec::new()
        }
    }
}

impl<Msg: Clone + 'static> Navigator<Msg> {
    /// Push `screen` onto `nav`'s stack, sliding it in from the right.
    /// Returns the screen's id — add the screen's content as children of it.
    /// Focus is remembered (for the eventual pop) and cleared; call
    /// [`Ui::focus_first`] on the returned id to move it into the new screen.
    ///
    /// Any in-flight transition is snapped to its end state first, so screen
    /// indices are settled when the new one takes its slot.
    pub fn push(ui: &mut Ui<Msg>, nav: WidgetId, screen: impl Widget<Msg>) -> WidgetId {
        Navigator::<Msg>::settle(ui, nav);
        let prev_focus = ui.focused();
        let id = ui.add_child(nav, screen);
        let index = ui.child_ids(nav).len() - 1;
        ui.with(nav, |n: &mut Navigator<Msg>| {
            n.note_push_at(index, prev_focus)
        });
        ui.focus(None);
        id
    }

    /// Pop the top screen, sliding back to the one beneath and restoring the
    /// focus remembered when it was covered. The popped screen (and any
    /// in-flight push above it) leaves the tree once the slide settles.
    /// Returns `false` (and does nothing) at the root screen.
    pub fn pop(ui: &mut Ui<Msg>, nav: WidgetId) -> bool {
        let restore = ui.with(nav, |n: &mut Navigator<Msg>| n.note_pop());
        match restore {
            Some(Some(focus)) => {
                ui.focus(focus);
                true
            }
            _ => false,
        }
    }

    /// Finish any in-flight transition instantly: the slide snaps to its end
    /// state and pop-retired screens are removed now. [`push`](Self::push)
    /// calls this; it's public for apps that want a hard cut.
    pub fn settle(ui: &mut Ui<Msg>, nav: WidgetId) {
        let kids = ui.child_ids(nav);
        let removals = ui
            .with(nav, |n: &mut Navigator<Msg>| n.snap(kids.len()))
            .unwrap_or_default();
        for idx in removals {
            if let Some(&cid) = kids.get(idx) {
                ui.remove(cid);
            }
        }
    }
}

impl<Msg> Default for Navigator<Msg> {
    fn default() -> Self {
        Navigator::new()
    }
}

impl<Msg: Clone + 'static> Widget<Msg> for Navigator<Msg> {
    fn layout_style(&self, _theme: &Theme) -> Style {
        Style {
            display: taffy::Display::Flex,
            // A positioned containing block: screens' percentage insets
            // resolve against this box.
            position: taffy::Position::Relative,
            size: taffy::Size {
                width: style::percent(1.0),
                height: style::percent(1.0),
            },
            flex_grow: 1.0,
            ..Style::default()
        }
    }

    fn position_child(&self, index: usize, style: &mut Style) {
        // Screen i fills the box, parked at i × 100% along the strip.
        style.position = taffy::Position::Absolute;
        style.inset = taffy::Rect {
            left: taffy::LengthPercentageAuto::percent(index as f32),
            top: taffy::LengthPercentageAuto::length(0.0),
            right: taffy::LengthPercentageAuto::auto(),
            bottom: taffy::LengthPercentageAuto::auto(),
        };
        style.size = taffy::Size {
            width: style::percent(1.0),
            height: style::percent(1.0),
        };
    }

    fn active_children(&self, len: usize) -> Option<Range<usize>> {
        // Only the target screen is interactive — covered screens keep their
        // state but not their clicks, and mid-transition input goes to where
        // the slide is headed.
        let top = self.top.min(len.saturating_sub(1));
        Some(top..(top + 1).min(len))
    }

    fn take_child_removals(&mut self, len: usize) -> Vec<usize> {
        if self.reap && self.pos.is_done() {
            self.reap = false;
            (self.top + 1..len).collect()
        } else {
            Vec::new()
        }
    }

    fn clips(&self) -> bool {
        true
    }

    fn content_offset(&self) -> Point {
        Point::new(-self.snapped_offset(), 0.0)
    }

    fn placed(&mut self, bounds: Rect, scale: Scale) {
        if bounds.w != self.bounds.w {
            // A resize moves every screen; discard any pending blit (the
            // full relayout repaints everything anyway).
            self.blit_dx = 0.0;
        }
        self.bounds = bounds;
        self.scale = scale.factor();
    }

    fn event(&mut self, ctx: &mut EventCtx<Msg>) {
        if let Event::Key {
            key: Key::Escape,
            pressed: true,
            ..
        } = ctx.event()
        {
            if self.back_key && self.top > 0 {
                if let Some(restore) = self.note_pop() {
                    match restore {
                        Some(f) => ctx.focus_widget(f),
                        None => ctx.clear_focus(),
                    }
                    if let Some(msg) = self.on_back.clone() {
                        ctx.emit(msg);
                    }
                    ctx.request_anim();
                    ctx.set_handled();
                }
            }
        }
    }

    fn animate(&mut self, dt: f32) -> Anim {
        if self.pos.is_done() {
            // Settled — but a zero-duration pop still owes its reap, which
            // `take_child_removals` hands over right after this returns.
            return Anim::IDLE;
        }
        let old = self.snapped_offset();
        let running = self.pos.advance(dt);
        let new = self.snapped_offset();
        if (new - old).abs() > f32::EPSILON {
            // Content shifts opposite the offset change; the pixels move via
            // the blit, the exposed strip is damaged when it's applied.
            self.blit_dx += old - new;
        }
        Anim {
            repaint: false,
            // Children re-place at the new offset each frame.
            relayout: true,
            running,
            // The blit accounts for all pixels; report empty precise damage
            // so the whole box isn't repainted.
            damage: Some(Rect::new(0.0, 0.0, 0.0, 0.0)),
        }
    }

    fn scroll_blit_xy(&mut self, bounds: Rect) -> Option<(Rect, f32, f32)> {
        if self.blit_dx.abs() < f32::EPSILON {
            None
        } else {
            Some((bounds, std::mem::take(&mut self.blit_dx), 0.0))
        }
    }

    fn paint(&self, _ctx: &mut PaintCtx) {
        // Chrome-free: the screens paint everything. (The Ui's region clear
        // paints the theme background behind a not-yet-covered area.)
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
