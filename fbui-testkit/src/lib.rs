//! Golden-image snapshot testing for the fbui software renderer.
//!
//! `fbui-render` is headless by construction — every painter primitive lands in
//! a normal-RAM [`tiny_skia::Pixmap`] with no device in the loop — so the natural
//! regression test is a *snapshot*: paint a scene, compare the pixels against a
//! committed reference PNG, fail loudly (with artifacts) on drift.
//!
//! The comparison is deliberately tolerant. tiny-skia is deterministic across
//! platforms for the same version, but anti-aliased edges can wobble by a single
//! code-value under different rounding, and we'd rather a one-LSB shimmer not
//! redden CI. [`assert_snapshot_in`] takes a per-channel tolerance and a cap on how
//! many pixels may exceed it.
//!
//! ## Workflow
//!
//! * First run / intentional change: set `FBUI_UPDATE_SNAPSHOTS=1` and the golden
//!   is (re)written instead of compared. Review the PNG, commit it.
//! * Normal run: the golden is loaded and compared. On mismatch the harness
//!   writes `<name>.actual.png` and `<name>.diff.png` next to the golden and
//!   panics with a summary, so the failure is inspectable, not just a number.

use std::path::{Path, PathBuf};

use tiny_skia::Pixmap;

/// How lenient a snapshot comparison is.
#[derive(Debug, Clone, Copy)]
pub struct Tolerance {
    /// Maximum allowed absolute difference, per channel (0–255), before a pixel
    /// counts as "changed". `0` demands an exact match.
    pub per_channel: u8,
    /// How many pixels may exceed `per_channel` before the snapshot fails.
    /// Absorbs a few AA-edge pixels without hiding a real regression.
    pub max_changed_pixels: u32,
}

impl Tolerance {
    /// Bit-exact: every channel of every pixel must match.
    pub const EXACT: Tolerance = Tolerance {
        per_channel: 0,
        max_changed_pixels: 0,
    };

    /// Forgiving default: a couple of code-values of AA wobble on a handful of
    /// edge pixels is fine; anything larger is a regression.
    pub const FUZZY: Tolerance = Tolerance {
        per_channel: 2,
        max_changed_pixels: 32,
    };
}

/// Outcome of comparing two same-sized images.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Comparison {
    /// Pixels whose per-channel delta exceeded the tolerance.
    pub changed_pixels: u32,
    /// The single largest per-channel delta seen anywhere.
    pub max_delta: u8,
}

/// Compare two pixmaps that must share dimensions, counting how many pixels
/// differ by more than `per_channel` on any channel.
///
/// Panics if the sizes differ — a size change is never a tolerable diff.
pub fn compare(a: &Pixmap, b: &Pixmap, per_channel: u8) -> Comparison {
    assert_eq!(
        (a.width(), a.height()),
        (b.width(), b.height()),
        "snapshot size mismatch: {}x{} vs {}x{}",
        a.width(),
        a.height(),
        b.width(),
        b.height()
    );
    let (mut changed, mut max_delta) = (0u32, 0u8);
    for (pa, pb) in a.data().chunks_exact(4).zip(b.data().chunks_exact(4)) {
        let mut over = false;
        for c in 0..4 {
            let d = pa[c].abs_diff(pb[c]);
            max_delta = max_delta.max(d);
            if d > per_channel {
                over = true;
            }
        }
        if over {
            changed += 1;
        }
    }
    Comparison {
        changed_pixels: changed,
        max_delta,
    }
}

/// Render a per-pixel diff: black where the two images agree, red scaled by the
/// delta where they don't. Handy to eyeball *where* a regression landed.
pub fn diff_image(a: &Pixmap, b: &Pixmap) -> Pixmap {
    let mut out = Pixmap::new(a.width(), a.height()).expect("diff pixmap");
    for (dst, (pa, pb)) in out
        .data_mut()
        .chunks_exact_mut(4)
        .zip(a.data().chunks_exact(4).zip(b.data().chunks_exact(4)))
    {
        let delta = (0..4).map(|c| pa[c].abs_diff(pb[c])).max().unwrap_or(0);
        // Amplify so even a 1-LSB difference is visible, then clamp.
        let v = (delta as u32 * 8).min(255) as u8;
        dst.copy_from_slice(&[v, 0, 0, 255]);
    }
    out
}

/// Assert that `actual` matches the golden PNG named `name` under `dir`.
///
/// * `FBUI_UPDATE_SNAPSHOTS=1` writes/overwrites the golden and returns.
/// * Otherwise the golden is loaded and compared under `tol`; on mismatch the
///   actual and a diff image are written beside the golden and the test panics.
pub fn assert_snapshot_in(dir: impl AsRef<Path>, name: &str, actual: &Pixmap, tol: Tolerance) {
    let dir = dir.as_ref();
    let golden = dir.join(format!("{name}.png"));

    if update_requested() {
        std::fs::create_dir_all(dir).expect("create snapshot dir");
        save_png(&golden, actual);
        eprintln!("snapshot updated: {}", golden.display());
        return;
    }

    let expected = match load_png(&golden) {
        Ok(p) => p,
        Err(e) => panic!(
            "missing golden snapshot {}: {e}\n\
             run with FBUI_UPDATE_SNAPSHOTS=1 to create it",
            golden.display()
        ),
    };

    if (expected.width(), expected.height()) != (actual.width(), actual.height()) {
        let actual_path = dir.join(format!("{name}.actual.png"));
        save_png(&actual_path, actual);
        panic!(
            "snapshot {name}: size {}x{} != golden {}x{} (wrote {})",
            actual.width(),
            actual.height(),
            expected.width(),
            expected.height(),
            actual_path.display()
        );
    }

    let cmp = compare(&expected, actual, tol.per_channel);
    if cmp.changed_pixels > tol.max_changed_pixels {
        let actual_path = dir.join(format!("{name}.actual.png"));
        let diff_path = dir.join(format!("{name}.diff.png"));
        save_png(&actual_path, actual);
        save_png(&diff_path, &diff_image(&expected, actual));
        panic!(
            "snapshot {name} drifted: {} pixels changed (max allowed {}), \
             largest per-channel delta {} (tolerance {}).\n  golden: {}\n  actual: {}\n  diff:   {}\n\
             if this change is intended, re-run with FBUI_UPDATE_SNAPSHOTS=1",
            cmp.changed_pixels,
            tol.max_changed_pixels,
            cmp.max_delta,
            tol.per_channel,
            golden.display(),
            actual_path.display(),
            diff_path.display(),
        );
    }
}

/// Whether the caller asked to (re)write goldens rather than compare.
pub fn update_requested() -> bool {
    std::env::var_os("FBUI_UPDATE_SNAPSHOTS").is_some_and(|v| !v.is_empty() && v != "0")
}

fn save_png(path: &Path, pixmap: &Pixmap) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create snapshot dir");
    }
    pixmap
        .save_png(path)
        .unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

fn load_png(path: &PathBuf) -> Result<Pixmap, String> {
    Pixmap::load_png(path).map_err(|e| e.to_string())
}
