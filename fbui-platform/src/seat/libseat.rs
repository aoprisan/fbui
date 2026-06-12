//! libseat session backend (feature `libseat`).
//!
//! Brokers device access through logind or seatd via the system `libseat`
//! library, so an unprivileged user on the active seat can drive the hardware,
//! and the VT handoff arrives as [`SessionEvent`]s (libseat raises
//! Enable/Disable around every switch). On Disable we must finish dropping
//! master and call [`Seat::dispatch`]'s machinery so libseat can complete the
//! switch; on Enable we re-acquire.
//!
//! Links the system `libseat` C library; only compiled with the `libseat`
//! feature and **not** validated in library-less environments (see
//! `PHASE1.md`). The pure-Rust [`noseat`](super::noseat) backend is the
//! verified default.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::os::unix::io::{AsRawFd, OwnedFd, RawFd};
use std::path::Path;
use std::rc::Rc;

use libseat::{Seat as LibSeat, SeatEvent};

use super::{Seat, SessionEvent};
use crate::error::{Error, Result};

/// Pending enable/disable notifications delivered by libseat's callback, queued
/// for [`dispatch`](Seat::dispatch) to forward as [`SessionEvent`]s.
type Pending = Rc<RefCell<VecDeque<SessionEvent>>>;

/// A libseat-managed session.
pub struct LibseatSession {
    seat: LibSeat,
    name: String,
    pending: Pending,
    /// libseat assigns each opened device an integer id we need to close it.
    devices: Vec<(RawFd, i32)>,
}

impl LibseatSession {
    /// Open the seat for the current session and block until it is active.
    pub fn open() -> Result<Self> {
        let pending: Pending = Rc::new(RefCell::new(VecDeque::new()));
        let cb_pending = pending.clone();
        let seat = LibSeat::open(move |_seat, event| {
            let mapped = match event {
                SeatEvent::Enable => SessionEvent::Activate,
                SeatEvent::Disable => SessionEvent::Deactivate,
            };
            cb_pending.borrow_mut().push_back(mapped);
        })
        .map_err(|e| Error::Seat(format!("libseat open: {e}")))?;

        let name = seat.name().to_string();
        Ok(LibseatSession {
            seat,
            name,
            pending,
            devices: Vec::new(),
        })
    }
}

impl Seat for LibseatSession {
    fn name(&self) -> &str {
        &self.name
    }

    fn open_device(&mut self, path: &Path) -> Result<OwnedFd> {
        let (id, fd) = self
            .seat
            .open_device(&path)
            .map_err(|e| Error::Seat(format!("libseat open_device {}: {e}", path.display())))?;
        let raw = fd.as_raw_fd();
        self.devices.push((raw, id));
        Ok(fd)
    }

    fn close_device(&mut self, fd: OwnedFd) {
        let raw = fd.as_raw_fd();
        if let Some(pos) = self.devices.iter().position(|&(f, _)| f == raw) {
            let (_, id) = self.devices.remove(pos);
            let _ = self.seat.close_device(id);
        }
        drop(fd);
    }

    fn session_fd(&self) -> Option<RawFd> {
        Some(self.seat.as_raw_fd())
    }

    fn dispatch(&mut self, sink: &mut dyn FnMut(SessionEvent)) -> Result<()> {
        // Pump libseat: this fires the open() callback, filling `pending`.
        self.seat
            .dispatch(0)
            .map_err(|e| Error::Seat(format!("libseat dispatch: {e}")))?;
        // On Activate we must tell libseat the switch is complete.
        let drained: Vec<SessionEvent> = self.pending.borrow_mut().drain(..).collect();
        for ev in drained {
            if ev == SessionEvent::Activate {
                // No-op if not pending; acknowledges the session is in use.
                let _ = self.seat.dispatch(0);
            }
            sink(ev);
        }
        Ok(())
    }
}
