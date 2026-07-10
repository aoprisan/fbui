//! [`Keyboard`] — an on-screen (virtual) keyboard for touch kiosks.
//!
//! A kiosk panel often has a touchscreen and **no physical keyboard**, so text
//! fields need an on-screen way to type. This widget is a docked key grid: it
//! paints all its keys itself and hit-tests taps internally (one tree node, one
//! damage region — the [`Select`](crate::widgets::Select) menu pattern rather
//! than a `Container` full of `Button`s).
//!
//! Two design constraints, both load-bearing (see `DESIGN.md`):
//!
//! 1. **It never takes focus.** Every focusable widget grabs focus on
//!    pointer-down; if the keyboard did too, it would steal focus from the
//!    [`TextInput`](crate::widgets::TextInput) being edited, whose key handling
//!    is gated on being focused. So `Keyboard` is non-focusable and never calls
//!    `request_focus` — the field you tapped keeps focus while you type.
//! 2. **Keys travel as an application message.** A widget can only
//!    [`emit`](crate::ctx::EventCtx::emit) a `Msg`; it cannot inject a synthetic
//!    key event. Each key tap emits `on_key(Key)`, and the app routes it to the
//!    focused field with [`Ui::send_key`](crate::Ui::send_key), which replays it
//!    through the same event path as hardware input (so `on_change` fires too):
//!
//! ```ignore
//! let kb = ui.add_child(root, Keyboard::new().on_key(Msg::Kbd));
//! // in App::update:
//! Msg::Kbd(k) => ui.send_key(k),
//! ```
//!
//! Typing behaves like a phone keyboard: **Shift is one-shot** (it reverts to
//! lower-case after the next character), and **Backspace auto-repeats** while
//! held — driven by the frame `dt` through
//! [`Widget::animate_with`](crate::Widget::animate_with), never a wall clock,
//! so it stays deterministic and testable.

use std::any::Any;

use fbui_render::geom::{Point, Rect};
use fbui_render::Color;

use crate::ctx::{AnimCtx, EventCtx, PaintCtx};
use crate::event::{Event, Key, PointerButton};
use crate::style::{self, Style};
use crate::theme::Theme;
use crate::util::{darken, text_style};
use crate::widget::{Anim, Widget};

/// Default keyboard height, logical px (four rows plus padding).
const DEFAULT_HEIGHT: f32 = 232.0;
/// Padding around the whole key grid.
const PAD: f32 = 8.0;
/// Gap between adjacent keys.
const GAP: f32 = 6.0;
/// Hold time before Backspace starts auto-repeating, seconds.
const REPEAT_DELAY: f32 = 0.45;
/// Interval between auto-repeated Backspaces once repeating, seconds.
const REPEAT_INTERVAL: f32 = 0.06;

/// Which glyph set the keyboard is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layer {
    /// Lower-case letters.
    Lower,
    /// Upper-case letters (Shift engaged).
    Upper,
    /// Digits and punctuation.
    Symbols,
}

/// A key's visual role, which picks its colors from the theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// A normal character key.
    Normal,
    /// An engaged modifier / primary action (Shift when active, Enter).
    Accent,
    /// A destructive key (Backspace).
    Danger,
}

/// What tapping a key does.
#[derive(Debug, Clone, Copy)]
enum Action {
    /// Emit this key to the application.
    Emit(Key),
    /// Toggle Shift (Lower ⇄ Upper).
    Shift,
    /// Toggle the symbols layer (letters ⇄ symbols).
    Symbols,
}

/// One key: its label, what it does, its relative width, and its role.
struct KeyDef {
    label: String,
    action: Action,
    weight: f32,
    role: Role,
}

impl KeyDef {
    /// A 1-wide character key.
    fn ch(c: char) -> Self {
        KeyDef {
            label: c.to_string(),
            action: Action::Emit(Key::Char(c)),
            weight: 1.0,
            role: Role::Normal,
        }
    }

    fn wide(label: &str, action: Action, weight: f32, role: Role) -> Self {
        KeyDef {
            label: label.to_string(),
            action,
            weight,
            role,
        }
    }
}

/// Auto-repeat state for a held Backspace key: time since press, and how many
/// repeats have fired (the release tap is suppressed once any have).
struct Repeat {
    t: f32,
    fired: u32,
}

/// An on-screen keyboard. Emits `on_key(Key)` on each key tap; toggles its
/// Shift / symbols layers internally.
pub struct Keyboard<Msg> {
    layer: Layer,
    /// The current layer's keys, rebuilt only on layer switch (not per event
    /// or paint — key labels are owned `String`s).
    rows: Vec<Vec<KeyDef>>,
    /// The `(row, col)` of the key under the finger, for pressed feedback.
    pressed: Option<(usize, usize)>,
    /// Armed while a Backspace is held; drives auto-repeat from `animate_with`.
    repeat: Option<Repeat>,
    height: f32,
    on_key: Option<Box<dyn Fn(Key) -> Msg>>,
}

impl<Msg> Keyboard<Msg> {
    pub fn new() -> Self {
        Keyboard {
            layer: Layer::Lower,
            rows: build_rows(Layer::Lower),
            pressed: None,
            repeat: None,
            height: DEFAULT_HEIGHT,
            on_key: None,
        }
    }

    /// Set the message factory invoked with each tapped [`Key`].
    pub fn on_key(mut self, f: impl Fn(Key) -> Msg + 'static) -> Self {
        self.on_key = Some(Box::new(f));
        self
    }

    /// Override the docked height (logical px).
    pub fn height(mut self, h: f32) -> Self {
        self.height = h;
        self
    }

    /// The rect of the key at `(row, col)` of the current layer, laid out to
    /// fill `bounds` (this widget's bounds) — the exact geometry `paint` and
    /// hit-testing use. Lets tests, and apps that highlight keys, locate a key
    /// without duplicating the layout constants.
    pub fn key_rect(&self, bounds: Rect, row: usize, col: usize) -> Option<Rect> {
        self.geometry(bounds, &self.rows)
            .get(row)?
            .get(col)
            .copied()
    }

    /// Switch layers, rebuilding the cached key rows.
    fn set_layer(&mut self, layer: Layer) {
        self.layer = layer;
        self.rows = build_rows(layer);
    }

    /// Absolute rects for every key in `rows`, laid out to fill `b`. Shared by
    /// `paint` and `event` so drawing and hit-testing can never disagree.
    fn geometry(&self, b: Rect, rows: &[Vec<KeyDef>]) -> Vec<Vec<Rect>> {
        let n = rows.len().max(1) as f32;
        let usable_w = (b.w - 2.0 * PAD).max(0.0);
        let row_h = ((b.h - 2.0 * PAD) - (n - 1.0) * GAP).max(0.0) / n;
        let mut out = Vec::with_capacity(rows.len());
        for (ri, row) in rows.iter().enumerate() {
            let ry = b.y + PAD + ri as f32 * (row_h + GAP);
            let total: f32 = row.iter().map(|k| k.weight).sum::<f32>().max(0.001);
            let keys = row.len().max(1) as f32;
            let content_w = (usable_w - (keys - 1.0) * GAP).max(0.0);
            let mut x = b.x + PAD;
            let mut rects = Vec::with_capacity(row.len());
            for k in row {
                let kw = content_w * (k.weight / total);
                rects.push(Rect::new(x, ry, kw, row_h));
                x += kw + GAP;
            }
            out.push(rects);
        }
        out
    }

    /// The `(row, col)` of the key containing `pos`, if any.
    fn key_at(&self, pos: Point, geom: &[Vec<Rect>]) -> Option<(usize, usize)> {
        for (ri, row) in geom.iter().enumerate() {
            for (ki, rect) in row.iter().enumerate() {
                if rect.contains_point(pos) {
                    return Some((ri, ki));
                }
            }
        }
        None
    }

    /// `(fill, text)` colors for a key's role and pressed state.
    fn colors(&self, role: Role, pressed: bool, theme: &Theme) -> (Color, Color) {
        let p = &theme.palette;
        let (fill, text) = match role {
            Role::Normal => (p.surface_alt, p.text),
            Role::Accent => (p.accent, p.on_accent),
            Role::Danger => (p.danger, p.on_accent),
        };
        if pressed {
            (darken(fill, 0.8), text)
        } else {
            (fill, text)
        }
    }
}

impl<Msg> Default for Keyboard<Msg> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Msg: 'static> Widget<Msg> for Keyboard<Msg> {
    fn layout_style(&self, _theme: &Theme) -> Style {
        Style {
            size: taffy::Size {
                width: style::percent(1.0),
                height: style::length(self.height),
            },
            flex_grow: 0.0,
            flex_shrink: 0.0,
            ..Style::default()
        }
    }

    fn paint(&self, ctx: &mut PaintCtx) {
        let b = ctx.bounds();
        let region = ctx.region();
        let theme = ctx.theme();
        let radius = theme.metrics.radius;
        let font_size = theme.metrics.font_size;
        let bg = theme.palette.surface;
        // A theme-derived text style, cloned per role's color below.
        let base_style = text_style(theme, font_size, theme.palette.text);
        let pressed = self.pressed;

        let rows = &self.rows;
        let geom = self.geometry(b, rows);
        // Resolve every key's colors up front, so the theme borrow is dropped
        // before we take the painter (which borrows `ctx` mutably).
        let styled: Vec<Vec<(Color, Color)>> = rows
            .iter()
            .enumerate()
            .map(|(ri, row)| {
                row.iter()
                    .enumerate()
                    .map(|(ki, k)| self.colors(k.role, pressed == Some((ri, ki)), theme))
                    .collect()
            })
            .collect();

        let (p, fonts) = ctx.painter_and_fonts();
        // A backing panel behind the keys, so the docked bar reads as one surface.
        p.fill_rect(b, bg);

        for (ri, row) in rows.iter().enumerate() {
            for (ki, key) in row.iter().enumerate() {
                let rect = geom[ri][ki];
                // Skip keys outside the damage region (small repaints stay small).
                if rect.right() < region.x
                    || rect.x > region.right()
                    || rect.bottom() < region.y
                    || rect.y > region.bottom()
                {
                    continue;
                }
                let (fill, fg) = styled[ri][ki];
                p.fill_rounded_rect(rect, radius, fill);
                if !key.label.is_empty() {
                    let mut st = base_style.clone();
                    st.color = fg;
                    let ts = fonts.layout(&key.label, &st, None).size();
                    let tx = rect.x + (rect.w - ts.w) / 2.0;
                    let ty = rect.y + (rect.h - ts.h) / 2.0;
                    fonts.draw_text(p, &key.label, &st, Point::new(tx, ty), None);
                }
            }
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
                let geom = self.geometry(b, &self.rows);
                if let Some(hit) = self.key_at(pos, &geom) {
                    self.pressed = Some(hit);
                    // Backspace auto-repeats while held: arm the timer and ask
                    // for animation ticks (walked only while something animates).
                    if matches!(self.rows[hit.0][hit.1].action, Action::Emit(Key::Backspace)) {
                        self.repeat = Some(Repeat { t: 0.0, fired: 0 });
                        ctx.request_anim();
                    }
                    // Capture (not focus) so a slight finger slide keeps the key
                    // armed — this does NOT move focus off the text field.
                    ctx.capture_pointer();
                    ctx.request_paint();
                    ctx.set_handled();
                }
            }
            Event::PointerMove { pos } => {
                // Sliding off a repeating Backspace aborts it — holding a
                // repeat must be cancellable without lifting onto another key.
                if self.repeat.is_some() {
                    if let Some(down) = self.pressed {
                        let geom = self.geometry(b, &self.rows);
                        if self.key_at(pos, &geom) != Some(down) {
                            self.repeat = None;
                            self.pressed = None;
                            ctx.request_paint();
                        }
                    }
                }
            }
            Event::PointerUp {
                button: PointerButton::Left,
                pos,
            } => {
                // If the hold already auto-repeated, the release is spent.
                let repeated = self.repeat.take().is_some_and(|r| r.fired > 0);
                if let Some(down) = self.pressed.take() {
                    let geom = self.geometry(b, &self.rows);
                    // Fire only if the release lands on the same key (like Button).
                    if self.key_at(pos, &geom) == Some(down) {
                        // Copy the action out so the arms can mutate `self`.
                        let action = self.rows[down.0][down.1].action;
                        match action {
                            Action::Emit(key) => {
                                if !repeated {
                                    if let Some(f) = &self.on_key {
                                        ctx.emit(f(key));
                                    }
                                }
                                // Shift is one-shot: typing a character drops
                                // back to lower-case, like a phone keyboard.
                                if self.layer == Layer::Upper && matches!(key, Key::Char(_)) {
                                    self.set_layer(Layer::Lower);
                                }
                            }
                            Action::Shift => {
                                self.set_layer(if self.layer == Layer::Upper {
                                    Layer::Lower
                                } else {
                                    Layer::Upper
                                });
                            }
                            Action::Symbols => {
                                self.set_layer(if self.layer == Layer::Symbols {
                                    Layer::Lower
                                } else {
                                    Layer::Symbols
                                });
                            }
                        }
                    }
                    ctx.release_pointer();
                    ctx.request_paint();
                    ctx.set_handled();
                }
            }
            _ => {}
        }
    }

    fn animate_with(&mut self, ctx: &mut AnimCtx<'_, Msg>) -> Anim {
        let Some(rep) = &mut self.repeat else {
            return Anim::IDLE;
        };
        rep.t += ctx.dt();
        // Emit one Backspace per elapsed interval past the hold delay, however
        // the frame `dt` happens to slice the time.
        while rep.t >= REPEAT_DELAY + rep.fired as f32 * REPEAT_INTERVAL {
            rep.fired += 1;
            if let Some(f) = &self.on_key {
                ctx.emit(f(Key::Backspace));
            }
        }
        // Nothing repaints while held (the key already shows pressed); just
        // keep the clock running until release or slide-off clears `repeat`.
        Anim {
            running: true,
            ..Anim::IDLE
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// The key rows of `layer`, built once per layer switch (see `Keyboard::rows`).
fn build_rows(layer: Layer) -> Vec<Vec<KeyDef>> {
    match layer {
        Layer::Lower => letter_rows(false),
        Layer::Upper => letter_rows(true),
        Layer::Symbols => symbol_rows(),
    }
}

/// The letter layout (QWERTY). `upper` selects the Shift layer.
fn letter_rows(upper: bool) -> Vec<Vec<KeyDef>> {
    let cv = |c: char| if upper { c.to_ascii_uppercase() } else { c };
    let letters = |s: &str| s.chars().map(|c| KeyDef::ch(cv(c))).collect::<Vec<_>>();

    let mut row3 = vec![KeyDef::wide(
        "Shift",
        Action::Shift,
        1.6,
        if upper { Role::Accent } else { Role::Normal },
    )];
    row3.extend(letters("zxcvbnm"));
    row3.push(KeyDef::wide(
        "Bksp",
        Action::Emit(Key::Backspace),
        1.6,
        Role::Danger,
    ));

    vec![
        letters("qwertyuiop"),
        letters("asdfghjkl"),
        row3,
        vec![
            KeyDef::wide("?123", Action::Symbols, 1.6, Role::Normal),
            KeyDef::ch(','),
            KeyDef::wide("space", Action::Emit(Key::Space), 5.0, Role::Normal),
            KeyDef::ch('.'),
            KeyDef::wide("Enter", Action::Emit(Key::Enter), 1.6, Role::Accent),
        ],
    ]
}

/// The digits-and-punctuation layout.
fn symbol_rows() -> Vec<Vec<KeyDef>> {
    let chars = |s: &str| s.chars().map(KeyDef::ch).collect::<Vec<_>>();

    let mut row3 = chars("*.,?!'");
    row3.push(KeyDef::wide(
        "Bksp",
        Action::Emit(Key::Backspace),
        1.6,
        Role::Danger,
    ));

    vec![
        chars("1234567890"),
        chars("-/:;()$&@\""),
        row3,
        vec![
            KeyDef::wide("ABC", Action::Symbols, 1.6, Role::Normal),
            KeyDef::ch('_'),
            KeyDef::wide("space", Action::Emit(Key::Space), 5.0, Role::Normal),
            KeyDef::ch('+'),
            KeyDef::wide("Enter", Action::Emit(Key::Enter), 1.6, Role::Accent),
        ],
    ]
}
