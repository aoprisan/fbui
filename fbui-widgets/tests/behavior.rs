//! Behavioral tests for the widget engine: layout, event routing, focus, and
//! the retained update loop. These are font-independent (they assert structure
//! and messages, not pixels), so they're robust across hosts.

use fbui_render::geom::{Point, Size};
use fbui_render::Scale;
use fbui_widgets::event::{Event, Key, Modifiers, PointerButton};
use fbui_widgets::widgets::{
    Button, Checkbox, Container, Dialog, Keyboard, List, RadioGroup, ScrollView, Select, Spinner,
    Stack, Switch, TabBar, TextInput, ToastKind, Toasts,
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
    Kbd(Key),
    Changed(String),
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

/// Center of the key at `(row, col)` of the keyboard's current layer, located
/// through the widget's own [`Keyboard::key_rect`] — the same geometry paint
/// and hit-testing use, so the tests don't duplicate its layout constants.
fn key_center(ui: &mut Ui<Msg>, kb: WidgetId, row: usize, col: usize) -> Point {
    let b = ui.bounds(kb).expect("laid out");
    let r = ui
        .with::<Keyboard<Msg>, _>(kb, |k| k.key_rect(b, row, col))
        .flatten()
        .expect("key exists");
    Point::new(r.x + r.w / 2.0, r.y + r.h / 2.0)
}

#[test]
fn keyboard_tap_emits_key_without_stealing_focus() {
    let mut ui = ui();
    let root = ui.set_root(Container::column().fill());
    let field = ui.add_child(root, TextInput::new());
    let kb = ui.add_child(root, Keyboard::new().on_key(Msg::Kbd));
    ui.layout_now();

    // Focus the field, then type on the keyboard.
    let fc = center(&ui, field);
    click(&mut ui, fc);
    assert_eq!(ui.focused(), Some(field));
    let _ = ui.take_messages();

    // First key of the top row is 'q'.
    let q = key_center(&mut ui, kb, 0, 0);
    click(&mut ui, q);
    assert_eq!(ui.take_messages(), vec![Msg::Kbd(Key::Char('q'))]);
    // The key must NOT have taken focus off the field it types into.
    assert_eq!(ui.focused(), Some(field), "keyboard keys never steal focus");
}

#[test]
fn keyboard_shift_is_one_shot() {
    let mut ui = ui();
    let root = ui.set_root(Container::column().fill());
    let kb = ui.add_child(root, Keyboard::new().on_key(Msg::Kbd));
    ui.layout_now();

    // Shift is the first key of the third row; it toggles a layer, emits nothing.
    let shift = key_center(&mut ui, kb, 2, 0);
    click(&mut ui, shift);
    assert!(ui.take_messages().is_empty(), "Shift emits no key");

    // The next letter comes through upper-cased...
    let q = key_center(&mut ui, kb, 0, 0);
    click(&mut ui, q);
    assert_eq!(ui.take_messages(), vec![Msg::Kbd(Key::Char('Q'))]);

    // ...and Shift has already dropped back to lower-case (one-shot).
    click(&mut ui, q);
    assert_eq!(ui.take_messages(), vec![Msg::Kbd(Key::Char('q'))]);
}

#[test]
fn keyboard_symbols_layer_types_digits() {
    let mut ui = ui();
    let root = ui.set_root(Container::column().fill());
    let kb = ui.add_child(root, Keyboard::new().on_key(Msg::Kbd));
    ui.layout_now();

    // '?123' is the first key of the bottom row; it switches to the symbols layer.
    let toggle = key_center(&mut ui, kb, 3, 0);
    click(&mut ui, toggle);
    assert!(ui.take_messages().is_empty(), "layer toggle emits no key");

    // The top row is now digits — its first key is '1'.
    let one = key_center(&mut ui, kb, 0, 0);
    click(&mut ui, one);
    assert_eq!(ui.take_messages(), vec![Msg::Kbd(Key::Char('1'))]);
}

#[test]
fn keyboard_release_on_a_different_key_emits_nothing() {
    let mut ui = ui();
    let root = ui.set_root(Container::column().fill());
    let kb = ui.add_child(root, Keyboard::new().on_key(Msg::Kbd));
    ui.layout_now();

    // Press 'q' but release over 'e': the tap is abandoned, like Button.
    let q = key_center(&mut ui, kb, 0, 0);
    let e = key_center(&mut ui, kb, 0, 2);
    ui.event(Event::PointerDown {
        pos: q,
        button: PointerButton::Left,
    });
    ui.event(Event::PointerUp {
        pos: e,
        button: PointerButton::Left,
    });
    assert!(
        ui.take_messages().is_empty(),
        "slide-off release emits nothing"
    );
}

#[test]
fn keyboard_backspace_auto_repeats_while_held() {
    let mut ui = ui();
    let root = ui.set_root(Container::column().fill());
    let kb = ui.add_child(root, Keyboard::new().on_key(Msg::Kbd));
    ui.layout_now();

    // Bksp is the last key of the third row. Holding it arms the repeat clock.
    let bksp = key_center(&mut ui, kb, 2, 8);
    // (`Ui::with` in key_center conservatively marks the tree animating; tick
    // once so the flag below genuinely comes from the held key.)
    ui.animate(0.0);
    ui.event(Event::PointerDown {
        pos: bksp,
        button: PointerButton::Left,
    });
    assert!(ui.take_messages().is_empty(), "nothing fires on press");
    assert!(ui.is_animating(), "holding Backspace arms the repeat clock");

    // Cross the hold delay (0.45s): the first repeat fires.
    ui.animate(0.5);
    assert_eq!(ui.take_messages(), vec![Msg::Kbd(Key::Backspace)]);

    // Two more repeat intervals (0.06s each) elapse.
    ui.animate(0.12);
    assert_eq!(
        ui.take_messages(),
        vec![Msg::Kbd(Key::Backspace), Msg::Kbd(Key::Backspace)]
    );

    // The hold already repeated, so the release itself is spent.
    ui.event(Event::PointerUp {
        pos: bksp,
        button: PointerButton::Left,
    });
    assert!(
        ui.take_messages().is_empty(),
        "release after auto-repeat emits nothing"
    );
}

#[test]
fn keyboard_slide_off_aborts_backspace_repeat() {
    let mut ui = ui();
    let root = ui.set_root(Container::column().fill());
    let kb = ui.add_child(root, Keyboard::new().on_key(Msg::Kbd));
    ui.layout_now();

    let bksp = key_center(&mut ui, kb, 2, 8);
    ui.event(Event::PointerDown {
        pos: bksp,
        button: PointerButton::Left,
    });
    ui.animate(0.5);
    assert_eq!(ui.take_messages(), vec![Msg::Kbd(Key::Backspace)]);

    // Sliding off the key aborts the repeat (and disarms the key entirely).
    ui.event(Event::PointerMove {
        pos: Point::new(bksp.x, bksp.y - 200.0),
    });
    ui.animate(0.5);
    ui.event(Event::PointerUp {
        pos: bksp,
        button: PointerButton::Left,
    });
    assert!(
        ui.take_messages().is_empty(),
        "slide-off stops the repeat and spends the tap"
    );
}

/// Regression: aborting a repeating Backspace by sliding off cleared `pressed`
/// before `PointerUp` ran, and the release of the pointer capture taken on
/// press was gated on `pressed` — so the keyboard kept capture and swallowed
/// every later pointer event on screen until a key tap completed on it.
#[test]
fn keyboard_slide_off_releases_pointer_capture() {
    let mut ui = ui();
    let root = ui.set_root(Container::column().fill());
    let button = ui.add_child(root, Button::new("ok").on_press(|| Msg::Pressed));
    let kb = ui.add_child(root, Keyboard::new().on_key(Msg::Kbd));
    ui.layout_now();

    // Hold Backspace until it repeats, slide off, release.
    let bksp = key_center(&mut ui, kb, 2, 8);
    ui.event(Event::PointerDown {
        pos: bksp,
        button: PointerButton::Left,
    });
    ui.animate(0.5);
    ui.event(Event::PointerMove {
        pos: Point::new(bksp.x, bksp.y - 200.0),
    });
    ui.event(Event::PointerUp {
        pos: bksp,
        button: PointerButton::Left,
    });
    let _ = ui.take_messages();

    // The keyboard must not still hold pointer capture: a click elsewhere
    // has to reach its target.
    let bc = center(&ui, button);
    click(&mut ui, bc);
    assert_eq!(
        ui.take_messages(),
        vec![Msg::Pressed],
        "button still clickable after a Backspace slide-off"
    );
}

/// `Ui::send_key` is the on-screen keyboard's routing: it replays a tapped key
/// through the real event path, so the focused field edits AND `on_change`
/// fires — full parity with hardware typing.
#[test]
fn send_key_edits_the_focused_field_and_fires_on_change() {
    let mut ui = ui();
    let root = ui.set_root(Container::column().fill());
    let field = ui.add_child(root, TextInput::new().on_change(Msg::Changed));
    ui.layout_now();

    let fc = center(&ui, field);
    click(&mut ui, fc);
    assert_eq!(ui.focused(), Some(field));
    let _ = ui.take_messages();

    ui.send_key(Key::Char('h'));
    ui.send_key(Key::Char('i'));
    assert_eq!(
        ui.take_messages(),
        vec![Msg::Changed("h".into()), Msg::Changed("hi".into())]
    );
    assert_eq!(
        ui.with::<TextInput<Msg>, _>(field, |t| t.text().to_string()),
        Some("hi".to_string())
    );
}

#[test]
fn text_input_apply_key_edits_at_the_caret() {
    let mut ui = ui();
    let root = ui.set_root(Container::column().fill());
    let field = ui.add_child(root, TextInput::new());
    ui.layout_now();

    // apply_key is the entry point the on-screen keyboard drives through the app.
    ui.with::<TextInput<Msg>, _>(field, |t| {
        assert!(t.apply_key(Key::Char('h')));
        assert!(t.apply_key(Key::Char('i')));
    });
    assert_eq!(
        ui.with::<TextInput<Msg>, _>(field, |t| t.text().to_string()),
        Some("hi".to_string())
    );

    ui.with::<TextInput<Msg>, _>(field, |t| assert!(t.apply_key(Key::Backspace)));
    assert_eq!(
        ui.with::<TextInput<Msg>, _>(field, |t| t.text().to_string()),
        Some("h".to_string())
    );
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
    // (Adding a widget arms one conservative animation tick, so a Spinner can
    // spin from birth; run it so the idleness asserted below is the real thing.)
    ui.animate(0.0);
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

/// The screenshot flow, headless: `request_screenshot` records a destination,
/// the embedder takes it exactly once and writes the painted surface out via
/// `Surface::write_png` — the same halves the `fbui` runner drives on device.
#[test]
fn screenshot_request_flows_from_app_to_surface() {
    let mut ui = ui();
    let root = ui.set_root(Container::column().fill());
    ui.add_child(root, Button::new("ok"));

    // What `App::update` does...
    ui.request_screenshot(std::env::temp_dir().join("fbui-behavior-screenshot.png"));

    // ...and what the runner does after the next paint.
    let mut surface = fbui_render::Surface::new(120, 90, Scale::ONE);
    ui.paint(&mut surface);
    let path = ui.take_screenshot_request().expect("request pending");
    surface.write_png(&path).expect("png written");
    assert!(
        ui.take_screenshot_request().is_none(),
        "a request is taken exactly once"
    );

    let png = std::fs::read(&path).expect("file exists");
    assert_eq!(&png[1..4], b"PNG");
    let _ = std::fs::remove_file(&path);
}

/// Center of tab `index`, located through the widget's own [`TabBar::tab_rect`]
/// — the same geometry paint and hit-testing use.
fn tab_center(ui: &mut Ui<Msg>, bar: WidgetId, index: usize) -> Point {
    let b = ui.bounds(bar).expect("laid out");
    let r = ui
        .with::<TabBar<Msg>, _>(bar, |t| t.tab_rect(b, index))
        .flatten()
        .expect("tab exists");
    Point::new(r.x + r.w / 2.0, r.y + r.h / 2.0)
}

#[test]
fn tabbar_click_selects_and_emits() {
    let mut ui = ui();
    let root = ui.set_root(Container::column().fill());
    let bar = ui.add_child(
        root,
        TabBar::new(["One", "Two", "Three"]).on_select(Msg::Picked),
    );
    ui.layout_now();

    let two = tab_center(&mut ui, bar, 1);
    click(&mut ui, two);
    assert_eq!(ui.take_messages(), vec![Msg::Picked(1)]);
    assert_eq!(
        ui.with::<TabBar<Msg>, _>(bar, |t| t.selected_index()),
        Some(1)
    );

    // Clicking the already-active tab is a no-op: the selection didn't change.
    click(&mut ui, two);
    assert!(ui.take_messages().is_empty(), "re-click emits nothing");
}

#[test]
fn tabbar_arrow_keys_move_selection_and_saturate() {
    let mut ui = ui();
    let root = ui.set_root(Container::column().fill());
    let bar = ui.add_child(root, TabBar::new(["a", "b", "c"]).on_select(Msg::Picked));
    ui.layout_now();

    // Focus by clicking the active tab (selection stays, focus arrives).
    let first = tab_center(&mut ui, bar, 0);
    click(&mut ui, first);
    assert_eq!(ui.focused(), Some(bar));
    let _ = ui.take_messages();

    ui.send_key(Key::Right);
    ui.send_key(Key::Right);
    assert_eq!(ui.take_messages(), vec![Msg::Picked(1), Msg::Picked(2)]);

    // At the last tab, Right saturates silently; End is likewise a no-op here.
    ui.send_key(Key::Right);
    ui.send_key(Key::End);
    assert!(ui.take_messages().is_empty());

    ui.send_key(Key::Home);
    assert_eq!(ui.take_messages(), vec![Msg::Picked(0)]);
}

#[test]
fn tabbar_release_off_the_pressed_tab_emits_nothing() {
    let mut ui = ui();
    let root = ui.set_root(Container::column().fill());
    let bar = ui.add_child(root, TabBar::new(["a", "b"]).on_select(Msg::Picked));
    ui.layout_now();

    // Press tab 1 but release over tab 0: abandoned, like Button/Keyboard.
    let (t1, t0) = (tab_center(&mut ui, bar, 1), tab_center(&mut ui, bar, 0));
    ui.event(Event::PointerDown {
        pos: t1,
        button: PointerButton::Left,
    });
    ui.event(Event::PointerUp {
        pos: t0,
        button: PointerButton::Left,
    });
    assert!(ui.take_messages().is_empty());
    assert_eq!(
        ui.with::<TabBar<Msg>, _>(bar, |t| t.selected_index()),
        Some(0),
        "selection unchanged"
    );
}

#[test]
fn spinner_animates_from_birth_and_stops_on_demand() {
    let mut ui = ui();
    let root = ui.set_root(Container::column().fill());
    let spin = ui.add_child(root, Spinner::new());
    ui.layout_now();

    // Adding a widget arms one conservative tick; the spinner keeps it running.
    assert!(ui.is_animating(), "insert arms the first tick");
    assert!(ui.animate(0.01), "a running spinner keeps animating");

    // Clear the initial build damage, then check step-quantized repaints: a
    // sub-step tick changes no pixels, crossing a step damages the spinner.
    let mut surface = fbui_render::Surface::new(120, 90, Scale::ONE);
    ui.paint(&mut surface);
    ui.animate(0.01);
    assert!(!ui.needs_paint(), "no repaint between head steps");
    ui.animate(0.1);
    assert!(ui.needs_paint(), "crossing a head step repaints");

    // Stopping makes the whole tree idle — the idle-burns-0% rule.
    ui.with::<Spinner, _>(spin, |s| s.set_running(false));
    assert!(!ui.animate(0.01), "stopped spinner goes idle");
    assert!(!ui.is_animating());
}
