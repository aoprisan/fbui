//! fbui Phase 0 — kernel-facing spike.
//!
//! A throwaway binary that de-risks the plumbing the rest of the framework will
//! depend on, *before* any API is designed:
//!
//!   * DRM/KMS dumb-buffer double-buffering with vsynced page flips,
//!   * shadow-buffer discipline (render to RAM, row-copy into the device
//!     mapping) vs. the naive direct-write path, with timing for both,
//!   * an RAII VT guard that puts the console in graphics mode and mutes the
//!     keyboard, and restores it on *every* exit path (drop, panic, signal),
//!   * the legacy fbdev fallback and its stride/format quirks.
//!
//! Run on a real Linux VT (switch to a text console, log in, run as root or on
//! an active seat). See `../NOTES.md` for the per-target test matrix.
//!
//! Usage:
//!   fbui-spike [drm|fbdev] [options]
//!     --device <path>     card/fb node (default /dev/dri/card0 or /dev/fb0)
//!     --seconds <n>       run for n seconds then exit cleanly (default 8; 0 = forever)
//!     --direct            (drm) render straight into the WC mapping, no shadow
//!     --no-vt-guard       don't touch console mode (for serial/pty/SSH testing)
//!     --panic-after <n>   panic after n frames — proves the console restores

mod card;
mod drm_backend;
mod fbdev;
mod ioctl;
mod scene;
mod vt;

use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

/// Cleared by the signal handler in `vt` would be ideal, but that handler is
/// kept minimal; instead the backends poll this flag for cooperative shutdown
/// and we also install a SIGINT/SIGTERM handler here that flips it.
pub static RUNNING: AtomicBool = AtomicBool::new(true);

#[derive(Debug)]
pub struct Args {
    pub backend: Backend,
    pub device: String,
    pub seconds: Option<f64>,
    pub direct: bool,
    pub vt_guard: bool,
    pub panic_after: Option<u64>,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Backend {
    Drm,
    Fbdev,
}

fn parse_args() -> Result<Args, String> {
    let mut backend = Backend::Drm;
    let mut device: Option<String> = None;
    let mut seconds: Option<f64> = Some(8.0);
    let mut direct = false;
    let mut vt_guard = true;
    let mut panic_after = None;

    let mut it = std::env::args().skip(1).peekable();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "drm" => backend = Backend::Drm,
            "fbdev" => backend = Backend::Fbdev,
            "--device" => {
                device = Some(it.next().ok_or("--device needs a value")?);
            }
            "--seconds" => {
                let v: f64 = it
                    .next()
                    .ok_or("--seconds needs a value")?
                    .parse()
                    .map_err(|_| "bad --seconds")?;
                seconds = if v <= 0.0 { None } else { Some(v) };
            }
            "--direct" => direct = true,
            "--no-vt-guard" => vt_guard = false,
            "--panic-after" => {
                let v: u64 = it
                    .next()
                    .ok_or("--panic-after needs a value")?
                    .parse()
                    .map_err(|_| "bad --panic-after")?;
                panic_after = Some(v);
            }
            "-h" | "--help" => return Err("help".into()),
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    let device = device.unwrap_or_else(|| match backend {
        Backend::Drm => "/dev/dri/card0".into(),
        Backend::Fbdev => "/dev/fb0".into(),
    });

    Ok(Args {
        backend,
        device,
        seconds,
        direct,
        vt_guard,
        panic_after,
    })
}

fn usage() {
    eprint!(
        "fbui Phase 0 spike\n\
         \n\
         USAGE: fbui-spike [drm|fbdev] [options]\n\
         \n\
         OPTIONS:\n  \
           --device <path>     card/fb node (default /dev/dri/card0 or /dev/fb0)\n  \
           --seconds <n>       run n seconds then exit (default 8; 0 = forever)\n  \
           --direct            (drm) render into the WC mapping, skip the shadow\n  \
           --no-vt-guard       leave console mode alone (serial/pty/SSH)\n  \
           --panic-after <n>   panic after n frames (console-restore test)\n  \
           -h, --help          this message\n"
    );
}

/// Flip `RUNNING` on Ctrl-C / SIGTERM for a clean shutdown + timing report.
/// (The `vt` module additionally restores the console from these signals.)
extern "C" fn request_stop(_sig: libc::c_int) {
    RUNNING.store(false, Ordering::SeqCst);
}

fn install_stop_handler() {
    // Chain after vt's restore handler? No — keep it simple: vt installs the
    // restore-and-reraise handler for hard signals; here we only want a *soft*
    // stop for SIGINT/SIGTERM so the run ends cleanly and prints timing. We
    // install ours and let it win for these two; on a muted-keyboard console
    // these only arrive via `kill`, which is the intended remote-stop path.
    //
    // NOTE: this means for SIGINT/SIGTERM we do a graceful stop (no re-raise);
    // the VtGuard's Drop restores the console as the run unwinds normally.
    unsafe {
        libc::signal(libc::SIGINT, request_stop as *const () as usize);
        libc::signal(libc::SIGTERM, request_stop as *const () as usize);
    }
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            if e != "help" {
                eprintln!("error: {e}\n");
            }
            usage();
            return if e == "help" {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(2)
            };
        }
    };

    eprintln!("fbui-spike: backend={:?} device={}", args.backend, args.device);

    // Acquire the console first (installs panic + hard-signal restore hooks),
    // then our soft-stop handler for SIGINT/SIGTERM.
    let _guard = if args.vt_guard {
        match vt::VtGuard::acquire("/dev/tty") {
            Ok(g) => g,
            Err(e) => {
                eprintln!(
                    "[vt] could not acquire console on /dev/tty ({e}); \
                     continuing without guard (use --no-vt-guard to silence)."
                );
                vt::VtGuard::disabled()
            }
        }
    } else {
        vt::VtGuard::disabled()
    };
    install_stop_handler();

    let result = match args.backend {
        Backend::Drm => drm_backend::run(&args),
        Backend::Fbdev => fbdev::run(&args),
    };

    // Drop the guard explicitly *before* reporting the error so the console is
    // readable when the message prints.
    drop(_guard);

    match result {
        Ok(()) => {
            eprintln!("fbui-spike: done.");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("fbui-spike: error: {e}");
            ExitCode::FAILURE
        }
    }
}
