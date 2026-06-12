//! Touch/pointer gesture recognition (Phase 4).
//!
//! A small, **headless, deterministic** state machine that turns a single stream
//! of pointer/touch contacts — down, move, up, with a caller-supplied millisecond
//! timestamp — into higher-level [`Gesture`]s: tap, long-press, drag, and fling.
//! It deliberately knows nothing about the platform or the widget tree, so it is
//! unit-testable in isolation and reusable by any embedder. The umbrella
//! `fbui::run` runner owns one and feeds it both mouse-button drags and the
//! primary touch contact, so the two are recognized identically — the "unified
//! with pointer events" the plan calls for.
//!
//! ## What it recognizes
//!
//! * [`Gesture::Tap`] — a down/up within [`tap_slop`](GestureConfig::tap_slop)
//!   and before the long-press timeout.
//! * [`Gesture::LongPress`] — a contact held past
//!   [`long_press_ms`](GestureConfig::long_press_ms) without moving past
//!   [`tap_slop`]. Fired from [`poll`](GestureRecognizer::poll), which the caller
//!   ticks on its frame clock (no timers live in here).
//! * [`Gesture::DragBegin`] / [`DragUpdate`](Gesture::DragUpdate) /
//!   [`DragEnd`](Gesture::DragEnd) — once a contact moves past the slop.
//! * [`Gesture::Fling`] — on release of a drag whose recent velocity exceeds
//!   [`min_fling_velocity`](GestureConfig::min_fling_velocity); carries a velocity
//!   in **logical pixels per second** for kinetic scrolling.
//!
//! Multi-finger gestures (pinch/rotate) are out of v1 scope; this tracks one
//! contact at a time, which covers a mouse and single-finger touch.
//!
//! [`tap_slop`]: GestureConfig::tap_slop

use fbui_render::geom::Point;

/// Tunables for the recognizer. [`Default`] is a sensible touch-and-mouse set.
#[derive(Debug, Clone, Copy)]
pub struct GestureConfig {
    /// Movement (logical px) a contact may wander and still count as a tap /
    /// not yet a drag.
    pub tap_slop: f32,
    /// Hold time (ms) before a stationary contact becomes a long-press.
    pub long_press_ms: u64,
    /// Minimum recent speed (logical px/s) on release to emit a [`Gesture::Fling`].
    pub min_fling_velocity: f32,
    /// Velocity is clamped to this magnitude (logical px/s) so a jittery last
    /// sample can't launch an absurd fling.
    pub max_fling_velocity: f32,
    /// Only samples within this window (ms) before release feed the velocity
    /// estimate, so a pause-then-release reads as velocity ~0 (no fling).
    pub velocity_window_ms: u64,
}

impl Default for GestureConfig {
    fn default() -> Self {
        GestureConfig {
            tap_slop: 8.0,
            long_press_ms: 500,
            min_fling_velocity: 120.0,
            max_fling_velocity: 6000.0,
            velocity_window_ms: 100,
        }
    }
}

/// A recognized gesture. Positions are in the same logical space the caller fed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Gesture {
    /// A quick press-and-release in place.
    Tap { pos: Point },
    /// A contact held in place past the long-press timeout.
    LongPress { pos: Point },
    /// A drag started (the contact crossed the slop). `pos` is where it began.
    DragBegin { pos: Point },
    /// The dragging contact moved. `delta` is the movement since the last update.
    DragUpdate { pos: Point, delta: Point },
    /// The drag ended (contact lifted) without enough speed to fling.
    DragEnd { pos: Point },
    /// A drag was released with momentum. `velocity` is logical px/s.
    Fling { pos: Point, velocity: Point },
}

/// Internal lifecycle of the single tracked contact.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    /// No contact down.
    Idle,
    /// Contact down, hasn't moved past slop or timed out yet (tap-or-long-press).
    Pending,
    /// Contact has crossed the slop and is dragging.
    Dragging,
    /// Long-press already fired; swallow the rest of this contact.
    LongPressed,
}

/// A tiny ring of recent samples for velocity estimation.
#[derive(Debug, Clone, Copy)]
struct Sample {
    t: u64,
    pos: Point,
}

/// The single-contact gesture state machine. Drive it with
/// [`pointer_down`](Self::pointer_down) / [`pointer_move`](Self::pointer_move) /
/// [`pointer_up`](Self::pointer_up), poll it for long-press, and read the
/// [`Gesture`]s each call returns.
pub struct GestureRecognizer {
    cfg: GestureConfig,
    phase: Phase,
    start: Point,
    down_t: u64,
    /// Last position seen, for per-update deltas.
    last: Point,
    /// Recent samples (newest last) within the velocity window.
    trail: Vec<Sample>,
}

impl GestureRecognizer {
    pub fn new(cfg: GestureConfig) -> Self {
        GestureRecognizer {
            cfg,
            phase: Phase::Idle,
            start: Point::new(0.0, 0.0),
            down_t: 0,
            last: Point::new(0.0, 0.0),
            trail: Vec::with_capacity(8),
        }
    }

    /// Whether a contact is currently down (between down and up).
    pub fn is_active(&self) -> bool {
        self.phase != Phase::Idle
    }

    /// A new contact went down at `pos` at time `now_ms`.
    pub fn pointer_down(&mut self, now_ms: u64, pos: Point) -> Vec<Gesture> {
        self.phase = Phase::Pending;
        self.start = pos;
        self.last = pos;
        self.down_t = now_ms;
        self.trail.clear();
        self.trail.push(Sample { t: now_ms, pos });
        Vec::new()
    }

    /// The active contact moved to `pos` at time `now_ms`. A no-op when idle
    /// (e.g. plain mouse hover), so the caller can feed every motion event.
    pub fn pointer_move(&mut self, now_ms: u64, pos: Point) -> Vec<Gesture> {
        let mut out = Vec::new();
        match self.phase {
            Phase::Idle | Phase::LongPressed => return out,
            Phase::Pending => {
                self.push_sample(now_ms, pos);
                if dist(self.start, pos) > self.cfg.tap_slop {
                    self.phase = Phase::Dragging;
                    out.push(Gesture::DragBegin { pos: self.start });
                    out.push(Gesture::DragUpdate {
                        pos,
                        delta: sub(pos, self.last),
                    });
                    self.last = pos;
                }
            }
            Phase::Dragging => {
                self.push_sample(now_ms, pos);
                out.push(Gesture::DragUpdate {
                    pos,
                    delta: sub(pos, self.last),
                });
                self.last = pos;
            }
        }
        out
    }

    /// The active contact lifted at `pos` at time `now_ms`.
    pub fn pointer_up(&mut self, now_ms: u64, pos: Point) -> Vec<Gesture> {
        let mut out = Vec::new();
        let phase = std::mem::replace(&mut self.phase, Phase::Idle);
        match phase {
            Phase::Idle => {}
            Phase::LongPressed => {} // long-press already consumed the contact
            Phase::Pending => {
                // No drag, no timeout: a tap (as long as it didn't wander).
                if dist(self.start, pos) <= self.cfg.tap_slop {
                    out.push(Gesture::Tap { pos });
                }
            }
            Phase::Dragging => {
                self.push_sample(now_ms, pos);
                let v = self.velocity(now_ms);
                out.push(Gesture::DragEnd { pos });
                if mag(v) >= self.cfg.min_fling_velocity {
                    out.push(Gesture::Fling { pos, velocity: v });
                }
            }
        }
        self.trail.clear();
        out
    }

    /// Cancel the in-progress contact (palm rejection, VT switch). Ends a drag
    /// cleanly so consumers can stop, but emits no tap/fling.
    pub fn cancel(&mut self) -> Vec<Gesture> {
        let mut out = Vec::new();
        if self.phase == Phase::Dragging {
            out.push(Gesture::DragEnd { pos: self.last });
        }
        self.phase = Phase::Idle;
        self.trail.clear();
        out
    }

    /// Advance time without a motion event, firing a long-press once the hold
    /// passes the timeout. Call on the frame clock.
    pub fn poll(&mut self, now_ms: u64) -> Vec<Gesture> {
        let mut out = Vec::new();
        if self.phase == Phase::Pending
            && now_ms.saturating_sub(self.down_t) >= self.cfg.long_press_ms
        {
            self.phase = Phase::LongPressed;
            out.push(Gesture::LongPress { pos: self.start });
        }
        out
    }

    fn push_sample(&mut self, now_ms: u64, pos: Point) {
        self.trail.push(Sample { t: now_ms, pos });
        // Drop samples older than the velocity window (keep at least one).
        let cutoff = now_ms.saturating_sub(self.cfg.velocity_window_ms);
        while self.trail.len() > 1 && self.trail[0].t < cutoff {
            self.trail.remove(0);
        }
    }

    /// Mean velocity across the retained sample window, clamped. Logical px/s.
    fn velocity(&self, _now_ms: u64) -> Point {
        if self.trail.len() < 2 {
            return Point::new(0.0, 0.0);
        }
        let first = self.trail[0];
        let last = self.trail[self.trail.len() - 1];
        let dt = last.t.saturating_sub(first.t);
        if dt == 0 {
            return Point::new(0.0, 0.0);
        }
        let dt_s = dt as f32 / 1000.0;
        let vx = (last.pos.x - first.pos.x) / dt_s;
        let vy = (last.pos.y - first.pos.y) / dt_s;
        clamp_mag(Point::new(vx, vy), self.cfg.max_fling_velocity)
    }
}

impl Default for GestureRecognizer {
    fn default() -> Self {
        GestureRecognizer::new(GestureConfig::default())
    }
}

fn sub(a: Point, b: Point) -> Point {
    Point::new(a.x - b.x, a.y - b.y)
}

fn dist(a: Point, b: Point) -> f32 {
    mag(sub(a, b))
}

fn mag(p: Point) -> f32 {
    (p.x * p.x + p.y * p.y).sqrt()
}

fn clamp_mag(p: Point, max: f32) -> Point {
    let m = mag(p);
    if m > max && m > 0.0 {
        let k = max / m;
        Point::new(p.x * k, p.y * k)
    } else {
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f32, y: f32) -> Point {
        Point::new(x, y)
    }

    #[test]
    fn quick_press_release_is_a_tap() {
        let mut g = GestureRecognizer::default();
        assert!(g.pointer_down(0, p(10.0, 10.0)).is_empty());
        let out = g.pointer_up(50, p(12.0, 11.0));
        assert_eq!(out, vec![Gesture::Tap { pos: p(12.0, 11.0) }]);
        assert!(!g.is_active());
    }

    #[test]
    fn wander_past_slop_then_release_is_no_tap() {
        let mut g = GestureRecognizer::default();
        g.pointer_down(0, p(0.0, 0.0));
        // Move well past slop -> becomes a drag, not a tap.
        let mv = g.pointer_move(10, p(40.0, 0.0));
        assert_eq!(mv[0], Gesture::DragBegin { pos: p(0.0, 0.0) });
        let up = g.pointer_up(20, p(40.0, 0.0));
        assert!(up.iter().all(|gst| !matches!(gst, Gesture::Tap { .. })));
        assert!(up.iter().any(|gst| matches!(gst, Gesture::DragEnd { .. })));
    }

    #[test]
    fn hold_in_place_fires_long_press_once() {
        let mut g = GestureRecognizer::default();
        g.pointer_down(0, p(5.0, 5.0));
        assert!(g.poll(100).is_empty()); // too soon
        let out = g.poll(600);
        assert_eq!(out, vec![Gesture::LongPress { pos: p(5.0, 5.0) }]);
        // No second long-press, and the release is swallowed (no tap).
        assert!(g.poll(700).is_empty());
        assert!(g.pointer_up(800, p(5.0, 5.0)).is_empty());
    }

    #[test]
    fn moving_cancels_long_press() {
        let mut g = GestureRecognizer::default();
        g.pointer_down(0, p(0.0, 0.0));
        g.pointer_move(10, p(40.0, 0.0)); // becomes a drag
        assert!(g.poll(600).is_empty()); // dragging, not pending: no long-press
    }

    #[test]
    fn fast_drag_release_emits_fling_with_velocity() {
        let mut g = GestureRecognizer::default();
        g.pointer_down(0, p(0.0, 100.0));
        // Move 200px up over 100ms => 2000 px/s.
        g.pointer_move(50, p(0.0, 50.0));
        g.pointer_move(100, p(0.0, -100.0));
        let up = g.pointer_up(100, p(0.0, -100.0));
        let fling = up
            .iter()
            .find_map(|gst| match gst {
                Gesture::Fling { velocity, .. } => Some(*velocity),
                _ => None,
            })
            .expect("expected a fling");
        assert!(fling.y < -1000.0, "upward fling, got {fling:?}");
    }

    #[test]
    fn slow_drag_release_does_not_fling() {
        let mut g = GestureRecognizer::default();
        g.pointer_down(0, p(0.0, 0.0));
        // 20px over 400ms => 50 px/s, below the 120 px/s threshold. The window
        // also drops the old start sample, so velocity reads ~0.
        g.pointer_move(200, p(10.0, 0.0));
        g.pointer_move(400, p(20.0, 0.0));
        let up = g.pointer_up(400, p(20.0, 0.0));
        assert!(up.iter().all(|gst| !matches!(gst, Gesture::Fling { .. })));
    }

    #[test]
    fn velocity_is_clamped() {
        let cfg = GestureConfig {
            max_fling_velocity: 1000.0,
            ..GestureConfig::default()
        };
        let mut g = GestureRecognizer::new(cfg);
        g.pointer_down(0, p(0.0, 0.0));
        g.pointer_move(1, p(0.0, 500.0)); // 500px in 1ms = 500_000 px/s
        let up = g.pointer_up(1, p(0.0, 500.0));
        if let Some(Gesture::Fling { velocity, .. }) =
            up.iter().find(|g| matches!(g, Gesture::Fling { .. }))
        {
            assert!(mag(*velocity) <= 1000.0 + 1e-3);
        } else {
            panic!("expected a fling");
        }
    }

    #[test]
    fn cancel_ends_drag_without_tap_or_fling() {
        let mut g = GestureRecognizer::default();
        g.pointer_down(0, p(0.0, 0.0));
        g.pointer_move(5, p(40.0, 0.0));
        let out = g.cancel();
        assert!(out.iter().any(|gst| matches!(gst, Gesture::DragEnd { .. })));
        assert!(!g.is_active());
    }

    #[test]
    fn hover_moves_while_idle_are_ignored() {
        let mut g = GestureRecognizer::default();
        // No contact down: moving the mouse must not produce gestures.
        assert!(g.pointer_move(0, p(10.0, 10.0)).is_empty());
        assert!(g.pointer_move(10, p(20.0, 20.0)).is_empty());
    }
}
