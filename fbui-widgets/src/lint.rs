//! Lints: what an eye would catch and a tree dump never will.
//!
//! A reader of text can check that a label says the right thing. What they
//! cannot check is that the label *fits*, that the button is big enough for a
//! finger, that nothing has drifted off the edge of the panel, or that a
//! scroll view someone forgot to fill is empty. Those are the mistakes a
//! glance at the screen catches instantly and a flow script never notices —
//! so [`Ui::lint`](crate::Ui::lint) walks the laid-out tree and reports them.
//!
//! The pass runs on demand: from `FBUI_LINT=1` after every layout, from
//! `expect no-lints` in a flow, or from a test. It costs one tree walk plus,
//! for the text rules, one intrinsic measure per text widget.
//!
//! ## Rules are conservative
//!
//! A lint that cries wolf gets switched off, and then it catches nothing. The
//! acceptance test for this module is that the shipped examples produce
//! **zero** findings, so every rule here is written to fire only when
//! something is actually wrong, and each has an escape hatch
//! ([`Ui::allow_lint`](crate::Ui::allow_lint)) for the deliberate case.

use fbui_render::geom::Rect;

use crate::tree::WidgetId;

/// Which rule a finding came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Rule {
    /// Two widgets were given the same name; the later one won, so the
    /// earlier is unreachable by `#name` and a flow silently addresses the
    /// wrong widget.
    DuplicateName,
    /// A tappable widget smaller than the touch-target minimum. Kiosks are
    /// touch, and a finger is about 9 mm wide.
    TouchTarget,
    /// A focusable widget that cannot be reached: no area, or entirely
    /// clipped away. Tab lands on it and nothing appears to happen.
    UnreachableFocus,
    /// A widget whose text does not fit the box it was laid out in, and which
    /// does not wrap — so the text is cut off.
    TruncatedText,
    /// A widget partly or wholly outside the surface.
    OffSurface,
    /// A container whose children do not fit inside it, though it neither
    /// clips nor stacks them.
    Overflow,
    /// A scroll viewport with no content — usually a forgotten `add_child`.
    EmptyScroll,
    /// Two modal dialogs are in the tree at once, so one is unreachable
    /// behind the other's scrim.
    StackedModals,
    /// A focusable widget that reports nothing from
    /// [`describe`](crate::Widget::describe), so it is a bare box in every
    /// dump and no flow can assert anything about it.
    Undescribed,
    /// A `Kind "text"` reference in a flow matches more than one widget, so
    /// the next layout change may flip which one it acts on. Reported by
    /// [`crate::script::ambiguous_refs`], not by the tree walk.
    AmbiguousRef,
}

impl Rule {
    /// The rule's kebab-case name, as it appears in output and in
    /// [`Ui::allow_lint`](crate::Ui::allow_lint).
    pub fn id(self) -> &'static str {
        match self {
            Rule::DuplicateName => "duplicate-name",
            Rule::TouchTarget => "touch-target",
            Rule::UnreachableFocus => "unreachable-focus",
            Rule::TruncatedText => "truncated-text",
            Rule::OffSurface => "off-surface",
            Rule::Overflow => "overflow",
            Rule::EmptyScroll => "empty-scroll",
            Rule::StackedModals => "stacked-modals",
            Rule::Undescribed => "undescribed",
            Rule::AmbiguousRef => "ambiguous-ref",
        }
    }
}

impl std::fmt::Display for Rule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.id())
    }
}

/// One finding: the rule, the widget it is about, and a one-line explanation.
#[derive(Debug, Clone, PartialEq)]
pub struct Lint {
    pub rule: Rule,
    /// The widget, when the finding is about one.
    pub id: Option<WidgetId>,
    /// Its type name (`Button`, `ScrollView`, …).
    pub kind: String,
    /// Its app-assigned name, if it has one.
    pub name: Option<String>,
    pub bounds: Rect,
    pub message: String,
}

impl std::fmt::Display for Lint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.rule, self.kind)?;
        if let Some(n) = &self.name {
            write!(f, " #{n}")?;
        }
        write!(
            f,
            " [{},{} {}x{}] {}",
            self.bounds.x.round() as i32,
            self.bounds.y.round() as i32,
            self.bounds.w.round() as i32,
            self.bounds.h.round() as i32,
            self.message
        )
    }
}

/// Default minimum tappable size in logical pixels.
///
/// The platform guidelines say 44–48; this toolkit's own controls are ~36
/// tall by theme, so the default flags what is *clearly* too small to hit
/// rather than what merely falls short of a guideline. Raise it with
/// [`Ui::set_touch_target`](crate::Ui::set_touch_target) — a kiosk that is
/// only ever used with a finger should set 44.
pub const DEFAULT_TOUCH_TARGET: f32 = 24.0;

/// Render findings the way `expect no-lints` and `FBUI_LINT` print them.
pub fn render(lints: &[Lint]) -> Vec<String> {
    lints.iter().map(|l| l.to_string()).collect()
}
