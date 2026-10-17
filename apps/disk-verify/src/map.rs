//! `SurfaceMap`: the disk as a grid of cells, coloured by what the scan found
//! — pending, skipped, good, wrong bytes, unreadable — with the read head
//! outlined. The custom widget of this example.
//!
//! State is kept at a fixed resolution ([`BUCKETS`] buckets across the disk)
//! independent of the on-screen grid, so a resize or a different screen only
//! changes how buckets fold into cells, never what was recorded. A cell shows
//! the worst bucket under it: one bad sector in a gigabyte still shows red.

use std::any::Any;

use fbui::describe::Describe;
use fbui::render::geom::{Rect, Size};
use fbui::render::{Color, FontContext};
use fbui::widget::{AvailableSize, KnownDims};
use fbui::{PaintCtx, Style, Theme, Widget};

use crate::ranges::Range;
use crate::scan::Status;

/// Resolution of the recorded state.
pub const BUCKETS: usize = 8192;
/// Target cell side, logical px.
const CELL: f32 = 10.0;

/// Ordered by severity: a bucket only ever moves up.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Cell {
    Pending,
    Skipped,
    Ok,
    Mismatch,
    Unreadable,
}

impl From<Status> for Cell {
    fn from(s: Status) -> Cell {
        match s {
            Status::Ok => Cell::Ok,
            Status::Mismatch => Cell::Mismatch,
            Status::Unreadable => Cell::Unreadable,
        }
    }
}

impl Cell {
    pub fn color(self, theme: &Theme) -> Color {
        match self {
            Cell::Pending => theme.palette.surface_alt,
            Cell::Skipped => theme.palette.line,
            Cell::Ok => Color::rgb(0x2e, 0xa0, 0x43),
            Cell::Mismatch => Color::rgb(0xe3, 0xb3, 0x41),
            Cell::Unreadable => theme.palette.danger,
        }
    }
}

pub struct SurfaceMap {
    sectors: u64,
    buckets: Vec<Cell>,
    head: Option<u64>,
}

impl Default for SurfaceMap {
    fn default() -> Self {
        Self::new()
    }
}

impl SurfaceMap {
    pub fn new() -> Self {
        SurfaceMap {
            sectors: 0,
            buckets: Vec::new(),
            head: None,
        }
    }

    /// Start over for a disk of `sectors` sectors, everything pending.
    pub fn reset(&mut self, sectors: u64) {
        self.sectors = sectors;
        let n = (sectors.min(BUCKETS as u64)) as usize;
        self.buckets = vec![Cell::Pending; n];
        self.head = None;
    }

    fn bucket_of(&self, sector: u64) -> usize {
        // u128: sector * BUCKETS overflows u64 on a multi-PB disk.
        ((sector as u128 * self.buckets.len() as u128) / self.sectors.max(1) as u128) as usize
    }

    /// Record that `r` is `cell` (never lowering a bucket's severity).
    pub fn mark(&mut self, r: Range, cell: Cell) {
        if self.buckets.is_empty() || r.is_empty() {
            return;
        }
        let a = self.bucket_of(r.start);
        let b = self.bucket_of(r.end - 1).min(self.buckets.len() - 1);
        for c in &mut self.buckets[a..=b] {
            *c = (*c).max(cell);
        }
    }

    pub fn set_head(&mut self, sector: Option<u64>) {
        self.head = sector;
    }

    pub fn count(&self, cell: Cell) -> usize {
        self.buckets.iter().filter(|c| **c == cell).count()
    }

    /// Grid for bounds `b`: (columns, rows, cell side).
    fn grid(&self, b: Rect) -> (usize, usize, f32) {
        let n = self.buckets.len().max(1);
        // Start at the target size, shrink until all cells fit (or bottom out
        // at a single device pixel and let buckets share cells).
        let mut side = CELL;
        loop {
            let cols = (b.w / side).floor().max(1.0) as usize;
            let rows = (b.h / side).floor().max(1.0) as usize;
            if cols * rows >= n || side <= 2.0 {
                let rows = rows.min(n.div_ceil(cols));
                return (cols, rows, side);
            }
            side -= 1.0;
        }
    }
}

impl<Msg: 'static> Widget<Msg> for SurfaceMap {
    fn layout_style(&self, _theme: &Theme) -> Style {
        Style {
            flex_grow: 1.0,
            ..Style::default()
        }
    }

    fn measure(
        &mut self,
        _fonts: &mut FontContext,
        _theme: &Theme,
        _known: KnownDims,
        _available: AvailableSize,
    ) -> Option<Size> {
        Some(Size::new(200.0, 120.0))
    }

    fn paint(&self, ctx: &mut PaintCtx) {
        let b = ctx.bounds();
        let theme = ctx.theme().clone();
        let p = ctx.painter();
        p.fill_rect(b, theme.palette.surface);
        if self.buckets.is_empty() {
            return;
        }
        let (cols, rows, side) = self.grid(b);
        let cells = cols * rows;
        let n = self.buckets.len();
        let head = self.head.map(|s| self.bucket_of(s));
        let gap = if side >= 5.0 { 1.0 } else { 0.0 };
        for i in 0..cells {
            // Buckets [lo, hi) fold into cell i.
            let lo = i * n / cells;
            let hi = ((i + 1) * n / cells).max(lo + 1).min(n);
            if lo >= n {
                break;
            }
            let worst = self.buckets[lo..hi]
                .iter()
                .copied()
                .max()
                .unwrap_or(Cell::Pending);
            let (cx, cy) = ((i % cols) as f32, (i / cols) as f32);
            let r = Rect::new(b.x + cx * side, b.y + cy * side, side - gap, side - gap);
            p.fill_rect(r, worst.color(&theme));
            if head.is_some_and(|h| (lo..hi).contains(&h)) {
                p.stroke_rect(r, theme.palette.text, 1.5);
            }
        }
    }

    fn describe(&self, out: &mut Describe) {
        out.prop("sectors", self.sectors);
        out.prop("ok", self.count(Cell::Ok));
        out.prop(
            "bad",
            self.count(Cell::Mismatch) + self.count(Cell::Unreadable),
        );
        out.prop("skipped", self.count(Cell::Skipped));
        out.prop("pending", self.count(Cell::Pending));
    }

    fn debug_name(&self) -> &'static str {
        "SurfaceMap"
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// A legend chip: a small square in a cell's colour.
pub struct Swatch(pub Cell);

impl<Msg: 'static> Widget<Msg> for Swatch {
    fn layout_style(&self, _theme: &Theme) -> Style {
        Style::default()
    }

    fn measure(
        &mut self,
        _fonts: &mut FontContext,
        _theme: &Theme,
        _known: KnownDims,
        _available: AvailableSize,
    ) -> Option<Size> {
        Some(Size::new(12.0, 12.0))
    }

    fn paint(&self, ctx: &mut PaintCtx) {
        let b = ctx.bounds();
        let color = self.0.color(ctx.theme());
        let line = ctx.theme().palette.muted;
        let p = ctx.painter();
        p.fill_rect(b, color);
        if self.0 == Cell::Pending {
            p.stroke_rect(b, line, 1.0);
        }
    }

    fn debug_name(&self) -> &'static str {
        "Swatch"
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_only_raise_severity() {
        let mut m = SurfaceMap::new();
        m.reset(100);
        assert_eq!(m.buckets.len(), 100); // small disk: one bucket per sector
        m.mark(Range::new(0, 10), Cell::Skipped);
        m.mark(Range::new(5, 6), Cell::Unreadable);
        m.mark(Range::new(0, 10), Cell::Ok);
        assert_eq!(m.count(Cell::Ok), 9);
        assert_eq!(m.count(Cell::Unreadable), 1);
        assert_eq!(m.count(Cell::Pending), 90);
    }

    #[test]
    fn one_bad_sector_on_a_big_disk_still_shows() {
        let mut m = SurfaceMap::new();
        m.reset(u64::MAX / 1024);
        m.mark(Range::new(0, u64::MAX / 1024), Cell::Ok);
        m.mark(Range::new(123_456_789, 123_456_790), Cell::Mismatch);
        assert_eq!(m.count(Cell::Mismatch), 1);
        assert_eq!(m.buckets.len(), BUCKETS);
    }

    #[test]
    fn grid_fits_every_bucket() {
        let mut m = SurfaceMap::new();
        m.reset(1 << 30);
        let (cols, rows, side) = m.grid(Rect::new(0.0, 0.0, 900.0, 600.0));
        assert!(cols * rows >= BUCKETS, "{cols}x{rows} @ {side}");
        assert!(side * rows as f32 <= 600.0 && side * cols as f32 <= 900.0);
    }
}
