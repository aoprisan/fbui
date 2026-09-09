//! The in-process flow executor: a flow script driving a real `Ui`.
//!
//! These pin the *meaning* of the grammar — what a `tap`, a `type`, an
//! `expect` do — which the runner and remote executors then reuse.

use fbui_render::geom::Size;
use fbui_render::{FontContext, Scale};
use fbui_widgets::harness;
use fbui_widgets::widgets::{Button, Checkbox, Container, Label, ScrollView, TextInput};
use fbui_widgets::{Theme, Ui, WidgetId};

#[derive(Clone, Debug, PartialEq)]
enum Msg {
    Inc,
    Dec,
    Agree(bool),
    Item(String),
}

/// A counter app: two buttons, a label, a checkbox and a text field — enough
/// to exercise taps, typing, keys and every expectation form.
struct App {
    count: i32,
    label: WidgetId,
    echo: WidgetId,
}

fn build() -> (Ui<Msg>, App) {
    let mut ui = Ui::<Msg>::with_fonts(
        Size::new(400.0, 300.0),
        Scale::ONE,
        Theme::dark(),
        FontContext::with_default_font(),
    );
    let root = ui.set_root(Container::column().fill().padding(12.0).gap(8.0));
    let label = ui.add_named(root, "count", Label::new("0"));
    let row = ui.add_named(root, "buttons", Container::row().gap(8.0));
    ui.add_named(row, "inc", Button::new("+").on_press(|| Msg::Inc));
    ui.add_named(row, "dec", Button::new("-").on_press(|| Msg::Dec));
    ui.add_named(
        root,
        "agree",
        Checkbox::new("I agree", false).on_toggle(Msg::Agree),
    );
    ui.add_named(root, "item", TextInput::new().on_change(Msg::Item));
    let echo = ui.add_named(root, "echo", Label::new(""));
    (
        ui,
        App {
            count: 0,
            label,
            echo,
        },
    )
}

fn update(app: &mut App, msg: Msg, ui: &mut Ui<Msg>) {
    match msg {
        Msg::Inc | Msg::Dec => {
            app.count += if msg == Msg::Inc { 1 } else { -1 };
            let text = app.count.to_string();
            ui.with::<Label, _>(app.label, |l| l.set_text(text));
        }
        Msg::Agree(on) => {
            ui.with::<Checkbox<Msg>, _>(ui.find("agree").unwrap(), |c| c.set_checked(on));
        }
        Msg::Item(text) => {
            ui.with::<Label, _>(app.echo, |l| l.set_text(text));
        }
    }
}

/// The loop from `TOOLING.md` §0, end to end: tap, type, key, assert.
#[test]
fn a_flow_taps_types_and_asserts() {
    let (mut ui, mut app) = build();
    let flow = r#"
fbui-rec 2
expect #count text "0"
tap #inc
tap #inc
expect #count text "2"
tap #dec
expect #count text "1"
tap #agree
expect #agree checked
tap #item
type "milk"
expect #item text "milk"
expect #echo text "milk"
expect Button "+"
expect #missing absent
"#;
    harness::assert_flow(&mut ui, flow, |m, ui| update(&mut app, m, ui));
    assert_eq!(app.count, 1);
}

/// A failing expectation names the line, the step, and what it actually saw —
/// and the report carries the tree, which is the thing that makes the failure
/// diagnosable without a screen.
#[test]
fn a_failed_expectation_reports_the_line_and_the_actual_value() {
    let (mut ui, mut app) = build();
    let flow = "fbui-rec 2\ntap #inc\nexpect #count text \"7\"\n";
    let report = harness::run_text(&mut ui, flow, |m, ui| update(&mut app, m, ui)).unwrap();
    assert!(!report.passed());
    assert_eq!(report.failures.len(), 1, "it stops at the first failure");
    let f = &report.failures[0];
    assert_eq!(f.line, 3);
    assert_eq!(f.source, "expect #count text \"7\"");
    assert!(
        f.message.contains("\"1\""),
        "the actual value: {}",
        f.message
    );
    assert!(report.tree.contains("Label #count"), "{}", report.tree);
}

/// A reference that matches nothing is the commonest authoring mistake, so it
/// fails loudly instead of tapping the void.
#[test]
fn an_unresolvable_reference_fails_the_step() {
    let (mut ui, mut app) = build();
    let report =
        harness::run_text(&mut ui, "tap #nope\n", |m, ui| update(&mut app, m, ui)).unwrap();
    assert!(!report.passed());
    assert!(report.failures[0].message.contains("matches no widget"));
}

/// A widget scrolled out of its viewport is in the tree but cannot be tapped;
/// acting on it is a failure, not a silent no-op.
#[test]
fn acting_on_a_clipped_widget_fails() {
    let mut ui = Ui::<Msg>::with_fonts(
        Size::new(200.0, 120.0),
        Scale::ONE,
        Theme::dark(),
        FontContext::with_default_font(),
    );
    let root = ui.set_root(Container::column().fill());
    let scroll = ui.add_child(root, ScrollView::new());
    let content = ui.add_child(scroll, Container::column());
    for i in 0..30 {
        ui.add_named(content, format!("row{i}"), Button::new(format!("row {i}")));
    }
    let report = harness::run_text(&mut ui, "tap #row29\n", |_m, _ui| {}).unwrap();
    assert!(!report.passed());
    assert!(
        report.failures[0].message.contains("not on screen"),
        "{}",
        report.failures[0].message
    );

    // ...and the expectation form says the same thing without acting.
    let report = harness::run_text(
        &mut ui,
        "expect #row29 hidden\nexpect #row0 visible\n",
        |_, _| {},
    )
    .unwrap();
    assert!(report.passed(), "{}", report.failure_text());
}

/// `Kind "text"` addressing works with no names at all, which is what lets a
/// flow be written against an app that was never instrumented.
#[test]
fn widgets_can_be_addressed_by_kind_and_text() {
    let mut ui = Ui::<Msg>::with_fonts(
        Size::new(300.0, 200.0),
        Scale::ONE,
        Theme::dark(),
        FontContext::with_default_font(),
    );
    let root = ui.set_root(Container::column().fill().gap(4.0));
    let label = ui.add_child(root, Label::new("idle"));
    ui.add_child(root, Button::new("Submit").on_press(|| Msg::Inc));

    let flow = "tap Button \"Submit\"\nexpect Label \"done\"\n";
    let report = harness::run_text(&mut ui, flow, |m, ui| {
        if m == Msg::Inc {
            ui.with::<Label, _>(label, |l| l.set_text("done"));
        }
    })
    .unwrap();
    assert!(report.passed(), "{}", report.failure_text());
}

/// Raw platform event lines belong to the runner; the harness says so rather
/// than skipping them and reporting a pass that means nothing.
#[test]
fn raw_event_lines_are_rejected_not_skipped() {
    let (mut ui, mut app) = build();
    let err = harness::run_text(&mut ui, "fbui-rec 2\n@0 m 5 5\n", |m, ui| {
        update(&mut app, m, ui)
    })
    .unwrap_err();
    assert!(err.to_string().contains("FBUI_REPLAY"), "{err}");
}

#[test]
fn a_parse_error_names_the_line() {
    let (mut ui, mut app) = build();
    let err = harness::run_text(&mut ui, "fbui-rec 2\ntap #inc\nwibble\n", |m, ui| {
        update(&mut app, m, ui)
    })
    .unwrap_err();
    assert!(err.to_string().starts_with("line 3:"), "{err}");
}

/// `wait settle` runs the frame clock until animations stop — bounded, so a
/// perpetual animation cannot hang a test.
#[test]
fn wait_settle_is_bounded() {
    use fbui_widgets::widgets::Spinner;
    let mut ui = Ui::<Msg>::with_fonts(
        Size::new(200.0, 200.0),
        Scale::ONE,
        Theme::dark(),
        FontContext::with_default_font(),
    );
    let root = ui.set_root(Container::column().fill());
    ui.add_named(root, "busy", Spinner::new());
    let report =
        harness::run_text(&mut ui, "wait settle\nexpect #busy running\n", |_, _| {}).unwrap();
    assert!(report.passed(), "{}", report.failure_text());
}

/// `expect no-lints` turns the lint pass into a test: a clean tree passes, and
/// a tree with a finding fails with the finding's text.
#[test]
fn expect_no_lints_reports_what_the_pass_found() {
    let (mut ui, mut app) = build();
    let report = harness::run_text(&mut ui, "expect no-lints\n", |m, ui| {
        update(&mut app, m, ui)
    })
    .unwrap();
    assert!(report.passed(), "{}", report.failure_text());

    // A viewport nobody filled is exactly the kind of thing a tree dump does
    // not make obvious.
    let mut ui2 = Ui::<Msg>::with_fonts(
        Size::new(200.0, 100.0),
        Scale::ONE,
        Theme::dark(),
        FontContext::with_default_font(),
    );
    let root = ui2.set_root(Container::column().fill());
    ui2.add_named(root, "empty", ScrollView::new());
    let report = harness::run_text(&mut ui2, "expect no-lints\n", |_, _| {}).unwrap();
    assert!(!report.passed());
    assert!(
        report.failures[0].message.contains("empty-scroll"),
        "{}",
        report.failures[0].message
    );
}

/// An ambiguous `Kind "text"` reference is fine while authoring and a hazard
/// in a committed flow, so the lint pass reports it alongside the tree rules.
#[test]
fn an_ambiguous_reference_is_a_lint() {
    let mut ui = Ui::<Msg>::with_fonts(
        Size::new(300.0, 200.0),
        Scale::ONE,
        Theme::dark(),
        FontContext::with_default_font(),
    );
    let root = ui.set_root(Container::column().fill().gap(4.0));
    ui.add_child(root, Button::new("OK").on_press(|| Msg::Inc));
    ui.add_child(root, Button::new("OK").on_press(|| Msg::Dec));

    let flow = "expect no-lints\ntap Button \"OK\"\n";
    let report = harness::run_text(&mut ui, flow, |_, _| {}).unwrap();
    assert!(!report.passed());
    assert!(
        report.failures[0].message.contains("ambiguous-ref"),
        "{}",
        report.failures[0].message
    );
}
