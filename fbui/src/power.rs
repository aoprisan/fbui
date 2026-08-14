//! Idle power management: dim, then blank, then wake on input.
//!
//! Kiosks and embedded panels care about burn-in and power draw: after a
//! period with no input the screen should dim, later turn off entirely, and
//! wake the moment someone touches it. The policy lives here as a pure,
//! headless state machine ([`IdleTracker`]) driven by the runner's clock and
//! input stream; the actual effects are a sysfs backlight write
//! (`fbui_platform::Backlight`) for dimming and a
//! `Display::set_power` request (DPMS / framebuffer blank) for blanking.
//!
//! Apps opt in by returning an [`IdlePolicy`] from `App::idle_policy`, and
//! operators can override the timings per deployment with `FBUI_IDLE_DIM` /
//! `FBUI_IDLE_BLANK` / `FBUI_IDLE_DIM_LEVEL` (seconds / seconds / percent;
//! `0` or `off` disables a stage). The input that wakes a blanked screen is
//! swallowed — a wake tap must not also press whatever it landed on.

use std::time::{Duration, Instant};

/// When (and how) the screen dims and blanks while idle, plus messages the
/// app wants on the transitions. Returned by `App::idle_policy`; disabled by
/// default.
#[derive(Debug, Clone)]
pub struct IdlePolicy<M> {
    /// Dim the backlight after this much inactivity (`None` = never dim).
    pub dim_after: Option<Duration>,
    /// Turn the panel off after this much inactivity (`None` = never blank).
    pub blank_after: Option<Duration>,
    /// Backlight level while dimmed, percent of maximum (default 20). A
    /// system without a controllable backlight skips dimming; blanking still
    /// works.
    pub dim_percent: u8,
    /// Delivered to `App::update` when the screen first leaves the active
    /// state (dims, or blanks if dimming is off) — switch to a screensaver /
    /// attract screen here.
    pub on_idle: Option<M>,
    /// Delivered to `App::update` when input wakes the screen.
    pub on_wake: Option<M>,
}

impl<M> IdlePolicy<M> {
    /// No idle management at all — the default.
    pub fn disabled() -> Self {
        IdlePolicy {
            dim_after: None,
            blank_after: None,
            dim_percent: 20,
            on_idle: None,
            on_wake: None,
        }
    }

    /// Dim after `secs` seconds of inactivity.
    pub fn dim_after_secs(mut self, secs: f64) -> Self {
        self.dim_after = Some(Duration::from_secs_f64(secs.max(0.0)));
        self
    }

    /// Blank (power off) after `secs` seconds of inactivity.
    pub fn blank_after_secs(mut self, secs: f64) -> Self {
        self.blank_after = Some(Duration::from_secs_f64(secs.max(0.0)));
        self
    }

    /// Backlight percent while dimmed (clamped to 100).
    pub fn dim_percent(mut self, pct: u8) -> Self {
        self.dim_percent = pct.min(100);
        self
    }

    /// Message for `App::update` when the screen goes idle.
    pub fn on_idle(mut self, msg: M) -> Self {
        self.on_idle = Some(msg);
        self
    }

    /// Message for `App::update` when input wakes the screen.
    pub fn on_wake(mut self, msg: M) -> Self {
        self.on_wake = Some(msg);
        self
    }

    /// Whether any stage is configured.
    pub fn enabled(&self) -> bool {
        self.dim_after.is_some() || self.blank_after.is_some()
    }

    /// Apply the operator overrides: `FBUI_IDLE_DIM` / `FBUI_IDLE_BLANK`
    /// (seconds; `0` or `off` disables the stage) and `FBUI_IDLE_DIM_LEVEL`
    /// (percent). Junk values are a hard error, like the other `FBUI_*`
    /// toggles — a kiosk that silently never blanks is a field failure.
    pub fn with_env(mut self) -> Result<Self, String> {
        if let Some(d) = env_duration("FBUI_IDLE_DIM")? {
            self.dim_after = d;
        }
        if let Some(d) = env_duration("FBUI_IDLE_BLANK")? {
            self.blank_after = d;
        }
        match std::env::var("FBUI_IDLE_DIM_LEVEL") {
            Err(_) => {}
            Ok(s) => {
                let pct = s
                    .trim()
                    .parse::<u8>()
                    .ok()
                    .filter(|p| *p <= 100)
                    .ok_or_else(|| format!("FBUI_IDLE_DIM_LEVEL {s:?}: expected 0-100"))?;
                self.dim_percent = pct;
            }
        }
        Ok(self)
    }
}

impl<M> Default for IdlePolicy<M> {
    fn default() -> Self {
        IdlePolicy::disabled()
    }
}

/// `Some(Some(d))` = stage set to `d`, `Some(None)` = stage explicitly
/// disabled, `None` = variable unset (keep the app's policy).
fn env_duration(var: &str) -> Result<Option<Option<Duration>>, String> {
    match std::env::var(var) {
        Err(_) => Ok(None),
        Ok(s) => match s.trim() {
            "0" | "off" | "none" => Ok(Some(None)),
            t => t
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite() && *v > 0.0)
                .map(|v| Some(Some(Duration::from_secs_f64(v))))
                .ok_or_else(|| format!("{var} {s:?}: expected seconds, \"0\", or \"off\"")),
        },
    }
}

/// How idle the screen currently is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Stage {
    Active,
    Dimmed,
    Blanked,
}

/// A stage boundary crossed by [`IdleTracker::poll`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Transition {
    /// Entering [`Stage::Dimmed`] — lower the backlight.
    Dim,
    /// Entering [`Stage::Blanked`] — power the panel off.
    Blank,
}

/// The pure idle clock: feed it activity and time, it reports stage
/// transitions. No I/O, no wall clock of its own — fully unit-testable.
#[derive(Debug)]
pub(crate) struct IdleTracker {
    dim_after: Option<Duration>,
    blank_after: Option<Duration>,
    last_activity: Instant,
    stage: Stage,
}

impl IdleTracker {
    pub fn new(dim_after: Option<Duration>, blank_after: Option<Duration>, now: Instant) -> Self {
        IdleTracker {
            dim_after,
            blank_after,
            last_activity: now,
            stage: Stage::Active,
        }
    }

    pub fn stage(&self) -> Stage {
        self.stage
    }

    /// Note user input at `now`. Returns `true` if this input *woke* the
    /// screen (it was dimmed or blanked) — the caller restores brightness /
    /// power and emits `on_wake`.
    pub fn note_activity(&mut self, now: Instant) -> bool {
        self.last_activity = now;
        std::mem::replace(&mut self.stage, Stage::Active) != Stage::Active
    }

    /// Advance the clock: the next stage boundary crossed at `now`, if any.
    /// Call repeatedly until `None` — a long stall can cross dim *and* blank.
    pub fn poll(&mut self, now: Instant) -> Option<Transition> {
        let idle = now.saturating_duration_since(self.last_activity);
        match self.stage {
            Stage::Active => {
                if let Some(d) = self.dim_after {
                    if idle >= d {
                        self.stage = Stage::Dimmed;
                        return Some(Transition::Dim);
                    }
                }
                if let Some(b) = self.blank_after {
                    if idle >= b {
                        self.stage = Stage::Blanked;
                        return Some(Transition::Blank);
                    }
                }
                None
            }
            Stage::Dimmed => {
                if let Some(b) = self.blank_after {
                    if idle >= b {
                        self.stage = Stage::Blanked;
                        return Some(Transition::Blank);
                    }
                }
                None
            }
            Stage::Blanked => None,
        }
    }

    /// Time until the next stage boundary, to bound the event loop's sleep.
    /// `None` when fully idle (blanked, or nothing configured) — the loop
    /// then just sleeps on its fds.
    pub fn next_deadline(&self, now: Instant) -> Option<Duration> {
        let idle = now.saturating_duration_since(self.last_activity);
        let until = |d: Duration| d.saturating_sub(idle);
        match self.stage {
            Stage::Active => match (self.dim_after, self.blank_after) {
                (Some(d), Some(b)) => Some(until(d.min(b))),
                (Some(d), None) => Some(until(d)),
                (None, Some(b)) => Some(until(b)),
                (None, None) => None,
            },
            Stage::Dimmed => self.blank_after.map(until),
            Stage::Blanked => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn dims_then_blanks_then_wakes() {
        let t0 = Instant::now();
        let mut tr = IdleTracker::new(Some(secs(10)), Some(secs(30)), t0);
        assert_eq!(tr.poll(t0 + secs(5)), None);
        assert_eq!(tr.poll(t0 + secs(10)), Some(Transition::Dim));
        assert_eq!(tr.stage(), Stage::Dimmed);
        assert_eq!(tr.poll(t0 + secs(11)), None, "no repeat while dimmed");
        assert_eq!(tr.poll(t0 + secs(30)), Some(Transition::Blank));
        assert_eq!(tr.poll(t0 + secs(60)), None, "blanked is terminal");

        // Input wakes: stage resets, and the caller learns it was a wake.
        assert!(tr.note_activity(t0 + secs(61)));
        assert_eq!(tr.stage(), Stage::Active);
        // The clock restarted from the wake.
        assert_eq!(tr.poll(t0 + secs(70)), None);
        assert_eq!(tr.poll(t0 + secs(71)), Some(Transition::Dim));
    }

    #[test]
    fn activity_while_active_is_not_a_wake() {
        let t0 = Instant::now();
        let mut tr = IdleTracker::new(Some(secs(10)), None, t0);
        assert!(!tr.note_activity(t0 + secs(1)));
        // …and it pushes the deadline out.
        assert_eq!(tr.poll(t0 + secs(10)), None);
        assert_eq!(tr.poll(t0 + secs(11)), Some(Transition::Dim));
    }

    #[test]
    fn blank_only_policy_skips_dim() {
        let t0 = Instant::now();
        let mut tr = IdleTracker::new(None, Some(secs(20)), t0);
        assert_eq!(tr.poll(t0 + secs(19)), None);
        assert_eq!(tr.poll(t0 + secs(20)), Some(Transition::Blank));
        assert_eq!(tr.stage(), Stage::Blanked);
    }

    #[test]
    fn a_long_stall_crosses_both_stages_in_order() {
        let t0 = Instant::now();
        let mut tr = IdleTracker::new(Some(secs(10)), Some(secs(30)), t0);
        // The loop slept 5 minutes (VT switch, say): both boundaries passed.
        let late = t0 + secs(300);
        assert_eq!(tr.poll(late), Some(Transition::Dim));
        assert_eq!(tr.poll(late), Some(Transition::Blank));
        assert_eq!(tr.poll(late), None);
    }

    #[test]
    fn deadlines_bound_the_sleep() {
        let t0 = Instant::now();
        let tr = IdleTracker::new(Some(secs(10)), Some(secs(30)), t0);
        assert_eq!(tr.next_deadline(t0 + secs(4)), Some(secs(6)));

        let mut tr = IdleTracker::new(Some(secs(10)), Some(secs(30)), t0);
        tr.poll(t0 + secs(10));
        assert_eq!(tr.next_deadline(t0 + secs(10)), Some(secs(20)));
        tr.poll(t0 + secs(30));
        assert_eq!(
            tr.next_deadline(t0 + secs(30)),
            None,
            "blanked: sleep on fds"
        );

        let tr = IdleTracker::new(None, None, t0);
        assert_eq!(tr.next_deadline(t0), None, "disabled: never ticks");
    }

    #[test]
    fn policy_env_overrides() {
        // Serialize env access within this test only; other tests don't read
        // these variables.
        let p: IdlePolicy<()> = IdlePolicy::disabled();
        assert!(!p.enabled());

        std::env::set_var("FBUI_IDLE_DIM", "2.5");
        std::env::set_var("FBUI_IDLE_BLANK", "off");
        std::env::set_var("FBUI_IDLE_DIM_LEVEL", "35");
        let p: IdlePolicy<()> = IdlePolicy::disabled()
            .blank_after_secs(60.0)
            .with_env()
            .unwrap();
        assert_eq!(p.dim_after, Some(Duration::from_secs_f64(2.5)));
        assert_eq!(p.blank_after, None, "explicit off beats the app policy");
        assert_eq!(p.dim_percent, 35);

        std::env::set_var("FBUI_IDLE_DIM", "garbage");
        assert!(IdlePolicy::<()>::disabled().with_env().is_err());
        std::env::remove_var("FBUI_IDLE_DIM");
        std::env::remove_var("FBUI_IDLE_BLANK");
        std::env::remove_var("FBUI_IDLE_DIM_LEVEL");
    }
}
