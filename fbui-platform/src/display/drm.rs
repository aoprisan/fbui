//! DRM/KMS dumb-buffer backend — the primary, vsynced display path.
//!
//! This promotes the Phase 0 spike's `drm_backend.rs` into a real [`Display`]:
//! open a card, pick the connected connector + its preferred mode, resolve a
//! CRTC, allocate **two** XRGB8888 dumb buffers, modeset, then page-flip between
//! them. The block on the DRM fd for the flip-complete event *is* the frame
//! clock — the event loop multiplexes that fd so an idle UI sleeps at ~0% CPU.
//!
//! What Phase 1 adds over the spike: the [`Display`] trait shape (back buffer +
//! stride + buffer age handed out by `begin_frame`), explicit master
//! acquire/release for cooperative VT switching ([`suspend`]/[`resume`]), and
//! the ability to take an already-opened fd from the seat layer instead of
//! opening the node itself.
//!
//! [`suspend`]: Display::suspend
//! [`resume`]: Display::resume

use std::os::unix::io::{AsFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};

use drm::buffer::{Buffer, DrmFourcc};
use drm::control::{
    connector, crtc, dumbbuffer::DumbBuffer, framebuffer, Device as ControlDevice, Mode,
    ModeTypeFlags, PageFlipFlags, ResourceHandles,
};
use drm::Device;

use super::{BackendKind, Display, DisplayInfo, Frame};
use crate::error::{Error, Result};
use crate::format::PixelFormat;
use crate::geom::{Rect, Size};

/// A DRM device node implementing the `drm-rs` device traits.
pub struct Card(OwnedFd);

impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}
// `AsFd` is the only prerequisite for the drm-rs device traits.
impl Device for Card {}
impl ControlDevice for Card {}

impl Card {
    /// Open a card node directly (root / `video` group — the `noseat` path).
    pub fn open(path: &str) -> Result<Self> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| Error::device(format!("open {path}"), e))?;
        Ok(Card(OwnedFd::from(file)))
    }

    /// Adopt an fd opened elsewhere (the seat manager hands us one over its
    /// session). We take ownership so the node is closed when the card drops.
    ///
    /// # Safety
    /// `fd` must be an open DRM card node not owned by anything else.
    pub unsafe fn from_raw_fd(fd: RawFd) -> Self {
        Card(OwnedFd::from_raw_fd(fd))
    }

    pub fn from_owned_fd(fd: OwnedFd) -> Self {
        Card(fd)
    }
}

/// One dumb buffer, mapped once for the life of the run.
///
/// Mapped a single time (the spike's discipline): we `forget` the RAII mapping
/// and `munmap` by hand at teardown so no per-frame mmap/munmap pollutes timing,
/// and the mapping is only ever written forward, whole-row.
struct DumbBuf {
    db: DumbBuffer,
    fb: framebuffer::Handle,
    ptr: *mut u8,
    len: usize,
    pitch: usize,
    /// Value of `present_count` when this buffer was last presented, for the
    /// EGL-style buffer-age hint. `None` until first presented.
    last_present: Option<u64>,
}

impl DumbBuf {
    fn create(card: &Card, size: Size, fourcc: DrmFourcc) -> Result<Self> {
        let mut db = card
            .create_dumb_buffer((size.w, size.h), fourcc, 32)
            .map_err(|e| Error::io("create_dumb_buffer", e))?;
        let fb = card
            .add_framebuffer(&db, 24, 32)
            .map_err(|e| Error::io("add_framebuffer", e))?;
        let pitch = db.pitch() as usize;
        let mut map = card
            .map_dumb_buffer(&mut db)
            .map_err(|e| Error::io("map_dumb_buffer", e))?;
        let slice = map.as_mut();
        let ptr = slice.as_mut_ptr();
        let len = slice.len();
        std::mem::forget(map);
        Ok(DumbBuf {
            db,
            fb,
            ptr,
            len,
            pitch,
            last_present: None,
        })
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: `ptr`/`len` came from a valid mapping that outlives `self`
        // (we only munmap at teardown); `&mut self` makes access exclusive.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

/// The DRM dumb-buffer display.
pub struct DrmDisplay {
    card: Card,
    crtc: crtc::Handle,
    connector: connector::Handle,
    mode: Mode,
    info: DisplayInfo,
    buffers: [DumbBuf; 2],
    /// Index of the buffer the caller draws into next.
    back: usize,
    /// A flip has been queued and not yet completed; no buffer is free.
    flip_pending: bool,
    /// Total presents issued — drives buffer-age accounting.
    present_count: u64,
    /// True while we hold DRM master (false between suspend and resume).
    master: bool,
}

impl DrmDisplay {
    /// Bring up the first connected connector at its preferred mode.
    pub fn new(card: Card) -> Result<Self> {
        let res: ResourceHandles = card
            .resource_handles()
            .map_err(|e| Error::io("resource_handles", e))?;

        let con = res
            .connectors()
            .iter()
            .flat_map(|&c| card.get_connector(c, true))
            .find(|i| i.state() == connector::State::Connected)
            .ok_or_else(|| Error::unsupported("no connected connector"))?;

        let mode = con
            .modes()
            .iter()
            .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
            .or_else(|| con.modes().first())
            .copied()
            .ok_or_else(|| Error::unsupported("connector has no modes"))?;

        let crtc = pick_crtc(&card, &res, &con)?;
        let (w, h) = mode.size();
        let size = Size::new(w as u32, h as u32);
        let format = PixelFormat::Xrgb8888;

        let buffers = [
            DumbBuf::create(&card, size, format.drm_fourcc_kind())?,
            DumbBuf::create(&card, size, format.drm_fourcc_kind())?,
        ];

        let info = DisplayInfo {
            size,
            format,
            refresh_mhz: mode.vrefresh().saturating_mul(1000),
            buffers: 2,
            backend: BackendKind::DrmDumb,
        };

        let mut me = DrmDisplay {
            card,
            crtc,
            connector: con.handle(),
            mode,
            info,
            buffers,
            back: 1,
            flip_pending: false,
            present_count: 0,
            master: true,
        };
        me.modeset()?;
        Ok(me)
    }

    /// Convenience: open a card node directly and bring it up (`noseat`).
    pub fn open(path: &str) -> Result<Self> {
        Self::new(Card::open(path)?)
    }

    /// Point the CRTC at buffer 0 with our mode. Requires DRM master.
    fn modeset(&mut self) -> Result<()> {
        self.card
            .set_crtc(
                self.crtc,
                Some(self.buffers[0].fb),
                (0, 0),
                &[self.connector],
                Some(self.mode),
            )
            .map_err(|e| master_aware_err("set_crtc", e))?;
        Ok(())
    }
}

impl Display for DrmDisplay {
    fn info(&self) -> DisplayInfo {
        self.info
    }

    fn begin_frame(&mut self) -> Result<Option<Frame<'_>>> {
        if self.flip_pending || !self.master {
            return Ok(None);
        }
        let present_count = self.present_count;
        let size = self.info.size;
        let format = self.info.format;
        let buf = &mut self.buffers[self.back];
        let age = match buf.last_present {
            Some(p) => (present_count - p) as u32,
            None => 0,
        };
        let pitch = buf.pitch;
        Ok(Some(Frame {
            buffer: buf.as_mut_slice(),
            stride: pitch,
            size,
            format,
            age,
        }))
    }

    fn present(&mut self, _damage: &[Rect]) -> Result<()> {
        if !self.master {
            return Err(Error::NotMaster);
        }
        // Queue a vsynced flip to the buffer just drawn; the EVENT flag makes the
        // card post a completion to its fd, which the loop polls. We do *not*
        // swap `back` here — the buffer stays ours until the flip completes.
        self.card
            .page_flip(
                self.crtc,
                self.buffers[self.back].fb,
                PageFlipFlags::EVENT,
                None,
            )
            .map_err(|e| master_aware_err("page_flip", e))?;
        self.buffers[self.back].last_present = Some(self.present_count);
        self.present_count += 1;
        self.flip_pending = true;
        Ok(())
    }

    fn present_fd(&self) -> Option<BorrowedFd<'_>> {
        Some(self.card.as_fd())
    }

    fn dispatch_present(&mut self) -> Result<bool> {
        let events = self
            .card
            .receive_events()
            .map_err(|e| Error::io("receive_events", e))?;
        let mut completed = false;
        for ev in events {
            if let drm::control::Event::PageFlip(_) = ev {
                completed = true;
            }
        }
        if completed {
            // The buffer we flipped is now on screen; the other one is free.
            self.flip_pending = false;
            self.back ^= 1;
        }
        Ok(completed)
    }

    fn suspend(&mut self) -> Result<()> {
        if self.master {
            self.card
                .release_master_lock()
                .map_err(|e| Error::io("release_master_lock", e))?;
            self.master = false;
            self.flip_pending = false;
        }
        Ok(())
    }

    fn resume(&mut self) -> Result<()> {
        if !self.master {
            self.card
                .acquire_master_lock()
                .map_err(|e| Error::io("acquire_master_lock", e))?;
            self.master = true;
        }
        // Re-establish the mode and force a full repaint: buffer contents are no
        // longer trustworthy after another session owned the CRTC.
        self.modeset()?;
        for b in &mut self.buffers {
            b.last_present = None;
        }
        self.flip_pending = false;
        self.back = 1;
        Ok(())
    }
}

impl Drop for DrmDisplay {
    fn drop(&mut self) {
        // Buffers own kernel objects; tear them down explicitly. We move them out
        // of the array via `std::mem::replace` with throwaway-but-valid stand-ins
        // is awkward, so destroy by reading the raw fields. Simpler: take them.
        let card = &self.card;
        // SAFETY-free: just consume each buffer's resources.
        for b in self.buffers.iter_mut() {
            // SAFETY: unmap our forgotten mapping; drop the kernel handles.
            unsafe {
                libc::munmap(b.ptr as *mut libc::c_void, b.len);
            }
            let _ = card.destroy_framebuffer(b.fb);
            let _ = card.destroy_dumb_buffer(b.db);
        }
    }
}

/// Pick the CRTC for a connector: prefer the one its current encoder drives,
/// otherwise the first candidate encoder's `possible_crtcs`, otherwise crtc[0].
fn pick_crtc(card: &Card, res: &ResourceHandles, con: &connector::Info) -> Result<crtc::Handle> {
    if let Some(enc) = con.current_encoder() {
        if let Ok(info) = card.get_encoder(enc) {
            if let Some(c) = info.crtc() {
                return Ok(c);
            }
        }
    }
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
        .ok_or_else(|| Error::unsupported("no CRTCs on this card"))
}

/// `set_crtc`/`page_flip` fail with `EACCES`/`EPERM` when we lack DRM master;
/// surface that as the dedicated [`Error::NotMaster`] so callers can point the
/// user at the cause instead of a bare errno.
fn master_aware_err(what: &str, e: std::io::Error) -> Error {
    match e.raw_os_error() {
        Some(libc::EACCES) | Some(libc::EPERM) => Error::NotMaster,
        _ => Error::io(what.to_string(), e),
    }
}

// Bridge our `PixelFormat` to the `drm-rs` fourcc enum. Kept here (rather than in
// `format.rs`) so the public format type carries no dependency on `drm`.
impl PixelFormat {
    fn drm_fourcc_kind(self) -> DrmFourcc {
        match self {
            PixelFormat::Xrgb8888 => DrmFourcc::Xrgb8888,
            PixelFormat::Argb8888 => DrmFourcc::Argb8888,
            PixelFormat::Rgb565 => DrmFourcc::Rgb565,
        }
    }
}
