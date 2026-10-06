//! `Ui::paint_banded` against `Ui::paint`: the same screens, the same input,
//! one painted through a whole-screen shadow and presented, the other painted
//! a band at a time straight into the destination. Frame by frame the
//! destinations must agree — including across scroll-blit (which the banded
//! side can't do, so it repaints) and animation.

use fbui_render::geom::{Point, Size};
use fbui_render::{Color, FontContext, Scale, Surface, TargetFormat};
use fbui_widgets::event::{Event, Key, Modifiers};
use fbui_widgets::widgets::{
    Button, Calendar, Chart, Checkbox, Container, Date, Gauge, Keyboard, Label, List, Menu,
    Navigator, ProgressBar, Slider, Spinner, Switch, TabBar, TextInput, TreeNode, TreeView,
};
use fbui_widgets::{PopupOptions, Theme, Tooltip, Ui};

#[derive(Clone)]
enum Msg {}

type Build = fn(&mut Ui<Msg>);
/// Advance the screen one step (input, animation) before frame `i`; `false`
/// ends the sequence.
type Step = fn(&mut Ui<Msg>, usize) -> bool;

fn ui(w: u32, h: u32, scale: f32) -> Ui<Msg> {
    Ui::with_fonts(
        Size::new(w as f32 / scale, h as f32 / scale),
        Scale::new(scale),
        Theme::dark(),
        FontContext::with_default_font(),
    )
}

struct Report {
    frames: usize,
    pixels: usize,
    max: u8,
}

/// Run `build` + `step` on a whole-screen and a banded `Ui` and compare the
/// destination after every frame.
/// What the banded side is held against.
#[derive(Clone, Copy, PartialEq)]
enum Reference {
    /// `Ui::paint` into a whole-screen shadow, presented — the hosted path,
    /// scroll-blit included.
    Paint,
    /// A whole-screen repaint of each frame's damage: one band the height
    /// of the screen, so no band edges and no scroll-blit.
    Repaint,
}

#[allow(clippy::too_many_arguments)]
fn compare(
    reference: Reference,
    w: u32,
    h: u32,
    scale: f32,
    rows: u32,
    format: TargetFormat,
    build: Build,
    step: Step,
) -> Report {
    let mut full_ui = ui(w, h, scale);
    let mut band_ui = ui(w, h, scale);
    build(&mut full_ui);
    build(&mut band_ui);
    let sc = Scale::new(scale);
    let mut full = match reference {
        Reference::Repaint => Surface::banded(w, h, h, sc),
        Reference::Paint => Surface::new(w, h, sc),
    };
    let mut band = Surface::banded(w, h, rows, sc);
    let dither = format == TargetFormat::Rgb565;
    full.set_dither(dither);
    band.set_dither(dither);
    let stride = w as usize * format.bytes_per_pixel() + 12;
    let mut want = vec![0u8; stride * h as usize];
    let mut got = vec![0u8; stride * h as usize];
    let mut r = Report {
        frames: 0,
        pixels: 0,
        max: 0,
    };
    for i in 0.. {
        if i > 0 && !(step(&mut full_ui, i) & step(&mut band_ui, i)) {
            break;
        }
        if full.band_rows().is_some() {
            full_ui.paint_banded(&mut full, &mut want, stride, format);
        } else {
            full_ui.paint(&mut full);
            full.present_to_buffer(&mut want, stride, format, u32::from(i > 0));
        }
        band_ui.paint_banded(&mut band, &mut got, stride, format);
        r.frames += 1;
        for (a, b) in want.iter().zip(&got) {
            if a != b {
                r.pixels += 1;
                r.max = r.max.max(a.abs_diff(*b));
            }
        }
    }
    r
}

fn no_steps(_: &mut Ui<Msg>, _: usize) -> bool {
    false
}

fn key(ui: &mut Ui<Msg>, key: Key) {
    for pressed in [true, false] {
        ui.event(Event::Key {
            key,
            pressed,
            mods: Modifiers::default(),
        });
    }
}

// --- screens -----------------------------------------------------------------

fn controls(ui: &mut Ui<Msg>) {
    let root = ui.set_root(Container::column().fill().padding(10.0).gap(8.0));
    ui.add_child(root, Label::new("Settings").size(18.0));
    let row = ui.add_child(root, Container::row().gap(8.0));
    ui.add_child(row, Checkbox::new("Wi-Fi", true));
    ui.add_child(row, Switch::new("Sound", true));
    ui.add_child(root, Slider::new(0.0, 100.0, 37.0));
    ui.add_child(root, ProgressBar::new(0.62));
    ui.add_child(root, TextInput::new().placeholder("Name"));
    let b = ui.add_child(root, Button::new("Apply"));
    ui.add_child(root, Button::new("Cancel").secondary());
    ui.add_child(root, TabBar::new(["one", "two", "three"]).selected(1));
    ui.add_child(root, Spinner::new().size(28.0));
    ui.set_tooltip(b, Tooltip::new("Write to disk"));
    ui.focus(Some(b));
}

fn controls_steps(ui: &mut Ui<Msg>, i: usize) -> bool {
    match i {
        1 => key(ui, Key::Tab),
        2 => key(ui, Key::Tab),
        3..=8 => {
            ui.animate(1.0 / 30.0);
        }
        _ => return false,
    }
    true
}

fn instruments(ui: &mut Ui<Msg>) {
    let root = ui.set_root(Container::column().fill().padding(10.0).gap(10.0));
    let dials = ui.add_child(root, Container::row().gap(10.0));
    let g1 = ui.add_child(
        dials,
        Gauge::new(0.0, 100.0)
            .zone(60.0, Color::rgb(0x34, 0xd3, 0x99))
            .zone(85.0, Color::rgb(0xfb, 0xbf, 0x24))
            .zone(100.0, Color::rgb(0xef, 0x44, 0x44))
            .animate_secs(0.0),
    );
    let g2 = ui.add_child(dials, Gauge::new(0.0, 8.0).animate_secs(0.0));
    let chart = ui.add_child(
        root,
        Chart::new()
            .fixed_range(0.0, 100.0)
            .fill(true)
            .time_grid_every(10)
            .sample_width(3.0),
    );
    ui.name(chart, "chart");
    ui.layout_now();
    ui.with(g1, |g: &mut Gauge| g.set_value(72.0));
    ui.with(g2, |g: &mut Gauge| g.set_value(3.6));
    for i in 0..120u32 {
        ui.stream(chart, |c: &mut Chart| {
            c.push(&[
                (i as f32 * 0.23).sin() * 30.0 + 55.0,
                (i as f32 * 0.09).cos() * 18.0 + 25.0,
            ])
        });
    }
}

/// Keep streaming: the whole-screen side scroll-blits the chart, the banded
/// side repaints it.
fn instruments_steps(ui: &mut Ui<Msg>, i: usize) -> bool {
    if i > 6 {
        return false;
    }
    let Some(chart) = ui.find("chart") else {
        return false;
    };
    ui.stream(chart, |c: &mut Chart| {
        c.push(&[
            (i as f32 * 0.7).sin() * 40.0 + 50.0,
            (i as f32 * 0.3).cos() * 20.0 + 30.0,
        ])
    });
    true
}

fn long_list(ui: &mut Ui<Msg>) {
    let root = ui.set_root(Container::column().fill().padding(6.0));
    let items: Vec<String> = (0..60).map(|i| format!("Row {i:02} — item")).collect();
    let list = ui.add_child(root, List::new(items));
    ui.name(list, "list");
    ui.focus(Some(list));
}

/// Arrow down through the list: the whole-screen side scroll-blits rows.
fn list_steps(ui: &mut Ui<Msg>, i: usize) -> bool {
    if i > 10 {
        return false;
    }
    key(ui, Key::Down);
    ui.animate(1.0 / 60.0);
    true
}

fn tree_and_calendar(ui: &mut Ui<Msg>) {
    let root = ui.set_root(Container::row().fill().gap(6.0));
    ui.add_child(
        root,
        TreeView::new(vec![
            TreeNode::branch(
                "a",
                vec![
                    TreeNode::leaf("a1"),
                    TreeNode::branch("a2", vec![TreeNode::leaf("a2x")]).expanded(true),
                ],
            )
            .expanded(true),
            TreeNode::branch("b", vec![TreeNode::leaf("b1")]),
            TreeNode::leaf("c"),
        ]),
    );
    ui.add_child(
        root,
        Calendar::new(Date::new(2026, 8, 14).unwrap()).today(Date::new(2026, 8, 20).unwrap()),
    );
}

fn menu_over_content(ui: &mut Ui<Msg>) {
    let root = ui.set_root(Container::column().fill().padding(12.0).gap(6.0));
    for i in 0..4 {
        ui.add_child(
            root,
            Container::row()
                .grow(1.0)
                .background(Color::rgb(0x2a + i * 9, 0x30, 0x3e), 8.0),
        );
    }
    let menu = ui.add_child(
        root,
        Menu::new(["Cut", "Copy"])
            .separator()
            .item("Paste")
            .disable(3),
    );
    ui.with::<Menu<Msg>, _>(menu, |m| m.open_at(Point::new(30.0, 25.0)));
    ui.open_popup(menu, PopupOptions::default());
    key(ui, Key::Down);
}

fn keyboard(ui: &mut Ui<Msg>) {
    let root = ui.set_root(Container::column().fill());
    ui.add_child(root, Keyboard::new().height(200.0));
}

fn navigator(ui: &mut Ui<Msg>) {
    let nav = ui.set_root(Navigator::new().duration(0.3));
    let s0 = Navigator::push(ui, nav, Container::column().fill().padding(10.0));
    ui.add_child(
        s0,
        Container::column()
            .grow(1.0)
            .background(Color::rgb(0x40, 0x80, 0x30), 6.0),
    );
    while ui.animate(1.0 / 60.0) {}
    let s1 = Navigator::push(ui, nav, Container::column().fill().padding(22.0));
    ui.add_child(
        s1,
        Container::column()
            .grow(1.0)
            .background(Color::rgb(0x80, 0x30, 0x40), 12.0),
    );
}

fn animate_steps(ui: &mut Ui<Msg>, i: usize) -> bool {
    i < 24 && ui.animate(1.0 / 60.0) | true
}

const SCREENS: &[(&str, u32, u32, Build, Step)] = &[
    ("controls", 320, 420, controls, controls_steps),
    ("instruments", 360, 240, instruments, instruments_steps),
    ("long_list", 240, 200, long_list, list_steps),
    ("tree_and_calendar", 420, 260, tree_and_calendar, no_steps),
    ("menu", 240, 200, menu_over_content, no_steps),
    ("keyboard", 360, 200, keyboard, no_steps),
    ("navigator", 200, 160, navigator, animate_steps),
];

fn check(reference: Reference, names: &[&str], scales: &[f32], rows: &[u32], format: TargetFormat) {
    for &(name, w, h, build, step) in SCREENS {
        if !names.is_empty() && !names.contains(&name) {
            continue;
        }
        for &scale in scales {
            let (w, h) = ((w as f32 * scale) as u32, (h as f32 * scale) as u32);
            for &r in rows {
                let rep = compare(reference, w, h, scale, r, format, build, step);
                assert!(
                    rep.pixels == 0,
                    "{name} at {scale}x, {r}-row bands, {format:?}: {} bytes differ (max {}) over {} frames",
                    rep.pixels,
                    rep.max,
                    rep.frames
                );
            }
        }
    }
}

/// Every screen, every frame, byte for byte against a whole-screen repaint:
/// thin bands (many band edges) at 1× and 1.5×, taller ones at 2×, and
/// RGB565 with dithering. One test per screen so they run in parallel.
macro_rules! repaint_tests {
    ($($test:ident => $screen:literal),* $(,)?) => {$(
        #[test]
        fn $test() {
            check(Reference::Repaint, &[$screen], &[1.0, 1.5], &[8], TargetFormat::Xrgb8888);
            check(Reference::Repaint, &[$screen], &[2.0], &[40], TargetFormat::Xrgb8888);
            check(Reference::Repaint, &[$screen], &[1.0], &[16], TargetFormat::Rgb565);
        }
    )*};
}

repaint_tests! {
    banded_matches_a_repaint_controls => "controls",
    banded_matches_a_repaint_instruments => "instruments",
    banded_matches_a_repaint_long_list => "long_list",
    banded_matches_a_repaint_tree_and_calendar => "tree_and_calendar",
    banded_matches_a_repaint_menu => "menu",
    banded_matches_a_repaint_keyboard => "keyboard",
    banded_matches_a_repaint_navigator => "navigator",
}

/// Against the hosted `Ui::paint` itself on the screens whose scroll-blit
/// moves whole device pixels (a list stepping by rows). A blit by a
/// fractional amount — the navigator's slide, a chart streaming at 1.5× —
/// shifts pixels rendered at the old sub-pixel offset, so `Ui::paint`
/// drifts from a true repaint there; that is a property of the blit, not
/// of banding, and the repaint test above covers those screens.
#[test]
fn banded_matches_ui_paint_where_its_blit_is_whole_pixels() {
    let screens = [
        "controls",
        "long_list",
        "tree_and_calendar",
        "menu",
        "keyboard",
    ];
    check(
        Reference::Paint,
        &screens,
        &[1.0, 1.5],
        &[16],
        TargetFormat::Xrgb8888,
    );
    check(
        Reference::Paint,
        &["instruments"],
        &[1.0, 2.0],
        &[16],
        TargetFormat::Xrgb8888,
    );
}
