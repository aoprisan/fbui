//! tiny-skia's anti-aliased hairline rasterizer, vendored so a banded surface
//! can draw hairlines exactly as a whole-screen one does.
//!
//! A hairline is a stroke at most one device pixel wide; tiny-skia draws it
//! with its own scan converter rather than as a filled outline, and that
//! converter clips every line to the target buffer before stepping along it.
//! A band clips almost every line the whole screen wouldn't, which nudges the
//! anti-aliasing along the clipped segments — and unlike filled paths there
//! is no coverage-mask form to route around it. So the scan converter lives
//! here, unmodified apart from module paths: it is handed the **whole
//! screen** as its clip (so it chops lines exactly where the whole-screen
//! render does), and the [`Blitter`] it feeds is fbui's own, which writes
//! only the band's rows using tiny-skia's blending arithmetic.
//!
//! Source: tiny-skia 0.12.0 (`src/scan/hairline.rs`, `src/scan/hairline_aa.rs`
//! and the private helpers they use), BSD-3-Clause — see
//! `LICENSE-tiny-skia` beside this file. Keep it in step with the tiny-skia
//! version in `Cargo.toml`: the band equivalence tests
//! (`fbui-render/tests/banded.rs`) fail if the two drift apart.

#![allow(dead_code, clippy::all)]

mod alpha_runs;
mod band;
mod blitter;
mod fixed_point;
mod geom;
mod hairline_aa;
#[path = "hairline.rs"]
mod hairline_impl;
mod line_clipper;
mod math;
mod path_geometry;

pub(crate) use band::BandBlitter;

/// Rasterize the anti-aliased hairline `path` (device space, whole-screen
/// coordinates) with a `surface_w × surface_h` clip — exactly what tiny-skia
/// does on a whole-screen target — into `blitter`.
pub(crate) fn stroke(
    path: &Path,
    cap: LineCap,
    surface_w: u32,
    surface_h: u32,
    blitter: &mut BandBlitter<'_>,
) {
    let Some(clip) = geom::ScreenIntRect::from_xywh(0, 0, surface_w, surface_h) else {
        return;
    };
    hairline_aa::stroke_path(path, cap, &clip, blitter);
}

// The handful of crate-root items tiny-skia's code refers to.
use tiny_skia_path::{IntRect, LineCap, Path, PathSegment, Point, Rect};
type LengthU32 = core::num::NonZeroU32;
type AlphaU8 = u8;
