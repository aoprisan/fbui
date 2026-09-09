//! The in-process flow executor: run a [`Script`] against a [`Ui`] in a test.
//!
//! ```no_run
//! # use fbui_widgets::{harness, Ui, Theme};
//! # use fbui_render::{Scale, geom::Size};
//! # #[derive(Clone)] enum Msg { Inc }
//! # let mut ui = Ui::<Msg>::new(Size::new(400.0, 300.0), Scale::ONE, Theme::dark());
//! # let mut count = 0;
//! let flow = "fbui-rec 2\ntap #inc\nexpect #count text \"1\"\n";
//! harness::run_text(&mut ui, flow, |msg, ui| match msg {
//!     Msg::Inc => { count += 1; /* update widgets via ui.with(..) */ let _ = ui; }
//! })
//! .expect("the flow passes");
//! ```
//!
//! This is the fastest of the three executors and the one a `cargo test` uses:
//! no display, no runner, no clock. It feeds widget [`Event`]s straight into
//! [`Ui::event`] — the same path `tests/behavior.rs` uses — and runs the
//! caller's `update` for every message the tree emits, exactly as the runner's
//! `App::update` does.
//!
//! ## What it does *not* do
//!
//! There is no gesture recognizer below the `Ui`, so the harness synthesizes
//! [`Event::Tap`] / [`Event::LongPress`] / [`Event::Fling`] directly, the way
//! the behavior tests always have. The runner's executor sends raw contacts
//! and lets the real recognizer classify them. The two are pinned to each
//! other by `flow::tests::synthesized_input_recognizes_as_the_gesture_the_harness_fakes`
//! in the `fbui` crate: it feeds the runner's synthesized input for each
//! gesture step through the real [`GestureRecognizer`](crate::GestureRecognizer)
//! and asserts it produces exactly the gesture the harness fakes. If those
//! ever diverged, a flow passing in `cargo test` would prove nothing about
//! the device.
//!
//! Raw v1 event lines (`@ms …`) are platform-level and cannot be replayed
//! here; a flow containing them is rejected rather than silently skipped.

use std::path::Path;

use fbui_render::geom::Point;

use crate::event::{Event, PointerButton};
use crate::script::{self, Act, Executor, Failure, Script, Snapshot};
use crate::tree::Ui;

/// Frame step the harness advances animations by: 60 Hz, the runner's cadence.
const DT: f32 = 1.0 / 60.0;
/// Bound on a `wait settle`, in frames — the same budget the runner's replay
/// screenshot uses, so a perpetual animation (a `Spinner`) cannot hang a test.
const MAX_SETTLE_FRAMES: u32 = 300;
/// Intermediate moves synthesized for one `drag`.
const DRAG_STEPS: usize = 8;
/// Logical pixels one wheel notch scrolls — the runner's `WHEEL_STEP`.
const WHEEL_STEP: f32 = 48.0;
/// Virtual milliseconds one flow step takes, for `Fling` velocities.
const STEP_MS: f32 = 50.0;

/// What a completed flow produced.
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// Every step that did not hold. Empty means the flow passed.
    pub failures: Vec<Failure>,
    /// Artifacts the flow asked for: `shot`/`tree` paths, in order. The
    /// harness writes tree dumps itself; screenshots need a `Surface`, so it
    /// reports them for the caller to capture with `fbui-testkit`.
    pub shots: Vec<std::path::PathBuf>,
    /// The tree dump at the end of the flow, always captured — it is what a
    /// failure message needs and what a passing run is usually compared on.
    pub tree: String,
}

impl Report {
    pub fn passed(&self) -> bool {
        self.failures.is_empty()
    }

    /// The failure text a test panics with: every failing step, then the tree
    /// as it stood when the flow stopped.
    pub fn failure_text(&self) -> String {
        let mut s = String::new();
        for f in &self.failures {
            s.push_str(&f.to_string());
            s.push('\n');
        }
        s.push_str("\ntree at failure:\n");
        s.push_str(&self.tree);
        s
    }
}

/// Why a flow could not be run at all (as opposed to failing an expectation).
#[derive(Debug, Clone)]
pub enum Error {
    /// The flow text did not parse.
    Parse(script::ParseError),
    /// The flow needs an executor this one is not (raw platform events).
    Unsupported(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Parse(e) => write!(f, "{e}"),
            Error::Unsupported(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for Error {}

/// Parse `text` and run it against `ui`, calling `update` for every message
/// the tree emits (the harness's stand-in for `App::update`).
pub fn run_text<Msg: 'static>(
    ui: &mut Ui<Msg>,
    text: &str,
    update: impl FnMut(Msg, &mut Ui<Msg>),
) -> Result<Report, Error> {
    let script = script::parse(text).map_err(Error::Parse)?;
    run(ui, script, update)
}

/// Run an already-parsed flow. See [`run_text`].
pub fn run<Msg: 'static>(
    ui: &mut Ui<Msg>,
    script: Script,
    mut update: impl FnMut(Msg, &mut Ui<Msg>),
) -> Result<Report, Error> {
    if script.has_raw {
        return Err(Error::Unsupported(
            "this flow contains raw `@ms` event lines, which only the runner \
             can replay (run it with FBUI_REPLAY instead)"
                .into(),
        ));
    }
    let mut h = Harness {
        ui,
        update: &mut update,
        pointer: Point::new(0.0, 0.0),
        report: Report::default(),
    };
    let mut exec = Executor::new(script);
    loop {
        // The lint pass is a tree walk plus a measure per text widget, so it
        // runs only when a step actually asks for it.
        let lints = if exec.wants_lints() {
            let mut l = crate::lint::render(&h.ui.lint());
            if let Some(t) = h.ui.inspect() {
                l.extend(script::ambiguous_refs(exec.script(), &t));
            }
            l
        } else {
            Vec::new()
        };
        // Resolution and assertions see the tree as it is *right now*, so a
        // flow follows the layout rather than a plan made at parse time.
        let tree = h.ui.inspect();
        let Some(act) = exec.advance(Snapshot::new(tree.as_ref()).with_lints(&lints)) else {
            break;
        };
        h.perform(act);
    }
    h.report.failures = exec.failures().to_vec();
    h.report.tree = h.ui.inspect_text();
    Ok(h.report)
}

/// Run a flow and panic with the failing steps and the tree if it fails —
/// the shape a `#[test]` wants.
pub fn assert_flow<Msg: 'static>(
    ui: &mut Ui<Msg>,
    text: &str,
    update: impl FnMut(Msg, &mut Ui<Msg>),
) -> Report {
    let report = run_text(ui, text, update).unwrap_or_else(|e| panic!("flow: {e}"));
    assert!(report.passed(), "{}", report.failure_text());
    report
}

struct Harness<'a, Msg: 'static> {
    ui: &'a mut Ui<Msg>,
    update: &'a mut dyn FnMut(Msg, &mut Ui<Msg>),
    /// Where the pointer is, so a bare `release` lifts in the right place.
    pointer: Point,
    report: Report,
}

impl<Msg: 'static> Harness<'_, Msg> {
    /// Deliver one event and run the messages it produced to completion — the
    /// harness's equivalent of the runner's `dispatch` + `drain_messages`.
    fn send(&mut self, ev: Event) {
        if let Some(p) = ev.pointer_pos() {
            self.pointer = p;
        }
        self.ui.event(ev);
        self.drain();
    }

    fn drain(&mut self) {
        // An update can emit further messages (a keyboard tap routed through
        // `Ui::send_key`), so loop until the queue is empty.
        loop {
            let msgs = self.ui.take_messages();
            if msgs.is_empty() {
                break;
            }
            for m in msgs {
                (self.update)(m, self.ui);
            }
        }
    }

    fn press(&mut self, at: Point) {
        self.send(Event::PointerMove { pos: at });
        self.send(Event::PointerDown {
            pos: at,
            button: PointerButton::Left,
        });
    }

    fn release(&mut self, at: Point) {
        self.send(Event::PointerUp {
            pos: at,
            button: PointerButton::Left,
        });
    }

    fn perform(&mut self, act: Act) {
        match act {
            Act::Tap(at) => {
                self.press(at);
                self.release(at);
                // No recognizer below the `Ui`: synthesize the gesture the
                // runner's recognizer would have produced.
                self.send(Event::Tap { pos: at });
            }
            Act::LongPress(at) => {
                self.press(at);
                self.send(Event::LongPress { pos: at });
                self.release(at);
            }
            Act::Press(at) => self.press(at),
            Act::Move(at) => self.send(Event::PointerMove { pos: at }),
            Act::Release(at) => {
                let at = at.unwrap_or(self.pointer);
                self.release(at);
            }
            Act::Drag { from, dx, dy, fast } => {
                self.press(from);
                for i in 1..=DRAG_STEPS {
                    let t = i as f32 / DRAG_STEPS as f32;
                    self.send(Event::PointerMove {
                        pos: Point::new(from.x + dx * t, from.y + dy * t),
                    });
                }
                let end = Point::new(from.x + dx, from.y + dy);
                self.release(end);
                if fast {
                    // The velocity a recognizer would have measured over the
                    // synthesized moves: total travel across the drag's span.
                    let secs = DRAG_STEPS as f32 * STEP_MS / 1000.0;
                    self.send(Event::Fling {
                        pos: end,
                        velocity_x: dx / secs,
                        velocity_y: dy / secs,
                    });
                }
            }
            Act::Wheel { at, notches } => self.send(Event::Scroll {
                pos: at,
                delta_x: 0.0,
                delta_y: -notches * WHEEL_STEP,
            }),
            Act::Text(text) => {
                for c in text.chars() {
                    self.key(crate::event::Key::Char(c), Default::default());
                }
            }
            Act::Key(spec) => self.key(spec.key, spec.mods),
            Act::WaitSettle => self.settle(),
            Act::WaitMs(ms) => {
                // No wall clock here: advance the animation clock by the same
                // amount of *frame* time, which is what a wait is for.
                let frames = (ms as f32 / 1000.0 / DT).round() as u32;
                for _ in 0..frames.max(1) {
                    if !self.ui.is_animating() {
                        break;
                    }
                    self.ui.animate(DT);
                    self.drain();
                }
            }
            Act::Shot(path) => {
                self.settle();
                self.report.shots.push(path);
            }
            Act::Tree(path) => {
                self.settle();
                let text = self.ui.inspect_text();
                if let Err(e) = std::fs::write(&path, text) {
                    self.report.failures.push(Failure {
                        line: 0,
                        source: format!("tree {}", path.display()),
                        message: format!("could not write the tree dump: {e}"),
                    });
                }
            }
            Act::Raw { .. } => unreachable!("rejected before the run starts"),
        }
    }

    fn key(&mut self, key: crate::event::Key, mods: crate::event::Modifiers) {
        self.send(Event::Key {
            key,
            pressed: true,
            mods,
        });
        self.send(Event::Key {
            key,
            pressed: false,
            mods,
        });
    }

    /// Advance the frame clock until nothing is animating, bounded so a
    /// perpetual animation cannot hang the test.
    fn settle(&mut self) {
        let mut frames = 0;
        while self.ui.is_animating() && frames < MAX_SETTLE_FRAMES {
            self.ui.animate(DT);
            self.drain();
            frames += 1;
        }
    }
}

/// Read a flow from a file and run it. The path is remembered for the failure
/// artifacts a caller may want to write beside it.
pub fn run_file<Msg: 'static>(
    ui: &mut Ui<Msg>,
    path: impl AsRef<Path>,
    update: impl FnMut(Msg, &mut Ui<Msg>),
) -> Result<Report, Error> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path)
        .map_err(|e| Error::Unsupported(format!("{}: {e}", path.display())))?;
    run_text(ui, &text, update)
}
