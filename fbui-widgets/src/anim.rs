//! Frame-clock animation: easing curves and tweens (Phase 5).
//!
//! A tween is a value that moves from `from` to `to` over a fixed duration,
//! shaped by an [`Easing`] curve, advanced by the frame `dt` through the
//! [`Widget::animate`](crate::Widget::animate) hook Phase 4 introduced. It is
//! **damage-aware by construction**: a widget owning a tween repaints only itself
//! while [`advance`](Tween::advance) reports it's still running, and stops
//! requesting frames the moment it settles — so an idle UI still burns ~0% CPU.
//!
//! Everything here is pure and headless: tweens take a `dt`, never a wall clock,
//! so animations are deterministic and unit-testable.

use fbui_render::Color;

/// An easing curve mapping linear progress `t ∈ [0,1]` to eased progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Easing {
    /// Constant rate.
    Linear,
    /// Accelerate from rest (slow start).
    EaseIn,
    /// Decelerate to rest (slow end).
    EaseOut,
    /// Accelerate then decelerate — the natural default for UI transitions.
    #[default]
    EaseInOut,
}

impl Easing {
    /// Shape linear progress into eased progress. `t` is clamped to `[0,1]`, and
    /// every curve fixes the endpoints (`0 → 0`, `1 → 1`).
    pub fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Easing::Linear => t,
            Easing::EaseIn => t * t,
            Easing::EaseOut => {
                let u = 1.0 - t;
                1.0 - u * u
            }
            Easing::EaseInOut => {
                if t < 0.5 {
                    2.0 * t * t
                } else {
                    let u = 1.0 - t;
                    1.0 - 2.0 * u * u
                }
            }
        }
    }
}

/// A value type a [`Tween`] can interpolate.
pub trait Lerp: Copy {
    /// Linear blend: `t = 0` yields `self`, `t = 1` yields `other`.
    fn lerp(self, other: Self, t: f32) -> Self;
}

impl Lerp for f32 {
    fn lerp(self, other: Self, t: f32) -> Self {
        self + (other - self) * t
    }
}

impl Lerp for Color {
    fn lerp(self, other: Self, t: f32) -> Self {
        let ch = |a: u8, b: u8| (a as f32).lerp(b as f32, t).round().clamp(0.0, 255.0) as u8;
        Color::rgba(
            ch(self.r, other.r),
            ch(self.g, other.g),
            ch(self.b, other.b),
            ch(self.a, other.a),
        )
    }
}

/// A value animating from `from` to `to` over `duration` seconds, eased.
///
/// Drive it from a widget's [`animate`](crate::Widget::animate): call
/// [`advance`](Self::advance) with the frame `dt` and read [`value`](Self::value)
/// when painting. [`retarget`](Self::retarget) restarts toward a new endpoint
/// from the *current* value, so an interrupted toggle animates smoothly rather
/// than snapping.
#[derive(Debug, Clone, Copy)]
pub struct Tween<T> {
    from: T,
    to: T,
    duration: f32,
    elapsed: f32,
    easing: Easing,
}

impl<T: Lerp> Tween<T> {
    /// A tween that animates from `from` to `to` over `duration` seconds,
    /// **starting now**. A non-positive duration makes it instant (already at
    /// `to`).
    pub fn new(from: T, to: T, duration: f32, easing: Easing) -> Self {
        Tween {
            from,
            to,
            duration: duration.max(0.0),
            elapsed: 0.0,
            easing,
        }
    }

    /// A tween already settled at a constant value (no animation in progress);
    /// `duration`/`easing` are remembered for a later [`retarget`](Self::retarget).
    pub fn settled(value: T, duration: f32, easing: Easing) -> Self {
        let mut t = Tween::new(value, value, duration, easing);
        t.elapsed = t.duration; // done
        t
    }

    /// Current eased value.
    pub fn value(&self) -> T {
        self.from.lerp(self.to, self.easing.apply(self.progress()))
    }

    /// Linear progress `∈ [0,1]` (un-eased).
    pub fn progress(&self) -> f32 {
        if self.duration <= 0.0 {
            1.0
        } else {
            (self.elapsed / self.duration).clamp(0.0, 1.0)
        }
    }

    /// Whether the tween has reached its target.
    pub fn is_done(&self) -> bool {
        self.elapsed >= self.duration
    }

    /// Advance by `dt` seconds, returning `true` while still running (so the
    /// caller keeps the frame clock alive) and `false` once settled.
    pub fn advance(&mut self, dt: f32) -> bool {
        if self.is_done() {
            return false;
        }
        self.elapsed = (self.elapsed + dt.max(0.0)).min(self.duration);
        !self.is_done()
    }

    /// Animate toward a new target starting from the *current* value, so an
    /// interruption (e.g. toggling back mid-animation) is smooth. No-op if the
    /// target is already the current endpoint and the tween is settled.
    pub fn retarget(&mut self, to: T) {
        self.from = self.value();
        self.to = to;
        self.elapsed = 0.0;
    }
}

/// A value animating toward a target under **spring physics** — the natural
/// motion for gestural UI, where [`Tween`]'s fixed duration fights the user.
///
/// Where a tween replays a curve, a spring integrates a damped harmonic
/// oscillator: it carries **velocity**, so a target change mid-flight
/// ([`retarget`](Self::retarget)) bends the motion smoothly instead of
/// restarting it, and a fling can be handed off by seeding
/// [`set_velocity`](Self::set_velocity) with the gesture's release speed.
///
/// The feel is described by two designer-facing parameters rather than raw
/// spring constants:
///
/// * [`response`](Self::response) — the undamped oscillation period in seconds;
///   smaller is snappier. Default **0.35 s**.
/// * [`damping_ratio`](Self::damping_ratio) — **1.0** (default) is critically
///   damped: fastest approach with no overshoot. Below 1.0 the spring
///   overshoots and rings; above 1.0 it approaches sluggishly.
///
/// Like [`Tween`] it is pure and frame-clock driven: call
/// [`advance`](Self::advance) with the frame `dt` from
/// [`Widget::animate`](crate::Widget::animate) and read
/// [`value`](Self::value) when painting. Integration is semi-implicit Euler in
/// bounded substeps, so the motion is deterministic for a given `dt` sequence
/// and stable even across a long stalled frame.
#[derive(Debug, Clone, Copy)]
pub struct Spring {
    value: f32,
    velocity: f32,
    target: f32,
    response: f32,
    damping_ratio: f32,
    /// Value-space settle threshold (position; velocity settles at 4× this
    /// per second).
    epsilon: f32,
}

impl Spring {
    /// A spring resting at `value` (target = value, zero velocity), with the
    /// default feel (response 0.35 s, critically damped).
    pub fn new(value: f32) -> Self {
        Spring {
            value,
            velocity: 0.0,
            target: value,
            response: 0.35,
            damping_ratio: 1.0,
            epsilon: 0.1,
        }
    }

    /// Set the undamped period in seconds (snappiness); clamped to ≥ 1 ms.
    pub fn response(mut self, seconds: f32) -> Self {
        self.response = seconds.max(0.001);
        self
    }

    /// Set the damping ratio (1.0 = critical, < 1.0 overshoots); clamped to
    /// > 0 so the spring always settles.
    pub fn damping_ratio(mut self, zeta: f32) -> Self {
        self.damping_ratio = zeta.max(0.01);
        self
    }

    /// Set the value-space settle threshold (default 0.1 — about a tenth of a
    /// logical pixel when animating pixel geometry).
    pub fn epsilon(mut self, eps: f32) -> Self {
        self.epsilon = eps.max(f32::EPSILON);
        self
    }

    /// Current position.
    pub fn value(&self) -> f32 {
        self.value
    }

    /// Current velocity, in value units per second.
    pub fn velocity(&self) -> f32 {
        self.velocity
    }

    /// The target being approached.
    pub fn target(&self) -> f32 {
        self.target
    }

    /// Retarget mid-flight: position **and velocity carry over**, so the
    /// motion bends toward the new target instead of restarting.
    pub fn retarget(&mut self, target: f32) {
        self.target = target;
    }

    /// Seed the velocity (value units/second) — hand-off from a fling gesture.
    pub fn set_velocity(&mut self, v: f32) {
        self.velocity = v;
    }

    /// Jump straight to `value` at rest (no animation).
    pub fn snap_to(&mut self, value: f32) {
        self.value = value;
        self.target = value;
        self.velocity = 0.0;
    }

    /// Whether the spring has settled at its target.
    pub fn is_done(&self) -> bool {
        (self.value - self.target).abs() <= self.epsilon
            && self.velocity.abs() <= self.epsilon * 4.0
    }

    /// Advance by `dt` seconds, returning `true` while still moving (keep the
    /// frame clock alive) and `false` once settled — same contract as
    /// [`Tween::advance`]. On settling the value snaps exactly to the target.
    pub fn advance(&mut self, dt: f32) -> bool {
        if self.is_done() {
            self.snap_to(self.target);
            return false;
        }
        let omega = std::f32::consts::TAU / self.response;
        let stiffness = omega * omega;
        let damping = 2.0 * self.damping_ratio * omega;
        // Substeps bounded by both a frame cap and the spring's own stiffness
        // keep semi-implicit Euler stable for any response and any stalled dt.
        let max_h = (1.0 / 120.0_f32).min(self.response / 24.0);
        let mut remaining = dt.max(0.0);
        while remaining > 0.0 {
            let h = remaining.min(max_h);
            remaining -= h;
            let accel = -stiffness * (self.value - self.target) - damping * self.velocity;
            self.velocity += accel * h;
            self.value += self.velocity * h;
        }
        if self.is_done() {
            self.snap_to(self.target);
            false
        } else {
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easing_fixes_endpoints() {
        for e in [
            Easing::Linear,
            Easing::EaseIn,
            Easing::EaseOut,
            Easing::EaseInOut,
        ] {
            assert!((e.apply(0.0)).abs() < 1e-6, "{e:?} at 0");
            assert!((e.apply(1.0) - 1.0).abs() < 1e-6, "{e:?} at 1");
        }
        // Out of range clamps.
        assert_eq!(Easing::Linear.apply(-1.0), 0.0);
        assert_eq!(Easing::Linear.apply(2.0), 1.0);
    }

    #[test]
    fn easeinout_is_symmetric_about_half() {
        let e = Easing::EaseInOut;
        assert!((e.apply(0.5) - 0.5).abs() < 1e-6);
        // Below the midpoint it lags linear; above, it leads.
        assert!(e.apply(0.25) < 0.25);
        assert!(e.apply(0.75) > 0.75);
    }

    #[test]
    fn f32_lerp_endpoints_and_mid() {
        assert_eq!(2.0_f32.lerp(6.0, 0.0), 2.0);
        assert_eq!(2.0_f32.lerp(6.0, 1.0), 6.0);
        assert_eq!(2.0_f32.lerp(6.0, 0.5), 4.0);
    }

    #[test]
    fn color_lerp_blends_each_channel() {
        let a = Color::rgb(0, 0, 0);
        let b = Color::rgb(255, 100, 50);
        let m = a.lerp(b, 0.5);
        assert_eq!((m.r, m.g, m.b), (128, 50, 25));
    }

    #[test]
    fn tween_runs_then_settles() {
        let mut t = Tween::new(0.0_f32, 10.0, 1.0, Easing::Linear);
        assert!(!t.is_done());
        assert!(t.advance(0.5));
        assert!((t.value() - 5.0).abs() < 1e-4, "halfway: {}", t.value());
        assert!(!t.advance(0.5)); // reaches the end -> no longer running
        assert!(t.is_done());
        assert_eq!(t.value(), 10.0);
        // Advancing a settled tween does nothing and reports not-running.
        assert!(!t.advance(1.0));
    }

    #[test]
    fn zero_duration_is_instant() {
        let t = Tween::new(0.0_f32, 1.0, 0.0, Easing::Linear);
        assert!(t.is_done());
        assert_eq!(t.value(), 1.0);
    }

    /// Advance a spring in fixed steps until it settles (with a generous cap
    /// so a bug can't hang the suite), returning the elapsed simulated time.
    fn settle(s: &mut Spring, dt: f32) -> f32 {
        let mut t = 0.0;
        for _ in 0..100_000 {
            t += dt;
            if !s.advance(dt) {
                return t;
            }
        }
        panic!(
            "spring failed to settle: value {} target {}",
            s.value(),
            s.target()
        );
    }

    #[test]
    fn spring_settles_exactly_on_target() {
        let mut s = Spring::new(0.0);
        s.retarget(100.0);
        settle(&mut s, 1.0 / 60.0);
        assert_eq!(s.value(), 100.0);
        assert_eq!(s.velocity(), 0.0);
        assert!(s.is_done());
        // A settled spring reports not-running without moving.
        assert!(!s.advance(1.0));
    }

    #[test]
    fn critically_damped_never_overshoots() {
        let mut s = Spring::new(0.0);
        s.retarget(100.0);
        while s.advance(1.0 / 60.0) {
            assert!(
                s.value() <= 100.0 + 1e-3,
                "critical damping overshot: {}",
                s.value()
            );
        }
    }

    #[test]
    fn underdamped_overshoots_then_settles() {
        let mut s = Spring::new(0.0).damping_ratio(0.4);
        s.retarget(100.0);
        let mut peak = 0.0f32;
        while s.advance(1.0 / 60.0) {
            peak = peak.max(s.value());
        }
        assert!(peak > 101.0, "underdamped should overshoot, peaked {peak}");
        assert_eq!(s.value(), 100.0);
    }

    #[test]
    fn retarget_preserves_position_and_velocity() {
        let mut s = Spring::new(0.0);
        s.retarget(100.0);
        for _ in 0..6 {
            s.advance(1.0 / 60.0);
        }
        let (v, vel) = (s.value(), s.velocity());
        assert!(vel > 0.0, "moving toward the target");
        s.retarget(-50.0);
        // Nothing snaps at the moment of retarget — the motion bends.
        assert_eq!(s.value(), v);
        assert_eq!(s.velocity(), vel);
        settle(&mut s, 1.0 / 60.0);
        assert_eq!(s.value(), -50.0);
    }

    #[test]
    fn spring_is_deterministic_and_dt_robust() {
        let run = |dt: f32| {
            let mut s = Spring::new(0.0).damping_ratio(0.6);
            s.retarget(50.0);
            settle(&mut s, dt)
        };
        // Same dt twice -> identical duration (pure function of the dt sequence).
        assert_eq!(run(1.0 / 60.0), run(1.0 / 60.0));
        // A huge stalled frame is integrated in substeps, not exploded through:
        // one 10 s step must land settled on the target.
        let mut s = Spring::new(0.0);
        s.retarget(50.0);
        assert!(!s.advance(10.0), "10 s covers the whole motion");
        assert_eq!(s.value(), 50.0);
    }

    #[test]
    fn fling_handoff_moves_before_pulling_back() {
        // Zero displacement but seeded velocity: the spring must first travel
        // with the fling, then come back to its (unchanged) target.
        let mut s = Spring::new(0.0);
        s.set_velocity(800.0);
        let mut peak = 0.0f32;
        while s.advance(1.0 / 60.0) {
            peak = peak.max(s.value());
        }
        assert!(peak > 5.0, "fling velocity carried it away: {peak}");
        assert_eq!(s.value(), 0.0);
    }

    #[test]
    fn retarget_starts_from_current_value() {
        let mut t = Tween::new(0.0_f32, 1.0, 1.0, Easing::Linear);
        t.retarget(1.0);
        t.advance(0.5); // value ~0.5
        t.retarget(0.0); // reverse from 0.5 toward 0
        assert!(
            (t.value() - 0.5).abs() < 1e-4,
            "starts at current: {}",
            t.value()
        );
        t.advance(1.0);
        assert_eq!(t.value(), 0.0);
    }
}
