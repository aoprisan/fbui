//! The lint pass and the diagnostics counters.
//!
//! Each rule gets a tree that trips it and (where the distinction matters) a
//! neighbouring tree that must *not* trip it: a lint that cries wolf gets
//! switched off, and then it catches nothing.

use fbui_render::geom::Size;
use fbui_render::{FontContext, Scale};
use fbui_widgets::lint::Rule;
use fbui_widgets::widgets::{Button, Container, Dialog, Label, ScrollView, TextInput};
use fbui_widgets::{PaintCtx, Style, Theme, Ui, Widget};

#[derive(Clone, Debug, PartialEq)]
enum Msg {}

fn tree(w: f32, h: f32) -> Ui<Msg> {
    Ui::with_fonts(
        Size::new(w, h),
        Scale::ONE,
        Theme::dark(),
        FontContext::with_default_font(),
    )
}

/// A focusable leaf of an exact size that says nothing about itself — the
/// shape both the `touch-target` and `undescribed` rules exist for, and a
/// stand-in for the third-party widget those rules are really aimed at.
struct Tiny(f32, f32);

impl Widget<Msg> for Tiny {
    fn layout_style(&self, _theme: &Theme) -> Style {
        let size = taffy::Size {
            width: taffy::Dimension::length(self.0),
            height: taffy::Dimension::length(self.1),
        };
        Style {
            size,
            min_size: size,
            ..Style::default()
        }
    }

    fn paint(&self, _ctx: &mut PaintCtx) {}

    fn focusable(&self) -> bool {
        true
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

fn rules(ui: &mut Ui<Msg>) -> Vec<Rule> {
    ui.lint().into_iter().map(|l| l.rule).collect()
}

/// A tidy tree produces nothing. This is the rule the whole module lives by,
/// and the reason the shipped examples are lint-clean.
#[test]
fn a_well_formed_tree_produces_no_findings() {
    let mut ui = tree(400.0, 300.0);
    let root = ui.set_root(Container::column().fill().padding(12.0).gap(8.0));
    ui.add_named(root, "title", Label::new("Hello"));
    ui.add_named(root, "go", Button::<Msg>::new("Go"));
    ui.add_named(root, "field", TextInput::<Msg>::new());
    assert_eq!(rules(&mut ui), Vec::<Rule>::new(), "{:?}", ui.lint());
}

/// The intentional one: a button forced below the touch minimum.
#[test]
fn a_tiny_button_is_a_touch_target_finding() {
    let mut ui = tree(400.0, 300.0);
    let root = ui.set_root(Container::column().fill());
    // A 16x16 tappable target is a real mistake on a touch panel.
    ui.add_named(root, "tiny", Tiny(16.0, 16.0));

    let found = ui.lint();
    let touch: Vec<_> = found
        .iter()
        .filter(|l| l.rule == Rule::TouchTarget)
        .collect();
    assert_eq!(touch.len(), 1, "{found:?}");
    assert_eq!(touch[0].name.as_deref(), Some("tiny"));
    assert!(touch[0].message.contains("touch minimum"));

    // The same widget trips `undescribed`: it is focusable and says nothing,
    // so no dump or flow can assert anything about it.
    assert!(
        found.iter().any(|l| l.rule == Rule::Undescribed),
        "{found:?}"
    );

    // The escape hatch silences exactly that widget and rule.
    let id = ui.find("tiny").unwrap();
    ui.allow_lint(id, Rule::TouchTarget);
    assert!(!rules(&mut ui).contains(&Rule::TouchTarget));
}

/// The threshold is a policy, not a constant: a touch-only kiosk raises it and
/// the toolkit's own 36px controls start being reported.
#[test]
fn the_touch_threshold_is_configurable() {
    let mut ui = tree(200.0, 100.0);
    let root = ui.set_root(Container::column().fill());
    ui.add_named(root, "b", Tiny(36.0, 36.0));
    assert!(
        !rules(&mut ui).contains(&Rule::TouchTarget),
        "36px passes 24"
    );
    ui.set_touch_target(44.0);
    assert!(rules(&mut ui).contains(&Rule::TouchTarget), "36px fails 44");
}

/// Text that does not fit its box, and does not wrap, is truncated on screen —
/// invisible in a tree dump, obvious to an eye.
#[test]
fn text_that_does_not_fit_is_reported_unless_it_wraps() {
    let long = "a sentence far too long for the box it was given";

    // A column of fixed width stretches its child across it, so the label is
    // given 60px of width for text that needs several hundred.
    let mut ui = tree(400.0, 300.0);
    let root = ui.set_root(Container::column().width(60.0).height(120.0));
    ui.add_named(root, "squeezed", Label::new(long));
    let found = ui.lint();
    let t: Vec<_> = found
        .iter()
        .filter(|l| l.rule == Rule::TruncatedText)
        .collect();
    assert_eq!(t.len(), 1, "{found:?}");
    assert!(t[0].message.contains("does not wrap"), "{:?}", t[0]);

    // The same text that wraps is fine.
    let mut wrapped = tree(400.0, 300.0);
    let root = wrapped.set_root(Container::column().width(60.0).height(400.0));
    wrapped.add_named(root, "wrapped", Label::new(long).wrap());
    assert!(!rules(&mut wrapped).contains(&Rule::TruncatedText));
}

/// A focusable widget that is entirely clipped away can be tabbed to and
/// nothing appears to happen.
#[test]
fn a_clipped_focusable_is_unreachable() {
    let mut ui = tree(200.0, 80.0);
    let root = ui.set_root(Container::column().fill());
    let scroll = ui.add_child(root, ScrollView::new());
    let col = ui.add_child(scroll, Container::column());
    for i in 0..20 {
        ui.add_named(col, format!("b{i}"), Button::<Msg>::new(format!("b{i}")));
    }
    let found = ui.lint();
    assert!(
        found.iter().any(|l| l.rule == Rule::UnreachableFocus),
        "{found:?}"
    );
    // ...but the ones on screen are not reported.
    assert!(!found
        .iter()
        .any(|l| l.rule == Rule::UnreachableFocus && l.name.as_deref() == Some("b0")));
}

/// Content scrolled below the fold is what a scroll view is *for*; only a
/// widget with no clipping ancestor is off-surface.
#[test]
fn scrolled_away_content_is_not_off_surface() {
    let mut ui = tree(200.0, 80.0);
    let root = ui.set_root(Container::column().fill());
    let scroll = ui.add_child(root, ScrollView::new());
    let col = ui.add_child(scroll, Container::column());
    for i in 0..20 {
        ui.add_named(col, format!("l{i}"), Label::new(format!("line {i}")));
    }
    let found = ui.lint();
    assert!(
        !found.iter().any(|l| l.rule == Rule::OffSurface),
        "{found:?}"
    );
}

/// A forgotten `add_child` leaves a viewport with nothing in it.
#[test]
fn an_empty_scroll_view_is_reported() {
    let mut ui = tree(200.0, 100.0);
    let root = ui.set_root(Container::column().fill());
    ui.add_named(root, "empty", ScrollView::new());
    assert!(rules(&mut ui).contains(&Rule::EmptyScroll));
}

/// Two modals at once means one is unreachable behind the other's scrim.
#[test]
fn two_dialogs_at_once_are_stacked_modals() {
    let mut ui = tree(300.0, 200.0);
    let root = ui.set_root(fbui_widgets::widgets::Stack::new());
    ui.add_named(root, "first", Dialog::<Msg>::new());
    assert!(
        !rules(&mut ui).contains(&Rule::StackedModals),
        "one is fine"
    );
    ui.add_named(root, "second", Dialog::<Msg>::new());
    assert!(rules(&mut ui).contains(&Rule::StackedModals));
}

/// A name claimed twice is reported, because the earlier widget silently stops
/// answering to it and a flow then addresses the wrong one.
#[test]
fn a_name_claimed_twice_is_reported() {
    let mut ui = tree(300.0, 200.0);
    let root = ui.set_root(Container::column().fill());
    ui.add_named(root, "go", Button::<Msg>::new("A"));
    let second = ui.add_named(root, "go", Button::<Msg>::new("B"));

    let found = ui.lint();
    assert!(
        found.iter().any(|l| l.rule == Rule::DuplicateName),
        "{found:?}"
    );
    assert_eq!(ui.find("go"), Some(second), "the last claim wins");
}

// ---- diagnostics ----------------------------------------------------------

/// The general form of the "an unchanged render produces no damage"
/// invariant: a test can assert *cost*, not only pixels.
#[test]
fn a_no_op_mutation_schedules_no_damage() {
    let mut ui = tree(200.0, 100.0);
    let root = ui.set_root(Container::column().fill());
    let label = ui.add_named(root, "l", Label::new("same"));
    ui.layout_now();
    ui.take_diagnostics();

    // `with` is a mutation by construction — it cannot know the closure
    // changed nothing — so it damages. That is the honest accounting, and
    // what a caller avoids by not calling `with` for a no-op.
    ui.with::<Label, _>(label, |l| l.set_text("same")).unwrap();
    let d = ui.take_diagnostics();
    assert_eq!(d.mutations, 1);
    assert!(d.damage_area > 0.0);

    // Doing nothing at all really does cost nothing.
    let d = ui.take_diagnostics();
    assert_eq!(d.mutations, 0);
    assert_eq!(d.damage_area, 0.0);
    assert_eq!(d.damage_rects, 0);
}

#[test]
fn counters_track_events_layouts_and_paints() {
    use fbui_render::Surface;

    let mut ui = tree(120.0, 60.0);
    let root = ui.set_root(Container::column().fill());
    ui.add_named(root, "b", Button::<Msg>::new("x"));
    let mut surface = Surface::new(120, 60, Scale::ONE);
    ui.paint(&mut surface);
    let d = ui.take_diagnostics();
    assert!(d.layouts >= 1 && d.paints == 1, "{d:?}");

    ui.event(fbui_widgets::Event::PointerMove {
        pos: fbui_render::geom::Point::new(10.0, 10.0),
    });
    assert_eq!(ui.take_diagnostics().events, 1);

    // Flush the hover's damage, then prove that painting a clean tree costs
    // nothing — the invariant, stated in counters.
    ui.paint(&mut surface);
    ui.take_diagnostics();
    ui.paint(&mut surface);
    ui.paint(&mut surface);
    assert_eq!(ui.take_diagnostics().paints, 0, "clean frames do no work");
}
