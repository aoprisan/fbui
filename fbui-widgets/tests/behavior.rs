//! Behavioral tests for the widget engine: layout, event routing, focus, and
//! the retained update loop. These are font-independent (they assert structure
//! and messages, not pixels), so they're robust across hosts.

use fbui_render::geom::{Point, Size};
use fbui_render::Scale;
use fbui_widgets::event::{Event, Key, Modifiers, PointerButton};
use fbui_widgets::widgets::{
    Button, Checkbox, Container, Dialog, List, RadioGroup, ScrollView, Select, Stack, Switch,
    ToastKind, Toasts,
};
use fbui_widgets::{Theme, Ui, WidgetId};

#[derive(Clone, Debug, PartialEq)]
enum Msg {
    Pressed,
    Toggled(bool),
    Picked(usize),
    Switched(bool),
    Chose(usize),
    Dismissed,
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
fn scrollview_blit_matches_a_full_repaint() {
    use fbui_render::{Color, Surface};

    // A ScrollView of colored fixed-height stripes (font-independent pixels).
    fn make() -> (Ui<Msg>, WidgetId, Surface) {
        let mut ui = Ui::<Msg>::new(Size::new(200.0, 200.0), Scale::ONE, Theme::dark());
        let root = ui.set_root(Container::column().fill());
        let scroll = ui.add_child(root, ScrollView::new());
        let col = ui.add_child(scroll, Container::column());
        for i in 0..40 {
            let c = Color::rgba((i * 6) as u8, 40, (255 - i * 6) as u8, 255);
            ui.add_child(col, Container::row().height(25.0).background(c, 0.0));
        }
        ui.layout_now();
        let surface = Surface::new(200, 200, Scale::ONE);
        (ui, scroll, surface)
    }

    let wheel = |ui: &mut Ui<Msg>, sv: WidgetId, dy: f32| {
        let b = ui.bounds(sv).unwrap();
        ui.event(Event::Scroll {
            pos: Point::new(b.x + 10.0, b.y + 10.0),
            delta_x: 0.0,
            delta_y: dy,
        });
    };

    let (mut ua, sa_id, mut sa) = make();
    let (mut ub, sb_id, mut sb) = make();
    ua.paint(&mut sa);
    ub.paint(&mut sb);
    let before = sa.pixmap().data().to_vec();

    // A: scroll with the blit fast path (only the exposed strip re-rasterizes).
    wheel(&mut ua, sa_id, 24.0);
    ua.paint(&mut sa);

    // B: same scroll, but force a full repaint over it (mark everything dirty),
    // so B is the ground-truth render of the scrolled state.
    wheel(&mut ub, sb_id, 24.0);
    ub.set_size(Size::new(200.0, 200.0), Scale::ONE); // marks the whole surface dirty
    ub.paint(&mut sb);

    assert_ne!(
        before,
        sa.pixmap().data(),
        "the scroll must actually move content"
    );
    assert_eq!(
        sa.pixmap().data(),
        sb.pixmap().data(),
        "ScrollView blit output must match a full repaint of the same offset"
    );
}

#[test]
fn scroll_blit_under_an_overlay_falls_back_to_a_full_repaint() {
    use fbui_render::{Color, Surface};

    // A List under a Stack overlay that covers part of it. The blit would drag
    // the overlay's pixels along; the Ui must detect the overlap and repaint in
    // full instead — output identical to the ground truth, overlay intact.
    fn make() -> (Ui<Msg>, WidgetId, Surface) {
        let mut ui = Ui::<Msg>::new(Size::new(200.0, 200.0), Scale::ONE, Theme::dark());
        let root = ui.set_root(Stack::new());
        let rows: Vec<String> = (0..500).map(|i| format!("row {i}")).collect();
        let list = ui.add_child(root, List::new(rows));
        // The overlay: a centered opaque card on top of the list.
        let overlay = ui.add_child(root, Container::column().padding(60.0));
        ui.add_child(
            overlay,
            Container::column()
                .grow(1.0)
                .background(Color::rgba(200, 80, 80, 255), 8.0),
        );
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

    wheel(&mut ua, la, 24.0);
    ua.paint(&mut sa);

    wheel(&mut ub, lb, 24.0);
    ub.set_size(Size::new(200.0, 200.0), Scale::ONE);
    ub.paint(&mut sb);

    assert_eq!(
        sa.pixmap().data(),
        sb.pixmap().data(),
        "a scroll under an overlay must not corrupt the overlay's pixels"
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

// ---- overlay layer: Dialog / Select / Toasts -------------------------------

fn press_key(ui: &mut Ui<Msg>, k: Key) {
    ui.event(Event::Key {
        key: k,
        pressed: true,
        mods: Modifiers::default(),
    });
}

#[test]
fn dialog_blocks_input_dismisses_and_removes() {
    let mut ui = ui();
    let stack = ui.set_root(Stack::new());
    let page = ui.add_child(stack, Container::column().padding(10.0));
    let page_btn = ui.add_child(page, Button::new("Page").on_press(|| Msg::Pressed));
    ui.layout_now();

    // Sanity: the page button works before the dialog opens.
    let c = center(&ui, page_btn);
    click(&mut ui, c);
    assert_eq!(ui.take_messages(), vec![Msg::Pressed]);

    // Open a modal dialog over it.
    let dialog = ui.add_child(stack, Dialog::new().on_dismiss(|| Msg::Dismissed));
    let card = ui.add_child(dialog, Container::column().padding(20.0).gap(8.0));
    let ok = ui.add_child(card, Button::new("OK").on_press(|| Msg::Toggled(true)));
    ui.layout_now();

    // A click where the page button sits now lands on the scrim: the page
    // button must NOT fire; the scrim click dismisses instead.
    click(&mut ui, c);
    let msgs = ui.take_messages();
    assert!(
        !msgs.contains(&Msg::Pressed),
        "modal blocks the page: {msgs:?}"
    );
    assert_eq!(msgs, vec![Msg::Dismissed], "scrim click asks to dismiss");

    // The dialog's own button still works.
    let ok_c = center(&ui, ok);
    click(&mut ui, ok_c);
    assert_eq!(ui.take_messages(), vec![Msg::Toggled(true)]);

    // Esc bubbles from the focused widget inside the card to the dialog.
    assert!(ui.focus_first(dialog), "dialog has a focusable child");
    assert_eq!(ui.focused(), Some(ok));
    press_key(&mut ui, Key::Escape);
    assert_eq!(ui.take_messages(), vec![Msg::Dismissed]);

    // The app closes it by removing the subtree; the page works again.
    ui.remove(dialog);
    assert_eq!(
        ui.focused(),
        None,
        "focus into the removed subtree is cleared"
    );
    ui.layout_now();
    click(&mut ui, c);
    assert_eq!(ui.take_messages(), vec![Msg::Pressed]);
}

#[test]
fn dialog_traps_tab_focus() {
    let mut ui = ui();
    let stack = ui.set_root(Stack::new());
    let page = ui.add_child(stack, Container::column().padding(10.0));
    let page_btn = ui.add_child(page, Button::new("Page").on_press(|| Msg::Pressed));

    let dialog = ui.add_child(stack, Dialog::new());
    let card = ui.add_child(dialog, Container::column().padding(20.0).gap(8.0));
    let a = ui.add_child(card, Button::new("A").on_press(|| Msg::Pressed));
    let b = ui.add_child(card, Button::new("B").on_press(|| Msg::Pressed));
    ui.layout_now();

    assert!(ui.focus_first(dialog));
    assert_eq!(ui.focused(), Some(a));
    // Tab cycles a -> b -> a ... never reaching the page button.
    for _ in 0..4 {
        press_key(&mut ui, Key::Tab);
        let f = ui.focused();
        assert!(
            f == Some(a) || f == Some(b),
            "focus stayed in the dialog, got {f:?} (page = {page_btn:?})"
        );
    }
}

#[test]
fn select_opens_commits_and_click_away_closes() {
    let mut ui = ui();
    let root = ui.set_root(Container::column().padding(10.0).gap(10.0));
    let sel = ui.add_child(
        root,
        Select::new(["Alpha", "Beta", "Gamma"]).on_change(Msg::Chose),
    );
    let btn = ui.add_child(root, Button::new("Below").on_press(|| Msg::Pressed));
    ui.layout_now();

    let b = ui.bounds(sel).unwrap();
    // Menu geometry (matches the widget's constants): below the field.
    let row_center = |i: usize| {
        Point::new(
            b.x + b.w / 2.0,
            b.bottom() + 2.0 + 4.0 + (i as f32 + 0.5) * 32.0,
        )
    };

    // Click the field: opens, captures.
    let sc = center(&ui, sel);
    click(&mut ui, sc);
    assert_eq!(ui.with::<Select<Msg>, _>(sel, |s| s.is_open()), Some(true));

    // Click row 1 (in the floating menu, over where the button would be).
    click(&mut ui, row_center(1));
    assert_eq!(ui.take_messages(), vec![Msg::Chose(1)]);
    assert_eq!(ui.with::<Select<Msg>, _>(sel, |s| s.is_open()), Some(false));
    assert_eq!(
        ui.with::<Select<Msg>, _>(sel, |s| s.selected_index()),
        Some(1)
    );

    // Reopen, then click away (far from both field and menu): the menu closes
    // without committing, and the click is consumed.
    click(&mut ui, sc);
    assert_eq!(ui.with::<Select<Msg>, _>(sel, |s| s.is_open()), Some(true));
    click(&mut ui, Point::new(390.0, 290.0));
    assert_eq!(ui.with::<Select<Msg>, _>(sel, |s| s.is_open()), Some(false));
    assert!(ui.take_messages().is_empty(), "click-away is consumed");

    // With the menu closed the button below works normally again.
    let bc = center(&ui, btn);
    click(&mut ui, bc);
    assert_eq!(ui.take_messages(), vec![Msg::Pressed]);
}

#[test]
fn select_keyboard_navigation() {
    let mut ui = ui();
    let root = ui.set_root(Container::column().padding(10.0));
    let sel = ui.add_child(
        root,
        Select::new(["Alpha", "Beta", "Gamma"]).on_change(Msg::Chose),
    );
    ui.layout_now();

    // Focus by clicking, then close with Esc (still focused).
    let sc = center(&ui, sel);
    click(&mut ui, sc);
    press_key(&mut ui, Key::Escape);
    assert_eq!(ui.with::<Select<Msg>, _>(sel, |s| s.is_open()), Some(false));
    assert_eq!(ui.focused(), Some(sel));

    // Enter opens with the hover on the current selection; Down moves; Enter
    // commits.
    press_key(&mut ui, Key::Enter);
    assert_eq!(ui.with::<Select<Msg>, _>(sel, |s| s.is_open()), Some(true));
    press_key(&mut ui, Key::Down);
    press_key(&mut ui, Key::Enter);
    assert_eq!(ui.take_messages(), vec![Msg::Chose(1)]);
    assert_eq!(ui.with::<Select<Msg>, _>(sel, |s| s.is_open()), Some(false));
}

#[test]
fn toasts_appear_and_expire_on_the_frame_clock() {
    use fbui_render::Surface;

    let mut ui = ui();
    let root = ui.set_root(Stack::new());
    let _page = ui.add_child(root, Container::column().padding(10.0));
    let toasts = ui.add_child(root, Toasts::new());
    ui.layout_now();

    let mut surface = Surface::new(400, 300, Scale::ONE);
    ui.paint(&mut surface);
    let baseline = surface.pixmap().data().to_vec();

    // Push a toast: the overlay appears (pixels change), driven by `with`.
    ui.with::<Toasts, _>(toasts, |t| t.push(ToastKind::Success, "Saved"));
    assert!(ui.needs_paint());
    ui.paint(&mut surface);
    assert_ne!(baseline, surface.pixmap().data(), "toast card is visible");
    assert!(ui.is_animating(), "toast lifetime rides the frame clock");

    // Let it live out its TTL: the card fades and vanishes, and the surface is
    // byte-identical to before the toast.
    let mut guard = 0;
    while ui.animate(0.25) {
        ui.paint(&mut surface);
        guard += 1;
        assert!(guard < 100, "toast must expire");
    }
    ui.paint(&mut surface);
    assert_eq!(
        baseline,
        surface.pixmap().data(),
        "expired toast fully erased"
    );
    assert_eq!(ui.with::<Toasts, _>(toasts, |t| t.len()), Some(0));
}

#[test]
fn scroll_blit_under_an_open_select_falls_back() {
    use fbui_render::Surface;

    // A coasting List keeps blitting while a Select menu floats over it; the
    // Ui must fall back to a full repaint so the menu isn't dragged along.
    fn make() -> (Ui<Msg>, WidgetId, WidgetId, Surface) {
        let mut ui = Ui::<Msg>::new(Size::new(200.0, 300.0), Scale::ONE, Theme::dark());
        let root = ui.set_root(Container::column().fill().padding(4.0).gap(4.0));
        let sel = ui.add_child(root, Select::new(["One", "Two", "Three"]));
        let rows: Vec<String> = (0..500).map(|i| format!("row {i}")).collect();
        let list = ui.add_child(root, List::new(rows));
        ui.layout_now();
        let surface = Surface::new(200, 300, Scale::ONE);
        (ui, sel, list, surface)
    }

    let run =
        |ui: &mut Ui<Msg>, sel: WidgetId, list: WidgetId, surface: &mut Surface, force: bool| {
            ui.paint(surface);
            // Fling the list so it coasts...
            let lb = ui.bounds(list).unwrap();
            let lp = Point::new(lb.x + 10.0, lb.y + 10.0);
            ui.event(Event::Fling {
                pos: lp,
                velocity_x: 0.0,
                velocity_y: -600.0,
            });
            // ...then open the select; its menu floats over the list.
            let sc = center(ui, sel);
            click(ui, sc);
            ui.paint(surface);
            // The coast continues under the open menu.
            for _ in 0..5 {
                ui.animate(1.0 / 60.0);
                if force {
                    ui.set_size(Size::new(200.0, 300.0), Scale::ONE); // ground truth: full repaint
                }
                ui.paint(surface);
            }
        };

    let (mut ua, sa, la, mut fa) = make();
    let (mut ub, sb, lb, mut fb) = make();
    run(&mut ua, sa, la, &mut fa, false);
    run(&mut ub, sb, lb, &mut fb, true);

    assert_eq!(
        fa.pixmap().data(),
        fb.pixmap().data(),
        "a blit under a floating menu must not corrupt the overlay"
    );
}
