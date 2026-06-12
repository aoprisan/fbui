//! Direct-open session backend (`noseat`).
//!
//! No session manager: we just `open(2)` the nodes, which requires running as
//! root or as a member of the `video` (DRM) and `input` (evdev) groups. This is
//! the embedded/kiosk default. There is no session fd — VT switching is handled
//! cooperatively by [`crate::vt`], not here — so [`session_fd`] is `None` and
//! [`dispatch`] never produces events.
//!
//! [`session_fd`]: Seat::session_fd
//! [`dispatch`]: Seat::dispatch

use std::os::unix::io::{OwnedFd, RawFd};
use std::path::Path;

use super::{Seat, SessionEvent};
use crate::error::{Error, Result};

/// A trivial seat that opens devices directly.
pub struct NoSeat {
    name: String,
}

impl NoSeat {
    pub fn new() -> Self {
        NoSeat {
            name: "seat0".to_string(),
        }
    }
}

impl Default for NoSeat {
    fn default() -> Self {
        Self::new()
    }
}

impl Seat for NoSeat {
    fn name(&self) -> &str {
        &self.name
    }

    fn open_device(&mut self, path: &Path) -> Result<OwnedFd> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| Error::device(format!("open {}", path.display()), e))?;
        Ok(OwnedFd::from(file))
    }

    fn close_device(&mut self, fd: OwnedFd) {
        drop(fd);
    }

    fn session_fd(&self) -> Option<RawFd> {
        None
    }

    fn dispatch(&mut self, _sink: &mut dyn FnMut(SessionEvent)) -> Result<()> {
        Ok(())
    }
}
