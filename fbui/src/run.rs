//! The on-device runner (feature `platform`): drive a widget [`Ui`] on a real
//! display through the Phase 1 event loop.
//!
//! This is the glue that turns the headless toolkit into an app: it translates
//! platform `InputEvent`s (physical pixels, raw keysyms) into widget [`Event`]s
//! (logical pixels, semantic keys), feeds them to the tree, runs `App::update`
//! for every emitted message, and presents the damaged surface each frame. It is
//! the only place that knows both halves of the stack.

use std::time::Instant;

use fbui_platform::{
    keysym, Button, Flow, Frame, InputEvent, KeyState, Keysym, Modifiers as PMods, Platform,
    PlatformConfig, PlatformHandler, Point as PPoint, Rect as PRect,
};
use fbui_render::geom::{Point, Size};
use fbui_render::{Scale, Surface};
use fbui_widgets::event::{Event, Key, Modifiers, PointerButton};
use fbui_widgets::gesture::{Gesture, GestureRecognizer};
use fbui_widgets::{Theme, Ui};

/// How far one wheel notch scrolls, in logical pixels.
const WHEEL_STEP: f32 = 48.0;

/// The application a [`run`] call drives. Build the tree once, then handle the
/// messages widgets emit.
pub trait App: 'static {
    /// The message type widgets in this app emit.
    type Message: Clone + 'static;

    /// Populate the tree. Called once, before the first frame.
    fn build(&mut self, ui: &mut Ui<Self::Message>);

    /// Handle one message: mutate application state and the widgets (via
    /// [`Ui::with`](fbui_widgets::Ui::with)).
    fn update(&mut self, msg: Self::Message, ui: &mut Ui<Self::Message>);

    /// The theme to start with. Default: dark.
    fn theme(&self) -> Theme {
        Theme::dark()
    }
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
    let mut ui = Ui::<A::Message>::new(logical, scale, app.theme());
    app.build(&mut ui);

    let now = Instant::now();
    let mut runner = Runner {
        app,
        ui,
        surface,
        logical,
        scale,
        phys_w: phys.w as f32,
        phys_h: phys.h as f32,
        cursor: (phys.w as f32 / 2.0, phys.h as f32 / 2.0),
        gestures: GestureRecognizer::default(),
        start: now,
        last_tick: now,
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
    /// Recognizes taps/long-press/fling from the primary pointer/touch, so mouse
    /// and touch get the same higher-level gestures.
    gestures: GestureRecognizer,
    /// Run start, the zero point for gesture timestamps.
    start: Instant,
    /// Last `tick` time, for the animation `dt`.
    last_tick: Instant,
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

        if self.ui.needs_paint() {
            Flow::Redraw
        } else {
            Flow::Continue
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>) -> Vec<PRect> {
        let Runner { ui, surface, .. } = self;
        ui.paint(surface);
        crate::span!("present");
        surface.copy_into_frame(frame)
    }

    fn on_session(&mut self, active: bool) {
        if active {
            // Back buffers hold unknown contents after a VT switch: full repaint.
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
