//! Minimal DRM device node wrapper implementing the `drm-rs` device traits.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::io::{AsFd, AsRawFd, BorrowedFd, RawFd};

use drm::control::Device as ControlDevice;
use drm::Device;

pub struct Card(File);

impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl AsRawFd for Card {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

// `AsFd` is the only prerequisite for the drm-rs device traits.
impl Device for Card {}
impl ControlDevice for Card {}

impl Card {
    pub fn open(path: &str) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        Ok(Card(file))
    }
}
