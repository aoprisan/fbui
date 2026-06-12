//! Shared kinetic-scroll momentum (Phase 4).
//!
//! A one-dimensional velocity that decays exponentially over time — the physics
//! behind "flick to coast" scrolling. A flung [`ScrollView`](crate::widgets::ScrollView)
//! or [`List`](crate::widgets::List) seeds it with the release velocity, then
//! steps it once per frame in [`Widget::animate`](crate::Widget::animate) until it
//! settles. Kept separate so the two widgets share identical feel and one place
//! to tune.

/// Exponential decay rate (1/s): higher stops sooner. Tuned for a natural coast
/// of roughly half a second from a brisk flick.
const DECAY_PER_S: f32 = 5.5;

/// Below this speed (logical px/s) the coast is imperceptible; snap to rest so we
/// stop ticking the frame clock.
const STOP_BELOW: f32 = 18.0;

/// A decaying scroll velocity.
#[derive(Debug, Clone, Copy, Default)]
pub struct Kinetic {
    vel: f32,
}

impl Kinetic {
    pub fn new() -> Self {
        Kinetic { vel: 0.0 }
    }

    /// Seed the coast with a release velocity (logical px/s of scroll offset).
    pub fn start(&mut self, vel: f32) {
        self.vel = vel;
    }

    /// Halt immediately (a new touch-down, or hitting a scroll bound).
    pub fn stop(&mut self) {
        self.vel = 0.0;
    }

    /// Whether there's still momentum to apply.
    pub fn is_running(&self) -> bool {
        self.vel != 0.0
    }

    /// Advance by `dt` seconds, returning the offset delta to apply this frame
    /// (`0.0` once at rest). Decays the velocity and snaps to rest below the
    /// stop threshold.
    pub fn step(&mut self, dt: f32) -> f32 {
        if self.vel == 0.0 || dt <= 0.0 {
            return 0.0;
        }
        let dy = self.vel * dt;
        self.vel *= (-DECAY_PER_S * dt).exp();
        if self.vel.abs() < STOP_BELOW {
            self.vel = 0.0;
        }
        dy
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decays_to_rest() {
        let mut k = Kinetic::new();
        k.start(2000.0);
        assert!(k.is_running());
        let mut total = 0.0;
        // At ~60fps it should settle within a couple of seconds.
        for _ in 0..240 {
            total += k.step(1.0 / 60.0);
            if !k.is_running() {
                break;
            }
        }
        assert!(!k.is_running(), "momentum settles");
        assert!(total > 0.0, "coasted in the velocity's direction: {total}");
    }

    #[test]
    fn stop_halts_immediately() {
        let mut k = Kinetic::new();
        k.start(1000.0);
        k.stop();
        assert!(!k.is_running());
        assert_eq!(k.step(0.016), 0.0);
    }

    #[test]
    fn negative_velocity_coasts_negative() {
        let mut k = Kinetic::new();
        k.start(-1500.0);
        let dy = k.step(1.0 / 60.0);
        assert!(dy < 0.0);
    }
}
