//! DRM/KMS dumb-buffer backend: the primary path.
//!
//! Flow (Phase 0 task 1+2+3):
//!   1. Open the card, enumerate connectors, pick the connected one and its
//!      preferred mode, resolve encoder -> CRTC.
//!   2. Allocate **two** XRGB8888 dumb buffers, modeset onto the front one,
//!      then drive an animated scene by page-flipping with `EVENT` and blocking
//!      on the DRM fd until the flip completes (that block *is* the vsync).
//!   3. Render into a normal-RAM shadow and row-copy into the dumb mapping
//!      (default), or `--direct` to write straight into the mapping for
//!      comparison.

use std::io;
use std::os::unix::io::AsRawFd;
use std::time::Duration;

use drm::buffer::{Buffer, DrmFourcc};
use drm::control::{connector, crtc, framebuffer, Device as ControlDevice, ModeTypeFlags};
use drm::control::{PageFlipFlags};

use crate::card::Card;
use crate::scene::{render_bars_direct_xrgb8888, Shadow, Timing};
use crate::Args;
use crate::RUNNING;

/// A dumb buffer mapped once for the lifetime of the run.
struct MappedBuffer {
    db: drm::control::dumbbuffer::DumbBuffer,
    fb: framebuffer::Handle,
    ptr: *mut u8,
    len: usize,
    pitch: usize,
}

impl MappedBuffer {
    fn create(card: &Card, w: u32, h: u32) -> io::Result<Self> {
        let mut db = card
            .create_dumb_buffer((w, h), DrmFourcc::Xrgb8888, 32)
            .map_err(io::Error::other)?;
        let fb = card
            .add_framebuffer(&db, 24, 32)
            .map_err(io::Error::other)?;
        let pitch = db.pitch() as usize;
        // Map once and keep the raw pointer; we forget the RAII mapping and
        // munmap by hand at teardown so per-frame timing isn't polluted by an
        // mmap/munmap syscall pair.
        let mut map = card.map_dumb_buffer(&mut db).map_err(io::Error::other)?;
        let slice = map.as_mut();
        let ptr = slice.as_mut_ptr();
        let len = slice.len();
        std::mem::forget(map);
        Ok(MappedBuffer {
            db,
            fb,
            ptr,
            len,
            pitch,
        })
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: `ptr`/`len` came from a valid mapping that outlives `self`
        // (we only munmap in `destroy`). Exclusive via `&mut self`.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }

    fn destroy(self, card: &Card) {
        // SAFETY: unmap the region we forgot earlier, then drop kernel objects.
        unsafe {
            libc::munmap(self.ptr as *mut libc::c_void, self.len);
        }
        let _ = card.destroy_framebuffer(self.fb);
        let _ = card.destroy_dumb_buffer(self.db);
    }
}

/// Pick the CRTC for a connector: prefer the one the current encoder already
/// drives, otherwise the first encoder's `possible_crtcs`, otherwise crtc[0].
fn pick_crtc(
    card: &Card,
    res: &drm::control::ResourceHandles,
    con: &connector::Info,
) -> io::Result<crtc::Handle> {
    // Current encoder already bound?
    if let Some(enc) = con.current_encoder() {
        if let Ok(info) = card.get_encoder(enc) {
            if let Some(c) = info.crtc() {
                return Ok(c);
            }
        }
    }
    // Otherwise, walk this connector's candidate encoders and resolve their
    // possible_crtcs mask against the card's crtc list.
    for &enc in con.encoders() {
        if let Ok(info) = card.get_encoder(enc) {
            if let Some(&c) = res.filter_crtcs(info.possible_crtcs()).first() {
                return Ok(c);
            }
        }
    }
    res.crtcs()
        .first()
        .copied()
        .ok_or_else(|| io::Error::other("no CRTCs on this card"))
}

/// Block on the DRM fd until a page-flip-complete event arrives (or timeout),
/// draining the event queue. Returns `true` if a flip completed.
fn wait_for_flip(card: &Card, timeout: Duration) -> io::Result<bool> {
    let mut pfd = libc::pollfd {
        fd: card.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
    // SAFETY: single valid pollfd.
    let r = unsafe { libc::poll(&mut pfd, 1, ms) };
    if r < 0 {
        let e = io::Error::last_os_error();
        // EINTR (our own signal handler) is not a failure; just retry/exit.
        if e.raw_os_error() == Some(libc::EINTR) {
            return Ok(false);
        }
        return Err(e);
    }
    if r == 0 {
        return Ok(false); // timeout
    }
    let mut flipped = false;
    for ev in card.receive_events().map_err(io::Error::other)? {
        if let drm::control::Event::PageFlip(_) = ev {
            flipped = true;
        }
    }
    Ok(flipped)
}

pub fn run(args: &Args) -> io::Result<()> {
    let card = Card::open(&args.device)?;

    let res = card.resource_handles().map_err(io::Error::other)?;

    // 1. Connected connector + preferred mode.
    let con = res
        .connectors()
        .iter()
        .flat_map(|&c| card.get_connector(c, true))
        .find(|i| i.state() == connector::State::Connected)
        .ok_or_else(|| io::Error::other("no connected connector"))?;

    let mode = con
        .modes()
        .iter()
        .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
        .or_else(|| con.modes().first())
        .copied()
        .ok_or_else(|| io::Error::other("connector has no modes"))?;

    let (w, h) = mode.size();
    let (w, h) = (w as u32, h as u32);
    let crtc = pick_crtc(&card, &res, &con)?;

    eprintln!(
        "[drm] {} {:?} on {:?}: {}x{} @ {} Hz (crtc {:?})",
        args.device,
        con.interface(),
        con.handle(),
        w,
        h,
        mode.vrefresh(),
        crtc,
    );
    eprintln!(
        "[drm] path: {}",
        if args.direct {
            "DIRECT (render into write-combined mapping)"
        } else {
            "SHADOW (render to RAM, row-copy into mapping)"
        }
    );

    // 2. Two dumb buffers.
    let mut buffers = [
        MappedBuffer::create(&card, w, h)?,
        MappedBuffer::create(&card, w, h)?,
    ];
    eprintln!(
        "[drm] two XRGB8888 dumb buffers: {}x{}, pitch {} bytes ({} KiB each)",
        w,
        h,
        buffers[0].pitch,
        buffers[0].len / 1024
    );

    // Modeset onto buffer 0 (this requires DRM master: root or an active seat).
    card.set_crtc(crtc, Some(buffers[0].fb), (0, 0), &[con.handle()], Some(mode))
        .map_err(|e| {
            io::Error::other(format!(
                "set_crtc failed ({e}) — need DRM master (run as root/on an active VT)"
            ))
        })?;

    let mut shadow = Shadow::new(w as usize, h as usize);
    let mut timing = Timing::default();
    timing.start();

    // Buffer 0 is on screen; render into 1, flip, swap, repeat.
    let mut back = 1usize;
    let mut frame: u64 = 0;
    let frame_timeout = Duration::from_millis(200);

    while RUNNING.load(std::sync::atomic::Ordering::Relaxed) {
        if let Some(secs) = args.seconds {
            let elapsed = timing.started.map(|s| s.elapsed().as_secs_f64()).unwrap_or(0.0);
            if elapsed >= secs {
                break;
            }
        }

        let (r, b) = {
            let buf = &mut buffers[back];
            let pitch = buf.pitch;
            let dst = buf.as_mut_slice();
            if args.direct {
                let r = render_bars_direct_xrgb8888(dst, pitch, w as usize, h as usize, frame);
                (r, Duration::ZERO)
            } else {
                let r = shadow.render_bars(frame);
                let b = shadow.blit_xrgb8888(dst, pitch);
                (r, b)
            }
        };
        timing.record(r, b);

        // Queue the flip and wait for vblank completion before touching the
        // (now-front) buffer again. This block is the frame pacing: an idle
        // app spends its time asleep in poll().
        card.page_flip(crtc, buffers[back].fb, PageFlipFlags::EVENT, None)
            .map_err(io::Error::other)?;

        // Optional crash test: panic mid-run to prove the console restores.
        if let Some(pf) = args.panic_after {
            if frame >= pf {
                panic!("[drm] deliberate panic after {pf} frames (console-restore test)");
            }
        }

        // Drain until *our* flip completes (or we're asked to stop).
        loop {
            if !RUNNING.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            match wait_for_flip(&card, frame_timeout)? {
                true => break,
                false => continue,
            }
        }

        back ^= 1;
        frame += 1;
    }

    timing.report(if args.direct { "drm/direct" } else { "drm/shadow" });

    for buf in buffers {
        buf.destroy(&card);
    }
    Ok(())
}
