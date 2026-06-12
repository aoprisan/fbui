//! Raw-evdev input backend (pure Rust, no system libraries).
//!
//! Opens `/dev/input/event*` directly and normalizes the kernel's
//! type/code/value packets into [`InputEvent`]s. This is the portable default;
//! it gives us keyboards, mice, absolute pointers, and (multi-)touch without
//! pulling in libinput. What it deliberately does *not* do is pointer
//! acceleration, gesture recognition, or palm rejection — that's the libinput
//! backend's job. The toolkit's gesture layer (Phase 4) sits on top of these
//! raw touch events either way.
//!
//! We work against evdev's *raw* event accessors (`event_type`/`code`/`value`)
//! and our own code constants rather than the crate's typed axis enums, so the
//! backend is insensitive to churn in those enums across crate versions.

use std::os::unix::io::{AsRawFd, RawFd};

use evdev::{Device, EventType};

use super::keymap::Keymap;
use super::{AxisSource, Button, InputEvent, InputSource, KeyEvent, KeyState};
use crate::error::{Error, Result};
use crate::geom::{Point, Size};

// `<linux/input-event-codes.h>` codes we reference directly.
const REL_X: u16 = 0x00;
const REL_Y: u16 = 0x01;
const REL_HWHEEL: u16 = 0x06;
const REL_WHEEL: u16 = 0x08;

const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_MT_SLOT: u16 = 0x2f;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;
const ABS_MT_TRACKING_ID: u16 = 0x39;

const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;
const BTN_MIDDLE: u16 = 0x112;
const BTN_TOUCH: u16 = 0x14a;
/// First button-class code; KEY codes below this are keyboard keys.
const BTN_FIRST: u16 = 0x100;

/// Linear range of an absolute axis, for scaling device units to pixels.
#[derive(Clone, Copy)]
struct AbsRange {
    min: i32,
    max: i32,
}

impl AbsRange {
    /// Map a device-unit value onto `[0, extent)` pixels.
    fn scale(self, v: i32, extent: u32) -> i32 {
        if self.max <= self.min || extent == 0 {
            return v;
        }
        let t = (v - self.min) as f64 / (self.max - self.min) as f64;
        (t * (extent - 1) as f64).round() as i32
    }
}

/// Per-device accumulator: input packets are a burst of type/code/value events
/// terminated by `SYN_REPORT`, so we coalesce motion within a packet and flush
/// on sync.
struct DeviceState {
    device: Device,
    fd: RawFd,
    is_multitouch: bool,
    abs_x: Option<AbsRange>,
    abs_y: Option<AbsRange>,
    // Relative motion accumulated this packet.
    rel_dx: f64,
    rel_dy: f64,
    rel_wheel: f64,
    rel_hwheel: f64,
    // Absolute pointer position being assembled this packet.
    abs_x_val: Option<i32>,
    abs_y_val: Option<i32>,
    // Multitouch: current slot + last-known position per slot for motion.
    cur_slot: i32,
    pending_x: Option<i32>,
    pending_y: Option<i32>,
    pending_id: Option<i32>,
}

impl DeviceState {
    fn new(device: Device) -> Self {
        let fd = device.as_raw_fd();
        let is_multitouch = device
            .supported_absolute_axes()
            .map(|axes| axes.iter().any(|a| a.0 == ABS_MT_SLOT))
            .unwrap_or(false);
        let (abs_x, abs_y) = abs_ranges(&device);
        DeviceState {
            device,
            fd,
            is_multitouch,
            abs_x,
            abs_y,
            rel_dx: 0.0,
            rel_dy: 0.0,
            rel_wheel: 0.0,
            rel_hwheel: 0.0,
            abs_x_val: None,
            abs_y_val: None,
            cur_slot: 0,
            pending_x: None,
            pending_y: None,
            pending_id: None,
        }
    }
}

/// Best-effort read of ABS_X / ABS_Y ranges for pointer/touch scaling.
fn abs_ranges(device: &Device) -> (Option<AbsRange>, Option<AbsRange>) {
    let mut x = None;
    let mut y = None;
    if let Ok(states) = device.get_abs_state() {
        // `get_abs_state` returns a fixed array indexed by absolute axis code.
        if let Some(info) = states.get(ABS_X as usize) {
            x = Some(AbsRange {
                min: info.minimum,
                max: info.maximum,
            });
        }
        if let Some(info) = states.get(ABS_Y as usize) {
            y = Some(AbsRange {
                min: info.minimum,
                max: info.maximum,
            });
        }
        // Multitouch panels report position under the MT axes; prefer those.
        if let Some(info) = states.get(ABS_MT_POSITION_X as usize) {
            if info.maximum > info.minimum {
                x = Some(AbsRange {
                    min: info.minimum,
                    max: info.maximum,
                });
            }
        }
        if let Some(info) = states.get(ABS_MT_POSITION_Y as usize) {
            if info.maximum > info.minimum {
                y = Some(AbsRange {
                    min: info.minimum,
                    max: info.maximum,
                });
            }
        }
    }
    (x, y)
}

/// The evdev input source: a set of open devices feeding one keymap.
pub struct EvdevInput {
    devices: Vec<DeviceState>,
    keymap: Keymap,
    /// Surface size, for scaling absolute/touch coordinates into pixels.
    surface: Size,
}

impl EvdevInput {
    /// Discover and open all input devices under `/dev/input` (the `noseat`
    /// path: needs read access, i.e. root or the `input` group).
    pub fn open_all(surface: Size) -> Result<Self> {
        let mut devices = Vec::new();
        for (_path, device) in evdev::enumerate() {
            if let Err(e) = configure(&device) {
                // Non-fatal: skip a device we can't set nonblocking on.
                let _ = e;
                continue;
            }
            devices.push(DeviceState::new(device));
        }
        Ok(EvdevInput {
            devices,
            keymap: Keymap::new(),
            surface,
        })
    }

    /// Build from already-opened devices (e.g. fds brokered by a seat manager).
    pub fn from_devices(devices: Vec<Device>, surface: Size) -> Result<Self> {
        let mut states = Vec::new();
        for d in devices {
            configure(&d).map_err(|e| Error::io("set evdev nonblocking", e))?;
            states.push(DeviceState::new(d));
        }
        Ok(EvdevInput {
            devices: states,
            keymap: Keymap::new(),
            surface,
        })
    }

    /// Update the surface size used to scale absolute coordinates (on mode
    /// change / hotplug of the display).
    pub fn set_surface(&mut self, surface: Size) {
        self.surface = surface;
    }
}

/// Put a device in nonblocking mode so `fetch_events` drains and returns instead
/// of blocking once we've consumed what `poll` told us was ready.
fn configure(device: &Device) -> std::io::Result<()> {
    device.set_nonblocking(true)
}

impl InputSource for EvdevInput {
    fn fds(&self) -> Vec<RawFd> {
        self.devices.iter().map(|d| d.fd).collect()
    }

    fn dispatch(&mut self, sink: &mut dyn FnMut(InputEvent)) -> Result<()> {
        let surface = self.surface;
        let keymap = &mut self.keymap;
        for dev in &mut self.devices {
            // Drain everything currently buffered into an owned vec first: the
            // `fetch_events` iterator borrows `dev.device`, but `translate` needs
            // `&mut dev`. EAGAIN just means "nothing more to read".
            let events: Vec<evdev::InputEvent> = match dev.device.fetch_events() {
                Ok(it) => it.collect(),
                Err(e) if e.raw_os_error() == Some(libc::EAGAIN) => continue,
                Err(e) => return Err(Error::io("evdev fetch_events", e)),
            };
            for ev in &events {
                translate(dev, keymap, surface, ev, sink);
            }
        }
        Ok(())
    }
}

/// Translate one raw evdev event, flushing coalesced motion on `SYN_REPORT`.
fn translate(
    dev: &mut DeviceState,
    keymap: &mut Keymap,
    surface: Size,
    ev: &evdev::InputEvent,
    sink: &mut dyn FnMut(InputEvent),
) {
    let code = ev.code();
    let value = ev.value();
    match ev.event_type() {
        EventType::KEY => {
            if code >= BTN_FIRST {
                handle_button(dev, code, value, sink);
            } else {
                handle_key(keymap, code as u32, value, sink);
            }
        }
        EventType::RELATIVE => match code {
            REL_X => dev.rel_dx += value as f64,
            REL_Y => dev.rel_dy += value as f64,
            REL_WHEEL => dev.rel_wheel += value as f64,
            REL_HWHEEL => dev.rel_hwheel += value as f64,
            _ => {}
        },
        EventType::ABSOLUTE => handle_abs(dev, code, value),
        EventType::SYNCHRONIZATION => flush_packet(dev, surface, sink),
        _ => {}
    }
}

fn handle_key(keymap: &mut Keymap, code: u32, value: i32, sink: &mut dyn FnMut(InputEvent)) {
    let state = match value {
        0 => KeyState::Released,
        2 => KeyState::Repeated,
        _ => KeyState::Pressed,
    };
    let t = keymap.key(code, state.is_down());
    sink(InputEvent::Key(KeyEvent {
        code,
        keysym: t.keysym,
        utf8: t.utf8,
        state,
        modifiers: t.modifiers,
    }));
}

fn handle_button(dev: &mut DeviceState, code: u16, value: i32, sink: &mut dyn FnMut(InputEvent)) {
    // On a touchscreen, BTN_TOUCH brackets a single-finger contact; for MT
    // devices the slot/tracking-id machinery drives touch instead, so ignore it.
    if code == BTN_TOUCH && dev.is_multitouch {
        return;
    }
    let button = match code {
        BTN_LEFT => Button::Left,
        BTN_RIGHT => Button::Right,
        BTN_MIDDLE => Button::Middle,
        other => Button::Other(other),
    };
    let state = if value == 0 {
        KeyState::Released
    } else {
        KeyState::Pressed
    };
    sink(InputEvent::PointerButton { button, state });
}

fn handle_abs(dev: &mut DeviceState, code: u16, value: i32) {
    match code {
        ABS_X => dev.abs_x_val = Some(value),
        ABS_Y => dev.abs_y_val = Some(value),
        ABS_MT_SLOT => dev.cur_slot = value,
        ABS_MT_POSITION_X => dev.pending_x = Some(value),
        ABS_MT_POSITION_Y => dev.pending_y = Some(value),
        ABS_MT_TRACKING_ID => dev.pending_id = Some(value),
        _ => {}
    }
}

/// Emit the coalesced result of one input packet.
fn flush_packet(dev: &mut DeviceState, surface: Size, sink: &mut dyn FnMut(InputEvent)) {
    // Relative pointer motion.
    if dev.rel_dx != 0.0 || dev.rel_dy != 0.0 {
        sink(InputEvent::PointerMotion {
            dx: dev.rel_dx,
            dy: dev.rel_dy,
        });
        dev.rel_dx = 0.0;
        dev.rel_dy = 0.0;
    }
    // Scroll.
    if dev.rel_wheel != 0.0 || dev.rel_hwheel != 0.0 {
        sink(InputEvent::PointerAxis {
            horizontal: dev.rel_hwheel,
            vertical: dev.rel_wheel,
            source: AxisSource::Wheel,
        });
        dev.rel_wheel = 0.0;
        dev.rel_hwheel = 0.0;
    }

    if dev.is_multitouch {
        flush_touch(dev, surface, sink);
    } else if dev.abs_x_val.is_some() || dev.abs_y_val.is_some() {
        // Absolute pointer (VM tablet / single-touch panel): emit a position.
        let x = dev.abs_x_val.unwrap_or(0);
        let y = dev.abs_y_val.unwrap_or(0);
        let px = dev.abs_x.map(|r| r.scale(x, surface.w)).unwrap_or(x);
        let py = dev.abs_y.map(|r| r.scale(y, surface.h)).unwrap_or(y);
        sink(InputEvent::PointerMotionAbsolute {
            position: Point::new(px, py),
        });
        dev.abs_x_val = None;
        dev.abs_y_val = None;
    }
}

/// Resolve one MT packet for the current slot into a touch event.
fn flush_touch(dev: &mut DeviceState, surface: Size, sink: &mut dyn FnMut(InputEvent)) {
    let slot = dev.cur_slot;
    let pos = match (dev.pending_x.take(), dev.pending_y.take()) {
        (Some(x), Some(y)) => {
            let px = dev.abs_x.map(|r| r.scale(x, surface.w)).unwrap_or(x);
            let py = dev.abs_y.map(|r| r.scale(y, surface.h)).unwrap_or(y);
            Some(Point::new(px, py))
        }
        _ => None,
    };
    match dev.pending_id.take() {
        Some(-1) => sink(InputEvent::TouchUp { slot }),
        Some(_id) => {
            // New contact: tracking id assigned this packet.
            if let Some(p) = pos {
                sink(InputEvent::TouchDown { slot, position: p });
            }
        }
        None => {
            // Continuation: position update for an existing contact.
            if let Some(p) = pos {
                sink(InputEvent::TouchMotion { slot, position: p });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abs_range_scales_endpoints() {
        let r = AbsRange { min: 0, max: 1000 };
        assert_eq!(r.scale(0, 100), 0);
        assert_eq!(r.scale(1000, 100), 99);
        assert_eq!(r.scale(500, 100), 50);
    }

    #[test]
    fn abs_range_identity_when_degenerate() {
        let r = AbsRange { min: 5, max: 5 };
        assert_eq!(r.scale(42, 100), 42);
    }
}
