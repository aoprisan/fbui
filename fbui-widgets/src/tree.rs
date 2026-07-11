//! The retained widget tree and its frame loop.
//!
//! [`Ui`] owns everything: a `taffy` layout tree (one node per widget), the
//! widgets themselves, focus/hover/capture state, the theme, a [`FontContext`],
//! and the accumulated damage. It exposes a small surface:
//!
//! * **Build:** [`set_root`](Ui::set_root), [`add_child`](Ui::add_child).
//! * **Mutate:** [`with`](Ui::with) to reach a concrete widget by id.
//! * **Drive:** [`event`](Ui::event) feeds input, [`take_messages`](Ui::take_messages)
//!   drains emitted messages for `App::update`, [`paint`](Ui::paint) lays out (if
//!   needed) and repaints the damaged region into a [`Surface`].
//!
//! See `DESIGN.md` for the model; the borrow-discipline trick throughout is to
//! *destructure `&mut self`* into disjoint field references so a walk can hold
//! `&mut nodes` and `&mut fonts` at once.

use fbui_render::geom::{Point, Rect, Size};
use fbui_render::{FontContext, Scale, Surface};
use slotmap::{SecondaryMap, SlotMap};
use taffy::{AvailableSpace, TaffyTree};

use crate::ctx::{AnimCtx, CaptureOp, EventCtx, FocusOp, Outputs, PaintCtx, PopupOp};
use crate::event::{Event, Key, Modifiers, PointerButton};
use crate::popup::{place_anchored, Alignment, AnchorSpec, Placement};
use crate::style::Style;
use crate::theme::Theme;
use crate::util::text_style;
use crate::widget::Widget;

slotmap::new_key_type! {
    /// A stable, generational handle to a widget in the tree.
    pub struct WidgetId;
}

/// How an interactive popup behaves while open (see [`Ui::open_popup`]).
#[derive(Debug, Clone, Copy)]
pub struct PopupOptions {
    /// A pointer press/tap outside every open popup dismisses this one
    /// (delivering [`Event::PopupDismissed`] to its owner) and is consumed,
    /// so it can't activate whatever sits underneath. Default: `true`.
    pub dismiss_on_outside_click: bool,
    /// Move keyboard focus to the owner while the popup is open and restore
    /// the previous focus when it closes. Widgets that are themselves the
    /// focus target (a [`Select`](crate::widgets::Select) field) leave this
    /// off and manage focus themselves. Default: `true`.
    pub grab_focus: bool,
}

impl Default for PopupOptions {
    fn default() -> Self {
        PopupOptions {
            dismiss_on_outside_click: true,
            grab_focus: true,
        }
    }
}

/// One open popup: bottom of the stack first, topmost last.
struct PopupEntry {
    owner: WidgetId,
    opts: PopupOptions,
    /// Focus to restore on close, when `opts.grab_focus` moved it.
    prev_focus: Option<WidgetId>,
}

/// Tooltip box padding and anchor gap, logical px.
const TIP_PAD_X: f32 = 8.0;
const TIP_PAD_Y: f32 = 5.0;
const TIP_GAP: f32 = 6.0;

/// A hover tooltip attached to a widget via [`Ui::set_tooltip`].
///
/// This is a **`Ui` facility**, not a widget: hover is delivered only to the
/// deepest hit widget, so a wrapper widget could never see hover over its
/// children — the `Ui`, which owns hover, runs the show/hide state machine
/// instead. The dwell delay counts down on the frame clock
/// ([`Ui::animate`]), so it's deterministic and costs nothing once shown.
#[derive(Debug, Clone)]
pub struct Tooltip {
    /// The tip text.
    pub text: String,
    /// Hover dwell before showing, seconds.
    pub delay: f32,
    /// Preferred side of the owner (flips when there's no room).
    pub placement: Placement,
}

impl Tooltip {
    /// A tooltip with the standard dwell (0.6 s), preferring above the owner.
    pub fn new(text: impl Into<String>) -> Self {
        Tooltip {
            text: text.into(),
            delay: 0.6,
            placement: Placement::Above,
        }
    }

    pub fn delay(mut self, seconds: f32) -> Self {
        self.delay = seconds;
        self
    }

    pub fn placement(mut self, placement: Placement) -> Self {
        self.placement = placement;
        self
    }
}

/// The `Ui`'s tooltip state machine (at most one tip armed or visible).
#[derive(Default)]
struct TipState {
    /// Hover dwell in progress: (owner, seconds remaining).
    armed: Option<(WidgetId, f32)>,
    /// The visible tip: (owner, its placed box).
    shown: Option<(WidgetId, Rect)>,
}

/// One node: the widget, its taffy peer, tree links, and resolved bounds.
struct Node<Msg> {
    widget: Box<dyn Widget<Msg>>,
    taffy: taffy::NodeId,
    parent: Option<WidgetId>,
    children: Vec<WidgetId>,
    /// Absolute logical bounds after the last layout (scroll offsets folded in).
    layout: Rect,
    /// The overlay rect this widget last painted (see
    /// [`Widget::overlay_rect`]), so a vanished overlay's pixels can be
    /// damaged even after the widget stops reporting it.
    last_overlay: Option<Rect>,
}

/// The widget tree plus its layout, input, and paint machinery.
pub struct Ui<Msg> {
    taffy: TaffyTree<WidgetId>,
    nodes: SlotMap<WidgetId, Node<Msg>>,
    root: Option<WidgetId>,
    /// Logical size of the surface the tree lays out within.
    size: Size,
    scale: Scale,
    theme: Theme,
    fonts: FontContext,
    focus: Option<WidgetId>,
    hover: Option<WidgetId>,
    capture: Option<WidgetId>,
    /// Open interactive popups, bottom-to-top (see [`open_popup`](Ui::open_popup)).
    popups: Vec<PopupEntry>,
    /// Hover tooltips by owner (see [`set_tooltip`](Ui::set_tooltip)).
    tooltips: SecondaryMap<WidgetId, Tooltip>,
    tip: TipState,
    /// Scratch sink lent to each `EventCtx`; drained after every dispatch.
    out: Outputs<Msg>,
    /// Messages awaiting `App::update`.
    messages: Vec<Msg>,
    /// Logical damage awaiting the next paint.
    damage: Vec<Rect>,
    needs_layout: bool,
    /// At least one widget has a running animation; drive [`animate`](Self::animate).
    animating: bool,
    /// A pending [`request_screenshot`](Self::request_screenshot) destination.
    screenshot: Option<std::path::PathBuf>,
}

impl<Msg: 'static> Ui<Msg> {
    /// Create an empty tree sized to `size` logical pixels at `scale`, with an
    /// empty font database (load fonts before drawing text, or use
    /// [`with_fonts`](Self::with_fonts)).
    pub fn new(size: Size, scale: Scale, theme: Theme) -> Self {
        Self::with_fonts(size, scale, theme, FontContext::new())
    }

    /// As [`new`](Self::new), but with a caller-built [`FontContext`] — the way
    /// to bundle a fixed font so text renders without depending on host fonts.
    pub fn with_fonts(size: Size, scale: Scale, theme: Theme, fonts: FontContext) -> Self {
        Ui {
            taffy: TaffyTree::new(),
            nodes: SlotMap::with_key(),
            root: None,
            size,
            scale,
            theme,
            fonts,
            focus: None,
            hover: None,
            capture: None,
            popups: Vec::new(),
            tooltips: SecondaryMap::new(),
            tip: TipState::default(),
            out: Outputs::default(),
            messages: Vec::new(),
            damage: Vec::new(),
            needs_layout: true,
            animating: false,
            screenshot: None,
        }
    }

    // ---- building --------------------------------------------------------

    /// Install the root widget, replacing any existing tree.
    pub fn set_root(&mut self, widget: impl Widget<Msg>) -> WidgetId {
        // Drop the old tree wholesale (simple + correct for a root swap).
        self.taffy = TaffyTree::new();
        self.nodes.clear();
        self.focus = None;
        self.hover = None;
        self.capture = None;
        self.popups.clear();
        self.tooltips.clear();
        self.tip = TipState::default();
        let id = self.insert(Box::new(widget), None);
        self.root = Some(id);
        self.mark_full();
        id
    }

    /// Append `widget` as the last child of `parent`.
    pub fn add_child(&mut self, parent: WidgetId, widget: impl Widget<Msg>) -> WidgetId {
        let id = self.insert(Box::new(widget), Some(parent));
        let (parent_taffy, child_taffy) = (self.nodes[parent].taffy, self.nodes[id].taffy);
        let _ = self.taffy.add_child(parent_taffy, child_taffy);
        self.nodes[parent].children.push(id);
        // Now that the parent link exists, re-resolve the child's style: a child
        // of a stacking container ([`Stack`]) is positioned to fill it.
        self.apply_style(id);
        self.mark_full();
        id
    }

    /// Remove a widget and its whole subtree from the tree, damaging whatever
    /// it (and any overlay it painted) occupied. Focus, hover, and pointer
    /// capture pointing into the removed subtree are cleared. Stale ids into
    /// the subtree simply stop resolving — slotmap keys are generational.
    ///
    /// This is how transient structure comes down: a dismissed
    /// [`Dialog`](crate::widgets::Dialog), a closed popover. (Removing it —
    /// rather than leaving an invisible full-size node — also keeps dormant
    /// overlays from disabling the scroll-blit fast path underneath.)
    pub fn remove(&mut self, id: WidgetId) {
        if !self.nodes.contains_key(id) {
            return;
        }
        // Collect the subtree, damaging as we go.
        let mut ids = Vec::new();
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            let Some(node) = self.nodes.get(n) else {
                continue;
            };
            ids.push(n);
            self.damage.push(node.layout);
            if let Some(o) = node.last_overlay {
                self.damage.push(o.inset(-1.0));
            }
            stack.extend(node.children.iter().copied());
        }

        // Detach from the parent (tree + taffy handle their own sides).
        if let Some(p) = self.nodes[id].parent {
            if let Some(pn) = self.nodes.get_mut(p) {
                pn.children.retain(|&c| c != id);
            }
        }
        if self.root == Some(id) {
            self.root = None;
        }
        // Drop popup entries whose owner is going away, restoring grabbed
        // focus first (unless the restore target is going away too). The
        // overlay pixels were already damaged above via `last_overlay`.
        for i in (0..self.popups.len()).rev() {
            if ids.contains(&self.popups[i].owner) {
                let entry = self.popups.remove(i);
                if entry.opts.grab_focus {
                    let prev = entry.prev_focus.filter(|p| !ids.contains(p));
                    self.set_focus(prev);
                }
            }
        }
        for &n in &ids {
            let t = self.nodes[n].taffy;
            let _ = self.taffy.remove(t);
            self.nodes.remove(n);
            self.tooltips.remove(n);
            if self.focus == Some(n) {
                self.focus = None;
            }
            if self.hover == Some(n) {
                self.hover = None;
            }
            if self.capture == Some(n) {
                self.capture = None;
            }
        }
        if self.tip.armed.is_some_and(|(t, _)| ids.contains(&t)) {
            self.tip.armed = None;
        }
        if self.tip.shown.is_some_and(|(t, _)| ids.contains(&t)) {
            self.hide_tip();
        }
        self.needs_layout = true;
    }

    /// Recompute and install a node's taffy style from its widget (and parent).
    fn apply_style(&mut self, id: WidgetId) {
        let style = self.resolved_style(id);
        let taffy = self.nodes[id].taffy;
        let _ = self.taffy.set_style(taffy, style);
    }

    /// The taffy style a node contributes, augmented by its parent: a child of a
    /// container that [`stacks_children`](Widget::stacks_children) is positioned
    /// `absolute` filling that container, so a [`Stack`](crate::widgets::Stack)'s
    /// children overlap instead of flowing.
    fn resolved_style(&self, id: WidgetId) -> Style {
        let node = &self.nodes[id];
        let mut style = node.widget.layout_style(&self.theme);
        if let Some(parent) = node.parent {
            if self
                .nodes
                .get(parent)
                .is_some_and(|p| p.widget.stacks_children())
            {
                style.position = taffy::Position::Absolute;
                let zero = taffy::LengthPercentageAuto::length(0.0);
                style.inset = taffy::Rect {
                    left: zero,
                    right: zero,
                    top: zero,
                    bottom: zero,
                };
            }
        }
        style
    }

    fn insert(&mut self, widget: Box<dyn Widget<Msg>>, parent: Option<WidgetId>) -> WidgetId {
        let style = widget.layout_style(&self.theme);
        let taffy_node = self.taffy.new_leaf(style).expect("taffy new_leaf");
        let id = self.nodes.insert(Node {
            widget,
            taffy: taffy_node,
            parent,
            children: Vec::new(),
            layout: Rect::new(0.0, 0.0, 0.0, 0.0),
            last_overlay: None,
        });
        // The node context lets the measure callback find the widget by id.
        let _ = self.taffy.set_node_context(taffy_node, Some(id));
        // The new widget may animate from birth (a Spinner). Tick once; the
        // next `animate` clears this again if nothing is actually running —
        // the same conservative arm `with` uses.
        self.animating = true;
        id
    }

    // ---- mutation --------------------------------------------------------

    /// Reach a concrete widget by id to mutate it, refreshing its layout style
    /// and damaging it. Returns the closure's value, or `None` if the id is stale
    /// or the type doesn't match.
    ///
    /// This is the retained-tree equivalent of "set a property": call it from
    /// `App::update` to push new state into a widget.
    pub fn with<W: Widget<Msg> + 'static, R>(
        &mut self,
        id: WidgetId,
        f: impl FnOnce(&mut W) -> R,
    ) -> Option<R> {
        let node = self.nodes.get_mut(id)?;
        let w = node.widget.as_any_mut().downcast_mut::<W>()?;
        let r = f(w);
        let layout = node.layout;
        // The widget may have changed size or appearance; refresh style + damage.
        // `resolved_style` re-applies any parent-imposed positioning (stacks).
        self.apply_style(id);
        self.damage.push(layout);
        self.damage_overlay(id);
        self.needs_layout = true;
        // A programmatic mutation may have started an animation (e.g. retargeting
        // a tween). Tick once; `animate` clears this again if nothing is running.
        self.animating = true;
        Some(r)
    }

    /// Mark a widget's bounds for repaint without mutating it.
    pub fn request_paint(&mut self, id: WidgetId) {
        if let Some(node) = self.nodes.get(id) {
            self.damage.push(node.layout);
        }
    }

    /// Damage a widget's floating overlay — both where it is now and where it
    /// was last painted — so overlay appearance/disappearance/movement always
    /// repaints cleanly. The rects are padded by one logical pixel because
    /// overlay ink can straddle the rect edge (a centered 1px border stroke
    /// leaves an anti-aliased halo just outside it).
    fn damage_overlay(&mut self, id: WidgetId) {
        let Some(node) = self.nodes.get(id) else {
            return;
        };
        let now = node.widget.overlay_rect(node.layout, self.size);
        let last = node.last_overlay;
        if let Some(o) = now {
            self.damage.push(o.inset(-1.0));
        }
        if let Some(o) = last {
            self.damage.push(o.inset(-1.0));
        }
    }

    // ---- popups ------------------------------------------------------------

    /// Promote `owner`'s floating overlay ([`Widget::overlay_rect`]) into an
    /// interactive **popup**: pointer events inside the overlay rect route to
    /// `owner` ahead of capture and tree hit-testing, presses outside dismiss
    /// it (per `opts`, delivering [`Event::PopupDismissed`]) and are consumed,
    /// scrolls outside are swallowed, and Tab is confined to `owner`'s
    /// subtree. Popups stack: the most recently opened is topmost.
    ///
    /// The owner's [`prepare_overlay`](Widget::prepare_overlay) is called
    /// first (with font access) so the overlay can size itself. No-op for a
    /// stale id or an already-open popup. Widgets open their own popup from
    /// an event handler with [`EventCtx::open_popup`](crate::EventCtx::open_popup);
    /// this method is for opening from `App::update` (after arming the widget
    /// via [`with`](Ui::with)).
    pub fn open_popup(&mut self, owner: WidgetId, opts: PopupOptions) {
        if !self.nodes.contains_key(owner) || self.popups.iter().any(|e| e.owner == owner) {
            return;
        }
        // The overlay is placed against the owner's laid-out bounds; make
        // sure they're current before the first damage is computed.
        self.layout_now();
        {
            let Self {
                nodes,
                fonts,
                theme,
                size,
                ..
            } = self;
            if let Some(node) = nodes.get_mut(owner) {
                node.widget.prepare_overlay(fonts, theme, *size);
            }
        }
        let prev_focus = self.focus;
        self.popups.push(PopupEntry {
            owner,
            opts,
            prev_focus,
        });
        if opts.grab_focus {
            self.set_focus(Some(owner));
        }
        self.damage_overlay(owner);
    }

    /// Close `owner`'s popup — the reverse of [`open_popup`](Ui::open_popup).
    /// No [`Event::PopupDismissed`] is delivered: closing explicitly means the
    /// caller already knows. No-op if `owner` has no open popup.
    pub fn close_popup(&mut self, owner: WidgetId) {
        let Some(i) = self.popups.iter().position(|e| e.owner == owner) else {
            return;
        };
        let entry = self.popups.remove(i);
        self.damage_overlay(owner);
        if entry.opts.grab_focus {
            let prev = entry.prev_focus.filter(|p| self.nodes.contains_key(*p));
            self.set_focus(prev);
        }
    }

    /// The owner of the topmost open popup, if any.
    pub fn popup_owner(&self) -> Option<WidgetId> {
        self.popups.last().map(|e| e.owner)
    }

    /// Drop popup entries whose owner vanished or no longer reports an
    /// overlay — a widget closed by direct mutation
    /// ([`Select::set_options`](crate::widgets::Select::set_options) while
    /// open, say) never got to call `close_popup`, and a stale entry would
    /// keep consuming outside clicks. No [`Event::PopupDismissed`]: the
    /// widget already knows it's closed.
    fn prune_popups(&mut self) {
        for i in (0..self.popups.len()).rev() {
            let owner = self.popups[i].owner;
            let alive = self
                .nodes
                .get(owner)
                .is_some_and(|n| n.widget.overlay_rect(n.layout, self.size).is_some());
            if !alive {
                let entry = self.popups.remove(i);
                self.damage_overlay(owner);
                if entry.opts.grab_focus {
                    let prev = entry.prev_focus.filter(|p| self.nodes.contains_key(*p));
                    self.set_focus(prev);
                }
            }
        }
    }

    /// Dismiss every popup at stack index `start` or above that opted into
    /// outside-click dismissal, top-down: each owner gets
    /// [`Event::PopupDismissed`], its overlay is damaged, and grabbed focus is
    /// restored. Collects the victims first so an owner reacting to the event
    /// (even re-opening) can't invalidate the walk.
    fn dismiss_popups_from(&mut self, start: usize) {
        let victims: Vec<WidgetId> = self.popups[start.min(self.popups.len())..]
            .iter()
            .filter(|e| e.opts.dismiss_on_outside_click)
            .map(|e| e.owner)
            .collect();
        for owner in victims.into_iter().rev() {
            let Some(i) = self.popups.iter().position(|e| e.owner == owner) else {
                continue;
            };
            let entry = self.popups.remove(i);
            self.dispatch_to(owner, &Event::PopupDismissed);
            self.damage_overlay(owner);
            if entry.opts.grab_focus {
                let prev = entry.prev_focus.filter(|p| self.nodes.contains_key(*p));
                self.set_focus(prev);
            }
        }
    }

    // ---- tooltips ----------------------------------------------------------

    /// Attach a hover [`Tooltip`] to `id`, replacing any existing one. The tip
    /// shows after the dwell delay while the pointer rests on `id` (or any
    /// descendant), immediately on a long-press (touch), and hides on hover
    /// change, any press/release, or a key.
    pub fn set_tooltip(&mut self, id: WidgetId, tip: Tooltip) {
        if self.nodes.contains_key(id) {
            self.tooltips.insert(id, tip);
        }
    }

    /// Remove `id`'s tooltip, hiding it if currently visible.
    pub fn clear_tooltip(&mut self, id: WidgetId) {
        self.tooltips.remove(id);
        if self.tip.armed.is_some_and(|(t, _)| t == id) {
            self.tip.armed = None;
        }
        if self.tip.shown.is_some_and(|(t, _)| t == id) {
            self.hide_tip();
        }
    }

    /// The nearest ancestor of `id` (inclusive) with a tooltip attached.
    fn tooltip_target(&self, mut cur: Option<WidgetId>) -> Option<WidgetId> {
        while let Some(c) = cur {
            if self.tooltips.contains_key(c) {
                return Some(c);
            }
            cur = self.nodes.get(c).and_then(|n| n.parent);
        }
        None
    }

    /// Measure and place `target`'s tooltip, mark it shown, damage its box.
    fn show_tip(&mut self, target: WidgetId) {
        self.tip.armed = None;
        if self.tip.shown.is_some_and(|(t, _)| t == target) {
            return;
        }
        self.hide_tip();
        let Self {
            nodes,
            fonts,
            theme,
            tooltips,
            size,
            ..
        } = self;
        let (Some(tip), Some(node)) = (tooltips.get(target), nodes.get(target)) else {
            return;
        };
        let st = text_style(theme, theme.metrics.font_size, theme.palette.text);
        let ts = fonts.layout(&tip.text, &st, None).size();
        let box_size = Size::new(ts.w + 2.0 * TIP_PAD_X, ts.h + 2.0 * TIP_PAD_Y);
        let rect = place_anchored(
            node.layout,
            box_size,
            *size,
            AnchorSpec {
                placement: tip.placement,
                align: Alignment::Center,
                gap: TIP_GAP,
            },
        );
        self.tip.shown = Some((target, rect));
        self.damage.push(rect.inset(-1.0));
    }

    /// Hide a visible tooltip, damaging where it was.
    fn hide_tip(&mut self) {
        if let Some((_, rect)) = self.tip.shown.take() {
            self.damage.push(rect.inset(-1.0));
        }
    }

    // ---- accessors -------------------------------------------------------

    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    /// Swap the theme and repaint everything.
    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
        // Styles can be theme-derived; refresh them all, then full-repaint.
        let ids: Vec<WidgetId> = self.nodes.keys().collect();
        for id in ids {
            self.apply_style(id);
        }
        self.mark_full();
    }

    pub fn size(&self) -> Size {
        self.size
    }

    pub fn scale(&self) -> Scale {
        self.scale
    }

    /// Resize the surface (logical size + scale); forces a full relayout/repaint.
    pub fn set_size(&mut self, size: Size, scale: Scale) {
        self.size = size;
        self.scale = scale;
        // A visible tooltip was placed against the old surface; drop it.
        self.tip = TipState::default();
        // Open popups size/place themselves against the surface; let them
        // re-measure for the new one. (`mark_full` repaints everything.)
        for i in 0..self.popups.len() {
            let owner = self.popups[i].owner;
            let Self {
                nodes,
                fonts,
                theme,
                size,
                ..
            } = self;
            if let Some(node) = nodes.get_mut(owner) {
                node.widget.prepare_overlay(fonts, theme, *size);
            }
        }
        self.mark_full();
    }

    /// The currently focused widget, if any.
    pub fn focused(&self) -> Option<WidgetId> {
        self.focus
    }

    /// The resolved absolute logical bounds of a widget after the last layout.
    /// `None` for a stale id or before the first layout.
    pub fn bounds(&self, id: WidgetId) -> Option<Rect> {
        self.nodes.get(id).map(|n| n.layout)
    }

    /// Force a layout pass now (so [`bounds`](Ui::bounds) is current) without
    /// painting. Normally layout happens lazily inside [`event`](Ui::event) /
    /// [`paint`](Ui::paint); this is for headless callers and tests.
    pub fn layout_now(&mut self) {
        if self.needs_layout {
            self.relayout();
        }
    }

    /// Drain the messages widgets have emitted since the last call.
    pub fn take_messages(&mut self) -> Vec<Msg> {
        std::mem::take(&mut self.messages)
    }

    /// Whether there is anything to repaint.
    pub fn needs_paint(&self) -> bool {
        self.needs_layout || !self.damage.is_empty()
    }

    /// Advance every widget's animation by `dt` seconds, accumulating damage for
    /// the ones that changed. Returns `true` if any widget is still animating, so
    /// the runner knows whether to keep the frame clock spinning.
    ///
    /// This is the hook kinetic scrolling rides on: a flung [`ScrollView`] /
    /// [`List`] coasts here until its velocity decays.
    ///
    /// [`ScrollView`]: crate::widgets::ScrollView
    /// [`List`]: crate::widgets::List
    pub fn animate(&mut self, dt: f32) -> bool {
        crate::span!("ui.animate");
        let ids: Vec<WidgetId> = self.nodes.keys().collect();
        let mut running = false;
        let mut msgs = Vec::new();
        for id in ids {
            let mut actx = AnimCtx {
                dt,
                messages: &mut msgs,
            };
            let anim = self.nodes[id].widget.animate_with(&mut actx);
            if anim.relayout {
                self.needs_layout = true;
            }
            if let Some(d) = anim.damage {
                // A precise rect (scroll-blit strip / moved thumb); the bulk was
                // shifted by `scroll_blit`, applied in `paint`.
                self.damage.push(d);
            } else if anim.repaint || anim.relayout {
                let b = self.nodes[id].layout;
                self.damage.push(b);
            }
            if anim.repaint || anim.relayout || anim.damage.is_some() {
                // An animating widget with a floating overlay (fading toasts)
                // changed it too; the overlay rect is its real footprint.
                self.damage_overlay(id);
            }
            running |= anim.running;
        }
        // Tooltip dwell countdown — Ui-level, on the same deterministic frame
        // clock. The clock only runs while the dwell is pending; a shown tip
        // costs nothing.
        if let Some((target, remaining)) = self.tip.armed {
            let left = remaining - dt;
            if left <= 0.0 {
                self.show_tip(target); // clears `armed`
            } else {
                self.tip.armed = Some((target, left));
                running = true;
            }
        }
        // Messages emitted from a tick (key auto-repeat) join the same queue as
        // event-emitted ones; the runner drains them right after `animate`.
        self.messages.append(&mut msgs);
        self.animating = running;
        running
    }

    /// Whether any widget has a running animation, so the runner knows to keep
    /// calling [`animate`](Self::animate) on the frame clock.
    pub fn is_animating(&self) -> bool {
        self.animating
    }

    fn mark_full(&mut self) {
        self.needs_layout = true;
        self.damage
            .push(Rect::new(0.0, 0.0, self.size.w, self.size.h));
    }

    // ---- layout ----------------------------------------------------------

    fn relayout(&mut self) {
        crate::span!("ui.layout");
        let Some(root) = self.root else {
            self.needs_layout = false;
            return;
        };
        let avail = taffy::Size {
            width: AvailableSpace::Definite(self.size.w),
            height: AvailableSpace::Definite(self.size.h),
        };

        let Self {
            taffy,
            nodes,
            fonts,
            theme,
            ..
        } = self;
        let root_taffy = nodes[root].taffy;
        let _ = taffy.compute_layout_with_measure(
            root_taffy,
            avail,
            |known, available, _node, ctx, _style| {
                let Some(&mut wid) = ctx else {
                    return taffy::Size::ZERO;
                };
                let Some(node) = nodes.get_mut(wid) else {
                    return taffy::Size::ZERO;
                };
                match node.widget.measure(fonts, theme, known, available) {
                    Some(s) => taffy::Size {
                        width: s.w,
                        height: s.h,
                    },
                    None => taffy::Size {
                        width: known.width.unwrap_or(0.0),
                        height: known.height.unwrap_or(0.0),
                    },
                }
            },
        );

        self.place(root, 0.0, 0.0);
        self.needs_layout = false;
    }

    /// Walk the tree assigning absolute logical bounds, folding scroll offsets in.
    fn place(&mut self, id: WidgetId, ox: f32, oy: f32) {
        let taffy_node = self.nodes[id].taffy;
        let layout = match self.taffy.layout(taffy_node) {
            Ok(l) => *l,
            Err(_) => return,
        };
        let rect = Rect::new(
            ox + layout.location.x,
            oy + layout.location.y,
            layout.size.width,
            layout.size.height,
        );
        self.nodes[id].layout = rect;

        // Feed scrolling widgets their content vs. viewport extents so they can
        // clamp their offset before we read it.
        if self.nodes[id].widget.clips() {
            let content = Size::new(layout.content_size.width, layout.content_size.height);
            let viewport = Size::new(layout.size.width, layout.size.height);
            self.nodes[id].widget.set_scroll_metrics(content, viewport);
        }

        let offset = self.nodes[id].widget.content_offset();
        let (cox, coy) = (rect.x + offset.x, rect.y + offset.y);
        // Clone the small child list so the recursion doesn't hold a borrow.
        let children = self.nodes[id].children.clone();
        for c in children {
            self.place(c, cox, coy);
        }
    }

    // ---- input -----------------------------------------------------------

    /// Feed one input event into the tree. Emitted messages and damage are
    /// queued; call [`take_messages`](Ui::take_messages) / [`paint`](Ui::paint)
    /// after.
    pub fn event(&mut self, event: Event) {
        crate::span!("ui.event");
        if self.needs_layout {
            self.relayout();
        }

        // Tooltips are input-shy: any press, release, or key hides the tip
        // and cancels a pending dwell; a long-press (touch has no hover)
        // shows the target's tip immediately.
        match &event {
            Event::PointerDown { .. } | Event::PointerUp { .. } | Event::Key { .. } => {
                self.tip.armed = None;
                self.hide_tip();
            }
            Event::LongPress { pos } if self.popups.is_empty() => {
                let hit = self.root.and_then(|r| self.hit(r, *pos));
                if let Some(t) = self.tooltip_target(hit) {
                    self.show_tip(t);
                }
            }
            _ => {}
        }

        // Tab navigation is handled by the Ui, not delivered to widgets.
        if let Event::Key {
            key: Key::Tab,
            pressed: true,
            mods,
        } = event
        {
            self.move_focus(mods);
            return;
        }

        // Hover tracking on pointer motion.
        if let Event::PointerMove { pos } = event {
            self.update_hover(pos);
        }

        // Interactive popups intercept pointer events ahead of capture and
        // tree hit-testing (see `open_popup`). A drag in progress keeps
        // motion/release routing to its capture holder even across popup
        // rects, so a slider drag can't be hijacked by an open menu.
        if !self.popups.is_empty() {
            self.prune_popups();
        }
        if !self.popups.is_empty() {
            let capture_first = self.capture.is_some()
                && matches!(event, Event::PointerMove { .. } | Event::PointerUp { .. });
            if let Some(pos) = event.pointer_pos().filter(|_| !capture_first) {
                // Hit-test the popup stack top-down.
                let mut hit: Option<(usize, WidgetId)> = None;
                for (i, e) in self.popups.iter().enumerate().rev() {
                    let Some(node) = self.nodes.get(e.owner) else {
                        continue;
                    };
                    if node
                        .widget
                        .overlay_rect(node.layout, self.size)
                        .is_some_and(|r| r.contains_point(pos))
                    {
                        hit = Some((i, e.owner));
                        break;
                    }
                }
                let press = matches!(
                    event,
                    Event::PointerDown { .. } | Event::Tap { .. } | Event::LongPress { .. }
                );
                if let Some((i, owner)) = hit {
                    // A press into a lower popup collapses the ones stacked
                    // above it.
                    if press {
                        self.dismiss_popups_from(i + 1);
                    }
                    self.dispatch_to(owner, &event);
                    return;
                }
                // Outside every popup.
                let dismissable = self.popups.iter().any(|e| e.opts.dismiss_on_outside_click);
                if dismissable && press {
                    // Consumed: a click-away must not activate what's
                    // underneath the popup it dismisses.
                    self.dismiss_popups_from(0);
                    return;
                }
                if dismissable && matches!(event, Event::Scroll { .. }) {
                    // Swallowed: the page must not scroll under an open menu.
                    return;
                }
                // Moves and releases fall through to normal routing.
            }
        }

        let target = self.target_for(&event);
        if let Some(id) = target {
            let bubbles = matches!(
                event,
                Event::Scroll { .. }
                    | Event::Key { .. }
                    | Event::Tap { .. }
                    | Event::LongPress { .. }
                    | Event::Fling { .. }
                    | Event::PointerDown {
                        button: PointerButton::Right,
                        ..
                    }
            );
            if bubbles {
                // Scrolls, keys, recognized gestures, and right-button presses
                // bubble: the target (deepest widget under the pointer / the
                // focused widget) gets first refusal, then its ancestors — so
                // a wheel or fling over a label inside a ScrollView scrolls
                // the view, Esc inside a Dialog dismisses it, and a
                // long-press/right-click on any child opens the enclosing
                // ContextMenu. Left-button presses/releases stay direct: two
                // widgets arming on one press would double-activate.
                let mut cur = Some(id);
                while let Some(c) = cur {
                    if self.dispatch_to(c, &event) {
                        break;
                    }
                    cur = self.nodes.get(c).and_then(|n| n.parent);
                }
            } else {
                self.dispatch_to(id, &event);
            }
        }
    }

    /// Deliver `key` to the focused widget as if typed on a hardware keyboard
    /// (a synthetic pressed [`Event::Key`] with no modifiers). This is how an
    /// on-screen [`Keyboard`](crate::widgets::Keyboard)'s taps get from
    /// `App::update` into the focused field: it drives the *same* event path as
    /// real key input, so `on_change` fires, damage is queued, and `Tab` moves
    /// focus — none of which the lower-level
    /// [`TextInput::apply_key`](crate::widgets::TextInput::apply_key) does.
    pub fn send_key(&mut self, key: Key) {
        self.event(Event::Key {
            key,
            pressed: true,
            mods: Modifiers::default(),
        });
    }

    /// Ask the embedder to save what's on screen as a PNG at `path` — remote
    /// diagnostics for a device with no second screen. Call it from
    /// `App::update` (wire a debug gesture or an IPC command to it); the runner
    /// captures **after the next paint**, so the shot includes whatever the
    /// triggering update changed, and writes via `Surface::write_png`.
    ///
    /// The `Ui` only records the request: it owns no surface. A later request
    /// before the last one was taken replaces it. Embedders (the `fbui` runner,
    /// or a custom one) collect it with
    /// [`take_screenshot_request`](Self::take_screenshot_request).
    pub fn request_screenshot(&mut self, path: impl Into<std::path::PathBuf>) {
        self.screenshot = Some(path.into());
    }

    /// Take the pending screenshot destination, if any — the embedder half of
    /// [`request_screenshot`](Self::request_screenshot). The `fbui` runner
    /// calls this after painting each frame (and when idle with nothing to
    /// paint) and writes the surface out; a custom runner should do the same.
    pub fn take_screenshot_request(&mut self) -> Option<std::path::PathBuf> {
        self.screenshot.take()
    }

    /// Which widget should receive `event`: the capture holder for pointer
    /// events, the hit widget for other pointer events, or the focused widget for
    /// keys.
    fn target_for(&self, event: &Event) -> Option<WidgetId> {
        match event {
            Event::Key { .. } => self.focus,
            _ => {
                if let Some(pos) = event.pointer_pos() {
                    if let Some(cap) = self.capture {
                        return Some(cap);
                    }
                    self.root.and_then(|r| self.hit(r, pos))
                } else {
                    None
                }
            }
        }
    }

    fn update_hover(&mut self, pos: Point) {
        let hit = self.root.and_then(|r| self.hit(r, pos));
        if hit != self.hover {
            // Repaint both the widget losing and the one gaining hover.
            if let Some(old) = self.hover {
                if let Some(n) = self.nodes.get(old) {
                    self.damage.push(n.layout);
                }
                self.dispatch_to(old, &Event::PointerLeave);
            }
            self.hover = hit;
            if let Some(new) = hit {
                if let Some(n) = self.nodes.get(new) {
                    self.damage.push(n.layout);
                }
            }

            // Tooltip dwell follows the nearest tooltip-bearing ancestor of
            // the hover target; an unchanged target keeps its state (a shown
            // tip stays up while the pointer wanders within its owner).
            let new_target = self.tooltip_target(hit);
            let cur_target = self
                .tip
                .armed
                .map(|(t, _)| t)
                .or_else(|| self.tip.shown.map(|(t, _)| t));
            if new_target != cur_target {
                self.tip.armed = None;
                self.hide_tip();
                if let Some(t) = new_target {
                    self.tip.armed = Some((t, self.tooltips[t].delay));
                    self.animating = true;
                }
            }
        }
    }

    /// Deepest widget containing `pos`, honoring clip boundaries.
    fn hit(&self, id: WidgetId, pos: Point) -> Option<WidgetId> {
        let node = self.nodes.get(id)?;
        if node.widget.clips() && !contains(node.layout, pos) {
            return None;
        }
        for &c in node.children.iter().rev() {
            if let Some(h) = self.hit(c, pos) {
                return Some(h);
            }
        }
        if contains(node.layout, pos) {
            Some(id)
        } else {
            None
        }
    }

    /// Deliver `event` to one widget and apply its requests. Returns whether
    /// the widget marked the event handled (for bubbling).
    fn dispatch_to(&mut self, id: WidgetId, event: &Event) -> bool {
        let (hovered, focused) = (self.hover == Some(id), self.focus == Some(id));
        let surface = self.size;
        self.out.reset_for_event();

        let Self {
            nodes,
            fonts,
            theme,
            out,
            ..
        } = self;
        let Some(node) = nodes.get_mut(id) else {
            return false;
        };
        let mut ctx = EventCtx {
            event,
            bounds: node.layout,
            surface,
            theme,
            fonts,
            hovered,
            focused,
            self_id: id,
            out,
        };
        node.widget.event(&mut ctx);

        let handled = self.out.handled;
        self.apply_outputs();
        handled
    }

    fn apply_outputs(&mut self) {
        // Move messages + damage out of the scratch sink.
        self.messages.append(&mut self.out.messages);
        self.damage.append(&mut self.out.damage);

        if self.out.relayout || self.out.scroll_layout {
            self.needs_layout = true;
        }
        if self.out.relayout {
            self.damage
                .push(Rect::new(0.0, 0.0, self.size.w, self.size.h));
        }
        if self.out.animate {
            self.animating = true;
            self.out.animate = false;
        }
        if let Some(op) = self.out.capture.take() {
            match op {
                CaptureOp::Set(id) => self.capture = Some(id),
                CaptureOp::Clear => self.capture = None,
            }
        }
        // Popups before focus, so an explicit `request_focus` in the same
        // event overrides `open_popup`'s focus grab.
        if let Some(op) = self.out.popup.take() {
            match op {
                PopupOp::Open(id, opts) => self.open_popup(id, opts),
                PopupOp::Close(id) => self.close_popup(id),
            }
        }
        if let Some(op) = self.out.focus.take() {
            self.apply_focus(op);
        }
        self.out.relayout = false;
        self.out.scroll_layout = false;
    }

    // ---- focus -----------------------------------------------------------

    fn apply_focus(&mut self, op: FocusOp) {
        let new = match op {
            FocusOp::Request(id) => Some(id),
            FocusOp::Clear => None,
            FocusOp::Next => self.adjacent_focus(true),
            FocusOp::Prev => self.adjacent_focus(false),
        };
        self.set_focus(new);
    }

    fn move_focus(&mut self, mods: Modifiers) {
        let new = self.adjacent_focus(!mods.shift);
        self.set_focus(new);
    }

    fn set_focus(&mut self, new: Option<WidgetId>) {
        if new == self.focus {
            return;
        }
        if let Some(old) = self.focus {
            if let Some(n) = self.nodes.get(old) {
                self.damage.push(n.layout);
            }
        }
        self.focus = new;
        if let Some(id) = new {
            if let Some(n) = self.nodes.get(id) {
                self.damage.push(n.layout);
            }
        }
    }

    /// Focus the first focusable widget inside `id`'s subtree (pre-order),
    /// returning whether one was found. Call it after adding a modal
    /// [`Dialog`](crate::widgets::Dialog) so keys (Esc, Tab) land inside it.
    pub fn focus_first(&mut self, id: WidgetId) -> bool {
        let mut order = Vec::new();
        self.collect_focusable(id, &mut order);
        match order.first() {
            Some(&f) => {
                self.set_focus(Some(f));
                true
            }
            None => false,
        }
    }

    /// The nearest ancestor of `id` (inclusive) that traps focus, if any.
    fn trap_ancestor(&self, id: WidgetId) -> Option<WidgetId> {
        let mut cur = Some(id);
        while let Some(c) = cur {
            let n = self.nodes.get(c)?;
            if n.widget.traps_focus() {
                return Some(c);
            }
            cur = n.parent;
        }
        None
    }

    /// Tab order = pre-order DFS over focusable widgets. Returns the focusable
    /// after (or before) the current focus, wrapping around. When a popup is
    /// open, the cycle is confined to the topmost popup owner's subtree (a
    /// menu must not tab out to the page under it); otherwise, when the
    /// current focus sits inside a focus trap (a modal dialog), the cycle is
    /// confined to that trap's subtree.
    fn adjacent_focus(&self, forward: bool) -> Option<WidgetId> {
        let scope = self
            .popups
            .last()
            .map(|e| e.owner)
            .or_else(|| self.focus.and_then(|f| self.trap_ancestor(f)))
            .or(self.root);
        let mut order = Vec::new();
        if let Some(scope) = scope {
            self.collect_focusable(scope, &mut order);
        }
        if order.is_empty() {
            // Nowhere to go: keep the current focus (an open popup's grab)
            // rather than dropping it.
            return self.focus;
        }
        let cur = self.focus.and_then(|f| order.iter().position(|&i| i == f));
        let next = match cur {
            Some(i) if forward => (i + 1) % order.len(),
            Some(i) => (i + order.len() - 1) % order.len(),
            None if forward => 0,
            None => order.len() - 1,
        };
        Some(order[next])
    }

    fn collect_focusable(&self, id: WidgetId, out: &mut Vec<WidgetId>) {
        if let Some(node) = self.nodes.get(id) {
            if node.widget.focusable() {
                out.push(id);
            }
            for &c in &node.children {
                self.collect_focusable(c, out);
            }
        }
    }

    // ---- paint -----------------------------------------------------------

    /// Lay out if needed, then repaint the damaged region into `surface`.
    ///
    /// The whole damaged region is repainted (clipped), so parent backgrounds
    /// under a dirty child are correct; subtrees that don't intersect the region
    /// are skipped. The surface's own damage tracker bounds the copy-out.
    pub fn paint(&mut self, surface: &mut Surface) {
        crate::span!("ui.paint");
        if self.needs_layout {
            self.relayout();
        }
        // Scroll-blit fast path: any widget with a pending vertical scroll has its
        // existing pixels shifted in place (a sequential memmove) rather than
        // re-rasterized; only the exposed strip is added to the repaint set. The
        // widget separately damaged the small bits that don't shift (e.g. a moved
        // scrollbar thumb), so this stays correct.
        let ids: Vec<WidgetId> = self.nodes.keys().collect();
        for id in ids {
            if let Some(dy) = self.nodes[id].widget.scroll_blit() {
                let bounds = self.nodes[id].layout;
                // The shift moves whatever pixels occupy the bounds — including
                // anything painted *over* the widget (a Stack overlay). In that
                // case reusing them would drag the overlay along; fall back to
                // repainting the widget in full.
                if self.overlaid(id, bounds) {
                    self.damage.push(bounds);
                } else {
                    let strip = surface.scroll_region(bounds, dy);
                    self.damage.push(strip);
                }
            }
        }
        if self.damage.is_empty() {
            return;
        }
        let Some(root) = self.root else {
            self.damage.clear();
            return;
        };

        // The repaint region: union of all pending damage, clamped to the surface.
        let region = self
            .damage
            .drain(..)
            .fold(Rect::new(0.0, 0.0, 0.0, 0.0), union_rect);
        let region = intersect_rect(region, Rect::new(0.0, 0.0, self.size.w, self.size.h));
        if region.is_empty() {
            return;
        }
        // Snap the region *out* to whole device pixels. The region-sized
        // background clear (and every draw clipped to it) must fully own its
        // boundary pixels: a fractional edge anti-aliases against the previous
        // frame's pixels, and repeated incremental repaints then never converge
        // to what a full repaint produces.
        let f = self.scale.factor();
        let (x0, y0) = ((region.x * f).floor() / f, (region.y * f).floor() / f);
        let (x1, y1) = (
            (region.right() * f).ceil() / f,
            (region.bottom() * f).ceil() / f,
        );
        let region = intersect_rect(
            Rect::new(x0, y0, x1 - x0, y1 - y0),
            Rect::new(0.0, 0.0, self.size.w, self.size.h),
        );

        // Floating overlays (open dropdowns, toasts) paint on top of the whole
        // tree, in tree order. Any overlay intersecting the region must repaint
        // — the base pass just painted underneath it.
        let mut overlays: Vec<(WidgetId, Rect)> = Vec::new();
        self.collect_overlays(root, &mut overlays);

        let Self {
            nodes,
            fonts,
            theme,
            hover,
            focus,
            size,
            tooltips,
            tip,
            ..
        } = self;
        let (hover, focus, size) = (*hover, *focus, *size);
        let tip_shown = tip.shown;
        surface.paint(|p| {
            p.push_clip(region);
            // Clear the region to the window background first.
            p.fill_rect(region, theme.palette.bg);
            paint_node(p, fonts, theme, nodes, root, hover, focus, region);
            for &(id, rect) in &overlays {
                // The 1px pad matches `damage_overlay`: border ink can sit
                // just outside the reported rect.
                if intersect_rect(rect.inset(-1.0), region).is_empty() {
                    continue;
                }
                let Some(node) = nodes.get(id) else { continue };
                let mut ctx = PaintCtx {
                    painter: p,
                    fonts,
                    theme,
                    bounds: rect,
                    region,
                    hovered: hover == Some(id),
                    focused: focus == Some(id),
                };
                node.widget.paint_overlay(&mut ctx);
            }
            // A visible tooltip paints above everything, overlays included.
            if let Some((owner, rect)) = tip_shown {
                if !intersect_rect(rect.inset(-1.0), region).is_empty() {
                    if let Some(t) = tooltips.get(owner) {
                        let st = text_style(theme, theme.metrics.font_size, theme.palette.text);
                        p.fill_rounded_rect(rect, 4.0, theme.palette.surface_alt);
                        p.stroke_rounded_rect(rect, 4.0, theme.palette.line, 1.0);
                        fonts.draw_text(
                            p,
                            &t.text,
                            &st,
                            Point::new(rect.x + TIP_PAD_X, rect.y + TIP_PAD_Y),
                            None,
                        );
                    }
                }
            }
            p.pop_clip();
        });

        // Remember where each overlay painted, so its pixels can be damaged
        // after it changes or vanishes.
        let ids: Vec<WidgetId> = self.nodes.keys().collect();
        for id in ids {
            let node = &self.nodes[id];
            let o = node.widget.overlay_rect(node.layout, size);
            self.nodes[id].last_overlay = o;
        }
    }

    /// DFS-collect the widgets currently reporting a floating overlay, in tree
    /// (paint) order.
    fn collect_overlays(&self, id: WidgetId, out: &mut Vec<(WidgetId, Rect)>) {
        let Some(node) = self.nodes.get(id) else {
            return;
        };
        if let Some(rect) = node.widget.overlay_rect(node.layout, self.size) {
            out.push((id, rect));
        }
        for &c in &node.children {
            self.collect_overlays(c, out);
        }
    }

    /// Whether anything painted *after* `id`'s subtree — a later sibling at any
    /// ancestor level, i.e. later in z-order — overlaps `bounds`. Those pixels
    /// sit on top of `id`'s, so an in-place shift of the region would corrupt
    /// them. Conservative: an overlapping node counts even if it painted
    /// nothing, so dormant overlays should be removed from the tree (or sized
    /// empty), not merely skipped in `paint`.
    fn overlaid(&self, id: WidgetId, bounds: Rect) -> bool {
        // A visible tooltip paints on top of everything.
        if let Some((_, r)) = self.tip.shown {
            if !intersect_rect(r.inset(-1.0), bounds).is_empty() {
                return true;
            }
        }
        // Floating overlays paint on top of everything, wherever their owner
        // sits in the tree (including where it *last* painted, if it's mid-
        // dismissal this frame). Padded by the same 1px ink halo as
        // `damage_overlay`.
        for (nid, node) in self.nodes.iter() {
            if nid == id {
                continue;
            }
            let rects = [
                node.widget.overlay_rect(node.layout, self.size),
                node.last_overlay,
            ];
            for o in rects.into_iter().flatten() {
                if !intersect_rect(o.inset(-1.0), bounds).is_empty() {
                    return true;
                }
            }
        }
        let mut cur = id;
        while let Some(parent) = self.nodes.get(cur).and_then(|n| n.parent) {
            let children = &self.nodes[parent].children;
            if let Some(pos) = children.iter().position(|&c| c == cur) {
                for &later in &children[pos + 1..] {
                    if self.subtree_intersects(later, bounds) {
                        return true;
                    }
                }
            }
            cur = parent;
        }
        false
    }

    /// Whether any node in `id`'s subtree has bounds overlapping `bounds`.
    fn subtree_intersects(&self, id: WidgetId, bounds: Rect) -> bool {
        let Some(node) = self.nodes.get(id) else {
            return false;
        };
        if !intersect_rect(node.layout, bounds).is_empty() {
            return true;
        }
        node.children
            .iter()
            .any(|&c| self.subtree_intersects(c, bounds))
    }
}

/// Recursively paint `id` and its subtree, skipping anything outside `region`.
#[allow(clippy::too_many_arguments)]
fn paint_node<Msg: 'static>(
    p: &mut fbui_render::Painter,
    fonts: &mut FontContext,
    theme: &Theme,
    nodes: &SlotMap<WidgetId, Node<Msg>>,
    id: WidgetId,
    hover: Option<WidgetId>,
    focus: Option<WidgetId>,
    region: Rect,
) {
    let Some(node) = nodes.get(id) else { return };
    let b = node.layout;
    if intersect_rect(b, region).is_empty() {
        return;
    }

    let mut ctx = PaintCtx {
        painter: p,
        fonts,
        theme,
        bounds: b,
        region,
        hovered: hover == Some(id),
        focused: focus == Some(id),
    };
    node.widget.paint(&mut ctx);

    let clips = node.widget.clips();
    if clips {
        p.push_clip(b);
    }
    // Children further constrained to the clip region (if any).
    let child_region = if clips {
        intersect_rect(b, region)
    } else {
        region
    };
    for &c in &node.children {
        paint_node(p, fonts, theme, nodes, c, hover, focus, child_region);
    }
    if clips {
        p.pop_clip();
    }
}

/// Point-in-rect for logical f32 geometry (half-open).
fn contains(r: Rect, p: Point) -> bool {
    p.x >= r.x && p.x < r.right() && p.y >= r.y && p.y < r.bottom()
}

fn union_rect(a: Rect, b: Rect) -> Rect {
    if a.is_empty() {
        return b;
    }
    if b.is_empty() {
        return a;
    }
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    let right = a.right().max(b.right());
    let bottom = a.bottom().max(b.bottom());
    Rect::new(x, y, right - x, bottom - y)
}

fn intersect_rect(a: Rect, b: Rect) -> Rect {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let right = a.right().min(b.right());
    let bottom = a.bottom().min(b.bottom());
    if right <= x || bottom <= y {
        Rect::new(0.0, 0.0, 0.0, 0.0)
    } else {
        Rect::new(x, y, right - x, bottom - y)
    }
}
