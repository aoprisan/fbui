//! libinput backend (feature `libinput`).
//!
//! The richer input path: udev device discovery + hotplug, pointer
//! acceleration, natural scrolling, tap-to-click, and touch calibration, all
//! handled by libinput before we ever see an event. We translate libinput's
//! events into the same [`InputEvent`] stream the evdev backend produces, so
//! nothing above the platform can tell which backend is live.
//!
//! This module links the system `libinput` C library and is therefore only
//! compiled with the `libinput` feature. It is **not** built or tested in
//! environments lacking the library (see `PHASE1.md`); the pure-Rust [`evdev`]
//! backend is the verified default.
//!
//! [`evdev`]: super::evdev

use std::os::unix::io::{AsRawFd, BorrowedFd, OwnedFd, RawFd};
use std::path::Path;

use input::event::keyboard::{KeyState as LiKeyState, KeyboardEventTrait};
use input::event::pointer::{Axis, ButtonState, PointerScrollEvent};
use input::event::{Event, PointerEvent, TouchEvent};
use input::{Libinput, LibinputInterface};

use super::keymap::Keymap;
use super::{AxisSource, Button, InputEvent, InputSource, KeyEvent, KeyState};
use crate::error::{Error, Result};
use crate::geom::{Point, Size};

/// How libinput should open and close device nodes. Under `noseat` we open them
/// directly; a seat manager would route these through its session instead.
struct DirectInterface;

impl LibinputInterface for DirectInterface {
    fn open_restricted(&mut self, path: &Path, flags: i32) -> std::result::Result<OwnedFd, i32> {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .custom_flags(flags)
            .read(true)
            .write((flags & libc::O_RDWR != 0) || (flags & libc::O_WRONLY != 0))
            .open(path)
            .map(OwnedFd::from)
            .map_err(|e| e.raw_os_error().unwrap_or(libc::EIO))
    }

    fn close_restricted(&mut self, fd: OwnedFd) {
        drop(fd);
    }
}

/// libinput-backed input source.
pub struct LibinputInput {
    li: Libinput,
    fd: RawFd,
    keymap: Keymap,
    surface: Size,
}

impl LibinputInput {
    /// Create a udev-backed context and assign it `seat_name` (usually
    /// `"seat0"`). Devices are opened directly, so this needs root or the
    /// `input` group; pair with the `libseat` backend for unprivileged use.
    pub fn new_udev(seat_name: &str, surface: Size) -> Result<Self> {
        let mut li = Libinput::new_with_udev(DirectInterface);
        li.udev_assign_seat(seat_name)
            .map_err(|_| Error::Seat(format!("libinput: could not assign seat {seat_name}")))?;
        let fd = li.as_raw_fd();
        Ok(LibinputInput {
            li,
            fd,
            keymap: Keymap::new(),
            surface,
        })
    }

    pub fn set_surface(&mut self, surface: Size) {
        self.surface = surface;
    }

    fn translate(&mut self, event: Event, sink: &mut dyn FnMut(InputEvent)) {
        match event {
            Event::Keyboard(k) => {
                let code = k.key();
                let pressed = matches!(k.key_state(), LiKeyState::Pressed);
                let state = if pressed {
                    KeyState::Pressed
                } else {
                    KeyState::Released
                };
                let t = self.keymap.key(code, pressed);
                sink(InputEvent::Key(KeyEvent {
                    code,
                    keysym: t.keysym,
                    utf8: t.utf8,
                    state,
                    modifiers: t.modifiers,
                }));
            }
            Event::Pointer(p) => self.translate_pointer(p, sink),
            Event::Touch(t) => self.translate_touch(t, sink),
            _ => {}
        }
    }

    fn translate_pointer(&mut self, p: PointerEvent, sink: &mut dyn FnMut(InputEvent)) {
        match p {
            PointerEvent::Motion(m) => {
                sink(InputEvent::PointerMotion {
                    dx: m.dx(),
                    dy: m.dy(),
                });
            }
            PointerEvent::MotionAbsolute(m) => {
                let x = m.absolute_x_transformed(self.surface.w) as i32;
                let y = m.absolute_y_transformed(self.surface.h) as i32;
                sink(InputEvent::PointerMotionAbsolute {
                    position: Point::new(x, y),
                });
            }
            PointerEvent::Button(b) => {
                let button = match b.button() {
                    0x110 => Button::Left,
                    0x111 => Button::Right,
                    0x112 => Button::Middle,
                    other => Button::Other(other as u16),
                };
                let state = match b.button_state() {
                    ButtonState::Pressed => KeyState::Pressed,
                    ButtonState::Released => KeyState::Released,
                };
                sink(InputEvent::PointerButton { button, state });
            }
            PointerEvent::ScrollWheel(s) => emit_scroll(&s, AxisSource::Wheel, sink),
            PointerEvent::ScrollFinger(s) => emit_scroll(&s, AxisSource::Finger, sink),
            PointerEvent::ScrollContinuous(s) => emit_scroll(&s, AxisSource::Continuous, sink),
            _ => {}
        }
    }

    fn translate_touch(&mut self, t: TouchEvent, sink: &mut dyn FnMut(InputEvent)) {
        use input::event::touch::{TouchEventPosition, TouchEventSlot};
        match t {
            TouchEvent::Down(d) => {
                let slot = d.slot().map(|s| s as i32).unwrap_or(0);
                let x = d.x_transformed(self.surface.w) as i32;
                let y = d.y_transformed(self.surface.h) as i32;
                sink(InputEvent::TouchDown {
                    slot,
                    position: Point::new(x, y),
                });
            }
            TouchEvent::Motion(m) => {
                let slot = m.slot().map(|s| s as i32).unwrap_or(0);
                let x = m.x_transformed(self.surface.w) as i32;
                let y = m.y_transformed(self.surface.h) as i32;
                sink(InputEvent::TouchMotion {
                    slot,
                    position: Point::new(x, y),
                });
            }
            TouchEvent::Up(u) => {
                let slot = u.slot().map(|s| s as i32).unwrap_or(0);
                sink(InputEvent::TouchUp { slot });
            }
            TouchEvent::Cancel(_) => sink(InputEvent::TouchCancel),
            _ => {}
        }
    }
}

fn emit_scroll<S: PointerScrollEvent>(s: &S, source: AxisSource, sink: &mut dyn FnMut(InputEvent)) {
    let horizontal = if s.has_axis(Axis::Horizontal) {
        s.scroll_value(Axis::Horizontal)
    } else {
        0.0
    };
    let vertical = if s.has_axis(Axis::Vertical) {
        s.scroll_value(Axis::Vertical)
    } else {
        0.0
    };
    if horizontal != 0.0 || vertical != 0.0 {
        sink(InputEvent::PointerAxis {
            horizontal,
            vertical,
            source,
        });
    }
}

impl InputSource for LibinputInput {
    fn fds(&self) -> Vec<RawFd> {
        vec![self.fd]
    }

    fn dispatch(&mut self, sink: &mut dyn FnMut(InputEvent)) -> Result<()> {
        self.li
            .dispatch()
            .map_err(|e| Error::io("libinput dispatch", e))?;
        // Drain queued events. `Libinput` is an iterator over ready events.
        let events: Vec<Event> = (&mut self.li).collect();
        for ev in events {
            self.translate(ev, sink);
        }
        Ok(())
    }
}

/// Borrowed view of the libinput fd, for callers that prefer `BorrowedFd`.
impl LibinputInput {
    pub fn as_borrowed_fd(&self) -> BorrowedFd<'_> {
        // SAFETY: `self.fd` is the libinput context's epoll fd, valid for the
        // lifetime of `self`.
        unsafe { BorrowedFd::borrow_raw(self.fd) }
    }
}
