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
