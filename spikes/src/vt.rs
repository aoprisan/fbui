//! VT guard: own the console safely and *always* hand it back.
//!
//! When a fullscreen app draws to KMS/fbdev it must put its VT into
//! `KD_GRAPHICS` (so the kernel text console stops scribbling over the screen)
//! and mute the keyboard (`K_OFF`, so keystrokes don't echo onto the dead
//! console or get interpreted by the shell behind us). The cardinal rule from
//! the plan: a crashed fullscreen app must **never** leave the console dead.
//!
//! So restoration has to happen on every exit path:
//!   * normal teardown            -> `Drop`
//!   * `panic!`                   -> panic hook
//!   * `SIGINT` / `SIGTERM` / `SIGSEGV` / ... -> signal handler
//!
//! The signal handler can only call async-signal-safe functions, so all the
//! state it needs (the tty fd and the saved keyboard mode) lives in atomics,
//! and restoration is nothing but two raw `ioctl`s.

use std::io;
use std::os::unix::io::{IntoRawFd, RawFd};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Once;

use crate::ioctl::*;

static TTY_FD: AtomicI32 = AtomicI32::new(-1);
static SAVED_KB_MODE: AtomicI32 = AtomicI32::new(-1);
static SAVED_KD_MODE: AtomicI32 = AtomicI32::new(-1);
static GUARD_ACTIVE: AtomicBool = AtomicBool::new(false);
static HOOKS_INSTALLED: Once = Once::new();

/// Restore the console to text mode + the saved keyboard mode.
///
/// Async-signal-safe: reads only atomics and issues raw `ioctl`s. Idempotent —
/// the `GUARD_ACTIVE` swap means concurrent Drop + signal paths run it once.
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
    // SAFETY: raw ioctls on a saved fd; both are async-signal-safe syscalls.
    unsafe {
        let kd = if kd < 0 { KD_TEXT } else { kd };
        libc::ioctl(fd, KDSETMODE as _, kd);
        if kb >= 0 {
            libc::ioctl(fd, KDSKBMODE as _, kb);
        }
    }
}

extern "C" fn signal_restore(sig: libc::c_int) {
    restore_console();
    // Re-raise with the default handler so the exit status / core dump is
    // what the user expects.
    unsafe {
        libc::signal(sig, libc::SIG_DFL);
        libc::raise(sig);
    }
}

fn install_hooks_once() {
    HOOKS_INSTALLED.call_once(|| {
        // Panic hook: restore first, then run the previous hook so the
        // backtrace still prints to the (now-readable) console.
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_console();
            prev(info);
        }));

        // Signals that would otherwise abort us with the console wedged.
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

/// RAII handle owning the console's graphics mode + muted keyboard.
pub struct VtGuard {
    fd: RawFd,
    active: bool,
}

impl VtGuard {
    /// Take the console: open `tty_path` (the controlling terminal of the VT
    /// we run on), save its modes, switch to `KD_GRAPHICS`, and mute the
    /// keyboard with `K_OFF`. Hooks are installed before the switch so a crash
    /// during setup still restores.
    pub fn acquire(tty_path: &str) -> io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(tty_path)?;
        let fd = file.into_raw_fd();

        // Save current modes so we restore exactly what was there.
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

        // Order matters: graphics mode first, then mute the keyboard. If the
        // keyboard mute fails we still want graphics + a restorable guard.
        ioctl_val(fd, KDSETMODE, KD_GRAPHICS).inspect_err(|_| restore_console())?;
        if let Err(e) = ioctl_val(fd, KDSKBMODE, K_OFF) {
            eprintln!("[vt] warning: could not mute keyboard (K_OFF): {e}");
        }

        eprintln!("[vt] console in KD_GRAPHICS, keyboard muted (K_OFF)");
        Ok(VtGuard { fd, active: true })
    }

    /// Construct a no-op guard (for `--no-vt-guard`, e.g. running over a serial
    /// console or pty where the KD ioctls would fail with ENOTTY).
    pub fn disabled() -> Self {
        eprintln!("[vt] guard disabled (no console mode switch)");
        VtGuard { fd: -1, active: false }
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
            eprintln!(
                "[vt] restoring console: KD_TEXT + {}",
                Self::human_kb(kb)
            );
        }
        restore_console();
        let _ = self.fd;
    }
}
