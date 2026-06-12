//! VT lifecycle: own the console safely, hand it back on every exit path, and
//! switch cooperatively.
//!
//! Two responsibilities, both first-class because everyone gets them wrong:
//!
//! 1. **Console guard** (inherited from the Phase 0 spike): put the owned VT in
//!    `KD_GRAPHICS` and mute the keyboard (`K_OFF`), and restore `KD_TEXT` +
//!    the saved keyboard mode on *every* exit — normal drop, `panic!`, and
//!    fatal/termination signals. A crashed fullscreen app must never leave the
//!    console dead.
//!
//! 2. **Cooperative switching** (new in Phase 1): under `noseat` we are the one
//!    who must mediate Ctrl-Alt-Fn. We ask the kernel for `VT_PROCESS` mode with
//!    a release signal and an acquire signal; the kernel then *asks permission*
//!    before switching away. The signal handlers are async-signal-safe — they
//!    only write one byte to a self-pipe — and the event loop turns those bytes
//!    into [`VtEvent`]s: on [`Release`] we drop DRM master, stop rendering, and
//!    [`ack_release`]; on [`Acquire`] we [`ack_acquire`], re-acquire master, and
//!    force a full redraw.
//!
//! (When a seat manager is in charge — the `libseat` backend — *it* mediates VT
//! switching and reports it as [`SessionEvent`](crate::seat::SessionEvent)
//! instead; this module's switching half is then unused.)
//!
//! [`Release`]: VtEvent::Release
//! [`Acquire`]: VtEvent::Acquire
//! [`ack_release`]: VtGuard::ack_release
//! [`ack_acquire`]: VtGuard::ack_acquire

use std::io;
use std::os::unix::io::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Once;

use crate::ioctl::*;

static TTY_FD: AtomicI32 = AtomicI32::new(-1);
static SAVED_KB_MODE: AtomicI32 = AtomicI32::new(-1);
static SAVED_KD_MODE: AtomicI32 = AtomicI32::new(-1);
static GUARD_ACTIVE: AtomicBool = AtomicBool::new(false);
/// True once we've put the VT in `VT_PROCESS` mode, so restore returns it to
/// `VT_AUTO`.
static SWITCHING_ENABLED: AtomicBool = AtomicBool::new(false);
/// Write end of the self-pipe the VT signal handlers poke. -1 when disabled.
static SWITCH_PIPE_W: AtomicI32 = AtomicI32::new(-1);
static HOOKS_INSTALLED: Once = Once::new();

/// Signals we ask the kernel to raise for VT release / acquire. `SIGUSR1` and
/// `SIGUSR2` are the conventional choice (logind uses them too).
const RELSIG: libc::c_int = libc::SIGUSR1;
const ACQSIG: libc::c_int = libc::SIGUSR2;

/// A cooperative-switch request from the kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VtEvent {
    /// The kernel wants to switch away; release master and `ack_release`.
    Release,
    /// We've been switched back to; `ack_acquire` and redraw.
    Acquire,
}

/// Restore the console to text mode + the saved keyboard mode (and `VT_AUTO` if
/// we'd taken over switching).
///
/// Async-signal-safe: reads only atomics and issues raw `ioctl`s. Idempotent —
/// the `GUARD_ACTIVE` swap means a concurrent Drop + signal run it once.
fn restore_console() {
    if !GUARD_ACTIVE.swap(false, Ordering::SeqCst) {
        return;
    }
    let fd = TTY_FD.load(Ordering::SeqCst);
    if fd < 0 {
        return;
    }
    let kb = SAVED_KB_MODE.load(Ordering::SeqCst);
    let kd = SAVED_KD_MODE.load(Ordering::SeqCst);
    // SAFETY: raw ioctls on a saved fd; all async-signal-safe syscalls.
    unsafe {
        if SWITCHING_ENABLED.load(Ordering::SeqCst) {
            // Return switching control to the kernel so the console stays usable.
            let mut auto = VtMode {
                mode: VT_AUTO,
                ..VtMode::default()
            };
            libc::ioctl(fd, VT_SETMODE as _, &mut auto as *mut VtMode);
        }
        let kd = if kd < 0 { KD_TEXT } else { kd };
        libc::ioctl(fd, KDSETMODE as _, kd);
        if kb >= 0 {
            libc::ioctl(fd, KDSKBMODE as _, kb);
        }
    }
}

extern "C" fn signal_restore(sig: libc::c_int) {
    restore_console();
    // Re-raise with the default disposition so exit status / core dump is what
    // the user expects.
    unsafe {
        libc::signal(sig, libc::SIG_DFL);
        libc::raise(sig);
    }
}

/// VT release/acquire handler: write a single byte to the self-pipe. Nothing
/// else — all the real work happens in the event loop when it reads the byte.
extern "C" fn vt_switch_signal(sig: libc::c_int) {
    let byte: u8 = if sig == RELSIG { b'R' } else { b'A' };
    let w = SWITCH_PIPE_W.load(Ordering::SeqCst);
    if w >= 0 {
        // SAFETY: `write` is async-signal-safe; one byte to our own pipe.
        unsafe {
            libc::write(w, &byte as *const u8 as *const libc::c_void, 1);
        }
    }
}

fn install_hooks_once() {
    HOOKS_INSTALLED.call_once(|| {
        // Panic hook: restore first, then run the previous hook so the backtrace
        // prints to the now-readable console.
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_console();
            prev(info);
        }));

        for sig in [
            libc::SIGINT,
            libc::SIGTERM,
            libc::SIGHUP,
            libc::SIGSEGV,
            libc::SIGABRT,
            libc::SIGILL,
            libc::SIGBUS,
            libc::SIGFPE,
        ] {
            // SAFETY: installing a minimal async-signal-safe handler.
            unsafe {
                libc::signal(sig, signal_restore as *const () as usize);
            }
        }
    });
}

/// Discover the currently-active VT number (best effort, for logging).
pub fn active_vt(fd: RawFd) -> Option<u16> {
    let mut st = VtStat::default();
    // SAFETY: VT_GETSTATE writes a `vt_stat` through the pointer.
    unsafe { ioctl_ptr(fd, VT_GETSTATE, &mut st).ok()? };
    Some(st.v_active)
}

/// RAII handle owning the console's graphics mode + muted keyboard, and
/// (optionally) cooperative VT switching.
pub struct VtGuard {
    fd: RawFd,
    active: bool,
    /// Read end of the switch self-pipe, if switching is enabled.
    switch_pipe_r: Option<OwnedFd>,
}

impl VtGuard {
    /// Take the console: open `tty_path`, save its modes, switch to
    /// `KD_GRAPHICS`, and mute the keyboard. Hooks are installed before the
    /// switch so a crash during setup still restores.
    pub fn acquire(tty_path: &str) -> io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(tty_path)?;
        let fd = file.into_raw_fd();

        let kd = ioctl_get_int(fd, KDGETMODE).unwrap_or(KD_TEXT);
        let kb = ioctl_get_int(fd, KDGKBMODE).unwrap_or(K_UNICODE);

        if let Some(vt) = active_vt(fd) {
            eprintln!("[vt] acquiring console on VT {vt} (fd {fd})");
        }

        TTY_FD.store(fd, Ordering::SeqCst);
        SAVED_KB_MODE.store(kb, Ordering::SeqCst);
        SAVED_KD_MODE.store(kd, Ordering::SeqCst);
        install_hooks_once();
        GUARD_ACTIVE.store(true, Ordering::SeqCst);

        ioctl_val(fd, KDSETMODE, KD_GRAPHICS).inspect_err(|_| restore_console())?;
        if let Err(e) = ioctl_val(fd, KDSKBMODE, K_OFF) {
            eprintln!("[vt] warning: could not mute keyboard (K_OFF): {e}");
        }

        eprintln!("[vt] console in KD_GRAPHICS, keyboard muted (K_OFF)");
        Ok(VtGuard {
            fd,
            active: true,
            switch_pipe_r: None,
        })
    }

    /// A no-op guard (for serial/pty/SSH where the KD ioctls fail with ENOTTY).
    pub fn disabled() -> Self {
        eprintln!("[vt] guard disabled (no console mode switch)");
        VtGuard {
            fd: -1,
            active: false,
            switch_pipe_r: None,
        }
    }

    /// Whether this guard actually owns a console (vs. [`disabled`]).
    ///
    /// [`disabled`]: VtGuard::disabled
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Enable cooperative VT switching: ask the kernel for `VT_PROCESS` mode so
    /// it signals us (rather than yanking the console) on Ctrl-Alt-Fn. Returns
    /// the read end of the self-pipe to register with the event loop; drive it
    /// with [`drain_switches`](VtGuard::drain_switches).
    pub fn enable_switching(&mut self) -> io::Result<RawFd> {
        if !self.active {
            return Err(io::Error::other("VT switching needs an active guard"));
        }
        // Self-pipe: nonblocking + close-on-exec.
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: writing two ints into a local array.
        let r = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) };
        if r < 0 {
            return Err(io::Error::last_os_error());
        }
        let (read_fd, write_fd) = (fds[0], fds[1]);
        SWITCH_PIPE_W.store(write_fd, Ordering::SeqCst);

        // SAFETY: minimal async-signal-safe handlers for the two VT signals.
        unsafe {
            libc::signal(RELSIG, vt_switch_signal as *const () as usize);
            libc::signal(ACQSIG, vt_switch_signal as *const () as usize);
        }

        let mut mode = VtMode {
            mode: VT_PROCESS,
            waitv: 0,
            relsig: RELSIG as libc::c_short,
            acqsig: ACQSIG as libc::c_short,
            frsig: 0,
        };
        // SAFETY: VT_SETMODE takes a vt_mode struct.
        let r = unsafe { ioctl_ptr(self.fd, VT_SETMODE, &mut mode) };
        if let Err(e) = r {
            SWITCH_PIPE_W.store(-1, Ordering::SeqCst);
            // SAFETY: closing the pipe ends we just made.
            unsafe {
                libc::close(read_fd);
                libc::close(write_fd);
            }
            return Err(e);
        }
        SWITCHING_ENABLED.store(true, Ordering::SeqCst);
        // SAFETY: `read_fd` is a fresh fd we own.
        self.switch_pipe_r = Some(unsafe { OwnedFd::from_raw_fd(read_fd) });
        eprintln!("[vt] cooperative switching enabled (VT_PROCESS)");
        Ok(read_fd)
    }

    /// Read end of the switch self-pipe, for the event loop to poll.
    pub fn switch_fd(&self) -> Option<RawFd> {
        self.switch_pipe_r.as_ref().map(|f| f.as_raw_fd())
    }

    /// Drain pending switch signals into `sink`. Call after [`switch_fd`] reads.
    ///
    /// [`switch_fd`]: VtGuard::switch_fd
    pub fn drain_switches(&mut self, mut sink: impl FnMut(VtEvent)) -> io::Result<()> {
        let Some(fd) = self.switch_pipe_r.as_ref().map(|f| f.as_raw_fd()) else {
            return Ok(());
        };
        let mut buf = [0u8; 32];
        loop {
            // SAFETY: reading into a stack buffer from our nonblocking pipe.
            let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if n > 0 {
                for &b in &buf[..n as usize] {
                    match b {
                        b'R' => sink(VtEvent::Release),
                        b'A' => sink(VtEvent::Acquire),
                        _ => {}
                    }
                }
                if (n as usize) < buf.len() {
                    break;
                }
            } else if n == 0 {
                break;
            } else {
                let e = io::Error::last_os_error();
                return match e.raw_os_error() {
                    Some(libc::EAGAIN) | Some(libc::EINTR) => Ok(()),
                    _ => Err(e),
                };
            }
        }
        Ok(())
    }

    /// Acknowledge a [`VtEvent::Release`]: tell the kernel it may complete the
    /// switch away. Call *after* dropping DRM master and stopping rendering.
    pub fn ack_release(&self) -> io::Result<()> {
        ioctl_val(self.fd, VT_RELDISP, VT_RELDISP_RELEASE_OK)
    }

    /// Acknowledge a [`VtEvent::Acquire`]: accept the console back. Call before
    /// re-acquiring master and redrawing.
    pub fn ack_acquire(&self) -> io::Result<()> {
        ioctl_val(self.fd, VT_RELDISP, VT_ACKACQ)
    }

    fn human_kb(mode: i32) -> &'static str {
        match mode {
            K_RAW => "K_RAW",
            K_XLATE => "K_XLATE",
            K_MEDIUMRAW => "K_MEDIUMRAW",
            K_UNICODE => "K_UNICODE",
            K_OFF => "K_OFF",
            _ => "K_?",
        }
    }
}

impl Drop for VtGuard {
    fn drop(&mut self) {
        if self.active {
            let kb = SAVED_KB_MODE.load(Ordering::SeqCst);
            eprintln!("[vt] restoring console: KD_TEXT + {}", Self::human_kb(kb));
        }
        // Tear down the switch self-pipe write end so no stale handler writes.
        let w = SWITCH_PIPE_W.swap(-1, Ordering::SeqCst);
        if w >= 0 {
            // SAFETY: closing our own pipe write end.
            unsafe {
                libc::close(w);
            }
        }
        restore_console();
        let _ = self.fd;
    }
}
