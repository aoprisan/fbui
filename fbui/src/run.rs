//! The on-device runner (feature `platform`): drive a widget [`Ui`] on a real
//! display through the Phase 1 event loop.
//!
//! This is the glue that turns the headless toolkit into an app: it translates
//! platform `InputEvent`s (physical pixels, raw keysyms) into widget [`Event`]s
//! (logical pixels, semantic keys), feeds them to the tree, runs `App::update`
//! for every emitted message, and presents the damaged surface each frame. It is
//! the only place that knows both halves of the stack.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use std::sync::mpsc::{self, Receiver, Sender};

use fbui_platform::cursor::SoftwareCursor;
use fbui_platform::{
    keysym, Button, Flow, Frame, InputEvent, KeyState, Keysym, Modifiers as PMods, Platform,
    PlatformConfig, PlatformHandler, Point as PPoint, Rect as PRect, Waker,
};
use fbui_render::geom::{IRect, Point, Size};
use fbui_render::{FontContext, Scale, Surface};
use fbui_widgets::event::{Event, Key, Modifiers, PointerButton};
use fbui_widgets::gesture::{Gesture, GestureRecognizer};
use fbui_widgets::{Theme, Ui};

use crate::record::{Recorder, Replayer};
use crate::timer::{Timer, TimerQueue};

/// How far one wheel notch scrolls, in logical pixels.
const WHEEL_STEP: f32 = 48.0;

/// Frame cadence while animating or mid-gesture, and the poll bound then.
const FRAME: Duration = Duration::from_millis(16);

/// A clonable, `Send` handle for delivering messages into the running app from
/// another thread — a worker computing results, an IPC reader, a progress feed.
///
/// [`send`](Proxy::send) queues a message and wakes the event loop; the runner
/// delivers it to [`App::update`] on the UI thread, exactly like a message a
/// widget emitted. This is how a long-running background task (off the UI
/// thread, so the UI never blocks) reports back. Obtain one from
/// [`App::on_start`].
pub struct Proxy<M> {
    tx: Sender<M>,
    waker: Waker,
    timers: TimerQueue<M>,
}

impl<M> Clone for Proxy<M> {
    fn clone(&self) -> Self {
        Proxy {
            tx: self.tx.clone(),
            waker: self.waker.clone(),
            timers: self.timers.clone(),
        }
    }
}

impl<M: Send + 'static> Proxy<M> {
    /// Queue `msg` for [`App::update`] and wake the loop to process it. Returns
    /// `false` if the app has already exited (the loop is gone).
    pub fn send(&self, msg: M) -> bool {
        if self.tx.send(msg).is_err() {
            return false;
        }
        self.waker.wake();
        true
    }

    /// Deliver `msg` to [`App::update`] once, `delay` from now — a toast that
    /// dismisses itself, a debounce, a one-off poll. The loop stays blocked in
    /// `poll` until the deadline (no ticking); accuracy is poll-timeout
    /// accuracy (about a millisecond — plenty for UI). The returned [`Timer`]
    /// cancels it; dropping the handle just detaches (the message still
    /// arrives). Works from any thread.
    pub fn send_after(&self, delay: Duration, msg: M) -> Timer {
        let t = self.timers.schedule(Instant::now() + delay, None, msg);
        self.waker.wake(); // re-evaluate the loop's sleep with the new deadline
        t
    }

    /// Deliver a clone of `msg` to [`App::update`] every `period`, starting one
    /// `period` from now, until the returned [`Timer`] is cancelled. Repeats
    /// are **fixed-delay**: each next delivery is scheduled `period` after the
    /// previous one ran, so a stalled loop catches up with one message, never
    /// a burst.
    pub fn send_every(&self, period: Duration, msg: M) -> Timer
    where
        M: Clone,
    {
        let t = self
            .timers
            .schedule(Instant::now() + period, Some(period), msg);
        self.waker.wake();
        t
    }
}

/// The application a [`run`] call drives. Build the tree once, then handle the
/// messages widgets emit.
pub trait App: 'static {
    /// The message type widgets in this app emit. (`Send` so a [`Proxy`] —
    /// and the timers behind [`Proxy::send_after`] — can carry messages
    /// across threads.)
    type Message: Clone + Send + 'static;

    /// Populate the tree. Called once, before the first frame.
    fn build(&mut self, ui: &mut Ui<Self::Message>);

    /// Called once, before the first frame, with a [`Proxy`] for delivering
    /// messages from background threads. Spawn workers here (an IPC reader, a
    /// progress poller) and hand them a clone. Default: no background work.
    fn on_start(&mut self, proxy: Proxy<Self::Message>) {
        let _ = proxy;
    }

    /// Handle one message: mutate application state and the widgets (via
    /// [`Ui::with`](fbui_widgets::Ui::with)).
    fn update(&mut self, msg: Self::Message, ui: &mut Ui<Self::Message>);

    /// The theme to start with. Default: dark.
    fn theme(&self) -> Theme {
        Theme::dark()
    }

    /// Fonts to render text with, as TTF/OTF bytes — typically
    /// `vec![include_bytes!("MyFont.ttf").to_vec()]`. Bundling your font here
    /// keeps text host-independent, which a boot image or kiosk needs.
    ///
    /// Default: empty. When empty, the runner uses the compiled-in default font
    /// if the `bundled-font` feature is on, and otherwise loads no fonts (text
    /// won't render until you supply some).
    fn fonts(&self) -> Vec<Vec<u8>> {
        Vec::new()
    }
}

/// The font context for an app that returned no fonts: the compiled-in default
/// under `bundled-font`, or an empty database otherwise.
#[cfg(feature = "bundled-font")]
fn default_font_context() -> FontContext {
    FontContext::with_default_font()
}
#[cfg(not(feature = "bundled-font"))]
fn default_font_context() -> FontContext {
    FontContext::new()
}

/// What happens when playback runs out of events (`FBUI_REPLAY_EXIT`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReplayEnd {
    /// Unset, no shot: the recording speaks for itself — a replayed Esc exits
    /// exactly as it did live, and a recording without one leaves the app
    /// running.
    AsRecorded,
    /// Explicit `0`/`false`: swallow the recording's quit and keep the app
    /// alive (interactive after playback).
    Stay,
    /// Explicit truthy, or defaulted by a shot: unattended run — swallow the
    /// recording's quit, settle, capture, exit.
    Exit,
}

/// Everything the runner tracks while a recording is being played back.
struct ReplayState {
    player: Replayer,
    /// PNG of the end state, written after the last event has settled.
    shot: Option<PathBuf>,
    /// What to do when playback finishes.
    end: ReplayEnd,
    /// Frames still to render after the last event, so the shot captures the
    /// settled UI. `None` until playback finishes (and animations stop).
    finish_frames: Option<u8>,
}

/// Build the recorder and replayer from `FBUI_RECORD` / `FBUI_REPLAY` (see
/// `docs/record-replay.md`). A requested-but-broken recording is a hard error:
/// silently running unrecorded (or replaying nothing) is worse than stopping.
fn record_replay_from_env(
    phys: fbui_platform::Size,
) -> fbui_platform::Result<(Option<Recorder>, Option<ReplayState>)> {
    let io_err = |what: String, e: std::io::Error| fbui_platform::Error::Io { what, source: e };

    let recorder = match std::env::var_os("FBUI_RECORD") {
        Some(path) => {
            let path = PathBuf::from(path);
            let r = Recorder::create(&path, (phys.w, phys.h))
                .map_err(|e| io_err(format!("FBUI_RECORD {}", path.display()), e))?;
            eprintln!("fbui: recording input to {}", path.display());
            Some(r)
        }
        None => None,
    };

    let replay = match std::env::var_os("FBUI_REPLAY") {
        Some(path) => {
            let path = PathBuf::from(path);
            let speed = match std::env::var("FBUI_REPLAY_SPEED").ok().as_deref() {
                None => 1.0,
                Some("max") => f64::INFINITY,
                Some(s) => s.parse::<f64>().ok().filter(|v| *v > 0.0).ok_or_else(|| {
                    io_err(
                        format!("FBUI_REPLAY_SPEED {s:?}"),
                        std::io::Error::other("expected a positive number or \"max\""),
                    )
                })?,
            };
            let player = Replayer::load(&path, speed)
                .map_err(|e| io_err(format!("FBUI_REPLAY {}", path.display()), e))?;
            if let Some((w, h)) = player.recorded_size {
                if (w, h) != (phys.w, phys.h) {
                    eprintln!(
                        "fbui: replay: recorded on {w}x{h}, display is {}x{} — \
                         coordinates may land on different widgets",
                        phys.w, phys.h
                    );
                }
            }
            let shot = std::env::var_os("FBUI_REPLAY_SHOT").map(PathBuf::from);
            let end = match std::env::var("FBUI_REPLAY_EXIT").ok().as_deref() {
                Some("0") | Some("false") => ReplayEnd::Stay,
                Some(_) => ReplayEnd::Exit,
                // A shot implies an unattended run; default to exiting then.
                None if shot.is_some() => ReplayEnd::Exit,
                None => ReplayEnd::AsRecorded,
            };
            eprintln!("fbui: replaying input from {}", path.display());
            Some(ReplayState {
                player,
                shot,
                end,
                finish_frames: None,
            })
        }
        None => None,
    };

    Ok((recorder, replay))
}

/// Bring up the platform and run `app` until it exits (Esc, or a fatal error).
pub fn run<A: App>(mut app: A) -> fbui_platform::Result<()> {
    let platform = Platform::new(&PlatformConfig::default())?;
    let phys = platform.info().size;
    let (recorder, replay) = record_replay_from_env(phys)?;
    let scale = Scale::ONE;
    let logical = Size::new(
        phys.w as f32 / scale.factor(),
        phys.h as f32 / scale.factor(),
    );

    let mut surface = Surface::new(phys.w, phys.h, scale);
    // 16-bit panels band badly on gradients; dither the copy-out for them.
    if platform.info().format == fbui_platform::PixelFormat::Rgb565 {
        surface.set_dither(true);
    }
    let fonts = app.fonts();
    let font_ctx = if fonts.is_empty() {
        default_font_context()
    } else {
        FontContext::with_fonts(fonts)
    };
    let mut ui = Ui::<A::Message>::with_fonts(logical, scale, app.theme(), font_ctx);
    app.build(&mut ui);

    let now = Instant::now();
    // Background threads (spawned from `App::on_start`) deliver messages here; the
    // runner drains them in `on_wake`. The `Waker` half arrives via `on_start`.
    let (tx, rx) = mpsc::channel();
    let mut runner = Runner {
        app,
        ui,
        surface,
        logical,
        scale,
        phys_w: phys.w as f32,
        phys_h: phys.h as f32,
        cursor: (phys.w as f32 / 2.0, phys.h as f32 / 2.0),
        cursor_sprite: SoftwareCursor::new(phys),
        cursor_dirty: true,
        gestures: GestureRecognizer::default(),
        start: now,
        last_tick: now,
        tx,
        rx,
        timers: TimerQueue::new(),
        recorder,
        replay,
        replay_now_ms: None,
    };
    platform.run(&mut runner)
}

struct Runner<A: App> {
    app: A,
    ui: Ui<A::Message>,
    surface: Surface,
    logical: Size,
    scale: Scale,
    phys_w: f32,
    phys_h: f32,
    /// Pointer position in physical pixels (the platform tracks none itself).
    cursor: (f32, f32),
    /// The arrow sprite composited over the frame; its position mirrors
    /// [`cursor`](Self::cursor) each render so the pointer is actually visible.
    cursor_sprite: SoftwareCursor,
    /// The pointer moved since the last present, so the frame must be redrawn to
    /// shift the arrow even when no widget changed.
    cursor_dirty: bool,
    /// Recognizes taps/long-press/fling from the primary pointer/touch, so mouse
    /// and touch get the same higher-level gestures.
    gestures: GestureRecognizer,
    /// Run start, the zero point for gesture timestamps.
    start: Instant,
    /// Last `tick` time, for the animation `dt`.
    last_tick: Instant,
    /// Cloned into each [`Proxy`] so background threads can post messages.
    tx: Sender<A::Message>,
    /// Drained in [`on_wake`](PlatformHandler::on_wake) for `App::update`.
    rx: Receiver<A::Message>,
    /// Deadlines armed via [`Proxy::send_after`] / [`Proxy::send_every`];
    /// serviced in `tick`/`on_wake`, and its earliest deadline bounds the
    /// loop's sleep via [`next_timeout`](PlatformHandler::next_timeout).
    timers: TimerQueue<A::Message>,
    /// `FBUI_RECORD`: every live input event is appended here as it arrives.
    recorder: Option<Recorder>,
    /// `FBUI_REPLAY`: a recording being fed back through
    /// [`handle_input`](Self::handle_input) on the (speed-scaled) clock.
    replay: Option<ReplayState>,
    /// While replaying, [`now_ms`](Self::now_ms) reports the *recording's*
    /// timeline (the timestamp of the last delivered event) instead of the
    /// wall clock, so time-sensitive gestures — long-press holds, fling
    /// velocities — replay identically at any `FBUI_REPLAY_SPEED`.
    replay_now_ms: Option<u64>,
}

impl<A: App> Runner<A> {
    /// Cursor in logical coordinates.
    fn cursor_logical(&self) -> Point {
        Point::new(
            self.cursor.0 / self.scale.factor(),
            self.cursor.1 / self.scale.factor(),
        )
    }

    /// Milliseconds since the run started, for the gesture recognizer's
    /// clock. During replay this is the recording's own timeline.
    fn now_ms(&self) -> u64 {
        self.replay_now_ms
            .unwrap_or_else(|| self.start.elapsed().as_millis() as u64)
    }

    /// Turn a recognized gesture into a widget event (and start the kinetic
    /// clock for flings). Drag gestures are left to the widgets' raw-pointer
    /// handling, so they aren't re-dispatched here.
    fn apply_gesture(&mut self, g: Gesture) {
        match g {
            Gesture::Tap { pos } => self.dispatch(Event::Tap { pos }),
            Gesture::LongPress { pos } => self.dispatch(Event::LongPress { pos }),
            Gesture::Fling { pos, velocity } => {
                // The scrollable widget under `pos` starts its kinetic coast and
                // calls `request_anim`, which is what keeps the clock alive.
                self.dispatch(Event::Fling {
                    pos,
                    velocity_x: velocity.x,
                    velocity_y: velocity.y,
                });
            }
            Gesture::DragBegin { .. } | Gesture::DragUpdate { .. } | Gesture::DragEnd { .. } => {}
        }
    }

    fn gesture_down(&mut self, pos: Point) {
        let t = self.now_ms();
        for g in self.gestures.pointer_down(t, pos) {
            self.apply_gesture(g);
        }
    }

    fn gesture_move(&mut self, pos: Point) {
        let t = self.now_ms();
        for g in self.gestures.pointer_move(t, pos) {
            self.apply_gesture(g);
        }
    }

    fn gesture_up(&mut self, pos: Point) {
        let t = self.now_ms();
        for g in self.gestures.pointer_up(t, pos) {
            self.apply_gesture(g);
        }
    }

    fn set_cursor(&mut self, p: PPoint) {
        self.cursor = (
            (p.x as f32).clamp(0.0, self.phys_w),
            (p.y as f32).clamp(0.0, self.phys_h),
        );
        self.cursor_dirty = true;
    }

    /// Feed a widget event and run any resulting messages.
    fn dispatch(&mut self, event: Event) {
        self.ui.event(event);
        self.drain_messages();
    }

    /// Deliver every ripe timer message through `App::update`.
    fn service_timers(&mut self) {
        let due = self.timers.take_due(Instant::now());
        if due.is_empty() {
            return;
        }
        for m in due {
            self.app.update(m, &mut self.ui);
        }
        self.drain_messages();
    }

    /// Run `App::update` until the message queue is empty. Loops because an
    /// update can itself queue messages — e.g. routing an on-screen keyboard
    /// tap via [`Ui::send_key`] makes the edited field emit `on_change` — and
    /// those must not sit undelivered until the next input event.
    fn drain_messages(&mut self) {
        loop {
            let msgs = self.ui.take_messages();
            if msgs.is_empty() {
                break;
            }
            for m in msgs {
                self.app.update(m, &mut self.ui);
            }
        }
        self.fulfill_screenshot();
    }

    /// Fulfill a pending [`Ui::request_screenshot`] — but only while the shadow
    /// surface is current. If a repaint is pending, the capture waits for
    /// `render` (which calls this right after painting), so the shot always
    /// includes what the requesting update changed.
    fn fulfill_screenshot(&mut self) {
        if self.ui.needs_paint() {
            return;
        }
        if let Some(path) = self.ui.take_screenshot_request() {
            if let Err(e) = self.surface.write_png(&path) {
                // Diagnostics must not kill the app; stderr is the kiosk log.
                eprintln!("fbui: screenshot to {} failed: {e}", path.display());
            }
        }
    }
}

impl<A: App> Runner<A> {
    /// The one input path: live platform events and replayed events both land
    /// here, so a replay exercises exactly what a user did.
    fn handle_input(&mut self, event: InputEvent) -> Flow {
        crate::span!("input");
        match event {
            InputEvent::Key(k) => {
                if k.keysym == keysym::ESCAPE && k.state == KeyState::Pressed {
                    return Flow::Exit;
                }
                if let Some(key) = map_key(k.keysym, k.utf8.as_deref()) {
                    self.dispatch(Event::Key {
                        key,
                        pressed: k.state.is_down(),
                        mods: map_mods(k.modifiers),
                    });
                }
            }
            InputEvent::PointerMotion { dx, dy } => {
                self.cursor.0 = (self.cursor.0 + dx as f32).clamp(0.0, self.phys_w);
                self.cursor.1 = (self.cursor.1 + dy as f32).clamp(0.0, self.phys_h);
                self.cursor_dirty = true;
                let pos = self.cursor_logical();
                self.dispatch(Event::PointerMove { pos });
                self.gesture_move(pos);
            }
            InputEvent::PointerMotionAbsolute { position } => {
                self.set_cursor(position);
                let pos = self.cursor_logical();
                self.dispatch(Event::PointerMove { pos });
                self.gesture_move(pos);
            }
            InputEvent::PointerButton { button, state } => {
                if let Some(b) = map_button(button) {
                    let pos = self.cursor_logical();
                    if state.is_down() {
                        self.dispatch(Event::PointerDown { pos, button: b });
                    } else {
                        self.dispatch(Event::PointerUp { pos, button: b });
                    }
                    // Only the primary (left) button drives the gesture stream.
                    if b == PointerButton::Left {
                        if state.is_down() {
                            self.gesture_down(pos);
                        } else {
                            self.gesture_up(pos);
                        }
                    }
                }
            }
            InputEvent::PointerAxis { vertical, .. } => {
                let pos = self.cursor_logical();
                // Wheel-away (positive vertical) scrolls toward earlier content.
                self.dispatch(Event::Scroll {
                    pos,
                    delta_x: 0.0,
                    delta_y: -(vertical as f32) * WHEEL_STEP,
                });
            }
            InputEvent::TouchDown { position, .. } => {
                self.set_cursor(position);
                let pos = self.cursor_logical();
                self.dispatch(Event::PointerDown {
                    pos,
                    button: PointerButton::Left,
                });
                self.gesture_down(pos);
            }
            InputEvent::TouchMotion { position, .. } => {
                self.set_cursor(position);
                let pos = self.cursor_logical();
                self.dispatch(Event::PointerMove { pos });
                self.gesture_move(pos);
            }
            InputEvent::TouchUp { .. } => {
                let pos = self.cursor_logical();
                self.dispatch(Event::PointerUp {
                    pos,
                    button: PointerButton::Left,
                });
                self.gesture_up(pos);
            }
            InputEvent::TouchCancel => {
                for g in self.gestures.cancel() {
                    self.apply_gesture(g);
                }
            }
            _ => {}
        }

        if self.ui.needs_paint() || self.cursor_dirty {
            Flow::Redraw
        } else {
            Flow::Continue
        }
    }

    /// Feed the replayer's due events through the normal input path. Returns
    /// the flow the replay wants (redraws while playing, and — once finished,
    /// settled, and screenshotted — an exit if configured).
    fn service_replay(&mut self) -> Flow {
        let Some(mut rs) = self.replay.take() else {
            return Flow::Continue;
        };
        let mut flow = Flow::Continue;
        for (ms, ev) in rs.player.due_events() {
            // Replay the *timeline*, not just the events: advance the gesture
            // clock to this event's recorded time first, so a held long-press
            // fires between a down and an up even at FBUI_REPLAY_SPEED=max.
            self.replay_now_ms = Some(ms);
            for g in self.gestures.poll(ms) {
                self.apply_gesture(g);
            }
            match self.handle_input(ev) {
                // The recording's own quit keystroke ends the run only when
                // the end is "as recorded"; a managed run (Stay / Exit /
                // shot) owns its ending.
                Flow::Exit if rs.end == ReplayEnd::AsRecorded => {
                    self.replay_now_ms = None;
                    return Flow::Exit; // replay state dropped here
                }
                Flow::Exit => {}
                Flow::Redraw => flow = Flow::Redraw,
                Flow::Continue => {}
            }
        }
        if rs.player.finished() {
            match rs.finish_frames {
                // Playback ended: let running animations (kinetic coasts,
                // tweens) settle before capturing anything.
                None if self.ui.is_animating() => flow = Flow::Redraw,
                None => {
                    if let Some(path) = rs.shot.take() {
                        self.ui.request_screenshot(path);
                    }
                    rs.finish_frames = Some(2);
                    flow = Flow::Redraw;
                }
                Some(0) => {
                    // Give the capture a last chance off the current surface,
                    // and be loud if it never happened (a run that exits 0
                    // without its artifact is worse than a noisy one).
                    self.fulfill_screenshot();
                    if let Some(p) = self.ui.take_screenshot_request() {
                        eprintln!(
                            "fbui: replay finished but the screenshot {} was never painted",
                            p.display()
                        );
                    }
                    // A recording that ends mid-contact must not leave a
                    // half-finished gesture armed on the live clock.
                    for g in self.gestures.cancel() {
                        self.apply_gesture(g);
                    }
                    self.replay_now_ms = None;
                    // Exit the unattended run, or hand the app back to the
                    // user (replay state dropped either way).
                    return match rs.end {
                        ReplayEnd::Exit => Flow::Exit,
                        _ => flow,
                    };
                }
                Some(n) => {
                    // The shot is fulfilled by the next paint; ticking it here
                    // too covers frames the display couldn't render.
                    self.fulfill_screenshot();
                    rs.finish_frames = Some(n - 1);
                    flow = Flow::Redraw;
                }
            }
        }
        self.replay = Some(rs);
        flow
    }
}

impl<A: App> PlatformHandler for Runner<A> {
    fn on_input(&mut self, event: InputEvent) -> Flow {
        if let Some(rec) = &mut self.recorder {
            rec.record(&event);
        }
        self.handle_input(event)
    }

    fn render(&mut self, frame: &mut Frame<'_>) -> Vec<PRect> {
        // Mirror the input cursor onto the sprite, then damage the pixels it is
        // leaving and entering so copy-out refreshes them from the clean shadow
        // (the arrow itself lives only in the frame, never the shadow).
        self.cursor_sprite
            .move_absolute(PPoint::new(self.cursor.0 as i32, self.cursor.1 as i32));
        let d = self.cursor_sprite.damage();
        self.surface
            .damage_device_rect(IRect::new(d.x, d.y, d.w, d.h));

        self.ui.paint(&mut self.surface);
        self.fulfill_screenshot();
        crate::span!("present");
        let rects = self.surface.copy_into_frame(frame);
        // Composite the arrow on top of the just-copied UI, into the back buffer.
        self.cursor_sprite.paint(frame);
        self.cursor_dirty = false;
        rects
    }

    fn on_start(&mut self, waker: Waker) {
        // Hand the app a proxy: its own message sender paired with the loop waker.
        let proxy = Proxy {
            tx: self.tx.clone(),
            waker,
            timers: self.timers.clone(),
        };
        self.app.on_start(proxy);
    }

    fn on_wake(&mut self) -> Flow {
        // Drain everything a background thread queued (wakes coalesce), running
        // each message through the app exactly like a widget-emitted one.
        while let Ok(msg) = self.rx.try_recv() {
            self.app.update(msg, &mut self.ui);
        }
        // Updates may have queued widget messages (e.g. via `Ui::send_key`).
        self.drain_messages();
        // A cross-thread `send_after` wakes the loop with nothing queued: the
        // wake exists to re-evaluate `next_timeout` — but a ripe deadline is
        // delivered right away.
        self.service_timers();
        if self.ui.needs_paint() {
            Flow::Redraw
        } else {
            Flow::Continue
        }
    }

    fn on_session(&mut self, active: bool) {
        if active {
            // Back buffers hold unknown contents after a VT switch: full repaint.
            self.cursor_dirty = true;
            self.ui.set_size(self.logical, self.scale);
        }
    }

    fn on_display_changed(&mut self, info: fbui_platform::DisplayInfo) {
        // A hotplug / mode change resized the scanout. Rebuild the surface at the
        // new physical size and re-lay-out the tree at the new logical size.
        let (pw, ph) = (info.size.w, info.size.h);
        self.phys_w = pw as f32;
        self.phys_h = ph as f32;
        self.logical = Size::new(
            pw as f32 / self.scale.factor(),
            ph as f32 / self.scale.factor(),
        );
        self.surface = Surface::new(pw, ph, self.scale);
        if info.format == fbui_platform::PixelFormat::Rgb565 {
            self.surface.set_dither(true);
        }
        self.cursor = (
            self.cursor.0.clamp(0.0, self.phys_w),
            self.cursor.1.clamp(0.0, self.phys_h),
        );
        // The surface was rebuilt at the new size; rebuild the sprite so its
        // clamp bounds match, and force a redraw to repaint the arrow.
        self.cursor_sprite = SoftwareCursor::new(info.size);
        self.cursor_dirty = true;
        self.ui.set_size(self.logical, self.scale);
    }

    fn tick(&mut self) -> Flow {
        crate::span!("tick");
        let now = Instant::now();
        let dt = (now - self.last_tick).as_secs_f32();
        self.last_tick = now;

        // Deliver any replayed events that have come due on the scaled clock.
        let replay_flow = self.service_replay();
        if replay_flow == Flow::Exit {
            return Flow::Exit;
        }

        // Fire a pending long-press if the contact has been held long enough.
        let t = self.now_ms();
        for g in self.gestures.poll(t) {
            self.apply_gesture(g);
        }

        // Advance any running animation (kinetic coast, widget tweens). Clamp dt
        // so a long stall (VT switch) doesn't teleport it. `is_animating` gates
        // the tree walk so an idle UI does no work here.
        if self.ui.is_animating() {
            self.ui.animate(dt.min(0.05));
            self.drain_messages();
        }

        // Deliver any timer deadlines that came due while the loop slept.
        self.service_timers();

        if self.ui.needs_paint() || replay_flow == Flow::Redraw {
            Flow::Redraw
        } else {
            Flow::Continue
        }
    }

    fn next_timeout(&mut self) -> Option<Duration> {
        // While animating or mid-gesture (a pending long-press needs
        // `gestures.poll`) the loop must tick at frame cadence; otherwise it
        // may block until the next timer deadline — or, with none pending,
        // indefinitely (the platform's hotplug backstop still bounds it).
        // This is what makes a truly idle app burn ~0% CPU.
        let frame = self.ui.is_animating() || self.gestures.is_active();
        let mut t = if frame { Some(FRAME) } else { None };
        if let Some(due) = self.timers.next_due() {
            let d = due.saturating_duration_since(Instant::now());
            t = Some(t.map_or(d, |cur| cur.min(d)));
        }
        // A replay in flight must keep the loop turning: until the next event
        // is due, and at frame cadence through the settle frames at the end.
        if let Some(rs) = &self.replay {
            let d = if rs.finish_frames.is_some() {
                Some(FRAME)
            } else {
                rs.player.next_due_in()
            };
            if let Some(d) = d {
                t = Some(t.map_or(d, |cur| cur.min(d)));
            }
        }
        t
    }
}

fn map_button(b: Button) -> Option<PointerButton> {
    match b {
        Button::Left => Some(PointerButton::Left),
        Button::Middle => Some(PointerButton::Middle),
        Button::Right => Some(PointerButton::Right),
        Button::Other(_) => None,
    }
}

fn map_mods(m: PMods) -> Modifiers {
    Modifiers {
        shift: m.contains(PMods::SHIFT),
        ctrl: m.contains(PMods::CTRL),
        alt: m.contains(PMods::ALT),
    }
}

fn map_key(sym: Keysym, text: Option<&str>) -> Option<Key> {
    let named = match sym {
        s if s == keysym::BACKSPACE => Some(Key::Backspace),
        s if s == keysym::TAB => Some(Key::Tab),
        s if s == keysym::RETURN => Some(Key::Enter),
        s if s == keysym::DELETE => Some(Key::Delete),
        s if s == keysym::HOME => Some(Key::Home),
        s if s == keysym::END => Some(Key::End),
        s if s == keysym::LEFT => Some(Key::Left),
        s if s == keysym::RIGHT => Some(Key::Right),
        s if s == keysym::UP => Some(Key::Up),
        s if s == keysym::DOWN => Some(Key::Down),
        _ => None,
    };
    if let Some(k) = named {
        return Some(k);
    }
    match text {
        Some(" ") => Some(Key::Space),
        Some(t) => t.chars().next().filter(|c| !c.is_control()).map(Key::Char),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A `Proxy` is only useful if it can move to a worker thread and be cloned
    // for several; pin that contract so a future field can't silently break it.
    #[test]
    fn proxy_is_send_and_clone() {
        fn assert_send<T: Send>() {}
        fn assert_clone<T: Clone>() {}
        assert_send::<Waker>();
        assert_send::<Proxy<i32>>();
        assert_clone::<Proxy<i32>>();
    }
}
