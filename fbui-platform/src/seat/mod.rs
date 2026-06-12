//! Session / seat management: who is allowed to open the DRM and input nodes,
//! and who tells us when we lose and regain them across a VT switch.
//!
//! Two backends, mirroring Slint's split:
//!
//! * [`noseat`] (default) — open device nodes directly. Works as root or with
//!   the `video`+`input` groups; the right answer for bare embedded/kiosk where
//!   there is no session manager. VT switching is mediated by [`crate::vt`]
//!   instead.
//! * `libseat` (feature) — broker every open through logind/seatd, so an
//!   unprivileged user on the active seat can run, and session
//!   activate/deactivate (the VT handoff) arrives as [`SessionEvent`]s.
//!
//! Both implement [`Seat`]. The platform opens its DRM card and input devices
//! *through* the seat so the privileged-vs-brokered choice stays in one place.

use std::os::unix::io::{OwnedFd, RawFd};
use std::path::Path;

use crate::error::Result;

#[cfg(feature = "libseat")]
pub mod libseat;
#[cfg(feature = "noseat")]
pub mod noseat;

/// The session gained or lost the right to drive the hardware.
///
/// On [`Deactivate`](SessionEvent::Deactivate) the platform must stop rendering
/// and drop DRM master (the kernel/seat is about to hand the VT to someone
/// else); on [`Activate`](SessionEvent::Activate) it re-acquires master,
/// restores the mode, and forces a full repaint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEvent {
    Activate,
    Deactivate,
}

/// A broker for opening restricted device nodes and reporting session changes.
///
/// Object-safe: the platform holds a `Box<dyn Seat>`.
pub trait Seat {
    /// The seat name (`"seat0"` for the usual single-seat machine).
    fn name(&self) -> &str;

    /// Open a device node (a DRM card or an `/dev/input/event*`), returning an
    /// fd we own. Under `noseat` this is a plain `open(2)`; under `libseat` the
    /// manager opens it and revokes it on deactivate.
    fn open_device(&mut self, path: &Path) -> Result<OwnedFd>;

    /// Hand a device fd back to the broker (a no-op release under `noseat`).
    fn close_device(&mut self, fd: OwnedFd);

    /// Descriptor to poll for session events, if the backend has one. `noseat`
    /// returns `None` (VT events come from [`crate::vt`] instead).
    fn session_fd(&self) -> Option<RawFd>;

    /// Drain session events after [`session_fd`](Seat::session_fd) signalled.
    fn dispatch(&mut self, sink: &mut dyn FnMut(SessionEvent)) -> Result<()>;
}
