//! Backlight control via sysfs (`/sys/class/backlight`) — the dimming half of
//! idle power management.
//!
//! Panels expose brightness as a `brightness` / `max_brightness` file pair
//! under a device-named directory. This module wraps one such directory with a
//! percent-based API. It's plain file I/O — no display or drm handle needed —
//! so it works identically over DRM, fbdev, and from any thread.
//!
//! Writing `brightness` needs permission (root, or a udev rule granting the
//! `video` group write access — the same provisioning story as the device
//! nodes, see `docs/running-on-your-device.md`). Failures are ordinary
//! `io::Error`s; treat dimming as best-effort.

use std::io;
use std::path::{Path, PathBuf};

/// One backlight device: a `/sys/class/backlight/<name>` directory.
#[derive(Debug, Clone)]
pub struct Backlight {
    dir: PathBuf,
    max: u32,
}

impl Backlight {
    /// The first backlight device on the system, if any. Desktop GPUs often
    /// have none (external monitors manage their own brightness — blanking
    /// via [`Display::set_power`](crate::Display::set_power) still works);
    /// laptop panels and embedded LCDs typically have exactly one.
    pub fn discover() -> Option<Backlight> {
        let entries = std::fs::read_dir("/sys/class/backlight").ok()?;
        let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        dirs.sort(); // deterministic pick when several exist
        dirs.into_iter().find_map(|d| Backlight::at(&d).ok())
    }

    /// Open a specific backlight directory (must contain `max_brightness`).
    /// This is also the testing seam: point it at any directory with the
    /// right files.
    pub fn at(dir: impl AsRef<Path>) -> io::Result<Backlight> {
        let dir = dir.as_ref().to_path_buf();
        let max: u32 = read_num(&dir.join("max_brightness"))?;
        if max == 0 {
            return Err(io::Error::other("max_brightness is 0"));
        }
        Ok(Backlight { dir, max })
    }

    /// The sysfs directory this device lives at.
    pub fn path(&self) -> &Path {
        &self.dir
    }

    /// Current brightness as a percentage of maximum (0–100).
    pub fn percent(&self) -> io::Result<u8> {
        let cur: u32 = read_num(&self.dir.join("brightness"))?;
        Ok(((cur.min(self.max) as u64 * 100).div_ceil(self.max as u64)) as u8)
    }

    /// Set brightness to a percentage of maximum, clamped to 0–100. `0` is
    /// whatever the driver does at zero — often fully dark; pair with
    /// [`Display::set_power`](crate::Display::set_power) for a true off.
    pub fn set_percent(&self, pct: u8) -> io::Result<()> {
        let raw = (self.max as u64 * pct.min(100) as u64) / 100;
        std::fs::write(self.dir.join("brightness"), format!("{raw}\n"))
    }
}

fn read_num(path: &Path) -> io::Result<u32> {
    let s = std::fs::read_to_string(path)?;
    s.trim()
        .parse::<u32>()
        .map_err(|e| io::Error::other(format!("{}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_backlight(max: u32, cur: u32) -> (tempfile::TempDir, Backlight) {
        let td = tempfile::tempdir().unwrap();
        std::fs::write(td.path().join("max_brightness"), format!("{max}\n")).unwrap();
        std::fs::write(td.path().join("brightness"), format!("{cur}\n")).unwrap();
        let bl = Backlight::at(td.path()).unwrap();
        (td, bl)
    }

    #[test]
    fn percent_reads_and_writes_scale() {
        let (_td, bl) = fake_backlight(255, 255);
        assert_eq!(bl.percent().unwrap(), 100);

        bl.set_percent(20).unwrap();
        let raw: u32 = std::fs::read_to_string(bl.path().join("brightness"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(raw, 51); // 20% of 255
        assert_eq!(bl.percent().unwrap(), 20);

        // Clamped, not wrapped.
        bl.set_percent(150).unwrap();
        assert_eq!(bl.percent().unwrap(), 100);
    }

    #[test]
    fn rejects_a_broken_device() {
        let td = tempfile::tempdir().unwrap();
        assert!(Backlight::at(td.path()).is_err(), "no max_brightness");
        std::fs::write(td.path().join("max_brightness"), "0\n").unwrap();
        assert!(Backlight::at(td.path()).is_err(), "zero max");
    }
}
