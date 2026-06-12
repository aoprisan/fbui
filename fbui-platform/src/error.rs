//! One error type for the whole platform layer.
//!
//! The layers above `fbui-platform` should never have to reason about a DRM
//! ioctl number or a libinput return code; they get a small, stable enum and an
//! attached `io::Error` when the cause was a syscall. Most variants carry a
//! short context string so the message says *which* operation failed, not just
//! that something did.

use std::fmt;

/// Result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Everything that can go wrong bringing up or driving the platform.
#[derive(Debug)]
pub enum Error {
    /// A device node could not be opened or accessed.
    Device {
        what: String,
        source: std::io::Error,
    },
    /// A syscall / ioctl failed.
    Io {
        what: String,
        source: std::io::Error,
    },
    /// We asked the kernel for something it doesn't have (no connected
    /// connector, no usable mode, an unsupported pixel format, …).
    Unsupported(String),
    /// DRM master is required for this operation and we don't hold it (not root
    /// and not the active session, or paused over a VT switch).
    NotMaster,
    /// The session/seat layer refused or failed.
    Seat(String),
    /// A backend was requested whose feature wasn't compiled in.
    FeatureDisabled(&'static str),
}

impl Error {
    pub(crate) fn device(what: impl Into<String>, source: std::io::Error) -> Self {
        Error::Device {
            what: what.into(),
            source,
        }
    }

    pub(crate) fn io(what: impl Into<String>, source: std::io::Error) -> Self {
        Error::Io {
            what: what.into(),
            source,
        }
    }

    pub(crate) fn unsupported(msg: impl Into<String>) -> Self {
        Error::Unsupported(msg.into())
    }

    /// Wrap the most recent OS error with context — the common shape after a
    /// raw `libc` call returns < 0. (Only the fbdev mmap path uses this today.)
    #[cfg_attr(not(feature = "fbdev"), allow(dead_code))]
    pub(crate) fn last_os(what: impl Into<String>) -> Self {
        Error::Io {
            what: what.into(),
            source: std::io::Error::last_os_error(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Device { what, source } => write!(f, "{what}: {source}"),
            Error::Io { what, source } => write!(f, "{what}: {source}"),
            Error::Unsupported(m) => write!(f, "unsupported: {m}"),
            Error::NotMaster => write!(
                f,
                "DRM master required (run as root, on the active VT, or via a seat manager)"
            ),
            Error::Seat(m) => write!(f, "seat: {m}"),
            Error::FeatureDisabled(feat) => {
                write!(f, "backend unavailable: rebuild with the `{feat}` feature")
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Device { source, .. } | Error::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<Error> for std::io::Error {
    fn from(e: Error) -> Self {
        match e {
            Error::Device { source, .. } | Error::Io { source, .. } => source,
            other => std::io::Error::other(other.to_string()),
        }
    }
}
