//! The runner's flow executor (feature `platform`): play an `fbui-rec 2`
//! script through the *real* input path.
//!
//! The sequencing, resolution and assertions all live in
//! [`fbui_widgets::script`]; this module is only the translation from an
//! [`Act`] to platform [`InputEvent`]s and the clock that paces them. That is
//! the whole point of the split: a flow means the same thing here as it does
//! in an in-process test and over the remote console, so a flow that passes
//! headless in CI is evidence about the device.
//!
//! Events enter through `Runner::handle_input` — the same one path live and
//! recorded input use — so gesture recognition runs, `FBUI_RECORD` captures a
//! scripted session, and a scripted Escape exits exactly like a physical one.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use fbui_platform::{keysym, InputEvent, KeyEvent, KeyState, Keysym, Modifiers as PMods, Point};
use fbui_widgets::event::Key;
use fbui_widgets::script::{Act, Executor, Failure, KeySpec, Script, Snapshot};

/// Default gap between flow steps on the replay clock (`FBUI_REPLAY_STEP`).
const DEFAULT_STEP_MS: u64 = 50;
/// Press-to-release of a synthesized tap: fast enough to be a tap, slow
/// enough to be a plausible human one.
const TAP_MS: u64 = 40;
/// A synthesized long-press holds past the recognizer's threshold with room
/// to spare, so `poll` fires it before the release.
const LONG_PRESS_HOLD_MS: u64 = 700;
/// Intermediate moves synthesized for one `drag`.
const DRAG_STEPS: u64 = 8;
/// Move spacing for a fling: a frame apart, so several samples land inside
/// the recognizer's velocity window and the release reads as fast.
const DRAG_FAST_MS: u64 = 16;
/// Move spacing for an ordinary drag. Wider than the recognizer's velocity
/// window (100 ms), and the release waits another gap, so *no* movement
/// sample survives the cutoff and the release reads as a stationary lift —
/// the "pause, then let go" a user does when they don't want a fling. That
/// makes it distance-independent: a slow drag never flings, however far it
/// went.
const DRAG_SLOW_MS: u64 = 150;

/// What the driver wants the runner to do this turn, beyond delivering input.
pub(crate) enum Pending {
    /// Nothing to do right now; come back on the next tick.
    Idle,
    /// Wait for animations to stop (bounded by the caller) before continuing.
    Settle,
    /// Write a settled screenshot / tree dump.
    Shot(PathBuf),
    Tree(PathBuf),
    /// Every step has run.
    Done,
}

/// A flow being played by the runner.
pub(crate) struct FlowDriver {
    exec: Executor,
    /// The flow's own file, so failure artifacts land beside it.
    path: PathBuf,
    /// Wall-clock start and speed multiplier (`FBUI_REPLAY_SPEED`), shared
    /// with the v1 replayer's semantics: `max` makes everything due at once.
    start: Instant,
    speed: f64,
    /// Gap between steps on the replay clock.
    step_ms: u64,
    /// Recording-clock time the next step is scheduled at.
    next_ms: u64,
    /// Events synthesized for the current step, with their clock times.
    pending: VecDeque<(u64, InputEvent)>,
    /// A `wait <n>ms` in progress: the clock time it ends at.
    waiting_until: Option<u64>,
    /// A `wait settle` / artifact step is in progress.
    blocked: Option<Pending>,
}

impl FlowDriver {
    pub(crate) fn new(script: Script, path: PathBuf, speed: f64) -> std::io::Result<Self> {
        let step_ms = match std::env::var("FBUI_REPLAY_STEP") {
            Err(_) => DEFAULT_STEP_MS,
            Ok(s) => s.trim().parse::<u64>().map_err(|_| {
                std::io::Error::other(format!("FBUI_REPLAY_STEP {s:?}: expected milliseconds"))
            })?,
        };
        Ok(FlowDriver {
            exec: Executor::new(script),
            path,
            start: Instant::now(),
            speed,
            step_ms,
            next_ms: 0,
            pending: VecDeque::new(),
            waiting_until: None,
            blocked: None,
        })
    }

    /// The flow file, for the failure artifacts (`<flow>.fail.txt` / `.png`).
    pub(crate) fn fail_artifact(&self, ext: &str) -> PathBuf {
        let mut p = self.path.clone();
        let stem = p
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "flow".into());
        let stem = stem
            .rsplit_once('.')
            .map(|(a, _)| a.to_string())
            .unwrap_or(stem);
        p.set_file_name(format!("{stem}.fail.{ext}"));
        p
    }

    pub(crate) fn failures(&self) -> &[Failure] {
        self.exec.failures()
    }

    pub(crate) fn position(&self) -> (usize, usize) {
        self.exec.position()
    }

    /// Every step has run (or the flow stopped at a failure). Input the last
    /// step queued may still be in flight — see [`has_pending`](Self::has_pending).
    pub(crate) fn is_done(&self) -> bool {
        self.exec.is_done()
    }

    /// Milliseconds of flow time elapsed on the (speed-scaled) clock.
    fn elapsed_ms(&self) -> u64 {
        if self.speed.is_infinite() {
            return u64::MAX;
        }
        (self.start.elapsed().as_secs_f64() * 1000.0 * self.speed) as u64
    }

    /// Events whose clock time has arrived, in order. The runner delivers
    /// each through `handle_input` after advancing the gesture clock to its
    /// timestamp, exactly as it does for a v1 recording.
    pub(crate) fn due_events(&mut self) -> Vec<(u64, InputEvent)> {
        let now = self.elapsed_ms();
        let mut out = Vec::new();
        while let Some((ms, _)) = self.pending.front() {
            if *ms > now {
                break;
            }
            out.push(self.pending.pop_front().expect("front just checked"));
        }
        out
    }

    /// Whether input for the current step is still queued.
    pub(crate) fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// The blocking condition, if the flow is waiting on the runner.
    pub(crate) fn blocked(&self) -> Option<&Pending> {
        self.blocked.as_ref()
    }

    /// The runner finished what `blocked` asked for.
    pub(crate) fn unblock(&mut self) {
        self.blocked = None;
        self.next_ms += self.step_ms;
    }

    /// Advance to the next step, given the tree as it stands now. Returns
    /// what the runner must do; input steps are queued into `pending` and
    /// reported as `Idle`.
    pub(crate) fn advance(
        &mut self,
        tree: Option<&fbui_widgets::InspectNode>,
        lints: &[String],
    ) -> Pending {
        if self.blocked.is_some() || self.has_pending() {
            return Pending::Idle;
        }
        if let Some(until) = self.waiting_until {
            if self.elapsed_ms() < until {
                return Pending::Idle;
            }
            self.waiting_until = None;
            self.next_ms = self.next_ms.max(until);
        }
        // Steps are paced on the replay clock, so long-press holds and fling
        // velocities are the same at every `FBUI_REPLAY_SPEED`.
        if self.elapsed_ms() < self.next_ms {
            return Pending::Idle;
        }
        let Some(act) = self.exec.advance(Snapshot::new(tree).with_lints(lints)) else {
            return Pending::Done;
        };
        let t = self.next_ms;
        match act {
            Act::WaitSettle => {
                self.blocked = Some(Pending::Settle);
                Pending::Settle
            }
            Act::WaitMs(ms) => {
                self.waiting_until = Some(t + ms);
                Pending::Idle
            }
            Act::Shot(p) => {
                self.blocked = Some(Pending::Shot(p.clone()));
                Pending::Shot(p)
            }
            Act::Tree(p) => {
                self.blocked = Some(Pending::Tree(p.clone()));
                Pending::Tree(p)
            }
            other => {
                let end = self.queue(t, other);
                self.next_ms = end + self.step_ms;
                Pending::Idle
            }
        }
    }

    /// Turn one act into timed platform events. Returns the clock time of the
    /// last event queued.
    fn queue(&mut self, t: u64, act: Act) -> u64 {
        let mut at = t;
        let push = |ms: u64, ev: InputEvent, q: &mut VecDeque<(u64, InputEvent)>| {
            q.push_back((ms, ev));
        };
        match act {
            Act::Tap(p) => {
                push(at, motion(p), &mut self.pending);
                push(at, button(true), &mut self.pending);
                at += TAP_MS;
                push(at, button(false), &mut self.pending);
            }
            Act::LongPress(p) => {
                push(at, motion(p), &mut self.pending);
                push(at, button(true), &mut self.pending);
                // The recognizer fires the long press from `gestures.poll` as
                // the clock crosses the threshold; releasing well after it
                // means the press has already been delivered.
                at += LONG_PRESS_HOLD_MS;
                push(at, button(false), &mut self.pending);
            }
            Act::Press(p) => {
                push(at, motion(p), &mut self.pending);
                push(at, button(true), &mut self.pending);
            }
            Act::Move(p) => push(at, motion(p), &mut self.pending),
            Act::Release(p) => {
                if let Some(p) = p {
                    push(at, motion(p), &mut self.pending);
                }
                push(at, button(false), &mut self.pending);
            }
            Act::Drag { from, dx, dy, fast } => {
                let gap = if fast { DRAG_FAST_MS } else { DRAG_SLOW_MS };
                push(at, motion(from), &mut self.pending);
                push(at, button(true), &mut self.pending);
                for i in 1..=DRAG_STEPS {
                    at += gap;
                    let f = i as f32 / DRAG_STEPS as f32;
                    push(
                        at,
                        motion(fbui_render::geom::Point::new(
                            from.x + dx * f,
                            from.y + dy * f,
                        )),
                        &mut self.pending,
                    );
                }
                if !fast {
                    at += gap; // the pause that makes it a lift, not a fling
                }
                push(at, button(false), &mut self.pending);
            }
            Act::Wheel { at: p, notches } => {
                push(at, motion(p), &mut self.pending);
                push(
                    at,
                    InputEvent::PointerAxis {
                        horizontal: 0.0,
                        // The runner negates this into a logical scroll
                        // delta, the same arithmetic the harness does.
                        vertical: notches as f64,
                        source: fbui_platform::AxisSource::Wheel,
                    },
                    &mut self.pending,
                );
            }
            Act::Text(text) => {
                for c in text.chars() {
                    for ev in char_events(c) {
                        push(at, ev, &mut self.pending);
                    }
                    at += 1; // distinct timestamps keep the order legible
                }
            }
            Act::Key(spec) => {
                for ev in key_events(spec) {
                    push(at, ev, &mut self.pending);
                }
            }
            Act::WaitSettle | Act::WaitMs(_) | Act::Shot(_) | Act::Tree(_) => {
                unreachable!("handled by `advance`")
            }
            Act::Raw { at_ms, body } => {
                // A raw v1 line keeps its own timestamp, so a mixed file
                // plays its recorded part exactly as `fbui-rec 1` would.
                if let Some(ev) = crate::record::parse_event(&body) {
                    at = at.max(at_ms);
                    push(at, ev, &mut self.pending);
                } else {
                    eprintln!("fbui: flow: skipping unknown raw event {body:?}");
                }
            }
        }
        at
    }

    /// Wall-clock time until the next queued event, for the loop's sleep.
    pub(crate) fn next_due_in(&self) -> Option<Duration> {
        if self.speed.is_infinite() {
            return Some(Duration::ZERO);
        }
        let next = match self.pending.front() {
            Some((ms, _)) => *ms,
            None if self.blocked.is_some() => return Some(Duration::ZERO),
            None => self.waiting_until.unwrap_or(self.next_ms),
        };
        let due_wall = next as f64 / 1000.0 / self.speed;
        Some(Duration::from_secs_f64(
            (due_wall - self.start.elapsed().as_secs_f64()).max(0.0),
        ))
    }
}

fn motion(p: fbui_render::geom::Point) -> InputEvent {
    InputEvent::PointerMotionAbsolute {
        position: Point::new(p.x.round() as i32, p.y.round() as i32),
    }
}

fn button(down: bool) -> InputEvent {
    InputEvent::PointerButton {
        button: fbui_platform::Button::Left,
        state: if down {
            KeyState::Pressed
        } else {
            KeyState::Released
        },
    }
}

/// A press+release pair for one printable character, shaped exactly like the
/// evdev and terminal parsers produce (text on the keysym *and* `utf8`), so
/// `map_key` in the runner lands on `Key::Char(c)`.
fn char_events(c: char) -> Vec<InputEvent> {
    key_pair(Keysym(c as u32), Some(c.to_string()), PMods::empty())
}

/// The platform events for one `key` step. The mapping is the inverse of the
/// runner's `map_key`, so a flow's `key Enter` is indistinguishable from a
/// physical Enter by the time it reaches a widget.
fn key_events(spec: KeySpec) -> Vec<InputEvent> {
    let mut mods = PMods::empty();
    if spec.mods.shift {
        mods |= PMods::SHIFT;
    }
    if spec.mods.ctrl {
        mods |= PMods::CTRL;
    }
    if spec.mods.alt {
        mods |= PMods::ALT;
    }
    let (sym, text) = match spec.key {
        Key::Enter => (keysym::RETURN, None),
        Key::Tab => (keysym::TAB, None),
        Key::Escape => (keysym::ESCAPE, None),
        Key::Backspace => (keysym::BACKSPACE, None),
        Key::Delete => (keysym::DELETE, None),
        Key::Home => (keysym::HOME, None),
        Key::End => (keysym::END, None),
        Key::Left => (keysym::LEFT, None),
        Key::Right => (keysym::RIGHT, None),
        Key::Up => (keysym::UP, None),
        Key::Down => (keysym::DOWN, None),
        Key::Space => (Keysym(' ' as u32), Some(" ".to_string())),
        Key::Char(c) => (
            Keysym(c as u32),
            // A Ctrl chord carries no printable text — that is exactly what
            // the live keymap reports, and what makes the runner fall back to
            // the character keysym.
            (!spec.mods.ctrl && !spec.mods.alt).then(|| c.to_string()),
        ),
        // PageUp/PageDown/Unknown have no keysym in the runner's table; they
        // would be dropped, so say so rather than sending nothing.
        other => {
            eprintln!("fbui: flow: no platform keysym for {other:?}; skipping");
            return Vec::new();
        }
    };
    key_pair(sym, text, mods)
}

fn key_pair(sym: Keysym, text: Option<String>, mods: PMods) -> Vec<InputEvent> {
    [KeyState::Pressed, KeyState::Released]
        .into_iter()
        .map(|state| {
            InputEvent::Key(KeyEvent {
                code: 0,
                keysym: sym,
                // Text is committed on press only, like the real parsers.
                utf8: (state == KeyState::Pressed).then(|| text.clone()).flatten(),
                state,
                modifiers: mods,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fbui_render::geom::Point as LPoint;

    fn driver(text: &str) -> FlowDriver {
        let script = fbui_widgets::script::parse(text).expect("parses");
        FlowDriver::new(script, PathBuf::from("flows/x.txt"), f64::INFINITY).unwrap()
    }

    /// A tap becomes a move, a press and a release, spaced so the gesture
    /// recognizer sees a tap rather than a long press.
    #[test]
    fn a_tap_expands_into_a_plausible_contact() {
        let mut d = driver("tap @10,20\n");
        assert!(matches!(d.advance(None, &[]), Pending::Idle));
        let evs = d.due_events();
        assert_eq!(evs.len(), 3);
        match &evs[0].1 {
            InputEvent::PointerMotionAbsolute { position } => {
                assert_eq!((position.x, position.y), (10, 20));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(evs[2].0 - evs[1].0, TAP_MS, "press to release");
    }

    /// A long press holds past the recognizer's threshold, so `poll` fires it
    /// before the release arrives.
    #[test]
    fn a_long_press_holds_past_the_threshold() {
        let mut d = driver("long-press @5,5\n");
        d.advance(None, &[]);
        let evs = d.due_events();
        let hold = evs.last().unwrap().0 - evs[1].0;
        assert!(
            hold > fbui_widgets::gesture::GestureConfig::default().long_press_ms,
            "held {hold}ms"
        );
    }

    /// A `fast` drag must be recognized as a fling and an ordinary one must
    /// not — checked against the *real* recognizer rather than against
    /// arithmetic, because that is the property flows depend on.
    #[test]
    fn a_fast_drag_flings_and_an_ordinary_one_does_not() {
        use fbui_widgets::gesture::{Gesture, GestureRecognizer};

        let flings = |flow: &str| {
            let mut d = driver(flow);
            d.advance(None, &[]);
            let mut rec = GestureRecognizer::default();
            let mut saw_fling = false;
            let mut at = LPoint::new(0.0, 0.0);
            for (ms, ev) in d.due_events() {
                let gs = match ev {
                    InputEvent::PointerMotionAbsolute { position } => {
                        at = LPoint::new(position.x as f32, position.y as f32);
                        rec.pointer_move(ms, at)
                    }
                    InputEvent::PointerButton {
                        state: KeyState::Pressed,
                        ..
                    } => rec.pointer_down(ms, at),
                    InputEvent::PointerButton { .. } => rec.pointer_up(ms, at),
                    _ => continue,
                };
                saw_fling |= gs.iter().any(|g| matches!(g, Gesture::Fling { .. }));
            }
            saw_fling
        };
        assert!(flings("drag @50,300 dy=-200 fast\n"), "fast drags fling");
        assert!(!flings("drag @50,300 dy=-200\n"), "ordinary drags do not");
        // Distance-independent: a short slow drag must not fling either.
        assert!(!flings("drag @50,300 dy=-30\n"));
    }

    /// Typing produces one press/release pair per character, with the text on
    /// the press — the shape the evdev and terminal parsers produce.
    #[test]
    fn typing_commits_text_on_the_press_only() {
        let mut d = driver("type \"hi\"\n");
        d.advance(None, &[]);
        let evs = d.due_events();
        assert_eq!(evs.len(), 4);
        let texts: Vec<Option<String>> = evs
            .iter()
            .map(|(_, e)| match e {
                InputEvent::Key(k) => k.utf8.clone(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(texts, vec![Some("h".into()), None, Some("i".into()), None]);
    }

    /// A Ctrl chord arrives with modifiers set and no printable text, which is
    /// what makes the runner map it to `Key::Char` with `ctrl`.
    #[test]
    fn a_ctrl_chord_carries_no_text() {
        let mut d = driver("key Ctrl+c\n");
        d.advance(None, &[]);
        let evs = d.due_events();
        match &evs[0].1 {
            InputEvent::Key(k) => {
                assert!(k.modifiers.contains(PMods::CTRL));
                assert!(k.utf8.is_none());
                assert_eq!(k.keysym, Keysym('c' as u32));
            }
            other => panic!("{other:?}"),
        }
    }

    /// A wheel notch maps to the platform's wheel convention, so the runner's
    /// own negation produces the same logical delta the harness computes.
    #[test]
    fn a_wheel_notch_keeps_the_platform_convention() {
        let mut d = driver("wheel @0,0 -3\n");
        d.advance(None, &[]);
        let evs = d.due_events();
        match &evs[1].1 {
            InputEvent::PointerAxis { vertical, .. } => assert_eq!(*vertical, -3.0),
            other => panic!("{other:?}"),
        }
    }

    /// Waits and artifacts block the driver until the runner says it is done,
    /// so a shot is always of a settled screen.
    #[test]
    fn artifacts_block_until_the_runner_reports_back() {
        let mut d = driver("wait settle\nshot end.png\ntap @1,1\n");
        assert!(matches!(d.advance(None, &[]), Pending::Settle));
        assert!(
            matches!(d.advance(None, &[]), Pending::Idle),
            "still blocked"
        );
        d.unblock();
        assert!(matches!(d.advance(None, &[]), Pending::Shot(_)));
        d.unblock();
        assert!(matches!(d.advance(None, &[]), Pending::Idle));
        assert!(d.has_pending(), "the tap was queued");
    }

    #[test]
    fn failure_artifacts_sit_beside_the_flow() {
        let d = driver("tap @1,1\n");
        assert_eq!(d.fail_artifact("txt"), PathBuf::from("flows/x.fail.txt"));
        assert_eq!(d.fail_artifact("png"), PathBuf::from("flows/x.fail.png"));
    }

    /// A reference that resolves to nothing stops the flow with a failure
    /// rather than acting on the void.
    #[test]
    fn an_unresolvable_reference_stops_the_flow() {
        let mut d = driver("tap #nope\n");
        assert!(matches!(d.advance(None, &[]), Pending::Done));
        assert_eq!(d.failures().len(), 1);
        assert!(d.failures()[0].message.contains("#nope"));
    }

    /// **The parity property of TOOLING.md §3.4.** The harness synthesizes
    /// `Event::Tap` / `LongPress` / `Fling` directly, because there is no
    /// recognizer below a `Ui`. The runner instead sends raw contacts and
    /// lets the *real* recognizer classify them. If those two disagreed, a
    /// flow that passes in `cargo test` would prove nothing about the device.
    ///
    /// So: run each gesture step's synthesized input through the recognizer
    /// and assert it produces exactly the gesture the harness fakes.
    #[test]
    fn synthesized_input_recognizes_as_the_gesture_the_harness_fakes() {
        use fbui_widgets::gesture::{Gesture, GestureRecognizer};

        let recognized = |flow: &str| -> Vec<&'static str> {
            let mut d = driver(flow);
            d.advance(None, &[]);
            let mut rec = GestureRecognizer::default();
            let mut at = LPoint::new(0.0, 0.0);
            let mut out = Vec::new();
            let feed = |gs: Vec<Gesture>, out: &mut Vec<&'static str>| {
                for g in gs {
                    match g {
                        Gesture::Tap { .. } => out.push("tap"),
                        Gesture::LongPress { .. } => out.push("long-press"),
                        Gesture::Fling { .. } => out.push("fling"),
                        _ => {}
                    }
                }
            };
            for (ms, ev) in d.due_events() {
                // The recognizer needs the clock advanced past a hold before
                // the release, exactly as the runner does in `service_replay`.
                feed(rec.poll(ms), &mut out);
                let gs = match ev {
                    InputEvent::PointerMotionAbsolute { position } => {
                        at = LPoint::new(position.x as f32, position.y as f32);
                        rec.pointer_move(ms, at)
                    }
                    InputEvent::PointerButton {
                        state: KeyState::Pressed,
                        ..
                    } => rec.pointer_down(ms, at),
                    InputEvent::PointerButton { .. } => rec.pointer_up(ms, at),
                    _ => continue,
                };
                feed(gs, &mut out);
            }
            out
        };

        assert_eq!(recognized("tap @50,50\n"), vec!["tap"]);
        assert_eq!(recognized("long-press @50,50\n"), vec!["long-press"]);
        assert_eq!(recognized("drag @50,300 dy=-200 fast\n"), vec!["fling"]);
        // An ordinary drag is a drag: no tap, no fling — the widget's own
        // pointer handling moved the content and the lift ends it.
        assert!(recognized("drag @50,300 dy=-200\n").is_empty());
    }

    #[test]
    fn point_acts_need_no_tree() {
        // Sanity: `@x,y` is usable before any tree exists, which is what makes
        // the driver's unit tests above meaningful.
        let mut d = driver("tap @3,4\n");
        d.advance(None, &[]);
        assert_eq!(d.due_events()[0].0, 0);
        let _ = LPoint::new(0.0, 0.0);
    }
}
