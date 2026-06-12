//! Damage tracking: which device-pixel regions changed, and which a given back
//! buffer must therefore repaint.
//!
//! CPU rendering at 1080p+ is only viable because we repaint *regions*, not
//! frames. Two jobs live here:
//!
//! 1. **Collection + merge.** The painter reports a device-pixel [`IRect`] for
//!    every primitive it draws. Tracking hundreds of tiny rects and copying each
//!    out separately is its own overhead, so [`DamageTracker`] merges: rects that
//!    overlap or whose combined bounding box barely exceeds their separate areas
//!    are fused, and once the list grows past a cap it collapses to one bounding
//!    rect. The output is a short list cheap to iterate in copy-out.
//!
//! 2. **Buffer age.** Under double buffering the buffer handed back by
//!    `begin_frame` already holds the frame from *age* presents ago. So the
//!    region that must be repainted into it is the union of damage from the last
//!    *age* frames — not just this frame's. [`DamageTracker::flush`] keeps a
//!    short ring of recent frame-damage and unions the right span; age `0` (or an
//!    age deeper than our history) means "contents undefined, repaint all".

use crate::geom::IRect;

/// Past how many rects we stop tracking them individually and just merge to a
/// bounding box. Keeps copy-out's per-rect overhead bounded.
const MERGE_CAP: usize = 16;

/// How many recent frames of damage we remember for buffer-age unioning. Two
/// covers standard double buffering; a little headroom is cheap.
const HISTORY: usize = 4;

/// Accumulates damage for the in-progress frame and remembers recent frames so a
/// buffer of any age can be brought current.
#[derive(Debug, Default)]
pub struct DamageTracker {
    /// Damage reported so far this frame, post-merge.
    current: Vec<IRect>,
    /// Per-frame *bounding* damage for the last [`HISTORY`] presented frames,
    /// most-recent last. Bounding (not the full list) keeps this tiny; age-based
    /// repaint slightly over-paints, which is correct and cheap.
    history: Vec<IRect>,
}

impl DamageTracker {
    pub fn new() -> Self {
        DamageTracker::default()
    }

    /// True if nothing has been damaged since the last [`flush`](Self::flush) —
    /// the event loop uses this to skip presenting entirely (idle = 0% CPU).
    pub fn is_clean(&self) -> bool {
        self.current.is_empty()
    }

    /// Record a damaged region. Empty rects are dropped; otherwise the rect is
    /// merged into the running set.
    pub fn add(&mut self, r: IRect) {
        if r.is_empty() {
            return;
        }
        // Fuse with an existing rect if that doesn't waste much area, else push.
        for existing in &mut self.current {
            if should_merge(*existing, r) {
                *existing = existing.union(r);
                return;
            }
        }
        self.current.push(r);
        if self.current.len() > MERGE_CAP {
            self.collapse();
        }
    }

    /// Replace the running set with its single bounding rectangle.
    fn collapse(&mut self) {
        let bound = self.current.iter().fold(IRect::EMPTY, |a, &b| a.union(b));
        self.current.clear();
        if !bound.is_empty() {
            self.current.push(bound);
        }
    }

    /// Finish the frame: clamp damage to the surface, fold in the last `age`
    /// frames of history (so an aged back buffer is fully refreshed), record this
    /// frame in history, and return the regions to repaint and copy out.
    ///
    /// `age == 0`, or an age deeper than our retained history, yields a single
    /// full-surface rect — the buffer's prior contents are unknown.
    pub fn flush(&mut self, age: u32, surface_w: u32, surface_h: u32) -> Vec<IRect> {
        let full = IRect::from_wh(surface_w, surface_h);

        // This frame's own damage, clamped to the surface.
        let mut frame: Vec<IRect> = self
            .current
            .drain(..)
            .map(|r| r.clamp_to(surface_w, surface_h))
            .filter(|r| !r.is_empty())
            .collect();

        // Remember this frame's bounding damage for future aged buffers, before
        // we union history into the returned set.
        let this_bound = frame.iter().fold(IRect::EMPTY, |a, &b| a.union(b));

        let repaint = if age == 0 || (age as usize) > HISTORY {
            // Undefined / too-old buffer: repaint everything.
            vec![full]
        } else {
            // Union in the previous `age - 1` frames (this frame is `frame`).
            for past in self.history.iter().rev().take(age as usize - 1) {
                frame.push(*past);
            }
            merge_list(frame, surface_w, surface_h)
        };

        self.push_history(this_bound);
        repaint
    }

    fn push_history(&mut self, bound: IRect) {
        self.history.push(bound);
        if self.history.len() > HISTORY {
            self.history.remove(0);
        }
    }
}

/// Whether fusing `a` and `b` into their bounding box is worth it: cheap if they
/// already overlap, or if the box wastes less than ~25% over their areas.
fn should_merge(a: IRect, b: IRect) -> bool {
    if a.is_empty() || b.is_empty() {
        return true;
    }
    if !a.intersect(b).is_empty() {
        return true;
    }
    let union_area = a.union(b).area();
    let sum = a.area() + b.area();
    union_area * 4 <= sum * 5
}

/// Merge a rect list with the same heuristic, then cap its length by collapsing
/// to a bounding box if it is still long. Used when folding history into a frame.
fn merge_list(mut rects: Vec<IRect>, w: u32, h: u32) -> Vec<IRect> {
    let mut out: Vec<IRect> = Vec::with_capacity(rects.len());
    for r in rects.drain(..) {
        let r = r.clamp_to(w, h);
        if r.is_empty() {
            continue;
        }
        let mut merged = false;
        for e in &mut out {
            if should_merge(*e, r) {
                *e = e.union(r);
                merged = true;
                break;
            }
        }
        if !merged {
            out.push(r);
        }
    }
    if out.len() > MERGE_CAP {
        let bound = out.iter().fold(IRect::EMPTY, |a, &b| a.union(b));
        out.clear();
        out.push(bound);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_until_damaged() {
        let mut d = DamageTracker::new();
        assert!(d.is_clean());
        d.add(IRect::new(0, 0, 10, 10));
        assert!(!d.is_clean());
    }

    #[test]
    fn overlapping_rects_fuse() {
        let mut d = DamageTracker::new();
        d.add(IRect::new(0, 0, 10, 10));
        d.add(IRect::new(5, 5, 10, 10));
        let out = d.flush(1, 100, 100);
        assert_eq!(out, vec![IRect::new(0, 0, 15, 15)]);
    }

    #[test]
    fn distant_rects_stay_separate() {
        let mut d = DamageTracker::new();
        d.add(IRect::new(0, 0, 10, 10));
        d.add(IRect::new(80, 80, 10, 10));
        let out = d.flush(1, 100, 100);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn age_zero_repaints_everything() {
        let mut d = DamageTracker::new();
        d.add(IRect::new(0, 0, 10, 10));
        assert_eq!(d.flush(0, 100, 80), vec![IRect::from_wh(100, 80)]);
    }

    #[test]
    fn age_unions_previous_frames() {
        let mut d = DamageTracker::new();
        // Frame 1: damage top-left.
        d.add(IRect::new(0, 0, 10, 10));
        let _ = d.flush(1, 100, 100);
        // Frame 2: damage bottom-right; present into a 2-present-old buffer, so
        // the prior frame's region must be repainted too.
        d.add(IRect::new(80, 80, 10, 10));
        let out = d.flush(2, 100, 100);
        // Both regions present (they're distant -> not fused).
        assert!(out.contains(&IRect::new(80, 80, 10, 10)));
        assert!(out.contains(&IRect::new(0, 0, 10, 10)));
    }

    #[test]
    fn excess_rects_collapse_to_bound() {
        let mut d = DamageTracker::new();
        for i in 0..(MERGE_CAP as i32 + 4) {
            d.add(IRect::new(i * 50, 0, 5, 5)); // spread out so none fuse
        }
        // Collapsed to a single bounding rect once over the cap.
        assert_eq!(d.current.len(), 1);
    }
}
