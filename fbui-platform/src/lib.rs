//! `fbui-platform` — the platform layer of the fbui framebuffer UI framework.
//!
//! This is the stable foundation Phase 1 produces: a display-server-free way to
//! own a Linux screen, read input, and survive VT switches, with **one API that
//! is ignorant of DRM vs fbdev**. Everything above it (`fbui-render`,
//! `fbui-widgets`) is written against the traits here and never learns which
//! backend is live.
//!
//! ## The four pieces
//!
//! * [`Display`] — a surface you can [`begin_frame`] → paint → [`present`]. Two
//!   backends: DRM/KMS dumb buffers (primary, vsynced page flips) and legacy
//!   fbdev (fallback). Stride and buffer age are first-class on every frame.
//! * [`InputSource`] / [`InputEvent`] — raw kernel input normalized to one tagged
//!   stream (keys with keysym+UTF-8, pointer abs/rel, touch slots, scroll).
//!   evdev by default; libinput behind a feature.
//! * [`Seat`] — who may open the nodes and who reports session changes; `noseat`
//!   (direct open) or `libseat` (logind/seatd brokered).
//! * [`VtGuard`] — owns the console in graphics mode, restores it on *every*
//!   exit path, and mediates cooperative VT switching.
//!
//! ## Getting a UI on screen
//!
//! Implement [`PlatformHandler`] and hand it to [`Platform::run`]; the
//! [`event_loop`] multiplexes every fd and calls you back. See `examples/echo.rs`
//! for a complete software-cursor-plus-keystroke-echo demo (the Phase 1 exit
//! criterion).
//!
//! [`begin_frame`]: Display::begin_frame
//! [`present`]: Display::present

pub mod cursor;
pub mod display;
pub mod error;
pub mod format;
pub mod geom;
pub mod input;
pub(crate) mod ioctl;
pub mod seat;
#[cfg(feature = "term")]
pub mod term;
pub mod vt;

#[cfg(feature = "event-loop")]
pub mod event_loop;
#[cfg(feature = "event-loop")]
pub(crate) mod uevent;

use std::path::PathBuf;

pub use crate::display::{BackendKind, Display, DisplayInfo, Frame};
pub use crate::error::{Error, Result};
pub use crate::format::PixelFormat;
pub use crate::geom::{Point, Rect, Size};
pub use crate::input::{
    keysym, AxisSource, Button, InputEvent, InputSource, KeyEvent, KeyState, Keysym, Modifiers,
};
pub use crate::seat::{Seat, SessionEvent};
pub use crate::vt::{VtEvent, VtGuard};

#[cfg(feature = "event-loop")]
pub use crate::event_loop::{Flow, PlatformHandler, Waker};

/// How to bring the platform up. [`Default`] picks the conventional nodes and
/// enables the VT guard — the right answer for a fullscreen app on the active
/// console — and honors the `FBUI_BACKEND` environment variable (`drm`,
/// `fbdev`, or `term`) so any binary can be redirected without a rebuild.
#[derive(Debug, Clone)]
pub struct PlatformConfig {
    /// DRM card node to try first.
    pub card: PathBuf,
    /// fbdev node for the fallback path.
    pub fb: PathBuf,
    /// Controlling terminal to take graphics mode on.
    pub tty: PathBuf,
    /// Skip DRM entirely and go straight to fbdev (for boards where DRM is
    /// flaky or absent).
    pub prefer_fbdev: bool,
    /// Skip the device backends entirely and run inside the controlling
    /// terminal emulator (requires the `term` feature; see [`term`]).
    pub prefer_term: bool,
    /// Prefer the libinput backend over raw evdev (requires the `libinput`
    /// feature; falls back to evdev if it can't initialize).
    pub prefer_libinput: bool,
    /// Take over the console (graphics mode + keyboard mute). Disable for
    /// serial/pty/SSH bring-up where the KD ioctls would fail.
    pub vt_guard: bool,
}

impl Default for PlatformConfig {
    fn default() -> Self {
        let backend = std::env::var("FBUI_BACKEND").unwrap_or_default();
        if !matches!(backend.as_str(), "" | "drm" | "fbdev" | "term") {
            eprintln!("[platform] ignoring unknown FBUI_BACKEND={backend:?} (drm | fbdev | term)");
        }
        PlatformConfig {
            card: PathBuf::from("/dev/dri/card0"),
            fb: PathBuf::from("/dev/fb0"),
            tty: PathBuf::from("/dev/tty"),
            prefer_fbdev: backend == "fbdev",
            prefer_term: backend == "term",
            prefer_libinput: false,
            vt_guard: true,
        }
    }
}

/// The assembled platform: a display, input sources, a seat, and the VT guard,
/// ready to [`run`](Platform::run).
// The device fields are consumed by `run`, which only exists with the
// `event-loop` feature; without it they're held but unused.
#[cfg_attr(not(feature = "event-loop"), allow(dead_code))]
pub struct Platform {
    display: Box<dyn Display>,
    inputs: Vec<Box<dyn InputSource>>,
    seat: Box<dyn Seat>,
    vt: VtGuard,
    info: DisplayInfo,
    /// Kernel uevent monitor for immediate hotplug; `None` falls back to polling.
    #[cfg(feature = "event-loop")]
    uevent: Option<crate::uevent::UeventMonitor>,
}

impl Platform {
    /// Bring everything up per `config`: pick a seat, open the display (DRM, then
    /// fbdev), take the console, wire cooperative switching when there's no seat
    /// manager, and open the input devices.
    pub fn new(config: &PlatformConfig) -> Result<Self> {
        #[cfg(feature = "term")]
        if config.prefer_term {
            return Self::new_term();
        }
        // Asking for the terminal on a build without it must fail loudly, not
        // quietly take over the console with a device backend instead.
        #[cfg(not(feature = "term"))]
        if config.prefer_term {
            return Err(Error::FeatureDisabled("term"));
        }

        let mut seat = open_seat()?;
        let display = match open_display(seat.as_mut(), config) {
            Ok(d) => d,
            // No DRM and no fbdev, but we're attached to a capable terminal
            // emulator (the "`cargo run` over SSH / on a dev box" case): run
            // in the terminal rather than dying.
            #[cfg(feature = "term")]
            Err(e) if term::suitable_for_fallback() => {
                eprintln!("[platform] no display device ({e}); falling back to the terminal");
                return Self::new_term();
            }
            Err(e) => return Err(e),
        };
        let info = display.info();
        eprintln!(
            "[platform] display {}x{} {:?} via {:?} ({} buffer{})",
            info.size.w,
            info.size.h,
            info.format,
            info.backend,
            info.buffers,
            if info.buffers == 1 { "" } else { "s" },
        );

        let mut vt = if config.vt_guard {
            match VtGuard::acquire(&config.tty.to_string_lossy()) {
                Ok(g) => g,
                Err(e) => {
                    eprintln!("[platform] VT guard unavailable ({e}); continuing without it");
                    VtGuard::disabled()
                }
            }
        } else {
            VtGuard::disabled()
        };

        // We mediate VT switching ourselves only when no seat manager does it.
        if seat.session_fd().is_none() && vt.is_active() {
            if let Err(e) = vt.enable_switching() {
                eprintln!("[platform] cooperative VT switching unavailable ({e})");
            }
        }

        let inputs = open_inputs(config, info.size)?;
        eprintln!("[platform] input: {} source(s)", inputs.len());

        // Best-effort hotplug trigger; the loop still polls as a backstop.
        #[cfg(feature = "event-loop")]
        let uevent = match crate::uevent::UeventMonitor::open() {
            Ok(m) => m,
            Err(e) => {
                eprintln!("[platform] uevent monitor unavailable ({e}); polling for hotplug");
                None
            }
        };

        Ok(Platform {
            display,
            inputs,
            seat,
            vt,
            info,
            #[cfg(feature = "event-loop")]
            uevent,
        })
    }

    /// Bring the platform up *inside the controlling terminal*: the terminal
    /// emulator is the display (kitty graphics or half-block cells) and the
    /// input device (keys + SGR mouse). No device nodes, no VT guard, no seat.
    #[cfg(feature = "term")]
    fn new_term() -> Result<Self> {
        let (display, input) = term::open_pair()?;
        let info = display.info();
        eprintln!(
            "[platform] display {}x{} {:?} via {:?} ({:?} protocol)",
            info.size.w,
            info.size.h,
            info.format,
            info.backend,
            display.protocol(),
        );
        Ok(Platform {
            display: Box::new(display),
            inputs: vec![Box::new(input)],
            seat: Box::new(term::TermSeat),
            vt: VtGuard::disabled(),
            info,
            #[cfg(feature = "event-loop")]
            uevent: None,
        })
    }

    /// The display's static properties.
    pub fn info(&self) -> DisplayInfo {
        self.info
    }

    /// Run the app until it asks to exit. Consumes the platform; the VT guard
    /// drops at the end, restoring the console.
    #[cfg(feature = "event-loop")]
    pub fn run(self, handler: &mut dyn PlatformHandler) -> Result<()> {
        crate::event_loop::run_loop(
            self.display,
            self.inputs,
            self.seat,
            self.vt,
            self.uevent,
            handler,
        )
    }
}

/// Pick the session backend: libseat if compiled and a session exists, else the
/// direct-open `noseat` path.
fn open_seat() -> Result<Box<dyn Seat>> {
    #[cfg(feature = "libseat")]
    {
        match crate::seat::libseat::LibseatSession::open() {
            Ok(s) => return Ok(Box::new(s)),
            Err(e) => eprintln!("[platform] libseat unavailable ({e}); using direct open"),
        }
    }
    #[cfg(feature = "noseat")]
    {
        return Ok(Box::new(crate::seat::noseat::NoSeat::new()));
    }
    #[allow(unreachable_code)]
    Err(Error::FeatureDisabled("noseat"))
}

/// Open the display: DRM dumb buffers first, fbdev as the fallback.
#[allow(unused_variables)]
fn open_display(seat: &mut dyn Seat, config: &PlatformConfig) -> Result<Box<dyn Display>> {
    #[cfg(feature = "drm-backend")]
    if !config.prefer_fbdev {
        match seat.open_device(&config.card) {
            Ok(fd) => {
                let card = crate::display::drm::Card::from_owned_fd(fd);
                match crate::display::drm::DrmDisplay::new(card) {
                    Ok(d) => return Ok(Box::new(d)),
                    Err(e) => eprintln!("[platform] DRM init failed ({e}); trying fbdev"),
                }
            }
            Err(e) => eprintln!(
                "[platform] open {} failed ({e}); trying fbdev",
                config.card.display()
            ),
        }
    }
    #[cfg(feature = "fbdev")]
    {
        let d = crate::display::fbdev::FbdevDisplay::open(&config.fb.to_string_lossy())?;
        return Ok(Box::new(d));
    }
    #[allow(unreachable_code)]
    Err(Error::unsupported("no display backend compiled in"))
}

/// Open the input source(s): libinput if requested+available, else evdev.
#[allow(unused_variables)]
fn open_inputs(config: &PlatformConfig, surface: Size) -> Result<Vec<Box<dyn InputSource>>> {
    #[cfg(feature = "libinput")]
    if config.prefer_libinput {
        match crate::input::libinput::LibinputInput::new_udev("seat0", surface) {
            Ok(li) => return Ok(vec![Box::new(li)]),
            Err(e) => eprintln!("[platform] libinput unavailable ({e}); using evdev"),
        }
    }
    #[cfg(feature = "evdev")]
    {
        let ev = crate::input::evdev::EvdevInput::open_all(surface)?;
        return Ok(vec![Box::new(ev)]);
    }
    #[allow(unreachable_code)]
    Ok(Vec::new())
}
