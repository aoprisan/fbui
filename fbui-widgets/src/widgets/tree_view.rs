//! [`TreeView`] — a hierarchical, expand/collapse counterpart to [`List`].
//!
//! Nodes are data, not child widgets: the tree is flattened to its *visible*
//! rows and painted with the same windowing, drag/kinetic scrolling, and
//! scroll-blit fast path as [`List`], so a huge collapsed tree costs what its
//! visible rows cost. Branch rows carry a disclosure triangle; rows indent by
//! depth.
//!
//! Nodes are addressed by a stable [`NodeId`] (assigned in depth-first
//! insertion order and unchanged by expand/collapse), which is what the
//! selection/toggle callbacks hand to the app.
//!
//! [`List`]: crate::widgets::List

use std::any::Any;

use fbui_render::geom::{Point, Rect};
use fbui_render::PathBuilder;

use crate::ctx::{EventCtx, PaintCtx};
use crate::event::{Event, Key, PointerButton};
use crate::kinetic::Kinetic;
use crate::style::{self, Style};
use crate::theme::Theme;
use crate::util::{text_style, union};
use crate::widget::{Anim, Widget};

const ROW_H: f32 = 40.0;
/// Horizontal indent per tree depth.
const INDENT: f32 = 20.0;
/// Width reserved at the start of a row for the disclosure triangle.
const DISCLOSURE_W: f32 = 24.0;
/// Left padding before the first indent level.
const PAD_X: f32 = 8.0;

/// Movement (logical px) past which a press becomes a scroll-drag rather than a
/// row tap.
const DRAG_SLOP: f32 = 6.0;

/// Stable identity of a tree node: its depth-first insertion index. Survives
/// expand/collapse (only [`TreeView::set_nodes`] renumbers).
pub type NodeId = usize;

/// One node of the tree an app hands to [`TreeView::new`] — a label plus
/// children, built literally:
///
/// ```
/// use fbui_widgets::widgets::TreeNode;
///
/// let fs = TreeNode::branch("etc", vec![
///     TreeNode::leaf("hosts"),
///     TreeNode::branch("ssh", vec![TreeNode::leaf("sshd_config")]).expanded(true),
/// ]);
/// ```
#[derive(Debug, Clone)]
pub struct TreeNode {
    label: String,
    children: Vec<TreeNode>,
    expanded: bool,
}

impl TreeNode {
    /// A childless node.
    pub fn leaf(label: impl Into<String>) -> Self {
        TreeNode {
            label: label.into(),
            children: Vec::new(),
            expanded: false,
        }
    }

    /// A node with children, collapsed by default.
    pub fn branch(label: impl Into<String>, children: Vec<TreeNode>) -> Self {
        TreeNode {
            label: label.into(),
            children,
            expanded: false,
        }
    }

    /// Set the initial expanded state (meaningful for branches).
    pub fn expanded(mut self, open: bool) -> Self {
        self.expanded = open;
        self
    }
}

/// Arena node: flat storage the recursive [`TreeNode`] input is lowered into.
struct Node {
    label: String,
    depth: usize,
    parent: Option<NodeId>,
    children: Vec<NodeId>,
    expanded: bool,
}

/// In-progress pointer drag over the tree (tap vs. scroll, as in `List`).
struct Drag {
    start_y: f32,
    last_y: f32,
    moved: bool,
}

/// A scrollable hierarchical tree of text rows with expand/collapse.
///
/// * **Tap** the disclosure triangle to toggle a branch; tap the row body to
///   select (any row).
/// * **Keys** (focused): Up/Down move through visible rows; Right expands a
///   branch (then descends), Left collapses (then ascends); Enter/Space select
///   the row, Space also toggling a branch.
/// * Wheel, drag, and fling scroll with the same kinetic + scroll-blit fast
///   path as [`List`](crate::widgets::List).
pub struct TreeView<Msg> {
    nodes: Vec<Node>,
    roots: Vec<NodeId>,
    /// The flattened visible rows, top to bottom (recomputed on any toggle).
    visible: Vec<NodeId>,
    row_h: f32,
    offset: f32,
    selected: Option<NodeId>,
    on_select: Option<Box<dyn Fn(NodeId) -> Msg>>,
    on_toggle: Option<Box<dyn Fn(NodeId, bool) -> Msg>>,
    /// Last bounds seen, so kinetic [`animate`](Widget::animate) can clamp and
    /// place the scrollbar without a layout context.
    bounds: Rect,
    drag: Option<Drag>,
    kinetic: Kinetic,
    /// Pending content shift (logical px) for the next [`scroll_blit`](Widget::scroll_blit).
    blit_dy: f32,
}

impl<Msg> TreeView<Msg> {
    pub fn new(roots: Vec<TreeNode>) -> Self {
        let mut tv = TreeView {
            nodes: Vec::new(),
            roots: Vec::new(),
            visible: Vec::new(),
            row_h: ROW_H,
            offset: 0.0,
            selected: None,
            on_select: None,
            on_toggle: None,
            bounds: Rect::new(0.0, 0.0, 0.0, 0.0),
            drag: None,
            kinetic: Kinetic::new(),
            blit_dy: 0.0,
        };
        tv.build(roots);
        tv
    }

    pub fn row_height(mut self, h: f32) -> Self {
        self.row_h = h;
        self
    }

    /// Message to emit when a row is selected (tap or Enter/Space).
    pub fn on_select(mut self, f: impl Fn(NodeId) -> Msg + 'static) -> Self {
        self.on_select = Some(Box::new(f));
        self
    }

    /// Message to emit when a branch is expanded/collapsed (the `bool` is the
    /// new expanded state).
    pub fn on_toggle(mut self, f: impl Fn(NodeId, bool) -> Msg + 'static) -> Self {
        self.on_toggle = Some(Box::new(f));
        self
    }

    /// Replace the whole tree (call via [`Ui::with`](crate::Ui::with)).
    /// Node ids are re-assigned; selection and scroll reset.
    pub fn set_nodes(&mut self, roots: Vec<TreeNode>) {
        self.build(roots);
        self.selected = None;
        self.offset = 0.0;
        self.kinetic.stop();
        self.drag = None;
        self.blit_dy = 0.0;
    }

    fn build(&mut self, roots: Vec<TreeNode>) {
        self.nodes.clear();
        self.roots.clear();
        fn lower<M>(
            tv: &mut TreeView<M>,
            n: TreeNode,
            depth: usize,
            parent: Option<NodeId>,
        ) -> NodeId {
            let id = tv.nodes.len();
            let expanded = n.expanded;
            tv.nodes.push(Node {
                label: n.label,
                depth,
                parent,
                children: Vec::new(),
                expanded,
            });
            for child in n.children {
                let c = lower(tv, child, depth + 1, Some(id));
                tv.nodes[id].children.push(c);
            }
            id
        }
        let mut roots_out = Vec::new();
        for r in roots {
            let id = lower(self, r, 0, None);
            roots_out.push(id);
        }
        self.roots = roots_out;
        self.reflatten();
    }

    fn reflatten(&mut self) {
        self.visible.clear();
        fn walk<M>(tv: &TreeView<M>, id: NodeId, out: &mut Vec<NodeId>) {
            out.push(id);
            if tv.nodes[id].expanded {
                for &c in &tv.nodes[id].children {
                    walk(tv, c, out);
                }
            }
        }
        let mut out = Vec::new();
        for &r in &self.roots {
            walk(self, r, &mut out);
        }
        self.visible = out;
    }

    /// Number of nodes in the whole tree (visible or not).
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The selected node, if any.
    pub fn selected(&self) -> Option<NodeId> {
        self.selected
    }

    /// A node's label.
    pub fn label(&self, id: NodeId) -> Option<&str> {
        self.nodes.get(id).map(|n| n.label.as_str())
    }

    /// Whether `id` has children.
    pub fn is_branch(&self, id: NodeId) -> bool {
        self.nodes.get(id).is_some_and(|n| !n.children.is_empty())
    }

    /// Whether a branch is currently expanded.
    pub fn is_expanded(&self, id: NodeId) -> bool {
        self.nodes.get(id).is_some_and(|n| n.expanded)
    }

    /// Set the selection programmatically (no message emitted, no scroll);
    /// `None` clears it. An out-of-range id clears too. Call via
    /// [`Ui::with`](crate::Ui::with); pair with a repaint of the widget.
    pub fn set_selected(&mut self, id: Option<NodeId>) {
        self.selected = id.filter(|&i| i < self.nodes.len());
    }

    /// Expand/collapse a branch programmatically (no message emitted). Call
    /// via [`Ui::with`](crate::Ui::with); pair with a repaint of the widget.
    pub fn set_expanded(&mut self, id: NodeId, open: bool) {
        if let Some(n) = self.nodes.get_mut(id) {
            if !n.children.is_empty() && n.expanded != open {
                n.expanded = open;
                self.reflatten();
            }
        }
    }

    fn total_h(&self) -> f32 {
        self.visible.len() as f32 * self.row_h
    }

    fn max_offset(&self, viewport_h: f32) -> f32 {
        (self.total_h() - viewport_h).max(0.0)
    }

    /// The scrollbar thumb rect at a given offset (padded for clean damage),
    /// or `None` when there's no overflow. Same geometry as `List`.
    fn thumb_rect(&self, offset: f32, b: Rect) -> Option<Rect> {
        let max_off = self.max_offset(b.h);
        if max_off <= 0.0 {
            return None;
        }
        let frac = (b.h / self.total_h()).clamp(0.0, 1.0);
        let thumb_h = (b.h * frac).max(24.0);
        let t = (offset / max_off).clamp(0.0, 1.0);
        let thumb_y = b.y + t * (b.h - thumb_h);
        Some(Rect::new(
            b.right() - 7.0,
            thumb_y - 1.0,
            7.0,
            thumb_h + 2.0,
        ))
    }

    /// Scroll by `dy` offset-pixels via the blit fast path (see `List`).
    fn scroll_blit_by(&mut self, dy: f32, b: Rect) -> Option<Rect> {
        let old = self.offset;
        let new = (old + dy).clamp(0.0, self.max_offset(b.h));
        if (new - old).abs() <= f32::EPSILON {
            return None;
        }
        self.offset = new;
        self.blit_dy += -(new - old);
        let old_thumb = self.thumb_rect(old, b);
        let new_thumb = self.thumb_rect(new, b);
        match (old_thumb, new_thumb) {
            (Some(a), Some(c)) => Some(union(a, c)),
            (a, c) => a.or(c),
        }
    }

    /// x offset (within the row) where a node's disclosure zone starts.
    fn node_indent(&self, id: NodeId) -> f32 {
        PAD_X + self.nodes[id].depth as f32 * INDENT
    }

    /// The visible-row index of a node, if it is currently visible.
    fn visible_index(&self, id: NodeId) -> Option<usize> {
        self.visible.iter().position(|&v| v == id)
    }

    /// Select a node (must be visible), emit, and keep it in view.
    fn select(&mut self, id: NodeId, ctx: &mut EventCtx<Msg>) {
        let Some(row) = self.visible_index(id) else {
            return;
        };
        self.selected = Some(id);
        if let Some(f) = &self.on_select {
            ctx.emit(f(id));
        }
        // Keep the selection in view (a jump — full repaint, not a blit).
        let b = ctx.bounds();
        let top = row as f32 * self.row_h;
        let bottom = top + self.row_h;
        if top < self.offset {
            self.offset = top;
        } else if bottom > self.offset + b.h {
            self.offset = bottom - b.h;
        }
        self.offset = self.offset.clamp(0.0, self.max_offset(b.h));
        ctx.request_paint();
    }

    /// Toggle a branch, emit, clamp the scroll to the new content height, and
    /// repaint. No-op on leaves.
    fn toggle(&mut self, id: NodeId, ctx: &mut EventCtx<Msg>) {
        if !self.is_branch(id) {
            return;
        }
        let open = !self.nodes[id].expanded;
        self.nodes[id].expanded = open;
        self.reflatten();
        // A collapse can orphan the selection (an ancestor closed over it);
        // move it to the collapsed branch itself so keyboard nav stays anchored.
        if let Some(sel) = self.selected {
            if self.visible_index(sel).is_none() {
                self.selected = Some(id);
            }
        }
        self.offset = self.offset.clamp(0.0, self.max_offset(ctx.bounds().h));
        if let Some(f) = &self.on_toggle {
            ctx.emit(f(id, open));
        }
        ctx.request_paint();
    }
}

impl<Msg: 'static> Widget<Msg> for TreeView<Msg> {
    fn layout_style(&self, _theme: &Theme) -> Style {
        Style {
            size: taffy::Size {
                width: style::percent(1.0),
                height: style::percent(1.0),
            },
            flex_grow: 1.0,
            ..Style::default()
        }
    }

    fn focusable(&self) -> bool {
        true
    }

    fn paint(&self, ctx: &mut PaintCtx) {
        let b = ctx.bounds();
        let region = ctx.region();
        let theme = ctx.theme();
        let (surface, accent, on_accent, muted, line) = (
            theme.palette.surface,
            theme.palette.accent,
            theme.palette.on_accent,
            theme.palette.muted,
            theme.palette.line,
        );
        let st_normal = text_style(theme, theme.metrics.font_size, theme.palette.text);
        let st_selected = text_style(theme, theme.metrics.font_size, on_accent);
        let font_size = theme.metrics.font_size;

        // Visible row window, bounded to the rows touching the damage region.
        let first = (self.offset / self.row_h).floor().max(0.0) as usize;
        let last = (((self.offset + b.h) / self.row_h).ceil() as usize).min(self.visible.len());
        let max_off = self.max_offset(b.h);
        let offset = self.offset;
        let row_h = self.row_h;
        let selected = self.selected;

        let (p, fonts) = ctx.painter_and_fonts();
        p.fill_rect(b, surface);
        p.push_clip(b);
        for (n, &id) in self.visible[first..last].iter().enumerate() {
            let row = first + n;
            let ry = b.y + row as f32 * row_h - offset;
            if ry + row_h <= region.y || ry >= region.bottom() {
                continue;
            }
            let node = &self.nodes[id];
            let indent = self.node_indent(id);
            let is_selected = selected == Some(id);
            let row_style = if is_selected {
                p.fill_rect(Rect::new(b.x, ry, b.w, row_h), accent);
                &st_selected
            } else {
                &st_normal
            };
            // Disclosure triangle for branches: ▶ collapsed, ▼ expanded.
            if !node.children.is_empty() {
                let cx = b.x + indent + DISCLOSURE_W / 2.0;
                let cy = ry + row_h / 2.0;
                let s = 5.0; // half-size of the triangle
                let mut pb = PathBuilder::new();
                if node.expanded {
                    pb.move_to(cx - s, cy - s / 2.0);
                    pb.line_to(cx + s, cy - s / 2.0);
                    pb.line_to(cx, cy + s);
                } else {
                    pb.move_to(cx - s / 2.0, cy - s);
                    pb.line_to(cx + s, cy);
                    pb.line_to(cx - s / 2.0, cy + s);
                }
                pb.close();
                if let Some(tri) = pb.finish() {
                    p.fill_path(&tri, if is_selected { on_accent } else { muted });
                }
            }
            fonts.draw_text(
                p,
                &node.label,
                row_style,
                Point::new(
                    b.x + indent + DISCLOSURE_W + 4.0,
                    ry + (row_h - font_size) / 2.0 - 1.0,
                ),
                None,
            );
        }
        // Scrollbar (cheap; always drawn, clipped to the region).
        if max_off > 0.0 {
            let frac = (b.h / self.total_h()).clamp(0.0, 1.0);
            let thumb_h = (b.h * frac).max(24.0);
            let t = offset / max_off;
            let thumb_y = b.y + t * (b.h - thumb_h);
            p.fill_rounded_rect(Rect::new(b.right() - 6.0, thumb_y, 4.0, thumb_h), 2.0, line);
        }
        p.pop_clip();
    }

    fn event(&mut self, ctx: &mut EventCtx<Msg>) {
        let b = ctx.bounds();
        self.bounds = b;
        let ev = ctx.event().clone();
        match ev {
            Event::Scroll { delta_y, .. } => {
                if let Some(dmg) = self.scroll_blit_by(delta_y, b) {
                    ctx.request_paint_rect(dmg);
                }
                ctx.set_handled();
            }
            Event::PointerDown {
                button: PointerButton::Left,
                pos,
            } => {
                self.kinetic.stop();
                ctx.request_focus();
                self.drag = Some(Drag {
                    start_y: pos.y,
                    last_y: pos.y,
                    moved: false,
                });
                ctx.capture_pointer();
                ctx.set_handled();
            }
            Event::PointerMove { pos } => {
                if let Some(drag) = &mut self.drag {
                    let dy = drag.last_y - pos.y;
                    drag.last_y = pos.y;
                    if (pos.y - drag.start_y).abs() > DRAG_SLOP {
                        drag.moved = true;
                    }
                    if let Some(dmg) = self.scroll_blit_by(dy, b) {
                        ctx.request_paint_rect(dmg);
                    }
                }
            }
            Event::PointerUp {
                button: PointerButton::Left,
                pos,
            } => {
                if let Some(drag) = self.drag.take() {
                    ctx.release_pointer();
                    if !drag.moved {
                        // Tap: which visible row is under the pointer?
                        let row = ((pos.y - b.y + self.offset) / self.row_h).floor();
                        if row >= 0.0 && (row as usize) < self.visible.len() {
                            let id = self.visible[row as usize];
                            let indent = self.node_indent(id);
                            let in_disclosure = self.is_branch(id)
                                && pos.x >= b.x + indent
                                && pos.x < b.x + indent + DISCLOSURE_W;
                            if in_disclosure {
                                self.toggle(id, ctx);
                            } else {
                                self.select(id, ctx);
                            }
                        }
                    }
                }
            }
            Event::Fling { velocity_y, .. } => {
                if self.max_offset(b.h) > 0.0 {
                    self.kinetic.start(-velocity_y);
                    ctx.request_anim();
                    ctx.set_handled();
                }
            }
            Event::Key {
                key, pressed: true, ..
            } if ctx.is_focused() => match key {
                Key::Down => {
                    let next = match self.selected.and_then(|s| self.visible_index(s)) {
                        Some(row) => (row + 1).min(self.visible.len().saturating_sub(1)),
                        None => 0,
                    };
                    if let Some(&id) = self.visible.get(next) {
                        self.select(id, ctx);
                    }
                    ctx.set_handled();
                }
                Key::Up => {
                    let prev = self
                        .selected
                        .and_then(|s| self.visible_index(s))
                        .unwrap_or(0)
                        .saturating_sub(1);
                    if let Some(&id) = self.visible.get(prev) {
                        self.select(id, ctx);
                    }
                    ctx.set_handled();
                }
                Key::Right => {
                    if let Some(id) = self.selected {
                        if self.is_branch(id) && !self.is_expanded(id) {
                            self.toggle(id, ctx);
                        } else if let Some(&child) = self.nodes[id]
                            .children
                            .first()
                            .filter(|_| self.is_expanded(id))
                        {
                            self.select(child, ctx);
                        }
                        ctx.set_handled();
                    }
                }
                Key::Left => {
                    if let Some(id) = self.selected {
                        if self.is_branch(id) && self.is_expanded(id) {
                            self.toggle(id, ctx);
                        } else if let Some(parent) = self.nodes[id].parent {
                            self.select(parent, ctx);
                        }
                        ctx.set_handled();
                    }
                }
                Key::Enter => {
                    if let Some(id) = self.selected {
                        self.select(id, ctx);
                        ctx.set_handled();
                    }
                }
                Key::Space => {
                    if let Some(id) = self.selected {
                        if self.is_branch(id) {
                            self.toggle(id, ctx);
                        } else {
                            self.select(id, ctx);
                        }
                        ctx.set_handled();
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    fn animate(&mut self, dt: f32) -> Anim {
        if !self.kinetic.is_running() {
            return Anim::IDLE;
        }
        let dy = self.kinetic.step(dt);
        let b = self.bounds;
        match self.scroll_blit_by(dy, b) {
            Some(dmg) => Anim {
                repaint: false,
                relayout: false,
                running: self.kinetic.is_running(),
                damage: Some(dmg),
            },
            None => {
                self.kinetic.stop();
                Anim::IDLE
            }
        }
    }

    fn scroll_blit(&mut self) -> Option<f32> {
        if self.blit_dy.abs() < f32::EPSILON {
            None
        } else {
            Some(std::mem::take(&mut self.blit_dy))
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
