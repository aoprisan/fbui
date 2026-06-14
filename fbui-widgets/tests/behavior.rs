//! Behavioral tests for the widget engine: layout, event routing, focus, and
//! the retained update loop. These are font-independent (they assert structure
//! and messages, not pixels), so they're robust across hosts.

use fbui_render::geom::{Point, Size};
use fbui_render::Scale;
use fbui_widgets::event::{Event, Key, Modifiers, PointerButton};
use fbui_widgets::widgets::{Button, Checkbox, Container, List, RadioGroup, Stack, Switch};
use fbui_widgets::{Theme, Ui, WidgetId};

#[derive(Clone, Debug, PartialEq)]
enum Msg {
    Pressed,
    Toggled(bool),
    Picked(usize),
    Switched(bool),
    Chose(usize),
}

fn ui() -> Ui<Msg> {
    Ui::new(Size::new(400.0, 300.0), Scale::ONE, Theme::dark())
}

fn center(ui: &Ui<Msg>, id: WidgetId) -> Point {
    let b = ui.bounds(id).expect("laid out");
    Point::new(b.x + b.w / 2.0, b.y + b.h / 2.0)
}

fn click(ui: &mut Ui<Msg>, at: Point) {
    ui.event(Event::PointerDown {
        pos: at,
        button: PointerButton::Left,
    });
    ui.event(Event::PointerUp {
        pos: at,
        button: PointerButton::Left,
    });
}

#[test]
fn layout_places_children_in_a_column() {
    let mut ui = ui();
    let root = ui.set_root(Container::column().padding(10.0).gap(8.0));
    let a = ui.add_child(root, Button::new("A"));
    let b = ui.add_child(root, Button::new("B"));
    ui.layout_now();

    let ra = ui.bounds(a).unwrap();
    let rb = ui.bounds(b).unwrap();
    assert!(ra.x >= 10.0 && ra.y >= 10.0, "padding applied: {ra:?}");
    assert!(rb.y >= ra.bottom(), "B is below A: {ra:?} {rb:?}");
    assert!((rb.y - ra.bottom() - 8.0).abs() < 1.0, "gap respected");
}

#[test]
fn button_click_emits_message() {
    let mut ui = ui();
    let root = ui.set_root(Container::column().padding(10.0));
    let btn = ui.add_child(root, Button::new("Go").on_press(|| Msg::Pressed));
    ui.layout_now();

    let c = center(&ui, btn);
    click(&mut ui, c);
    assert_eq!(ui.take_messages(), vec![Msg::Pressed]);
}

#[test]
fn click_outside_button_emits_nothing() {
    let mut ui = ui();
    let root = ui.set_root(Container::column().padding(10.0));
    let _btn = ui.add_child(root, Button::new("Go").on_press(|| Msg::Pressed));
    ui.layout_now();

    click(&mut ui, Point::new(390.0, 290.0)); // empty corner
    assert!(ui.take_messages().is_empty());
}

#[test]
fn tab_cycles_focus_over_focusables() {
    let mut ui = ui();
    let root = ui.set_root(Container::column());
    let a = ui.add_child(root, Button::new("A"));
    let b = ui.add_child(root, Button::new("B"));
    ui.layout_now();
    assert_eq!(ui.focused(), None);

    let tab = || Event::Key {
        key: Key::Tab,
        pressed: true,
        mods: Modifiers::default(),
    };
    ui.event(tab());
    assert_eq!(ui.focused(), Some(a));
    ui.event(tab());
    assert_eq!(ui.focused(), Some(b));
    ui.event(tab());
    assert_eq!(ui.focused(), Some(a), "wraps around");
}

#[test]
fn shift_tab_goes_backwards() {
    let mut ui = ui();
    let root = ui.set_root(Container::column());
    let a = ui.add_child(root, Button::new("A"));
    let b = ui.add_child(root, Button::new("B"));
    ui.layout_now();

    ui.event(Event::Key {
        key: Key::Tab,
        pressed: true,
        mods: Modifiers {
            shift: true,
            ..Default::default()
        },
    });
    // First backward step from no focus lands on the last focusable.
    assert_eq!(ui.focused(), Some(b));
    let _ = a;
}

#[test]
fn keyboard_activates_focused_button() {
    let mut ui = ui();
    let root = ui.set_root(Container::column());
    let _btn = ui.add_child(root, Button::new("Go").on_press(|| Msg::Pressed));
    ui.layout_now();

    ui.event(Event::Key {
        key: Key::Tab,
        pressed: true,
        mods: Modifiers::default(),
    });
    ui.event(Event::Key {
        key: Key::Enter,
        pressed: true,
        mods: Modifiers::default(),
    });
    assert_eq!(ui.take_messages(), vec![Msg::Pressed]);
}

#[test]
fn checkbox_toggles_and_reports_state() {
    let mut ui = ui();
    let root = ui.set_root(Container::column().padding(10.0));
    let cb = ui.add_child(root, Checkbox::new("On", false).on_toggle(Msg::Toggled));
    ui.layout_now();

    let c = center(&ui, cb);
    click(&mut ui, c);
    assert_eq!(ui.take_messages(), vec![Msg::Toggled(true)]);
    // State actually flipped on the widget.
    let checked = ui.with::<Checkbox<Msg>, _>(cb, |c| c.checked()).unwrap();
    assert!(checked);
}

#[test]
fn list_click_selects_row() {
    let mut ui = ui();
    let root = ui.set_root(Container::column().fill());
    let rows: Vec<String> = (0..1000).map(|i| format!("row {i}")).collect();
    let list = ui.add_child(root, List::new(rows).on_select(Msg::Picked));
    ui.layout_now();

    let b = ui.bounds(list).unwrap();
    // Click near the top: row 0 (default row height 40).
    click(&mut ui, Point::new(b.x + 20.0, b.y + 10.0));
    assert_eq!(ui.take_messages(), vec![Msg::Picked(0)]);
}

#[test]
fn fling_kinetically_scrolls_the_list() {
    let mut ui = ui();
    let root = ui.set_root(Container::column().fill());
    let rows: Vec<String> = (0..1000).map(|i| format!("row {i}")).collect();
    let list = ui.add_child(root, List::new(rows).on_select(Msg::Picked));
    ui.layout_now();
    let b = ui.bounds(list).unwrap();

    // Fling upward (negative velocity_y) over the list: content should coast up.
    ui.event(Event::Fling {
        pos: Point::new(b.x + 20.0, b.y + 20.0),
        velocity_x: 0.0,
        velocity_y: -2000.0,
    });
    assert!(ui.animate(1.0 / 60.0), "coast is running after a fling");

    // Run the coast to rest.
    for _ in 0..600 {
        if !ui.animate(1.0 / 60.0) {
            break;
        }
    }
    assert!(!ui.animate(1.0 / 60.0), "coast settles to rest");

    // The list scrolled, so the row now under the top of the viewport is no
    // longer row 0 — a tap there selects a later row.
    click(&mut ui, Point::new(b.x + 20.0, b.y + 10.0));
    match ui.take_messages().as_slice() {
        [Msg::Picked(idx)] => assert!(*idx > 0, "scrolled past row 0, got {idx}"),
        other => panic!("expected one Picked(>0), got {other:?}"),
    }
}

#[test]
fn scroll_blit_matches_a_full_repaint() {
    use fbui_render::Surface;

    // Build two identical lists; paint both fully at offset 0.
    fn make() -> (Ui<Msg>, WidgetId, Surface) {
        let mut ui = Ui::<Msg>::new(Size::new(200.0, 200.0), Scale::ONE, Theme::dark());
        let root = ui.set_root(Container::column().fill());
        let rows: Vec<String> = (0..500).map(|i| format!("row {i}")).collect();
        let list = ui.add_child(root, List::new(rows));
        ui.layout_now();
        let surface = Surface::new(200, 200, Scale::ONE);
        (ui, list, surface)
    }

    let wheel = |ui: &mut Ui<Msg>, list: WidgetId, dy: f32| {
        let b = ui.bounds(list).unwrap();
        ui.event(Event::Scroll {
            pos: Point::new(b.x + 10.0, b.y + 10.0),
            delta_x: 0.0,
            delta_y: dy,
        });
    };

    let (mut ua, la, mut sa) = make();
    let (mut ub, lb, mut sb) = make();
    ua.paint(&mut sa);
    ub.paint(&mut sb);

    // A: scroll with the blit fast path (only the strip is re-rasterized).
    wheel(&mut ua, la, 24.0);
    ua.paint(&mut sa);

    // B: same scroll, but force a full repaint over it (mark everything dirty),
    // so B is the ground-truth render of the scrolled state.
    wheel(&mut ub, lb, 24.0);
    ub.set_size(Size::new(200.0, 200.0), Scale::ONE); // marks the whole surface dirty
    ub.paint(&mut sb);

    assert_eq!(
        sa.pixmap().data(),
        sb.pixmap().data(),
        "scroll-blit output must match a full repaint of the same offset"
    );
}

#[test]
fn switch_toggles_and_animates_then_settles() {
    let mut ui = ui();
    let root = ui.set_root(Container::column().padding(10.0));
    let sw = ui.add_child(root, Switch::new("Wi-Fi", false).on_toggle(Msg::Switched));
    ui.layout_now();
    assert!(!ui.is_animating(), "idle before interaction");

    // Click flips state, emits the message, and starts the slide animation.
    let c = center(&ui, sw);
    click(&mut ui, c);
    assert_eq!(ui.take_messages(), vec![Msg::Switched(true)]);
    // The click alone (no `with`) must have started the animation.
    assert!(ui.is_animating(), "toggle started an animation");

    // The animation runs for a few frames, then settles and stops the clock.
    let mut frames = 0;
    while ui.is_animating() && frames < 120 {
        ui.animate(1.0 / 60.0);
        frames += 1;
    }
    assert!(!ui.is_animating(), "animation settled");
    assert!(frames > 1, "took more than one frame to animate");
}

#[test]
fn stack_overlaps_children_at_the_same_origin() {
    let mut ui = ui();
    // A full-screen stack with two children: both fill the same box.
    let root = ui.set_root(Stack::new());
    let back = ui.add_child(root, Container::column().fill());
    let front = ui.add_child(root, Container::column().fill());
    ui.layout_now();

    let rb = ui.bounds(back).unwrap();
    let rf = ui.bounds(front).unwrap();
    // Stacked, not flowed: identical rects, each the size of the surface.
    assert_eq!(rb, rf, "children overlap exactly");
    assert!(
        (rb.w - 400.0).abs() < 0.5 && (rb.h - 300.0).abs() < 0.5,
        "each child fills the stack: {rb:?}"
    );
}

#[test]
fn stack_hit_tests_topmost_child_first() {
    let mut ui = ui();
    let root = ui.set_root(Stack::new());
    // Both children are pinned to the stack's origin and overlap there. They emit
    // distinct messages so we can tell which one received the click.
    let back = ui.add_child(root, Button::new("xxxx").on_press(|| Msg::Pressed));
    // Added last → on top.
    let front = ui.add_child(root, Button::new("xxxx").on_press(|| Msg::Switched(true)));
    ui.layout_now();

    // They occupy the same rect (overlap), anchored at the stack's top-left.
    assert_eq!(ui.bounds(back), ui.bounds(front), "layers overlap");
    let b = ui.bounds(front).unwrap();

    // A click inside that shared rect goes to the topmost (last-added) child.
    click(&mut ui, Point::new(b.x + b.w / 2.0, b.y + b.h / 2.0));
    assert_eq!(
        ui.take_messages(),
        vec![Msg::Switched(true)],
        "the topmost (last-added) child receives the click, not the one beneath"
    );
}

#[test]
fn radio_group_selects_on_click_and_reports_state() {
    let mut ui = ui();
    let root = ui.set_root(Container::column().padding(10.0));
    let rg = ui.add_child(
        root,
        RadioGroup::new(["Low", "Medium", "High"]).on_change(Msg::Chose),
    );
    ui.layout_now();

    let b = ui.bounds(rg).unwrap();
    // Click the third row (rows are DISC=20 tall on a 26px pitch).
    click(&mut ui, Point::new(b.x + 10.0, b.y + 2.0 * 26.0 + 4.0));
    assert_eq!(ui.take_messages(), vec![Msg::Chose(2)]);
    let sel = ui
        .with::<RadioGroup<Msg>, _>(rg, |r| r.selection())
        .unwrap();
    assert_eq!(sel, 2, "state flipped to the clicked row");
}

#[test]
fn radio_group_arrow_keys_move_selection() {
    let mut ui = ui();
    let root = ui.set_root(Container::column().padding(10.0));
    let rg = ui.add_child(root, RadioGroup::new(["A", "B", "C"]).on_change(Msg::Chose));
    ui.layout_now();

    // Focus the group, then arrow down twice and back up once.
    ui.event(Event::Key {
        key: Key::Tab,
        pressed: true,
        mods: Modifiers::default(),
    });
    assert_eq!(ui.focused(), Some(rg));

    let key = |ui: &mut Ui<Msg>, k: Key| {
        ui.event(Event::Key {
            key: k,
            pressed: true,
            mods: Modifiers::default(),
        });
    };
    key(&mut ui, Key::Down);
    key(&mut ui, Key::Down);
    key(&mut ui, Key::Up);
    assert_eq!(
        ui.take_messages(),
        vec![Msg::Chose(1), Msg::Chose(2), Msg::Chose(1)]
    );

    // Already at the top: Up clamps and emits nothing.
    key(&mut ui, Key::Up);
    key(&mut ui, Key::Up);
    assert_eq!(ui.take_messages(), vec![Msg::Chose(0)]);
}

#[test]
fn mutation_marks_dirty_paint_clears_it() {
    let mut ui = ui();
    let root = ui.set_root(Container::column());
    let cb = ui.add_child(root, Checkbox::new("x", false).on_toggle(Msg::Toggled));
    ui.layout_now();

    // A surface big enough to paint into.
    let mut surface = fbui_render::Surface::new(400, 300, Scale::ONE);
    ui.paint(&mut surface);
    assert!(!ui.needs_paint(), "clean after paint");

    ui.with::<Checkbox<Msg>, _>(cb, |c| c.set_checked(true));
    assert!(ui.needs_paint(), "mutation marks dirty");
}
