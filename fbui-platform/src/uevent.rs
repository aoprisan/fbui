//! A kernel uevent monitor over a netlink socket — the hotplug *trigger*.
//!
//! Pure libc, no libudev: open `NETLINK_KOBJECT_UEVENT`, bind to the kernel
//! multicast group, and watch for `SUBSYSTEM=drm` events. The event loop adds
//! this fd and, on a DRM uevent, asks the display to [`reconfigure`] (a no-op if
//! nothing actually changed). That makes hotplug react immediately instead of
//! waiting for the periodic poll, and needs no system library — so it works on a
//! bare embedded image where there is no udevd.
//!
//! Best-effort by design: if the socket can't be opened or bound (a sandbox
//! without netlink, say), the caller falls back to the periodic poll.
//!
//! [`reconfigure`]: crate::display::Display::reconfigure

use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd, RawFd};

use crate::error::{Error, Result};

/// A bound netlink socket receiving kernel uevents.
pub struct UeventMonitor {
    fd: OwnedFd,
}

impl UeventMonitor {
    /// Open and bind the socket, listening to kernel-originated events (group 1).
    /// `Ok(None)` if it can't be created or bound — the caller then relies on the
    /// periodic poll rather than failing to start.
    pub fn open() -> Result<Option<Self>> {
        // SAFETY: socket(2) with constant args; returns -1 on error, checked below.
        let raw = unsafe {
            libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_DGRAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                libc::NETLINK_KOBJECT_UEVENT,
            )
        };
        if raw < 0 {
            return Ok(None);
        }
        // SAFETY: `raw` is a fresh owned fd from socket(2).
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };

        // SAFETY: zeroed sockaddr_nl is a valid (unbound) address; we then set the
        // family and the kernel multicast group.
        let mut addr: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        addr.nl_family = libc::AF_NETLINK as u16;
        // group 1 = kernel-originated uevents (raw format, not the libudev one).
        addr.nl_groups = 1;
        // SAFETY: bind(2) with a properly-sized, initialized sockaddr_nl.
        let rc = unsafe {
            libc::bind(
                fd.as_raw_fd(),
                &addr as *const libc::sockaddr_nl as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
            )
        };
        if rc < 0 {
            return Ok(None);
        }
        Ok(Some(UeventMonitor { fd }))
    }

    /// The raw fd, for adding to the event loop.
    pub fn fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    /// Drain every pending datagram; return `true` if any is a DRM event.
    ///
    /// Over-triggering is harmless — the loop's `reconfigure` is idempotent and
    /// returns "no change" unless the mode/connector actually moved.
    pub fn drain_is_drm(&self) -> Result<bool> {
        // A kernel uevent is NUL-separated `KEY=VALUE` records; the subsystem
        // value is exactly "drm", so match it NUL-terminated to avoid a prefix
        // hit on some hypothetical `drm…` subsystem.
        const DRM: &[u8] = b"SUBSYSTEM=drm\0";
        let mut buf = [0u8; 8192];
        let mut found = false;
        loop {
            // SAFETY: recv(2) into a local buffer; the length is its capacity.
            let n = unsafe {
                libc::recv(
                    self.fd.as_raw_fd(),
                    buf.as_mut_ptr() as *mut libc::c_void,
                    buf.len(),
                    0,
                )
            };
            if n < 0 {
                let err = std::io::Error::last_os_error();
                match err.raw_os_error() {
                    Some(c) if c == libc::EAGAIN || c == libc::EWOULDBLOCK => break,
                    Some(c) if c == libc::EINTR => continue,
                    _ => return Err(Error::io("uevent recv", err)),
                }
            }
            if n == 0 {
                break;
            }
            if contains(&buf[..n as usize], DRM) {
                found = true;
            }
        }
        Ok(found)
    }
}

/// Byte-substring search. Needles and messages are both tiny, so the naive scan
/// is fine and pulls in no dependency.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    needle.len() <= haystack.len() && haystack.windows(needle.len()).any(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::contains;

    #[test]
    fn finds_nul_terminated_drm_subsystem() {
        let msg = b"change@/devices/pci/drm/card0\0ACTION=change\0SUBSYSTEM=drm\0HOTPLUG=1\0";
        assert!(contains(msg, b"SUBSYSTEM=drm\0"));
    }

    #[test]
    fn ignores_other_subsystems() {
        let msg = b"add@/devices/usb\0SUBSYSTEM=usb\0";
        assert!(!contains(msg, b"SUBSYSTEM=drm\0"));
    }
}
