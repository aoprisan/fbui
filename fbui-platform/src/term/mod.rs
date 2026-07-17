//! The terminal backend (feature `term`): run an fbui app *inside a terminal*
//! — over SSH, in a terminal emulator on a dev laptop, in CI — with no DRM
//! node, no fbdev, no root, and no real VT.
//!
//! It is an ordinary display/input backend pair behind the same traits as
//! DRM/evdev, so every app gains it without a line of code changed:
//!
//! * [`TermDisplay`] presents the frame either as
//!   full-resolution pixels via the **kitty graphics protocol** (kitty,
//!   Ghostty, WezTerm, Konsole, …) with damage expressed as small patch
//!   images, or as **half-block cells** (`▀` + 24-bit color, ~2 pixels per
//!   character cell) in any truecolor terminal.
//! * [`TermInput`] turns the raw byte stream into normalized
//!   [`InputEvent`](crate::InputEvent)s: UTF-8 keys, CSI/SS3 navigation keys
//!   with modifiers, and SGR mouse reporting (buttons, motion, wheel — with
//!   pixel-precision coordinates where the terminal supports SGR-Pixels).
//!
//! Selection: `FBUI_BACKEND=term` forces it; otherwise the platform falls back
//! to it automatically when neither DRM nor fbdev can be opened and the
//! process is attached to an interactive terminal (the "`cargo run` on a dev
//! box over SSH" case). `FBUI_TERM_PROTOCOL=kitty|cells` overrides protocol
//! auto-detection.
//!
//! The same crash-safety invariant as [`VtGuard`](crate::VtGuard) applies: the
//! terminal — raw mode, the alternate screen, mouse reporting, transmitted
//! images — is restored on *every* exit path: drop, panic, and fatal signals.

use std::cell::UnsafeCell;
use std::io;
use std::os::unix::io::{AsRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, Once};

use crate::error::{Error, Result};

pub mod display;
mod encode;
pub mod input;

pub use display::TermDisplay;
pub use input::TermInput;

/// How pixels reach the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TermProtocol {
    /// Kitty graphics protocol: real pixels, damage as patch placements.
    Kitty,
    /// Unicode half-blocks: one `▀` per cell, 1×2 pixels, truecolor SGR.
    Cells,
}

/// Escapes written when taking the terminal: alternate screen, cursor hidden,
/// autowrap off (so a bottom-right cell write can't scroll), clear, and SGR
/// mouse reporting for buttons + all motion.
const SETUP: &[u8] = b"\x1b[?1049h\x1b[?25l\x1b[?7l\x1b[2J\x1b[H\x1b[?1002h\x1b[?1003h\x1b[?1006h";
/// Extra setup in kitty mode: SGR-Pixels mouse coordinates.
const SETUP_PIXEL_MOUSE: &[u8] = b"\x1b[?1016h";
/// The unconditional restore, written on every exit path. Deleting kitty
/// images or disabling never-enabled modes is harmless on other terminals.
const RESTORE: &[u8] =
    b"\x1b_Ga=d,d=A,q=2\x1b\\\x1b[?1016l\x1b[?1003l\x1b[?1002l\x1b[?1006l\x1b[0m\x1b[?25h\x1b[?7h\x1b[?1049l";

// ---- global restore state (the VtGuard pattern: atomics + a Once) ---------

static TERM_FD: AtomicI32 = AtomicI32::new(-1);
static TERM_ACTIVE: AtomicBool = AtomicBool::new(false);
static HOOKS_INSTALLED: Once = Once::new();

/// The termios to restore, written once per acquire *before* `TERM_ACTIVE`
/// flips true (SeqCst gives the signal handler a happens-before edge), read
/// only after swapping it false — the same discipline as `vt.rs`.
struct TermiosCell(UnsafeCell<libc::termios>);
// SAFETY: access is serialized by the TERM_ACTIVE flag as described above.
unsafe impl Sync for TermiosCell {}
static SAVED_TERMIOS: TermiosCell = TermiosCell(UnsafeCell::new(unsafe { std::mem::zeroed() }));

/// Put the terminal back: images deleted, mouse off, cursor shown, main
/// screen, cooked termios. Async-signal-safe (write + tcsetattr) and
/// idempotent via the `TERM_ACTIVE` swap.
fn restore_terminal() {
    if !TERM_ACTIVE.swap(false, Ordering::SeqCst) {
        return;
    }
    let fd = TERM_FD.load(Ordering::SeqCst);
    if fd < 0 {
        return;
    }
    write_all_best_effort(fd, RESTORE);
    // SAFETY: reading the cell after the swap; set before ACTIVE went true.
    unsafe {
        libc::tcsetattr(fd, libc::TCSANOW, SAVED_TERMIOS.0.get());
    }
}

/// The one write(2) loop the whole backend uses: full write or an error.
pub(crate) fn write_all(fd: RawFd, mut buf: &[u8]) -> io::Result<()> {
    while !buf.is_empty() {
        // SAFETY: plain write(2) on a live fd with an in-bounds buffer.
        let n = unsafe { libc::write(fd, buf.as_ptr() as *const libc::c_void, buf.len()) };
        if n > 0 {
            buf = &buf[n as usize..];
        } else {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(err);
        }
    }
    Ok(())
}

/// [`write_all`] for exit paths, where an EIO/closed pty can't be acted on.
fn write_all_best_effort(fd: RawFd, buf: &[u8]) {
    let _ = write_all(fd, buf);
}

extern "C" fn term_signal_restore(sig: libc::c_int) {
    restore_terminal();
    // Re-raise with the default disposition, as vt.rs does.
    unsafe {
        libc::signal(sig, libc::SIG_DFL);
        libc::raise(sig);
    }
}

// NOTE: these hooks and vt.rs's install their handlers for the same fatal
// signals, and `libc::signal` replaces rather than chains — whichever
// subsystem armed last wins. That's safe today because the two guards never
// coexist (the term backend always runs with `VtGuard::disabled()`), but a
// future backend mixing them should migrate both onto one shared
// restore-registry hook.
fn install_hooks_once() {
    HOOKS_INSTALLED.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal();
            prev(info);
        }));
        for sig in [
            libc::SIGINT,
            libc::SIGTERM,
            libc::SIGHUP,
            libc::SIGQUIT,
            libc::SIGSEGV,
            libc::SIGABRT,
            libc::SIGILL,
            libc::SIGBUS,
            libc::SIGFPE,
        ] {
            // SAFETY: installing a minimal async-signal-safe handler.
            unsafe {
                libc::signal(sig, term_signal_restore as *const () as usize);
            }
        }
    });
}

// ---- guard -----------------------------------------------------------------

/// RAII ownership of one terminal: raw termios + the [`SETUP`] screen state,
/// undone in [`Drop`] (and by the global panic/signal hooks when
/// `global_restore` was requested — the production path; tests opt out so
/// parallel pty tests don't fight over the statics).
pub(crate) struct TtyGuard {
    fd: Arc<OwnedFd>,
    saved: libc::termios,
    global: bool,
}

impl TtyGuard {
    fn acquire(fd: Arc<OwnedFd>, pixel_mouse: bool, global: bool) -> Result<Self> {
        let raw = fd.as_raw_fd();
        // SAFETY: isatty on a live fd.
        if unsafe { libc::isatty(raw) } == 0 {
            return Err(Error::unsupported("term backend needs a tty"));
        }
        // SAFETY: tcgetattr writes a termios through the pointer.
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(raw, &mut saved) } != 0 {
            return Err(Error::io("tcgetattr", io::Error::last_os_error()));
        }
        let mut rawios = saved;
        // SAFETY: cfmakeraw mutates the struct in place.
        unsafe { libc::cfmakeraw(&mut rawios) };
        rawios.c_cc[libc::VMIN] = 1;
        rawios.c_cc[libc::VTIME] = 0;

        if global {
            // Arm the crash restore *before* changing anything, so a wedged
            // setup still puts the terminal back.
            // SAFETY: writing the cell before TERM_ACTIVE goes true.
            unsafe {
                *SAVED_TERMIOS.0.get() = saved;
            }
            TERM_FD.store(raw, Ordering::SeqCst);
            install_hooks_once();
            TERM_ACTIVE.store(true, Ordering::SeqCst);
        }

        if unsafe { libc::tcsetattr(raw, libc::TCSANOW, &rawios) } != 0 {
            if global {
                TERM_ACTIVE.store(false, Ordering::SeqCst);
                TERM_FD.store(-1, Ordering::SeqCst);
            }
            return Err(Error::io("tcsetattr raw", io::Error::last_os_error()));
        }
        write_all_best_effort(raw, SETUP);
        if pixel_mouse {
            write_all_best_effort(raw, SETUP_PIXEL_MOUSE);
        }
        Ok(TtyGuard { fd, saved, global })
    }
}

impl Drop for TtyGuard {
    fn drop(&mut self) {
        let raw = self.fd.as_raw_fd();
        if self.global {
            // The global path owns the escape + termios restore (idempotent).
            restore_terminal();
            TERM_FD.store(-1, Ordering::SeqCst);
        } else {
            write_all_best_effort(raw, RESTORE);
            // SAFETY: restoring the termios we saved at acquire.
            unsafe {
                libc::tcsetattr(raw, libc::TCSANOW, &self.saved);
            }
        }
    }
}

// ---- shared display/input state --------------------------------------------

/// State the display writes and the input reader consumes: the tty itself,
/// the mouse coordinate space, and any bytes the display swallowed while
/// querying the terminal that belong to the input stream.
pub(crate) struct Shared {
    pub fd: Arc<OwnedFd>,
    /// Mouse reports arrive in pixels (SGR-Pixels, mode 1016) vs cells.
    pub mouse_pixels: AtomicBool,
    /// Pixel size of one character cell: read by TermInput to scale
    /// cell-mouse coordinates, written by TermDisplay when a resize changes
    /// it. The single source of truth for cell geometry.
    pub cell_w: AtomicU32,
    pub cell_h: AtomicU32,
    /// Bytes read past a query response during bring-up; TermInput drains
    /// these before reading the fd.
    pub pending_input: Mutex<Vec<u8>>,
    /// Armed (readable) eventfd when `pending_input` holds bytes, so the
    /// event loop calls `dispatch` for them without waiting for new tty
    /// input. `None` when nothing was stashed.
    pub input_wake: Option<OwnedFd>,
}

/// Terminal geometry as the kernel reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WinSize {
    pub cols: u32,
    pub rows: u32,
    /// Text area in pixels; zero when the terminal doesn't report it.
    pub x_px: u32,
    pub y_px: u32,
}

pub(crate) fn winsize(fd: RawFd) -> Result<WinSize> {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: TIOCGWINSZ fills a winsize struct.
    if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) } != 0 {
        return Err(Error::io("TIOCGWINSZ", io::Error::last_os_error()));
    }
    if ws.ws_col == 0 || ws.ws_row == 0 {
        return Err(Error::unsupported("terminal reports a zero-sized window"));
    }
    Ok(WinSize {
        cols: ws.ws_col as u32,
        rows: ws.ws_row as u32,
        x_px: ws.ws_xpixel as u32,
        y_px: ws.ws_ypixel as u32,
    })
}

/// Ask the terminal for its text-area pixel size (`CSI 14 t`), with a short
/// deadline so unsupporting terminals just fall through. Any unrelated bytes
/// read while waiting are stashed for [`TermInput`].
fn query_pixel_size(fd: RawFd, stash: &mut Vec<u8>) -> Option<(u32, u32)> {
    write_all_best_effort(fd, b"\x1b[14t");
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(200);
    let mut buf = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            stash.extend_from_slice(&buf);
            return None;
        }
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: polling one fd with a millisecond timeout.
        let n = unsafe { libc::poll(&mut pfd, 1, left.as_millis() as i32) };
        if n <= 0 {
            stash.extend_from_slice(&buf);
            return None;
        }
        let mut byte = [0u8; 64];
        // SAFETY: reading into a stack buffer.
        let r = unsafe { libc::read(fd, byte.as_mut_ptr() as *mut libc::c_void, byte.len()) };
        if r <= 0 {
            stash.extend_from_slice(&buf);
            return None;
        }
        buf.extend_from_slice(&byte[..r as usize]);
        // Look for ESC [ 4 ; height ; width t
        if let Some((consumed_to, h, w)) = parse_pixel_report(&buf) {
            // Everything except the report belongs to the input stream.
            stash.extend_from_slice(&buf[..consumed_to.0]);
            stash.extend_from_slice(&buf[consumed_to.1..]);
            if w > 0 && h > 0 {
                return Some((w, h));
            }
            return None;
        }
    }
}

/// Find `ESC [ 4 ; h ; w t` in `buf`; returns ((start, end), h, w).
fn parse_pixel_report(buf: &[u8]) -> Option<((usize, usize), u32, u32)> {
    let mut i = 0;
    while i + 4 < buf.len() {
        if buf[i] == 0x1b && buf[i + 1] == b'[' && buf[i + 2] == b'4' && buf[i + 3] == b';' {
            let mut j = i + 4;
            let mut nums = [0u32; 2];
            let mut which = 0;
            while j < buf.len() {
                match buf[j] {
                    b'0'..=b'9' if which < 2 => {
                        nums[which] = nums[which].saturating_mul(10) + (buf[j] - b'0') as u32;
                        j += 1;
                    }
                    b';' if which == 0 => {
                        which = 1;
                        j += 1;
                    }
                    b't' if which == 1 => {
                        return Some(((i, j + 1), nums[0], nums[1]));
                    }
                    _ => break,
                }
            }
            if j >= buf.len() {
                return None; // incomplete; keep reading
            }
        }
        i += 1;
    }
    None
}

// ---- detection ---------------------------------------------------------------

/// Would falling back to the terminal backend make sense right now? True when
/// the process has a controlling terminal that looks like a capable emulator
/// (not the bare Linux console, where a DRM/fbdev failure is a permissions
/// problem the user should see, and not a dumb pipe). An explicit
/// `FBUI_BACKEND` naming a device backend disables the fallback entirely —
/// asking for DRM means wanting DRM's error, not a different backend.
pub(crate) fn suitable_for_fallback() -> bool {
    if matches!(
        std::env::var("FBUI_BACKEND").as_deref(),
        Ok("drm") | Ok("fbdev")
    ) {
        return false;
    }
    let term = std::env::var("TERM").unwrap_or_default();
    if term.is_empty() || term == "dumb" || term == "linux" {
        return false;
    }
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .map(|f| unsafe { libc::isatty(f.as_raw_fd()) } == 1)
        .unwrap_or(false)
}

/// Pick the pixel protocol: `FBUI_TERM_PROTOCOL` wins, then known
/// kitty-graphics terminals, then the universal half-block fallback.
pub(crate) fn detect_protocol() -> TermProtocol {
    match std::env::var("FBUI_TERM_PROTOCOL").as_deref() {
        Ok("kitty") => return TermProtocol::Kitty,
        Ok("cells") | Ok("halfblocks") => return TermProtocol::Cells,
        _ => {}
    }
    let term = std::env::var("TERM").unwrap_or_default();
    let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default();
    if std::env::var_os("KITTY_WINDOW_ID").is_some()
        || term.contains("kitty")
        || term.contains("ghostty")
        || term_program == "WezTerm"
        || term_program == "ghostty"
        || std::env::var_os("WEZTERM_PANE").is_some()
    {
        TermProtocol::Kitty
    } else {
        TermProtocol::Cells
    }
}

// ---- bring-up ---------------------------------------------------------------

/// Explicit bring-up parameters, for tests and embedders; the production
/// path (`Platform::new`) fills them from the environment.
pub struct TermSetup {
    /// Pixel protocol to use.
    pub protocol: TermProtocol,
    /// Ask the terminal for its pixel size (`CSI 14 t`) when the kernel
    /// doesn't report one. Off in tests (nothing answers a pty).
    pub query_pixel_size: bool,
    /// Register the terminal with the global panic/signal restore hooks.
    /// On for real apps; off in tests so parallel guards don't share statics.
    pub global_restore: bool,
    /// Fallback cell size in pixels when nothing reports one (kitty mode).
    pub fallback_cell_px: (u32, u32),
}

impl Default for TermSetup {
    fn default() -> Self {
        TermSetup {
            protocol: detect_protocol(),
            query_pixel_size: true,
            global_restore: true,
            fallback_cell_px: (8, 16),
        }
    }
}

/// Open the process's controlling terminal as a display + input pair.
pub(crate) fn open_pair() -> Result<(TermDisplay, TermInput)> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .map_err(|e| Error::device("/dev/tty", e))?;
    open_pair_on(OwnedFd::from(file), &TermSetup::default())
}

/// Open the terminal on an explicit fd (a pty in tests, a serial-attached
/// terminal, …). The fd must be a tty; it is used for both output and input.
pub fn open_pair_on(tty: OwnedFd, setup: &TermSetup) -> Result<(TermDisplay, TermInput)> {
    let fd = Arc::new(tty);
    let pixel_mouse = setup.protocol == TermProtocol::Kitty;
    let guard = TtyGuard::acquire(fd.clone(), pixel_mouse, setup.global_restore)?;

    let ws = winsize(fd.as_raw_fd())?;
    let mut stash = Vec::new();
    let cell_px = match setup.protocol {
        TermProtocol::Cells => (1, 2),
        TermProtocol::Kitty => {
            let from_kernel = (ws.x_px > 0 && ws.y_px > 0).then_some((ws.x_px, ws.y_px));
            let px = from_kernel.or_else(|| {
                if setup.query_pixel_size {
                    query_pixel_size(fd.as_raw_fd(), &mut stash)
                } else {
                    None
                }
            });
            match px {
                Some((w, h)) => ((w / ws.cols).max(1), (h / ws.rows).max(1)),
                None => setup.fallback_cell_px,
            }
        }
    };

    // Anything typed during the bring-up query belongs to the input stream;
    // an armed eventfd makes the event loop deliver it immediately instead of
    // waiting for the next keystroke to make the tty readable.
    let input_wake = if stash.is_empty() {
        None
    } else {
        // SAFETY: creating a fresh eventfd we own, pre-armed with a count of 1.
        let efd = unsafe { libc::eventfd(1, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if efd < 0 {
            None // no wake fd: the stash still drains on the next tty input
        } else {
            // SAFETY: `efd` is a fresh fd we own.
            Some(unsafe { std::os::unix::io::FromRawFd::from_raw_fd(efd) })
        }
    };

    let shared = Arc::new(Shared {
        fd,
        mouse_pixels: AtomicBool::new(pixel_mouse),
        cell_w: AtomicU32::new(cell_px.0),
        cell_h: AtomicU32::new(cell_px.1),
        pending_input: Mutex::new(stash),
        input_wake,
    });

    let display = TermDisplay::new(shared.clone(), guard, setup.protocol, ws);
    let input = TermInput::new(shared);
    Ok((display, input))
}

/// The no-op seat for the terminal backend: there are no device nodes to
/// broker and no session changes — the terminal emulator is the session.
pub(crate) struct TermSeat;

impl crate::seat::Seat for TermSeat {
    fn name(&self) -> &str {
        "term"
    }
    fn open_device(&mut self, path: &std::path::Path) -> Result<OwnedFd> {
        Err(Error::unsupported(format!(
            "terminal backend has no devices to open ({})",
            path.display()
        )))
    }
    fn close_device(&mut self, _fd: OwnedFd) {}
    fn session_fd(&self) -> Option<RawFd> {
        None
    }
    fn dispatch(&mut self, _sink: &mut dyn FnMut(crate::seat::SessionEvent)) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_report_parses_and_splits_stream() {
        let buf = b"abc\x1b[4;600;800tdef";
        let ((s, e), h, w) = parse_pixel_report(buf).unwrap();
        assert_eq!((h, w), (600, 800));
        assert_eq!(&buf[..s], b"abc");
        assert_eq!(&buf[e..], b"def");
    }

    #[test]
    fn pixel_report_incomplete_returns_none() {
        assert!(parse_pixel_report(b"\x1b[4;600;8").is_none());
        assert!(parse_pixel_report(b"junk with no report").is_none());
    }

    #[test]
    fn restore_is_idempotent_without_a_tty() {
        TERM_FD.store(-1, Ordering::SeqCst);
        TERM_ACTIVE.store(true, Ordering::SeqCst);
        restore_terminal();
        assert!(!TERM_ACTIVE.load(Ordering::SeqCst));
        restore_terminal(); // second run: no-op
        assert!(!TERM_ACTIVE.load(Ordering::SeqCst));
    }
}
