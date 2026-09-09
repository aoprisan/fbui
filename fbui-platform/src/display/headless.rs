//! A display that presents nowhere: two RAM back buffers behind the same
//! [`Display`] trait, for CI, agents, and anyone driving the app without a
//! screen (`FBUI_BACKEND=headless`).
//!
//! The point is **not** a test double. It is to run the *same* `fbui::run` —
//! frame clock, gesture recognizer, timers, `Proxy`, power policy,
//! record/replay, monkey, remote console — with the display replaced, so a
//! headless run is evidence about the real app rather than about a stand-in.
//! The terminal backend was meant to be that path, but `TtyGuard::acquire`
//! (correctly) refuses a non-tty, so CI could never actually run a replay.
//!
//! Three details keep it honest rather than merely convenient:
//!
//! * **Two buffers with real ages.** Presenting alternates between them and
//!   accounts [`Frame::age`] exactly the way the DRM backend does, so the
//!   partial-redraw path is what runs headless — not just the `age = 0`
//!   repaint-everything path a single buffer would force.
//! * **A padded stride.** The rows are padded to a 64-byte multiple, so
//!   `stride != width * bpp` and any code that recomputes the stride (the one
//!   thing the whole stack promises never to do) corrupts its output loudly
//!   in CI instead of quietly on a device whose pitch happens to be padded.
//! * **Presents complete synchronously.** A buffer is always free, there is
//!   no fd to poll, and the event loop needs no pacing timer — so an idle
//!   headless app blocks in `poll` and burns ~0% CPU, like the device
//!   backends.
//!
//! Hotplug is simulatable: `SIGUSR1` (or [`HeadlessDisplay::request_mode`])
//! makes the next [`reconfigure`](Display::reconfigure) report a new mode and
//! reallocate, which drives `on_display_changed` — today reachable only on
//! VKMS.

use std::os::unix::io::BorrowedFd;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::{BackendKind, Display, DisplayInfo, Frame};
use crate::error::{Error, Result};
use crate::format::PixelFormat;
use crate::geom::{Rect, Size};

/// Surface size when `FBUI_HEADLESS_SIZE` says nothing: a common small-panel
/// resolution, wide enough that the shipped examples lay out as designed.
pub const DEFAULT_SIZE: Size = Size { w: 1024, h: 600 };

/// Row padding, bytes. Deliberately not the pixel width: see the module docs.
const STRIDE_ALIGN: usize = 64;

/// Set by the `SIGUSR1` handler; drained by [`Display::reconfigure`].
static MODE_CHANGE_REQUESTED: AtomicBool = AtomicBool::new(false);
/// A mode requested out of band, packed `w << 32 | h`; `0` means none.
static MODE_REQUEST: AtomicU64 = AtomicU64::new(0);
/// Whether the `SIGUSR1` handler has been installed (once per process).
static HANDLER_INSTALLED: AtomicBool = AtomicBool::new(false);

/// `SIGUSR1`: flag only. Nothing else here is async-signal-safe.
extern "C" fn on_sigusr1(_sig: libc::c_int) {
    MODE_CHANGE_REQUESTED.store(true, Ordering::Relaxed);
}

fn install_sigusr1() {
    if HANDLER_INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    // SAFETY: installing a handler that only stores to an atomic.
    unsafe {
        libc::signal(libc::SIGUSR1, on_sigusr1 as *const () as libc::sighandler_t);
    }
}

/// Ask the *next* [`reconfigure`](Display::reconfigure) to report `size`,
/// from anywhere in the process (the remote console, a test). `SIGUSR1` is the
/// no-code equivalent, which flips to the portrait swap of the current mode.
pub fn request_mode(size: Size) {
    MODE_REQUEST.store(((size.w as u64) << 32) | size.h as u64, Ordering::SeqCst);
    MODE_CHANGE_REQUESTED.store(true, Ordering::SeqCst);
}

/// Parse a `WxH` size, rejecting zero dimensions.
fn parse_size(s: &str) -> Option<Size> {
    let (w, h) = s.trim().split_once(['x', 'X'])?;
    let (w, h): (u32, u32) = (w.trim().parse().ok()?, h.trim().parse().ok()?);
    (w > 0 && h > 0).then_some(Size::new(w, h))
}

/// The surface size for a headless run: `FBUI_HEADLESS_SIZE` (`WxH`) or
/// [`DEFAULT_SIZE`]. A malformed value is a hard error, like the other
/// `FBUI_*` knobs — a run that silently used the wrong geometry would produce
/// screenshots and flow results that mean nothing.
pub fn size_from_env() -> Result<Size> {
    match std::env::var("FBUI_HEADLESS_SIZE") {
        Err(_) => Ok(DEFAULT_SIZE),
        Ok(s) => parse_size(&s).ok_or_else(|| Error::Io {
            what: format!("FBUI_HEADLESS_SIZE {s:?}"),
            source: std::io::Error::other("expected WxH, e.g. 1024x600"),
        }),
    }
}

/// One RAM back buffer plus the present it last held contents from.
struct Buf {
    pixels: Vec<u8>,
    /// Present index this buffer was last shown at; `None` = never presented,
    /// so its contents are undefined (`age = 0`).
    last_present: Option<u64>,
}

/// A [`Display`] backed by two RAM buffers and presenting to nothing.
pub struct HeadlessDisplay {
    info: DisplayInfo,
    stride: usize,
    buffers: [Buf; 2],
    /// Index of the buffer the caller draws into next.
    back: usize,
    /// Total presents issued — drives buffer-age accounting.
    present_count: u64,
    /// A mode change to report from the next `reconfigure`.
    pending_mode: Option<Size>,
    /// Frames presented so far, for diagnostics.
    presents: u64,
}

impl HeadlessDisplay {
    /// A headless display of `size` in `Xrgb8888`.
    pub fn new(size: Size) -> Self {
        install_sigusr1();
        let stride = stride_for(size.w);
        HeadlessDisplay {
            info: DisplayInfo {
                size,
                format: PixelFormat::Xrgb8888,
                refresh_mhz: 60_000,
                buffers: 2,
                backend: BackendKind::Headless,
            },
            stride,
            buffers: [alloc(stride, size.h), alloc(stride, size.h)],
            back: 0,
            present_count: 0,
            pending_mode: None,
            presents: 0,
        }
    }

    /// A headless display sized by `FBUI_HEADLESS_SIZE`.
    pub fn from_env() -> Result<Self> {
        Ok(Self::new(size_from_env()?))
    }

    /// How many frames have been presented. The headless equivalent of
    /// "did anything reach the screen".
    pub fn presented_frames(&self) -> u64 {
        self.presents
    }

    /// The pixels most recently presented, as `(bytes, stride)` in the
    /// display's format — the closest thing to "what is on the screen".
    pub fn front_buffer(&self) -> (&[u8], usize) {
        // `back` was swapped past the buffer we just presented.
        (&self.buffers[self.back ^ 1].pixels, self.stride)
    }

    /// Ask the next [`reconfigure`](Display::reconfigure) to switch to `size`.
    pub fn request_mode(&mut self, size: Size) {
        self.pending_mode = Some(size);
    }

    /// Resize to `size`: fresh buffers, undefined contents (age 0 next frame).
    fn apply_mode(&mut self, size: Size) {
        self.stride = stride_for(size.w);
        self.buffers = [alloc(self.stride, size.h), alloc(self.stride, size.h)];
        self.back = 0;
        self.info.size = size;
    }
}

fn stride_for(w: u32) -> usize {
    let raw = w as usize * PixelFormat::Xrgb8888.bytes_per_pixel();
    raw.next_multiple_of(STRIDE_ALIGN)
}

fn alloc(stride: usize, h: u32) -> Buf {
    Buf {
        pixels: vec![0u8; stride * h as usize],
        last_present: None,
    }
}

impl Display for HeadlessDisplay {
    fn info(&self) -> DisplayInfo {
        self.info
    }

    fn begin_frame(&mut self) -> Result<Option<Frame<'_>>> {
        let present_count = self.present_count;
        let (size, format, stride) = (self.info.size, self.info.format, self.stride);
        let buf = &mut self.buffers[self.back];
        let age = match buf.last_present {
            Some(p) => (present_count - p) as u32,
            None => 0,
        };
        Ok(Some(Frame {
            buffer: &mut buf.pixels,
            stride,
            size,
            format,
            age,
        }))
    }

    fn present(&mut self, _damage: &[Rect]) -> Result<()> {
        // Nothing to scan out, so the "flip" completes immediately: the other
        // buffer is free at once and the loop never waits on an fd.
        self.buffers[self.back].last_present = Some(self.present_count);
        self.present_count += 1;
        self.presents += 1;
        self.back ^= 1;
        Ok(())
    }

    fn present_fd(&self) -> Option<BorrowedFd<'_>> {
        None
    }

    fn dispatch_present(&mut self) -> Result<bool> {
        Ok(false)
    }

    fn reconfigure(&mut self) -> Result<Option<DisplayInfo>> {
        // A `SIGUSR1` with no explicit size means "some other mode": the
        // portrait swap of the current one, which is a real relayout.
        if MODE_CHANGE_REQUESTED.swap(false, Ordering::SeqCst) {
            let packed = MODE_REQUEST.swap(0, Ordering::SeqCst);
            self.pending_mode = Some(if packed != 0 {
                Size::new((packed >> 32) as u32, packed as u32)
            } else {
                Size::new(self.info.size.h, self.info.size.w)
            });
        }
        let Some(size) = self.pending_mode.take() else {
            return Ok(None);
        };
        if size == self.info.size {
            return Ok(None);
        }
        self.apply_mode(size);
        Ok(Some(self.info))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn present(d: &mut HeadlessDisplay) -> u32 {
        let age = d.begin_frame().unwrap().expect("always a free buffer").age;
        d.present(&[]).unwrap();
        age
    }

    /// The buffer-age accounting must match the DRM backend's exactly — that
    /// is the whole reason for two buffers here. Both buffers start
    /// undefined (`0`); from then on each returns two presents after it was
    /// last shown, so a headless run exercises the partial-redraw path.
    #[test]
    fn buffer_age_matches_the_drm_double_buffered_sequence() {
        let mut d = HeadlessDisplay::new(Size::new(64, 32));
        assert_eq!(present(&mut d), 0, "buffer 0 has never been presented");
        assert_eq!(present(&mut d), 0, "buffer 1 has never been presented");
        for frame in 0..6 {
            assert_eq!(present(&mut d), 2, "frame {frame} reuses a 2-old buffer");
        }
        assert_eq!(d.presented_frames(), 8);
    }

    /// A buffer is always free: `begin_frame` never returns `None`, so the
    /// loop needs no present fd and no pacing timer.
    #[test]
    fn a_buffer_is_always_free() {
        let mut d = HeadlessDisplay::new(Size::new(16, 8));
        for _ in 0..4 {
            assert!(d.begin_frame().unwrap().is_some());
            d.present(&[]).unwrap();
        }
        assert!(d.present_fd().is_none());
        assert!(!d.dispatch_present().unwrap());
    }

    /// The stride is padded, never `width * bpp` — the invariant the whole
    /// stack rests on, made visible where CI can trip over it.
    #[test]
    fn stride_is_padded_beyond_the_pixel_width() {
        let mut d = HeadlessDisplay::new(Size::new(100, 4));
        let frame = d.begin_frame().unwrap().unwrap();
        assert_eq!(frame.stride, 448, "100 * 4 = 400, padded to 64");
        assert!(frame.stride > frame.size.w as usize * 4);
        assert_eq!(frame.buffer.len(), 448 * 4);
    }

    /// Rows land at stride offsets and stay inside the allocation.
    #[test]
    fn rows_are_addressable_at_the_reported_stride() {
        let mut d = HeadlessDisplay::new(Size::new(10, 3));
        let mut frame = d.begin_frame().unwrap().unwrap();
        for y in 0..3 {
            frame.row(y).fill(y as u8 + 1);
        }
        let stride = frame.stride;
        assert_eq!(frame.buffer[0], 1);
        assert_eq!(frame.buffer[stride], 2);
        assert_eq!(frame.buffer[2 * stride], 3);
        // Padding is untouched: nothing wrote past the visible row.
        assert_eq!(frame.buffer[40], 0);
    }

    /// A simulated hotplug reports the new mode once and reallocates, so the
    /// runner's `on_display_changed` path runs off-device.
    #[test]
    fn a_requested_mode_change_reconfigures_once() {
        let mut d = HeadlessDisplay::new(Size::new(64, 32));
        present(&mut d);
        assert!(d.reconfigure().unwrap().is_none(), "nothing pending");

        d.request_mode(Size::new(128, 64));
        let info = d.reconfigure().unwrap().expect("a mode change");
        assert_eq!(info.size, Size::new(128, 64));
        assert!(d.reconfigure().unwrap().is_none(), "reported once");
        // Fresh buffers: contents are undefined again.
        assert_eq!(present(&mut d), 0);
        let frame = d.begin_frame().unwrap().unwrap();
        assert_eq!(frame.buffer.len(), stride_for(128) * 64);
    }

    /// Requesting the mode already in force is not a change.
    #[test]
    fn requesting_the_current_mode_is_a_no_op() {
        let mut d = HeadlessDisplay::new(Size::new(64, 32));
        d.request_mode(Size::new(64, 32));
        assert!(d.reconfigure().unwrap().is_none());
    }

    #[test]
    fn size_parsing_rejects_junk_and_zero() {
        assert_eq!(parse_size("1024x600"), Some(Size::new(1024, 600)));
        assert_eq!(parse_size(" 800X480 "), Some(Size::new(800, 480)));
        assert_eq!(parse_size("1024"), None);
        assert_eq!(parse_size("1024x0"), None);
        assert_eq!(parse_size("axb"), None);
    }
}
