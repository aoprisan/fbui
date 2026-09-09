//! `Widget::describe`, widget names, and the tree dump — what an author who
//! cannot see the screen reads instead of a screenshot.
//!
//! Text metrics come from the compiled-in font (a dev-dependency feature), so
//! the laid-out bounds in the golden dump are the same on every host.
//!
//! Regenerate the golden after an intentional change:
//! `FBUI_UPDATE_SNAPSHOTS=1 cargo test -p fbui-widgets --test describe`

use fbui_render::geom::Size;
use fbui_render::{FontContext, Scale};
use fbui_widgets::widgets::{
    Button, ButtonVariant, Calendar, Chart, Checkbox, Container, Date, Dialog, Gauge, List,
    Navigator, ProgressBar, RadioGroup, ScrollView, Select, Slider, Spinner, Switch, TabBar,
    TextArea, TextInput, ToastKind, Toasts,
};
use fbui_widgets::{InspectNode, Theme, Ui, Widget};

#[derive(Clone, Debug, PartialEq)]
enum Msg {}

fn ui() -> Ui<Msg> {
    Ui::with_fonts(
        Size::new(400.0, 300.0),
        Scale::ONE,
        Theme::dark(),
        FontContext::with_default_font(),
    )
}

/// Describe one widget in isolation: it becomes the root, and we read back the
/// snapshot node the inspector would report for it.
fn describe_root(w: impl Widget<Msg>) -> InspectNode {
    let mut ui = ui();
    ui.set_root(w);
    ui.inspect().expect("a root exists")
}

/// `(text, props)` of a widget standing alone.
fn described(w: impl Widget<Msg>) -> (Option<String>, Vec<(&'static str, String)>) {
    let n = describe_root(w);
    (n.text, n.props)
}

fn prop(w: impl Widget<Msg>, key: &str) -> Option<String> {
    describe_root(w).prop(key).map(|s| s.to_string())
}

// ---- per-widget descriptions ----------------------------------------------

#[test]
fn label_and_button_report_the_words_a_person_reads() {
    use fbui_widgets::widgets::Label;
    let (text, props) = described(Label::new("Counter"));
    assert_eq!(text.as_deref(), Some("Counter"));
    assert!(props.is_empty(), "a plain label has nothing else to say");

    let (text, _) = described(Label::new("wrapped").wrap());
    assert_eq!(text.as_deref(), Some("wrapped"));
    assert_eq!(
        prop(Label::new("w").wrap(), "wrap").as_deref(),
        Some("true")
    );

    let (text, props) = described(Button::<Msg>::new("Submit"));
    assert_eq!(text.as_deref(), Some("Submit"));
    assert!(props.is_empty(), "the default variant is not worth a line");
    assert_eq!(
        prop(
            Button::<Msg>::new("Delete").variant(ButtonVariant::Danger),
            "variant"
        )
        .as_deref(),
        Some("danger"),
    );
}

#[test]
fn toggles_report_their_state() {
    let (text, props) = described(Checkbox::<Msg>::new("I agree", false));
    assert_eq!(text.as_deref(), Some("I agree"));
    assert_eq!(props, vec![("checked", "false".to_string())]);
    assert_eq!(
        prop(Checkbox::<Msg>::new("x", true), "checked").as_deref(),
        Some("true")
    );

    let (text, props) = described(Switch::<Msg>::new("Wi-Fi", true));
    assert_eq!(text.as_deref(), Some("Wi-Fi"));
    assert_eq!(props, vec![("on", "true".to_string())]);
}

#[test]
fn choice_widgets_report_the_selected_option_as_their_text() {
    let radio = RadioGroup::<Msg>::new(["Low", "Medium", "High"]).selected(2);
    let (text, props) = described(radio);
    assert_eq!(text.as_deref(), Some("High"));
    assert_eq!(
        props,
        vec![("selected", "2".to_string()), ("options", "3".to_string())]
    );

    let (text, props) = described(Select::<Msg>::new(["A", "B"]).selected(1));
    assert_eq!(text.as_deref(), Some("B"));
    assert_eq!(
        props,
        vec![
            ("selected", "1".to_string()),
            ("options", "2".to_string()),
            ("open", "false".to_string()),
        ]
    );

    let (text, props) = described(TabBar::<Msg>::new(["One", "Two", "Three"]).selected(0));
    assert_eq!(text.as_deref(), Some("One"));
    assert_eq!(
        props,
        vec![("selected", "0".to_string()), ("tabs", "3".to_string())]
    );
}

#[test]
fn value_widgets_report_a_value_with_its_range() {
    let (text, props) = described(Slider::<Msg>::new(0.0, 100.0, 40.0));
    assert!(text.is_none(), "a slider has no words to read");
    assert_eq!(
        props,
        vec![
            ("value", "40".to_string()),
            ("min", "0".to_string()),
            ("max", "100".to_string()),
        ]
    );

    assert_eq!(
        prop(ProgressBar::new(0.25), "value").as_deref(),
        Some("0.25")
    );
    assert_eq!(
        prop(ProgressBar::new(0.25), "percent").as_deref(),
        Some("25")
    );

    let g = Gauge::new(0.0, 10.0).label("Speed").value(7.5);
    let (text, _) = described(g);
    assert_eq!(text.as_deref(), Some("Speed"));
    assert_eq!(
        prop(Gauge::new(0.0, 10.0).value(7.5), "value").as_deref(),
        Some("7.5")
    );
}

#[test]
fn text_fields_report_their_content_and_caret() {
    let (text, props) = described(TextInput::<Msg>::new().value("milk").placeholder("item"));
    assert_eq!(text.as_deref(), Some("milk"));
    assert_eq!(
        props,
        vec![
            ("placeholder", "item".to_string()),
            ("cursor", "4".to_string()),
        ]
    );

    let (text, props) = described(TextArea::<Msg>::new().value("a\nb").rows(4));
    assert_eq!(text.as_deref(), Some("a\nb"));
    assert_eq!(props.last(), Some(&("rows", "4".to_string())));
}

#[test]
fn scrollers_report_offset_and_extent() {
    let (_, props) = described(ScrollView::new());
    let keys: Vec<_> = props.iter().map(|(k, _)| *k).collect();
    assert_eq!(keys, vec!["offset", "content", "viewport"]);

    let (text, props) = described(List::<Msg>::new(vec!["a".into(), "b".into(), "c".into()]));
    assert!(text.is_none(), "nothing is selected yet");
    assert_eq!(props[0], ("rows", "3".to_string()));
    assert_eq!(props[1], ("selected", "none".to_string()));
}

/// Selecting a row makes it the list's reading — the answer to "what does
/// this list say?" without looking at pixels.
#[test]
fn a_selected_list_row_becomes_the_lists_text() {
    use fbui_render::geom::Point;
    use fbui_widgets::event::PointerButton;

    let mut ui = ui();
    let root = ui.set_root(Container::column().fill());
    let list = ui.add_named(
        root,
        "items",
        List::<Msg>::new(vec!["alpha".into(), "beta".into(), "gamma".into()]),
    );
    ui.layout_now();
    let b = ui.bounds(list).expect("laid out");
    // Second row: one row height down from the top edge, plus a little.
    let at = Point::new(b.x + 10.0, b.y + 40.0);
    ui.event(fbui_widgets::Event::PointerDown {
        pos: at,
        button: PointerButton::Left,
    });
    ui.event(fbui_widgets::Event::PointerUp {
        pos: at,
        button: PointerButton::Left,
    });

    let snap = ui.inspect().unwrap();
    let node = snap
        .iter()
        .find(|n| n.name.as_deref() == Some("items"))
        .unwrap();
    assert_eq!(node.text.as_deref(), Some("beta"));
    assert_eq!(node.prop("selected"), Some("1"));
}

#[test]
fn containers_report_their_direction() {
    assert_eq!(
        prop(Container::column().gap(8.0), "direction").as_deref(),
        Some("column")
    );
    assert_eq!(
        prop(Container::column().gap(8.0), "gap").as_deref(),
        Some("8")
    );
    assert_eq!(prop(Container::row(), "direction").as_deref(), Some("row"));
}

#[test]
fn the_rest_of_the_set_describes_itself() {
    assert_eq!(prop(Spinner::new(), "running").as_deref(), Some("true"));
    assert_eq!(prop(Dialog::<Msg>::new(), "modal").as_deref(), Some("true"));

    // A fresh navigator is one screen deep and not sliding.
    assert_eq!(prop(Navigator::<Msg>::new(), "depth").as_deref(), Some("0"));
    assert_eq!(
        prop(Navigator::<Msg>::new(), "transitioning").as_deref(),
        Some("false")
    );

    let mut toasts = Toasts::new();
    toasts.push(ToastKind::Info, "Saved");
    let (text, props) = described(toasts);
    assert_eq!(text.as_deref(), Some("Saved"));
    assert_eq!(props, vec![("count", "1".to_string())]);

    let mut chart = Chart::new();
    chart.push_one(1.0);
    chart.push_one(3.0);
    assert_eq!(prop(chart, "samples").as_deref(), Some("2"));

    let cal = Calendar::<Msg>::new(Date::new(2026, 2, 14).unwrap());
    let (text, _) = described(cal);
    assert_eq!(text.as_deref(), Some("2026-02-14"));
}

/// Every built-in widget the set ships must say *something*: a widget that
/// describes nothing is a bare type name and a box in every dump, and a flow
/// script can neither address nor assert on it.
#[test]
fn no_built_in_widget_is_silent() {
    let mut silent = Vec::new();
    let mut check = |n: InspectNode| {
        if n.text.is_none() && n.props.is_empty() {
            silent.push(n.kind);
        }
    };
    use fbui_render::Image;
    use fbui_widgets::widgets::{
        ContextMenu, ImageView, Keyboard, Label, Menu, Stack, TreeNode, TreeView,
    };
    check(describe_root(Label::new("x")));
    check(describe_root(Button::<Msg>::new("x")));
    check(describe_root(Checkbox::<Msg>::new("x", false)));
    check(describe_root(Switch::<Msg>::new("x", false)));
    check(describe_root(RadioGroup::<Msg>::new(["a"])));
    check(describe_root(Slider::<Msg>::new(0.0, 1.0, 0.5)));
    check(describe_root(ProgressBar::new(0.5)));
    check(describe_root(Gauge::new(0.0, 1.0)));
    check(describe_root(Spinner::new()));
    check(describe_root(TextInput::<Msg>::new()));
    check(describe_root(TextArea::<Msg>::new()));
    check(describe_root(List::<Msg>::new(vec!["a".into()])));
    check(describe_root(ScrollView::new()));
    check(describe_root(Select::<Msg>::new(["a"])));
    check(describe_root(TabBar::<Msg>::new(["a"])));
    check(describe_root(Container::column()));
    check(describe_root(Stack::new()));
    check(describe_root(Dialog::<Msg>::new()));
    check(describe_root(Navigator::<Msg>::new()));
    check(describe_root(Toasts::new()));
    check(describe_root(ImageView::new(
        Image::from_rgba_bytes(1, 1, &[0, 0, 0, 255]).unwrap(),
    )));
    check(describe_root(fbui_widgets::widgets::VideoView::new()));
    check(describe_root(Chart::new()));
    check(describe_root(Calendar::<Msg>::new(
        Date::new(2026, 1, 1).unwrap(),
    )));
    check(describe_root(Keyboard::<Msg>::new()));
    check(describe_root(TreeView::<Msg>::new(vec![TreeNode::leaf(
        "a",
    )])));
    check(describe_root(Menu::<Msg>::new(["Copy"])));
    check(describe_root(ContextMenu::<Msg>::new(["Copy"])));
    assert!(silent.is_empty(), "widgets with no description: {silent:?}");
}

// ---- names ----------------------------------------------------------------

#[test]
fn names_resolve_and_die_with_their_widget() {
    let mut ui = ui();
    let root = ui.set_root(Container::column());
    let inc = ui.add_named(root, "inc", Button::<Msg>::new("+"));
    let dec = ui.add_child(root, Button::<Msg>::new("-"));
    ui.name(dec, "dec");

    assert_eq!(ui.find("inc"), Some(inc));
    assert_eq!(ui.find("dec"), Some(dec));
    assert_eq!(ui.find("nope"), None);
    assert_eq!(ui.name_of(inc), Some("inc"));

    ui.remove(dec);
    assert_eq!(
        ui.find("dec"),
        None,
        "a removed widget's name stops resolving"
    );
    assert_eq!(ui.find("inc"), Some(inc));

    // Reusing the freed name is fine.
    let again = ui.add_named(root, "dec", Button::<Msg>::new("-"));
    assert_eq!(ui.find("dec"), Some(again));
}

#[test]
fn renaming_drops_the_old_name() {
    let mut ui = ui();
    let root = ui.set_root(Container::column());
    let b = ui.add_named(root, "old", Button::<Msg>::new("x"));
    ui.name(b, "new");
    assert_eq!(ui.find("old"), None);
    assert_eq!(ui.find("new"), Some(b));
    assert_eq!(ui.name_of(b), Some("new"));
}

#[test]
fn a_path_name_is_scoped_by_its_named_ancestors() {
    let mut ui = ui();
    let root = ui.set_root(Container::column());
    let form = ui.add_named(root, "form", Container::column());
    let inner = ui.add_child(form, Container::row());
    let field = ui.add_named(inner, "name", TextInput::<Msg>::new());

    assert_eq!(ui.find("name"), Some(field));
    assert_eq!(ui.find("form/name"), Some(field), "nested under `form`");
    assert_eq!(ui.find("other/name"), None, "not under `other`");
}

#[test]
fn set_root_clears_every_name() {
    let mut ui = ui();
    let root = ui.set_root(Container::column());
    ui.add_named(root, "gone", Button::<Msg>::new("x"));
    ui.set_root(Container::column());
    assert_eq!(ui.find("gone"), None);
    assert_eq!(ui.names().count(), 0);
}

// ---- visibility -----------------------------------------------------------

/// A widget scrolled out of its viewport is reported `visible: false`, which
/// is what "can I tap it" needs — the bounds alone would say it is fine.
#[test]
fn clipped_children_are_reported_invisible() {
    let mut ui = ui();
    let root = ui.set_root(Container::column().fill());
    let scroll = ui.add_child(root, ScrollView::new());
    let content = ui.add_child(scroll, Container::column());
    let mut ids = Vec::new();
    for i in 0..40 {
        ids.push(ui.add_named(
            content,
            format!("row{i}"),
            Button::<Msg>::new(format!("row {i}")),
        ));
    }
    let snap = ui.inspect().unwrap();
    let by_name = |name: &str| {
        snap.iter()
            .find(|n| n.name.as_deref() == Some(name))
            .expect("named node in the dump")
            .visible
    };
    assert!(by_name("row0"), "the first row is on screen");
    assert!(!by_name("row39"), "the last row is scrolled out of view");
}

// ---- the tree dump --------------------------------------------------------

/// The dump line carries kind, name, bounds, text, props and live state, in
/// that order — the format `docs/tooling.md` documents and flow failures
/// print.
#[test]
fn a_dump_line_carries_everything_a_reader_needs() {
    let mut ui = ui();
    let root = ui.set_root(Container::column().padding(10.0).fill());
    let cb = ui.add_named(root, "agree", Checkbox::<Msg>::new("I agree", false));
    ui.focus(Some(cb));

    let dump = ui.inspect_text();
    let line = dump
        .lines()
        .find(|l| l.contains("Checkbox"))
        .expect("the checkbox is in the dump");
    assert!(line.starts_with("  Checkbox #agree ["), "{line}");
    assert!(line.contains("\"I agree\""), "{line}");
    assert!(line.contains("checked=false"), "{line}");
    assert!(line.ends_with(" focused"), "{line}");
    assert!(
        dump.starts_with("Container [0,0 400x300] direction=column"),
        "{dump}"
    );
}

#[test]
fn an_empty_tree_dumps_to_nothing() {
    let mut ui = ui();
    assert_eq!(ui.inspect_text(), "");
}

/// A gallery of the whole widget set, dumped to a committed golden. A diff of
/// this file in review says exactly what changed on screen — which widget
/// moved, which text changed, which state flipped — without anyone opening a
/// PNG.
#[test]
fn the_gallery_tree_matches_its_golden_dump() {
    let mut ui = Ui::<Msg>::with_fonts(
        Size::new(480.0, 640.0),
        Scale::ONE,
        Theme::dark(),
        FontContext::with_default_font(),
    );
    let root = ui.set_root(Container::column().fill().padding(12.0).gap(8.0));
    ui.name(root, "page");

    ui.add_named(root, "title", fbui_widgets::widgets::Label::new("Gallery"));
    let row = ui.add_named(root, "actions", Container::row().gap(8.0));
    ui.add_named(row, "save", Button::<Msg>::new("Save"));
    ui.add_named(
        row,
        "delete",
        Button::<Msg>::new("Delete").variant(ButtonVariant::Danger),
    );
    ui.add_named(root, "agree", Checkbox::<Msg>::new("I agree", true));
    ui.add_named(root, "wifi", Switch::<Msg>::new("Wi-Fi", false));
    ui.add_named(
        root,
        "quality",
        RadioGroup::<Msg>::new(["Low", "High"]).selected(1),
    );
    ui.add_named(root, "volume", Slider::<Msg>::new(0.0, 100.0, 30.0));
    ui.add_named(root, "progress", ProgressBar::new(0.4));
    ui.add_named(root, "item", TextInput::<Msg>::new().value("milk"));
    ui.add_named(root, "tabs", TabBar::<Msg>::new(["One", "Two"]).selected(1));
    ui.add_named(
        root,
        "items",
        List::<Msg>::new(vec!["alpha".into(), "beta".into()]),
    );

    let actual = ui.inspect_text();
    let path = std::path::Path::new("tests/snapshots/gallery.tree.txt");
    if fbui_testkit::update_requested() {
        std::fs::write(path, &actual).expect("write the golden dump");
        eprintln!("updated {}", path.display());
        return;
    }
    let expected = std::fs::read_to_string(path).unwrap_or_else(|e| {
        panic!(
            "missing golden {} ({e}); regenerate with FBUI_UPDATE_SNAPSHOTS=1",
            path.display()
        )
    });
    assert_eq!(
        actual, expected,
        "the gallery tree changed; review the diff and regenerate with \
         FBUI_UPDATE_SNAPSHOTS=1 if it is intentional"
    );
}
