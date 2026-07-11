//! Anchored placement for floating popups (menus, dropdowns, tooltips).
//!
//! [`place_anchored`] positions a box of a desired size against an anchor
//! rect on the logical surface: on the preferred side when it fits, flipped
//! to the opposite side when it doesn't *and* the opposite side has more
//! room, and always clamped to the surface (shrinking the box as a last
//! resort). This is [`Select`](crate::widgets::Select)'s menu-flip rule,
//! generalized to all four sides plus cross-axis alignment, so every
//! floating widget places itself the same way.

use fbui_render::geom::{Rect, Size};

/// Which side of the anchor the popup prefers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Under the anchor (a dropdown menu).
    Below,
    /// Over the anchor (a tooltip).
    Above,
    /// To the right of the anchor.
    Right,
    /// To the left of the anchor.
    Left,
}

/// Cross-axis alignment of the popup against the anchor: for [`Placement::Below`]
/// / [`Placement::Above`] this is horizontal (Start = left edges flush), for
/// [`Placement::Right`] / [`Placement::Left`] vertical (Start = top edges flush).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alignment {
    Start,
    Center,
    End,
}

/// How to place a popup against its anchor.
#[derive(Debug, Clone, Copy)]
pub struct AnchorSpec {
    pub placement: Placement,
    pub align: Alignment,
    /// Gap between the anchor edge and the popup, logical px.
    pub gap: f32,
}

impl AnchorSpec {
    /// `placement` with Start alignment and the conventional 2 px gap.
    pub fn new(placement: Placement) -> Self {
        AnchorSpec {
            placement,
            align: Alignment::Start,
            gap: 2.0,
        }
    }

    pub fn align(mut self, align: Alignment) -> Self {
        self.align = align;
        self
    }

    pub fn gap(mut self, gap: f32) -> Self {
        self.gap = gap;
        self
    }
}

impl Default for AnchorSpec {
    fn default() -> Self {
        AnchorSpec::new(Placement::Below)
    }
}

/// Place a `size` popup against `anchor` on a `surface` per `spec`.
///
/// Main axis (the placement direction): the preferred side is used when the
/// popup fits there *or* the preferred side has at least as much room as the
/// opposite one; otherwise the popup flips. Either way it is shrunk to the
/// room actually available. Cross axis: aligned per [`AnchorSpec::align`],
/// then clamped onto the surface (and shrunk to the surface extent if wider
/// than the whole screen). A zero-size `anchor` places against a point.
pub fn place_anchored(anchor: Rect, size: Size, surface: Size, spec: AnchorSpec) -> Rect {
    let vertical = matches!(spec.placement, Placement::Below | Placement::Above);

    // Main axis: room after (below/right of) and before (above/left of) the
    // anchor, then Select's rule — preferred side unless it's both too small
    // and smaller than the opposite side.
    let (a_start, a_end, main_extent, want_main) = if vertical {
        (anchor.y, anchor.bottom(), surface.h, size.h)
    } else {
        (anchor.x, anchor.right(), surface.w, size.w)
    };
    let after = main_extent - a_end - spec.gap;
    let before = a_start - spec.gap;
    let prefer_after = matches!(spec.placement, Placement::Below | Placement::Right);
    let (pref, opp) = if prefer_after {
        (after, before)
    } else {
        (before, after)
    };
    let use_pref = want_main <= pref || pref >= opp;
    let on_after_side = prefer_after == use_pref;
    let avail = if use_pref { pref } else { opp };
    let main_len = want_main.min(avail.max(0.0));
    let main_pos = if on_after_side {
        a_end + spec.gap
    } else {
        a_start - spec.gap - main_len
    };

    // Cross axis: align against the anchor, clamp onto the surface.
    let (c_start, c_len, cross_extent, want_cross) = if vertical {
        (anchor.x, anchor.w, surface.w, size.w)
    } else {
        (anchor.y, anchor.h, surface.h, size.h)
    };
    let cross_len = want_cross.min(cross_extent);
    let raw = match spec.align {
        Alignment::Start => c_start,
        Alignment::Center => c_start + (c_len - cross_len) / 2.0,
        Alignment::End => c_start + c_len - cross_len,
    };
    let cross_pos = raw.clamp(0.0, (cross_extent - cross_len).max(0.0));

    if vertical {
        Rect::new(cross_pos, main_pos, cross_len, main_len)
    } else {
        Rect::new(main_pos, cross_pos, main_len, cross_len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SURFACE: Size = Size::new(320.0, 240.0);

    fn spec(placement: Placement) -> AnchorSpec {
        AnchorSpec::new(placement)
    }

    #[test]
    fn fits_below() {
        let anchor = Rect::new(40.0, 20.0, 100.0, 30.0);
        let r = place_anchored(anchor, Size::new(100.0, 80.0), SURFACE, spec(Placement::Below));
        assert_eq!(r, Rect::new(40.0, 52.0, 100.0, 80.0));
    }

    #[test]
    fn flips_above_when_below_is_short_and_above_larger() {
        // Anchor near the bottom: 18 px below, plenty above.
        let anchor = Rect::new(40.0, 190.0, 100.0, 30.0);
        let r = place_anchored(anchor, Size::new(100.0, 80.0), SURFACE, spec(Placement::Below));
        assert_eq!(r, Rect::new(40.0, 190.0 - 2.0 - 80.0, 100.0, 80.0));
    }

    #[test]
    fn stays_below_when_below_has_more_room_even_if_too_small() {
        // 108 px below, 18 px above: stay below, shrunk.
        let anchor = Rect::new(40.0, 20.0, 100.0, 110.0);
        let r = place_anchored(anchor, Size::new(100.0, 200.0), SURFACE, spec(Placement::Below));
        assert_eq!(r, Rect::new(40.0, 132.0, 100.0, 240.0 - 130.0 - 2.0));
    }

    #[test]
    fn above_preferred_flips_below_when_no_headroom() {
        let anchor = Rect::new(40.0, 4.0, 100.0, 20.0);
        let r = place_anchored(anchor, Size::new(60.0, 50.0), SURFACE, spec(Placement::Above));
        assert_eq!(r, Rect::new(40.0, 26.0, 60.0, 50.0));
    }

    #[test]
    fn right_and_left_mirror_the_vertical_rule() {
        let anchor = Rect::new(20.0, 100.0, 40.0, 30.0);
        let r = place_anchored(anchor, Size::new(80.0, 60.0), SURFACE, spec(Placement::Right));
        assert_eq!(r, Rect::new(62.0, 100.0, 80.0, 60.0));
        // Anchor hugging the right edge: Right flips to Left.
        let anchor = Rect::new(290.0, 100.0, 25.0, 30.0);
        let r = place_anchored(anchor, Size::new(80.0, 60.0), SURFACE, spec(Placement::Right));
        assert_eq!(r, Rect::new(290.0 - 2.0 - 80.0, 100.0, 80.0, 60.0));
    }

    #[test]
    fn cross_axis_alignment_and_clamp() {
        let anchor = Rect::new(100.0, 50.0, 40.0, 20.0);
        let center =
            place_anchored(anchor, Size::new(80.0, 30.0), SURFACE, spec(Placement::Below).align(Alignment::Center));
        assert_eq!(center.x, 100.0 + (40.0 - 80.0) / 2.0);
        let end =
            place_anchored(anchor, Size::new(80.0, 30.0), SURFACE, spec(Placement::Below).align(Alignment::End));
        assert_eq!(end.x, 100.0 + 40.0 - 80.0);
        // Anchor at the left edge, centered popup would go negative: clamped to 0.
        let anchor = Rect::new(2.0, 50.0, 10.0, 20.0);
        let clamped =
            place_anchored(anchor, Size::new(80.0, 30.0), SURFACE, spec(Placement::Below).align(Alignment::Center));
        assert_eq!(clamped.x, 0.0);
    }

    #[test]
    fn tiny_surface_shrinks_both_axes() {
        let surface = Size::new(60.0, 40.0);
        let anchor = Rect::new(10.0, 10.0, 20.0, 10.0);
        let r = place_anchored(anchor, Size::new(100.0, 100.0), surface, spec(Placement::Below));
        assert!(r.w <= surface.w && r.h <= surface.h);
        assert!(r.x >= 0.0 && r.y >= 0.0);
        assert!(r.bottom() <= surface.h && r.right() <= surface.w);
    }

    #[test]
    fn point_anchor_places_against_a_point() {
        let anchor = Rect::new(150.0, 120.0, 0.0, 0.0);
        let r = place_anchored(anchor, Size::new(60.0, 40.0), SURFACE, spec(Placement::Below));
        assert_eq!(r, Rect::new(150.0, 122.0, 60.0, 40.0));
    }

    #[test]
    fn select_parity() {
        // Byte-for-byte the old Select::menu_rect behavior: below unless the
        // menu is too tall for the room below AND above has more room.
        let surface = Size::new(320.0, 240.0);
        let field = Rect::new(30.0, 200.0, 120.0, 32.0);
        let h = 3.0 * 32.0 + 8.0; // 3 rows + padding
        let r = place_anchored(field, Size::new(field.w, h), surface, AnchorSpec::default());
        // below = 240-232-2 = 6, above = 198 -> flips above at full height.
        assert_eq!(r, Rect::new(30.0, 200.0 - 2.0 - h, 120.0, h));
    }
}
