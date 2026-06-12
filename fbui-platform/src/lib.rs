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

pub mod backend;
pub mod cursor;
pub mod display;
pub mod error;
pub mod format;
pub mod geom;
pub mod input;
pub(crate) mod ioctl;
pub mod seat;
pub mod vt;

#[cfg(feature = "event-loop")]
pub mod event_loop;

use std::path::PathBuf;

pub use crate::backend::{Attempt, Backend};
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
pub use crate::event_loop::{Flow, PlatformHandler};

/// How to bring the platform up. [`Default`] picks the conventional nodes and
/// enables the VT guard — the right answer for a fullscreen app on the active
/// console.
#[derive(Debug, Clone)]
pub struct PlatformConfig {
    /// DRM card node to try first.
    pub card: PathBuf,
    /// fbdev node for the fallback path.
    pub fb: PathBuf,
    /// Controlling terminal to take graphics mode on.
    pub tty: PathBuf,
    /// Which display backend to bring up. [`Backend::Auto`] (the default) tries
    /// DRM/KMS dumb buffers then the fbdev fallback; an explicit choice (or the
    /// `FBUI_BACKEND` env var) overrides it. The software path is the default.
    pub backend: Backend,
    /// Prefer the libinput backend over raw evdev (requires the `libinput`
    /// feature; falls back to evdev if it can't initialize).
    pub prefer_libinput: bool,
    /// Take over the console (graphics mode + keyboard mute). Disable for
    /// serial/pty/SSH bring-up where the KD ioctls would fail.
    pub vt_guard: bool,
}

impl Default for PlatformConfig {
    fn default() -> Self {
        PlatformConfig {
            card: PathBuf::from("/dev/dri/card0"),
            fb: PathBuf::from("/dev/fb0"),
            tty: PathBuf::from("/dev/tty"),
            // Honor FBUI_BACKEND so the backend can be chosen at runtime without
            // recompiling; unset ⇒ Auto.
            backend: Backend::from_env(),
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
}

impl Platform {
    /// Bring everything up per `config`: pick a seat, open the display (DRM, then
    /// fbdev), take the console, wire cooperative switching when there's no seat
    /// manager, and open the input devices.
    pub fn new(config: &PlatformConfig) -> Result<Self> {
        let mut seat = open_seat()?;
        let display = open_display(seat.as_mut(), config)?;
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

        Ok(Platform {
            display,
            inputs,
            seat,
            vt,
            info,
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
        crate::event_loop::run_loop(self.display, self.inputs, self.seat, self.vt, handler)
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

/// Open the display by walking the [`Backend::order`] for the configured
/// preference, returning the first attempt that succeeds. Only [`Backend::Auto`]
/// falls back between attempts; an explicit choice surfaces its error.
#[allow(unused_variables)]
fn open_display(seat: &mut dyn Seat, config: &PlatformConfig) -> Result<Box<dyn Display>> {
    let order = config.backend.order();
    let mut last_err: Option<Error> = None;
    for &attempt in order {
        match try_open(seat, config, attempt) {
            Ok(Some(display)) => return Ok(display),
            // Feature not compiled in: skip to the next attempt.
            Ok(None) => {
                eprintln!("[platform] {attempt:?} backend not compiled in; skipping");
            }
            Err(e) => {
                eprintln!("[platform] {attempt:?} backend unavailable ({e})");
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| {
        Error::unsupported(format!(
            "no usable display backend for {:?} (compiled-in: {})",
            config.backend,
            compiled_backends()
        ))
    }))
}

/// Try one concrete backend. `Ok(None)` means "this backend isn't compiled in"
/// (skip it); `Err` means it's present but failed to come up.
#[allow(unused_variables)]
fn try_open(
    seat: &mut dyn Seat,
    config: &PlatformConfig,
    attempt: Attempt,
) -> Result<Option<Box<dyn Display>>> {
    match attempt {
        #[cfg(feature = "drm-backend")]
        Attempt::DrmDumb => {
            let fd = seat.open_device(&config.card)?;
            let card = crate::display::drm::Card::from_owned_fd(fd);
            let display = crate::display::drm::DrmDisplay::new(card)?;
            Ok(Some(Box::new(display)))
        }
        #[cfg(not(feature = "drm-backend"))]
        Attempt::DrmDumb => Ok(None),
        #[cfg(feature = "fbdev")]
        Attempt::Fbdev => {
            let display = crate::display::fbdev::FbdevDisplay::open(&config.fb.to_string_lossy())?;
            Ok(Some(Box::new(display)))
        }
        #[cfg(not(feature = "fbdev"))]
        Attempt::Fbdev => Ok(None),
        // The DRM+GBM+EGL path (Phase 6). The trait seam is here; the backend
        // itself needs the `gpu` feature and a GPU/EGL host (see PHASE6.md).
        Attempt::Gpu => Err(Error::unsupported(
            "GPU backend (drm-gbm-egl) is not built in; it requires the `gpu` \
             feature and a GPU/EGL host — see PHASE6.md",
        )),
    }
}

/// Human-readable list of which display backends this build actually contains,
/// for the "no usable backend" diagnostic.
fn compiled_backends() -> &'static str {
    match (cfg!(feature = "drm-backend"), cfg!(feature = "fbdev")) {
        (true, true) => "drm-dumb, fbdev",
        (true, false) => "drm-dumb",
        (false, true) => "fbdev",
        (false, false) => "none",
    }
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
