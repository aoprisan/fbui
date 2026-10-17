//! Finding disks to scan: whole block devices from `/sys/block`, plus any
//! paths given on the command line (image files work too — handy for trying
//! the app out, and what the headless flow uses).

use std::fs;
use std::path::Path;

/// A scan target as the device list shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// What to open: `/dev/sda`, or a file path.
    pub path: String,
    /// Model / vendor text, if the kernel reports one.
    pub model: String,
    pub sector_size: usize,
    pub sectors: u64,
    pub removable: bool,
}

impl Target {
    pub fn bytes(&self) -> u64 {
        self.sectors * self.sector_size as u64
    }

    /// One list row: `/dev/sda  QEMU HARDDISK  64.0 MiB`.
    pub fn label(&self) -> String {
        let mut s = self.path.clone();
        if !self.model.is_empty() {
            s.push_str("  ");
            s.push_str(&self.model);
        }
        s.push_str("  ");
        s.push_str(&human_bytes(self.bytes()));
        if self.removable {
            s.push_str("  removable");
        }
        s
    }
}

/// `1536` → `1.5 KiB`.
pub fn human_bytes(b: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut v = b as f64;
    let mut u = 0;
    while v >= 1024.0 && u + 1 < UNITS.len() {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

fn read_trim(p: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

/// The logical sector size of the block device behind `path`, from sysfs.
/// `None` for anything that isn't a block device under `/dev`.
pub fn logical_sector_size(path: &str) -> Option<usize> {
    let name = path.strip_prefix("/dev/")?;
    read_trim(format!("/sys/class/block/{name}/queue/logical_block_size"))?
        .parse()
        .ok()
}

/// Block devices worth scanning: whole disks with media, not RAM disks, loop
/// devices nobody attached, or the optical drive the ISO booted from.
pub fn block_devices() -> Vec<Target> {
    let Ok(entries) = fs::read_dir("/sys/block") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if ["ram", "zram", "loop", "sr", "fd"]
            .iter()
            .any(|p| name.starts_with(p))
        {
            continue;
        }
        let base = e.path();
        // `size` is always in 512-byte units, whatever the sector size.
        let Some(size512) = read_trim(base.join("size")).and_then(|s| s.parse::<u64>().ok()) else {
            continue;
        };
        if size512 == 0 {
            continue;
        }
        let sector_size = read_trim(base.join("queue/logical_block_size"))
            .and_then(|s| s.parse().ok())
            .unwrap_or(512usize);
        // SCSI/ATA disks report a vendor string; virtio and NVMe a PCI id
        // (`0x1af4`), which says nothing to an operator.
        let vendor = read_trim(base.join("device/vendor"))
            .filter(|v| !v.starts_with("0x"))
            .unwrap_or_default();
        let model = read_trim(base.join("device/model")).unwrap_or_default();
        let model = if vendor.is_empty() || model.starts_with(&vendor) {
            model
        } else {
            format!("{vendor} {model}")
        };
        out.push(Target {
            path: format!("/dev/{name}"),
            model,
            sector_size,
            sectors: size512 * 512 / sector_size as u64,
            removable: read_trim(base.join("removable")).as_deref() == Some("1"),
        });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// A command-line path (image file or device node) as a target.
pub fn from_path(path: &str) -> Option<Target> {
    let g = crate::scan::geometry(path).ok()?;
    Some(Target {
        path: path.to_string(),
        model: if logical_sector_size(path).is_some() {
            String::new()
        } else {
            "image".into()
        },
        sector_size: g.sector_size,
        sectors: g.sectors,
        removable: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_sizes() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KiB");
        assert_eq!(human_bytes(64 << 20), "64.0 MiB");
    }
}
