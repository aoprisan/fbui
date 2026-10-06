//! # fbui-bare — fbui with no operating system
//!
//! The `fbui` crate's runner sits on Linux: DRM/fbdev, evdev, a VT, calloop.
//! This crate is its `no_std + alloc` counterpart for a target with **no OS
//! at all** — a microcontroller with an LCD controller, a Raspberry Pi booted
//! straight into your binary, a UEFI app. Everything above the platform layer
//! is the same code the Linux build runs: [`fbui_widgets::Ui`], layout, focus,
//! gestures, animation, the CPU painter, damage tracking and copy-out.
//!
//! The board supplies three things:
//!
//! * a [`Framebuffer`] — any linear pixel buffer (XRGB8888, ARGB8888 or
//!   RGB565) plus an optional [`flush`](Framebuffer::flush) for panels that
//!   need one (an SPI LCD, a cache clean before DMA scanout);
//! * an input poll — [`Board::poll_input`] returns [`Input`]s from whatever
//!   the board has: a UART, GPIO buttons, a resistive touch controller;
//! * a millisecond clock — [`Board::now_ms`], for animation `dt` and gesture
//!   timing — and a way to sleep, [`Board::wait`] (`wfi`, `wfe`, or a spin).
//!
//! and the app implements [`App`] — the same `build`/`update` shape as the
//! Linux `fbui::App`, minus threads and file I/O. Deferred and repeating
//! messages (a clock, a sensor poll, a timeout) go through [`Timers`], handed
//! to [`App::on_start`]; a panel mounted sideways or upside down is handled
//! with [`Runner::with_rotation`].
//!
//! [`run`] is the whole main loop. It keeps fbui's idle rule: with no damage,
//! no animation and no pending gesture it calls [`Board::wait`] with no
//! deadline, so an idle UI costs nothing but the board's sleep. For tests and
//! for boards with their own loop, [`Runner`] exposes the steps one at a time.
//!
//! ```
//! use fbui_bare::{App, FbInfo, Framebuffer, Input, Runner};
//! use fbui_render::TargetFormat;
//! use fbui_widgets::{Ui, widgets::Label};
//!
//! struct Hello;
//! impl App for Hello {
//!     type Message = ();
//!     fn build(&mut self, ui: &mut Ui<()>) {
//!         ui.set_root(Label::new("hello, bare metal"));
//!     }
//!     fn update(&mut self, _: (), _: &mut Ui<()>) {}
//! }
//!
//! // A RAM framebuffer standing in for the board's scanout memory.
//! struct Ram(Vec<u8>);
//! impl Framebuffer for Ram {
//!     fn info(&self) -> FbInfo {
//!         FbInfo { width: 64, height: 32, stride: 64 * 4, format: TargetFormat::Xrgb8888 }
//!     }
//!     fn pixels(&mut self) -> &mut [u8] { &mut self.0 }
//! }
//!
//! let mut fb = Ram(vec![0; 64 * 32 * 4]);
//! let mut runner = Runner::new(Hello, fb.info(), 1.0);
//! assert!(runner.frame(&mut fb, 0)); // first frame paints everything
//! assert!(!runner.frame(&mut fb, 16)); // then idles: nothing to do
//! ```

#![no_std]

extern crate alloc;

mod timer;

use alloc::vec::Vec;

use fbui_render::geom::{IRect, Point, Size};
use fbui_render::{FontContext, Scale, Surface, TargetFormat};
use fbui_widgets::event::{Event, Key, Modifiers, PointerButton};
use fbui_widgets::gesture::{Gesture, GestureRecognizer};
use fbui_widgets::{Theme, Ui};

pub use fbui_render::Rotation;
pub use timer::{Timer, Timers};

/// Frame period while animating or mid-gesture (~60 Hz).
pub const FRAME_MS: u64 = 16;

/// Upper bound on one animation step, so a long stall (a slow page render, a
/// debugger halt) doesn't teleport an animation to its end.
const MAX_DT: f32 = 0.05;

/// Logical pixels one wheel notch / scroll step moves.
pub const SCROLL_STEP: f32 = 48.0;

/// The shape of the board's scanout memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FbInfo {
    /// Visible width in pixels.
    pub width: u32,
    /// Visible height in pixels.
    pub height: u32,
    /// Bytes per row as the display controller reports it. **Never** assume
    /// `width * bpp` — controllers pad rows (fbui's first invariant).
    pub stride: usize,
    pub format: TargetFormat,
}

/// A linear framebuffer the runner copies damaged spans into.
///
/// The runner writes rows forward only, whole damaged spans at a time, so
/// write-combined or uncached scanout memory is fine to hand out directly.
pub trait Framebuffer {
    fn info(&self) -> FbInfo;
    /// The scanout memory: at least `stride * height` bytes.
    fn pixels(&mut self) -> &mut [u8];
    /// Called after a frame's spans were written, with the rects (device
    /// pixels) that changed. Push them to an SPI panel, clean the data cache
    /// for a DMA engine, flip buffers — or do nothing (the default) when the
    /// controller scans `pixels()` out directly.
    fn flush(&mut self, damage: &[IRect]) {
        let _ = damage;
    }
}

/// Raw input as a board produces it. Positions are **device** pixels; the
/// runner converts to logical coordinates and runs the gesture recognizer,
/// exactly as the Linux runner does for evdev.
#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    /// A key changed state.
    Key {
        key: Key,
        pressed: bool,
        mods: Modifiers,
    },
    /// A press-and-release, for sources that only report presses (a UART, a
    /// keypad): delivered as a pressed then a released `Key`.
    KeyTap(Key),
    /// Primary button / finger went down.
    PointerDown { x: f32, y: f32 },
    /// Pointer or finger moved.
    PointerMove { x: f32, y: f32 },
    /// Primary button / finger lifted.
    PointerUp { x: f32, y: f32 },
    /// Wheel or scroll keys: `notches` > 0 scrolls toward later content.
    Scroll { x: f32, y: f32, notches: f32 },
}

/// What the board provides besides the framebuffer.
pub trait Board {
    /// Next pending input, if any. Must not block.
    fn poll_input(&mut self) -> Option<Input>;
    /// Monotonic milliseconds since boot (any epoch; only differences matter).
    fn now_ms(&self) -> u64;
    /// Sleep until input may be pending or `deadline_ms` passes (`None`: no
    /// deadline — the UI is idle). `wfi` with a timer interrupt armed, `wfe`,
    /// or simply returning (a busy poll) are all correct; returning early is
    /// always allowed.
    fn wait(&mut self, deadline_ms: Option<u64>) {
        let _ = deadline_ms;
    }
}

/// A bare-metal fbui application.
pub trait App {
    /// The message type widgets in this app emit.
    type Message: Clone + 'static;

    /// Populate the tree. Called once, before the first frame.
    fn build(&mut self, ui: &mut Ui<Self::Message>);

    /// Handle one message.
    fn update(&mut self, msg: Self::Message, ui: &mut Ui<Self::Message>);

    /// Called once, right after [`build`](Self::build), with the handle for
    /// deferred and repeating messages — keep it to arm timers later from
    /// `update`. Timers armed here count from the runner's first clock
    /// reading. Default: no timers.
    fn on_start(&mut self, timers: Timers<Self::Message>) {
        let _ = timers;
    }

    /// See a key before the focused widget does — app-wide shortcuts (page
    /// keys in a viewer, a menu key). Return `true` to consume it. Default:
    /// nothing is intercepted.
    fn on_key(&mut self, key: Key, ui: &mut Ui<Self::Message>) -> bool {
        let _ = (key, ui);
        false
    }

    /// The theme to start with. Default: dark.
    fn theme(&self) -> Theme {
        Theme::dark()
    }

    /// Fonts (TTF/OTF bytes). With none, the runner uses the bundled font
    /// under `bundled-font`, and otherwise text lays out empty.
    fn fonts(&self) -> Vec<Vec<u8>> {
        Vec::new()
    }
}

/// The main loop, one step at a time. [`run`] is the usual way to drive it.
pub struct Runner<A: App> {
    app: A,
    ui: Ui<A::Message>,
    surface: Surface,
    gestures: GestureRecognizer,
    timers: Timers<A::Message>,
    scale: f32,
    /// The panel (framebuffer) size in device pixels — what input speaks.
    panel: (u32, u32),
    /// How the UI is turned on the panel; the surface is UI-oriented.
    rotation: Rotation,
    /// `now_ms` of the previous frame, for the animation `dt`.
    last_frame_ms: Option<u64>,
    /// Whether the framebuffer already holds the previous frame (copy only
    /// damage) or must be filled whole (first frame, after `invalidate`).
    fb_current: bool,
}

impl<A: App> Runner<A> {
    /// Build the app's tree for a framebuffer of shape `info` at UI `scale`
    /// (logical size = device size / scale).
    pub fn new(mut app: A, info: FbInfo, scale: f32) -> Self {
        let fonts = app.fonts();
        let fonts = if fonts.is_empty() {
            default_fonts()
        } else {
            FontContext::with_fonts(fonts)
        };
        let sc = Scale::new(scale);
        let size = Size::new(info.width as f32 / scale, info.height as f32 / scale);
        let mut ui = Ui::with_fonts(size, sc, app.theme(), fonts);
        app.build(&mut ui);
        let timers = Timers::new();
        app.on_start(timers.clone());
        let mut runner = Runner {
            app,
            ui,
            surface: Surface::new(info.width, info.height, sc),
            gestures: GestureRecognizer::default(),
            timers,
            scale,
            panel: (info.width, info.height),
            rotation: Rotation::Rot0,
            last_frame_ms: None,
            fb_current: false,
        };
        runner.drain_messages();
        runner
    }

    /// [`set_rotation`](Self::set_rotation), builder-style — for a panel
    /// that is mounted turned:
    ///
    /// ```ignore
    /// Runner::new(app, fb.info(), 1.0)
    ///     .with_rotation(Rotation::Rot90)
    ///     .run(&mut fb, &mut board)
    /// ```
    pub fn with_rotation(mut self, rotation: Rotation) -> Self {
        self.set_rotation(rotation);
        self
    }

    /// Turn the UI on the panel: `rotation` is how far the UI appears turned
    /// **clockwise** (a landscape panel stood on its left edge shows an
    /// upright portrait UI at [`Rotation::Rot90`]). The UI is laid out in the
    /// rotated orientation — width and height swap for the quarter turns — and
    /// the rotation is applied at copy-out, so the framebuffer keeps its
    /// physical shape and [`Framebuffer::flush`] gets panel-space rects.
    /// [`Input`] stays in panel coordinates; the runner maps it back.
    ///
    /// Can be called at any time (an accelerometer flip): the tree relays out
    /// and the next frame repaints the whole panel.
    pub fn set_rotation(&mut self, rotation: Rotation) {
        if rotation == self.rotation {
            return;
        }
        let (sw, sh) = rotation.surface_size(self.panel.0, self.panel.1);
        let sc = Scale::new(self.scale);
        let mut surface = Surface::new(sw, sh, sc);
        surface.set_rotation(rotation);
        self.surface = surface;
        self.rotation = rotation;
        self.ui.set_size(
            Size::new(sw as f32 / self.scale, sh as f32 / self.scale),
            sc,
        );
        // A gesture in flight was tracked in the old orientation.
        self.gestures = GestureRecognizer::default();
        self.fb_current = false;
    }

    /// The current rotation (see [`set_rotation`](Self::set_rotation)).
    pub fn rotation(&self) -> Rotation {
        self.rotation
    }

    /// Another handle on the app's timer queue — for board code that wants
    /// to post the app a message (a card-detect pin, a sensor reading).
    pub fn timers(&self) -> Timers<A::Message> {
        self.timers.clone()
    }

    pub fn app(&self) -> &A {
        &self.app
    }

    pub fn app_mut(&mut self) -> &mut A {
        &mut self.app
    }

    pub fn ui(&mut self) -> &mut Ui<A::Message> {
        &mut self.ui
    }

    /// The rendered shadow surface (what the last frame copied out).
    pub fn surface(&self) -> &Surface {
        &self.surface
    }

    /// Run `f` against the app and the tree (an update from outside the
    /// input path — a card inserted, a sensor reading), then deliver any
    /// messages it queued.
    pub fn with_app<R>(&mut self, f: impl FnOnce(&mut A, &mut Ui<A::Message>) -> R) -> R {
        let r = f(&mut self.app, &mut self.ui);
        self.drain_messages();
        r
    }

    /// Forget what the framebuffer holds: the next frame copies everything.
    /// Call after something else drew into it (a boot splash, a panic screen).
    pub fn invalidate(&mut self) {
        self.fb_current = false;
    }

    /// Feed one input event at time `now_ms`.
    pub fn handle(&mut self, input: Input, now_ms: u64) {
        self.timers.set_now(now_ms);
        match input {
            Input::Key { key, pressed, mods } => self.key(key, pressed, mods),
            Input::KeyTap(key) => {
                self.key(key, true, Modifiers::default());
                self.key(key, false, Modifiers::default());
            }
            Input::PointerDown { x, y } => {
                let pos = self.logical(x, y);
                self.ui.event(Event::PointerDown {
                    pos,
                    button: PointerButton::Left,
                });
                for g in self.gestures.pointer_down(now_ms, pos) {
                    self.gesture(g);
                }
            }
            Input::PointerMove { x, y } => {
                let pos = self.logical(x, y);
                self.ui.event(Event::PointerMove { pos });
                for g in self.gestures.pointer_move(now_ms, pos) {
                    self.gesture(g);
                }
            }
            Input::PointerUp { x, y } => {
                let pos = self.logical(x, y);
                self.ui.event(Event::PointerUp {
                    pos,
                    button: PointerButton::Left,
                });
                for g in self.gestures.pointer_up(now_ms, pos) {
                    self.gesture(g);
                }
            }
            Input::Scroll { x, y, notches } => {
                let pos = self.logical(x, y);
                self.ui.event(Event::Scroll {
                    pos,
                    delta_x: 0.0,
                    delta_y: notches * SCROLL_STEP,
                });
            }
        }
        self.drain_messages();
    }

    /// Advance time to `now_ms` and, if anything changed, paint and copy the
    /// damaged spans into `fb`. Returns whether a frame was produced.
    pub fn frame(&mut self, fb: &mut impl Framebuffer, now_ms: u64) -> bool {
        let dt = match self.last_frame_ms {
            Some(prev) => now_ms.saturating_sub(prev) as f32 / 1000.0,
            None => 0.0,
        };
        self.last_frame_ms = Some(now_ms);
        self.timers.set_now(now_ms);

        // Due timers first, so what they change paints in this frame.
        let due = self.timers.take_due(now_ms);
        if !due.is_empty() {
            for m in due {
                self.app.update(m, &mut self.ui);
            }
            self.drain_messages();
        }

        for g in self.gestures.poll(now_ms) {
            self.gesture(g);
        }
        if self.ui.is_animating() {
            self.ui.animate(dt.min(MAX_DT));
            self.drain_messages();
        }
        if !self.ui.needs_paint() && self.fb_current {
            return false;
        }

        self.ui.paint(&mut self.surface);
        let info = fb.info();
        // Single-buffered scanout: once a frame landed, the buffer holds it
        // (age 1) and only damage needs copying; before that, age 0 = all.
        let age = u32::from(self.fb_current);
        let rects = self
            .surface
            .present_to_buffer(fb.pixels(), info.stride, info.format, age);
        self.fb_current = true;
        if !rects.is_empty() {
            fb.flush(&rects);
        }
        true
    }

    /// When the loop must wake next even with no input: a frame period from
    /// now while animating or a gesture is in flight (a long-press timer),
    /// the next [`Timers`] deadline if that is sooner, otherwise `None` —
    /// idle until input.
    pub fn next_deadline(&self, now_ms: u64) -> Option<u64> {
        self.timers.set_now(now_ms);
        let frame = (self.ui.is_animating() || self.gestures.is_active() || self.ui.needs_paint())
            .then_some(now_ms + FRAME_MS);
        let timer = self.timers.next_due().map(|due| due.max(now_ms));
        match (frame, timer) {
            (Some(f), Some(t)) => Some(f.min(t)),
            (f, t) => f.or(t),
        }
    }

    /// The whole main loop on a configured runner (see [`run`]): forever
    /// drain input, produce a frame if anything changed, and sleep until the
    /// next deadline.
    pub fn run(mut self, fb: &mut impl Framebuffer, board: &mut impl Board) -> ! {
        loop {
            while let Some(input) = board.poll_input() {
                let now = board.now_ms();
                self.handle(input, now);
            }
            let now = board.now_ms();
            self.frame(fb, now);
            let deadline = self.next_deadline(now);
            board.wait(deadline);
        }
    }

    /// A panel-space device pixel → UI logical coordinates.
    fn logical(&self, x: f32, y: f32) -> Point {
        let (pw, ph) = (self.panel.0 as f32, self.panel.1 as f32);
        let (ux, uy) = self.rotation.map_panel_point(x, y, pw, ph);
        Point::new(ux / self.scale, uy / self.scale)
    }

    fn key(&mut self, key: Key, pressed: bool, mods: Modifiers) {
        if pressed && self.app.on_key(key, &mut self.ui) {
            self.drain_messages();
            return;
        }
        self.ui.event(Event::Key { key, pressed, mods });
    }

    fn gesture(&mut self, g: Gesture) {
        match g {
            Gesture::Tap { pos } => self.ui.event(Event::Tap { pos }),
            Gesture::LongPress { pos } => self.ui.event(Event::LongPress { pos }),
            Gesture::Fling { pos, velocity } => self.ui.event(Event::Fling {
                pos,
                velocity_x: velocity.x,
                velocity_y: velocity.y,
            }),
            // Widgets track drags from the raw pointer events themselves.
            Gesture::DragBegin { .. } | Gesture::DragUpdate { .. } | Gesture::DragEnd { .. } => {}
        }
    }

    /// Run `App::update` until the queue is empty (an update may queue more).
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
    }
}

/// The whole bare-metal main loop: build `app`, then forever drain input,
/// produce a frame if anything changed, and sleep until the next deadline.
///
/// Shorthand for `Runner::new(app, fb.info(), scale).run(fb, board)`; build
/// the [`Runner`] yourself to configure it first (e.g. a rotation).
pub fn run<A: App>(app: A, fb: &mut impl Framebuffer, board: &mut impl Board, scale: f32) -> ! {
    Runner::new(app, fb.info(), scale).run(fb, board)
}

#[cfg(feature = "bundled-font")]
fn default_fonts() -> FontContext {
    FontContext::with_default_font()
}

#[cfg(not(feature = "bundled-font"))]
fn default_fonts() -> FontContext {
    FontContext::new()
}
