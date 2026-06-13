//! The on-device runner (feature `platform`): drive a widget [`Ui`] on a real
//! display through the Phase 1 event loop.
//!
//! This is the glue that turns the headless toolkit into an app: it translates
//! platform `InputEvent`s (physical pixels, raw keysyms) into widget [`Event`]s
//! (logical pixels, semantic keys), feeds them to the tree, runs `App::update`
//! for every emitted message, and presents the damaged surface each frame. It is
//! the only place that knows both halves of the stack.

use std::time::Instant;

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

/// How far one wheel notch scrolls, in logical pixels.
const WHEEL_STEP: f32 = 48.0;

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
}

impl<M> Clone for Proxy<M> {
    fn clone(&self) -> Self {
        Proxy {
            tx: self.tx.clone(),
            waker: self.waker.clone(),
        }
    }
}

impl<M> Proxy<M> {
    /// Queue `msg` for [`App::update`] and wake the loop to process it. Returns
    /// `false` if the app has already exited (the loop is gone).
    pub fn send(&self, msg: M) -> bool {
        if self.tx.send(msg).is_err() {
            return false;
        }
        self.waker.wake();
        true
    }
}

/// The application a [`run`] call drives. Build the tree once, then handle the
/// messages widgets emit.
pub trait App: 'static {
    /// The message type widgets in this app emit.
    type Message: Clone + 'static;

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

/// Bring up the platform and run `app` until it exits (Esc, or a fatal error).
pub fn run<A: App>(mut app: A) -> fbui_platform::Result<()> {
    let platform = Platform::new(&PlatformConfig::default())?;
    let phys = platform.info().size;
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
}

impl<A: App> Runner<A> {
    /// Cursor in logical coordinates.
    fn cursor_logical(&self) -> Point {
        Point::new(
            self.cursor.0 / self.scale.factor(),
            self.cursor.1 / self.scale.factor(),
        )
    }

    /// Milliseconds since the run started, for the gesture recognizer's clock.
    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
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
        let msgs = self.ui.take_messages();
        for m in msgs {
            self.app.update(m, &mut self.ui);
        }
    }
}

impl<A: App> PlatformHandler for Runner<A> {
    fn on_input(&mut self, event: InputEvent) -> Flow {
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
        };
        self.app.on_start(proxy);
    }

    fn on_wake(&mut self) -> Flow {
        // Drain everything a background thread queued (wakes coalesce), running
        // each message through the app exactly like a widget-emitted one.
        while let Ok(msg) = self.rx.try_recv() {
            self.app.update(msg, &mut self.ui);
        }
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
            let msgs = self.ui.take_messages();
            for m in msgs {
                self.app.update(m, &mut self.ui);
            }
        }

        if self.ui.needs_paint() {
            Flow::Redraw
        } else {
            Flow::Continue
        }
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
