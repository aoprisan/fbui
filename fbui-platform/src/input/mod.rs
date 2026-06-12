//! Input: raw kernel events in, one normalized [`InputEvent`] stream out.
//!
//! The layers above the platform must not care whether events came from
//! libinput or raw evdev, nor parse evdev type/code/value tuples. They get a
//! single tagged enum: keys carry a keysym *and* the UTF-8 they produce, pointer
//! motion is split into relative vs absolute, touch is slot-tracked, and scroll
//! is normalized. Two backends fill this stream:
//!
//! * [`evdev`] — pure-Rust, reads `/dev/input/event*` directly. The portable
//!   default; no system libraries.
//! * [`libinput`] (feature `libinput`) — pointer acceleration, gestures, tap,
//!   calibration, hotplug via udev. Needs the C library.
//!
//! Both implement [`InputSource`], which the event loop multiplexes alongside
//! the display fd.

use std::os::unix::io::RawFd;

use crate::error::Result;
use crate::geom::Point;

#[cfg(feature = "evdev")]
pub mod evdev;
pub mod keymap;
#[cfg(feature = "libinput")]
pub mod libinput;

/// Whether a key/button transitioned down, up, or auto-repeated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyState {
    Released,
    Pressed,
    /// Kernel/libinput auto-repeat while held (evdev value 2).
    Repeated,
}

impl KeyState {
    pub fn is_down(self) -> bool {
        matches!(self, KeyState::Pressed | KeyState::Repeated)
    }
}

/// A keyboard keysym. With the `xkbcommon` feature this is a real X keysym from
/// the active layout; with the built-in fallback it's a US-QWERTY keysym. Either
/// way, named keysyms ([`keysym`] module) compare equal across both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Keysym(pub u32);

/// A handful of named keysyms (X11 values) the toolkit needs for navigation.
/// The numbers match `xkbcommon`'s so apps written against either agree.
pub mod keysym {
    use super::Keysym;
    pub const BACKSPACE: Keysym = Keysym(0xFF08);
    pub const TAB: Keysym = Keysym(0xFF09);
    pub const RETURN: Keysym = Keysym(0xFF0D);
    pub const ESCAPE: Keysym = Keysym(0xFF1B);
    pub const DELETE: Keysym = Keysym(0xFFFF);
    pub const HOME: Keysym = Keysym(0xFF50);
    pub const LEFT: Keysym = Keysym(0xFF51);
    pub const UP: Keysym = Keysym(0xFF52);
    pub const RIGHT: Keysym = Keysym(0xFF53);
    pub const DOWN: Keysym = Keysym(0xFF54);
    pub const END: Keysym = Keysym(0xFF57);
    pub const NONE: Keysym = Keysym(0);
}

bitflags::bitflags! {
    /// Active modifier state at the time of an event.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Modifiers: u8 {
        const SHIFT = 1 << 0;
        const CTRL  = 1 << 1;
        const ALT   = 1 << 2;
        /// Super / Meta / "logo" key.
        const LOGO  = 1 << 3;
        const CAPS  = 1 << 4;
        const NUM   = 1 << 5;
    }
}

/// A keyboard event after keymap translation.
#[derive(Debug, Clone)]
pub struct KeyEvent {
    /// Raw evdev keycode (hardware key), useful for game-style bindings.
    pub code: u32,
    /// Translated keysym under the current layout + modifiers.
    pub keysym: Keysym,
    /// The text this keypress produced, if any (`None` for pure modifiers,
    /// function keys, navigation keys, …). Released/repeated events also carry
    /// text where the platform produces it.
    pub utf8: Option<String>,
    pub state: KeyState,
    pub modifiers: Modifiers,
}

/// Pointer buttons, with the common three named.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
    Middle,
    /// Anything else, by its evdev `BTN_*` code.
    Other(u16),
}

/// Where a scroll event originated — discrete wheels click, fingers glide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AxisSource {
    Wheel,
    Finger,
    Continuous,
}

/// The normalized input event the rest of fbui consumes.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum InputEvent {
    Key(KeyEvent),
    /// Relative pointer motion (mice). Coordinates are deltas in pixels.
    PointerMotion {
        dx: f64,
        dy: f64,
    },
    /// Absolute pointer position (touchpads in abs mode, touchscreens used as a
    /// pointer, VMs). Already in physical pixels.
    PointerMotionAbsolute {
        position: Point,
    },
    PointerButton {
        button: Button,
        state: KeyState,
    },
    /// Scroll. Positive `vertical` scrolls the content up (wheel away from user).
    PointerAxis {
        horizontal: f64,
        vertical: f64,
        source: AxisSource,
    },
    /// A new touch contact at `position`, tracked by `slot`.
    TouchDown {
        slot: i32,
        position: Point,
    },
    TouchMotion {
        slot: i32,
        position: Point,
    },
    TouchUp {
        slot: i32,
    },
    /// The compositor/kernel cancelled all active touches (e.g. palm rejection,
    /// VT switch). Consumers must drop in-progress gestures.
    TouchCancel,
}

/// A source of normalized input events, multiplexable by the event loop.
///
/// Object-safe: the platform holds a `Box<dyn InputSource>`.
pub trait InputSource {
    /// File descriptors to poll for readiness. evdev returns one per open device
    /// (and may grow on hotplug); libinput returns its single epoll fd.
    fn fds(&self) -> Vec<RawFd>;

    /// Read all currently-available events, normalizing each into `sink`.
    /// Called after any [`fds`](InputSource::fds) descriptor signals readable.
    fn dispatch(&mut self, sink: &mut dyn FnMut(InputEvent)) -> Result<()>;
}
